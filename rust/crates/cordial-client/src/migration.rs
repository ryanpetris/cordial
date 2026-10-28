//! RAM-only assisted migration. Callers exclusively own all adapter connections until this
//! worker returns success or acknowledges explicit cancellation; failure does not release that
//! ownership. The engine never reads or writes the host filesystem.
mod graph;
pub(crate) mod wire;
mod worker;

pub use graph::{Change, Checkpoint, Edge, Format, Graph, Snapshot, Transformation};
pub use worker::{AdapterPort, Discovery, Transport, migrate_with};
#[cfg(feature = "serialport")]
pub use worker::{migrate, migrate_with_backend};

use cordial_protocol as p;
use std::fmt;

/// A terminal result after all adapter handles have been closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Success,
    Cancelled,
}

/// A permanent migration failure. Transport interruptions are retried until cancellation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub reason: String,
    pub outcome_unknown: bool,
}
impl Error {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            outcome_unknown: false,
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Checks `Status.information.target_storage_format` without comparing release versions.
/// Run this on bootstrap status before issuing any other adapter commands. A supported target
/// does not establish a known committed format, valid storage, or readiness.
pub fn check_target(status: &p::Status) -> Result<u32> {
    Graph::standard().check_target(status)
}

fn storage_error(error: cordial_core::storage::Error) -> Error {
    Error::new(format!("invalid storage dataset: {error:?}"))
}

// RAM datasets never yield: polling once also catches an accidental asynchronous dependency.
fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match std::pin::pin!(future).as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("RAM dataset unexpectedly yielded"),
    }
}
