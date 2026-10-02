#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn a_refused_page_pauses_accept_list_initiation_until_it_ends() {
    common::initiation::page_during_accept_list();
}
