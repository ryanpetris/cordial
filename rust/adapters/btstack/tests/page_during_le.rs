#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn a_refused_page_cancels_le_creation_and_serializes_setup() {
    common::initiation::page_during_le_creation();
}
