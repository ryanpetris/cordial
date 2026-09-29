//! Terminal presentation and interaction: the full-screen TUI, the
//! interactive shell, and the human-readable text both share with scripts.
pub mod catalog;
pub mod command;
#[cfg(test)]
mod fake;
pub mod field;
mod shell;
mod term;
pub mod text;
mod tui;

use crate::{
    client::{Cancellation, Wait},
    controller::{Command, Controller, Event, OpenOptions, RunOptions, SessionId, State, Ticket},
    transport::{self, PortInfo},
};
use cordial_protocol::identifiers::CandidateId;
use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    thread,
    time::Duration,
};

pub use shell::{Shell, shell};
pub use term::Tui;

/// How long an interface waits to open an adapter and complete its handshake.
const OPEN_TIMEOUT: Duration = Duration::from_secs(6);

pub struct UiOptions {
    /// An explicit serial port, opened instead of choosing an adapter.
    pub port: Option<String>,
}

#[derive(Debug)]
pub enum UiError {
    /// SIGINT or SIGTERM closed the shell.
    Interrupted,
    Terminal(std::io::Error),
}
impl fmt::Display for UiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UiError::Interrupted => f.write_str("interrupted"),
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
    fn open(&self, port: String, keep_unready: bool) -> SessionId;
    fn close(&self);
    fn run(&self, command: Command) -> Ticket;
    /// Runs a command that the interface may cancel, such as a file transfer.
    fn run_cancellable(&self, command: Command, cancel: Cancellation) -> Ticket;
    fn state(&self) -> Option<State>;
    fn hide_candidate(&self, id: &CandidateId);
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
    fn open(&self, port: String, keep_unready: bool) -> SessionId {
        self.controller.open(
            port,
            OpenOptions {
                wait: Wait::timeout(OPEN_TIMEOUT),
                keep_unready,
            },
        )
    }
    fn close(&self) {
        self.controller.close();
    }
    fn run(&self, command: Command) -> Ticket {
        self.controller.run(command, RunOptions::default())
    }
    fn run_cancellable(&self, command: Command, cancel: Cancellation) -> Ticket {
        let wait = Wait {
            deadline: None,
            cancellation: cancel,
        };
        self.controller.run(
            command,
            RunOptions {
                wait,
                ..RunOptions::default()
            },
        )
    }
    fn state(&self) -> Option<State> {
        self.controller.state()
    }
    fn hide_candidate(&self, id: &CandidateId) {
        self.controller.hide_candidate(id);
    }
    fn list_ports(&self) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Msg::Ports(transport::ports().map_err(|e| e.to_string())));
        });
    }
}

/// The full-screen TUI and a handle that interrupts it.
pub fn tui(options: UiOptions) -> (Tui, Interrupter) {
    Tui::new(options)
}
