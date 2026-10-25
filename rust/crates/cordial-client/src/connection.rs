use crate::{Error, Result};
use cordial_protocol::{
    self as p, MAX_REQUEST_BYTES,
    frame::{self, Decoder},
    message,
    request::Command,
    response,
};
use prost::Message as _;
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

/// How long a request may wait without incoming bytes unless [`Connection::set_timeout`] changes it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a stalled write may take before the connection is closed.
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long [`Connection::close`] waits for the reader thread to release the stream.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(1);

/// The largest decoded response or event frame accepted from the Dongle.
const MAX_RECEIVED_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// What the reader thread received, passed to a handler in the order it arrived.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "Events transfer ownership to handlers without an extra heap allocation."
)]
pub enum Received<'a> {
    Event(p::Event),
    /// A response and the request it answers, passed to the handler before that request
    /// returns it.
    Response(&'a p::Request, &'a p::Response),
    /// The connection closed, and why. Nothing follows.
    Closed(&'a Error),
}

type Reply = SyncSender<Result<p::Response>>;

/// Sees a request's response on the reader thread; see [`Connection::request_with`].
type Observer = Box<dyn FnOnce(&p::Response) + Send>;

struct State {
    /// One entry per request written and not yet answered, oldest first.
    pending: VecDeque<(p::Request, Reply, Option<Observer>)>,
    closed: Option<Error>,
    confirmed: bool,
    #[cfg(test)]
    confirmation_waiting: Option<mpsc::Sender<()>>,
}

struct Shared {
    /// The writer, held across a whole frame so frames never interleave.
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    state: Mutex<State>,
    confirmation: Condvar,
    /// Set once the first request is about to be written; nothing before it is read.
    started: AtomicBool,
    stop: AtomicBool,
    timeout: Mutex<Option<Duration>>,
    last_received: Mutex<Instant>,
    /// Set once the reader thread has dropped the reading half.
    released: Mutex<bool>,
    release: Condvar,
}

impl Shared {
    /// Closes the connection once. Every waiting request fails with `error`.
    fn close(&self, error: Error) -> bool {
        let pending = {
            let mut state = self.state.lock().unwrap();
            if state.closed.is_some() {
                return false;
            }
            state.closed = Some(error.clone());
            std::mem::take(&mut state.pending)
        };
        self.confirmation.notify_all();
        for (_, reply, _) in pending {
            let _ = reply.try_send(Err(error.clone()));
        }
        self.stop.store(true, Ordering::Release);
        // Dropping the writer ends the session, such as by lowering DTR on a serial port.
        drop(self.writer.lock().unwrap().take());
        true
    }
}

/// A session with one Dongle over a byte stream.
///
/// Requests may be sent from any thread. Each waits for its own response; responses are matched
/// to requests in the order the requests were written. A reader thread receives everything the
/// Dongle sends. Following the session rules, the connection sends a frame delimiter before its
/// first request and ignores everything it receives before the first response that follows it.
/// Only the first request is sent until its response arrives; later requests may be pipelined.
/// A decoded response or event frame may be at most 32 MiB.
///
/// Dropping the connection closes it.
pub struct Connection {
    shared: Arc<Shared>,
    reader: ThreadId,
}

