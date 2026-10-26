use cordial_client::{Connection, Error, Received};
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
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

const TEST_DEADLINE: Duration = Duration::from_secs(10);

fn client_hello_frame() -> Vec<u8> {
    let mut bytes = vec![frame::DELIMITER];
    frame::encode(&p::ClientHello {}, &mut bytes);
    bytes
}

fn adapter_hello_frame() -> Vec<u8> {
    let mut bytes = Vec::new();
    frame::encode(
        &p::AdapterHello {
            protocol_version: Some(p::CURRENT_PROTOCOL_VERSION),
            ..Default::default()
        },
        &mut bytes,
    );
    bytes
}

const HOST_RECEIVE_LIMIT: usize = 32 * 1024 * 1024;

/// One direction of an in-memory byte stream. Reads time out like a serial port's.
struct PipeReader {
    rx: Receiver<Vec<u8>>,
    buffer: Vec<u8>,
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.buffer.is_empty() {
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => self.buffer = bytes,
                Err(RecvTimeoutError::Timeout) => return Err(io::ErrorKind::TimedOut.into()),
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let n = out.len().min(self.buffer.len());
        out[..n].copy_from_slice(&self.buffer[..n]);
        self.buffer.drain(..n);
        Ok(n)
    }
}

#[derive(Clone)]
struct PipeWriter(Sender<Vec<u8>>);

impl Write for PipeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .send(bytes.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn pipe() -> (PipeWriter, PipeReader) {
    let (tx, rx) = mpsc::channel();
    (
        PipeWriter(tx),
        PipeReader {
            rx,
            buffer: Vec::new(),
        },
    )
}

enum ReadStep {
    WaitForWrites(usize),
    Bytes { bytes: Vec<u8>, offset: usize },
    Wait(Receiver<()>),
    Fail(io::ErrorKind),
}

struct ScriptReader {
    hello: Arc<AtomicUsize>,
    hello_sent: bool,
    writes: Arc<AtomicUsize>,
    steps: VecDeque<ReadStep>,
}

impl Read for ScriptReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.hello_sent && self.hello.load(Ordering::Acquire) != 0 {
            self.hello_sent = true;
            let bytes = adapter_hello_frame();
            out[..bytes.len()].copy_from_slice(&bytes);
            return Ok(bytes.len());
        }
        loop {
            let Some(step) = self.steps.front_mut() else {
                thread::sleep(Duration::from_millis(1));
                return Err(io::ErrorKind::TimedOut.into());
            };
            match step {
                ReadStep::WaitForWrites(count) => {
                    if self.writes.load(Ordering::Acquire) < *count {
                        thread::sleep(Duration::from_millis(1));
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    self.steps.pop_front();
                }
                ReadStep::Bytes { bytes, offset } => {
                    if *offset == bytes.len() {
                        self.steps.pop_front();
                        continue;
                    }
                    let n = out.len().min(bytes.len() - *offset);
                    out[..n].copy_from_slice(&bytes[*offset..*offset + n]);
                    *offset += n;
                    return Ok(n);
                }
                ReadStep::Wait(signal) => {
                    if signal.try_recv().is_err() {
                        thread::sleep(Duration::from_millis(1));
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    self.steps.pop_front();
                }
                ReadStep::Fail(kind) => return Err((*kind).into()),
            }
        }
    }
}

struct CountingWriter(Arc<AtomicUsize>, Arc<AtomicUsize>);

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes == client_hello_frame() {
            self.1.store(1, Ordering::Release);
        } else {
            self.0.fetch_add(1, Ordering::Release);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Holds the first write so another caller can queue behind the writer lock.
struct HeldWriter {
    inner: CountingWriter,
    written: Sender<Vec<u8>>,
    release: Option<Receiver<io::Result<()>>>,
}

impl Write for HeldWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes == client_hello_frame() {
            return self.inner.write(bytes);
        }
        self.written.send(bytes.to_vec()).unwrap();
        if let Some(release) = self.release.take() {
            release.recv_timeout(TEST_DEADLINE).unwrap()?;
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn hold_writer(inner: CountingWriter) -> (HeldWriter, Receiver<Vec<u8>>, Sender<io::Result<()>>) {
    let (written, writes) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    (
        HeldWriter {
            inner,
            written,
            release: Some(wait),
        },
        writes,
        release,
    )
}

fn background<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (started, ready) = mpsc::channel();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        started.send(()).unwrap();
        let _ = done.send(work());
    });
    ready.recv_timeout(TEST_DEADLINE).unwrap();
    result
}

fn scripted(steps: impl IntoIterator<Item = ReadStep>) -> (ScriptReader, CountingWriter) {
    let writes = Arc::new(AtomicUsize::new(0));
    let hello = Arc::new(AtomicUsize::new(0));
    (
        ScriptReader {
            hello: hello.clone(),
            hello_sent: false,
            writes: writes.clone(),
            steps: steps.into_iter().collect(),
        },
        CountingWriter(writes, hello),
    )
}

fn bytes(bytes: Vec<u8>) -> ReadStep {
    ReadStep::Bytes { bytes, offset: 0 }
}

fn framed(kind: message::Kind) -> Vec<u8> {
    let mut bytes = Vec::new();
    frame::encode(&p::Message { kind: Some(kind) }, &mut bytes);
    bytes
}

fn oversized_frame(response: bool) -> Vec<u8> {
    let kind = if response {
        message::Kind::Response(p::Response {
            result: Some(response::Result::File(p::FileData {
                data: vec![1; HOST_RECEIVE_LIMIT],
                ..Default::default()
            })),
        })
    } else {
        message::Kind::Event(p::Event {
            kind: Some(p::event::Kind::Adapter(p::Status {
                name: "x".repeat(HOST_RECEIVE_LIMIT),
                ..Default::default()
            })),
        })
    };
    let message = p::Message { kind: Some(kind) };
    assert!(message.encoded_len() > HOST_RECEIVE_LIMIT);
    let mut bytes = Vec::new();
    frame::encode(&message, &mut bytes);
    bytes
}

fn exact_limit_response_frame() -> (Vec<u8>, usize) {
    // At this size, the three nested protobuf lengths use four bytes each.
    let data_len = HOST_RECEIVE_LIMIT - 15;
    let message = p::Message {
        kind: Some(message::Kind::Response(p::Response {
            result: Some(response::Result::File(p::FileData {
                data: vec![1; data_len],
                ..Default::default()
            })),
        })),
    };
    assert_eq!(message.encoded_len(), HOST_RECEIVE_LIMIT);
    let mut bytes = Vec::new();
    frame::encode(&message, &mut bytes);
    (bytes, data_len)
}

fn wait_for_writes(writes: &AtomicUsize, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while writes.load(Ordering::Acquire) < count {
        assert!(Instant::now() < deadline, "request was not written");
        thread::yield_now();
    }
}

/// The Dongle's side of the stream.
struct Out(PipeWriter);

