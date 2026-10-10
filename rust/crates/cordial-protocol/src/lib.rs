//! The Cordial serial API: protobuf messages generated from `proto/cordial.proto`, the frame
//! encoding used on the USB serial port, and the information and setting keys from
//! `proto/keys.toml`; and the Dongle's saved records in [`storage`].
//!
//! A client sends [`Request`] frames and receives [`Message`] frames, each holding a
//! [`Response`] or an [`Event`]. [`frame`] turns messages into frames and back.
#![cfg_attr(not(feature = "json"), no_std)]

extern crate alloc;

#[allow(clippy::all, clippy::pedantic, missing_docs)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/cordial.rs"));
    #[cfg(feature = "json")]
    include!(concat!(env!("OUT_DIR"), "/cordial.serde.rs"));
}
pub use generated::*;

/// The records the Dongle saves in its filesystem, generated from `proto/storage.proto`. Each file
/// holds one of these messages.
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod storage {
    include!(concat!(env!("OUT_DIR"), "/cordial.storage.rs"));
    #[cfg(feature = "json")]
    include!(concat!(env!("OUT_DIR"), "/cordial.storage.serde.rs"));
}

pub mod frame;
pub mod keys;

/// USB vendor ID of every Dongle (pid.codes).
pub const USB_VENDOR_ID: u16 = 0x1209;
/// USB product ID of every Dongle.
pub const USB_PRODUCT_ID: u16 = 0xc0d1;
/// The longest request a Dongle accepts, before frame encoding.
pub const MAX_REQUEST_BYTES: usize = 1024;
