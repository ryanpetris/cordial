//! Multiplexed current-protocol session. A failed write is never replayed.
use crate::transport::{self, Transport};
use cordial_protocol::{
    HEARTBEAT_INTERVAL_MS, HEARTBEAT_TIMEOUT_MS, MAX_LINE_BYTES, MAX_REVISION, PROTOCOL_VERSION,
    codec,
    identifiers::RequestId,
    messages::{
        Capabilities, Command, Empty, Enabled, Message, Request as WireRequest, RequestRef, Status,
        WireError,
    },
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt, io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, Error>;
const MESSAGE_BUFFER: usize = 512;

#[derive(Clone, Debug)]
pub struct Error {
    pub message: String,
    /// Boxed: typed details make a wire error large.
    pub wire: Option<Box<WireError>>,
    pub command: Option<&'static str>,
    /// Streamed rows received before a failed terminal response.
    pub responses: Vec<Envelope>,
}
impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            wire: None,
            command: None,
            responses: Vec::new(),
        }
    }
    pub(crate) fn adapter(command: Option<&'static str>, wire: WireError) -> Self {
        let code = serde_json::to_value(wire.code).unwrap();
        Self {
            message: code.as_str().unwrap().to_owned(),
            wire: Some(Box::new(wire)),
            command,
            responses: Vec::new(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::new(value.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct Envelope {
    pub message: Message,
    pub raw: String,
    pub command: Option<&'static str>,
    pub internal: bool,
    /// Lets the session finish updating its view before a command returns.
    pub sequence: u64,
}
impl Envelope {
    pub fn data(&self) -> Result<&Value> {
        match &self.message {
            Message::Response {
                ok: true,
                result: Some(value),
                ..
            } => Ok(value),
            Message::Response {
                error: Some(error), ..
            } => Err(Error::adapter(self.command, error.clone())),
            Message::Event { data, .. } => Ok(data),
            _ => Err(Error::new("invalid response")),
        }
    }
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.data()?.clone()).map_err(|e| Error::new(e.to_string()))
    }
    pub fn done(&self) -> bool {
        matches!(self.message, Message::Response { done: true, .. })
    }
    pub fn event(&self) -> Option<&str> {
        match &self.message {
            Message::Event { event, .. } => Some(event),
            _ => None,
        }
    }
    fn parse(bytes: &[u8], sequence: u64) -> Result<Self> {
        let message: Message = codec::decode(bytes)
            .map_err(|e| Error::new(format!("invalid adapter message: {e:?}")))?;
        match &message {
            Message::Response {
                v,
                ok,
                done,
                result,
                error,
                ..
            } => {
                // Only adapter.capabilities returns a list; see Inner::receive.
                if *v != PROTOCOL_VERSION
                    || (*ok
                        && (!result
                            .as_ref()
                            .is_some_and(|r| r.is_object() || r.is_array())
                            || error.is_some()))
                    || (!*ok && (!*done || result.is_some() || error.is_none()))
                {
                    return Err(Error::new("invalid response envelope"));
                }
            }
            Message::Event {
                v,
                event,
                data,
                request_id,
            } => {
                if *v != PROTOCOL_VERSION
                    || event.is_empty()
                    || !data.is_object()
                    || (matches!(
                        event.as_str(),
                        "discovery.result" | "pairing.prompt" | "pairing.display"
                    ) && request_id.is_none())
                {
                    return Err(Error::new("invalid event envelope"));
                }
            }
        }
        Ok(Self {
            message,
            raw: String::from_utf8(bytes.to_vec()).map_err(|e| Error::new(e.to_string()))?,
            command: None,
            internal: false,
            sequence,
        })
    }
}

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Clone)]
pub struct Wait {
    pub deadline: Option<Instant>,
    pub cancellation: Cancellation,
}
impl Wait {
    pub fn timeout(duration: Duration) -> Self {
        Self {
            deadline: Some(Instant::now() + duration),
            cancellation: Cancellation::default(),
        }
    }
    pub fn unlimited() -> Self {
        Self {
            deadline: None,
            cancellation: Cancellation::default(),
        }
    }
    pub fn check(&self) -> Result<()> {
        if self.cancellation.cancelled() {
            return Err(Error::new("operation cancelled"));
        }
        if self.deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(Error::new("operation timed out"));
        }
        Ok(())
    }
    fn slice(&self) -> Duration {
        self.deadline
            .map(|end| end.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(50))
            .min(Duration::from_millis(50))
    }
}

