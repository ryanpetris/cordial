//! Bounded stderr delivery for protocol records and foreground diagnostics.
use cordial_client::diagnostics::Diagnostic;
use cordial_protocol::message;
use std::{
    fmt,
    io::{self, IsTerminal, Write},
    sync::{
        Arc, Weak,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub type Observer = cordial_client::diagnostics::Sink;

/// Observes wire responses and events without changing command results.
pub fn observe(
    connector: Arc<crate::controller::Connector>,
    observer: Observer,
) -> Arc<crate::controller::Connector> {
    Arc::new(move |port, handler| {
        let connection = connector(port, handler)?;
        connection.set_diagnostics(observer.clone());
        Ok(connection)
    })
}

const BACKLOG: usize = 256;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Output {
    tx: Arc<SyncSender<Message>>,
    resume: SyncSender<()>,
    worker: JoinHandle<()>,
    done: Receiver<()>,
}

enum Message {
    Record(Box<message::Kind>),
    Diagnostic(Vec<u8>),
}

/// Sends foreground diagnostics through the protocol logger's bounded queue.
#[derive(Clone)]
pub struct Diagnostics {
    tx: Weak<SyncSender<Message>>,
}

impl Diagnostics {
    fn send(&self, message: Message) {
        if let Some(tx) = self.tx.upgrade() {
            // Logging cannot change a command's result or interrupt an interface.
            let _ = tx.try_send(message);
        }
    }
}

impl Write for Diagnostics {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.send(Message::Diagnostic(bytes.to_vec()));
        Ok(bytes.len())
    }

    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> io::Result<()> {
        // One formatted diagnostic stays intact beside concurrent protocol records.
        self.send(Message::Diagnostic(args.to_string().into_bytes()));
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Output::finish drains the queue with a deadline.
        Ok(())
    }
}

impl Output {
    pub fn start() -> Self {
        Self::configured(io::stderr(), false)
    }

    /// Defers terminal-bound diagnostics until the interface has restored terminal ownership.
    pub fn terminal() -> Self {
        Self::configured(io::stderr(), io::stderr().is_terminal())
    }

    #[cfg(test)]
    fn with_writer(writer: impl Write + Send + 'static) -> Self {
        Self::configured(writer, false)
    }

    fn configured(mut writer: impl Write + Send + 'static, deferred: bool) -> Self {
        let (resume, ready) = mpsc::sync_channel(1);
        if !deferred {
            let _ = resume.try_send(());
        }
        let (tx, rx) = mpsc::sync_channel::<Message>(BACKLOG);
        let (finished, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = ready.recv();
            while let Ok(message) = rx.recv() {
                let written = match message {
                    Message::Record(kind) => writeln!(writer, "{}", crate::runner::json(*kind)),
                    Message::Diagnostic(bytes) => writer.write_all(&bytes),
                };
                if written.and_then(|()| writer.flush()).is_err() {
                    break;
                }
            }
            let _ = finished.send(());
        });
        Self {
            tx: Arc::new(tx),
            resume,
            worker,
            done,
        }
    }

    pub fn observer(&self) -> Observer {
        // Observers do not keep the queue open after shutdown.
        let diagnostics = self.diagnostics();
        Arc::new(move |diagnostic| {
            let kind = match diagnostic {
                Diagnostic::Response(_, response) => message::Kind::Response(response.clone()),
                Diagnostic::Event(event) => message::Kind::Event(event.clone()),
            };
            diagnostics.send(Message::Record(Box::new(kind)));
        })
    }

    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            tx: Arc::downgrade(&self.tx),
        }
    }

    /// Drains accepted diagnostics for at most two seconds after the interface stops.
    pub fn finish(self) {
        self.finish_with_timeout(DRAIN_TIMEOUT);
    }

    fn finish_with_timeout(self, timeout: Duration) {
        let _ = self.resume.try_send(());
        drop(self.tx);
        if self.done.recv_timeout(timeout).is_ok() {
            let _ = self.worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordial_client::Received;
    use cordial_protocol::{self as p, request, response};
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn prints_complete_responses_and_events_as_protobuf_json() {
        let buffer = Buffer::default();
        let output = Output::with_writer(buffer.clone());
        let observer = output.observer();
        let response = p::Response {
            result: Some(response::Result::File(p::FileData {
                path: "/data.bin".into(),
                offset: 0,
                size: 1537,
                data: (0..1537).map(|n| n as u8).collect(),
            })),
        };
        let request = p::Request {
            command: Some(request::Command::GetStatus(p::GetStatus {})),
        };
        let event = p::Event {
            kind: Some(p::event::Kind::Adapter(crate::sim::status(true))),
        };
        observer(Diagnostic::Response(&request, &response));
        observer(Diagnostic::Event(&event));
        output.finish();
        let bytes = buffer.0.lock().unwrap();
        let actual: Vec<p::Message> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            actual,
            [
                p::Message {
                    kind: Some(message::Kind::Response(response))
                },
                p::Message {
                    kind: Some(message::Kind::Event(event))
                },
            ]
        );
    }

    #[test]
    fn diagnostics_and_records_share_a_writer_without_splitting_formatted_lines() {
        struct ConcurrentRecord(Observer);
        impl fmt::Display for ConcurrentRecord {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                (self.0)(Diagnostic::Event(&p::Event::default()));
                f.write_str("adapter")
            }
        }
        let buffer = Buffer::default();
        let output = Output::with_writer(buffer.clone());
        let mut diagnostics = output.diagnostics();
        diagnostics.write_all(b"raw diagnostic: \xff\n").unwrap();
        writeln!(
            diagnostics,
            "Waiting for {}.",
            ConcurrentRecord(output.observer())
        )
        .unwrap();
        output.observer()(Diagnostic::Event(&p::Event::default()));
        output.finish();
        assert_eq!(
            &*buffer.0.lock().unwrap(),
            b"raw diagnostic: \xff\n{\"event\":{}}\nWaiting for adapter.\n{\"event\":{}}\n"
        );
    }

    #[test]
    fn a_blocked_writer_drops_excess_diagnostics_without_blocking_the_producer() {
        struct Blocked(Option<mpsc::Sender<()>>, mpsc::Receiver<()>);

        impl Write for Blocked {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(entered) = self.0.take() {
                    entered.send(()).unwrap();
                    self.1.recv().unwrap();
                }
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (entered, writing) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let output = Output::with_writer(Blocked(Some(entered), blocked));
        let observer = output.observer();
        let event = p::Event::default();
        observer(Diagnostic::Event(&event));
        writing
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        for _ in 0..BACKLOG {
            observer(Diagnostic::Event(&event));
        }
        writeln!(output.diagnostics(), "Waiting for adapter.").unwrap();
        drop(observer);
        drop(output.tx);
        release.send(()).unwrap();
        output.worker.join().unwrap();
    }

    #[test]
    fn shutdown_finishes_when_a_reader_stalls_with_space_left_in_the_queue() {
        struct Blocked {
            entered: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
            dropped: mpsc::Sender<()>,
        }
        impl Write for Blocked {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Drop for Blocked {
            fn drop(&mut self) {
                let _ = self.dropped.send(());
            }
        }
        for queued in [0, BACKLOG] {
            let (entered, writing) = mpsc::channel();
            let (release, blocked) = mpsc::channel();
            let (dropped, exited) = mpsc::channel();
            let output = Output::with_writer(Blocked {
                entered: Some(entered),
                release: blocked,
                dropped,
            });
            let observer = output.observer();
            observer(Diagnostic::Event(&p::Event::default()));
            writing.recv_timeout(Duration::from_secs(2)).unwrap();
            for _ in 0..queued {
                observer(Diagnostic::Event(&p::Event::default()));
            }
            if queued == 0 {
                writeln!(output.diagnostics(), "Waiting for adapter.").unwrap();
            }
            let (finished, result) = mpsc::channel();
            let shutdown = thread::spawn(move || {
                output.finish_with_timeout(Duration::from_millis(50));
                let _ = finished.send(());
            });
            let outcome = result.recv_timeout(Duration::from_secs(2));
            // Release the writer even when shutdown fails to meet its deadline.
            release.send(()).unwrap();
            drop(observer);
            shutdown.join().unwrap();
            exited.recv_timeout(Duration::from_secs(2)).unwrap();
            outcome.unwrap();
        }
    }

    fn complete_error() -> p::Error {
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            outcome: p::ErrorOutcome::Complete as i32,
            related: vec![p::Error {
                code: p::ErrorCode::Unsupported as i32,
                outcome: p::ErrorOutcome::NoEffect as i32,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn wire_diagnostics_preserve_completed_errors_and_missing_payloads() {
        let error = complete_error();
        let dongle = crate::sim::Dongle::with(|sim| {
            for command in ["set_settings", "get_status"] {
                sim.refuse.insert(command, error.clone());
            }
        });
        let buffer = Buffer::default();
        let output = Output::with_writer(buffer.clone());
        let connector = observe(Arc::new(dongle.connector()), output.observer());
        let (events, seen) = mpsc::channel();
        let connection = connector(
            "simulated",
            Box::new(move |received| {
                if let Received::Event(event) = received
                    && let Some(p::event::Kind::Pairing(pairing)) = event.kind
                {
                    events.send(pairing.candidate).unwrap();
                }
            }),
        )
        .unwrap();
        let result = connection.set_settings(1, Vec::new());
        assert!(matches!(result, Err(cordial_client::Error::Dongle(e)) if e == error));
        let result = connection.status();
        assert!(matches!(result, Err(cordial_client::Error::Dongle(e)) if e == error));
        let event = p::Event {
            kind: Some(p::event::Kind::Pairing(p::Pairing {
                candidate: 7,
                step: Some(p::pairing::Step::Error(error.clone())),
            })),
        };
        dongle.event(event.kind.clone().unwrap());
        assert_eq!(seen.recv_timeout(Duration::from_secs(2)).unwrap(), 7);
        assert!(connection.close_and_wait_release());
        output.finish();
        let bytes = buffer.0.lock().unwrap();
        let records: Vec<p::Message> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let response = message::Kind::Response(p::Response {
            result: Some(response::Result::Error(error)),
        });
        assert_eq!(
            records,
            [response.clone(), response, message::Kind::Event(event)]
                .map(|kind| p::Message { kind: Some(kind) })
        );
    }

    #[test]
    fn terminal_diagnostics_wait_for_terminal_release_and_drain_before_exit() {
        let buffer = Buffer::default();
        let output = Output::configured(buffer.clone(), true);
        let response = p::Response {
            result: Some(response::Result::Error(complete_error())),
        };
        output.observer()(Diagnostic::Response(&p::Request::default(), &response));
        writeln!(output.diagnostics(), "Waiting for adapter.").unwrap();
        // The worker waits until finish releases terminal ownership.
        assert!(buffer.0.lock().unwrap().is_empty());
        output.finish();
        let bytes = buffer.0.lock().unwrap();
        let lines: Vec<_> = std::str::from_utf8(&bytes).unwrap().lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<p::Message>(lines[0]).unwrap().kind,
            Some(message::Kind::Response(response))
        );
        assert_eq!(lines[1], "Waiting for adapter.");
    }

    #[test]
    fn deferred_overflow_does_not_end_the_terminal_session() {
        let buffer = Buffer::default();
        let output = Output::configured(buffer.clone(), true);
        let observer = output.observer();
        for _ in 0..BACKLOG + 17 {
            observer(Diagnostic::Event(&p::Event::default()));
        }
        assert!(buffer.0.lock().unwrap().is_empty());
        output.finish();
        let bytes = buffer.0.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(text.lines().count(), BACKLOG);
        for line in text.lines() {
            serde_json::from_str::<p::Message>(line).unwrap();
        }
    }

    #[test]
    fn broken_output_does_not_fail_shutdown() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let output = Output::with_writer(Broken);
        output.observer()(Diagnostic::Event(&p::Event::default()));
        output.finish();
    }
}
