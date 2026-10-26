//! Adapter sessions and injectable connection, discovery and migration backends.
use crate::{
    controller::{Command, Controller, RunOptions, State, Ticket},
    migration::{self, Engine, Worker},
};
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
    engine: Engine,
    connector: Arc<crate::controller::Connector>,
    ports: Arc<dyn Fn() -> Result<Vec<PortInfo>, String> + Send + Sync>,
}

impl Live {
    /// Uses serial discovery and in-process migration, optionally restricted to one port.
    pub fn new(tx: Sink, only: Option<String>) -> Self {
        Self {
            worker: RefCell::new(None),
            engine: migration::live(),
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
    /// Uses supplied connections, port discovery and migration for simulation or tests.
    pub fn with_backend(
        mut self,
        connector: Arc<crate::controller::Connector>,
        ports: impl Fn() -> Result<Vec<PortInfo>, String> + Send + Sync + 'static,
        engine: Engine,
    ) -> Self {
        self.connector = connector;
        self.ports = Arc::new(ports);
        self.engine = engine;
        self
    }

    fn open_as(&self, open: impl FnOnce(&Controller)) -> Slot {
        let slot = self.next.get() + 1;
        self.next.set(slot);
        let sink = self.tx.clone();
        // A bounded sink makes the controller wait while the UI stops consuming; its own
        // bounded event queue then applies its loss policy.
        let connector = self.connector.clone();
        let controller = Controller::with_connector(
            move |e| sink(Msg::Fleet(slot, Box::new(e))),
            move |port, handler| connector(port, handler),
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
        *self.worker.borrow_mut() = Some(Worker::start(
            &self.engine,
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
