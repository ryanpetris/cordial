//! Serial session: request framing, the one outstanding response, and output in frames.
//!
//! Each open port accepts ordinary requests and empty identification queries. The adapter
//! reports its native protocol version without tracking a client version.
//!
//! Requests are answered one at a time, in order: the next request is read only after the previous
//! response has been written. Events are written whenever no response is waiting, one frame at a
//! time, so a client that stops reading holds back only the serial port, never HID forwarding.
use alloc::vec::Vec;

use cordial_protocol::{
    self as p, MAX_REQUEST_BYTES,
    frame::{self, Decoder, Encoder, FrameError},
};
use prost::Message;

struct Frame {
    bytes: Vec<u8>,
    offset: usize,
    serial: u64,
    response: bool,
    terminal_pairing: bool,
    in_flight: bool,
}

/// Identifies one chunk handed to the USB writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputToken {
    generation: u64,
    frame: u64,
    offset: usize,
    length: usize,
}

pub struct Session {
    input: Decoder,
    output: Option<Frame>,
    response: Option<Vec<u8>>,
    stream: Option<Encoder>,
    generation: u64,
    serial: u64,
    open: bool,
    hello: Vec<u8>,
    stopped: bool,
    ending: Option<p::SessionEndingReason>,
    ending_pairing: Option<Vec<u8>>,
    padding: u32,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            input: Decoder::new(Some(MAX_REQUEST_BYTES)),
            output: None,
            response: None,
            stream: None,
            generation: 0,
            serial: 0,
            open: false,
            hello: p::handshake::encode_adapter_hello(&p::AdapterHello {
                protocol_version: Some(p::CURRENT_PROTOCOL_VERSION),
                ..Default::default()
            })
            .expect("native protocol version"),
            stopped: false,
            ending: None,
            ending_pairing: None,
            padding: 0,
        }
    }
    /// Includes build metadata available without reading storage. Metadata that exceeds the
    /// handshake bound is omitted.
    pub fn with_identity(firmware_version: &str, serial_number: &str) -> Self {
        let mut session = Self::new();
        if let Ok(hello) = p::handshake::encode_adapter_hello(&p::AdapterHello {
            protocol_version: Some(p::CURRENT_PROTOCOL_VERSION),
            firmware_version: Some(firmware_version.into()),
            serial_number: Some(serial_number.into()),
            name: None,
        }) {
            session.hello = hello;
        }
        session
    }
    /// Starts (port opened) or ends (port closed) a session, discarding any partial input and
    /// unsent output. Returns whether a previous session ended.
    pub fn session(&mut self, open: bool) -> bool {
        let ended = self.open();
        self.open = open;
        self.input = Decoder::new(Some(MAX_REQUEST_BYTES));
        self.output = None;
        self.response = None;
        self.stream = None;
        self.stopped = false;
        self.ending = None;
        self.ending_pairing = None;
        self.padding = 0;
        self.generation = self.generation.wrapping_add(1);
        ended
    }
    /// Whether the physical port is open.
    pub fn open(&self) -> bool {
        self.open
    }
    /// Stops dispatch and appends the final event after the owed response and any queued terminal
    /// pairing outcome. A streamed response finishes with BUSY. `padding` closes any declared
    /// whole-file bytes field before that error.
    pub fn end(&mut self, reason: p::SessionEndingReason, padding: u32) {
        self.end_with_pairing(reason, padding, None);
    }
    /// Includes the application's terminal pairing snapshot before the final event.
    pub(crate) fn end_with_pairing(
        &mut self,
        reason: p::SessionEndingReason,
        padding: u32,
        pairing: Option<&p::Pairing>,
    ) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if !self.open() {
            return;
        }
        self.ending = Some(reason);
        self.padding = padding;
        self.ending_pairing = pairing
            .filter(|pairing| Self::terminal_pairing(pairing))
            .map(|pairing| {
                Self::encode(p::Message {
                    kind: Some(p::message::Kind::Event(p::Event {
                        kind: Some(p::event::Kind::Pairing(pairing.clone())),
                    })),
                })
            });
        if self
            .output
            .as_ref()
            .is_some_and(|frame| !frame.response && frame.offset == 0 && !frame.in_flight)
        {
            let frame = self.output.take().unwrap();
            if frame.terminal_pairing && self.ending_pairing.is_none() {
                self.ending_pairing = Some(frame.bytes);
            }
        }
    }
    fn terminal_pairing(pairing: &p::Pairing) -> bool {
        matches!(
            pairing.step,
            Some(p::pairing::Step::Done(_) | p::pairing::Step::Error(_))
        )
    }
    /// Whether intentional shutdown has stopped command dispatch.
    pub fn ending(&self) -> bool {
        self.stopped
    }
    /// Whether a response is waiting or being written.
    fn answering(&self) -> bool {
        self.stream.is_some()
            || self.response.is_some()
            || self.output.as_ref().is_some_and(|f| f.response)
    }
    /// Whether the output is free for an event.
    pub fn idle(&self) -> bool {
        self.open() && !self.stopped && self.output.is_none() && !self.answering()
    }
    /// Frames waiting to be written, including the one being written.
    pub fn queued(&self) -> usize {
        usize::from(self.ending.is_some())
            + usize::from(self.ending_pairing.is_some())
            + usize::from(self.output.is_some())
            + usize::from(self.response.is_some())
            + usize::from(
                self.stream.is_some()
                    && self.response.is_none()
                    && !self.output.as_ref().is_some_and(|f| f.response),
            )
    }
    fn encode(message: p::Message) -> Vec<u8> {
        let mut bytes = Vec::new();
        frame::encode(&message, &mut bytes);
        bytes
    }
    /// Queues the response to the request just read.
    pub fn respond(&mut self, response: p::Response) {
        if !self.open() {
            return;
        }
        if self.stream.is_some() {
            self.stream_response(response, true);
            return;
        }
        self.response = Some(Self::encode(p::Message {
            kind: Some(p::message::Kind::Response(response)),
        }));
    }
    /// Reserves the output for a response produced over several application steps.
    pub fn start_stream(&mut self) {
        if self.open() {
            self.stream = Some(Encoder::new());
        }
    }

    /// Whether the producer can supply its next bounded batch.
    pub fn stream_ready(&self) -> bool {
        self.open() && self.output.is_none() && self.response.is_none()
    }

    /// Appends an ordinary response envelope to the current logical protobuf message.
    /// The caller waits for `stream_ready` before producing another batch.
    pub fn stream_response(&mut self, response: p::Response, finished: bool) {
        if !self.open() {
            return;
        }
        assert!(self.stream_ready());
        let encoder = self.stream.as_mut().expect("response stream");
        let bytes = p::Message {
            kind: Some(p::message::Kind::Response(response)),
        }
        .encode_to_vec();
        let mut encoded = Vec::new();
        encoder.push(&bytes, &mut encoded);
        if finished {
            self.stream.take().unwrap().finish(&mut encoded);
        }
        if !encoded.is_empty() {
            self.response = Some(encoded);
        }
    }

    /// Appends already encoded protobuf bytes to a single response frame.
    /// File readers use this to split one length-delimited bytes field across bounded reads.
    pub fn stream_bytes(&mut self, bytes: &[u8], finished: bool) {
        if !self.open() {
            return;
        }
        assert!(self.stream_ready());
        let encoder = self.stream.as_mut().expect("response stream");
        let mut encoded = Vec::new();
        encoder.push(bytes, &mut encoded);
        if finished {
            self.stream.take().unwrap().finish(&mut encoded);
        }
        if !encoded.is_empty() {
            self.response = Some(encoded);
        }
    }

    pub fn fail(&mut self, error: p::Error) {
        self.respond(p::Response {
            result: Some(p::response::Result::Error(error)),
        });
    }
    /// Queues an event. The caller checks [`Self::idle`] first.
    pub fn event(&mut self, kind: p::event::Kind) {
        if !self.idle() {
            return;
        }
        let terminal_pairing = matches!(
            &kind,
            p::event::Kind::Pairing(pairing) if Self::terminal_pairing(pairing)
        );
        let bytes = Self::encode(p::Message {
            kind: Some(p::message::Kind::Event(p::Event { kind: Some(kind) })),
        });
        self.start(bytes, false, terminal_pairing);
    }
    fn start(&mut self, bytes: Vec<u8>, response: bool, terminal_pairing: bool) {
        self.serial = self.serial.wrapping_add(1);
        self.output = Some(Frame {
            bytes,
            offset: 0,
            serial: self.serial,
            response,
            terminal_pairing,
            in_flight: false,
        });
    }
    /// Consumes received bytes until one complete request is read, and returns it with the number
    /// of bytes consumed. An empty decoded frame queues the adapter identity. An unusable
    /// request is answered here. Nothing is read while a response is outstanding; zero
    /// consumption tells the USB reader to keep its bytes.
    pub fn feed(&mut self, bytes: &[u8]) -> (usize, Option<p::Request>) {
        let mut consumed = 0;
        while consumed < bytes.len() && self.open() && !self.stopped && !self.answering() {
            let byte = bytes[consumed];
            consumed += 1;
            let error = match self.input.push(byte) {
                None => continue,
                Some(Ok([])) => {
                    let mut bytes = Vec::new();
                    frame::encode_bytes(&self.hello, &mut bytes);
                    self.response = Some(bytes);
                    continue;
                }
                Some(Ok(frame)) => match p::Request::decode(frame) {
                    Ok(request) if request.command.is_some() => return (consumed, Some(request)),
                    Ok(_) => p::ErrorCode::UnknownCommand,
                    Err(_) => p::ErrorCode::BadRequest,
                },
                Some(Err(FrameError::TooLong)) => p::ErrorCode::TooLong,
                Some(Err(FrameError::Malformed)) => p::ErrorCode::BadRequest,
            };
            self.fail(p::Error {
                code: error as i32,
                ..Default::default()
            });
        }
        (consumed, None)
    }
    /// The next chunk of output, at most `max` bytes. The USB writer copies it before returning
    /// control; only a matching completion advances the output.
    pub fn output(&mut self, max: usize) -> Option<(OutputToken, &[u8])> {
        if max == 0 || !self.open() {
            return None;
        }
        if self.ending.is_some() && self.stream.is_some() && self.stream_ready() {
            if self.padding != 0 {
                let length = self.padding.min(512) as usize;
                self.padding -= length as u32;
                self.stream_bytes(&[0; 512][..length], false);
            } else {
                self.fail(p::Error {
                    code: p::ErrorCode::Busy as i32,
                    ..Default::default()
                });
            }
        }
        if self.output.is_none()
            && let Some(bytes) = self.response.take()
        {
            self.start(bytes, true, false);
        }
        if self.output.is_none()
            && self.stream.is_none()
            && let Some(bytes) = self.ending_pairing.take()
        {
            self.start(bytes, false, true);
        }
        if self.output.is_none()
            && self.stream.is_none()
            && let Some(reason) = self.ending.take()
        {
            self.start(
                Self::encode(p::Message {
                    kind: Some(p::message::Kind::Event(p::Event {
                        kind: Some(p::event::Kind::SessionEnding(p::SessionEnding {
                            reason: reason as i32,
                        })),
                    })),
                }),
                false,
                false,
            );
        }
        let generation = self.generation;
        let f = self.output.as_mut()?;
        if f.in_flight {
            return None;
        }
        let length = (f.bytes.len() - f.offset).min(max);
        f.in_flight = true;
        Some((
            OutputToken {
                generation,
                frame: f.serial,
                offset: f.offset,
                length,
            },
            &f.bytes[f.offset..f.offset + length],
        ))
    }
    pub fn output_complete(&mut self, token: OutputToken, sent: usize) {
        if token.generation != self.generation || sent > token.length {
            return;
        }
        let Some(f) = self.output.as_mut() else {
            return;
        };
        if !f.in_flight || f.serial != token.frame || f.offset != token.offset {
            return;
        }
        f.in_flight = false;
        f.offset += sent;
        if f.offset == f.bytes.len() {
            self.output = None;
        }
    }
}
