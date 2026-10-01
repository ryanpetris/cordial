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
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

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
                    let request = p::Request::decode(frame.unwrap()).unwrap();
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

fn device(id: &str) -> p::Device {
    p::Device {
        id: id.into(),
        transport: p::Transport::Ble as i32,
        ..Default::default()
    }
}

#[test]
fn responses_match_requests_in_order_with_events_between() {
    let (reader, writer, _, _out) = dongle(
        |_| {},
        |command, out| match command {
            Command::GetStatus(_) => {
                out.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
                    id: "d_1".into(),
                }));
                out.respond(Some(response::Result::Status(status("Desk"))));
            }
            Command::ListDevices(_) => {
                out.event(p::event::Kind::Device(device("d_2")));
                out.respond(Some(response::Result::Devices(p::DeviceList {
                    devices: vec![device("d_2"), device("d_3")],
                })));
            }
            Command::StopScan(_) => out.respond(None),
            other => panic!("{other:?}"),
        },
    );
    let (connection, events) = Connection::new(reader, writer);
    assert_eq!(connection.status().unwrap().name, "Desk");
    let devices = connection.list_devices().unwrap();
    assert_eq!(
        devices.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        ["d_2", "d_3"]
    );
    connection.stop_scan().unwrap();
    // The event before the first response belongs to the leftovers the session ignores.
    let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(event.kind, Some(p::event::Kind::Device(device("d_2"))));
    assert!(events.try_recv().is_err());
}

#[test]
fn the_first_request_follows_a_delimiter_and_leftovers_are_ignored() {
    let (reader, writer, received, _out) = dongle(
        |out| {
            // A previous session's partial frame and event, then the new session's delimiter.
            out.raw(&[3, 1, 2]);
            out.raw(&[0]);
            out.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
                id: "old".into(),
            }));
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
    let mut expected = vec![frame::DELIMITER];
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
fn dongle_errors_are_error_values_and_raw_responses_keep_them() {
    let error = p::Error {
        code: p::ErrorCode::NoCapacity as i32,
        reason: p::CapacityReason::Enabled as i32,
        outcome_unknown: false,
    };
    let reply = error;
    let (reader, writer, _, _out) = dongle(
        |_| {},
        move |_, out| out.respond(Some(response::Result::Error(reply))),
    );
    let (connection, _events) = Connection::new(reader, writer);
    let result = connection.set_device(p::SetDevice {
        device: "d_1".into(),
        enabled: Some(true),
        ..Default::default()
    });
    match result {
        Err(Error::Dongle(e)) => assert_eq!(e, error),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        connection.connect_device("d_1").unwrap_err().code(),
        Some(p::ErrorCode::NoCapacity)
    );
    let response = connection
        .request(Command::GetStatus(p::GetStatus {}))
        .unwrap();
    assert_eq!(response.result, Some(response::Result::Error(error)));
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
    connection.refresh_device("d_1").unwrap();
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
                out.respond(Some(response::Result::Device(device("d_1"))));
            }
            Command::GetStatus(_) => out.respond(Some(response::Result::Status(status("B")))),
            other => panic!("{other:?}"),
        },
    );
    let (connection, _events) = Connection::new(reader, writer);
    connection.set_timeout(Some(Duration::from_millis(50)));
    assert!(matches!(connection.get_device("d_1"), Err(Error::Timeout)));
    assert!(matches!(connection.closed(), Some(Error::Timeout)));
    release.send(()).unwrap();
    assert!(matches!(connection.status(), Err(Error::Timeout)));
}

#[test]
fn a_request_over_the_frame_limit_is_not_sent() {
    let (reader, writer, received, _out) = dongle(|_| {}, |_, out| out.respond(None));
    let (connection, _events) = Connection::new(reader, writer);
    let change = p::SettingChange {
        integration: p::IntegrationKind::Hidpp as i32,
        key: "k".repeat(MAX_REQUEST_BYTES),
        value: None,
    };
    assert!(matches!(
        connection.set_settings("d_1", vec![change]),
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
    assert!(matches!(waiting.join().unwrap(), Err(Error::Closed)));
    assert!(events.recv_timeout(Duration::from_secs(1)).is_err());
    assert!(matches!(connection.closed(), Some(Error::Closed)));
    assert!(matches!(connection.list_devices(), Err(Error::Closed)));
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
    assert!(matches!(connection.status(), Err(Error::Closed)));
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
    assert!(matches!(connection.status(), Err(Error::Protocol)));
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
                    truncated: false,
                }));
                out.respond(Some(response::Result::Settings(p::DeviceSettings {
                    device: "d_1".into(),
                    settings: Vec::new(),
                })));
                out.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
                    id: "d_1".into(),
                }));
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
    let settings = connection.list_settings("d_1").unwrap();
    assert_eq!(settings.device, "d_1");
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

trait Name {
    fn as_name(&self) -> &'static str;
}

impl Name for p::event::Kind {
    fn as_name(&self) -> &'static str {
        match self {
            p::event::Kind::Adapter(_) => "adapter",
            p::event::Kind::Device(_) => "device",
            p::event::Kind::DeviceRemoved(_) => "device_removed",
            p::event::Kind::Settings(_) => "settings",
            p::event::Kind::ScanFound(_) => "scan_found",
            p::event::Kind::ScanDone(_) => "scan_done",
            p::event::Kind::Pairing(_) => "pairing",
            p::event::Kind::Warnings(_) => "warnings",
        }
    }
}