impl Out {
    fn message(&mut self, kind: message::Kind) {
        let mut bytes = Vec::new();
        frame::encode(&p::Message { kind: Some(kind) }, &mut bytes);
        self.0.write_all(&bytes).unwrap();
    }
    fn respond(&mut self, result: Option<response::Result>) {
        self.message(message::Kind::Response(p::Response { result }));
    }
    fn event(&mut self, kind: p::event::Kind) {
        self.message(message::Kind::Event(p::Event { kind: Some(kind) }));
    }
    fn raw(&mut self, bytes: &[u8]) {
        self.0.write_all(bytes).unwrap();
    }
}

/// A fake Dongle: decodes each request frame and passes it to `answer`. Returns the client's
/// stream ends, the raw bytes the Dongle received, and a writer for unsolicited output.
fn dongle(
    before: impl FnOnce(&mut Out),
    mut answer: impl FnMut(Command, &mut Out) + Send + 'static,
) -> (PipeReader, PipeWriter, Arc<Mutex<Vec<u8>>>, Out) {
    let (to_client, client_reader) = pipe();
    let (client_writer, mut from_client) = pipe();
    let mut out = Out(to_client.clone());
    before(&mut out);
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = received.clone();
    let mut out_thread = Out(to_client.clone());
    thread::spawn(move || {
        let mut decoder = Decoder::new(Some(MAX_REQUEST_BYTES));
        let mut negotiated = false;
        let mut buffer = [0; 256];
        loop {
            let n = match from_client.read(&mut buffer) {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => continue,
            };
            log.lock().unwrap().extend_from_slice(&buffer[..n]);
            for &byte in &buffer[..n] {
                if let Some(frame) = decoder.push(byte) {
                    let frame = frame.unwrap();
                    if !negotiated {
                        assert_eq!(
                            p::handshake::decode_client_hello(frame).unwrap(),
                            p::ClientHello {}
                        );
                        out_thread.raw(&adapter_hello_frame());
                        negotiated = true;
                        continue;
                    }
                    let request = p::Request::decode(frame).unwrap();
                    answer(request.command.unwrap(), &mut out_thread);
                }
            }
        }
    });
    (client_reader, client_writer, received, Out(to_client))
}

fn status(name: &str) -> p::Status {
    p::Status {
        id: "adapter".into(),
        name: name.into(),
        ready: true,
        ..Default::default()
    }
}

fn device(id: u32) -> p::Device {
    p::Device {
        id,
        transport: p::Transport::Ble as i32,
        ..Default::default()
    }
}

fn devices(entries: Vec<p::device_list_entry::Entry>) -> p::DeviceList {
    p::DeviceList {
        entries: entries
            .into_iter()
            .map(|entry| p::DeviceListEntry { entry: Some(entry) })
            .collect(),
    }
}

fn saved(id: u32) -> p::device_list_entry::Entry {
    p::device_list_entry::Entry::Device(device(id))
}

#[test]
fn responses_match_requests_in_order_with_events_between() {
    let (reader, writer, _, _out) = dongle(
        |_| {},
        |command, out| match command {
            Command::GetStatus(_) => {
                out.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 1 }));
                out.respond(Some(response::Result::Status(status("Desk"))));
            }
            Command::ListDevices(_) => {
                out.event(p::event::Kind::Device(device(2)));
                out.respond(Some(response::Result::Devices(devices(vec![
                    saved(2),
                    saved(3),
                ]))));
            }
            Command::StopScan(_) => out.respond(None),
            other => panic!("{other:?}"),
        },
    );
    let (connection, events) = Connection::new(reader, writer);
    assert_eq!(connection.status().unwrap().name, "Desk");
    let list = connection.list_devices().unwrap();
    assert_eq!(
        list.entries
            .iter()
            .map(cordial_client::order::device_entry_id)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    connection.stop_scan().unwrap();
    let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        event.kind,
        Some(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 1 }))
    );
    let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(event.kind, Some(p::event::Kind::Device(device(2))));
    assert!(events.try_recv().is_err());
}

#[test]
fn status_and_device_events_preserve_scoped_errors_and_resources() {
    let mut adapter = status("Desk");
    adapter.errors = vec![
        p::Error {
            code: p::ErrorCode::UsbRequiredCapacityExceeded as i32,
            ..Default::default()
        },
        p::Error {
            code: p::ErrorCode::UsbReservedCapacityExceeded as i32,
            ..Default::default()
        },
    ];
    adapter.resources = vec![p::ResourceCapacity {
        r#type: p::ResourceType::Interfaces as i32,
        count: Some(5),
        limit: None,
    }];
    adapter.configuration_interfaces = vec![p::ConfigurationInterfaceSupport {
        interface: p::ConfigurationInterface::Via as i32,
        enabled: true,
        active: false,
        profile: 3,
        ..Default::default()
    }];
    let mut saved_device = device(2);
    saved_device.errors = vec![p::Error {
        code: p::ErrorCode::ProfileMemoryExceeded as i32,
        ..Default::default()
    }];
    saved_device.integrations = vec![p::Integration {
        kind: p::IntegrationKind::Hidpp as i32,
        enabled: true,
        status: Some(p::integration::Status::Error(p::Error {
            code: p::ErrorCode::Timeout as i32,
            ..Default::default()
        })),
        ..Default::default()
    }];
    let reply = adapter.clone();
    let changed = saved_device.clone();
    let (reader, writer, _, _out) = dongle(
        |_| {},
        move |command, out| match command {
            Command::GetStatus(_) => out.respond(Some(response::Result::Status(reply.clone()))),
            Command::GetDevice(_) => {
                out.respond(Some(response::Result::Device(changed.clone())));
                out.event(p::event::Kind::Device(changed.clone()));
            }
            other => panic!("{other:?}"),
        },
    );
    let (connection, events) = Connection::new(reader, writer);
    assert_eq!(connection.status().unwrap(), adapter);
    assert_eq!(connection.get_device(2).unwrap(), saved_device);
    assert_eq!(
        events.recv_timeout(Duration::from_secs(1)).unwrap().kind,
        Some(p::event::Kind::Device(saved_device))
    );
}

#[test]
fn the_first_request_follows_a_delimiter_and_leftovers_are_ignored() {
    let (reader, writer, received, _out) = dongle(
        |out| {
            // A previous session's partial frame and event, then the new session's delimiter.
            out.raw(&[3, 1, 2]);
            out.raw(&[0]);
            out.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 9 }));
            out.raw(&[0xff, 0xff, 0]);
        },
        |command, out| match command {
            Command::GetStatus(_) => out.respond(Some(response::Result::Status(status("A")))),
            other => panic!("{other:?}"),
        },
    );
    let (connection, events) = Connection::new(reader, writer);
    thread::sleep(Duration::from_millis(50));
    assert_eq!(connection.status().unwrap().name, "A");
    assert_eq!(connection.status().unwrap().name, "A");
    let mut expected = client_hello_frame();
    for _ in 0..2 {
        frame::encode(
            &p::Request {
                command: Some(Command::GetStatus(p::GetStatus {})),
            },
            &mut expected,
        );
    }
    assert_eq!(*received.lock().unwrap(), expected);
    assert!(events.try_recv().is_err());
}

