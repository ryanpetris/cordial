use cordial_core::storage::Error;
use embassy_futures::block_on;
use embedded_storage_async::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashErrorKind, ReadNorFlash,
};
type Storage = cordial_record_storage::Storage<Flash>;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
struct Flash(Rc<RefCell<Vec<u8>>>);
impl Flash {
    fn blank() -> Self {
        Self::sized(128 * 1024)
    }
    fn sized(bytes: usize) -> Self {
        Self(Rc::new(RefCell::new(vec![0xff; bytes])))
    }
}
impl ErrorType for Flash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for Flash {
    const READ_SIZE: usize = 1;
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let data = self.0.borrow();
        let source = data
            .get(offset as usize..offset as usize + bytes.len())
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        bytes.copy_from_slice(source);
        Ok(())
    }
    fn capacity(&self) -> usize {
        self.0.borrow().len()
    }
}
impl NorFlash for Flash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        if !(from as usize).is_multiple_of(Self::ERASE_SIZE)
            || !(to as usize).is_multiple_of(Self::ERASE_SIZE)
        {
            return Err(NorFlashErrorKind::NotAligned);
        }
        self.0
            .borrow_mut()
            .get_mut(from as usize..to as usize)
            .ok_or(NorFlashErrorKind::OutOfBounds)?
            .fill(0xff);
        Ok(())
    }
    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let mut data = self.0.borrow_mut();
        let target = data
            .get_mut(offset as usize..offset as usize + bytes.len())
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        for (old, new) in target.iter_mut().zip(bytes) {
            if *old & *new != *new {
                return Err(NorFlashErrorKind::Other);
            }
            *old &= *new;
        }
        Ok(())
    }
}
impl MultiwriteNorFlash for Flash {}

