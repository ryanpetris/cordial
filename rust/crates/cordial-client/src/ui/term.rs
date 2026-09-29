//! Terminal ownership: raw mode and restoration, the input reader and the
//! TUI's event loop.
use crate::ui::{Interrupter, Live, Msg, UiError, UiOptions, tui};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    crossterm::{
        cursor, event, execute,
        terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    },
};
use std::{
    io::{self, Write},
    sync::{
        Arc, Once,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The longest a TUI waits for its session cleanup after Quit.
const CLOSE_LIMIT: Duration = Duration::from_secs(5);

static FULL_SCREEN: AtomicBool = AtomicBool::new(false);

/// Restores the terminal modes this process set.
pub(crate) fn restore() {
    let mut out = io::stdout();
    if FULL_SCREEN.swap(false, Ordering::AcqRel) {
        let _ = execute!(
            out,
            event::DisableMouseCapture,
            event::DisableBracketedPaste,
            LeaveAlternateScreen,
            cursor::Show
        );
    } else {
        let _ = execute!(out, event::DisableBracketedPaste, cursor::Show);
    }
    let _ = terminal::disable_raw_mode();
    let _ = out.flush();
}

/// Puts the terminal in raw mode until dropped, and restores it before a
/// panic message too.
pub(crate) struct Modes;
impl Modes {
    pub(crate) fn enter(full: bool) -> io::Result<Self> {
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        if full {
            FULL_SCREEN.store(true, Ordering::Release);
            execute!(
                out,
                EnterAlternateScreen,
                event::EnableMouseCapture,
                event::EnableBracketedPaste,
                cursor::Hide
            )?;
        } else {
            execute!(out, event::EnableBracketedPaste)?;
        }
        Ok(Self)
    }
}
impl Drop for Modes {
    fn drop(&mut self) {
        restore();
    }
}

/// Reads terminal events on its own thread, as crossterm requires.
pub(crate) struct Input {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Input {
    pub(crate) fn spawn(tx: SyncSender<Msg>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                match event::poll(Duration::from_millis(50)) {
                    Ok(false) => continue,
                    Ok(true) => {}
                    Err(_) => return,
                }
                let msg = match event::read() {
                    Ok(event::Event::Key(k)) if k.kind != event::KeyEventKind::Release => {
                        Msg::Key(k)
                    }
                    Ok(event::Event::Mouse(m)) => Msg::Mouse(m),
                    Ok(event::Event::Paste(text)) => Msg::Paste(text),
                    Ok(event::Event::Resize(w, h)) => Msg::Resize(w, h),
                    Ok(_) => continue,
                    Err(_) => return,
                };
                // Never block on a full queue, so stopping can always join.
                let mut pending = msg;
                loop {
                    match tx.try_send(pending) {
                        Ok(()) => break,
                        Err(TrySendError::Full(msg)) if !flag.load(Ordering::Acquire) => {
                            pending = msg;
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => return,
                    }
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Messages waiting for an interface; senders wait once it is full.
const QUEUE: usize = 256;

pub(crate) fn channel() -> (SyncSender<Msg>, Receiver<Msg>, Interrupter, Arc<AtomicBool>) {
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    let flag = Arc::new(AtomicBool::new(false));
    let interrupter = Interrupter {
        flag: flag.clone(),
        tx: tx.clone(),
    };
    (tx, rx, interrupter, flag)
}

/// The full-screen TUI. Ctrl+C and SIGINT exit it on every screen, stopping
/// monitoring, releasing the serial port and restoring the terminal.
pub struct Tui {
    port: Option<String>,
    tx: SyncSender<Msg>,
    rx: Receiver<Msg>,
    interrupted: Arc<AtomicBool>,
}

impl Tui {
    pub(crate) fn new(options: UiOptions) -> (Self, Interrupter) {
        let (tx, rx, interrupter, interrupted) = channel();
        (
            Self {
                port: options.port,
                tx,
                rx,
                interrupted,
            },
            interrupter,
        )
    }

    pub fn run(self) -> Result<(), UiError> {
        let modes = Modes::enter(true)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        let input = Input::spawn(self.tx.clone());
        let mut model = tui::Model::new(Live::new(self.tx.clone()), self.port);
        let mut closing: Option<Instant> = None;
        loop {
            if self.interrupted.load(Ordering::Acquire) && !model.quitting() {
                model.update(Msg::Interrupt);
            }
            terminal.draw(|frame| {
                // The drawn area is authoritative, even if a resize event
                // was missed or is still queued.
                let area = frame.area();
                if (usize::from(area.width), usize::from(area.height))
                    != (model.width, model.height)
                {
                    model.update(Msg::Resize(area.width, area.height));
                }
                model.render(frame.buffer_mut());
            })?;
            if model.done {
                break;
            }
            if model.quitting() {
                let since = *closing.get_or_insert_with(Instant::now);
                if since.elapsed() > CLOSE_LIMIT {
                    break;
                }
            }
            match self.rx.recv_timeout(tui::tick_interval(model.animating())) {
                Ok(msg) => {
                    model.update(msg);
                    // Apply everything already waiting before drawing again.
                    while let Ok(msg) = self.rx.try_recv() {
                        model.update(msg);
                    }
                }
                Err(RecvTimeoutError::Timeout) => model.tick(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        drop(input);
        drop(terminal);
        drop(modes);
        Ok(())
    }
}