impl Connection {
    /// Starts a connection and returns it with a channel of the Dongle's events. The channel
    /// disconnects when the connection closes.
    pub fn new<R, W>(reader: R, writer: W) -> (Self, Receiver<p::Event>)
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let connection = Self::with_handler(reader, writer, move |received| {
            if let Received::Event(event) = received {
                let _ = tx.send(event);
            }
        });
        (connection, rx)
    }

    /// Starts a connection that passes everything it receives to `handler` on the reader
    /// thread, in the order received. A response reaches the handler before its request
    /// returns, so a caller that applies events and responses to one view in the handler sees
    /// them in the Dongle's order.
    ///
    /// Reads should time out, as a serial port's do, so the reader thread notices a close.
    pub fn with_handler<R, W, H>(reader: R, writer: W, handler: H) -> Self
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
        H: FnMut(Received<'_>) + Send + 'static,
    {
        let shared = Arc::new(Shared {
            writer: Mutex::new(Some(Box::new(writer))),
            state: Mutex::new(State {
                pending: VecDeque::new(),
                closed: None,
                confirmed: false,
                #[cfg(test)]
                confirmation_waiting: None,
            }),
            confirmation: Condvar::new(),
            started: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            timeout: Mutex::new(Some(DEFAULT_TIMEOUT)),
            last_received: Mutex::new(Instant::now()),
            released: Mutex::new(false),
            release: Condvar::new(),
        });
        let reader_shared = shared.clone();
        let reader = thread::Builder::new()
            .name("cordial-read".into())
            .spawn(move || read_loop(&reader_shared, reader, handler))
            .expect("spawn reader thread")
            .thread()
            .id();
        Self { shared, reader }
    }

    /// Sets how long each request may wait without incoming bytes; `None` waits without limit.
    /// Progressing responses may take longer. A stalled request closes the connection.
    pub fn set_timeout(&self, timeout: Option<Duration>) {
        *self.shared.timeout.lock().unwrap() = timeout;
    }

    /// Why the connection closed, or `None` while it is open.
    pub fn closed(&self) -> Option<Error> {
        self.shared.state.lock().unwrap().closed.clone()
    }

    /// Closes the connection. Waiting requests fail with [`Error::Closed`].
    ///
    /// Both halves of the stream are dropped before this returns, so the same port can be
    /// opened again at once. Called from the handler, or when the reader thread doesn't finish
    /// its current read within a second, it returns without waiting for the reading half.
    pub fn close(&self) {
        self.shared.close(Error::Closed);
        if thread::current().id() == self.reader {
            return;
        }
        let released = self.shared.released.lock().unwrap();
        let _ = self
            .shared
            .release
            .wait_timeout_while(released, RELEASE_TIMEOUT, |released| !*released);
    }

    /// Sends one command and returns the Dongle's response, which may hold an error result.
    pub fn request(&self, command: Command) -> Result<p::Response> {
        self.send(command, None)
    }

    /// Sends one command as [`Connection::request`] does, and passes its response to `observe`
    /// on the reader thread, after the handler and before the request returns it. Whatever
    /// `observe` does with the response is ordered with what the handler does with the events
    /// around it.
    pub fn request_with(
        &self,
        command: Command,
        observe: impl FnOnce(&p::Response) + Send + 'static,
    ) -> Result<p::Response> {
        self.send(command, Some(Box::new(observe)))
    }

    fn send(&self, command: Command, observe: Option<Observer>) -> Result<p::Response> {
        let request = p::Request {
            command: Some(command),
        };
        let bytes = request.encode_to_vec();
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(Error::TooLong);
        }
        let mut frame = Vec::with_capacity(bytes.len() + bytes.len() / 254 + 3);
        let (tx, rx) = mpsc::sync_channel(1);
        {
            let mut writer = loop {
                let writer = self.shared.writer.lock().unwrap();
                let mut state = self.shared.state.lock().unwrap();
                if let Some(error) = &state.closed {
                    return Err(error.clone());
                }
                if !self.shared.started.load(Ordering::Acquire) || state.confirmed {
                    state.pending.push_back((request, tx, observe));
                    break writer;
                }
                // Until a response confirms the session, a rejected opening frame must not
                // let another request's response answer the first request.
                drop(writer);
                #[cfg(test)]
                if let Some(waiting) = state.confirmation_waiting.take() {
                    waiting.send(()).unwrap();
                }
                drop(
                    self.shared
                        .confirmation
                        .wait_while(state, |state| !state.confirmed && state.closed.is_none())
                        .unwrap(),
                );
            };
            if !self.shared.started.swap(true, Ordering::AcqRel) {
                frame.push(frame::DELIMITER);
            }
            frame::encode_bytes(&bytes, &mut frame);
            let Some(stream) = writer.as_mut() else {
                return Err(Error::Closed);
            };
            if let Err(error) = write_all(stream.as_mut(), &frame) {
                drop(writer);
                let error = Error::from(error);
                self.shared.close(error.clone());
                return Err(error);
            }
        }
        let timeout = *self.shared.timeout.lock().unwrap();
        let started = Instant::now();
        let reply = match timeout {
            Some(timeout) => loop {
                let idle = (*self.shared.last_received.lock().unwrap())
                    .max(started)
                    .elapsed();
                match rx.recv_timeout(timeout.saturating_sub(idle)) {
                    Ok(reply) => break Ok(reply),
                    Err(RecvTimeoutError::Timeout) => {
                        if (*self.shared.last_received.lock().unwrap())
                            .max(started)
                            .elapsed()
                            >= timeout
                        {
                            // A lost response would make every later response answer the wrong request.
                            self.shared.close(Error::Timeout);
                            break Err(Error::Timeout);
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        break Err(self.closed().unwrap_or(Error::Closed));
                    }
                }
            },
            None => rx
                .recv()
                .map_err(|_| self.closed().unwrap_or(Error::Closed)),
        };
        reply?
    }

    fn call<T>(
        &self,
        command: Command,
        result: impl FnOnce(response::Result) -> Option<T>,
    ) -> Result<T> {
        match self.request(command)?.result {
            Some(response::Result::Error(error)) => Err(Error::Dongle(error)),
            Some(other) => result(other).ok_or(Error::UnexpectedResponse),
            None => Err(Error::UnexpectedResponse),
        }
    }

    /// Sends a command whose success carries no result.
    fn done(&self, command: Command) -> Result<()> {
        match self.request(command)?.result {
            Some(response::Result::Error(error)) => Err(Error::Dongle(error)),
            _ => Ok(()),
        }
    }

    pub fn status(&self) -> Result<p::Status> {
        self.call(Command::GetStatus(p::GetStatus {}), |r| match r {
            response::Result::Status(s) => Some(s),
            _ => None,
        })
    }

    /// Saves adapter preferences. Success carries no result: the Dongle holds what was sent,
    /// and an adapter event follows when anything changed.
    pub fn set_adapter(&self, update: p::SetAdapter) -> Result<()> {
        self.done(Command::SetAdapter(update))
    }

    /// Development firmware only. The Dongle reboots after responding, which ends the session.
    pub fn enter_bootloader(&self) -> Result<()> {
        self.done(Command::EnterBootloader(p::EnterBootloader {}))
    }

    pub fn start_scan(&self, transports: &[p::Transport], seconds: u32) -> Result<()> {
        self.done(Command::StartScan(p::StartScan {
            transports: transports.iter().map(|t| *t as i32).collect(),
            seconds,
        }))
    }

    pub fn stop_scan(&self) -> Result<()> {
        self.done(Command::StopScan(p::StopScan {}))
    }

    pub fn start_pairing(&self, candidate: u32) -> Result<()> {
        self.done(Command::StartPairing(p::StartPairing { candidate }))
    }

    pub fn accept_prompt(&self, value: &str) -> Result<()> {
        self.done(Command::AcceptPrompt(p::AcceptPrompt {
            value: value.into(),
        }))
    }

    pub fn reject_prompt(&self) -> Result<()> {
        self.done(Command::RejectPrompt(p::RejectPrompt {}))
    }

    pub fn cancel_pairing(&self) -> Result<()> {
        self.done(Command::CancelPairing(p::CancelPairing {}))
    }

    /// Every saved device, including IDs whose records could not be read.
    pub fn list_devices(&self) -> Result<p::DeviceList> {
        self.call(Command::ListDevices(p::ListDevices {}), |r| match r {
            response::Result::Devices(list) => Some(list),
            _ => None,
        })
    }

    pub fn all_devices(&self) -> Result<Vec<p::DeviceListEntry>> {
        Ok(self.list_devices()?.entries)
    }

    pub fn get_device(&self, device: u32) -> Result<p::Device> {
        self.call(Command::GetDevice(p::GetDevice { device }), device_result)
    }

    /// Saves device preferences. Success carries no result: the Dongle holds what was sent, and
    /// a device event follows when anything changed.
    pub fn set_device(&self, update: p::SetDevice) -> Result<()> {
        self.done(Command::SetDevice(update))
    }

    pub fn connect_device(&self, device: u32) -> Result<p::Device> {
        self.call(
            Command::ConnectDevice(p::ConnectDevice { device }),
            device_result,
        )
    }

    pub fn disconnect_device(&self, device: u32) -> Result<p::Device> {
        self.call(
            Command::DisconnectDevice(p::DisconnectDevice { device }),
            device_result,
        )
    }

    pub fn unpair_device(&self, device: u32) -> Result<()> {
        self.done(Command::UnpairDevice(p::UnpairDevice { device }))
    }

    pub fn refresh_device(&self, device: u32) -> Result<()> {
        self.done(Command::RefreshDevice(p::RefreshDevice { device }))
    }

    /// Every settings object of the device.
    pub fn list_settings(&self, device: u32) -> Result<p::DeviceSettings> {
        self.call(
            Command::ListSettings(p::ListSettings { device }),
            |r| match r {
                response::Result::Settings(list) => Some(list),
                _ => None,
            },
        )
    }

    pub fn all_settings(&self, device: u32) -> Result<Vec<p::SettingsObject>> {
        Ok(self.list_settings(device)?.objects)
    }

    /// Saves and forgets settings in one request; the changes apply in order. Success carries no
    /// result: the Dongle holds what was sent, and a settings_changed event follows.
    pub fn set_settings(&self, device: u32, changes: Vec<p::SettingsUpdate>) -> Result<()> {
        self.done(Command::SetSettings(p::SetSettings { device, changes }))
    }

    /// Every saved profile, including IDs whose records could not be read.
    pub fn list_profiles(&self) -> Result<p::ProfileList> {
        self.call(Command::ListProfiles(p::ListProfiles {}), |r| match r {
            response::Result::Profiles(list) => Some(list),
            _ => None,
        })
    }

    pub fn all_profiles(&self) -> Result<Vec<p::ProfileListEntry>> {
        Ok(self.list_profiles()?.entries)
    }

    pub fn get_profile(&self, profile: u32) -> Result<p::Profile> {
        self.call(
            Command::GetProfile(p::GetProfile { profile }),
            |r| match r {
                response::Result::Profile(profile) => Some(profile),
                _ => None,
            },
        )
    }

    /// Creates an empty profile and returns its ID. A profile event follows.
    pub fn create_profile(&self, name: &str) -> Result<u32> {
        self.call(
            Command::CreateProfile(p::CreateProfile { name: name.into() }),
            created_result,
        )
    }

    /// Copies a saved profile's rules into a new profile and returns its ID. A profile event
    /// follows.
    pub fn copy_profile(&self, profile: u32, name: &str) -> Result<u32> {
        self.call(
            Command::CopyProfile(p::CopyProfile {
                profile,
                name: name.into(),
            }),
            created_result,
        )
    }

    /// A profile_removed event follows.
    pub fn delete_profile(&self, profile: u32) -> Result<()> {
        self.done(Command::DeleteProfile(p::DeleteProfile { profile }))
    }

    /// Every rule of the profile.
    pub fn list_profile_rules(&self, profile: u32) -> Result<p::ProfileRules> {
        self.call(
            Command::ListProfileRules(p::ListProfileRules { profile }),
            |r| match r {
                response::Result::ProfileRules(list) => Some(list),
                _ => None,
            },
        )
    }

    pub fn all_profile_rules(&self, profile: u32) -> Result<Vec<p::ProfileRule>> {
        Ok(self.list_profile_rules(profile)?.rules)
    }

    /// Saves and forgets rules in one request; the changes apply in order. Success carries no
    /// result: the Dongle holds what was sent.
    pub fn set_profile_rules(
        &self,
        profile: u32,
        changes: Vec<p::ProfileRuleChange>,
    ) -> Result<()> {
        self.done(Command::SetProfileRules(p::SetProfileRules {
            profile,
            changes,
        }))
    }

    /// Every feature of the device. Development firmware only.
    pub fn list_features(&self, device: u32) -> Result<p::FeatureList> {
        self.call(
            Command::ListFeatures(p::ListFeatures { device }),
            |r| match r {
                response::Result::Features(list) => Some(list),
                _ => None,
            },
        )
    }

    pub fn all_features(&self, device: u32) -> Result<Vec<p::Feature>> {
        Ok(self.list_features(device)?.features)
    }

    /// Every entry of a directory. Development firmware only.
    pub fn list_files(&self, path: &str) -> Result<p::FileList> {
        self.call(
            Command::ListFiles(p::ListFiles { path: path.into() }),
            |r| match r {
                response::Result::Files(list) => Some(list),
                _ => None,
            },
        )
    }

    pub fn all_files(&self, path: &str) -> Result<Vec<p::FileEntry>> {
        Ok(self.list_files(path)?.entries)
    }

    /// Development firmware only.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        self.call(
            Command::ReadFile(p::ReadFile { path: path.into() }),
            |r| match r {
                response::Result::File(file) => Some(file.data),
                _ => None,
            },
        )
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