#[test]
fn nonblank_and_stale_markers_recover_only_the_owned_filesystem() {
    block_on(async {
        for offset in [4096, 4196, 8192, 128 * 1024 - 1] {
            let flash = Flash::sized(132 * 1024);
            flash.0.borrow_mut()[..4096].fill(0x42);
            flash.0.borrow_mut()[128 * 1024..].fill(0x24);
            flash.0.borrow_mut()[offset] = 0;
            let mut fs = Storage::open_or_recover(flash.clone(), 4096..128 * 1024, [1; 32])
                .await
                .unwrap();
            assert_eq!(
                cordial_core::storage::format(&mut fs).await.unwrap().format,
                1
            );
            assert!(flash.0.borrow()[..4096].iter().all(|v| *v == 0x42));
            assert!(flash.0.borrow()[128 * 1024..].iter().all(|v| *v == 0x24));
        }
    });
}
#[test]
fn files_span_sectors_and_atomic_replacement_preserves_exact_bytes() {
    block_on(async {
        let flash = Flash::blank();
        let mut store = Storage::provision_blank(flash.clone(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let path = "/devices/1/device.pb";
        let data: Vec<u8> = (0..9000).map(|n| (n % 251) as u8).collect();
        store.replace_file(path, &data).unwrap();
        let generation = store.generation();
        let before = flash.0.borrow().clone();
        store.replace_file(path, &data).unwrap();
        assert_eq!(store.generation(), generation);
        assert!(*flash.0.borrow() == before, "equal bytes are not written");
        for n in 0..300 {
            store
                .replace_file("/adapter.pb", &u32::to_le_bytes(n))
                .unwrap();
        }
        let mut store = Storage::open(store.into_flash(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let mut got = Vec::new();
        let mut bytes = [0; 512];
        loop {
            let n = store.read_file(path, got.len() as u32, &mut bytes).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&bytes[..n]);
        }
        assert_eq!(got, data);
        assert_eq!(
            store
                .entry("/devices", 0)
                .unwrap()
                .unwrap()
                .file_name()
                .as_str(),
            "1"
        );
        assert!(store.entry("/devices", 1).unwrap().is_none());
        // A file is saved in place, beside nothing else.
        assert_eq!(
            store
                .entry("/devices/1", 0)
                .unwrap()
                .unwrap()
                .file_name()
                .as_str(),
            "device.pb"
        );
        assert!(store.entry("/devices/1", 1).unwrap().is_none());
        store.remove_file(path).unwrap();
        assert_eq!(store.file_size(path).unwrap(), None);
        assert!(store.read_file("/../identity.pb", 0, &mut bytes).is_err());
        assert_eq!(&flash.0.borrow()[..4096], &[0xff; 4096]);
    });
}
#[test]
fn preferences_share_one_file_and_have_no_512_byte_ceiling() {
    use cordial_core::model::{
        hidpp::{FeatureId, FeatureRevision},
        settings::{SettingKey, SettingScope},
    };
    use cordial_core::{
        compact::{Metadata, Preference},
        settings::PreferenceStore,
        storage::Preferences,
    };
    block_on(async {
        let mut store = Storage::provision_blank(Flash::blank(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let mut pref = Preference {
            metadata: Metadata {
                key: SettingKey::BacklightEnabled,
                feature: FeatureId::BACKLIGHT,
                revision: FeatureRevision(3),
                scope: SettingScope::Device,
                choices: Box::new([]),
                range: None,
            },
            value: 1,
        };
        for device in [1, 2] {
            let mut preferences = Preferences {
                store: &mut store,
                device,
            };
            preferences.save(&pref).await.unwrap();
            assert_eq!(preferences.load_all().await.unwrap(), vec![pref.clone()]);
        }
        pref.value = 0;
        Preferences {
            store: &mut store,
            device: 1,
        }
        .save(&pref)
        .await
        .unwrap();
        assert_eq!(
            store
                .entry("/devices/1", 0)
                .unwrap()
                .unwrap()
                .file_name()
                .as_str(),
            "settings.pb"
        );
        Preferences {
            store: &mut store,
            device: 1,
        }
        .remove_all()
        .await
        .unwrap();
        assert_eq!(
            Preferences {
                store: &mut store,
                device: 2
            }
            .load_all()
            .await
            .unwrap()
            .len(),
            1
        );
        store.replace_file("/large.bin", &[42; 8192]).unwrap();
    });
}

struct CutFlash {
    flash: Flash,
    remaining: usize,
    cut: bool,
}
impl ErrorType for CutFlash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for CutFlash {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        if self.cut {
            return Err(NorFlashErrorKind::Other);
        }
        self.flash.read(offset, bytes).await
    }
}
impl CutFlash {
    fn mutation(&mut self) -> Result<(), NorFlashErrorKind> {
        if self.remaining == 0 {
            self.cut = true;
            return Err(NorFlashErrorKind::Other);
        }
        self.remaining -= 1;
        Ok(())
    }
}
impl NorFlash for CutFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn write(&mut self, off: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.mutation()?;
        self.flash.write(off, bytes).await
    }
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.mutation()?;
        self.flash.erase(from, to).await
    }
}
#[test]
fn interrupted_replacements_preserve_old_or_new_complete_file() {
    // Inline and block-backed contents, growing and shrinking.
    for (old, new) in [(7000, 11000), (11000, 300), (100, 200), (300, 5000)] {
        interrupted_replacement(vec![13; old], vec![27; new]);
    }
}
fn interrupted_replacement(old: Vec<u8>, new: Vec<u8>) {
    block_on(async {
        let flash = Flash::blank();
        let mut fs = Storage::provision_blank(flash.clone(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        fs.replace_file("/device.pb", &old).unwrap();
        let baseline = flash.0.borrow().clone();
        let mut completed = false;
        for cut in 0..100 {
            let flash = Flash(Rc::new(RefCell::new(baseline.clone())));
            let driver = CutFlash {
                flash: flash.clone(),
                remaining: cut,
                cut: false,
            };
            let mut fs =
                cordial_record_storage::Storage::<_>::open(driver, 4096..128 * 1024, [1; 32])
                    .await
                    .unwrap();
            let result = fs.replace_file("/device.pb", &new);
            let mut reopened = Storage::open(flash, 4096..128 * 1024, [1; 32])
                .await
                .unwrap();
            let mut data = vec![0; 12000];
            let n = reopened.read_file("/device.pb", 0, &mut data).unwrap();
            assert!(data[..n] == old || data[..n] == new, "cut {cut}");
            if result.is_ok() {
                assert_eq!(data[..n], new);
                completed = true;
                break;
            }
        }
        assert!(completed);
    });
}

#[test]
fn interrupted_creations_leave_no_record_or_the_complete_record() {
    use cordial_core::storage::{RecordStore, record_key};
    for size in [200, 6000] {
        block_on(async {
            let flash = Flash::blank();
            let range = 4096..128 * 1024;
            let mut fs = Storage::provision_blank(flash.clone(), range.clone(), [1; 32])
                .await
                .unwrap();
            fs.save(record_key(2, 1), b"saved").await.unwrap();
            let baseline = flash.0.borrow().clone();
            let new = vec![27; size];
            let mut completed = false;
            let mut empty = false;
            for cut in 0..100 {
                let flash = Flash(Rc::new(RefCell::new(baseline.clone())));
                let driver = CutFlash {
                    flash: flash.clone(),
                    remaining: cut,
                    cut: false,
                };
                let mut fs =
                    cordial_record_storage::Storage::<_>::open(driver, range.clone(), [1; 32])
                        .await
                        .unwrap();
                let result = fs.save(record_key(2, 2), &new).await;
                drop(fs);
                // Inspect the flash as the interruption left it; this mount cannot clean up.
                let mut left = cordial_record_storage::Storage::<_>::open(
                    RejectedWrites(flash.clone()),
                    range.clone(),
                    [1; 32],
                )
                .await
                .unwrap();
                empty |= left.file_size("/devices/2/device.pb").unwrap() == Some(0);
                drop(left);
                let mut reopened = Storage::open(flash, range.clone(), [1; 32]).await.unwrap();
                let loaded = reopened.load_owned(record_key(2, 2)).await.unwrap();
                let keys = reopened.keys().await.unwrap();
                match &loaded {
                    None => {
                        assert_eq!(keys, [record_key(2, 1)], "cut {cut}");
                        // Mounting removed the directory of the incomplete record.
                        assert!(
                            reopened.entry("/devices", 1).unwrap().is_none(),
                            "cut {cut}"
                        );
                    }
                    Some(bytes) => {
                        assert_eq!(*bytes, new, "cut {cut}");
                        assert_eq!(keys, [record_key(2, 1), record_key(2, 2)]);
                    }
                }
                if result.is_ok() {
                    assert!(loaded.is_some());
                    completed = true;
                    break;
                }
            }
            assert!(completed);
            assert!(empty, "some interruption leaves the created file empty");
        });
    }
}

/// Flash whose one mutation numbered `fail_at` fails, and every other access succeeds.
struct OneFailure {
    flash: Flash,
    mutations: usize,
    fail_at: usize,
}
impl ErrorType for OneFailure {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for OneFailure {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(offset, bytes).await
    }
}
impl OneFailure {
    fn mutation(&mut self) -> Result<(), NorFlashErrorKind> {
        self.mutations += 1;
        if self.mutations - 1 == self.fail_at {
            Err(NorFlashErrorKind::Other)
        } else {
            Ok(())
        }
    }
}
impl NorFlash for OneFailure {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn write(&mut self, off: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.mutation()?;
        self.flash.write(off, bytes).await
    }
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.mutation()?;
        self.flash.erase(from, to).await
    }
}

#[test]
fn a_failed_creation_reclaims_its_empty_file_and_directory() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let mut fs = Storage::provision_blank(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        fs.save(record_key(2, 1), b"saved").await.unwrap();
        let baseline = flash.0.borrow().clone();
        let new = vec![27; 6000];
        let mut completed = false;
        for fail_at in 0..100 {
            let flash = Flash(Rc::new(RefCell::new(baseline.clone())));
            let driver = OneFailure {
                flash: flash.clone(),
                mutations: 0,
                fail_at,
            };
            let mut fs = cordial_record_storage::Storage::<_>::open(driver, range.clone(), [1; 32])
                .await
                .unwrap();
            let result = fs.save(record_key(2, 2), &new).await;
            let mut left = cordial_record_storage::Storage::<_>::open(
                RejectedWrites(flash.clone()),
                range.clone(),
                [1; 32],
            )
            .await
            .unwrap();
            let directory = left.file_size("/devices/2");
            assert_eq!(
                left.load_owned(record_key(2, 1)).await.unwrap(),
                Some(b"saved".to_vec())
            );
            if result.is_ok() {
                assert_eq!(left.load_owned(record_key(2, 2)).await.unwrap(), Some(new));
                completed = true;
                break;
            }
            // Nothing is left of the record, not even its directory.
            assert!(
                matches!(directory, Ok(None)),
                "fail at {fail_at}: {directory:?}"
            );
        }
        assert!(completed);
    });
}

#[test]
fn an_empty_record_file_holds_no_record() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let mut store = Storage::provision_blank(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        store.replace_file("/format.pb", b"").unwrap();
        store.replace_file("/devices/3/device.pb", b"").unwrap();
        store.replace_file("/devices/4/settings.pb", b"").unwrap();
        store.save(record_key(2, 4), b"device").await.unwrap();
        assert_eq!(store.next_key(None).await.unwrap(), Some(record_key(2, 4)));
        assert_eq!(store.keys().await.unwrap(), [record_key(2, 4)]);
        let mut bytes = [0; 8];
        assert_eq!(store.load(record_key(0, 0), &mut bytes).await, Ok(None));
        assert_eq!(store.load_owned(record_key(2, 3)).await, Ok(None));
        assert_eq!(store.load_owned(record_key(4, 4)).await, Ok(None));
        // Saving over an empty file writes the record.
        store.save(record_key(4, 4), b"settings").await.unwrap();
        assert_eq!(
            store.load_owned(record_key(4, 4)).await.unwrap(),
            Some(b"settings".to_vec())
        );
        drop(store);
        let mut store = Storage::open(flash, range, [1; 32]).await.unwrap();
        assert_eq!(store.record_ids(2, 0, 4).await.unwrap(), [4]);
    });
}
#[test]
fn matching_guard_with_broken_filesystem_is_reformatted() {
    block_on(async {
        let flash = Flash::blank();
        flash.0.borrow_mut()[4096..4128].fill(1);
        let mut store = Storage::open_or_recover(flash, 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        assert_eq!(
            cordial_core::storage::format(&mut store)
                .await
                .unwrap()
                .format,
            1
        );
    });
}

#[test]
fn interrupted_startup_recovers_with_a_bounded_retry() {
    block_on(async {
        let mut completed = false;
        for cut in 0..80 {
            let flash = Flash::blank();
            let driver = CutFlash {
                flash: flash.clone(),
                remaining: cut,
                cut: false,
            };
            let result =
                cordial_record_storage::Storage::open_or_recover(driver, 4096..128 * 1024, [1; 32])
                    .await;
            let mut reopened = Storage::open_or_recover(flash, 4096..128 * 1024, [1; 32])
                .await
                .unwrap();
            assert_eq!(
                cordial_core::storage::format(&mut reopened)
                    .await
                    .unwrap()
                    .format,
                1
            );
            if result.is_ok() {
                completed = true;
                break;
            }
        }
        assert!(completed);
    });
}

#[test]
fn interrupted_device_deletion_never_restores_a_committed_device() {
    block_on(async {
        let flash = Flash::blank();
        let mut fs = Storage::provision_blank(flash.clone(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let device = "/devices/1/device.pb";
        fs.replace_file(device, b"device").unwrap();
        fs.replace_file("/devices/1/settings.pb", b"preferences")
            .unwrap();
        let baseline = flash.0.borrow().clone();
        let mut completed = false;
        for cut in 0..40 {
            let flash = Flash(Rc::new(RefCell::new(baseline.clone())));
            let driver = CutFlash {
                flash: flash.clone(),
                remaining: cut,
                cut: false,
            };
            let mut fs =
                cordial_record_storage::Storage::<_>::open(driver, 4096..128 * 1024, [1; 32])
                    .await
                    .unwrap();
            let result = fs.remove_file(device);
            let mut reopened = Storage::open(flash, 4096..128 * 1024, [1; 32])
                .await
                .unwrap();
            let size = reopened.file_size(device).unwrap();
            assert!(size == Some(6) || size.is_none());
            if size.is_none() {
                assert!(reopened.entry("/devices", 0).unwrap().is_none());
            }
            if result.is_ok() {
                assert!(size.is_none());
                completed = true;
                break;
            }
        }
        assert!(completed);
    });
}

#[test]
fn full_filesystem_preserves_old_file_and_allows_deletion() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let mut fs = Storage::provision_blank(Flash::blank(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        fs.replace_file("/important", &[7; 8000]).unwrap();
        let mut full = None;
        for i in 0..100 {
            let name = format!("/fill{i}");
            if let Err(e) = fs.replace_file(&name, &[3; 4096]) {
                assert_eq!(e, Error::Full);
                full = Some(name);
                break;
            }
        }
        // A failed creation leaves no file behind.
        assert_eq!(fs.file_size(&full.unwrap()).unwrap(), None);
        assert!(fs.replace_file("/important", &[8; 120 * 1024]).is_err());
        // Nor does a failed record creation leave its record directory.
        assert_eq!(
            fs.save(record_key(4, 9), &[1; 4096]).await,
            Err(Error::Full)
        );
        assert!(matches!(fs.file_size("/devices/9"), Ok(None)));
        let mut bytes = [0; 8001];
        assert_eq!(fs.read_file("/important", 0, &mut bytes).unwrap(), 8000);
        assert_eq!(bytes[..8000], [7; 8000]);
        fs.remove_file("/important").unwrap();
        assert_eq!(fs.file_size("/important").unwrap(), None);
    });
}

#[test]
fn device_documents_have_their_own_files_and_layouts_are_not_enumerated() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let mut store = Storage::provision_blank(Flash::blank(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        for kind in [5, 4, 2] {
            store.save(record_key(kind, 1), &[kind]).await.unwrap();
        }
        for name in ["device.pb", "settings.pb", "layout.pb"] {
            let path = format!("/devices/1/{name}");
            assert_eq!(store.file_size(&path).unwrap(), Some(1), "{name}");
        }
        // Layouts are read by key and never enumerated.
        assert_eq!(
            store.keys().await.unwrap(),
            [record_key(2, 1), record_key(4, 1)]
        );
        let mut store = Storage::open(store.into_flash(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let mut bytes = [0; 4];
        assert_eq!(
            store.load(record_key(5, 1), &mut bytes).await.unwrap(),
            Some(1)
        );
        assert_eq!(bytes[0], 5);
        store.remove(record_key(5, 1)).await.unwrap();
        assert_eq!(
            store.load(record_key(5, 1), &mut bytes).await.unwrap(),
            None
        );
        assert_eq!(store.file_size("/devices/1/layout.pb").unwrap(), None);
    });
}

#[test]
fn profile_records_survive_remount_and_enumerate_without_device_directories() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let mut store = Storage::provision_blank(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        let key = record_key(8, 42);
        let bytes = vec![b'x'; 2048];
        store.save(key, &bytes).await.unwrap();
        assert!(store.keys().await.unwrap().contains(&key));
        drop(store);
        let mut store = Storage::open(flash, range, [1; 32]).await.unwrap();
        assert_eq!(store.load_owned(key).await.unwrap().unwrap(), bytes);
        store.remove(key).await.unwrap();
        assert!(!store.keys().await.unwrap().contains(&key));
    });
}

#[test]
fn mounting_reclaims_interrupted_profile_files() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let mut store = Storage::provision_blank(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        store.save(record_key(8, 1), b"saved").await.unwrap();
        store.save(record_key(9, 1), b"rules").await.unwrap();
        // A copy interrupted before its profile.pb commits leaves only its rules.
        store.save(record_key(9, 2), &[b'x'; 8000]).await.unwrap();
        // An interrupted creation leaves an empty profile.pb.
        store.save(record_key(9, 3), &[b'x'; 8000]).await.unwrap();
        store.replace_file("/profiles/3/profile.pb", b"").unwrap();
        store
            .replace_file("/profiles/4/profile.pb/x", &[b'x'; 8000])
            .unwrap();
        let before = store.available().await.unwrap();
        drop(store);
        let mut store = Storage::open(flash, range, [1; 32]).await.unwrap();
        assert!(store.available().await.unwrap() > before);
        assert_eq!(
            store.load_owned(record_key(8, 1)).await.unwrap(),
            Some(b"saved".to_vec())
        );
        assert_eq!(
            store.load_owned(record_key(9, 1)).await.unwrap(),
            Some(b"rules".to_vec())
        );
        assert_eq!(store.load_owned(record_key(9, 2)).await.unwrap(), None);
        assert_eq!(store.load_owned(record_key(9, 3)).await.unwrap(), None);
        assert_eq!(
            store
                .entry("/profiles", 0)
                .unwrap()
                .unwrap()
                .file_name()
                .as_str(),
            "1"
        );
        assert!(store.entry("/profiles", 1).unwrap().is_none());
    });
}

