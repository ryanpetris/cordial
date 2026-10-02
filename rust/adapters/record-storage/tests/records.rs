use cordial_core::storage::Error;
use embassy_futures::block_on;
use embedded_storage_async::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashErrorKind, ReadNorFlash,
};
type Storage = cordial_record_storage::Storage<Flash, 30>;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
struct Flash(Rc<RefCell<Vec<u8>>>);
impl Flash {
    fn blank() -> Self {
        Self(Rc::new(RefCell::new(vec![0xff; 128 * 1024])))
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
fn unknown_or_partly_used_partitions_are_never_provisioned_or_changed() {
    block_on(async {
        let flash = Flash::blank();
        let range = 4096..128 * 1024;
        assert!(matches!(
            Storage::open(flash.clone(), range.clone(), [1; 32]).await,
            Err(Error::Unprovisioned)
        ));
        flash.0.borrow_mut()[128 * 1024 - 1] = 0;
        let before = flash.0.borrow().clone();
        assert!(matches!(
            Storage::open_or_provision_blank(flash.clone(), range.clone(), [1; 32]).await,
            Err(Error::Layout)
        ));
        assert_eq!(*flash.0.borrow(), before);
        flash.0.borrow_mut()[4096] = 7;
        let before = flash.0.borrow().clone();
        assert!(matches!(
            Storage::open_or_provision_blank(flash.clone(), range, [1; 32]).await,
            Err(Error::Layout)
        ));
        assert_eq!(*flash.0.borrow(), before);
    });
}
#[test]
fn files_span_sectors_and_atomic_replacement_preserves_exact_bytes() {
    block_on(async {
        let flash = Flash::blank();
        let mut store = Storage::provision_blank(flash.clone(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let path = "/devices/0000000000000001/device.json";
        let data: Vec<u8> = (0..9000).map(|n| (n % 251) as u8).collect();
        store.replace_file(path, &data).unwrap();
        let generation = store.generation();
        store.replace_file(path, &data).unwrap();
        assert_eq!(store.generation(), generation);
        for n in 0..300 {
            store
                .replace_file("/adapter.json", format!("{{\"counter\":{n}}}").as_bytes())
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
            "0000000000000001"
        );
        assert!(store.entry("/devices", 1).unwrap().is_none());
        store.remove_file(path).unwrap();
        assert_eq!(store.file_size(path).unwrap(), None);
        assert!(store.read_file("/../identity.json", 0, &mut bytes).is_err());
        assert_eq!(&flash.0.borrow()[..4096], &[0xff; 4096]);
    });
}
#[test]
fn preferences_share_one_json_file_and_have_no_512_byte_ceiling() {
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
                .entry("/devices/0000000000000001", 0)
                .unwrap()
                .unwrap()
                .file_name()
                .as_str(),
            "hidpp.json"
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
    block_on(async {
        let flash = Flash::blank();
        let mut fs = Storage::provision_blank(flash.clone(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        let old = vec![13; 7000];
        let new = vec![27; 11000];
        fs.replace_file("/device.json", &old).unwrap();
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
                cordial_record_storage::Storage::<_, 30>::open(driver, 4096..128 * 1024, [1; 32])
                    .await
                    .unwrap();
            let result = fs.replace_file("/device.json", &new);
            let mut reopened = Storage::open(flash, 4096..128 * 1024, [1; 32])
                .await
                .unwrap();
            let mut data = vec![0; 12000];
            let n = reopened.read_file("/device.json", 0, &mut data).unwrap();
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
fn matching_guard_with_broken_filesystem_is_never_reformatted() {
    block_on(async {
        let flash = Flash::blank();
        flash.0.borrow_mut()[4096..4128].fill(1);
        let before = flash.0.borrow().clone();
        assert!(
            Storage::open_or_provision_blank(flash.clone(), 4096..128 * 1024, [1; 32])
                .await
                .is_err()
        );
        assert_eq!(*flash.0.borrow(), before);
    });
}

#[test]
fn interrupted_provisioning_never_reformats_a_claimed_partition() {
    block_on(async {
        let mut completed = false;
        for cut in 0..40 {
            let flash = Flash::blank();
            let driver = CutFlash {
                flash: flash.clone(),
                remaining: cut,
                cut: false,
            };
            let result = cordial_record_storage::Storage::<_, 30>::provision_blank(
                driver,
                4096..128 * 1024,
                [1; 32],
            )
            .await;
            let claimed = flash.0.borrow()[4096..128 * 1024]
                .iter()
                .any(|b| *b != 0xff);
            let before = flash.0.borrow().clone();
            let reopened =
                Storage::open_or_provision_blank(flash.clone(), 4096..128 * 1024, [1; 32]).await;
            if claimed {
                assert_eq!(*flash.0.borrow(), before, "cut {cut}");
            }
            if result.is_ok() {
                assert!(reopened.is_ok());
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
        let device = "/devices/0000000000000001/device.json";
        fs.replace_file(device, b"device").unwrap();
        fs.replace_file("/devices/0000000000000001/hidpp.json", b"preferences")
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
                cordial_record_storage::Storage::<_, 30>::open(driver, 4096..128 * 1024, [1; 32])
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
    block_on(async {
        let mut fs = Storage::provision_blank(Flash::blank(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        fs.replace_file("/important", &[7; 8000]).unwrap();
        let mut full = false;
        for i in 0..100 {
            if fs.replace_file(&format!("/fill{i}"), &[3; 4096]).is_err() {
                full = true;
                break;
            }
        }
        assert!(full);
        assert!(fs.replace_file("/important", &[8; 120 * 1024]).is_err());
        assert_eq!(fs.file_size("/important.tmp").unwrap(), None);
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
        for name in ["device.json", "hidpp.json", "layout.json"] {
            let path = format!("/devices/0000000000000001/{name}");
            assert_eq!(store.file_size(&path).unwrap(), Some(1), "{name}");
        }
        // Layouts are read by key and never enumerated.
        assert_eq!(
            store.keys().await.unwrap(),
            [record_key(2, 1), record_key(4, 1)]
        );
        // Mounting removes an interrupted replacement and keeps the saved file.
        store
            .replace_file("/devices/0000000000000001/layout.json.tmp", b"partial")
            .unwrap();
        let mut store = Storage::open(store.into_flash(), 4096..128 * 1024, [1; 32])
            .await
            .unwrap();
        assert_eq!(
            store
                .file_size("/devices/0000000000000001/layout.json.tmp")
                .unwrap(),
            None
        );
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
        assert_eq!(
            store
                .file_size("/devices/0000000000000001/layout.json")
                .unwrap(),
            None
        );
    });
}
