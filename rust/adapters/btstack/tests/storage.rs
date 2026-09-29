use cordial_btstack::storage::Storage;
use cordial_core::storage::{Error, RecordKey, RecordStore, record_key};
use embassy_futures::block_on;
use std::{collections::BTreeMap, ptr};

#[derive(Default)]
struct Records {
    records: BTreeMap<RecordKey, Vec<u8>>,
    fail: bool,
}
impl RecordStore for Records {
    async fn keys(&mut self) -> Result<Vec<RecordKey>, Error> {
        if self.fail {
            return Err(Error::Io);
        }
        Ok(self.records.keys().copied().collect())
    }
    async fn available(&mut self) -> Result<usize, Error> {
        if self.fail {
            return Err(Error::Io);
        }
        Ok(65536)
    }

    async fn load(&mut self, key: RecordKey, bytes: &mut [u8]) -> Result<Option<usize>, Error> {
        if self.fail {
            return Err(Error::Io);
        }
        Ok(self.records.get(&key).map(|v| {
            bytes[..v.len()].copy_from_slice(v);
            v.len()
        }))
    }
    async fn save(&mut self, key: RecordKey, value: &[u8]) -> Result<(), Error> {
        if self.fail {
            return Err(Error::Io);
        }
        self.records.insert(key, value.to_vec());
        Ok(())
    }
    async fn remove(&mut self, key: RecordKey) -> Result<(), Error> {
        if self.fail {
            return Err(Error::Io);
        }
        self.records.remove(&key);
        Ok(())
    }
}

#[test]
fn root_read_failure_is_sticky_and_reported_once() {
    let bad = Storage::new(Records {
        fail: true,
        ..Records::default()
    });
    assert_eq!(bad.cache_identity(), Err(Error::Io));
    let api = Storage::<Records>::API;
    let mut bytes = [7; 16];
    unsafe {
        assert_eq!(
            (api.get)(bad.context(), 0x534d4952, bytes.as_mut_ptr(), 16),
            0
        );
    }
    assert_eq!(bytes, [0; 16]);
    assert_eq!(bad.take_error(), Some(Error::Io));
    assert_eq!(bad.take_error(), None);
    assert_eq!(
        block_on(bad.handle().save(record_key(1, 0), &[0])),
        Err(Error::Io)
    );
    assert_eq!(
        unsafe { (api.get)(bad.context(), 0x534d4952, ptr::null_mut(), 0) },
        0
    );
}
