#![no_std]

extern crate alloc;

pub mod codec;
pub mod errors;
pub mod hidpp;
pub mod identifiers;
pub mod info;
pub mod messages;
pub mod payloads;
pub mod settings;
pub mod translation;

pub const PROTOCOL_VERSION: u8 = 1;
/// Includes the terminating LF.
pub const MAX_LINE_BYTES: usize = 4096;
pub const MAX_REQUEST_ID: u32 = 2_147_483_647;
pub const MAX_REVISION: u64 = 9_007_199_254_740_991;
pub const HEARTBEAT_INTERVAL_MS: u32 = 5_000;
pub const HEARTBEAT_TIMEOUT_MS: u32 = 15_000;

/// Canonical adapter names contain 1..64 UTF-8 bytes and no control characters.
pub fn adapter_name(value: &str) -> Option<&str> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let name = value.trim();
    (!name.is_empty() && name.len() <= 64).then_some(name)
}
