//! Original protocol diagnostics, independent of normalized state and command results.
use crate::protocol as p;
use std::{
    io::{self, Write},
    sync::{
        Arc, OnceLock,
        mpsc::{self, SyncSender},
    },
    thread,
    time::Duration,
};

/// Diagnostic input before the host outcome policy changes a message.
pub enum Diagnostic<'a> {
    /// The original response and the request it answers.
    Response(&'a p::Request, &'a p::Response),
    /// The original adapter event.
    Event(&'a p::Event),
    /// The complete original error tree for an operation normalized to success.
    Completed(&'a p::Error),
}

/// A diagnostic callback on the serial reader. It must return without waiting for output.
pub type Sink = Arc<dyn for<'a> Fn(Diagnostic<'a>) + Send + Sync>;

enum Message {
    Completed(p::Error),
    Flush(SyncSender<()>),
}

const BACKLOG: usize = 256;
static STDERR: OnceLock<SyncSender<Message>> = OnceLock::new();

pub(crate) fn completed(error: &p::Error) {
    let tx = STDERR.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel(BACKLOG);
        // One process-wide worker bounds resources even if stderr never becomes writable.
        thread::spawn(move || {
            while let Ok(message) = rx.recv() {
                match message {
                    Message::Completed(error) => {
                        // Errors contain protocol enums and related errors, never request data.
                        let _ = writeln!(io::stderr(), "Completed adapter operation: {error:?}");
                    }
                    Message::Flush(done) => {
                        let _ = done.try_send(());
                    }
                }
            }
        });
        tx
    });
    // A stalled diagnostic consumer must not stall commands or connection shutdown.
    let _ = tx.try_send(Message::Completed(error.clone()));
}

/// Waits up to two seconds for accepted default stderr diagnostics before process exit.
/// Returns false if stderr is blocked or the bounded queue cannot accept the drain marker.
/// Excess diagnostics are dropped when the 256-entry queue is full.
/// Custom connection sinks own their own delivery and shutdown.
pub fn flush() -> bool {
    let Some(tx) = STDERR.get() else {
        return true;
    };
    let (done, result) = mpsc::sync_channel(1);
    tx.try_send(Message::Flush(done)).is_ok() && result.recv_timeout(Duration::from_secs(2)).is_ok()
}
