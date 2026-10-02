#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn an_le_cancel_that_never_ends_releases_its_link_and_restarts() {
    common::closing::stuck_le_cancel();
}