pub struct Request {
    pub id: RequestId,
    pub command: &'static str,
    responses: Receiver<Envelope>,
    /// Set by the reader when this request's bounded queue overflowed.
    overflow: Arc<AtomicBool>,
}
struct Pending {
    command: &'static str,
    internal: bool,
    progressed: bool,
    responses: SyncSender<Envelope>,
    /// Shared with the Request: a file stream whose consumer fell behind loses
    /// its remaining rows, and only that request fails.
    overflow: Arc<AtomicBool>,
}
/// A file stream's consumer fell behind its bounded queue.
pub const TOO_SLOW: &str = "file transfer stopped: this computer read it too slowly";
#[derive(Default)]
struct State {
    next_id: u32,
    pending: BTreeMap<RequestId, Pending>,
    error: Option<Error>,
    capabilities: Option<Capabilities>,
    status: Option<Status>,
}
struct Inner {
    writer: Mutex<Option<Box<dyn Transport>>>,
    state: Mutex<State>,
    events: Mutex<Receiver<Envelope>>,
    event_tx: SyncSender<Envelope>,
    lost: AtomicU64,
    queued: AtomicUsize,
    closed: AtomicBool,
    closing: Mutex<bool>,
    wake: Condvar,
}
pub struct Client {
    inner: Arc<Inner>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}
