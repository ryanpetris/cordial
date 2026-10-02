#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn an_accept_list_cancel_that_never_ends_restarts() {
    common::closing::stuck_accept_list_cancel();
}
