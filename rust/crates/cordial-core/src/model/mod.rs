//! The firmware's model of devices, settings, and Bluetooth state.
pub mod errors;
pub mod hidpp;
pub mod identifiers;
pub mod info;
pub mod link;
pub mod settings;
pub mod translation;

/// The largest integer saved preferences keep exactly in their JSON documents.
pub const MAX_INTEGER: u64 = 9_007_199_254_740_991;

/// A canonical absolute UTF-8 file path of at most 192 bytes.
pub fn storage_path(path: &str) -> bool {
    (path == "/" || (!path.ends_with('/') && !path.contains("//")))
        && !path.contains('\\')
        && path.starts_with('/')
        && path.len() <= 192
        && !path.chars().any(char::is_control)
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
