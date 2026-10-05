//! Terminal presentation and interaction: the full-screen TUI, the
//! interactive shell, and the human-readable text both share with scripts.
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

use crate::controller::{Command, Controller, Event, RunOptions, SessionId, State, Ticket};
use cordial_client::serial::{self, PortInfo};
use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use std::{
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
    Ports(Result<Vec<PortInfo>, String>),
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
    fn close(&self);
    fn run(&self, command: Command) -> Ticket;
    fn state(&self) -> Option<State>;
    fn hide_candidate(&self, id: u32);
    /// Lists adapters off the interface thread; the result arrives as Msg::Ports.
    fn list_ports(&self);
}

pub(crate) struct Live {
    controller: Controller,
    tx: SyncSender<Msg>,
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
        Self { controller, tx }
    }
}
impl Backend for Live {
    fn open(&self, port: String) -> SessionId {
        self.controller.open(port)
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
    fn hide_candidate(&self, id: u32) {
        self.controller.hide_candidate(id);
    }
    fn list_ports(&self) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Msg::Ports(serial::ports().map_err(|e| e.to_string())));
        });
    }
}

/// The full-screen TUI and a handle that interrupts it.
pub fn tui(options: UiOptions) -> (Tui, Interrupter) {
    Tui::new(options)
}