fn reserved(command: &str) -> bool {
    matches!(
        command,
        "adapter.status"
            | "adapter.capabilities"
            | "session.heartbeat"
            | "adapter.wait_ready"
            | "session.monitor.set"
            | "pairing.reply"
            | "request.cancel"
    )
}
fn streaming(command: &str) -> bool {
    matches!(
        command,
        "device.list"
            | "hidpp.feature.list"
            | "hidpp.setting.list"
            | "hidpp.setting.refresh"
            | "hidpp.setting.apply"
            | "storage.list"
            | "storage.read"
    )
}
pub fn optional(event: &str) -> bool {
    event.starts_with("device.") || matches!(event, "adapter.changed" | "hidpp.setting.changed")
}
impl Inner {
    fn error(&self) -> Option<Error> {
        self.state.lock().unwrap().error.clone()
    }
    fn fail(&self, error: Error) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            state.error = Some(error);
            state.pending.clear();
        }
        if let Some(mut writer) = self.writer.lock().unwrap().take() {
            let _ = writer.set_dtr(false);
        }
        self.wake.notify_all();
    }
    fn start(
        &self,
        command: Command,
        internal: bool,
        wait: &Wait,
        cleanup: bool,
    ) -> Result<Request> {
        wait.check()?;
        if !cleanup && *self.closing.lock().unwrap() {
            return Err(Error::new("control session is closing"));
        }
        if !command.valid_arguments() {
            return Err(Error::new("invalid command arguments"));
        }
        let name = command.name();
        // Hold the writer gate through ID assignment and the complete frame.
        let mut writer = self.writer.lock().unwrap();
        wait.check()?;
        let mut state = self.state.lock().unwrap();
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        if let Some(reason) = state
            .capabilities
            .as_ref()
            .and_then(|c| c.unsupported(&command))
        {
            return Err(Error::new(format!("{name}: {reason}")));
        }
        let limit = state
            .status
            .as_ref()
            .map_or(4, |s| s.limits.max_pending_requests);
        if state.pending.len() >= limit + 8
            || (!reserved(name)
                && state
                    .pending
                    .values()
                    .filter(|p| !reserved(p.command))
                    .count()
                    >= limit)
        {
            return Err(Error::new("pending request limit reached"));
        }
        let id = RequestId::try_from(state.next_id + 1)
            .map_err(|_| Error::new("request IDs exhausted; reopen adapter"))?;
        let bytes = codec::encode(&WireRequest {
            v: PROTOCOL_VERSION,
            id,
            command,
        })
        .map_err(|e| Error::new(format!("cannot encode command: {e:?}")))?;
        let capacity = state.status.as_ref().map_or(65, |s| match name {
            "hidpp.feature.list" => s.limits.hidpp_features + 1,
            "hidpp.setting.list" | "hidpp.setting.refresh" | "hidpp.setting.apply" => {
                s.limits.hidpp_settings + 1
            }
            "device.list" => s.limits.saved_devices + 1,
            "storage.list" | "storage.read" => MESSAGE_BUFFER,
            _ => 2,
        });
        let (tx, rx) = mpsc::sync_channel(capacity);
        let overflow = Arc::new(AtomicBool::new(false));
        state.next_id = id.get();
        state.pending.insert(
            id,
            Pending {
                command: name,
                internal,
                progressed: false,
                responses: tx,
                overflow: overflow.clone(),
            },
        );
        drop(state);
        let port = writer
            .as_mut()
            .ok_or_else(|| Error::new("control session is closed"))?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut rest = bytes.as_slice();
        let sent = (|| {
            while !rest.is_empty() {
                wait.check()?;
                if Instant::now() >= deadline {
                    return Err(Error::new("serial write timed out"));
                }
                match port.write(rest) {
                    Ok(0) => thread::sleep(Duration::from_millis(1)),
                    Ok(n) => rest = &rest[n..],
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::Interrupted
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        })();
        drop(writer);
        if let Err(error) = sent {
            let error = Error::new(format!(
                "serial write failed; command outcome uncertain: {error}"
            ));
            self.fail(error.clone());
            return Err(error);
        }
        Ok(Request {
            id,
            command: name,
            responses: rx,
            overflow,
        })
    }
    fn wait(&self, request: Request, wait: &Wait) -> Result<Vec<Envelope>> {
        let mut result = Vec::new();
        loop {
            // A terminal acknowledgement takes precedence over a later unplug.
            let message = match request.responses.try_recv() {
                Ok(message) => message,
                Err(TryRecvError::Disconnected) => {
                    let mut error = self
                        .error()
                        .unwrap_or_else(|| Error::new("request ended without a response"));
                    error.responses = result;
                    return Err(error);
                }
                Err(TryRecvError::Empty) => {
                    if request.overflow.load(Ordering::Acquire) {
                        let mut error = Error::new(TOO_SLOW);
                        error.responses = result;
                        return Err(error);
                    }
                    if let Err(mut error) = wait.check() {
                        error.responses = result;
                        return Err(error);
                    }
                    match request.responses.recv_timeout(wait.slice()) {
                        Ok(message) => message,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => {
                            let mut error = self
                                .error()
                                .unwrap_or_else(|| Error::new("request ended without a response"));
                            error.responses = result;
                            return Err(error);
                        }
                    }
                }
            };
            let done = message.done();
            let failure = message.data().err();
            result.push(message);
            if done {
                return match failure {
                    Some(mut error) => {
                        error.responses = result;
                        Err(error)
                    }
                    None => Ok(result),
                };
            }
        }
    }
    fn call(
        &self,
        command: Command,
        internal: bool,
        wait: &Wait,
        cleanup: bool,
    ) -> Result<Vec<Envelope>> {
        for attempt in 0..3 {
            let result = self.wait(self.start(command.clone(), internal, wait, cleanup)?, wait);
            if attempt < 2
                && matches!(command, Command::Devices(_))
                && result
                    .as_ref()
                    .err()
                    .and_then(|e| e.wire.as_ref())
                    .is_some_and(|e| e.code == cordial_protocol::errors::ErrorCode::Busy)
            {
                wait.check()?;
                continue;
            }
            return result;
        }
        unreachable!()
    }
    fn receive(&self, mut envelope: Envelope) -> Result<()> {
        if let Message::Response { id, done, .. } = &envelope.message {
            let mut state = self.state.lock().unwrap();
            let pending = state
                .pending
                .get_mut(id)
                .ok_or_else(|| Error::new("unexpected response ID"))?;
            let list = matches!(&envelope.message, Message::Response { result: Some(r), .. } if r.is_array());
            let ok = matches!(&envelope.message, Message::Response { ok: true, .. });
            if list != (pending.command == "adapter.capabilities" && ok) {
                return Err(Error::new("invalid response result"));
            }
            if !done && pending.command == "adapter.wait_ready" && !pending.progressed {
                pending.progressed = true;
            } else if !done && !streaming(pending.command) {
                return Err(Error::new("unexpected nonterminal response"));
            }
            envelope.command = Some(pending.command);
            envelope.internal = pending.internal;
            let responses = pending.responses.clone();
            let overflow = pending.overflow.clone();
            if *done {
                state.pending.remove(id);
            }
            drop(state);
            // The reader never waits for a consumer. A file stream that fills
            // its bounded queue is failed alone: later rows, including its
            // terminal one, are discarded and its consumer sees TOO_SLOW.
            let storage = matches!(envelope.command, Some("storage.list" | "storage.read"));
            if !(storage && overflow.load(Ordering::Acquire)) {
                match responses.try_send(envelope.clone()) {
                    Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
                    Err(mpsc::TrySendError::Full(_)) if storage => {
                        overflow.store(true, Ordering::Release);
                    }
                    Err(mpsc::TrySendError::Full(_)) => {
                        return Err(Error::new("response buffer limit exceeded; reopen adapter"));
                    }
                }
            }
        }
        // File payloads go only to their request consumer, never the event log.
        if matches!(envelope.command, Some("storage.list" | "storage.read")) {
            return Ok(());
        }
        if envelope.event().is_some_and(optional)
            && self.queued.load(Ordering::Relaxed) >= MESSAGE_BUFFER - 16
        {
            self.lost.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        if let Err(error) = self.event_tx.try_send(envelope.clone()) {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            if envelope.event().is_some_and(optional) {
                self.lost.fetch_add(1, Ordering::Relaxed);
            } else {
                return Err(Error::new(format!(
                    "mandatory notification buffer full: {error}"
                )));
            }
        }
        if envelope.event() == Some("protocol.error") {
            return Err(Error::new("adapter protocol error; reopen adapter"));
        }
        Ok(())
    }
}
impl Client {
    pub fn open(port: &str, wait: &Wait) -> Result<Self> {
        wait.check()?;
        Self::connect(transport::open(port)?, wait)
    }
    pub fn connect(mut port: Box<dyn Transport>, wait: &Wait) -> Result<Self> {
        let handshake = (|| {
            port.set_timeout(Duration::from_millis(50))?;
            port.set_dtr(false)?;
            thread::sleep(Duration::from_millis(60));
            wait.check()?;
            port.clear_input()?;
            port.set_dtr(true)?;
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut byte = [0];
            loop {
                wait.check()?;
                if Instant::now() >= deadline {
                    return Err(Error::new("waiting for session separator timed out"));
                }
                match port.read(&mut byte) {
                    Ok(1) if byte[0] == b'\n' => break,
                    Ok(0) => return Err(Error::new("adapter closed during handshake")),
                    Ok(_) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        })();
        if let Err(error) = handshake {
            let _ = port.set_dtr(false);
            return Err(error);
        }
        let reader = match port.try_clone() {
            Ok(reader) => reader,
            Err(error) => {
                let _ = port.set_dtr(false);
                return Err(error.into());
            }
        };
        let (tx, rx) = mpsc::sync_channel(MESSAGE_BUFFER);
        let inner = Arc::new(Inner {
            writer: Mutex::new(Some(port)),
            state: Mutex::new(State::default()),
            events: Mutex::new(rx),
            event_tx: tx,
            lost: AtomicU64::new(0),
            queued: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            closing: Mutex::new(false),
            wake: Condvar::new(),
        });
        let client = Self {
            inner: inner.clone(),
            threads: Mutex::new(Vec::new()),
        };
        client.threads.lock().unwrap().push(
            thread::Builder::new()
                .name("cordial-read".into())
                .spawn(move || read_loop(inner, reader))?,
        );
        let handshake = Wait {
            deadline: Some(
                wait.deadline
                    .unwrap_or(Instant::now() + Duration::from_secs(4))
                    .min(Instant::now() + Duration::from_secs(4)),
            ),
            cancellation: wait.cancellation.clone(),
        };
        // Capabilities are per connection and precede status validation.
        let capabilities: Capabilities = client
            .call(Command::Capabilities(Empty {}), true, &handshake)?
            .last()
            .unwrap()
            .decode()?;
        if !capabilities.valid() {
            return Err(Error::new("invalid adapter capabilities"));
        }
        client.inner.state.lock().unwrap().capabilities = Some(capabilities.clone());
        let status: Status = client
            .call(Command::Status(Empty {}), true, &handshake)?
            .last()
            .unwrap()
            .decode()?;
        validate_status(&status, &capabilities)?;
        client.inner.state.lock().unwrap().status = Some(status);
        client.call(Command::Heartbeat(Empty {}), true, &handshake)?;
        let inner = client.inner.clone();
        client.threads.lock().unwrap().push(
            thread::Builder::new()
                .name("cordial-heartbeat".into())
                .spawn(move || {
                    loop {
                        let closing = inner.closing.lock().unwrap();
                        let (closing, _) = inner
                            .wake
                            .wait_timeout_while(
                                closing,
                                Duration::from_millis(HEARTBEAT_INTERVAL_MS.into()),
                                |closing| !*closing && !inner.closed.load(Ordering::Acquire),
                            )
                            .unwrap();
                        if *closing || inner.closed.load(Ordering::Acquire) {
                            break;
                        }
                        drop(closing);
                        if let Err(error) = inner.call(
                            Command::Heartbeat(Empty {}),
                            true,
                            &Wait::timeout(Duration::from_secs(4)),
                            false,
                        ) {
                            if !*inner.closing.lock().unwrap() {
                                inner.fail(Error::new(format!("heartbeat failed: {error}")));
                            }
                            break;
                        }
                    }
                })?,
        );
        Ok(client)
    }
    pub fn status(&self) -> Status {
        self.inner.state.lock().unwrap().status.clone().unwrap()
    }
    pub fn capabilities(&self) -> Capabilities {
        self.inner
            .state
            .lock()
            .unwrap()
            .capabilities
            .clone()
            .unwrap()
    }
    pub fn error(&self) -> Option<Error> {
        self.inner.error()
    }
    pub fn fail(&self, error: Error) {
        self.inner.fail(error);
    }
    pub fn start(&self, command: Command, internal: bool, wait: &Wait) -> Result<Request> {
        self.inner.start(command, internal, wait, false)
    }
    pub fn wait(&self, request: Request, wait: &Wait) -> Result<Vec<Envelope>> {
        self.inner.wait(request, wait)
    }
    pub fn call(&self, command: Command, internal: bool, wait: &Wait) -> Result<Vec<Envelope>> {
        self.inner.call(command, internal, wait, false)
    }
    /// Consume a response stream without retaining its chunks or publishing file contents as events.
    pub fn stream(
        &self,
        command: Command,
        wait: &Wait,
        mut consume: impl FnMut(&Envelope) -> Result<()>,
    ) -> Result<()> {
        let request = self.start(command, true, wait)?;
        let mut finished = false;
        let result = (|| {
            loop {
                wait.check()?;
                // Rows after an overflow are gone; never finish with a gap.
                if request.overflow.load(Ordering::Acquire) {
                    return Err(Error::new(TOO_SLOW));
                }
                match request.responses.recv_timeout(wait.slice()) {
                    Ok(message) => {
                        let done = message.done();
                        finished |= done;
                        let result = message.data().map(drop).and_then(|()| consume(&message));
                        if done || result.is_err() {
                            return result.map(|()| done);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => (),
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(self
                            .error()
                            .unwrap_or_else(|| Error::new("incomplete response stream")));
                    }
                }
            }
        })();
        match result {
            Ok(_) => Ok(()),
            Err(error) => {
                // An abandoned stream is cancelled; its later rows are discarded.
                if !finished && self.error().is_none() {
                    let _ = self.inner.start(
                        Command::Cancel(RequestRef {
                            request_id: request.id,
                        }),
                        true,
                        &Wait::timeout(Duration::from_secs(1)),
                        false,
                    );
                }
                Err(error)
            }
        }
    }
    pub fn next(&self, timeout: Duration) -> Result<Option<Envelope>> {
        let lost = self.inner.lost.swap(0, Ordering::Relaxed);
        if lost != 0 {
            return Ok(Some(Envelope {
                message: Message::event(
                    "local.events_lost".into(),
                    None,
                    serde_json::json!({"dropped":lost}),
                ),
                raw: String::new(),
                command: None,
                internal: false,
                sequence: 0,
            }));
        }
        let rx = self.inner.events.lock().unwrap();
        match rx.try_recv() {
            Ok(message) => {
                self.inner.queued.fetch_sub(1, Ordering::Relaxed);
                return Ok(Some(message));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return Err(Error::new("control session closed")),
        }
        if let Some(error) = self.error() {
            return Err(error);
        }
        match rx.recv_timeout(timeout) {
            Ok(message) => {
                self.inner.queued.fetch_sub(1, Ordering::Relaxed);
                Ok(Some(message))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(Error::new("control session closed")),
        }
    }
    pub fn shutdown(&self) {
        {
            let mut closing = self.inner.closing.lock().unwrap();
            if *closing {
                return;
            }
            *closing = true;
        }
        self.inner.wake.notify_all();
        let wait = Wait::timeout(Duration::from_secs(1));
        let ids: Vec<_> = self
            .inner
            .state
            .lock()
            .unwrap()
            .pending
            .iter()
            .filter_map(|(&id, p)| {
                matches!(p.command, "pairing.start" | "discovery.scan").then_some(id)
            })
            .collect();
        let mut requests = Vec::new();
        if let Ok(request) = self.inner.start(
            Command::Monitor(Enabled { enabled: false }),
            true,
            &wait,
            true,
        ) {
            requests.push(request);
        }
        for id in ids {
            if let Ok(request) = self.inner.start(
                Command::Cancel(RequestRef { request_id: id }),
                true,
                &wait,
                true,
            ) {
                requests.push(request);
            }
        }
        for request in requests {
            let _ = self.inner.wait(request, &wait);
        }
        self.inner.fail(Error::new("control session closed"));
        for handle in self.threads.lock().unwrap().drain(..) {
            let _ = handle.join();
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.inner.fail(Error::new("control session closed"));
        for handle in self.threads.get_mut().unwrap().drain(..) {
            let _ = handle.join();
        }
    }
}
fn read_loop(inner: Arc<Inner>, mut reader: Box<dyn Transport>) {
    let mut storage = [0; MAX_LINE_BYTES];
    let mut framer = codec::Framer::new(&mut storage);
    let mut buffer = [0; 512];
    let mut sequence = 0;
    let result = (|| {
        while !inner.closed.load(Ordering::Acquire) {
            let n = match reader.read(&mut buffer) {
                Ok(0) => {
                    return Err(Error::new(
                        "adapter disconnected; pending command outcomes uncertain",
                    ));
                }
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(e) => {
                    return Err(Error::new(format!(
                        "serial read failed; pending command outcomes uncertain: {e}"
                    )));
                }
            };
            for &byte in &buffer[..n] {
                if let Some(line) = framer.push(byte) {
                    let line = line.map_err(|_| Error::new("adapter exceeded line limit"))?;
                    sequence += 1;
                    inner.receive(Envelope::parse(line, sequence)?)?;
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        inner.fail(error);
    }
}

/// Enabled constraints name distinct supported transports; pairing has one
/// entry per supported transport, with a reason exactly when unavailable.
fn valid_capacity(status: &Status, capabilities: &Capabilities) -> bool {
    use cordial_protocol::identifiers::Transport;
    let unique = |t: &[Transport]| t.iter().enumerate().all(|(i, x)| !t[..i].contains(x));
    let pairing: Vec<_> = status
        .capacity
        .pairing
        .iter()
        .map(|p| p.transport)
        .collect();
    status.capacity.enabled.iter().all(|c| {
        !c.transports.is_empty()
            && unique(&c.transports)
            && c.transports
                .iter()
                .all(|t| capabilities.supports_transport(*t))
            && c.enabled <= c.limit
    }) && unique(&pairing)
        && pairing.len()
            == [Transport::Classic, Transport::Ble]
                .into_iter()
                .filter(|t| capabilities.supports_transport(*t))
                .count()
        && pairing.iter().all(|t| capabilities.supports_transport(*t))
        && status
            .capacity
            .pairing
            .iter()
            .all(|p| p.available == p.reason.is_none())
}

pub fn validate_status(status: &Status, capabilities: &Capabilities) -> Result<()> {
    let l = &status.limits;
    if !capabilities.valid()
        || status.protocol != PROTOCOL_VERSION
        || status.adapter_id.is_empty()
        || status.boot_id.is_empty()
        || status.session_id.is_empty()
        || l.max_line_bytes != MAX_LINE_BYTES
        || !(4..=64).contains(&l.max_pending_requests)
        || !(1..=65_535).contains(&l.saved_devices)
        || !(1..=256).contains(&l.scan_candidates)
        || !(1..=64).contains(&l.hidpp_settings)
        || !(1..=l.hidpp_settings).contains(&l.hidpp_saved_settings)
        || !(1..=65536).contains(&l.hidpp_setting_choices)
        || !(1..=256).contains(&l.hidpp_features)
        || l.hidpp_sensors > 16
        || l.hidpp_firmware_entities > 16
        || status.counts.saved > l.saved_devices
        || status.counts.paired > status.counts.saved
        || status.counts.preferred_enabled > status.counts.saved
        || status.counts.enabled > status.counts.preferred_enabled
        || status.counts.enabled > status.counts.paired
        || status.counts.connected > status.counts.enabled
        || status.counts.connected > l.active_connections
        || !valid_capacity(status, capabilities)
        || status.revision > MAX_REVISION
        || status.heartbeat.interval_ms != HEARTBEAT_INTERVAL_MS
        || status.heartbeat.timeout_ms != HEARTBEAT_TIMEOUT_MS
    {
        return Err(Error::new("invalid adapter status"));
    }
    Ok(())
}