#[test]
fn profile_rules_have_their_own_file_and_are_not_enumerated() {
    use cordial_core::storage::{Error, RecordStore, record_key};
    block_on(async {
        let mut store = Storage::provision_blank(Flash::blank(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        store.save(record_key(9, 7), b"rules").await.unwrap();
        store.save(record_key(8, 7), b"profile").await.unwrap();
        assert_eq!(store.file_size("/profiles/7/rules.pb").unwrap(), Some(5));
        assert_eq!(store.file_size("/profiles/7/profile.pb").unwrap(), Some(7));
        assert_eq!(store.keys().await.unwrap(), [record_key(8, 7)]);
        assert_eq!(
            store.save(record_key(9, 0), b"rules").await,
            Err(Error::Bounds)
        );
        store.remove(record_key(9, 7)).await.unwrap();
        assert_eq!(store.load_owned(record_key(9, 7)).await.unwrap(), None);
        assert_eq!(
            store.load_owned(record_key(8, 7)).await.unwrap(),
            Some(b"profile".to_vec())
        );
        // Removing the commit file of an otherwise empty profile removes its directory.
        store.remove(record_key(8, 7)).await.unwrap();
        assert!(store.entry("/profiles", 0).unwrap().is_none());
    });
}

#[test]
fn record_ids_page_record_directories_in_ascending_numeric_order() {
    use cordial_core::storage::{Error, RecordStore, record_key};
    block_on(async {
        // Every directory takes a metadata pair, more than the small test filesystem holds.
        let mut store = cordial_record_storage::Storage::<_>::provision_blank(
            Flash::sized(512 * 1024),
            4096..512 * 1024,
            [1; 32],
        )
        .await
        .unwrap();
        assert!(store.record_ids(2, 0, 4).await.unwrap().is_empty());
        assert!(store.record_ids(8, 0, 4).await.unwrap().is_empty());
        // Directory order differs from numeric order: "10" sorts before "2" by name.
        for id in [11, 2, 10, 1, 3] {
            store.save(record_key(2, id), b"device").await.unwrap();
        }
        for id in [20, 4, 100] {
            store.save(record_key(8, id), b"profile").await.unwrap();
        }
        // Record directories without a record of the kind are listed; reading them finds nothing.
        store.save(record_key(4, 5), b"settings").await.unwrap();
        store.save(record_key(9, 6), b"rules").await.unwrap();
        store.replace_file("/devices/7/device.pb/x", b"x").unwrap();
        store
            .replace_file("/profiles/8/profile.pb/x", b"x")
            .unwrap();
        // Files and directories that are not record directories are not.
        store.replace_file("/devices/9", b"file").unwrap();
        for name in ["/devices/012/device.pb", "/devices/abc/device.pb"] {
            store.replace_file(name, b"device").unwrap();
        }
        store.replace_file("/profiles/04/profile.pb", b"x").unwrap();

        let mut pages = Vec::new();
        let mut after = 0;
        loop {
            let page = store.record_ids(2, after, 2).await.unwrap();
            let Some(&last) = page.last() else { break };
            after = last;
            pages.push(page);
        }
        assert_eq!(pages, [vec![1, 2], vec![3, 5], vec![7, 10], vec![11]]);
        assert_eq!(
            store.record_ids(2, 0, 16).await.unwrap(),
            [1, 2, 3, 5, 7, 10, 11]
        );
        assert_eq!(store.record_ids(2, 3, 1).await.unwrap(), [5]);
        let mut bytes = [0; 16];
        assert_eq!(store.load(record_key(2, 5), &mut bytes).await, Ok(None));
        assert!(store.record_ids(2, 0, 0).await.unwrap().is_empty());

        assert_eq!(store.record_ids(8, 0, 2).await.unwrap(), [4, 6]);
        assert_eq!(store.record_ids(8, 20, 2).await.unwrap(), [100]);
        assert!(store.record_ids(8, 100, 2).await.unwrap().is_empty());
        assert_eq!(
            store.record_ids(8, 0, 16).await.unwrap(),
            [4, 6, 8, 20, 100]
        );

        for kind in [0, 4, 5, 9] {
            assert_eq!(store.record_ids(kind, 0, 2).await, Err(Error::Bounds));
        }
    });
}

/// Counts reads, fails every access while `fail` is set and fails writes and
/// erases while `fail_writes` is set.
struct ProbeFlash {
    flash: Flash,
    reads: Rc<std::cell::Cell<usize>>,
    fail: Rc<std::cell::Cell<bool>>,
    fail_writes: Rc<std::cell::Cell<bool>>,
}
impl ErrorType for ProbeFlash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for ProbeFlash {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.reads.set(self.reads.get() + 1);
        if self.fail.get() {
            return Err(NorFlashErrorKind::Other);
        }
        self.flash.read(offset, bytes).await
    }
}
impl NorFlash for ProbeFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn write(&mut self, off: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        if self.fail.get() || self.fail_writes.get() {
            return Err(NorFlashErrorKind::Other);
        }
        self.flash.write(off, bytes).await
    }
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        if self.fail.get() || self.fail_writes.get() {
            return Err(NorFlashErrorKind::Other);
        }
        self.flash.erase(from, to).await
    }
}

