#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn a_connection_before_the_direct_cancel_keeps_the_link() {
    common::initiation::resync_race_keeps_the_link();
}
