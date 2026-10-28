//! A client for the Cordial serial API: finding attached Dongles, opening a session on one, and
//! sending requests over the connection.
//!
//! [`Connection`] works over any byte stream that implements [`std::io::Read`] and
//! [`std::io::Write`]. It sends one [`Request`](protocol::Request) at a time per call, returns
//! the matching [`Response`](protocol::Response) in order, and delivers each
//! [`Event`](protocol::Event) to the caller. The `serialport` feature adds [`serial`], which
//! lists attached Dongles and opens their USB serial ports.
//!
//! Every listing returns its complete response. [`order`] identifies and orders its entries.
//! [`rules`] puts profile rules in the form the Dongle saves them.
//!
//! The library keeps no state beyond the open connection and writes nothing to stdout.
//! Host builds without debug assertions treat completed adapter operations as success and write
//! their original error details through bounded stderr delivery. Call [`diagnostics::flush`] before
//! process exit, or install a connection diagnostic sink. Debug builds preserve those errors.
pub mod compatibility;
mod connection;
pub mod diagnostics;
mod error;
pub mod migration;
pub mod order;
mod outcomes;
pub mod rules;
#[cfg(feature = "serialport")]
pub mod serial;

pub use connection::{
    Connection, DEFAULT_TIMEOUT, HANDSHAKE_TIMEOUT, MAX_RECEIVED_FRAME_BYTES, Received,
};
pub use cordial_protocol as protocol;
pub use error::Error;

/// The result of a request.
pub type Result<T> = std::result::Result<T, Error>;

/// An adapter event after applying the host build's outcome policy.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Protocol(protocol::Event),
    /// The bond is saved. The adapter did not supply the saved device ID.
    PairingComplete {
        candidate: u32,
    },
}