#[test]
fn any_opening_response_allows_pipelining_before_callbacks_finish() {
    for result in [
        Some(response::Result::Status(status("Ready"))),
        Some(response::Result::Error(p::Error::default())),
        None,
        Some(response::Result::Devices(p::DeviceList::default())),
    ] {
        let opening_response = p::Response { result };
        let (reader, writer) = scripted([
            ReadStep::WaitForWrites(1),
            bytes(framed(message::Kind::Response(opening_response.clone()))),
            ReadStep::WaitForWrites(3),
            bytes(framed(message::Kind::Response(p::Response::default()))),
            bytes(framed(message::Kind::Response(p::Response::default()))),
        ]);
        let (writer, writes, release_write) = hold_writer(writer);
        let (entered, callback) = mpsc::channel();
        let (release_callback, wait) = mpsc::channel();
        let mut first = true;
        let connection = Arc::new(Connection::with_handler(reader, writer, move |received| {
            if matches!(received, Received::Response(..)) && first {
                first = false;
                entered.send(()).unwrap();
                wait.recv_timeout(TEST_DEADLINE).unwrap();
            }
        }));
        let opening = {
            let connection = connection.clone();
            background(move || connection.status())
        };
        assert_ne!(writes.recv_timeout(TEST_DEADLINE).unwrap()[0], 0);
        let next = {
            let connection = connection.clone();
            background(move || connection.stop_scan())
        };
        release_write.send(Ok(())).unwrap();
        callback.recv_timeout(TEST_DEADLINE).unwrap();
        assert_ne!(writes.recv_timeout(TEST_DEADLINE).unwrap()[0], 0);
        let later = {
            let connection = connection.clone();
            background(move || connection.stop_scan())
        };
        assert_ne!(writes.recv_timeout(TEST_DEADLINE).unwrap()[0], 0);
        release_callback.send(()).unwrap();
        let answer = opening.recv_timeout(TEST_DEADLINE).unwrap();
        match opening_response.result {
            Some(response::Result::Status(expected)) => assert_eq!(answer.unwrap(), expected),
            Some(response::Result::Error(expected)) => {
                assert!(matches!(answer, Err(Error::Dongle(error)) if error == expected));
            }
            _ => assert!(matches!(answer, Err(Error::UnexpectedResponse))),
        }
        next.recv_timeout(TEST_DEADLINE).unwrap().unwrap();
        later.recv_timeout(TEST_DEADLINE).unwrap().unwrap();
        assert!(connection.closed().is_none());
    }
}

#[test]
fn closing_or_io_failure_wakes_unsent_requests_without_a_timeout() {
    for case in 0..3 {
        let (fail, wait) = mpsc::channel();
        let (reader, writer) = scripted([
            ReadStep::Wait(wait),
            ReadStep::Fail(io::ErrorKind::ConnectionReset),
        ]);
        let (writer, writes, release) = hold_writer(writer);
        let (connection, _events) = Connection::new(reader, writer);
        connection.set_timeout(None);
        let connection = Arc::new(connection);
        let opening = {
            let connection = connection.clone();
            background(move || connection.status())
        };
        writes.recv_timeout(TEST_DEADLINE).unwrap();
        let next = {
            let connection = connection.clone();
            background(move || connection.status())
        };
        release
            .send(if case == 2 {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            })
            .unwrap();
        if case != 2 {
            assert!(matches!(
                writes.recv_timeout(Duration::from_millis(100)),
                Err(RecvTimeoutError::Timeout)
            ));
            if case == 0 {
                connection.close();
            } else {
                fail.send(()).unwrap();
            }
        }
        for (index, failed) in [
            opening.recv_timeout(TEST_DEADLINE).unwrap(),
            next.recv_timeout(TEST_DEADLINE).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            let failed = if index == 1 {
                failed.map_err(|error| {
                    let Error::NotSent(error) = error else {
                        panic!("unsent request has the wrong error");
                    };
                    Arc::unwrap_or_clone(error)
                })
            } else {
                failed
            };
            match case {
                0 => assert!(matches!(failed, Err(Error::Closed))),
                1 => assert!(
                    matches!(failed, Err(Error::Io(ref error)) if error.kind() == io::ErrorKind::ConnectionReset)
                ),
                2 => assert!(
                    matches!(failed, Err(Error::Io(ref error)) if error.kind() == io::ErrorKind::BrokenPipe)
                ),
                _ => unreachable!(),
            }
        }
        assert!(writes.try_recv().is_err());
        assert!(connection.closed().is_some());
    }
}

