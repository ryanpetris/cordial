#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn a_failed_direct_cancel_restarts_bluetooth() {
    common::initiation::failed_resync_restarts();
}
