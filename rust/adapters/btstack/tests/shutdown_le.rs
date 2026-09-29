#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn shutdown_cancels_pending_le_and_disconnects_a_late_success() {
    common::shutdown::pending_create(false);
}