#[test]
fn the_opening_observer_can_close_with_a_concurrent_request() {
    let (reader, writer) = scripted([
        ReadStep::WaitForWrites(1),
        bytes(framed(message::Kind::Response(p::Response::default()))),
    ]);
    let (writer, writes, release) = hold_writer(writer);
    let (connection, _events) = Connection::new(reader, writer);
    connection.set_timeout(None);
    let connection = Arc::new(connection);
    let opening = {
        let connection = connection.clone();
        background(move || {
            let close = connection.clone();
            connection.request_with(Command::StopScan(p::StopScan {}), move |_| close.close())
        })
    };
    writes.recv_timeout(TEST_DEADLINE).unwrap();
    let next = {
        let connection = connection.clone();
        background(move || connection.status())
    };
    release.send(Ok(())).unwrap();
    assert_eq!(
        opening.recv_timeout(TEST_DEADLINE).unwrap().unwrap(),
        p::Response::default()
    );
    match next.recv_timeout(TEST_DEADLINE).unwrap() {
        Err(Error::Closed) => {}
        Err(Error::NotSent(error)) => assert!(matches!(*error, Error::Closed)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(connection.closed(), Some(Error::Closed)));
}

#[test]
fn a_response_at_the_host_receive_limit_is_accepted() {
    let (response, data_len) = exact_limit_response_frame();
    let (reader, writer) = scripted([ReadStep::WaitForWrites(1), bytes(response)]);
    let (connection, _events) = Connection::new(reader, writer);

    assert_eq!(connection.read_file("/large").unwrap().len(), data_len);
    assert!(connection.closed().is_none());
}

#[test]
fn dongle_errors_are_error_values_and_raw_responses_keep_them() {
    let error = p::Error {
        code: p::ErrorCode::NoCapacity as i32,
        reason: p::ErrorReason::EnabledDevices as i32,
        outcome: p::ErrorOutcome::Partial as i32,
        related: vec![p::Error {
            code: p::ErrorCode::Timeout as i32,
            reason: p::ErrorReason::Radio as i32,
            outcome: p::ErrorOutcome::Unknown as i32,
            related: vec![p::Error {
                code: 200,
                reason: 201,
                outcome: 202,
                related: vec![],
            }],
        }],
    };
    let reply = error.clone();
    let (reader, writer, _, _out) = dongle(
        |_| {},
        move |_, out| out.respond(Some(response::Result::Error(reply.clone()))),
    );
    let (connection, _events) = Connection::new(reader, writer);
    let result = connection.set_device(p::SetDevice {
        device: 1,
        enabled: Some(true),
        ..Default::default()
    });
    match result {
        Err(Error::Dongle(e)) => assert_eq!(e, error),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        connection.connect_device(1).unwrap_err().code(),
        Some(p::ErrorCode::NoCapacity)
    );
    let response = connection
        .request(Command::GetStatus(p::GetStatus {}))
        .unwrap();
    assert_eq!(response.result, Some(response::Result::Error(error)));
}

#[test]
fn profile_commands_carry_their_fields_and_results() {
    let profile = |id: u32, name: &str| p::Profile {
        id,
        name: name.into(),
        roles: vec![p::Role::Mouse as i32],
    };
    let rule = p::ProfileRule {
        input: Some(p::Usage {
            usage_page: 9,
            usage: 1,
        }),
        collection: Some(p::Usage {
            usage_page: 1,
            usage: 2,
        }),
        effect: Some(p::profile_rule::Effect::Remap(p::profile_rule::Remap {
            outputs: vec![p::profile_rule::Output {
                usage: Some(p::Usage {
                    usage_page: 9,
                    usage: 2,
                }),
                collection: None,
            }],
        })),
    };
    let saved = rule.clone();
    let (reader, writer, _, _out) = dongle(
        |_| {},
        move |command, out| match command {
            Command::GetStatus(_) => out.respond(Some(response::Result::Status(status("A")))),
            Command::GetProfile(g) => {
                out.respond(Some(response::Result::Profile(profile(g.profile, "Work"))))
            }
            Command::CreateProfile(c) => {
                assert_eq!(c.name, "Games");
                out.respond(Some(response::Result::ProfileCreated(p::ProfileCreated {
                    profile: 5,
                })))
            }
            Command::CopyProfile(c) => {
                assert_eq!((c.profile, c.name.as_str()), (2, "Games"));
                out.respond(Some(response::Result::ProfileCreated(p::ProfileCreated {
                    profile: 3,
                })));
            }
            Command::DeleteProfile(d) => {
                assert_eq!(d.profile, 3);
                out.respond(None);
            }
            Command::ListProfileRules(l) => {
                out.respond(Some(response::Result::ProfileRules(p::ProfileRules {
                    profile: l.profile,
                    rules: vec![saved.clone()],
                })))
            }
            Command::SetProfileRules(s) => {
                assert_eq!(s.profile, 2);
                assert!(matches!(
                    s.changes[0].change,
                    Some(p::profile_rule_change::Change::Forget(_))
                ));
                out.respond(None)
            }
            other => panic!("{other:?}"),
        },
    );
    let (connection, _events) = Connection::new(reader, writer);
    connection.status().unwrap();
    assert_eq!(connection.get_profile(2).unwrap(), profile(2, "Work"));
    assert_eq!(connection.create_profile("Games").unwrap(), 5);
    assert_eq!(connection.copy_profile(2, "Games").unwrap(), 3);
    connection.delete_profile(3).unwrap();
    assert_eq!(connection.all_profile_rules(2).unwrap(), [rule]);
    let forget = p::ProfileRuleChange {
        change: Some(p::profile_rule_change::Change::Forget(p::ProfileRuleRef {
            input: Some(p::Usage {
                usage_page: 9,
                usage: 1,
            }),
            collection: Some(p::Usage {
                usage_page: 1,
                usage: 2,
            }),
        })),
    };
    connection.set_profile_rules(2, vec![forget]).unwrap();
}

#[test]
fn typed_settings_keep_metadata_partial_updates_and_whole_events() {
    let object = |index| p::SettingsObject {
        integration: p::IntegrationKind::Hidpp as i32,
        group: Some(p::settings_object::Group::PointerSensor(
            p::PointerSensorSettings {
                index,
                dpi: Some(p::IntegerSetting {
                    value: Some(0),
                    saved: Some(800),
                    status: Some(p::SettingStatus {
                        result: Some(p::setting_status::Result::State(
                            p::SettingState::Pending as i32,
                        )),
                    }),
                    limits: Some(p::integer_setting::Limits::Range(p::IntegerRange {
                        min: 0,
                        max: 3200,
                        step: 100,
                    })),
                }),
            },
        )),
    };
    let expected = vec![object(0), object(1)];
    let change = p::SettingsUpdate {
        integration: p::IntegrationKind::Hidpp as i32,
        group: Some(p::settings_update::Group::Backlight(
            p::BacklightSettingsUpdate {
                enabled: Some(p::BoolSettingUpdate {
                    change: Some(p::bool_setting_update::Change::Value(false)),
                }),
                delay: Some(p::BacklightDelaySettingsUpdate {
                    hands_away: Some(p::IntegerSettingUpdate {
                        change: Some(p::integer_setting_update::Change::Value(0)),
                    }),
                    external_power: Some(p::IntegerSettingUpdate {
                        change: Some(p::integer_setting_update::Change::Forget(
                            p::SettingForget {},
                        )),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    };
    let event = p::SettingsChanged {
        device: 1,
        changed: vec![object(1)],
        removed: vec![cordial_client::order::settings_ref(&object(0))],
    };
    let saved_change = change;
    let saved_event = event.clone();
    let (reader, writer, _, _out) = dongle(
        |_| {},
        move |command, out| match command {
            Command::GetStatus(_) => out.respond(Some(response::Result::Status(status("Desk")))),
            Command::ListSettings(list) => {
                assert_eq!(list.device, 1);
                out.respond(Some(response::Result::Settings(p::DeviceSettings {
                    device: 1,
                    objects: vec![object(0), object(1)],
                })));
            }
            Command::SetSettings(update) => {
                assert_eq!(update.device, 1);
                assert_eq!(update.changes, vec![saved_change]);
                out.respond(None);
                out.event(p::event::Kind::SettingsChanged(saved_event.clone()));
            }
            other => panic!("{other:?}"),
        },
    );
    let (connection, events) = Connection::new(reader, writer);
    connection.status().unwrap();
    assert_eq!(connection.all_settings(1).unwrap(), expected);
    connection.set_settings(1, vec![change]).unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(1)).unwrap().kind,
        Some(p::event::Kind::SettingsChanged(event))
    );
}

#[test]
fn a_result_of_the_wrong_kind_is_unexpected_and_unit_commands_accept_any_success() {
    let (reader, writer, _, _out) = dongle(
        |_| {},
        |_, out| out.respond(Some(response::Result::Status(status("A")))),
    );
    let (connection, _events) = Connection::new(reader, writer);
    assert!(matches!(
        connection.list_devices(),
        Err(Error::UnexpectedResponse)
    ));
    connection.refresh_device(1).unwrap();
    assert_eq!(connection.status().unwrap().name, "A");
}

#[test]
fn a_timed_out_request_closes_the_connection() {
    let (release, wait) = mpsc::channel::<()>();
    let (reader, writer, _, _out) = dongle(
        |_| {},
        move |command, out| match command {
            Command::GetDevice(_) => {
                wait.recv().unwrap();
                out.respond(Some(response::Result::Device(device(1))));
            }
            Command::GetStatus(_) => out.respond(Some(response::Result::Status(status("B")))),
            other => panic!("{other:?}"),
        },
    );
    let (connection, _events) = Connection::new(reader, writer);
    connection.set_timeout(Some(Duration::from_millis(50)));
    assert!(matches!(connection.get_device(1), Err(Error::Timeout)));
    assert!(matches!(connection.closed(), Some(Error::Timeout)));
    release.send(()).unwrap();
    assert!(
        matches!(connection.status(), Err(Error::NotSent(error)) if matches!(*error, Error::Timeout))
    );
}

#[test]
fn a_request_over_the_frame_limit_is_not_sent() {
    let (reader, writer, received, _out) = dongle(|_| {}, |_, out| out.respond(None));
    let (connection, _events) = Connection::new(reader, writer);
    let change = p::SettingsUpdate {
        integration: p::IntegrationKind::Hidpp as i32,
        group: Some(p::settings_update::Group::Power(p::PowerSettingsUpdate {
            auto_off: Some(p::IntegerSettingUpdate {
                change: Some(p::integer_setting_update::Change::Value(30)),
            }),
        })),
    };
    assert!(matches!(
        connection.set_settings(1, vec![change; MAX_REQUEST_BYTES]),
        Err(Error::TooLong)
    ));
    thread::sleep(Duration::from_millis(30));
    assert!(received.lock().unwrap().is_empty());
    connection.stop_scan().unwrap();
}

#[test]
fn the_stream_ending_fails_waiting_and_later_requests() {
    let (to_client, reader) = pipe();
    let (writer, _from_client) = pipe();
    let (connection, events) = Connection::new(reader, writer);
    let connection = Arc::new(connection);
    let waiting = {
        let connection = connection.clone();
        thread::spawn(move || connection.status())
    };
    thread::sleep(Duration::from_millis(50));
    drop(to_client);
    assert!(matches!(waiting.join().unwrap(), Err(Error::Handshake(_))));
    assert!(events.recv_timeout(Duration::from_secs(1)).is_err());
    assert!(matches!(connection.closed(), Some(Error::Closed)));
    assert!(matches!(
        connection.list_devices(),
        Err(Error::Handshake(_))
    ));
}

#[test]
fn closing_fails_waiting_requests_and_disconnects_events() {
    let (reader, writer, _, _out) = dongle(|_| {}, |_, _| {});
    let (connection, events) = Connection::new(reader, writer);
    let connection = Arc::new(connection);
    let waiting = {
        let connection = connection.clone();
        thread::spawn(move || connection.status())
    };
    thread::sleep(Duration::from_millis(50));
    connection.close();
    assert!(matches!(waiting.join().unwrap(), Err(Error::Closed)));
    assert!(
        matches!(connection.status(), Err(Error::NotSent(error)) if matches!(*error, Error::Closed))
    );
    assert!(events.recv_timeout(Duration::from_secs(1)).is_err());
    assert!(matches!(connection.closed(), Some(Error::Closed)));
}

#[test]
fn an_invalid_frame_after_the_first_response_closes_the_connection() {
    let (reader, writer, _, mut out) = dongle(
        |_| {},
        |_, out| out.respond(Some(response::Result::Status(status("A")))),
    );
    let (connection, _events) = Connection::new(reader, writer);
    connection.status().unwrap();
    out.raw(&[0xff, 0xff, 0xff, 0]);
    thread::sleep(Duration::from_millis(100));
    assert!(matches!(connection.closed(), Some(Error::Protocol)));
    assert!(
        matches!(connection.status(), Err(Error::NotSent(error)) if matches!(*error, Error::Protocol))
    );
}

#[test]
fn oversized_responses_and_events_close_the_synchronized_connection() {
    for is_response in [true, false] {
        let synchronized = framed(message::Kind::Response(p::Response {
            result: Some(response::Result::Status(status("Ready"))),
        }));
        let later_response = framed(message::Kind::Response(p::Response {
            result: Some(response::Result::Devices(devices(vec![saved(9)]))),
        }));
        let (reader, writer) = scripted([
            ReadStep::WaitForWrites(1),
            bytes(synchronized),
            ReadStep::WaitForWrites(3),
            bytes(oversized_frame(is_response)),
            bytes(later_response),
        ]);
        let writes = writer.0.clone();
        let (connection, events) = Connection::new(reader, writer);
        connection.set_timeout(Some(Duration::from_secs(1)));
        assert_eq!(connection.status().unwrap().name, "Ready");
        let connection = Arc::new(connection);

        let first = {
            let connection = connection.clone();
            thread::spawn(move || connection.request(Command::GetStatus(p::GetStatus {})))
        };
        wait_for_writes(&writes, 2);
        let second = {
            let connection = connection.clone();
            thread::spawn(move || connection.request(Command::ListDevices(p::ListDevices {})))
        };

        assert!(matches!(first.join().unwrap(), Err(Error::Protocol)));
        assert!(matches!(second.join().unwrap(), Err(Error::Protocol)));
        assert!(matches!(connection.closed(), Some(Error::Protocol)));
        assert!(events.recv_timeout(Duration::from_secs(1)).is_err());
    }
}

#[test]
fn a_handler_sees_events_and_responses_in_stream_order() {
    let (reader, writer, _, _out) = dongle(
        |_| {},
        |command, out| match command {
            Command::GetStatus(_) => out.respond(Some(response::Result::Status(status("A")))),
            Command::ListSettings(_) => {
                out.event(p::event::Kind::ScanDone(p::ScanDone {
                    count: 2,
                    error: None,
                }));
                out.respond(Some(response::Result::Settings(p::DeviceSettings {
                    device: 1,
                    objects: Vec::new(),
                })));
                out.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 1 }));
            }
            other => panic!("{other:?}"),
        },
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let connection = Connection::with_handler(reader, writer, move |received| {
        log.lock().unwrap().push(match received {
            Received::Event(e) => format!("event {:?}", e.kind.map(|k| k.as_name())),
            Received::Response(_, r) => format!("response {}", r.result.is_some()),
            Received::Closed(e) => format!("closed {e}"),
        });
    });
    connection.status().unwrap();
    let settings = connection.list_settings(1).unwrap();
    assert_eq!(settings.device, 1);
    // The response reached the handler before the request returned it.
    assert!(seen.lock().unwrap().len() >= 3);
    thread::sleep(Duration::from_millis(50));
    connection.close();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "response true",
            "event Some(\"scan_done\")",
            "response true",
            "event Some(\"device_removed\")",
            "closed connection closed",
        ]
    );
}

#[test]
fn an_observer_sees_its_response_between_the_events_around_it() {
    let (reader, writer, _, _out) = dongle(
        |_| {},
        |command, out| match command {
            Command::GetStatus(_) => out.respond(Some(response::Result::Status(status("A")))),
            Command::StopScan(_) => {
                out.respond(None);
                out.event(p::event::Kind::ScanDone(p::ScanDone::default()));
            }
            other => panic!("{other:?}"),
        },
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let connection = Connection::with_handler(reader, writer, move |received| {
        if let Received::Event(_) | Received::Response(..) = received {
            log.lock().unwrap().push(match received {
                Received::Event(_) => "event",
                _ => "response",
            });
        }
    });
    connection.status().unwrap();
    let log = seen.clone();
    connection
        .request_with(Command::StopScan(p::StopScan {}), move |r| {
            assert!(r.result.is_none());
            log.lock().unwrap().push("observed");
        })
        .unwrap();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        *seen.lock().unwrap(),
        ["response", "response", "observed", "event"]
    );
}

/// A reading half that records when it is dropped, as a serial port releases its lock.
struct Tracked(PipeReader, Arc<Mutex<bool>>);

impl Read for Tracked {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0.read(out)
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        *self.1.lock().unwrap() = true;
    }
}

#[test]
fn close_releases_the_reading_half_before_returning() {
    let (reader, writer, _, _out) = dongle(
        |_| {},
        |_, out| out.respond(Some(response::Result::Status(status("A")))),
    );
    let dropped = Arc::new(Mutex::new(false));
    let (connection, _events) = Connection::new(Tracked(reader, dropped.clone()), writer);
    connection.status().unwrap();
    connection.close();
    assert!(*dropped.lock().unwrap());
}

trait Name {
    fn as_name(&self) -> &'static str;
}

impl Name for p::event::Kind {
    fn as_name(&self) -> &'static str {
        match self {
            p::event::Kind::Adapter(_) => "adapter",
            p::event::Kind::Device(_) => "device",
            p::event::Kind::DeviceRemoved(_) => "device_removed",
            p::event::Kind::SettingsChanged(_) => "settings_changed",
            p::event::Kind::ScanFound(_) => "scan_found",
            p::event::Kind::ScanDone(_) => "scan_done",
            p::event::Kind::Pairing(_) => "pairing",
            p::event::Kind::Profile(_) => "profile",
            p::event::Kind::ProfileRemoved(_) => "profile_removed",
            p::event::Kind::SessionEnding(_) => "session_ending",
        }
    }
}

