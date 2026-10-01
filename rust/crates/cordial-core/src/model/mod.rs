//! The firmware's model of devices, settings and Bluetooth state.
pub mod errors;
pub mod hidpp;
pub mod identifiers;
pub mod info;
pub mod link;
pub mod settings;
pub mod translation;

/// The largest integer saved preferences keep exactly in their JSON documents.
pub const MAX_INTEGER: u64 = 9_007_199_254_740_991;

/// A development file path: absolute ASCII, at most 255 bytes, with no `.` or `..` components
/// and no control bytes.
pub fn storage_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 255
        && path.is_ascii()
        && !path.bytes().any(|b| b < 32 || b == 127)
        && !path.split('/').any(|p| p == "." || p == "..")
}

/// Canonical adapter names contain 1..64 UTF-8 bytes and no control characters.
pub fn adapter_name(value: &str) -> Option<&str> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let name = value.trim();
    (!name.is_empty() && name.len() <= 64).then_some(name)
}
