//! The sessions the TUI holds: one controller per opened port, each named by a slot. Every
//! attached adapter gets its own session, the way the desktop application keeps one session open
//! to each adapter.
use crate::{
    controller::{Command, Controller, RunOptions, State, Ticket},
    ui::Msg,
};
use cordial_client::serial::{self, PortInfo};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::Path,
    sync::mpsc::SyncSender,
    thread,
};

/// Names one opened port's session for as long as the TUI holds it. Slots are never reused.
pub(crate) type Slot = u64;

/// The controller calls the TUI makes, so the model can be tested with a stand-in.
pub(crate) trait Fleet {
    /// Opens a session on `port`. Its phases follow as `Msg::Fleet` with the returned slot.
    fn open(&self, port: &str) -> Slot;
    /// Closes a session; `Event::Closed` follows for its slot.
    fn close(&self, slot: Slot);
    /// Runs a command on a session; `Event::Done` reports its result.
    fn run(&self, slot: Slot, command: Command) -> Ticket;
    /// The session's view.
    fn state(&self, slot: Slot) -> Option<State>;
    /// Lists the attached adapters off the interface thread; the result arrives as `Msg::Ports`.
    fn list_ports(&self);
}

pub(crate) struct Live {
    tx: SyncSender<Msg>,
    /// An explicit port, used instead of discovery.
    only: Option<String>,
    controllers: RefCell<HashMap<Slot, Controller>>,
    next: Cell<Slot>,
}

impl Live {
    pub(crate) fn new(tx: SyncSender<Msg>, only: Option<String>) -> Self {
        Self {
            tx,
            only,
            controllers: RefCell::new(HashMap::new()),
            next: Cell::new(0),
        }
    }
}

impl Fleet for Live {
    fn open(&self, port: &str) -> Slot {
        let slot = self.next.get() + 1;
        self.next.set(slot);
        let sink = self.tx.clone();
        // The channel is bounded: while the interface stops consuming, the controller's threads
        // wait here and each session's own bounded queue applies its loss policy.
        let controller = Controller::new(move |e| {
            let _ = sink.send(Msg::Fleet(slot, Box::new(e)));
        });
        controller.open(port.to_owned());
        self.controllers.borrow_mut().insert(slot, controller);
        slot
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
        thread::spawn(move || {
            let listed = serial::ports().map_err(|e| e.to_string());
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
            let _ = tx.send(Msg::Ports(result));
        });
    }
}