fn list_result(kind: usize, start: u32, count: u32) -> response::Result {
    let ids = || start + 1..start + count + 1;
    match kind {
        0 => response::Result::Devices(devices(
            ids()
                .map(|id| {
                    p::device_list_entry::Entry::Error(p::RecordError {
                        id,
                        error: Some(p::Error {
                            code: p::ErrorCode::StorageFailed as i32,
                            reason: p::ErrorReason::Storage as i32,
                            outcome: p::ErrorOutcome::NoEffect as i32,
                            related: vec![],
                        }),
                    })
                })
                .collect(),
        )),
        1 => response::Result::Profiles(p::ProfileList {
            entries: ids()
                .map(|id| p::ProfileListEntry {
                    entry: Some(p::profile_list_entry::Entry::Error(p::RecordError {
                        id,
                        error: Some(p::Error {
                            code: p::ErrorCode::StorageFailed as i32,
                            reason: p::ErrorReason::Storage as i32,
                            outcome: p::ErrorOutcome::NoEffect as i32,
                            related: vec![],
                        }),
                    })),
                })
                .collect(),
        }),
        2 => response::Result::Settings(p::DeviceSettings {
            device: 7,
            objects: ids()
                .map(|integration| p::SettingsObject {
                    integration: integration as i32,
                    ..Default::default()
                })
                .collect(),
        }),
        3 => response::Result::ProfileRules(p::ProfileRules {
            profile: 7,
            rules: ids()
                .map(|usage| p::ProfileRule {
                    input: Some(p::Usage {
                        usage_page: 9,
                        usage,
                    }),
                    collection: Some(p::Usage {
                        usage_page: 1,
                        usage: 5,
                    }),
                    ..Default::default()
                })
                .collect(),
        }),
        4 => response::Result::Features(p::FeatureList {
            features: ids()
                .map(|integration| p::Feature {
                    integration: integration as i32,
                    ..Default::default()
                })
                .collect(),
        }),
        5 => response::Result::Files(p::FileList {
            entries: ids()
                .map(|id| p::FileEntry {
                    name: format!("entry-{id}"),
                    ..Default::default()
                })
                .collect(),
        }),
        _ => unreachable!(),
    }
}

