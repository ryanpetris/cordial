#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
#[test]
fn pending_classic_page_cannot_block_shutdown_of_an_established_le_link() {
    common::shutdown::pending_create(true);
}
