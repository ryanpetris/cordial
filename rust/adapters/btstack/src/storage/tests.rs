use super::*;
#[allow(dead_code)]
mod support {
    use alloc::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}

#[test]
fn native_callbacks_cannot_replace_committed_identity_roots() {
    let store = Storage::new(support::Store::default());
    let identity = block_on(cordial_core::identity::Identity::initialize(
        &mut store.handle(),
        [2; 6],
        || 42,
    ))
    .unwrap();
    store.cache_identity().unwrap();
    let api = Storage::<support::Store>::API;
    let mut bytes = [0; 16];
    unsafe {
        assert_eq!(
            (api.get)(store.context(), 0x534d4952, bytes.as_mut_ptr(), 16),
            16
        );
        assert_eq!(bytes, identity.0[8..24]);
        assert_eq!(
            (api.store)(store.context(), 0x534d4952, [9; 16].as_ptr(), 16),
            -1
        );
    }
    assert_eq!(store.error(), Some(Error::Corrupt));
}