fn read_list(connection: &Connection, kind: usize) -> cordial_client::Result<response::Result> {
    match kind {
        0 => connection.list_devices().map(response::Result::Devices),
        1 => connection.list_profiles().map(response::Result::Profiles),
        2 => connection.list_settings(7).map(response::Result::Settings),
        3 => connection
            .list_profile_rules(7)
            .map(response::Result::ProfileRules),
        4 => connection.list_features(7).map(response::Result::Features),
        5 => connection
            .list_files("/records")
            .map(response::Result::Files),
        _ => unreachable!(),
    }
}

fn streaming_frame(results: impl IntoIterator<Item = response::Result>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for result in results {
        p::Message {
            kind: Some(message::Kind::Response(p::Response {
                result: Some(result),
            })),
        }
        .encode(&mut bytes)
        .unwrap();
    }
    let mut frame = Vec::new();
    frame::encode_bytes(&bytes, &mut frame);
    frame
}

#[test]
fn all_six_lists_merge_nested_messages_in_one_frame_without_followup_requests() {
    for kind in 0..6 {
        for count in [0, 300] {
            let expected = list_result(kind, 0, count);
            let results = if count == 0 {
                vec![expected.clone()]
            } else {
                (0..count)
                    .map(|start| list_result(kind, start, 1))
                    .collect()
            };
            let frame = streaming_frame(results);
            if count != 0 {
                assert!(frame.len() > 1024);
            }
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = requests.clone();
            let (reader, writer, _, _out) = dongle(
                |_| {},
                move |command, out| {
                    log.lock().unwrap().push(command);
                    for chunk in frame.chunks(17) {
                        out.raw(chunk);
                    }
                },
            );
            let (connection, _events) = Connection::new(reader, writer);
            assert_eq!(read_list(&connection, kind).unwrap(), expected);
            let expected_command = match kind {
                0 => Command::ListDevices(p::ListDevices {}),
                1 => Command::ListProfiles(p::ListProfiles {}),
                2 => Command::ListSettings(p::ListSettings { device: 7 }),
                3 => Command::ListProfileRules(p::ListProfileRules { profile: 7 }),
                4 => Command::ListFeatures(p::ListFeatures { device: 7 }),
                5 => Command::ListFiles(p::ListFiles {
                    path: "/records".into(),
                }),
                _ => unreachable!(),
            };
            assert_eq!(*requests.lock().unwrap(), [expected_command]);
        }
    }
}

#[test]
fn terminal_errors_replace_each_streamed_list_before_observers_see_it() {
    for kind in 0..6 {
        let error = p::Error {
            code: p::ErrorCode::NotReady as i32,
            ..Default::default()
        };
        let frame = streaming_frame([
            list_result(kind, 0, 1),
            response::Result::Error(error.clone()),
        ]);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let log = observed.clone();
        let (reader, writer, _, _out) = dongle(|_| {}, move |_, out| out.raw(&frame));
        let connection = Connection::with_handler(reader, writer, move |received| {
            if let Received::Response(_, response) = received {
                log.lock().unwrap().push(response.clone());
            }
        });
        assert!(matches!(read_list(&connection, kind), Err(Error::Dongle(e)) if e == error));
        assert_eq!(
            *observed.lock().unwrap(),
            [p::Response {
                result: Some(response::Result::Error(error))
            }]
        );
    }
}

