//! Adapter sessions and injectable connection, discovery and migration backends.
use crate::{
    controller::{Command, Controller, RunOptions, State, Ticket},
    migration::{self, Engine, Worker},
};
#[cfg(test)]
use cordial_client::Received;
use cordial_client::serial::{self, PortInfo};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::Path,
    sync::Arc,
    thread,
};

use super::Msg;

/// Receives backend messages. Bounded channel senders preserve controller backpressure.
pub type Sink = Arc<dyn Fn(Msg) + Send + Sync>;

/// Names one opened port's session for as long as the application holds it. Slots are never reused.
pub type Slot = u64;

/// Controller operations used by the shared application.
pub trait Fleet {
    /// Opens a session on `port`. Its phases follow as `Msg::Fleet` with the returned slot.
    fn open(&self, port: &str) -> Slot;
    /// Opens a session on `port` for the adapter with ID `id`, as `open` does. Another adapter
    /// on the port reports `Phase::Other`.
    fn open_adapter(&self, port: &str, id: &str) -> Slot;
    /// Closes a session; `Event::Closed` follows for its slot.
    fn close(&self, slot: Slot);
    /// Runs a command on a session; `Event::Done` reports its result.
    fn run(&self, slot: Slot, command: Command) -> Ticket;
    /// The session's view.
    fn state(&self, slot: Slot) -> Option<State>;
    /// Lists the attached adapters off the interface thread; the result arrives as `Msg::Ports`.
    fn list_ports(&self);
    /// Migrates an adapter once every session has closed; events arrive as `Msg::Migration` and
    /// the result as `Msg::Migrated`.
    fn migrate(&self, id: &str, port: &str);
    fn cancel_migration(&self);
}

/// Controllers for serial adapters, with worker-backed discovery and migration.
pub struct Live {
    tx: Sink,
    /// An explicit port, used instead of discovery.
    only: Option<String>,
    controllers: RefCell<HashMap<Slot, Controller>>,
    next: Cell<Slot>,
    worker: RefCell<Option<Worker>>,
    engine: Option<Engine>,
    connector: Arc<crate::controller::Connector>,
    ports: Arc<dyn Fn() -> Result<Vec<PortInfo>, String> + Send + Sync>,
}

impl Live {
    /// Uses serial discovery and in-process migration, optionally restricted to one port.
    pub fn new(tx: Sink, only: Option<String>) -> Self {
        Self {
            worker: RefCell::new(None),
            engine: None,
            connector: Arc::new(|port, handler| serial::open_with_handler(port, handler)),
            ports: Arc::new(|| serial::ports().map_err(|e| e.to_string())),
            tx,
            only,
            controllers: RefCell::new(HashMap::new()),
            next: Cell::new(0),
        }
    }
}

impl Live {
    /// Observes raw responses and events from controller and live migration connections.
    pub fn observe(mut self, observer: crate::json::Observer) -> Self {
        self.connector = crate::json::observe(self.connector, observer);
        self
    }

    /// Uses supplied connections, port discovery and migration for simulation or tests.
    pub fn with_backend(
        mut self,
        connector: Arc<crate::controller::Connector>,
        ports: impl Fn() -> Result<Vec<PortInfo>, String> + Send + Sync + 'static,
        engine: Engine,
    ) -> Self {
        self.connector = connector;
        self.ports = Arc::new(ports);
        self.engine = Some(engine);
        self
    }

    fn open_as(&self, open: impl FnOnce(&Controller)) -> Slot {
        let slot = self.next.get() + 1;
        self.next.set(slot);
        let sink = self.tx.clone();
        // A bounded sink makes the controller wait while the UI stops consuming; its own
        // bounded event queue then applies its loss policy.
        let connector = self.connector.clone();
        let ports = self.ports.clone();
        let explicit = self.only.is_some();
        let controller = Controller::with_backend(
            move |e| sink(Msg::Fleet(slot, Box::new(e))),
            move |port, handler| connector(port, handler),
            move || {
                if explicit {
                    Ok(Vec::new())
                } else {
                    ports().map_err(std::io::Error::other)
                }
            },
        );
        open(&controller);
        self.controllers.borrow_mut().insert(slot, controller);
        slot
    }
}

