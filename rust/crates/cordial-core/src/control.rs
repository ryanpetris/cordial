//! Serial session: request framing, the one outstanding response, and output in frames.
//!
//! Requests are answered one at a time, in order: the next request is read only after the previous
//! response has been written. Events are written whenever no response is waiting, one frame at a
//! time, so a client that stops reading holds back only the serial port, never HID forwarding.
use alloc::vec::Vec;

use cordial_protocol::{
    self as p, MAX_REQUEST_BYTES,
    frame::{self, Decoder, FrameError},
};
use prost::Message;

struct Frame {
    bytes: Vec<u8>,
    offset: usize,
    serial: u64,
    response: bool,
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
    generation: u64,
    serial: u64,
    active: bool,
    stopped: bool,
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
            generation: 0,
            serial: 0,
            active: false,
            stopped: false,
        }
    }
    /// Starts (port opened) or ends (port closed) a session, discarding any partial input and
    /// unsent output. Returns whether a previous session ended.
    pub fn session(&mut self, active: bool) -> bool {
        let ended = self.active;
        self.active = active;
        self.input.reset();
        self.output = None;
        self.response = None;
        self.generation = self.generation.wrapping_add(1);
        ended
    }
    pub fn active(&self) -> bool {
        self.active
    }
    /// Accepted bootloader entry stops reading further requests.
    pub fn stop_commands(&mut self) {
        self.stopped = true;
    }
    /// Whether a response is waiting or being written.
    fn answering(&self) -> bool {
        self.response.is_some() || self.output.as_ref().is_some_and(|f| f.response)
    }
    /// Whether the output is free for an event.
    pub fn idle(&self) -> bool {
        self.active && self.output.is_none() && self.response.is_none()
    }
    /// Frames waiting to be written, including the one being written.
    pub fn queued(&self) -> usize {
        usize::from(self.output.is_some()) + usize::from(self.response.is_some())
    }
    fn encode(message: p::Message) -> Vec<u8> {
        let mut bytes = Vec::new();
        frame::encode(&message, &mut bytes);
        bytes
    }
    /// Queues the response to the request just read.
    pub fn respond(&mut self, response: p::Response) {
        if !self.active {
            return;
        }
        self.response = Some(Self::encode(p::Message {
            kind: Some(p::message::Kind::Response(response)),
        }));
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
        let bytes = Self::encode(p::Message {
            kind: Some(p::message::Kind::Event(p::Event { kind: Some(kind) })),
        });
        self.start(bytes, false);
    }
    fn start(&mut self, bytes: Vec<u8>, response: bool) {
        self.serial = self.serial.wrapping_add(1);
        self.output = Some(Frame {
            bytes,
            offset: 0,
            serial: self.serial,
            response,
            in_flight: false,
        });
    }
    /// Consumes received bytes until one complete request is read, and returns it with the number
    /// of bytes consumed. A frame that is not a usable request is answered here. Nothing is read
    /// while a response is outstanding; zero consumption tells the USB reader to keep its bytes.
    pub fn feed(&mut self, bytes: &[u8]) -> (usize, Option<p::Request>) {
        let mut consumed = 0;
        while consumed < bytes.len() && self.active && !self.stopped && !self.answering() {
            let byte = bytes[consumed];
            consumed += 1;
            let error = match self.input.push(byte) {
                None => continue,
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
        if max == 0 || !self.active {
            return None;
        }
        if self.output.is_none()
            && let Some(bytes) = self.response.take()
        {
            self.start(bytes, true);
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
