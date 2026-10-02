#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn a_refused_le_creation_waits_for_the_page() {
    common::initiation::le_creation_during_page();
}
