//! Private framed transport used by the desktop app's compatibility helper.
use cordial_client::{
    MAX_RECEIVED_FRAME_BYTES,
    compatibility::{Compatibility, NativeHello, is_ordinary_message},
    serial,
};
use cordial_protocol::{
    self as p,
    frame::{self, Decoder},
    handshake,
};
use prost::Message as _;
use serde_json::json;
use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

const STARTUP_LIMIT: usize = 4096;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(4);
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

struct Arguments {
    port: String,
    compatible: bool,
}

fn arguments(args: &[String]) -> Result<Arguments, &'static str> {
    let [
        port_flag,
        port,
        command,
        protocol_flag,
        protocol,
        schema_flag,
        schema,
    ] = args
    else {
        return Err("invalid proxy arguments");
    };
    if port_flag != "--port"
        || port.is_empty()
        || port.starts_with('-')
        || command != "proxy"
        || protocol_flag != "--protocol"
        || schema_flag != "--schema"
        || protocol.is_empty()
        || !protocol.bytes().all(|b| b.is_ascii_digit())
        || schema.len() != 64
        || !schema
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("invalid proxy arguments");
    }
    let protocol: u32 = protocol.parse().map_err(|_| "invalid proxy arguments")?;
    if protocol == 0 {
        return Err("invalid proxy arguments");
    }
    Ok(Arguments {
        port: port.clone(),
        compatible: protocol == p::CURRENT_PROTOCOL_VERSION
            && schema == p::OPERATIONAL_SCHEMA_SHA256,
    })
}

fn startup(result: &str, hello: Option<&NativeHello>) -> Vec<u8> {
    let hex = hello.map(|hello| {
        hello
            .bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    });
    let bytes = serde_json::to_vec(&json!({
        "kind": "cordial-proxy-startup", "protocol": p::CURRENT_PROTOCOL_VERSION,
        "schema": p::OPERATIONAL_SCHEMA_SHA256, "result": result, "nativeHello": hex,
    }))
    .expect("startup JSON is valid");
    assert!(bytes.len() <= STARTUP_LIMIT);
    let mut framed = Vec::new();
    frame::encode_bytes(&bytes, &mut framed);
    framed
}

/// Dispatches before shell, script, TUI, or ordinary option parsing.
pub fn dispatch(args: &[String]) -> Option<bool> {
    let command = cordial::runner::Options::parse(args.iter().cloned())
        .ok()
        .and_then(|options| options.args.first().cloned());
    let positioned = args.first().is_some_and(|arg| arg == "proxy")
        || (args.first().is_some_and(|arg| arg == "--port")
            && args.get(2).is_some_and(|arg| arg == "proxy"));
    if !positioned && command.as_deref() != Some("proxy") {
        return None;
    }
    let args = match arguments(args) {
        Ok(args) => args,
        Err(error) => {
            let _ = io::stdout().write_all(&startup("incompatible-client", None));
            eprintln!("{error}");
            return Some(false);
        }
    };
    if !args.compatible {
        let _ = io::stdout().write_all(&startup("incompatible-client", None));
        return Some(false);
    }
    Some(run(args))
}

struct Input {
    chunks: Receiver<Vec<u8>>,
    ended: Arc<AtomicBool>,
}

fn input(mut reader: impl Read + Send + 'static) -> Input {
    let (tx, chunks) = mpsc::sync_channel(1);
    let ended = Arc::new(AtomicBool::new(false));
    let eof = ended.clone();
    thread::spawn(move || {
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        eof.store(true, Ordering::Release);
    });
    Input { chunks, ended }
}

struct Output {
    chunks: SyncSender<Vec<u8>>,
    failed: Arc<AtomicBool>,
    queued: AtomicUsize,
    written: Arc<AtomicUsize>,
    progress: Arc<AtomicUsize>,
}

