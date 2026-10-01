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
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

/// How long a request waits for its response unless [`Connection::set_timeout`] changes it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a stalled write may take before the connection is closed.
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

/// What the reader thread received, passed to a handler in the order it arrived.
#[derive(Debug)]
pub enum Received<'a> {
    Event(p::Event),
    /// A response and the request it answers, passed to the handler before that request
    /// returns it.
    Response(&'a p::Request, &'a p::Response),
    /// The connection closed, and why. Nothing follows.
    Closed(&'a Error),
}

type Reply = SyncSender<Result<p::Response>>;

struct State {
    /// One entry per request written and not yet answered, oldest first.
    pending: VecDeque<(p::Request, Reply)>,
    closed: Option<Error>,
}

struct Shared {
    /// The writer, held across a whole frame so frames never interleave.
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    state: Mutex<State>,
    /// Set once the first request is about to be written; nothing before it is read.
    started: AtomicBool,
    stop: AtomicBool,
    timeout: Mutex<Option<Duration>>,
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
        for (_, reply) in pending {
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
///
/// Dropping the connection closes it.
pub struct Connection {
    shared: Arc<Shared>,
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
            }),
            started: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            timeout: Mutex::new(Some(DEFAULT_TIMEOUT)),
        });
        let reader_shared = shared.clone();
        thread::Builder::new()
            .name("cordial-read".into())
            .spawn(move || read_loop(&reader_shared, reader, handler))
            .expect("spawn reader thread");
        Self { shared }
    }

    /// Sets how long each request waits for its response; `None` waits without limit. A request
    /// that times out closes the connection.
    pub fn set_timeout(&self, timeout: Option<Duration>) {
        *self.shared.timeout.lock().unwrap() = timeout;
    }

    /// Why the connection closed, or `None` while it is open.
    pub fn closed(&self) -> Option<Error> {
        self.shared.state.lock().unwrap().closed.clone()
    }

    /// Closes the connection. Waiting requests fail with [`Error::Closed`].
    pub fn close(&self) {
        self.shared.close(Error::Closed);
    }

    /// Sends one command and returns the Dongle's response, which may hold an error result.
    pub fn request(&self, command: Command) -> Result<p::Response> {
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
            let mut writer = self.shared.writer.lock().unwrap();
            {
                let mut state = self.shared.state.lock().unwrap();
                if let Some(error) = &state.closed {
                    return Err(error.clone());
                }
                state.pending.push_back((request, tx));
            }
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
        let reply = match timeout {
            Some(timeout) => rx.recv_timeout(timeout).map_err(|e| match e {
                // Every later response would answer the wrong request if this one was lost, so
                // the connection ends.
                RecvTimeoutError::Timeout => {
                    self.shared.close(Error::Timeout);
                    Error::Timeout
                }
                RecvTimeoutError::Disconnected => self.closed().unwrap_or(Error::Closed),
            }),
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

    pub fn set_adapter(&self, update: p::SetAdapter) -> Result<p::Status> {
        self.call(Command::SetAdapter(update), |r| match r {
            response::Result::Status(s) => Some(s),
            _ => None,
        })
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

    pub fn start_pairing(&self, candidate: &str) -> Result<()> {
        self.done(Command::StartPairing(p::StartPairing {
            candidate: candidate.into(),
        }))
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

    pub fn list_devices(&self) -> Result<Vec<p::Device>> {
        self.call(Command::ListDevices(p::ListDevices {}), |r| match r {
            response::Result::Devices(list) => Some(list.devices),
            _ => None,
        })
    }

    pub fn get_device(&self, device: &str) -> Result<p::Device> {
        self.call(
            Command::GetDevice(p::GetDevice {
                device: device.into(),
            }),
            device_result,
        )
    }

    pub fn set_device(&self, update: p::SetDevice) -> Result<p::Device> {
        self.call(Command::SetDevice(update), device_result)
    }

    pub fn connect_device(&self, device: &str) -> Result<p::Device> {
        self.call(
            Command::ConnectDevice(p::ConnectDevice {
                device: device.into(),
            }),
            device_result,
        )
    }

    pub fn disconnect_device(&self, device: &str) -> Result<p::Device> {
        self.call(
            Command::DisconnectDevice(p::DisconnectDevice {
                device: device.into(),
            }),
            device_result,
        )
    }

    pub fn unpair_device(&self, device: &str) -> Result<()> {
        self.done(Command::UnpairDevice(p::UnpairDevice {
            device: device.into(),
        }))
    }

    pub fn refresh_device(&self, device: &str) -> Result<()> {
        self.done(Command::RefreshDevice(p::RefreshDevice {
            device: device.into(),
        }))
    }

    pub fn list_warnings(&self, device: &str) -> Result<p::DeviceWarnings> {
        self.call(
            Command::ListWarnings(p::ListWarnings {
                device: device.into(),
            }),
            |r| match r {
                response::Result::Warnings(w) => Some(w),
                _ => None,
            },
        )
    }

    pub fn list_settings(&self, device: &str) -> Result<p::DeviceSettings> {
        self.call(
            Command::ListSettings(p::ListSettings {
                device: device.into(),
            }),
            settings_result,
        )
    }

    pub fn set_settings(
        &self,
        device: &str,
        changes: Vec<p::SettingChange>,
    ) -> Result<p::DeviceSettings> {
        self.call(
            Command::SetSettings(p::SetSettings {
                device: device.into(),
                changes,
            }),
            settings_result,
        )
    }

    pub fn forget_settings(
        &self,
        device: &str,
        settings: Vec<p::SettingRef>,
    ) -> Result<p::DeviceSettings> {
        self.call(
            Command::ForgetSettings(p::ForgetSettings {
                device: device.into(),
                settings,
            }),
            settings_result,
        )
    }

    /// Development firmware only.
    pub fn list_features(&self, device: &str) -> Result<Vec<p::Feature>> {
        self.call(
            Command::ListFeatures(p::ListFeatures {
                device: device.into(),
            }),
            |r| match r {
                response::Result::Features(list) => Some(list.features),
                _ => None,
            },
        )
    }

    /// Development firmware only.
    pub fn list_files(&self, path: &str) -> Result<Vec<p::FileEntry>> {
        self.call(
            Command::ListFiles(p::ListFiles { path: path.into() }),
            |r| match r {
                response::Result::Files(list) => Some(list.entries),
                _ => None,
            },
        )
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

fn device_result(r: response::Result) -> Option<p::Device> {
    match r {
        response::Result::Device(d) => Some(d),
        _ => None,
    }
}

fn settings_result(r: response::Result) -> Option<p::DeviceSettings> {
    match r {
        response::Result::Settings(s) => Some(s),
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
    let mut decoder = Decoder::new(None);
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
                    let waiting = shared.state.lock().unwrap().pending.pop_front();
                    let Some((request, reply)) = waiting else {
                        failed = Some(Error::Protocol);
                        break;
                    };
                    handler(Received::Response(&request, &response));
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
    shared.close(error.clone());
    let closed = shared.state.lock().unwrap().closed.clone();
    handler(Received::Closed(&closed.unwrap_or(error)));
}
