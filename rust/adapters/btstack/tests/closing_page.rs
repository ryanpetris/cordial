#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn a_page_that_never_ends_releases_its_link_and_restarts() {
    common::closing::stuck_page();
}
