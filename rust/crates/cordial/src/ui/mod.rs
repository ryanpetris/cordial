//! Shared application state and human-readable text, plus terminal interaction.
pub mod app;
pub mod catalog;
pub mod command;
#[cfg(test)]
mod fake;
pub mod field;
mod reconnect;
mod shell;
mod term;
pub mod text;
mod tui;

use crate::{
    controller::{Command, Controller, Event, RunOptions, SessionId, State, Ticket},
    migration::{self, Worker},
};
use cordial_client::{
    migration::{Outcome, Result as MigrationResult},
    serial::{self, PortInfo},
};
use cordial_protocol::migration::MigrationEvent;
use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use std::{
    cell::RefCell,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    thread,
};

pub use shell::{Shell, shell};
pub use term::Tui;

pub struct UiOptions {
    /// An explicit serial port, opened instead of choosing an adapter.
    pub port: Option<String>,
}

#[derive(Debug)]
pub enum UiError {
    Terminal(std::io::Error),
}
impl fmt::Display for UiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UiError::Terminal(e) => write!(f, "terminal: {e}"),
        }
    }
}
impl std::error::Error for UiError {}
impl From<std::io::Error> for UiError {
    fn from(e: std::io::Error) -> Self {
        UiError::Terminal(e)
    }
}

/// Everything an interface reacts to, delivered on one channel.
pub(crate) enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize(u16, u16),
    Controller(Box<Event>),
    /// An event of one of the TUI's sessions.
    Fleet(u64, Box<Event>),
    Ports(Result<Vec<PortInfo>, String>),
    /// An event of the running migration.
    Migration(MigrationEvent),
    /// The migration worker's result; it has closed its connection.
    Migrated(MigrationResult<Outcome>),
    Interrupt,
}

/// Lets a signal handler close an interface as Quit does. It never blocks:
/// the flag is checked on every wakeup, and the message only wakes the loop.
#[derive(Clone)]
pub struct Interrupter {
    flag: Arc<AtomicBool>,
    tx: SyncSender<Msg>,
}
impl Interrupter {
    pub fn interrupt(&self) {
        self.flag.store(true, Ordering::Release);
        let _ = self.tx.try_send(Msg::Interrupt);
    }
}

/// The controller calls an interface makes, so models can be tested with a
/// stand-in.
pub(crate) trait Backend {
    fn open(&self, port: String) -> SessionId;
    /// Opens `port` for the adapter with ID `id`; another adapter there reports `Phase::Other`.
    fn open_adapter(&self, port: String, id: String) -> SessionId;
    fn close(&self);
    fn run(&self, command: Command) -> Ticket;
    fn state(&self) -> Option<State>;
    /// Lists adapters off the interface thread; the result arrives as Msg::Ports.
    fn list_ports(&self);
    /// Migrates an adapter once the session has closed; events arrive as Msg::Migration and
    /// the result as Msg::Migrated.
    fn migrate(&self, id: String, port: String);
    fn cancel_migration(&self);
}

pub(crate) struct Live {
    controller: Controller,
    tx: SyncSender<Msg>,
    worker: Migrator,
}

/// Starts and cancels the migration worker for an interface.
pub(crate) struct Migrator {
    tx: SyncSender<Msg>,
    worker: RefCell<Option<Worker>>,
}
impl Migrator {
    pub(crate) fn new(tx: SyncSender<Msg>) -> Self {
        Self {
            tx,
            worker: RefCell::new(None),
        }
    }

    pub(crate) fn start(&self, id: String, port: String) {
        let (events, done) = (self.tx.clone(), self.tx.clone());
        let port = (!port.is_empty()).then_some(port);
        let worker = Worker::start(
            &migration::live(),
            id,
            port,
            move |e| {
                let _ = events.send(Msg::Migration(e));
            },
            move |result| {
                let _ = done.send(Msg::Migrated(result));
            },
        );
        *self.worker.borrow_mut() = Some(worker);
    }

    pub(crate) fn cancel(&self) {
        if let Some(worker) = &*self.worker.borrow() {
            worker.cancel();
        }
    }
}
impl Live {
    pub(crate) fn new(tx: SyncSender<Msg>) -> Self {
        let sink = tx.clone();
        // The channel is bounded: while the interface stops consuming, the
        // controller's threads wait here and the session's own bounded queue
        // applies its loss policy instead of this process accumulating events.
        let controller = Controller::new(move |e| {
            let _ = sink.send(Msg::Controller(Box::new(e)));
        });
        Self {
            controller,
            worker: Migrator::new(tx.clone()),
            tx,
        }
    }
}
impl Backend for Live {
    fn open(&self, port: String) -> SessionId {
        self.controller.open(port)
    }
    fn open_adapter(&self, port: String, id: String) -> SessionId {
        self.controller.open_adapter(port, id)
    }
    fn close(&self) {
        self.controller.close();
    }
    fn run(&self, command: Command) -> Ticket {
        self.controller.run(command, RunOptions::default())
    }
    fn state(&self) -> Option<State> {
        self.controller.state()
    }
    fn list_ports(&self) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Msg::Ports(serial::ports().map_err(|e| e.to_string())));
        });
    }
    fn migrate(&self, id: String, port: String) {
        self.worker.start(id, port);
    }
    fn cancel_migration(&self) {
        self.worker.cancel();
    }
}

/// The full-screen TUI and a handle that interrupts it.
pub fn tui(options: UiOptions) -> (Tui, Interrupter) {
    Tui::new(options)
}

impl From<app::Msg> for Msg {
    fn from(msg: app::Msg) -> Self {
        match msg {
            app::Msg::Fleet(slot, event) => Self::Fleet(slot, event),
            app::Msg::Ports(ports) => Self::Ports(ports),
            app::Msg::Migration(event) => Self::Migration(event),
            app::Msg::Migrated(result) => Self::Migrated(result),
            app::Msg::Interrupt => Self::Interrupt,
        }
    }
}