#[test]
fn operations_reuse_the_mount_and_remount_after_a_failure() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let flash = Flash::sized(512 * 1024);
        let range = 4096..512 * 1024;
        let mut store = cordial_record_storage::Storage::<_>::provision_blank(
            flash.clone(),
            range.clone(),
            [1; 32],
        )
        .await
        .unwrap();
        for id in 1..=32 {
            store.save(record_key(8, id), b"profile").await.unwrap();
        }
        drop(store);
        let reads = Rc::new(std::cell::Cell::new(0));
        let fail = Rc::new(std::cell::Cell::new(false));
        let fail_writes = Rc::new(std::cell::Cell::new(false));
        let driver = ProbeFlash {
            flash,
            reads: reads.clone(),
            fail: fail.clone(),
            fail_writes: fail_writes.clone(),
        };
        let mut store = cordial_record_storage::Storage::<_>::open(driver, range, [1; 32])
            .await
            .unwrap();
        let load = async |store: &mut cordial_record_storage::Storage<ProbeFlash>| {
            reads.set(0);
            let mut bytes = [0; 16];
            let result = store.load(record_key(8, 20), &mut bytes).await;
            (result, reads.get())
        };
        let (result, mounted) = load(&mut store).await;
        assert_eq!(result, Ok(Some(7)));

        fail.set(true);
        assert!(load(&mut store).await.0.is_err());
        fail.set(false);
        // A failed read leaves the next operation to mount again, which reads
        // every metadata pair: at least one per profile directory.
        let (result, remounted) = load(&mut store).await;
        assert_eq!(result, Ok(Some(7)));
        assert!(remounted > 3 * mounted, "{remounted} and {mounted} reads");
        let (result, again) = load(&mut store).await;
        assert_eq!(result, Ok(Some(7)));
        assert!(again <= mounted, "{again} and {mounted} reads");

        // A failed write is resolved against a fresh mount, which keeps the
        // saved record, and the next free-space query counts again.
        let free = store.available().await.unwrap();
        fail_writes.set(true);
        reads.set(0);
        assert!(store.save(record_key(8, 20), b"changed").await.is_err());
        assert!(
            reads.get() > 3 * mounted,
            "{} and {mounted} reads",
            reads.get()
        );
        fail_writes.set(false);
        let mut bytes = [0; 16];
        assert_eq!(store.load(record_key(8, 20), &mut bytes).await, Ok(Some(7)));
        assert_eq!(&bytes[..7], b"profile");
        reads.set(0);
        assert_eq!(store.available().await.unwrap(), free);
        assert!(reads.get() > 0);

        // Free space is counted once until the next write.
        reads.set(0);
        assert_eq!(store.available().await.unwrap(), free);
        assert_eq!(reads.get(), 0);
        store.save(record_key(8, 20), b"changed").await.unwrap();
        reads.set(0);
        store.available().await.unwrap();
        assert!(reads.get() > 0);
    });
}

#[test]
fn free_space_follows_writes_and_loads_check_the_buffer() {
    use cordial_core::storage::{Error, RecordStore, record_key};
    block_on(async {
        let mut store = Storage::provision_blank(Flash::blank(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let empty = store.available().await.unwrap();
        store.save(record_key(8, 1), &[b'x'; 9000]).await.unwrap();
        let used = store.available().await.unwrap();
        assert!(used < empty);
        assert_eq!(store.available().await.unwrap(), used);
        let mut bytes = [0; 8999];
        assert_eq!(
            store.load(record_key(8, 1), &mut bytes).await,
            Err(Error::TooLarge)
        );
        store.remove(record_key(8, 1)).await.unwrap();
        assert!(store.available().await.unwrap() > used);
        store
            .replace_file("/profiles/2/profile.pb/x", b"x")
            .unwrap();
        assert_eq!(
            store.load(record_key(8, 2), &mut bytes).await,
            Err(Error::Corrupt)
        );
        assert_eq!(
            store.load_owned(record_key(8, 2)).await,
            Err(Error::Corrupt)
        );
    });
}

#[test]
fn owned_loads_allocate_the_record_length_once() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let mut store = Storage::provision_blank(Flash::blank(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        for n in [1, 511, 512, 513, 9000] {
            let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            store.save(record_key(9, 1), &data).await.unwrap();
            let loaded = store.load_owned(record_key(9, 1)).await.unwrap().unwrap();
            assert_eq!(loaded, data);
            assert_eq!(loaded.capacity(), n);
        }
        assert_eq!(store.load_owned(record_key(9, 2)).await.unwrap(), None);
    });
}

/// Flash whose writes and erases fail, so every removal fails.
struct RejectedWrites(Flash);
impl ErrorType for RejectedWrites {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for RejectedWrites {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.0.capacity()
    }
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes).await
    }
}
impl NorFlash for RejectedWrites {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn write(&mut self, _: u32, _: &[u8]) -> Result<(), Self::Error> {
        Err(NorFlashErrorKind::Other)
    }
    async fn erase(&mut self, _: u32, _: u32) -> Result<(), Self::Error> {
        Err(NorFlashErrorKind::Other)
    }
}

#[test]
fn mounting_opens_storage_when_cleanup_cannot_remove_files() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let mut store = Storage::provision_blank(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        store.save(record_key(2, 1), b"device").await.unwrap();
        store
            .replace_file("/devices/2/layout.pb", b"orphan")
            .unwrap();
        drop(store);
        let baseline = flash.0.borrow().clone();
        let mut store = cordial_record_storage::Storage::<_>::open(
            RejectedWrites(flash.clone()),
            range.clone(),
            [1; 32],
        )
        .await
        .unwrap();
        assert_eq!(*flash.0.borrow(), baseline);
        // The directory without a record holds no record, and saved records still read.
        assert_eq!(store.load_owned(record_key(2, 2)).await.unwrap(), None);
        assert_eq!(store.keys().await.unwrap(), [record_key(2, 1)]);
        assert_eq!(
            store.load_owned(record_key(2, 1)).await.unwrap(),
            Some(b"device".to_vec())
        );
        drop(store);
        // A later mount that can write cleans up.
        let mut store = Storage::open(flash, range, [1; 32]).await.unwrap();
        assert!(store.entry("/devices", 1).unwrap().is_none());
    });
}

