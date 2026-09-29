//! Serial session framing, request ordering, monitor lease and bounded output.
use alloc::{boxed::Box, vec::Vec};
use cordial_protocol::{
    HEARTBEAT_TIMEOUT_MS, MAX_REVISION,
    codec::{self, DecodeError, Framer},
    errors::ErrorCode,
    identifiers::RequestId,
    messages::{Message, Request, WireError},
};
use serde::Serialize;

include!(concat!(env!("OUT_DIR"), "/output_frames.rs"));
pub const OUTPUT_TIMEOUT_MS: u64 = 5_000;
pub const READY_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    Starting,
    Ready,
    StorageFailed,
    RadioFailed,
}

#[derive(Default)]
struct Frame {
    bytes: Box<[u8]>,
    offset: usize,
    queued_at: u64,
    serial: u64,
    optional: bool,
    in_flight: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmitError {
    Unavailable,
    Full,
    TooLarge,
    Encoding,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputToken {
    generation: u64,
    frame: u64,
    offset: usize,
    length: usize,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Tick {
    pub presence_expired: bool,
    pub session_fault: bool,
}

#[derive(Clone, Copy)]
enum DeferredError {
    Protocol(ErrorCode, Option<RequestId>),
    Request(RequestId, ErrorCode),
}

use cordial_protocol::payloads::{
    HeartbeatResult as Heartbeat, LostEvents as Lost, MonitorResult as Monitor, ProtocolError,
    ReadyResult as Ready,
};

pub struct Session<'a> {
    input: Framer<'a>,
    frames: [Frame; OUTPUT_FRAMES],
    order: [usize; OUTPUT_FRAMES],
    count: usize,
    last_id: u32,
    generation: u64,
    serial: u64,
    presence_until: u64,
    lost: u64,
    revision: u64,
    active: bool,
    present: bool,
    monitor: bool,
    faulted: bool,
    fault_notice: bool,
    stopped: bool,
    ready: Option<(RequestId, u64)>,
    deferred_error: Option<DeferredError>,
}
impl<'a> Session<'a> {
    pub fn new(input: &'a mut [u8]) -> Self {
        Self {
            input: Framer::new(input),
            frames: core::array::from_fn(|_| Frame::default()),
            order: [0; OUTPUT_FRAMES],
            count: 0,
            last_id: 0,
            generation: 0,
            serial: 0,
            presence_until: 0,
            lost: 0,
            revision: 0,
            active: false,
            present: false,
            monitor: false,
            faulted: false,
            fault_notice: false,
            stopped: false,
            ready: None,
            deferred_error: None,
        }
    }
    /// Returns whether the owner must tear down an old healthy control session.
    /// The USB adapter supplies the LF separator before new-session output.
    pub fn session(&mut self, active: bool, now: u64) -> bool {
        let ended = self.active();
        self.active = active;
        self.faulted = false;
        self.fault_notice = false;
        self.monitor = false;
        self.present = active;
        self.lost = 0;
        self.last_id = 0;
        self.count = 0;
        self.frames = core::array::from_fn(|_| Frame::default());
        self.input.reset();
        self.ready = None;
        self.deferred_error = None;
        self.generation = self.generation.wrapping_add(1);
        self.presence_until = now.saturating_add(HEARTBEAT_TIMEOUT_MS.into());
        ended
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn active(&self) -> bool {
        self.active && !self.faulted
    }
    pub fn present(&self) -> bool {
        self.active() && self.present
    }
    pub fn monitoring(&self) -> bool {
        self.active() && self.monitor
    }
    pub fn remaining_ms(&self, now: u64) -> u64 {
        if self.present() {
            self.presence_until.saturating_sub(now)
        } else {
            0
        }
    }
    pub fn revision(&mut self, revision: u64) {
        self.revision = revision.min(MAX_REVISION);
    }
    /// Accepted bootloader entry survives DTR falling while its reply drains.
    pub fn stop_commands(&mut self) {
        self.stopped = true;
        self.ready = None;
    }
    /// Bulk stream items leave room for admitting and answering control requests.
    pub fn can_stream(&self) -> bool {
        self.active() && self.count < OUTPUT_FRAMES - 2
    }
    pub fn queued(&self) -> usize {
        self.count
    }
    fn erase(&mut self, index: usize) {
        self.frames[self.order[index]] = Frame::default();
        self.order.copy_within(index + 1..self.count, index);
        self.count -= 1;
    }
    fn discard_optional(&mut self) {
        let mut i = 0;
        while i < self.count {
            let f = &self.frames[self.order[i]];
            if f.optional && f.offset == 0 && !f.in_flight {
                self.lost = self.lost.saturating_add(1).min(MAX_REVISION);
                self.erase(i);
            } else {
                i += 1;
            }
        }
    }
    fn enqueue<T: Serialize>(
        &mut self,
        value: &T,
        optional: bool,
        now: u64,
    ) -> Result<(), EmitError> {
        if !self.active() {
            return Err(EmitError::Unavailable);
        }
        if self.count == OUTPUT_FRAMES {
            return Err(EmitError::Full);
        }
        let bytes = codec::encode(value).map_err(|e| match e {
            codec::EncodeError::TooLong => EmitError::TooLarge,
            codec::EncodeError::Capacity => EmitError::Full,
            codec::EncodeError::Serialization => EmitError::Encoding,
        })?;
        self.enqueue_bytes(bytes.into_boxed_slice(), optional, now);
        Ok(())
    }
    fn enqueue_bytes(&mut self, bytes: Box<[u8]>, optional: bool, now: u64) {
        let index = (0..OUTPUT_FRAMES)
            .find(|i| !self.order[..self.count].contains(i))
            .unwrap();
        let f = &mut self.frames[index];
        f.bytes = bytes;
        f.offset = 0;
        f.queued_at = now;
        self.serial = self.serial.wrapping_add(1);
        f.serial = self.serial;
        f.optional = optional;
        f.in_flight = false;
        self.order[self.count] = index;
        self.count += 1;
    }
    /// The caller retains required results and retries after Full. No state is
    /// committed on serialization failure or queue exhaustion.
    pub fn response<T: Serialize>(
        &mut self,
        id: RequestId,
        done: bool,
        result: T,
        now: u64,
    ) -> Result<(), EmitError> {
        if !done && self.count >= OUTPUT_FRAMES - 2 {
            return Err(EmitError::Full);
        }
        self.enqueue(&Message::<T, ()>::success(id, result, done), false, now)
    }
    /// Discovery has a fixed version-zero envelope, even when the control protocol changes.
    pub fn protocol(&mut self, id: RequestId, now: u64) -> Result<(), EmitError> {
        self.enqueue(
            &Message::<_, ()>::Response {
                v: 0,
                id,
                ok: true,
                done: true,
                result: Some(cordial_protocol::messages::ProtocolResult {
                    protocol: cordial_protocol::PROTOCOL_VERSION,
                }),
                error: None,
            },
            false,
            now,
        )
    }
    pub fn error(&mut self, id: RequestId, code: ErrorCode, now: u64) -> Result<(), EmitError> {
        self.enqueue(
            &Message::<(), ()>::failure(
                id,
                WireError {
                    code,
                    details: None,
                },
            ),
            false,
            now,
        )
    }
    pub fn failure(&mut self, id: RequestId, error: WireError, now: u64) -> Result<(), EmitError> {
        self.enqueue(&Message::<(), ()>::failure(id, error), false, now)
    }
    fn loss_marker(&mut self, now: u64) -> Result<(), EmitError> {
        if self.lost != 0 {
            self.event(
                "events.lost",
                None,
                Lost {
                    dropped: self.lost,
                    revision: self.revision,
                },
                false,
                now,
            )?;
            self.lost = 0;
        }
        Ok(())
    }
    pub fn event<T: Serialize>(
        &mut self,
        name: &str,
        request_id: Option<RequestId>,
        data: T,
        optional: bool,
        now: u64,
    ) -> Result<(), EmitError> {
        if optional {
            if !self.monitoring() {
                return Ok(());
            }
            if self.count >= OUTPUT_FRAMES - 2 {
                self.lost = self.lost.saturating_add(1).min(MAX_REVISION);
                return Ok(());
            }
            if let Err(error) = self.loss_marker(now) {
                if error == EmitError::Full {
                    self.lost = self.lost.saturating_add(1).min(MAX_REVISION);
                    return Ok(());
                }
                return Err(error);
            }
            if self.count >= OUTPUT_FRAMES - 2 {
                self.lost = self.lost.saturating_add(1).min(MAX_REVISION);
                return Ok(());
            }
        }
        let result = self.enqueue(
            &Message::<(), T>::event(name.into(), request_id, data),
            optional,
            now,
        );
        if optional && result == Err(EmitError::Full) {
            self.lost = self.lost.saturating_add(1).min(MAX_REVISION);
            return Ok(());
        }
        result
    }
    fn protocol_error(&mut self, code: ErrorCode, supplied_id: Option<RequestId>, now: u64) {
        self.deferred_error = Some(DeferredError::Protocol(code, supplied_id));
        self.flush_error(now);
    }
    fn flush_error(&mut self, now: u64) {
        let result = match self.deferred_error {
            None => return,
            Some(DeferredError::Protocol(code, supplied_id)) => self.event(
                "protocol.error",
                None,
                ProtocolError { code, supplied_id },
                false,
                now,
            ),
            Some(DeferredError::Request(id, code)) => self.error(id, code, now),
        };
        if result.is_ok() {
            self.deferred_error = None;
        }
    }
    /// Consume at most one valid request. Dispatch it before feeding more input.
    /// Zero consumption tells the USB reader to retain its unconsumed bytes.
    pub fn feed(&mut self, bytes: &[u8], now: u64) -> (usize, Option<Request>) {
        let mut consumed = 0;
        while consumed < bytes.len()
            && self.active()
            && !self.stopped
            && self.deferred_error.is_none()
            && self.count <= OUTPUT_FRAMES - 2
        {
            let byte = bytes[consumed];
            consumed += 1;
            let Some(line) = self.input.push(byte) else {
                continue;
            };
            let request = match line {
                Ok(line) => codec::decode_request(line),
                Err(_) => Err(DecodeError::TooLong),
            };
            let (id, error) = match request {
                Ok(request) => {
                    if request.id.get() <= self.last_id {
                        self.protocol_error(ErrorCode::InvalidRequest, Some(request.id), now);
                        continue;
                    }
                    self.last_id = request.id.get();
                    return (consumed, Some(request));
                }
                Err(DecodeError::Arguments { id }) => (id, ErrorCode::InvalidArgs),
                Err(DecodeError::UnknownCommand { id }) => (id, ErrorCode::UnknownCommand),
                Err(DecodeError::InvalidRequest { supplied }) => {
                    self.protocol_error(ErrorCode::InvalidRequest, supplied, now);
                    continue;
                }
                Err(DecodeError::Version { id }) => {
                    self.protocol_error(
                        if id.get() <= self.last_id {
                            ErrorCode::InvalidRequest
                        } else {
                            ErrorCode::UnsupportedVersion
                        },
                        Some(id),
                        now,
                    );
                    continue;
                }
                Err(DecodeError::Json(_)) => {
                    self.protocol_error(ErrorCode::InvalidJson, None, now);
                    continue;
                }
                Err(DecodeError::TooLong) => {
                    self.protocol_error(ErrorCode::MessageTooLarge, None, now);
                    continue;
                }
            };
            if id.get() <= self.last_id {
                self.protocol_error(ErrorCode::InvalidRequest, Some(id), now);
            } else {
                self.last_id = id.get();
                self.deferred_error = Some(DeferredError::Request(id, error));
                self.flush_error(now);
            }
        }
        (consumed, None)
    }
    pub fn heartbeat(&mut self, id: RequestId, now: u64) -> Result<(), EmitError> {
        self.response(
            id,
            true,
            Heartbeat {
                timeout_ms: HEARTBEAT_TIMEOUT_MS,
                monitor: self.monitor,
            },
            now,
        )?;
        self.present = true;
        self.presence_until = now.saturating_add(HEARTBEAT_TIMEOUT_MS.into());
        Ok(())
    }
    pub fn waiting_ready(&self) -> bool {
        self.ready.is_some()
    }
    pub fn ready<T: Serialize>(
        &mut self,
        id: RequestId,
        readiness: Readiness,
        status: &T,
        now: u64,
    ) -> Result<(), EmitError> {
        if self.ready.is_some() {
            return self.error(id, ErrorCode::Busy, now);
        }
        if readiness == Readiness::Starting {
            self.response(id, false, Ready::<()>::Initializing, now)?;
            self.ready = Some((id, now.saturating_add(READY_TIMEOUT_MS)));
            Ok(())
        } else {
            self.ready = Some((id, now.saturating_add(READY_TIMEOUT_MS)));
            let result = self.ready_poll(readiness, status, now);
            if result.is_err() {
                self.ready = None;
            }
            result
        }
    }
    /// The owner supplies a fresh status snapshot. Full leaves the wait pending.
    pub fn ready_poll<T: Serialize>(
        &mut self,
        readiness: Readiness,
        status: &T,
        now: u64,
    ) -> Result<(), EmitError> {
        let Some((id, deadline)) = self.ready else {
            return Ok(());
        };
        if self.stopped || !self.active() {
            return Ok(());
        }
        match readiness {
            Readiness::StorageFailed => self.error(id, ErrorCode::StorageFailed, now)?,
            Readiness::RadioFailed => self.error(id, ErrorCode::RadioUnavailable, now)?,
            Readiness::Ready => self.response(id, true, Ready::Ready { status }, now)?,
            Readiness::Starting if now >= deadline => self.error(id, ErrorCode::Timeout, now)?,
            Readiness::Starting => return Ok(()),
        }
        self.ready = None;
        Ok(())
    }
    pub fn monitor(&mut self, id: RequestId, enabled: bool, now: u64) -> Result<(), EmitError> {
        if enabled && !self.present() {
            return self.error(id, ErrorCode::HeartbeatRequired, now);
        }
        // Reserve both the optional loss marker and subscription acknowledgement.
        if self.count + usize::from(self.lost != 0) + 1 > OUTPUT_FRAMES {
            return Err(EmitError::Full);
        }
        self.loss_marker(now)?;
        self.response(
            id,
            true,
            Monitor {
                enabled,
                revision: self.revision,
            },
            now,
        )?;
        self.monitor = enabled;
        Ok(())
    }
    pub fn tick(&mut self, now: u64) -> Tick {
        let mut tick = Tick::default();
        if !self.active() {
            return tick;
        }
        self.flush_error(now);
        if self.present && now >= self.presence_until {
            self.present = false;
            self.monitor = false;
            self.discard_optional();
            tick.presence_expired = true;
        }
        if self.order[..self.count].iter().any(|&i| {
            !self.frames[i].optional
                && now.saturating_sub(self.frames[i].queued_at) >= OUTPUT_TIMEOUT_MS
        }) {
            self.faulted = true;
            self.fault_notice = true;
            self.present = false;
            self.monitor = false;
            self.discard_optional();
            self.lost = 0;
            tick.session_fault = true;
        }
        if self.active() && self.count < OUTPUT_FRAMES - 2 {
            let _ = self.loss_marker(now);
        }
        tick
    }
    /// The USB writer copies this chunk before returning control to the owner.
    /// Only matching completion advances the queue; a new session rejects old tokens.
    pub fn output(&mut self, max: usize, now: u64) -> Option<(OutputToken, &[u8])> {
        if max == 0 {
            return None;
        }
        if self.count == 0 && self.active && self.fault_notice {
            const NOTICE:&[u8]=b"{\"v\":1,\"type\":\"event\",\"event\":\"protocol.error\",\"data\":{\"code\":\"session_fault\"}}\n";
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(NOTICE.len()).ok()?;
            bytes.extend_from_slice(NOTICE);
            self.enqueue_bytes(bytes.into_boxed_slice(), false, now);
            self.fault_notice = false;
        }
        if self.count == 0 {
            return None;
        }
        let f = &mut self.frames[self.order[0]];
        if f.in_flight {
            return None;
        }
        let length = (f.bytes.len() - f.offset).min(max);
        f.in_flight = true;
        Some((
            OutputToken {
                generation: self.generation,
                frame: f.serial,
                offset: f.offset,
                length,
            },
            &f.bytes[f.offset..f.offset + length],
        ))
    }
    pub fn output_complete(&mut self, token: OutputToken, sent: usize) {
        if token.generation != self.generation || self.count == 0 || sent > token.length {
            return;
        }
        let f = &mut self.frames[self.order[0]];
        if !f.in_flight || f.serial != token.frame || f.offset != token.offset {
            return;
        }
        f.in_flight = false;
        f.offset += sent;
        if f.offset == f.bytes.len() {
            self.erase(0);
        }
    }
}
