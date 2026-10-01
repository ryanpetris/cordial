#![cfg(feature = "ffi")]
mod common;
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::model::errors::ErrorCode;
use cordial_core::{
    bluetooth::Bluetooth,
    storage::{Error, RecordKey, RecordStore},
};
use embassy_futures::block_on;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

struct Records {
    data: Rc<RefCell<BTreeMap<RecordKey, Vec<u8>>>>,
    fail_once: bool,
}
fn root(_: &[u8; 4]) -> RecordKey {
    cordial_core::identity::KEY
}
impl RecordStore for Records {
    async fn keys(&mut self) -> Result<Vec<RecordKey>, Error> {
        Ok(self.data.borrow().keys().copied().collect())
    }
    async fn available(&mut self) -> Result<usize, Error> {
        Ok(65536)
    }

    async fn load(&mut self, key: RecordKey, output: &mut [u8]) -> Result<Option<usize>, Error> {
        if self.fail_once && key == root(b"SMER") {
            self.fail_once = false;
            return Err(Error::Io);
        }
        Ok(self.data.borrow().get(&key).map(|bytes| {
            output[..bytes.len()].copy_from_slice(bytes);
            bytes.len()
        }))
    }
    async fn save(&mut self, key: RecordKey, value: &[u8]) -> Result<(), Error> {
        self.data.borrow_mut().insert(key, value.to_vec());
        Ok(())
    }
    async fn remove(&mut self, key: RecordKey) -> Result<(), Error> {
        self.data.borrow_mut().remove(&key);
        Ok(())
    }
}
#[test]
fn a_transient_security_root_read_failure_never_regenerates_persisted_identity() {
    let data = Rc::new(RefCell::new(BTreeMap::new()));
    let mut init = Records {
        data: data.clone(),
        fail_once: false,
    };
    block_on(cordial_core::identity::Identity::initialize(
        &mut init,
        [2; 6],
        || 42,
    ))
    .unwrap();
    let io = Box::leak(Box::new(Io::new()));
    let storage = Box::leak(Box::new(Storage::new(Records {
        data: data.clone(),
        fail_once: true,
    })));
    let state = Box::leak(Box::new(
        State::new(storage, io, || 0, || std::process::abort()).unwrap(),
    ));
    let mut radio = unsafe { Backend::new(state, None) };
    assert_eq!(
        radio.start(Some([2, 3, 4, 5, 6, 7])),
        Err(ErrorCode::StorageFailed)
    );
    let persisted = data.borrow()[&root(b"SMER")].clone();
    assert_eq!(block_on(radio.bonds()), Err(ErrorCode::StorageFailed));
    assert_eq!(radio.scan(1, false, true), Err(ErrorCode::StorageFailed));
    let api = Storage::<Records>::API;
    unsafe {
        assert_eq!(
            (api.store)(
                storage.context(),
                u32::from_be_bytes(*b"SMER"),
                [9; 16].as_ptr(),
                16
            ),
            -1
        );
        (api.delete)(storage.context(), u32::from_be_bytes(*b"SMIR"));
    }
    assert_eq!(data.borrow()[&root(b"SMER")], persisted);
}