#[test]
fn startup_preserves_valid_roots_and_recovers_a_mismatched_epoch() {
    use cordial_core::identity::Identity;
    use prost::Message;
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let mut store = Storage::open_or_recover(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        let identity = Identity::initialize(&mut store, [1, 2, 3, 4, 5, 6], || 9)
            .await
            .unwrap();
        let mut bytes = [0; 512];
        let n = store.read_file("/format.pb", 0, &mut bytes).unwrap();
        assert_eq!(
            cordial_protocol::storage::Format::decode(&bytes[..n]).unwrap(),
            cordial_protocol::storage::Format {
                format: 1,
                initialized: true
            }
        );
        let n = store.read_file("/sequence.pb", 0, &mut bytes).unwrap();
        assert_eq!(
            cordial_protocol::storage::Sequence::decode(&bytes[..n]).unwrap(),
            cordial_protocol::storage::Sequence {
                next_device: 1,
                next_profile: 1
            }
        );
        let mut store = Storage::open_or_recover(store.into_flash(), range.clone(), [1; 32])
            .await
            .unwrap();
        assert_eq!(
            Identity::initialize(&mut store, [1, 2, 3, 4, 5, 6], || 10).await,
            Ok(identity)
        );
        // Another radio address is refused without changing the identity.
        assert_eq!(
            Identity::initialize(&mut store, [6; 6], || 10).await,
            Err(Error::Layout)
        );
        drop(store);
        let mut store = Storage::open_or_recover(flash.clone(), range.clone(), [2; 32])
            .await
            .unwrap();
        assert!(Identity::load(&mut store).await.unwrap().is_some());
        store.replace_file("/format.pb", &[8, 2, 16, 1]).unwrap();
        drop(store);
        let mut store = Storage::open_or_recover(flash, range, [2; 32])
            .await
            .unwrap();
        assert!(Identity::load(&mut store).await.unwrap().is_none());
        assert!(
            !cordial_core::storage::format(&mut store)
                .await
                .unwrap()
                .initialized
        );
    });
}

/// Flash whose first read of some blocks fails.
struct UnreadableOnce {
    flash: Flash,
    blocks: Vec<usize>,
}
impl ErrorType for UnreadableOnce {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for UnreadableOnce {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let block = offset as usize / 4096;
        if let Some(index) = self.blocks.iter().position(|b| *b == block) {
            self.blocks.remove(index);
            return Err(NorFlashErrorKind::Other);
        }
        self.flash.read(offset, bytes).await
    }
}
impl NorFlash for UnreadableOnce {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn write(&mut self, off: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.flash.write(off, bytes).await
    }
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.flash.erase(from, to).await
    }
}

#[test]
fn a_record_whose_old_contents_cannot_be_read_is_written_over() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let mut store = Storage::provision_blank(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        let key = record_key(2, 1);
        store.save(key, &[0x5a; 6000]).await.unwrap();
        drop(store);
        // The blocks holding the old contents fail when the save first reads them.
        let blocks: Vec<usize> = flash
            .0
            .borrow()
            .chunks(4096)
            .enumerate()
            .filter(|(_, block)| block.windows(512).any(|w| w.iter().all(|b| *b == 0x5a)))
            .map(|(index, _)| index)
            .collect();
        assert!(!blocks.is_empty());
        let mut store = cordial_record_storage::Storage::<_>::open(
            UnreadableOnce {
                flash: flash.clone(),
                blocks,
            },
            range.clone(),
            [1; 32],
        )
        .await
        .unwrap();
        store.save(key, &[0x27; 6000]).await.unwrap();
        drop(store);
        let mut store = Storage::open(flash, range, [1; 32]).await.unwrap();
        assert_eq!(store.load_owned(key).await.unwrap(), Some(vec![0x27; 6000]));
    });
}

#[test]
fn a_failed_write_over_unreadable_contents_is_a_definite_failure() {
    use cordial_core::storage::{RecordStore, record_key};
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let mut store = Storage::provision_blank(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        let key = record_key(2, 1);
        store.save(key, &[0x5a; 6000]).await.unwrap();
        drop(store);
        // The blocks holding the old contents can never be read, so writing a new copy fails.
        let blocks: Vec<usize> = flash
            .0
            .borrow()
            .chunks(4096)
            .enumerate()
            .filter(|(_, block)| block.windows(512).any(|w| w.iter().all(|b| *b == 0x5a)))
            .map(|(index, _)| index)
            .collect();
        let mut store = cordial_record_storage::Storage::<_>::open(
            UnreadableBlocks {
                flash: flash.clone(),
                blocks,
            },
            range.clone(),
            [1; 32],
        )
        .await
        .unwrap();
        assert_eq!(store.save(key, &[0x27; 6000]).await, Err(Error::Io));
        drop(store);
        let mut store = Storage::open(flash, range, [1; 32]).await.unwrap();
        assert_eq!(store.load_owned(key).await.unwrap(), Some(vec![0x5a; 6000]));
    });
}

/// Flash whose reads of some blocks always fail.
struct UnreadableBlocks {
    flash: Flash,
    blocks: Vec<usize>,
}
impl ErrorType for UnreadableBlocks {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for UnreadableBlocks {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        if self.blocks.contains(&(offset as usize / 4096)) {
            return Err(NorFlashErrorKind::Other);
        }
        self.flash.read(offset, bytes).await
    }
}
impl NorFlash for UnreadableBlocks {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn write(&mut self, off: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.flash.write(off, bytes).await
    }
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.flash.erase(from, to).await
    }
}

#[test]
fn device_output_preferences_share_the_existing_record_and_layout() {
    use cordial_core::{
        bonds::{Bond, Keys},
        devices::{Peer, Policies, Policy},
        model::identifiers::Transport,
        outputs::{self, Preference},
        storage::{RecordStore, record_key},
    };
    use cordial_protocol::{OutputMode, Role};
    use prost::Message;
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let layout = [1; 32];
        let mut store = Storage::provision_blank(flash.clone(), range.clone(), layout)
            .await
            .unwrap();
        let peer = Peer {
            address: [1; 6],
            random: false,
            transport: Transport::Classic,
        };
        let mut policy = Policy::paired(1, peer, b"Gamepad");
        policy.bond = 1;
        let bond = Bond {
            owner: 1,
            identity: peer,
            complete: true,
            keys: Keys::Classic {
                key: [42; 16],
                kind: 4,
            },
        };
        cordial_core::bonds::commit(&mut store, &policy, &bond)
            .await
            .unwrap();
        let legacy = store.load_owned(record_key(2, 1)).await.unwrap().unwrap();
        drop(store);
        let mut store = Storage::open(flash.clone(), range.clone(), layout)
            .await
            .unwrap();
        let old = Policies { store: &mut store }.load(1).await.unwrap();
        assert!(old.output_preferences.is_empty());
        assert_eq!(
            outputs::gamepad(&old.output_preferences),
            OutputMode::Disabled
        );
        Policies { store: &mut store }.save(&old).await.unwrap();
        assert_eq!(
            store.load_owned(record_key(2, 1)).await.unwrap().unwrap(),
            legacy
        );
        policy.output_preferences.push(Preference {
            role: Role::Gamepad,
            mode: OutputMode::Xinput,
        });
        Policies { store: &mut store }.save(&policy).await.unwrap();
        drop(store);
        let mut store = Storage::open(flash, range, layout).await.unwrap();
        let restored = Policies { store: &mut store }.load(1).await.unwrap();
        assert_eq!(restored, policy);
        assert_eq!(store.keys().await.unwrap(), [record_key(2, 1)]);
        let bytes = store.load_owned(record_key(2, 1)).await.unwrap().unwrap();
        let saved = cordial_protocol::storage::Device::decode(bytes.as_slice()).unwrap();
        assert_eq!(saved.policy.unwrap().output_preferences.len(), 1);
    });
}