#[test]
fn response_progress_extends_the_timeout_until_the_frame_finishes() {
    let timeout = Duration::from_millis(300);
    let (reader, writer, _, _out) = dongle(
        |_| {},
        |_, out| {
            let frame = streaming_frame((0..20).map(|start| list_result(0, start, 1)));
            for chunk in frame.chunks(30) {
                thread::sleep(Duration::from_millis(100));
                out.raw(chunk);
            }
        },
    );
    let (connection, _events) = Connection::new(reader, writer);
    connection.set_timeout(Some(timeout));
    let started = std::time::Instant::now();
    assert_eq!(connection.list_devices().unwrap().entries.len(), 20);
    assert!(started.elapsed() > timeout);
    assert!(connection.closed().is_none());
}

#[test]
fn incomplete_lists_never_reach_observers_and_close_discards_them() {
    let (started, wait) = mpsc::channel();
    let frame = streaming_frame([list_result(0, 0, 20)]);
    let partial = frame[..frame.len() - 1].to_vec();
    let (reader, writer, _, _out) = dongle(
        |_| {},
        move |_, out| {
            out.raw(&partial);
            started.send(()).unwrap();
        },
    );
    let observed = Arc::new(Mutex::new(Vec::new()));
    let log = observed.clone();
    let connection = Arc::new(Connection::with_handler(reader, writer, move |received| {
        if let Received::Response(_, response) = received {
            log.lock().unwrap().push(response.clone());
        }
    }));
    let pending = connection.clone();
    let worker = thread::spawn(move || pending.list_devices());
    wait.recv_timeout(Duration::from_secs(1)).unwrap();
    connection.close();
    assert!(matches!(worker.join().unwrap(), Err(Error::Closed)));
    assert!(observed.lock().unwrap().is_empty());
}

#[test]
fn the_request_limit_includes_the_envelope_and_profile_rule_changes_are_not_split() {
    let (reader, writer, received, _out) = dongle(|_| {}, |_, out| out.respond(None));
    let (connection, _events) = Connection::new(reader, writer);
    let mut update = p::SetAdapter {
        name: Some(String::new()),
        ..Default::default()
    };
    loop {
        let request = p::Request {
            command: Some(Command::SetAdapter(update.clone())),
        };
        if request.encoded_len() == MAX_REQUEST_BYTES {
            break;
        }
        update.name.as_mut().unwrap().push('x');
    }
    connection.set_adapter(update.clone()).unwrap();
    let accepted = received.lock().unwrap().clone();
    update.name.as_mut().unwrap().push('x');
    assert!(matches!(
        connection.set_adapter(update),
        Err(Error::TooLong)
    ));
    let change = p::ProfileRuleChange {
        change: Some(p::profile_rule_change::Change::Forget(p::ProfileRuleRef {
            input: Some(p::Usage {
                usage_page: 9,
                usage: 1,
            }),
            collection: Some(p::Usage {
                usage_page: 1,
                usage: 5,
            }),
        })),
    };
    assert!(matches!(
        connection.set_profile_rules(7, vec![change; 100]),
        Err(Error::TooLong)
    ));
    assert_eq!(*received.lock().unwrap(), accepted);
}

