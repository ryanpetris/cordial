use cordial_btstack::storage::Storage;
use cordial_core::storage::{Error, RecordKey, RecordStore, record_key};
use embassy_futures::block_on;
use std::{collections::BTreeMap, ptr};

#[derive(Default)]
struct Records {
    records: BTreeMap<RecordKey, Vec<u8>>,
    files: BTreeMap<String, Vec<u8>>,
    fail: bool,
}
impl RecordStore for Records {
    async fn file_size(&mut self, path: &str) -> Result<Option<usize>, Error> {
        if self.fail {
            return Err(Error::Io);
        }
        Ok(self.files.get(path).map(Vec::len))
    }
    async fn file_write(&mut self, path: &str, offset: u32, data: &[u8]) -> Result<(), Error> {
        if self.fail {
            return Err(Error::Io);
        }
        if offset == 0 {
            self.files.insert(path.into(), data.into());
            return Ok(());
        }
        let file = self.files.get_mut(path).ok_or(Error::Missing)?;
        let offset = offset as usize;
        if offset > file.len() {
            return Err(Error::Bounds);
        }
        file.resize(file.len().max(offset + data.len()), 0);
        file[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
    async fn file_move(&mut self, source: &str, destination: &str) -> Result<(), Error> {
        if self.fail {
            return Err(Error::Io);
        }
        let bytes = self.files.remove(source).ok_or(Error::Missing)?;
        self.files.insert(destination.into(), bytes);
        Ok(())
    }
    async fn file_delete(&mut self, path: &str) -> Result<(), Error> {
        if self.fail {
            return Err(Error::Io);
        }
        self.files.remove(path);
        Ok(())
    }
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

#[test]
fn file_commands_forward_through_the_shared_store_even_after_a_native_fault() {
    let store = Storage::new(Ok::<_, Error>(Records::default()));
    let mut handle = store.handle();
    assert_eq!(store.cache_identity(), Err(Error::Corrupt));
    block_on(async {
        assert_eq!(handle.file_size("/data").await, Ok(None));
        handle.file_write("/data", 0, b"start").await.unwrap();
        handle.file_write("/data", 5, b"end").await.unwrap();
        assert_eq!(handle.file_size("/data").await, Ok(Some(8)));
        assert_eq!(
            handle.file_write("/data", 9, b"gap").await,
            Err(Error::Bounds)
        );
        handle.file_move("/data", "/other").await.unwrap();
        assert_eq!(handle.file_size("/data").await, Ok(None));
        assert_eq!(handle.file_size("/other").await, Ok(Some(8)));
        handle.file_delete("/other").await.unwrap();
        handle.file_delete("/other").await.unwrap();
        assert_eq!(handle.file_size("/other").await, Ok(None));
    });
    assert_eq!(store.error(), Some(Error::Corrupt));
}

#[test]
fn file_commands_preserve_backend_errors_and_refuse_reentrant_access() {
    block_on(async {
        let store = Storage::new(Records {
            fail: true,
            ..Default::default()
        });
        let mut handle = store.handle();
        assert_eq!(handle.file_size("/data").await, Err(Error::Io));
        assert_eq!(handle.file_write("/data", 0, b"data").await, Err(Error::Io));
        assert_eq!(handle.file_move("/data", "/other").await, Err(Error::Io));
        assert_eq!(handle.file_delete("/data").await, Err(Error::Io));
        let store = Storage::new(Records::default());
        let mut handle = store.handle();
        let key = record_key(1, 0);
        handle.save(key, b"data").await.unwrap();
        let mut entered = false;
        handle
            .read_parts(key, &mut |_| {
                entered = true;
                let mut other = store.handle();
                assert_eq!(block_on(other.file_size("/data")), Err(Error::Unavailable));
                assert_eq!(
                    block_on(other.file_write("/data", 0, b"data")),
                    Err(Error::Unavailable)
                );
                assert_eq!(
                    block_on(other.file_move("/data", "/other")),
                    Err(Error::Unavailable)
                );
                assert_eq!(
                    block_on(other.file_delete("/data")),
                    Err(Error::Unavailable)
                );
                true
            })
            .await
            .unwrap();
        assert!(entered);
    });
}