#[test]
fn physical_capacity_uses_stable_reservations_and_sector_boundaries() {
    use cordial_record_storage::flash_range;
    for mib in [2, 3, 4, 6, 8, 12, 16, 32, 64] {
        let capacity = mib * 1024 * 1024;
        let start = if mib < 4 {
            1024 * 1024
        } else {
            2 * 1024 * 1024
        };
        assert_eq!(flash_range(capacity), Ok(start..capacity));
    }
    for capacity in [0, 1024 * 1024, 4194305, u32::MAX] {
        assert_eq!(flash_range(capacity), Err(Error::Bounds));
    }
}

#[test]
fn compatible_filesystem_grows_and_preserves_roots_and_records() {
    use cordial_core::{identity::Identity, storage::RecordStore};
    block_on(async {
        let flash = Flash::sized(512 * 1024);
        let mut fs = Storage::open_or_recover(flash.clone(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let identity = Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
        fs.replace_file("/saved.bin", &[42; 9000]).unwrap();
        drop(fs);
        // A stale or torn guard has no authority over a valid filesystem.
        flash.0.borrow_mut()[4096..4128].fill(0);
        let mut fs = Storage::open_or_recover(flash.clone(), 4096..512 * 1024, [2; 32])
            .await
            .unwrap();
        assert_eq!(fs.total_bytes().unwrap(), 126 * 4096);
        assert_eq!(RecordStore::total_bytes(&mut fs).await.unwrap(), 126 * 4096);
        assert_eq!(
            Identity::load(&mut fs).await.unwrap(),
            Some(identity.clone())
        );
        assert_eq!(fs.file_size("/saved.bin").unwrap(), Some(9000));
        let free = fs.available().await.unwrap();
        assert!(free < fs.total_bytes().unwrap());
        drop(fs);
        let before = flash.0.borrow().clone();
        let mut fs = Storage::open_or_recover(flash.clone(), 4096..512 * 1024, [2; 32])
            .await
            .unwrap();
        assert_eq!(Identity::load(&mut fs).await.unwrap(), Some(identity));
        assert_eq!(*flash.0.borrow(), before);
    });
}

#[test]
fn growth_uses_current_geometry_and_leaves_smaller_or_missing_measurements_alone() {
    use cordial_core::{
        identity::Identity,
        storage::{RecordStore, record_key},
    };
    block_on(async {
        let flash = Flash::sized(1024 * 1024);
        let range = 4096..1024 * 1024;
        let mut fs = Storage::open_or_recover(flash.clone(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let identity = Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
        fs.save(record_key(2, 1), &[42; 9000]).await.unwrap();
        drop(fs);
        for (step, end) in [
            Some(512 * 1024),
            None,
            Some(128 * 1024),
            Some(512 * 1024),
            Some(64 * 1024),
            Some(1024 * 1024),
        ]
        .into_iter()
        .enumerate()
        {
            let before = flash.0.borrow().clone();
            let mut fs = Storage::open_with_capacity(flash.clone(), range.clone(), end, [1; 32])
                .await
                .unwrap();
            let expected_end = if end == Some(1024 * 1024) {
                1024 * 1024
            } else {
                512 * 1024
            };
            assert_eq!(fs.total_bytes().unwrap(), expected_end - 8192);
            assert_eq!(
                Identity::load(&mut fs).await.unwrap(),
                Some(identity.clone())
            );
            assert_eq!(
                fs.load_owned(record_key(2, 1)).await.unwrap(),
                Some(vec![42; 9000])
            );
            if (1..5).contains(&step) {
                assert_eq!(*flash.0.borrow(), before);
            }
        }
    });
}

#[test]
fn inaccessible_existing_blocks_never_trigger_formatting() {
    block_on(async {
        let flash = Flash::sized(512 * 1024);
        let mut fs = Storage::open_or_recover(flash.clone(), 4096..512 * 1024, [1; 32])
            .await
            .unwrap();
        fs.replace_file("/saved.bin", &[42; 128 * 1024]).unwrap();
        drop(fs);
        let before = flash.0.borrow().clone();
        let result =
            Storage::open_with_capacity(flash.clone(), 4096..128 * 1024, None, [1; 32]).await;
        match result {
            Ok(mut fs) => {
                // Metadata can fit below the limit even when file data does not.
                let mut bytes = vec![0; 128 * 1024];
                assert_eq!(fs.read_file("/saved.bin", 0, &mut bytes), Err(Error::Io));
            }
            Err(e) => assert_eq!(e, Error::Io),
        }
        assert_eq!(*flash.0.borrow(), before);
        let mut fs = Storage::open_or_recover(flash, 4096..512 * 1024, [1; 32])
            .await
            .unwrap();
        let mut bytes = vec![0; 128 * 1024];
        assert_eq!(
            fs.read_file("/saved.bin", 0, &mut bytes).unwrap(),
            bytes.len()
        );
        assert!(bytes.iter().all(|b| *b == 42));
    });
}

#[test]
fn invalid_epochs_and_essential_roots_reset_the_whole_filesystem() {
    use cordial_core::identity::Identity;
    block_on(async {
        for root in ["/format.pb", "/identity.pb", "/sequence.pb"] {
            let invalid_value = if root == "/sequence.pb" {
                &[8, 0, 16, 1]
            } else {
                &[8, 2, 16, 1]
            };
            for bytes in [
                None,
                Some(&b"invalid"[..]),
                Some(&invalid_value[..]),
                Some(&[8, 0, 16, 1][..]),
                Some(&[8, 1][..]),
            ] {
                let flash = Flash::blank();
                let range = 4096..128 * 1024;
                let mut fs = Storage::open_or_recover(flash.clone(), range.clone(), [1; 32])
                    .await
                    .unwrap();
                Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
                fs.replace_file("/devices/1/device.pb", b"saved bond")
                    .unwrap();
                if let Some(bytes) = bytes {
                    fs.replace_file(root, bytes).unwrap();
                } else {
                    fs.remove_file(root).unwrap();
                }
                drop(fs);
                let mut fs = Storage::open_or_recover(flash, range, [1; 32])
                    .await
                    .unwrap();
                assert!(Identity::load(&mut fs).await.unwrap().is_none(), "{root}");
                assert_eq!(fs.file_size("/devices/1/device.pb").unwrap(), None);
            }
        }
    });
}

#[test]
fn startup_io_failure_preserves_the_flash_and_can_be_retried() {
    use cordial_core::identity::Identity;
    block_on(async {
        for initialized in [false, true] {
            let flash = Flash::blank();
            let range = 4096..128 * 1024;
            if initialized {
                let mut fs = Storage::open_or_recover(flash.clone(), range.clone(), [1; 32])
                    .await
                    .unwrap();
                Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
            }
            let before = flash.0.borrow().clone();
            let driver = UnreadableOnce {
                flash: flash.clone(),
                blocks: vec![2],
            };
            assert!(matches!(
                cordial_record_storage::Storage::open_or_recover(driver, range.clone(), [1; 32])
                    .await,
                Err(Error::Io)
            ));
            assert_eq!(*flash.0.borrow(), before);
            let mut fs = Storage::open_or_recover(flash, range, [1; 32])
                .await
                .unwrap();
            assert_eq!(
                Identity::load(&mut fs).await.unwrap().is_some(),
                initialized
            );
        }
    });
}

#[test]
fn interrupted_growth_and_guard_repair_preserve_committed_records() {
    use cordial_core::identity::Identity;
    block_on(async {
        let flash = Flash::sized(512 * 1024);
        let mut fs = Storage::open_or_recover(flash.clone(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let identity = Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
        fs.replace_file("/saved.bin", &[42; 9000]).unwrap();
        drop(fs);
        let baseline = flash.0.borrow().clone();
        let mut completed = false;
        for cut in 0..80 {
            *flash.0.borrow_mut() = baseline.clone();
            let driver = CutFlash {
                flash: flash.clone(),
                remaining: cut,
                cut: false,
            };
            let result =
                cordial_record_storage::Storage::open_or_recover(driver, 4096..512 * 1024, [2; 32])
                    .await;
            let mut fs = Storage::open_or_recover(flash.clone(), 4096..512 * 1024, [2; 32])
                .await
                .unwrap();
            assert_eq!(
                Identity::load(&mut fs).await.unwrap(),
                Some(identity.clone()),
                "cut {cut}"
            );
            let mut bytes = [0; 9000];
            assert_eq!(
                fs.read_file("/saved.bin", 0, &mut bytes).unwrap(),
                bytes.len()
            );
            assert_eq!(bytes, [42; 9000]);
            assert_eq!(fs.total_bytes().unwrap(), 126 * 4096);
            if result.is_ok() {
                completed = true;
                break;
            }
        }
        assert!(completed);
    });
}

#[test]
fn interrupted_first_identity_initialization_resumes_safely() {
    use cordial_core::{identity::Identity, storage::validate_roots};
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        let fs = Storage::open_or_recover(flash.clone(), range.clone(), [1; 32])
            .await
            .unwrap();
        drop(fs);
        let baseline = flash.0.borrow().clone();
        let mut completed = false;
        for cut in 0..80 {
            *flash.0.borrow_mut() = baseline.clone();
            let driver = CutFlash {
                flash: flash.clone(),
                remaining: cut,
                cut: false,
            };
            let mut fs =
                cordial_record_storage::Storage::open_or_recover(driver, range.clone(), [1; 32])
                    .await
                    .unwrap();
            let result = Identity::initialize(&mut fs, [1; 6], || 7).await;
            drop(fs);
            let mut fs = Storage::open_or_recover(flash.clone(), range.clone(), [1; 32])
                .await
                .unwrap();
            Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
            validate_roots(&mut fs).await.unwrap();
            if result.is_ok() {
                completed = true;
                break;
            }
        }
        assert!(completed);
    });
}

#[test]
fn moved_filesystem_start_resets_bonds_within_the_current_owned_range() {
    use cordial_core::identity::Identity;
    use cordial_record_storage::flash_range;
    block_on(async {
        let flash = Flash::sized(4 * 1024 * 1024);
        let mut fs = Storage::open_or_recover(flash.clone(), 1024 * 1024..4 * 1024 * 1024, [1; 32])
            .await
            .unwrap();
        Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
        fs.replace_file("/devices/1/device.pb", b"saved bond")
            .unwrap();
        drop(fs);
        let firmware = flash.0.borrow()[..2 * 1024 * 1024].to_vec();
        let mut fs = Storage::open_or_recover(
            flash.clone(),
            flash_range(4 * 1024 * 1024).unwrap(),
            [1; 32],
        )
        .await
        .unwrap();
        assert_eq!(Identity::load(&mut fs).await.unwrap(), None);
        assert_eq!(fs.file_size("/devices/1/device.pb").unwrap(), None);
        assert_eq!(fs.total_bytes().unwrap(), 2 * 1024 * 1024 - 4096);
        assert_eq!(&flash.0.borrow()[..2 * 1024 * 1024], firmware);
    });
}

#[test]
fn pico_cold_boot_retains_geometry_records_and_current_reservation() {
    use cordial_core::identity::Identity;
    const MIB: u32 = 1024 * 1024;
    block_on(async {
        for minimum in [2 * MIB, 4 * MIB] {
            let flash = Flash::sized(16 * MIB as usize);
            let mut fs = Storage::open_detected(flash.clone(), minimum, Some(8 * MIB), [1; 32])
                .await
                .unwrap();
            let identity = Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
            fs.replace_file("/saved.bin", &vec![42; 3 * MIB as usize])
                .unwrap();
            drop(fs);
            for measured in [None, Some(MIB), Some(2 * MIB), Some(4 * MIB), Some(8 * MIB)] {
                let before = flash.0.borrow().clone();
                let mut fs = Storage::open_detected(flash.clone(), minimum, measured, [1; 32])
                    .await
                    .unwrap();
                assert_eq!(fs.total_bytes().unwrap(), 6 * MIB as usize - 4096);
                assert_eq!(
                    Identity::load(&mut fs).await.unwrap(),
                    Some(identity.clone())
                );
                let mut bytes = vec![0; 3 * MIB as usize];
                assert_eq!(
                    fs.read_file("/saved.bin", 0, &mut bytes).unwrap(),
                    bytes.len()
                );
                assert!(bytes.iter().all(|b| *b == 42));
                drop(fs);
                assert_eq!(*flash.0.borrow(), before);
            }
            let mut fs = Storage::open_detected(flash.clone(), minimum, Some(16 * MIB), [1; 32])
                .await
                .unwrap();
            assert_eq!(fs.total_bytes().unwrap(), 14 * MIB as usize - 4096);
            assert_eq!(Identity::load(&mut fs).await.unwrap(), Some(identity));
        }
    });
}

#[test]
fn pico_fresh_format_uses_only_the_minimum_region_without_detection() {
    const MIB: u32 = 1024 * 1024;
    block_on(async {
        for minimum in [2 * MIB, 4 * MIB] {
            let flash = Flash::sized(16 * MIB as usize);
            flash.0.borrow_mut()[minimum as usize..].fill(0x42);
            let mut fs = Storage::open_detected(flash.clone(), minimum, None, [1; 32])
                .await
                .unwrap();
            let start = cordial_record_storage::flash_range(minimum).unwrap().start;
            assert_eq!(fs.total_bytes().unwrap(), (minimum - start) as usize - 4096);
            assert!(
                flash.0.borrow()[minimum as usize..]
                    .iter()
                    .all(|b| *b == 0x42)
            );
        }
    });
}

#[test]
fn pico_root_recovery_preserves_geometry_and_resets_only_once() {
    use cordial_core::{
        identity::Identity,
        storage::{RecordStore, record_key},
    };
    const MIB: u32 = 1024 * 1024;
    block_on(async {
        for minimum in [2 * MIB, 4 * MIB] {
            for root in ["/format.pb", "/identity.pb", "/sequence.pb"] {
                for measured in [None, Some(MIB), Some(2 * MIB)] {
                    let flash = Flash::sized(16 * MIB as usize);
                    let mut fs =
                        Storage::open_detected(flash.clone(), minimum, Some(8 * MIB), [1; 32])
                            .await
                            .unwrap();
                    Identity::initialize(&mut fs, [1; 6], || 7).await.unwrap();
                    fs.save(record_key(2, 1), &[42; 100]).await.unwrap();
                    if root == "/format.pb" {
                        fs.replace_file(root, &[8, 99]).unwrap();
                    } else {
                        fs.remove_file(root).unwrap();
                    }
                    drop(fs);
                    let before = flash.0.borrow().clone();
                    if measured.is_some_and(|end| end < minimum) {
                        let mutations = Rc::new(std::cell::Cell::new(0));
                        let result = cordial_record_storage::Storage::open_detected(
                            WrappingReadFlash {
                                flash: flash.clone(),
                                mutations: mutations.clone(),
                            },
                            minimum,
                            measured,
                            [1; 32],
                        )
                        .await;
                        assert!(matches!(result, Err(Error::Bounds)));
                        assert_eq!(mutations.get(), 0);
                        assert_eq!(*flash.0.borrow(), before);
                    }
                    // A missing probe permits recovery within the checked geometry.
                    let measured = measured.filter(|end| *end >= minimum);
                    let mut fs = Storage::open_detected(flash.clone(), minimum, measured, [1; 32])
                        .await
                        .unwrap();
                    assert_eq!(fs.total_bytes().unwrap(), 6 * MIB as usize - 4096);
                    assert!(Identity::load(&mut fs).await.unwrap().is_none());
                    assert!(fs.load_owned(record_key(2, 1)).await.unwrap().is_none());
                    assert_eq!(
                        flash.0.borrow()[..2 * MIB as usize],
                        before[..2 * MIB as usize]
                    );
                    assert_eq!(
                        flash.0.borrow()[8 * MIB as usize..],
                        before[8 * MIB as usize..]
                    );
                    let identity = Identity::initialize(&mut fs, [2; 6], || 8).await.unwrap();
                    fs.save(record_key(2, 1), &[7; 9000]).await.unwrap();
                    fs.save(record_key(4, 1), &[8; 100]).await.unwrap();
                    drop(fs);
                    let before = flash.0.borrow().clone();
                    for _ in 0..3 {
                        let mut fs = Storage::open_detected(flash.clone(), minimum, None, [1; 32])
                            .await
                            .unwrap();
                        assert_eq!(fs.total_bytes().unwrap(), 6 * MIB as usize - 4096);
                        assert_eq!(
                            Identity::load(&mut fs).await.unwrap(),
                            Some(identity.clone())
                        );
                        assert_eq!(
                            fs.load_owned(record_key(2, 1)).await.unwrap(),
                            Some(vec![7; 9000])
                        );
                        assert_eq!(
                            fs.load_owned(record_key(4, 1)).await.unwrap(),
                            Some(vec![8; 100])
                        );
                        drop(fs);
                        assert_eq!(*flash.0.borrow(), before);
                    }
                }
            }
        }
    });
}

/// Models address aliasing on an undersized chip and forbids every mutation.
struct WrappingReadFlash {
    flash: Flash,
    mutations: Rc<std::cell::Cell<usize>>,
}
impl ErrorType for WrappingReadFlash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for WrappingReadFlash {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        16 * 1024 * 1024
    }
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let data = self.flash.0.borrow();
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = data[(offset as usize + i) % data.len()];
        }
        Ok(())
    }
}
impl NorFlash for WrappingReadFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn write(&mut self, _: u32, _: &[u8]) -> Result<(), Self::Error> {
        self.mutations.set(self.mutations.get() + 1);
        Err(NorFlashErrorKind::Other)
    }
    async fn erase(&mut self, _: u32, _: u32) -> Result<(), Self::Error> {
        self.mutations.set(self.mutations.get() + 1);
        Err(NorFlashErrorKind::Other)
    }
}