impl Fleet for Live {
    fn open(&self, port: &str) -> Slot {
        self.open_as(|c| {
            c.open(port.to_owned());
        })
    }

    fn open_adapter(&self, port: &str, id: &str) -> Slot {
        self.open_as(|c| {
            c.open_adapter(port.to_owned(), id.to_owned());
        })
    }

    fn close(&self, slot: Slot) {
        // The closing thread keeps the session alive until it has ended and reported Closed.
        if let Some(controller) = self.controllers.borrow_mut().remove(&slot) {
            controller.close();
        }
    }

    fn run(&self, slot: Slot, command: Command) -> Ticket {
        match self.controllers.borrow().get(&slot) {
            Some(controller) => controller.run(command, RunOptions::default()),
            None => 0,
        }
    }

    fn state(&self, slot: Slot) -> Option<State> {
        self.controllers.borrow().get(&slot)?.state()
    }

    fn list_ports(&self) {
        let tx = self.tx.clone();
        let only = self.only.clone();
        let ports = self.ports.clone();
        thread::spawn(move || {
            let listed = ports();
            let result = match only {
                // An explicit port bypasses the USB filter; it is present while its path exists,
                // and keeps its USB serial number when discovery lists it too.
                Some(port) => Ok(listed
                    .ok()
                    .and_then(|ports| ports.into_iter().find(|p| p.port == port))
                    .or_else(|| {
                        Path::new(&port).exists().then(|| PortInfo {
                            port: port.clone(),
                            serial: String::new(),
                        })
                    })
                    .into_iter()
                    .collect()),
                None => listed,
            };
            tx(Msg::Ports(result));
        });
    }

    fn migrate(&self, id: &str, port: &str) {
        let events = self.tx.clone();
        let done = self.tx.clone();
        let engine = self.engine.clone().unwrap_or_else(|| {
            let ports = self.ports.clone();
            migration::with_backend(self.connector.clone(), move || {
                ports().map_err(std::io::Error::other)
            })
        });
        *self.worker.borrow_mut() = Some(Worker::start(
            &engine,
            id.to_owned(),
            (!port.is_empty()).then(|| port.to_owned()),
            move |e| events(Msg::Migration(e)),
            move |result| done(Msg::Migrated(result)),
        ));
    }

