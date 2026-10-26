//! The Cordial serial API: protobuf messages generated from `proto/serial/v1/cordial.proto`, the frame
//! encoding used on the USB serial port, and the Dongle's saved records in [`storage`].
//!
//! The permanent [`handshake`] identifies the operational revision. A client sends [`Request`] frames and receives [`Message`] frames, each holding a
//! [`Response`] or an [`Event`]. [`frame`] turns messages into frames and back.
#![cfg_attr(not(feature = "json"), no_std)]

extern crate alloc;

#[allow(clippy::all, clippy::pedantic, missing_docs)]
mod generated {
    include!(concat!(
        env!("OUT_DIR"),
        "/",
        env!("CORDIAL_OPERATIONAL_PACKAGE"),
        ".rs"
    ));
    #[cfg(feature = "json")]
    include!(concat!(
        env!("OUT_DIR"),
        "/",
        env!("CORDIAL_OPERATIONAL_PACKAGE"),
        ".serde.rs"
    ));
}
pub use generated::*;

/// The records the Dongle saves in its filesystem, generated from
/// `proto/storage/v2/storage.proto`. Each file holds one of these messages.
pub mod storage;

/// Framed desktop/CLI subprocess messages. Available only to host migration tools.
#[cfg(feature = "migration")]
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod migration {
    include!(concat!(env!("OUT_DIR"), "/cordial.migration.rs"));
    #[cfg(feature = "json")]
    include!(concat!(env!("OUT_DIR"), "/cordial.migration.serde.rs"));

    /// Maximum protobuf payload length after the four-byte little-endian length prefix.
    pub const MAX_FRAME_BYTES: usize = 65_536;
}

#[cfg(feature = "json")]
mod error_json;
pub mod frame;
pub mod handshake;
pub use handshake::{AdapterHello, ClientHello, MAX_HANDSHAKE_BYTES};

include!(concat!(env!("OUT_DIR"), "/current.rs"));

/// USB vendor ID of every Dongle (pid.codes).
pub const USB_VENDOR_ID: u16 = 0x1209;
/// USB product ID of every Dongle.
pub const USB_PRODUCT_ID: u16 = 0xc0d1;
/// The longest request a Dongle accepts, before frame encoding.
pub const MAX_REQUEST_BYTES: usize = 1024;

/// Conservative migration data chunk length, independent of the serial request envelope.
pub const STORAGE_MIGRATION_CHUNK_BYTES: usize = 512;
/// Maximum UTF-8 byte length of an absolute migration dataset path.
pub const STORAGE_MIGRATION_PATH_BYTES: usize = 192;
/// Maximum inventory file count accepted by a host before downloading or allocating.
pub const STORAGE_MIGRATION_MAX_FILES: usize = 16_384;
/// Maximum checked total input bytes accepted by a host, independent of adapter capacity.
pub const STORAGE_MIGRATION_MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
/// Host RAM budget for migration input, output and working allocations together.
pub const STORAGE_MIGRATION_MAX_MEMORY_BYTES: usize = 256 * 1024 * 1024;
