#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn a_page_closed_before_its_acknowledgement_is_cancelled_once_acknowledged() {
    common::closing::cancel_before_acknowledgement();
}