    fn cancel_migration(&self) {
        if let Some(worker) = &*self.worker.borrow() {
            worker.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordial_protocol::{self as p, message, request};
    use std::{
        sync::{Mutex, mpsc},
        time::Duration,
    };

    #[cfg(unix)]
    #[test]
    fn profile_reconnect_uses_fleet_discovery_and_preserves_an_explicit_symlink() {
        use crate::controller::{Event, Outcome, Phase, Target};

        let dir = std::env::temp_dir().join(format!("cordial-reconnect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.join("port");
        let alias = dir.join("alias");
        std::fs::write(&canonical, []).unwrap();
        std::os::unix::fs::symlink(&canonical, &alias).unwrap();
        let canonical = canonical.to_str().unwrap().to_owned();
        let alias = alias.to_str().unwrap().to_owned();
        for explicit in [false, true] {
            let dongle = crate::sim::Dongle::with(|sim| {
                crate::sim::enable_profiles(&mut sim.status);
                sim.add_profile("Work", Vec::new());
                sim.reconnect_interfaces = true;
                let interface = &mut sim.status.configuration_interfaces[0];
                interface.enabled = true;
                interface.active = true;
                interface.profile = 1;
            });
            let opened = Arc::new(Mutex::new(Vec::new()));
            let calls = opened.clone();
            let adapter = dongle.clone();
            let discovered = canonical.clone();
            let (tx, rx) = mpsc::channel();
            let live = Live::new(
                Arc::new(move |msg| {
                    let _ = tx.send(msg);
                }),
                explicit.then(|| alias.clone()),
            )
            .with_backend(
                Arc::new(move |port, handler| {
                    calls.lock().unwrap().push(port.to_owned());
                    adapter.connect(handler)
                }),
                move || {
                    Ok(vec![PortInfo {
                        port: discovered.clone(),
                        serial: "ADAPTER01".into(),
                    }])
                },
                migration::live(),
            );
            let slot = live.open(&alias);
            loop {
                if let Msg::Fleet(_, event) = rx.recv_timeout(Duration::from_secs(5)).unwrap()
                    && matches!(
                        *event,
                        Event::Connection {
                            phase: Phase::Ready,
                            ..
                        }
                    )
                {
                    break;
                }
            }
            let ticket = live.run(slot, Command::ProfileDelete(Target::Id(1)));
            loop {
                if let Msg::Fleet(_, event) = rx.recv_timeout(Duration::from_secs(5)).unwrap()
                    && let Event::Done {
                        ticket: done,
                        result,
                        ..
                    } = *event
                    && done == ticket
                {
                    assert!(matches!(result.unwrap(), Outcome::ProfileDeleted(_)));
                    break;
                }
            }
            live.list_ports();
            let listed = loop {
                if let Msg::Ports(result) = rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                    break result.unwrap();
                }
            };
            let state = live.state(slot).unwrap();
            let expected = if explicit { &alias } else { &canonical };
            let calls = opened.lock().unwrap().clone();
            live.close(slot);
            loop {
                if let Msg::Fleet(_, event) = rx.recv_timeout(Duration::from_secs(5)).unwrap()
                    && matches!(*event, Event::Closed)
                {
                    break;
                }
            }
            assert_eq!(dongle.handles(), 0);
            if explicit {
                std::fs::remove_dir_all(&dir).unwrap();
            }
            assert_eq!(state.port, *expected);
            assert_eq!(calls, [alias.as_str(), expected.as_str()]);
            assert!(listed.iter().any(|port| port.port == state.port));
        }
    }

    #[test]
    fn observes_live_migration_bootstrap_and_file_responses_once() {
        let data: Vec<u8> = (0..1537).map(|n| n as u8).collect();
        let dongle = crate::sim::Dongle::with(|sim| {
            sim.status.ready = false;
            sim.status.errors = vec![p::Error {
                code: p::ErrorCode::StorageMigrationRequired as i32,
                reason: p::ErrorReason::Storage as i32,
                outcome: p::ErrorOutcome::NoEffect as i32,
                ..Default::default()
            }];
            sim.files.insert(
                "/".into(),
                vec![p::FileEntry {
                    name: "format.pb".into(),
                    size: data.len() as u64,
                    directory: false,
                }],
            );
            sim.data.insert("/format.pb".into(), data.clone());
        });
        let (tx, rx) = mpsc::channel();
        let mut live = Live::new(
            Arc::new(move |msg| {
                let _ = tx.send(msg);
            }),
            None,
        );
        live.connector = Arc::new(dongle.connector());
        live.ports = Arc::new(|| {
            Ok(vec![PortInfo {
                port: "simulated".into(),
                serial: "ADAPTER01".into(),
            }])
        });
        let output = Arc::new(Mutex::new(Vec::new()));
        let observed = output.clone();
        let live = live.observe(Arc::new(move |diagnostic| {
            if let Some(kind) = diagnostic_kind(diagnostic) {
                observed.lock().unwrap().push(crate::runner::json(kind));
            }
        }));
        live.migrate("ADAPTER01", "simulated");
        loop {
            match rx.recv_timeout(Duration::from_secs(2)).unwrap() {
                Msg::Migrated(result) => {
                    // Invalid storage contents stop the migration after downloading them.
                    assert!(result.is_err());
                    break;
                }
                Msg::Migration(_) => {}
                _ => panic!("unexpected controller message"),
            }
        }
        let output = output.lock().unwrap();
        let responses: Vec<p::Response> = output
            .iter()
            .map(|line| {
                let message: p::Message = serde_json::from_str(line).unwrap();
                let Some(message::Kind::Response(response)) = message.kind else {
                    panic!("expected response")
                };
                response
            })
            .collect();
        let sent = dongle.sent();
        assert_eq!(&sent[..2], ["get_status", "list_files"]);
        assert!(sent[2..].iter().all(|command| *command == "read_file"));
        assert_eq!(
            sent.len(),
            3 + data.len().div_ceil(p::STORAGE_MIGRATION_CHUNK_BYTES)
        );
        assert_eq!(responses.len(), dongle.sent().len());
        assert!(
            matches!(&responses[0].result, Some(p::response::Result::Status(status)) if status.id == "ADAPTER01")
        );
        assert!(
            matches!(&responses[1].result, Some(p::response::Result::Files(files)) if files.entries[0].name == "format.pb")
        );
        let mut downloaded = Vec::new();
        for response in &responses[2..] {
            let Some(p::response::Result::File(file)) = &response.result else {
                panic!("expected file data")
            };
            assert_eq!(file.offset, downloaded.len() as u64);
            assert_eq!(file.size, data.len() as u64);
            assert_eq!(file.path, "/format.pb");
            downloaded.extend_from_slice(&file.data);
        }
        assert_eq!(downloaded, data);
        assert_eq!(dongle.handles(), 0);
    }

    #[test]
    fn observes_raw_messages_once_before_handling_with_a_supplied_connector() {
        let dongle = crate::sim::Dongle::with(|sim| {
            sim.data.insert("/data.bin".into(), vec![0, 1, 2, 255]);
        });
        let connector = dongle.connector();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed = seen.clone();
        let live = Live::new(Arc::new(|_| {}), None)
            .with_backend(Arc::new(connector), || Ok(Vec::new()), migration::live())
            .observe(Arc::new(move |diagnostic| {
                if let Some(kind) = diagnostic_kind(diagnostic) {
                    observed.lock().unwrap().push((true, kind));
                }
            }));
        let handled = seen.clone();
        let (event_seen, event_ready) = mpsc::channel();
        let connection = (live.connector)(
            "simulated",
            Box::new(move |received| {
                if let Some(kind) = kind(received) {
                    if matches!(kind, message::Kind::Event(_)) {
                        event_seen.send(()).unwrap();
                    }
                    handled.lock().unwrap().push((false, kind));
                }
            }),
        )
        .unwrap();
        let response = connection
            .request(request::Command::ReadFile(p::ReadFile {
                path: "/data.bin".into(),
                offset: 0,
                length: None,
            }))
            .unwrap();
        assert!(
            matches!(response.result, Some(p::response::Result::File(file)) if file.data == [0, 1, 2, 255])
        );
        dongle.event(p::event::Kind::Adapter(crate::sim::status(true)));
        event_ready.recv_timeout(Duration::from_secs(2)).unwrap();
        connection.close_and_wait_release();
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty());
        assert_eq!(seen.len() % 2, 0);
        for pair in seen.as_chunks::<2>().0 {
            assert!(pair[0].0);
            assert!(!pair[1].0);
            assert_eq!(pair[0].1, pair[1].1);
        }
        assert_eq!(
            seen.iter()
                .filter(|(observer, kind)| *observer && matches!(kind, message::Kind::Response(_)))
                .count(),
            1
        );
        assert!(
            seen.iter()
                .any(|(observer, kind)| *observer && matches!(kind, message::Kind::Event(_)))
        );
    }

    fn diagnostic_kind(
        diagnostic: cordial_client::diagnostics::Diagnostic<'_>,
    ) -> Option<message::Kind> {
        use cordial_client::diagnostics::Diagnostic;
        match diagnostic {
            Diagnostic::Response(_, response) => Some(message::Kind::Response(response.clone())),
            Diagnostic::Event(event) => Some(message::Kind::Event(event.clone())),
            Diagnostic::Completed(_) => None,
        }
    }

    fn kind(received: Received<'_>) -> Option<message::Kind> {
        match received {
            Received::Response(_, response) => Some(message::Kind::Response(response.clone())),
            Received::Event(event) => Some(message::Kind::Event(event)),
            Received::Closed(_) | Received::PairingComplete { .. } => None,
        }
    }
}