#[test]
fn pico_detected_undersized_chips_never_format_even_when_addresses_wrap() {
    const MIB: u32 = 1024 * 1024;
    block_on(async {
        for (minimum, physical) in [(2 * MIB, MIB), (4 * MIB, MIB), (4 * MIB, 2 * MIB)] {
            for fill in [0xff, 0x42] {
                let flash = Flash::sized(physical as usize);
                flash.0.borrow_mut().fill(fill);
                flash.0.borrow_mut()[..64 * 1024].fill(0x24);
                let before = flash.0.borrow().clone();
                let mutations = Rc::new(std::cell::Cell::new(0));
                let result = cordial_record_storage::Storage::open_detected(
                    WrappingReadFlash {
                        flash: flash.clone(),
                        mutations: mutations.clone(),
                    },
                    minimum,
                    Some(physical),
                    [1; 32],
                )
                .await;
                assert_eq!(mutations.get(), 0);
                assert!(matches!(result, Err(Error::Bounds)));
                assert_eq!(*flash.0.borrow(), before);
            }
        }
    });
}

#[test]
fn pico_current_two_mib_filesystem_wins_over_stale_one_mib_filesystem() {
    const MIB: u32 = 1024 * 1024;
    block_on(async {
        let flash = Flash::sized(16 * MIB as usize);
        let mut fs = Storage::open_or_recover(flash.clone(), MIB..2 * MIB, [1; 32])
            .await
            .unwrap();
        fs.replace_file("/old.bin", &[1; 100]).unwrap();
        drop(fs);
        let old = flash.0.borrow()[MIB as usize..2 * MIB as usize].to_vec();
        let mut fs = Storage::open_detected(flash.clone(), 2 * MIB, Some(8 * MIB), [1; 32])
            .await
            .unwrap();
        assert_eq!(fs.file_size("/old.bin").unwrap(), None);
        assert_eq!(flash.0.borrow()[MIB as usize..2 * MIB as usize], old);
        fs.replace_file("/current.bin", &[2; 100]).unwrap();
        drop(fs);
        for measured in [None, Some(2 * MIB), Some(4 * MIB)] {
            let before = flash.0.borrow().clone();
            let mut fs = Storage::open_detected(flash.clone(), 2 * MIB, measured, [1; 32])
                .await
                .unwrap();
            assert_eq!(fs.total_bytes().unwrap(), 6 * MIB as usize - 4096);
            assert_eq!(fs.file_size("/current.bin").unwrap(), Some(100));
            assert_eq!(fs.file_size("/old.bin").unwrap(), None);
            assert_eq!(*flash.0.borrow(), before);
        }
    });
}