fn created_result(r: response::Result) -> Option<u32> {
    match r {
        response::Result::ProfileCreated(created) => Some(created.profile),
        _ => None,
    }
}

fn device_result(r: response::Result) -> Option<p::Device> {
    match r {
        response::Result::Device(d) => Some(d),
        _ => None,
    }
}

fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn write_all(stream: &mut dyn Write, mut bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match stream.write(bytes) {
            Ok(0) => thread::sleep(Duration::from_millis(1)),
            Ok(n) => bytes = &bytes[n..],
            Err(error) if retry(&error) => thread::sleep(Duration::from_millis(1)),
            Err(error) => return Err(error),
        }
    }
    loop {
        match stream.flush() {
            Ok(()) => return Ok(()),
            Err(error) if retry(&error) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) => return Err(error),
        }
    }
}

fn read_loop<R: Read>(shared: &Shared, mut reader: R, mut handler: impl FnMut(Received<'_>)) {
    let mut decoder = Decoder::new(Some(MAX_RECEIVED_FRAME_BYTES));
    let mut buffer = [0; 4096];
    // Until the first response to this client's first request, frames are leftovers.
    let mut synced = false;
    let error = loop {
        if shared.stop.load(Ordering::Acquire) {
            break Error::Closed;
        }
        let n = match reader.read(&mut buffer) {
            Ok(0) => break Error::Closed,
            Ok(n) => n,
            Err(error) if retry(&error) => continue,
            Err(error) => break Error::from(error),
        };
        if shared.stop.load(Ordering::Acquire) {
            break Error::Closed;
        }
        *shared.last_received.lock().unwrap() = Instant::now();
        let mut failed = None;
        for &byte in &buffer[..n] {
            let Some(frame) = decoder.push(byte) else {
                continue;
            };
            let message = frame.ok().and_then(|frame| p::Message::decode(frame).ok());
            if !synced {
                if shared.started.load(Ordering::Acquire)
                    && let Some(p::Message {
                        kind: Some(message::Kind::Response(_)),
                    }) = &message
                {
                    synced = true;
                } else {
                    continue;
                }
            }
            let Some(message) = message else {
                failed = Some(Error::Protocol);
                break;
            };
            match message.kind {
                Some(message::Kind::Event(event)) => handler(Received::Event(event)),
                Some(message::Kind::Response(response)) => {
                    let waiting = {
                        let mut state = shared.state.lock().unwrap();
                        let waiting = state.pending.pop_front();
                        if waiting.is_some() {
                            state.confirmed = true;
                        }
                        waiting
                    };
                    let Some((request, reply, observe)) = waiting else {
                        failed = Some(Error::Protocol);
                        break;
                    };
                    shared.confirmation.notify_all();
                    handler(Received::Response(&request, &response));
                    if let Some(observe) = observe {
                        observe(&response);
                    }
                    let _ = reply.try_send(Ok(response));
                }
                // A message kind added after this client was built.
                None => {}
            }
        }
        if let Some(error) = failed {
            break error;
        }
    };
    // The reading half holds the port too; dropping it lets the port be opened again.
    drop(reader);
    *shared.released.lock().unwrap() = true;
    shared.release.notify_all();
    shared.close(error.clone());
    let closed = shared.state.lock().unwrap().closed.clone();
    handler(Received::Closed(&closed.unwrap_or(error)));
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEADLINE: Duration = Duration::from_secs(10);

    struct Reader(Receiver<Vec<u8>>);

    impl Read for Reader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            match self.0.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => {
                    out[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Err(RecvTimeoutError::Timeout) => Err(io::ErrorKind::TimedOut.into()),
                Err(RecvTimeoutError::Disconnected) => Ok(0),
            }
        }
    }

    struct Writer(mpsc::Sender<()>);

    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.send(()).unwrap();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn connection() -> (Arc<Connection>, mpsc::Sender<Vec<u8>>, Receiver<()>) {
        let (responses, reader) = mpsc::channel();
        let (writer, writes) = mpsc::channel();
        let (connection, _) = Connection::new(Reader(reader), Writer(writer));
        connection.set_timeout(None);
        (Arc::new(connection), responses, writes)
    }

    fn request(connection: &Arc<Connection>) -> Receiver<Result<p::Response>> {
        let connection = connection.clone();
        let (reply, result) = mpsc::channel();
        thread::spawn(move || {
            let _ = reply.send(connection.request(Command::StopScan(p::StopScan {})));
        });
        result
    }

    fn blocked_request(connection: &Arc<Connection>) -> Receiver<Result<p::Response>> {
        let (waiting, entered) = mpsc::channel();
        connection.shared.state.lock().unwrap().confirmation_waiting = Some(waiting);
        let result = request(connection);
        entered.recv_timeout(DEADLINE).unwrap();
        // The signal is sent with the state locked. Acquiring it proves the waiter has
        // released it inside the condition variable wait, before confirmation or close.
        drop(connection.shared.state.lock().unwrap());
        result
    }

    #[test]
    fn an_opening_response_wakes_a_blocked_confirmation_waiter() {
        let (connection, responses, writes) = connection();
        let opening = request(&connection);
        writes.recv_timeout(DEADLINE).unwrap();
        let waiting = blocked_request(&connection);

        let mut response = Vec::new();
        frame::encode(
            &p::Message {
                kind: Some(message::Kind::Response(p::Response::default())),
            },
            &mut response,
        );
        responses.send(response).unwrap();
        opening.recv_timeout(DEADLINE).unwrap().unwrap();
        writes.recv_timeout(DEADLINE).unwrap();

        connection.close();
        assert!(matches!(
            waiting.recv_timeout(DEADLINE).unwrap(),
            Err(Error::Closed)
        ));
    }

    #[test]
    fn closing_wakes_a_blocked_confirmation_waiter_without_a_timeout() {
        let (connection, _responses, writes) = connection();
        let opening = request(&connection);
        writes.recv_timeout(DEADLINE).unwrap();
        let waiting = blocked_request(&connection);

        connection.close();
        for result in [opening, waiting] {
            assert!(matches!(
                result.recv_timeout(DEADLINE).unwrap(),
                Err(Error::Closed)
            ));
        }
        assert!(writes.try_recv().is_err());
    }
}
