//! A client for the Cordial serial API: finding attached Dongles, opening a session on one, and
//! sending requests over the connection.
//!
//! [`Connection`] works over any byte stream that implements [`std::io::Read`] and
//! [`std::io::Write`]. It sends one [`Request`](protocol::Request) at a time per call, returns
//! the matching [`Response`](protocol::Response) in order, and delivers each
//! [`Event`](protocol::Event) to the caller. The `serialport` feature adds [`serial`], which
//! lists attached Dongles and opens their USB serial ports.
//!
//! The library keeps no state beyond the open connection and writes nothing to stdout or logs.
mod connection;
mod error;
#[cfg(feature = "serialport")]
pub mod serial;

pub use connection::{Connection, DEFAULT_TIMEOUT, Received};
pub use cordial_protocol as protocol;
pub use error::Error;

/// The result of a request.
pub type Result<T> = std::result::Result<T, Error>;