#[test]
fn pico_one_mib_current_layout_survives_missing_smaller_and_equal_detection() {
    const MIB: u32 = 1024 * 1024;
    block_on(async {
        let flash = Flash::sized(16 * MIB as usize);
        let mut fs = Storage::open_detected(flash.clone(), 2 * MIB, Some(2 * MIB), [1; 32])
            .await
            .unwrap();
        fs.replace_file("/saved.bin", &[42; 9000]).unwrap();
        drop(fs);
        for measured in [None, Some(MIB), Some(2 * MIB)] {
            let before = flash.0.borrow().clone();
            let mut fs = Storage::open_detected(flash.clone(), 2 * MIB, measured, [1; 32])
                .await
                .unwrap();
            assert_eq!(fs.total_bytes().unwrap(), MIB as usize - 4096);
            assert_eq!(fs.file_size("/saved.bin").unwrap(), Some(9000));
            assert_eq!(*flash.0.borrow(), before);
        }
    });
}

#[test]
fn pico_discovery_reuses_the_selected_mount_for_validation_and_growth() {
    const MIB: u32 = 1024 * 1024;
    block_on(async {
        let flash = Flash::sized(16 * MIB as usize);
        let fs = Storage::open_detected(flash.clone(), 4 * MIB, Some(4 * MIB), [1; 32])
            .await
            .unwrap();
        drop(fs);
        let before = flash.0.borrow().clone();
        for measured in [None, Some(4 * MIB), Some(8 * MIB)] {
            let mut counts = Vec::new();
            for discovery in [false, true] {
                flash.0.borrow_mut().copy_from_slice(&before);
                let reads = Rc::new(std::cell::Cell::new(0));
                let driver = ProbeFlash {
                    flash: flash.clone(),
                    reads: reads.clone(),
                    fail: Rc::new(std::cell::Cell::new(false)),
                    fail_writes: Rc::new(std::cell::Cell::new(measured != Some(8 * MIB))),
                };
                let mut fs = if discovery {
                    cordial_record_storage::Storage::open_detected(
                        driver,
                        4 * MIB,
                        measured,
                        [1; 32],
                    )
                    .await
                    .unwrap()
                } else {
                    cordial_record_storage::Storage::open_with_limits(
                        driver,
                        2 * MIB..measured.unwrap_or(4 * MIB),
                        Some(measured.unwrap_or(4 * MIB)),
                        measured,
                        [1; 32],
                    )
                    .await
                    .unwrap()
                };
                assert_eq!(
                    fs.total_bytes().unwrap(),
                    (measured.unwrap_or(4 * MIB) - 2 * MIB) as usize - 4096
                );
                counts.push(reads.get());
            }
            // Selecting geometry adds no reads to the ordinary mounted path.
            assert_eq!(counts[0], counts[1]);
        }
    });
}

#[test]
fn pico_geometry_must_fit_addressability_and_io_failures_do_not_format() {
    const MIB: u32 = 1024 * 1024;
    block_on(async {
        let flash = Flash::sized(16 * MIB as usize);
        let fs = Storage::provision_blank(flash.clone(), 2 * MIB..16 * MIB, [1; 32])
            .await
            .unwrap();
        drop(fs);
        // The superblock has a valid CRC but its absolute end exceeds the driver.
        flash.0.borrow_mut().truncate(8 * MIB as usize);
        let before = flash.0.borrow().clone();
        let mut fs = Storage::open_detected(flash.clone(), 4 * MIB, None, [1; 32])
            .await
            .unwrap();
        assert_eq!(fs.total_bytes().unwrap(), 2 * MIB as usize - 4096);
        assert_eq!(
            flash.0.borrow()[4 * MIB as usize..],
            before[4 * MIB as usize..]
        );
        drop(fs);
        let before = flash.0.borrow().clone();
        let result = cordial_record_storage::Storage::open_detected(
            UnreadableBlocks {
                flash: flash.clone(),
                blocks: vec![2 * MIB as usize / 4096 + 1],
            },
            4 * MIB,
            None,
            [1; 32],
        )
        .await;
        assert!(matches!(result, Err(Error::Io)));
        assert_eq!(*flash.0.borrow(), before);
    });
}