fn output(mut writer: impl Write + Send + 'static) -> Output {
    let (chunks, rx) = mpsc::sync_channel::<Vec<u8>>(1);
    let failed = Arc::new(AtomicBool::new(false));
    let broken = failed.clone();
    let written = Arc::new(AtomicUsize::new(0));
    let completed = written.clone();
    let progress = Arc::new(AtomicUsize::new(0));
    let advanced = progress.clone();
    thread::spawn(move || {
        for bytes in rx {
            let mut pending = bytes.as_slice();
            while !pending.is_empty() {
                match writer.write(pending) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending = &pending[n..];
                        advanced.fetch_add(n, Ordering::Release);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if !pending.is_empty() || writer.flush().is_err() {
                broken.store(true, Ordering::Release);
                return;
            }
            completed.fetch_add(1, Ordering::Release);
        }
    });
    Output {
        chunks,
        failed,
        queued: AtomicUsize::new(0),
        written,
        progress,
    }
}

fn emit(out: &Output, input: &Input, bytes: Vec<u8>, deadline: Instant) -> io::Result<()> {
    let mut pending = bytes;
    loop {
        if input.ended.load(Ordering::Acquire)
            || out.failed.load(Ordering::Acquire)
            || Instant::now() >= deadline
        {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        match out.chunks.try_send(pending) {
            Ok(()) => {
                let target = out.queued.fetch_add(1, Ordering::Relaxed) + 1;
                wait_written(out, input, target, deadline)?;
                return Ok(());
            }
            Err(TrySendError::Full(bytes)) => pending = bytes,
            Err(TrySendError::Disconnected(_)) => return Err(io::ErrorKind::BrokenPipe.into()),
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_written(out: &Output, input: &Input, target: usize, deadline: Instant) -> io::Result<()> {
    while out.written.load(Ordering::Acquire) < target {
        if input.ended.load(Ordering::Acquire)
            || out.failed.load(Ordering::Acquire)
            || Instant::now() >= deadline
        {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn write_native(
    writer: &mut impl Write,
    bytes: &[u8],
    input: &Input,
    deadline: Instant,
) -> io::Result<()> {
    let mut bytes = bytes;
    while !bytes.is_empty() {
        if input.ended.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match writer.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

struct NativeInput {
    chunks: Receiver<Vec<u8>>,
    ended: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    reader: Option<thread::JoinHandle<()>>,
}

impl NativeInput {
    fn new(mut reader: impl Read + Send + 'static) -> Self {
        let (tx, chunks) = mpsc::sync_channel(1);
        let ended = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (eof, stopping) = (ended.clone(), stop.clone());
        let reader = thread::spawn(move || {
            let mut buffer = [0; 4096];
            'read: while !stopping.load(Ordering::Acquire) {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut bytes = buffer[..n].to_vec();
                        loop {
                            match tx.try_send(bytes) {
                                Ok(()) => break,
                                Err(TrySendError::Full(pending)) => bytes = pending,
                                Err(TrySendError::Disconnected(_)) => break 'read,
                            }
                            if stopping.load(Ordering::Acquire) {
                                break 'read;
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
            }
            drop(reader);
            eof.store(true, Ordering::Release);
        });
        Self {
            chunks,
            ended,
            stop,
            reader: Some(reader),
        }
    }
}

impl Drop for NativeInput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn run(args: Arguments) -> bool {
    // Stdin is monitored before the physical port is opened, including during negotiation.
    let input = input(io::stdin());
    let output = output(io::stdout());
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let (reader, mut writer) = match serial::open_stream(&args.port) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = emit(&output, &input, startup("open-failed", None), deadline);
            eprintln!("the adapter couldn't open: {error}");
            return false;
        }
    };
    let native = NativeInput::new(reader);
    let result = session(&input, &output, &native, &mut writer, deadline);
    // The physical reader and writer are released before process exit, with no reopen path.
    drop(native);
    drop(writer);
    if let Err(error) = result {
        eprintln!("the adapter proxy closed: {error}");
    }
    false
}

fn receive(
    input: &Input,
    output: &Output,
    native: &NativeInput,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    loop {
        if input.ended.load(Ordering::Acquire) || output.failed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match native.chunks.try_recv() {
            Ok(bytes) => return Ok(bytes),
            Err(TryRecvError::Disconnected) => return Err(io::ErrorKind::BrokenPipe.into()),
            Err(TryRecvError::Empty) => {
                if native.ended.load(Ordering::Acquire) {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn session(
    input: &Input,
    output: &Output,
    native: &NativeInput,
    writer: &mut impl Write,
    deadline: Instant,
) -> io::Result<()> {
    let mut negotiate = || -> io::Result<(NativeHello, Vec<u8>)> {
        let mut hello = vec![frame::DELIMITER];
        frame::encode_bytes(
            &handshake::encode_client_hello(&p::ClientHello {}).unwrap(),
            &mut hello,
        );
        write_native(writer, &hello, input, deadline)?;
        let mut decoder = Decoder::new(Some(MAX_RECEIVED_FRAME_BYTES));
        loop {
            let received = receive(input, output, native, deadline)?;
            for (index, byte) in received.iter().enumerate() {
                if let Some(frame) = decoder.push(*byte) {
                    let bytes = frame.map_err(|_| io::ErrorKind::InvalidData)?;
                    let hello = match NativeHello::decode(bytes) {
                        Ok(hello) => hello,
                        Err(_) if is_ordinary_message(bytes) => continue,
                        Err(_) => return Err(io::ErrorKind::InvalidData.into()),
                    };
                    return Ok((hello, received[index + 1..].to_vec()));
                }
            }
        }
    };
    let (hello, tail) = match negotiate() {
        Ok(hello) => hello,
        Err(error) => {
            let _ = emit(
                output,
                input,
                startup("handshake-failed", None),
                Instant::now() + Duration::from_millis(500),
            );
            return Err(error);
        }
    };
    if hello.compatibility() != Compatibility::Current {
        emit(
            output,
            input,
            startup("unsupported-native", Some(&hello)),
            deadline,
        )?;
        return Err(io::ErrorKind::Unsupported.into());
    }
    if native.ended.load(Ordering::Acquire) {
        emit(
            output,
            input,
            startup("handshake-failed", Some(&hello)),
            deadline,
        )?;
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    emit(output, input, startup("ready", Some(&hello)), deadline)?;
    let mut decoder = Decoder::new(Some(p::MAX_HANDSHAKE_BYTES));
    let mut virtual_ready = false;
    let mut request = Vec::new();
    let mut request_decoder = Decoder::new(Some(p::MAX_REQUEST_BYTES));
    let virtual_deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut pending_output = (!tail.is_empty()).then_some(tail);
    let mut output_progress = output.progress.load(Ordering::Acquire);
    let mut output_deadline = Instant::now() + WRITE_TIMEOUT;
    loop {
        if input.ended.load(Ordering::Acquire) || output.failed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if native.ended.load(Ordering::Acquire) {
            if virtual_ready {
                // Deliver bytes already read before reporting the physical stream's end.
                let deadline = Instant::now() + Duration::from_secs(1);
                if let Some(bytes) = pending_output.take() {
                    emit(output, input, bytes, deadline)?;
                }
                while let Ok(bytes) = native.chunks.try_recv() {
                    emit(output, input, bytes, deadline)?;
                }
                wait_written(
                    output,
                    input,
                    output.queued.load(Ordering::Relaxed),
                    deadline,
                )?;
            }
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if !virtual_ready && Instant::now() >= virtual_deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let progress = output.progress.load(Ordering::Acquire);
        if progress != output_progress
            || (pending_output.is_none()
                && output.queued.load(Ordering::Relaxed) == output.written.load(Ordering::Acquire))
        {
            output_progress = progress;
            output_deadline = Instant::now() + WRITE_TIMEOUT;
        } else if virtual_ready && Instant::now() >= output_deadline {
            // Backpressure can prevent the native reader from observing a physical disconnect.
            return Err(io::ErrorKind::TimedOut.into());
        }
        if virtual_ready && let Some(bytes) = pending_output.take() {
            match output.chunks.try_send(bytes) {
                Ok(()) => {
                    output.queued.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Full(bytes)) => pending_output = Some(bytes),
                Err(TrySendError::Disconnected(_)) => return Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
        if virtual_ready && pending_output.is_none() {
            match native.chunks.try_recv() {
                Ok(bytes) => pending_output = Some(bytes),
                Err(TryRecvError::Disconnected) => continue,
                Err(TryRecvError::Empty) => {}
            }
        }
        match input.chunks.try_recv() {
            Ok(bytes) => {
                for byte in bytes {
                    if !virtual_ready {
                        if let Some(frame) = decoder.push(byte) {
                            handshake::decode_client_hello(
                                frame.map_err(|_| io::ErrorKind::InvalidData)?,
                            )
                            .map_err(|_| io::ErrorKind::InvalidData)?;
                            let mut framed = Vec::new();
                            frame::encode_bytes(&hello.bytes, &mut framed);
                            emit(output, input, framed, virtual_deadline)?;
                            virtual_ready = true;
                        }
                    } else {
                        // Buffer one complete bounded request so malformed input has no physical effect.
                        if request.is_empty() && byte == frame::DELIMITER {
                            continue;
                        }
                        if request.len() >= p::MAX_REQUEST_BYTES + p::MAX_REQUEST_BYTES / 254 + 2 {
                            return Err(io::ErrorKind::InvalidData.into());
                        }
                        request.push(byte);
                        if let Some(decoded) = request_decoder.push(byte) {
                            let request_bytes = decoded.map_err(|_| io::ErrorKind::InvalidData)?;
                            if !request_bytes.is_empty() {
                                let message = p::Request::decode(request_bytes)
                                    .map_err(|_| io::ErrorKind::InvalidData)?;
                                if message.command.is_none() {
                                    return Err(io::ErrorKind::InvalidData.into());
                                }
                            }
                            write_native(writer, &request, input, Instant::now() + WRITE_TIMEOUT)?;
                            request.clear();
                        }
                    }
                }
            }
            Err(TryRecvError::Disconnected) => return Err(io::ErrorKind::BrokenPipe.into()),
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::Shutdown, os::unix::net::UnixStream};

    fn frame(bytes: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame::encode_bytes(bytes, &mut frame);
        frame
    }

    fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut decoder = Decoder::new(None);
        loop {
            assert!(Instant::now() < deadline, "frame did not arrive");
            let mut byte = [0];
            match stream.read(&mut byte) {
                Ok(0) => panic!("stream ended"),
                Ok(_) => {
                    if let Some(frame) = decoder.push(byte[0]) {
                        return frame.unwrap().to_vec();
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                Err(e) => panic!("{e}"),
            }
        }
    }

    struct Fixture {
        app: UnixStream,
        adapter: UnixStream,
        done: Receiver<io::Result<()>>,
    }

    fn fixture(timeout: Duration) -> Fixture {
        fixture_with_output(timeout, |writer| writer)
    }

    fn fixture_with_output<W: Write + Send + 'static>(
        timeout: Duration,
        output_writer: impl FnOnce(UnixStream) -> W + Send + 'static,
    ) -> Fixture {
        let (app, proxy_input) = UnixStream::pair().unwrap();
        let proxy_output = proxy_input.try_clone().unwrap();
        let stop_input = proxy_input.try_clone().unwrap();
        let (adapter, native_writer) = UnixStream::pair().unwrap();
        let native_reader = native_writer.try_clone().unwrap();
        for stream in [&app, &adapter, &native_reader] {
            stream
                .set_read_timeout(Some(Duration::from_millis(10)))
                .unwrap();
        }
        native_writer
            .set_write_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        let (done, result) = mpsc::channel();
        thread::spawn(move || {
            let input = input(proxy_input);
            let output = output(output_writer(proxy_output));
            let native = NativeInput::new(native_reader);
            let mut writer = native_writer;
            let result = session(
                &input,
                &output,
                &native,
                &mut writer,
                Instant::now() + timeout,
            );
            drop(native);
            drop(writer);
            drop(output);
            stop_input.shutdown(Shutdown::Both).unwrap();
            let _ = done.send(result);
        });
        Fixture {
            app,
            adapter,
            done: result,
        }
    }

    fn hello(version: u32) -> Vec<u8> {
        let mut bytes = handshake::encode_adapter_hello(&p::AdapterHello {
            protocol_version: Some(version),
            firmware_version: Some("fixture".into()),
            ..Default::default()
        })
        .unwrap();
        bytes.extend_from_slice(&[0x98, 0x06, 1]);
        bytes
    }

    fn ready(f: &mut Fixture) -> Vec<u8> {
        assert_eq!(
            handshake::decode_client_hello(&read_frame(&mut f.adapter)).unwrap(),
            p::ClientHello {}
        );
        let hello = hello(p::CURRENT_PROTOCOL_VERSION);
        f.adapter.write_all(&frame(&hello)).unwrap();
        let startup: serde_json::Value = serde_json::from_slice(&read_frame(&mut f.app)).unwrap();
        assert_eq!(startup["result"], "ready");
        assert_eq!(startup["protocol"], p::CURRENT_PROTOCOL_VERSION);
        assert_eq!(startup["schema"], p::OPERATIONAL_SCHEMA_SHA256);
        hello
    }

    fn virtual_hello(app: &mut UnixStream) {
        app.write_all(&frame(
            &handshake::encode_client_hello(&p::ClientHello {}).unwrap(),
        ))
        .unwrap();
    }

    #[test]
    fn ready_then_virtual_hello_preserves_tail_events_requests_and_large_responses() {
        let mut f = fixture(STARTUP_TIMEOUT);
        read_frame(&mut f.adapter);
        let hello = hello(p::CURRENT_PROTOCOL_VERSION);
        let event = p::Message {
            kind: Some(p::message::Kind::Event(p::Event {
                kind: Some(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 7 })),
            })),
        }
        .encode_to_vec();
        let mut native = frame(&hello);
        native.extend(frame(&event));
        f.adapter.write_all(&native).unwrap();
        let startup: serde_json::Value = serde_json::from_slice(&read_frame(&mut f.app)).unwrap();
        assert_eq!(startup["result"], "ready");
        assert_eq!(
            startup["nativeHello"],
            hello.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        assert!(matches!(
            f.app.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        let request = p::Request {
            command: Some(p::request::Command::SetAdapter(p::SetAdapter {
                name: Some("Desk".into()),
                ..Default::default()
            })),
        }
        .encode_to_vec();
        let mut input = frame(&handshake::encode_client_hello(&p::ClientHello {}).unwrap());
        input.extend(frame(&request));
        f.app.write_all(&input).unwrap();
        assert_eq!(read_frame(&mut f.app), hello);
        assert_eq!(read_frame(&mut f.app), event);
        assert_eq!(read_frame(&mut f.adapter), request);
        let response = p::Message {
            kind: Some(p::message::Kind::Response(p::Response {
                result: Some(p::response::Result::File(p::FileData {
                    data: [0, 1, 0xfe, 0xff].repeat(100_000),
                    ..Default::default()
                })),
            })),
        }
        .encode_to_vec();
        f.adapter.write_all(&frame(&response)).unwrap();
        assert_eq!(read_frame(&mut f.app), response);
        f.adapter.shutdown(Shutdown::Both).unwrap();
        assert!(
            f.done
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn native_disconnect_exits_with_no_reopen_or_replay_when_idle_or_pending() {
        for pending in [false, true] {
            let mut f = fixture(STARTUP_TIMEOUT);
            let hello = ready(&mut f);
            virtual_hello(&mut f.app);
            assert_eq!(read_frame(&mut f.app), hello);
            if pending {
                let request = p::Request {
                    command: Some(p::request::Command::SetAdapter(p::SetAdapter::default())),
                }
                .encode_to_vec();
                f.app.write_all(&frame(&request)).unwrap();
                assert_eq!(read_frame(&mut f.adapter), request);
            }
            f.adapter.shutdown(Shutdown::Both).unwrap();
            assert!(
                f.done
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
            assert_eq!(f.app.read(&mut [0]).unwrap(), 0);
        }
    }

    #[test]
    fn stdin_eof_releases_the_native_stream_during_startup_or_after_readiness() {
        for stage in 0..3 {
            let mut f = fixture(STARTUP_TIMEOUT);
            if stage == 0 {
                read_frame(&mut f.adapter);
            } else {
                let hello = ready(&mut f);
                if stage == 2 {
                    virtual_hello(&mut f.app);
                    assert_eq!(read_frame(&mut f.app), hello);
                }
            }
            f.app.shutdown(Shutdown::Both).unwrap();
            assert!(
                f.done
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
            assert_eq!(f.adapter.read(&mut [0]).unwrap(), 0);
        }
    }

    #[test]
    fn nonempty_virtual_hello_never_reaches_the_adapter() {
        for bytes in [
            vec![8, 0],
            vec![8, 1],
            vec![8, 1, 8, 1],
            vec![8, 2],
            vec![0x0a, 0],
        ] {
            let mut f = fixture(STARTUP_TIMEOUT);
            ready(&mut f);
            f.app.write_all(&frame(&bytes)).unwrap();
            assert!(
                f.done
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
            assert_eq!(f.adapter.read(&mut [0]).unwrap(), 0);
        }
    }

    #[test]
    fn invalid_or_oversized_requests_have_no_physical_effect() {
        for bytes in [
            vec![0x98, 0x06, 1],
            vec![0xff],
            vec![0x12, 1, 0xff],
            vec![1; p::MAX_REQUEST_BYTES + 1],
        ] {
            let mut f = fixture(STARTUP_TIMEOUT);
            ready(&mut f);
            virtual_hello(&mut f.app);
            read_frame(&mut f.app);
            f.app.write_all(&frame(&bytes)).unwrap();
            assert!(
                f.done
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
            assert_eq!(f.adapter.read(&mut [0]).unwrap(), 0);
        }
    }

    #[test]
    fn discovery_skips_unknown_event_payloads_and_requires_a_native_hello() {
        for version in [p::CURRENT_PROTOCOL_VERSION, p::CURRENT_PROTOCOL_VERSION + 1] {
            let mut f = fixture(STARTUP_TIMEOUT);
            assert!(read_frame(&mut f.adapter).is_empty());
            let mut event = vec![];
            prost::encoding::bytes::encode(2, &vec![0xff; 100_000], &mut event);
            f.adapter.write_all(&frame(&event)).unwrap();
            assert!(matches!(
                f.adapter.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ));
            f.adapter.write_all(&frame(&hello(version))).unwrap();
            let startup: serde_json::Value =
                serde_json::from_slice(&read_frame(&mut f.app)).unwrap();
            assert_eq!(
                startup["result"],
                if version == p::CURRENT_PROTOCOL_VERSION {
                    "ready"
                } else {
                    "unsupported-native"
                }
            );
            if version == p::CURRENT_PROTOCOL_VERSION {
                virtual_hello(&mut f.app);
                assert_eq!(read_frame(&mut f.app), hello(version));
                virtual_hello(&mut f.app);
                assert!(read_frame(&mut f.adapter).is_empty());
                f.adapter.write_all(&frame(&hello(version))).unwrap();
                assert_eq!(read_frame(&mut f.app), hello(version));
            } else {
                assert!(
                    f.done
                        .recv_timeout(Duration::from_secs(1))
                        .unwrap()
                        .is_err()
                );
                assert_eq!(f.adapter.read(&mut [0]).unwrap(), 0);
            }
        }
    }

    #[test]
    fn unsolicited_events_do_not_extend_the_native_hello_deadline() {
        let mut f = fixture(Duration::from_millis(100));
        assert!(read_frame(&mut f.adapter).is_empty());
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(150) {
            if f.adapter.write_all(&frame(&[0x12, 1, 0xff])).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let startup: serde_json::Value = serde_json::from_slice(&read_frame(&mut f.app)).unwrap();
        assert_eq!(startup["result"], "handshake-failed");
        assert!(
            f.done
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn native_refusal_and_handshake_failure_are_framed_startup_errors() {
        for bytes in [
            hello(p::CURRENT_PROTOCOL_VERSION + 1),
            vec![8, 0],
            vec![1; p::MAX_HANDSHAKE_BYTES + 1],
        ] {
            let mut f = fixture(STARTUP_TIMEOUT);
            read_frame(&mut f.adapter);
            f.adapter.write_all(&frame(&bytes)).unwrap();
            let startup: serde_json::Value =
                serde_json::from_slice(&read_frame(&mut f.app)).unwrap();
            assert_eq!(
                startup["result"],
                if bytes[1] > 1 && bytes.len() < p::MAX_HANDSHAKE_BYTES {
                    "unsupported-native"
                } else {
                    "handshake-failed"
                }
            );
            assert!(
                f.done
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
            assert_eq!(f.adapter.read(&mut [0]).unwrap(), 0);
        }
    }

    #[test]
    fn native_handshake_deadline_does_not_extend_with_garbage_progress() {
        let mut f = fixture(Duration::from_millis(100));
        read_frame(&mut f.adapter);
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(150) {
            if f.adapter.write_all(&[1]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let startup: serde_json::Value = serde_json::from_slice(&read_frame(&mut f.app)).unwrap();
        assert_eq!(startup["result"], "handshake-failed");
        assert!(
            f.done
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn stdout_backpressure_does_not_hold_the_port_after_stdin_eof() {
        let mut f = fixture(STARTUP_TIMEOUT);
        ready(&mut f);
        virtual_hello(&mut f.app);
        read_frame(&mut f.app);
        let mut adapter_writer = f.adapter.try_clone().unwrap();
        adapter_writer
            .set_write_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let writer = thread::spawn(move || {
            let _ = adapter_writer.write_all(&vec![1; 2 * 1024 * 1024]);
        });
        thread::sleep(Duration::from_millis(50));
        f.app.shutdown(Shutdown::Both).unwrap();
        assert!(
            f.done
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err()
        );
        writer.join().unwrap();
    }

    #[test]
    fn output_timeout_counts_partial_progress_and_leaves_idle_sessions_open() {
        struct ControlledWriter {
            stream: UnixStream,
            initial: usize,
            permits: Receiver<usize>,
        }

        impl Write for ControlledWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let limit = if self.initial > 0 {
                    self.initial -= 1;
                    bytes.len()
                } else {
                    self.permits.recv().map_err(|_| io::ErrorKind::BrokenPipe)?
                };
                self.stream.write(&bytes[..bytes.len().min(limit)])
            }

            fn flush(&mut self) -> io::Result<()> {
                self.stream.flush()
            }
        }

        let (permits, receiver) = mpsc::channel();
        let mut f = fixture_with_output(STARTUP_TIMEOUT, move |stream| ControlledWriter {
            stream,
            initial: 2,
            permits: receiver,
        });
        ready(&mut f);
        virtual_hello(&mut f.app);
        read_frame(&mut f.app);
        f.app
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        thread::sleep(WRITE_TIMEOUT + Duration::from_millis(50));
        assert!(matches!(f.done.try_recv(), Err(TryRecvError::Empty)));

        let bytes = frame(&[1; 8]);
        f.adapter.write_all(&bytes).unwrap();
        let mut received = Vec::new();
        // One output chunk takes longer than the timeout, but each partial write advances it.
        for _ in 0..3 {
            thread::sleep(WRITE_TIMEOUT / 2);
            permits.send(3).unwrap();
            let mut partial = [0; 3];
            f.app.read_exact(&mut partial).unwrap();
            received.extend_from_slice(&partial);
            assert!(matches!(f.done.try_recv(), Err(TryRecvError::Empty)));
        }
        permits.send(bytes.len() - 9).unwrap();
        let mut last = [0];
        f.app.read_exact(&mut last).unwrap();
        received.extend_from_slice(&last);
        assert_eq!(received, bytes);

        // The writer blocks on its first write, with no full output queue or native EOF.
        f.adapter.write_all(&bytes).unwrap();
        assert_eq!(
            f.done
                .recv_timeout(WRITE_TIMEOUT + Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(f.adapter.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn bytes_received_before_native_eof_are_forwarded_before_closing() {
        let mut f = fixture(STARTUP_TIMEOUT);
        ready(&mut f);
        virtual_hello(&mut f.app);
        read_frame(&mut f.app);
        let response = p::Message {
            kind: Some(p::message::Kind::Response(p::Response::default())),
        }
        .encode_to_vec();
        f.adapter.write_all(&frame(&response)).unwrap();
        f.adapter.shutdown(Shutdown::Write).unwrap();
        assert_eq!(read_frame(&mut f.app), response);
        assert!(
            f.done
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn a_command_argument_named_proxy_does_not_select_the_private_mode() {
        let args = ["--port", "adapter-fixture", "name", "proxy"].map(str::to_owned);
        assert!(dispatch(&args).is_none());
    }

    /// Runs the private engine in a child process with a simulated physical stream.
    #[test]
    fn subprocess_fixture() {
        let Some(path) = std::env::var_os("CORDIAL_PROXY_TEST_STREAM") else {
            return;
        };
        let mut writer = UnixStream::connect(path).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        writer
            .set_write_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        let native = NativeInput::new(writer.try_clone().unwrap());
        let input = input(io::stdin());
        let output = output(io::stdout());
        let result = session(
            &input,
            &output,
            &native,
            &mut writer,
            Instant::now() + STARTUP_TIMEOUT,
        );
        drop(native);
        drop(writer);
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }

    struct ProcessFixture {
        child: std::process::Child,
        adapter: UnixStream,
        listener: std::os::unix::net::UnixListener,
        path: std::path::PathBuf,
        frames: Receiver<Vec<u8>>,
    }

    impl ProcessFixture {
        fn new() -> Self {
            use std::{
                os::unix::net::UnixListener,
                process::{Command, Stdio},
            };
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "cordial-proxy-{}-{}.sock",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let listener = UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "proxy::tests::subprocess_fixture",
                    "--nocapture",
                    "--format",
                    "terse",
                    "--test-threads",
                    "1",
                ])
                .env("CORDIAL_PROXY_TEST_STREAM", &path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let adapter = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => {
                        child.kill().unwrap();
                        child.wait().unwrap();
                        panic!("{error}");
                    }
                }
            };
            adapter
                .set_read_timeout(Some(Duration::from_millis(10)))
                .unwrap();
            adapter
                .set_write_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let mut stdout = child.stdout.take().unwrap();
            let (send, frames) = mpsc::sync_channel(1);
            thread::spawn(move || {
                // The test runner's prefix precedes the engine's otherwise exclusive stdout.
                let prefix = b"\nrunning 1 test\n";
                let mut actual = vec![0; prefix.len()];
                stdout.read_exact(&mut actual).unwrap();
                assert_eq!(&actual, prefix);
                let mut decoder = Decoder::new(None);
                let mut byte = [0];
                while stdout.read(&mut byte).unwrap() != 0 {
                    if let Some(frame) = decoder.push(byte[0])
                        && send.send(frame.unwrap().to_vec()).is_err()
                    {
                        break;
                    }
                }
            });
            Self {
                child,
                adapter,
                listener,
                path,
                frames,
            }
        }

        fn frame(&self) -> Vec<u8> {
            self.frames.recv_timeout(Duration::from_secs(3)).unwrap()
        }

        fn ready(&mut self) {
            assert_eq!(
                handshake::decode_client_hello(&read_frame(&mut self.adapter)).unwrap(),
                p::ClientHello {}
            );
            self.adapter
                .write_all(&frame(&hello(p::CURRENT_PROTOCOL_VERSION)))
                .unwrap();
            let startup: serde_json::Value = serde_json::from_slice(&self.frame()).unwrap();
            assert_eq!(startup["result"], "ready");
            self.child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(&frame(&[]))
                .unwrap();
            assert_eq!(
                handshake::decode_adapter_hello(&self.frame())
                    .unwrap()
                    .protocol_version,
                Some(p::CURRENT_PROTOCOL_VERSION)
            );
        }

        fn exited(&mut self) {
            let deadline = Instant::now() + WRITE_TIMEOUT + Duration::from_secs(1);
            while self.child.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "helper process did not exit");
                thread::sleep(Duration::from_millis(1));
            }
            self.child.wait().unwrap();
            match self.adapter.read(&mut [0]) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset
                            | io::ErrorKind::BrokenPipe
                            | io::ErrorKind::NotConnected
                    ) => {}
                other => panic!("physical stream stayed open: {other:?}"),
            }
            assert_eq!(
                self.listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }

    impl Drop for ProcessFixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn helper_process_exits_after_idle_or_pending_native_disconnect_without_reopening() {
        for pending in [false, true] {
            let mut f = ProcessFixture::new();
            f.ready();
            if pending {
                let request = p::Request {
                    command: Some(p::request::Command::SetAdapter(p::SetAdapter::default())),
                }
                .encode_to_vec();
                f.child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(&frame(&request))
                    .unwrap();
                assert_eq!(read_frame(&mut f.adapter), request);
            }
            f.adapter.shutdown(Shutdown::Write).unwrap();
            f.exited();
        }
    }

    #[test]
    fn helper_process_exits_on_stdin_eof_during_startup_and_after_connecting() {
        for established in [false, true] {
            let mut f = ProcessFixture::new();
            if established {
                f.ready();
            } else {
                read_frame(&mut f.adapter);
            }
            drop(f.child.stdin.take());
            f.exited();
        }
    }

    #[test]
    fn helper_process_forwards_binary_responses_events_and_original_request_bytes() {
        let mut f = ProcessFixture::new();
        f.ready();
        let mut request = p::Request {
            command: Some(p::request::Command::ReadFile(p::ReadFile {
                path: "/fixture".into(),
                ..Default::default()
            })),
        }
        .encode_to_vec();
        request.extend_from_slice(&[0x98, 0x06, 1]);
        f.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&frame(&request))
            .unwrap();
        assert_eq!(read_frame(&mut f.adapter), request);
        let response = p::Message {
            kind: Some(p::message::Kind::Response(p::Response {
                result: Some(p::response::Result::File(p::FileData {
                    data: [0, 1, 0xfe, 0xff].repeat(100_000),
                    ..Default::default()
                })),
            })),
        }
        .encode_to_vec();
        let event = p::Message {
            kind: Some(p::message::Kind::Event(p::Event {
                kind: Some(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 7 })),
            })),
        }
        .encode_to_vec();
        let mut bytes = frame(&response);
        bytes.extend(frame(&event));
        f.adapter.write_all(&bytes).unwrap();
        assert_eq!(f.frame(), response);
        assert_eq!(f.frame(), event);
        f.adapter.shutdown(Shutdown::Write).unwrap();
        f.exited();
    }

    #[test]
    fn helper_process_exits_with_a_blocked_stdout_writer_after_stdin_eof_or_native_disconnect() {
        for native_disconnect in [false, true] {
            let mut f = ProcessFixture::new();
            f.ready();
            let mut writer = f.adapter.try_clone().unwrap();
            let (blocked, waiting) = mpsc::channel();
            let write = thread::spawn(move || {
                let bytes = frame(&vec![1; 400_000]);
                for _ in 0..10 {
                    if let Err(error) = writer.write_all(&bytes) {
                        blocked
                            .send(matches!(
                                error.kind(),
                                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                            ))
                            .unwrap();
                        return;
                    }
                }
                blocked.send(false).unwrap();
            });
            assert!(
                waiting.recv_timeout(Duration::from_secs(3)).unwrap(),
                "stdout must apply backpressure before EOF"
            );
            if native_disconnect {
                f.adapter.shutdown(Shutdown::Write).unwrap();
            } else {
                drop(f.child.stdin.take());
            }
            f.exited();
            write.join().unwrap();
        }
    }

    #[test]
    fn startup_size_fits_a_maximum_native_hello() {
        let mut hello = p::AdapterHello {
            protocol_version: Some(p::CURRENT_PROTOCOL_VERSION),
            name: Some(String::new()),
            ..Default::default()
        };
        while hello.encoded_len() < p::MAX_HANDSHAKE_BYTES {
            hello.name.as_mut().unwrap().push('a');
        }
        if hello.encoded_len() > p::MAX_HANDSHAKE_BYTES {
            hello.name.as_mut().unwrap().pop();
        }
        let native = NativeHello::decode(&hello.encode_to_vec()).unwrap();
        let framed = startup("ready", Some(&native));
        let mut decoder = Decoder::new(Some(STARTUP_LIMIT));
        let bytes = framed
            .iter()
            .find_map(|b| decoder.push(*b).map(|r| r.unwrap().to_vec()))
            .unwrap();
        assert!(bytes.len() <= STARTUP_LIMIT);
    }
}
