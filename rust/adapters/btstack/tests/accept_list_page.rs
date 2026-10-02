#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn accept_list_initiation_continues_during_a_page() {
    common::initiation::accept_list_during_page();
}