fn next_frame(reader: &mut impl Read) -> Vec<u8> {
    let deadline = Instant::now() + TEST_DEADLINE;
    let mut decoder = Decoder::new(None);
    loop {
        assert!(Instant::now() < deadline, "frame did not arrive");
        let mut byte = [0];
        match reader.read(&mut byte) {
            Ok(0) => panic!("stream ended before frame"),
            Ok(_) => {
                if let Some(frame) = decoder.push(byte[0]) {
                    return frame.unwrap().to_vec();
                }
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => panic!("{error}"),
        }
    }
}

#[test]
fn concurrent_first_mutation_and_probe_share_one_hello_without_observing_it() {
    let (mut adapter, reader) = pipe();
    let (writer, mut requests) = pipe();
    let seen = Arc::new(AtomicUsize::new(0));
    let events_seen = seen.clone();
    let connection = Arc::new(Connection::with_handler(reader, writer, move |received| {
        if !matches!(received, Received::Closed(_)) {
            events_seen.fetch_add(1, Ordering::Relaxed);
        }
    }));
    let mutate = {
        let c = connection.clone();
        background(move || {
            c.set_adapter(p::SetAdapter {
                name: Some("Desk".into()),
                ..Default::default()
            })
        })
    };
    assert_eq!(
        p::handshake::decode_client_hello(&next_frame(&mut requests)).unwrap(),
        p::ClientHello {}
    );
    let probe = {
        let c = connection.clone();
        background(move || c.handshake())
    };
    let status = {
        let c = connection.clone();
        background(move || c.status())
    };
    let mut byte = [0];
    assert_eq!(
        requests.read(&mut byte).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    adapter.write_all(&adapter_hello_frame()).unwrap();
    let hello = probe.recv_timeout(TEST_DEADLINE).unwrap().unwrap();
    assert!(hello.hello.serial_number.is_none());
    assert_eq!(seen.load(Ordering::Relaxed), 0);
    assert!(matches!(
        p::Request::decode(next_frame(&mut requests).as_slice())
            .unwrap()
            .command,
        Some(Command::SetAdapter(_))
    ));
    let mut out = Out(adapter);
    out.respond(None);
    mutate.recv_timeout(TEST_DEADLINE).unwrap().unwrap();
    assert!(matches!(
        p::Request::decode(next_frame(&mut requests).as_slice())
            .unwrap()
            .command,
        Some(Command::GetStatus(_))
    ));
    out.respond(Some(response::Result::Status(status_for_identity())));
    assert_eq!(
        status.recv_timeout(TEST_DEADLINE).unwrap().unwrap().id,
        "status-identity"
    );
}

fn status_for_identity() -> p::Status {
    p::Status {
        id: "status-identity".into(),
        ..Default::default()
    }
}

#[test]
fn a_newer_peer_keeps_original_metadata_and_never_receives_the_first_write() {
    let (mut adapter, reader) = pipe();
    let (writer, mut requests) = pipe();
    let (connection, events) = Connection::new(reader, writer);
    let connection = Arc::new(connection);
    let write = {
        let c = connection.clone();
        background(move || c.set_adapter(p::SetAdapter::default()))
    };
    next_frame(&mut requests);
    let hello = p::AdapterHello {
        protocol_version: Some(p::CURRENT_PROTOCOL_VERSION + 1),
        firmware_version: Some("fixture".into()),
        name: Some("Desk".into()),
        ..Default::default()
    };
    let mut bytes = p::handshake::encode_adapter_hello(&hello).unwrap();
    bytes.extend_from_slice(&[0x98, 0x06, 0x01]);
    let mut framed = Vec::new();
    frame::encode_bytes(&bytes, &mut framed);
    adapter.write_all(&framed).unwrap();
    let Err(Error::Incompatible(native)) = write.recv_timeout(TEST_DEADLINE).unwrap() else {
        panic!("expected compatibility refusal");
    };
    assert_eq!(native.hello, hello);
    assert_eq!(native.bytes, bytes);
    assert_eq!(connection.handshake().unwrap(), native);
    assert!(matches!(
        connection.stop_scan(),
        Err(Error::Incompatible(_))
    ));
    let mut byte = [0];
    assert_eq!(
        requests.read(&mut byte).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn malformed_or_oversized_hello_fails_before_an_ordinary_request() {
    for bytes in [
        vec![],
        vec![8, 0],
        vec![8, 1, 8, 1],
        vec![0xff],
        vec![1; p::MAX_HANDSHAKE_BYTES + 1],
    ] {
        let (mut adapter, reader) = pipe();
        let (writer, mut requests) = pipe();
        let (connection, _) = Connection::new(reader, writer);
        let connection = Arc::new(connection);
        let result = {
            let c = connection.clone();
            background(move || c.stop_scan())
        };
        next_frame(&mut requests);
        let mut frame = Vec::new();
        frame::encode_bytes(&bytes, &mut frame);
        adapter.write_all(&frame).unwrap();
        assert!(
            matches!(result.recv_timeout(TEST_DEADLINE).unwrap(), Err(Error::Handshake(error)) if matches!(*error, Error::Protocol))
        );
        assert_eq!(requests.read(&mut [0]).unwrap(), 0);
    }
}

#[test]
fn handshake_has_an_absolute_deadline_despite_a_garbage_trickle_and_no_request_timeout() {
    let (mut adapter, reader) = pipe();
    let (writer, mut requests) = pipe();
    let (connection, _) = Connection::new(reader, writer);
    connection.set_timeout(None);
    let connection = Arc::new(connection);
    let started = Instant::now();
    let result = {
        let c = connection.clone();
        background(move || c.stop_scan())
    };
    next_frame(&mut requests);
    let trickle = thread::spawn(move || {
        for _ in 0..100 {
            if adapter.write_all(&[1]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    });
    assert!(
        matches!(result.recv_timeout(TEST_DEADLINE).unwrap(), Err(Error::Handshake(error)) if matches!(*error, Error::Timeout))
    );
    assert!(started.elapsed() >= cordial_client::HANDSHAKE_TIMEOUT);
    assert!(started.elapsed() < cordial_client::HANDSHAKE_TIMEOUT + Duration::from_secs(1));
    trickle.join().unwrap();
}

#[test]
fn close_cancels_negotiation_and_releases_the_stream() {
    let (_adapter, reader) = pipe();
    let (writer, mut requests) = pipe();
    let (connection, _) = Connection::new(reader, writer);
    let connection = Arc::new(connection);
    let probe = {
        let c = connection.clone();
        background(move || c.handshake())
    };
    next_frame(&mut requests);
    let started = Instant::now();
    connection.close();
    assert!(
        matches!(probe.recv_timeout(TEST_DEADLINE).unwrap(), Err(Error::Handshake(error)) if matches!(*error, Error::Closed))
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(requests.read(&mut [0]).unwrap(), 0);
}

#[test]
fn discovery_ignores_ordinary_envelopes_until_a_valid_identity_selects_the_codec() {
    for version in [p::CURRENT_PROTOCOL_VERSION, p::CURRENT_PROTOCOL_VERSION + 1] {
        let (mut adapter, reader) = pipe();
        let (writer, mut requests) = pipe();
        let (connection, events) = Connection::new(reader, writer);
        let connection = Arc::new(connection);
        let result = {
            let connection = connection.clone();
            background(move || connection.stop_scan())
        };
        assert!(next_frame(&mut requests).is_empty());
        let mut ordinary = vec![];
        // The payload deliberately cannot be decoded by the current operational codec.
        prost::encoding::bytes::encode(2, &vec![0xff; 100_000], &mut ordinary);
        let mut framed = vec![];
        frame::encode_bytes(&ordinary, &mut framed);
        adapter.write_all(&framed).unwrap();
        assert_eq!(
            requests.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(events.try_recv().is_err());
        let mut framed = vec![];
        frame::encode(
            &p::AdapterHello {
                protocol_version: Some(version),
                ..Default::default()
            },
            &mut framed,
        );
        adapter.write_all(&framed).unwrap();
        if version == p::CURRENT_PROTOCOL_VERSION {
            assert!(matches!(
                p::Request::decode(next_frame(&mut requests).as_slice())
                    .unwrap()
                    .command,
                Some(Command::StopScan(_))
            ));
            Out(adapter).respond(None);
            result.recv_timeout(TEST_DEADLINE).unwrap().unwrap();
        } else {
            assert!(matches!(
                result.recv_timeout(TEST_DEADLINE).unwrap(),
                Err(Error::Incompatible(_))
            ));
            assert_eq!(
                requests.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::TimedOut
            );
        }
    }
}

#[test]
fn ordinary_events_do_not_extend_the_identity_deadline() {
    let (mut adapter, reader) = pipe();
    let (writer, mut requests) = pipe();
    let (connection, events) = Connection::new(reader, writer);
    connection.set_timeout(None);
    let started = Instant::now();
    let result = background(move || connection.stop_scan());
    assert!(next_frame(&mut requests).is_empty());
    let incoming = thread::spawn(move || {
        let mut ordinary = vec![];
        frame::encode_bytes(&[0x12, 1, 0xff], &mut ordinary);
        for _ in 0..100 {
            if adapter.write_all(&ordinary).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    });
    assert!(
        matches!(result.recv_timeout(TEST_DEADLINE).unwrap(), Err(Error::Handshake(error))
        if matches!(*error, Error::Timeout))
    );
    assert!(started.elapsed() >= cordial_client::HANDSHAKE_TIMEOUT);
    assert!(started.elapsed() < cordial_client::HANDSHAKE_TIMEOUT + Duration::from_secs(1));
    assert_eq!(requests.read(&mut [0]).unwrap(), 0);
    assert!(events.try_recv().is_err());
    incoming.join().unwrap();
}

#[test]
fn session_ending_waits_for_its_delimiter_and_does_not_answer_a_pending_write() {
    let ending = p::event::Kind::SessionEnding(p::SessionEnding { reason: 12345 });
    let mut frame = framed(message::Kind::Event(p::Event {
        kind: Some(ending.clone()),
    }));
    let delimiter = frame.pop().unwrap();
    assert_eq!(delimiter, frame::DELIMITER);
    let (release, wait) = mpsc::channel();
    let (reader, writer) = scripted([
        ReadStep::WaitForWrites(1),
        bytes(framed(message::Kind::Response(p::Response {
            result: Some(response::Result::Status(status("Desk"))),
        }))),
        ReadStep::WaitForWrites(2),
        bytes(frame),
        ReadStep::Wait(wait),
        bytes(vec![delimiter]),
        ReadStep::Fail(io::ErrorKind::BrokenPipe),
    ]);
    let (connection, events) = Connection::new(reader, writer);
    connection.status().unwrap();
    let connection = Arc::new(connection);
    let writing = connection.clone();
    let result = background(move || {
        writing.request(Command::SetAdapter(p::SetAdapter {
            name: Some("Studio".into()),
            ..Default::default()
        }))
    });
    assert!(events.try_recv().is_err());
    release.send(()).unwrap();
    assert_eq!(
        events.recv_timeout(TEST_DEADLINE).unwrap().kind,
        Some(ending)
    );
    assert!(result.recv_timeout(TEST_DEADLINE).unwrap().is_err());
    connection.close();
}
