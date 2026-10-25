//! Exercise the actual ESP raw adapter without hardware or ESP-IDF calls.
#![allow(non_camel_case_types, non_upper_case_globals)]
extern crate self as esp_idf_sys;
use cordial_core::{identity::Identity, storage::Error};
use embassy_futures::block_on;
use prost::Message;
use std::{
    cell::{Cell, RefCell},
    ffi::{c_char, c_void},
};
mod board {
    pub const MINIMUM_FLASH_BYTES: u32 = 4 * 1024 * 1024;
    pub const STORAGE_START: u32 = 2 * 1024 * 1024;
}
#[path = "../platforms/esp32s3/src/storage.rs"]
mod adapter;
type esp_err_t = i32;
type esp_partition_subtype_t = u32;
const ESP_OK: i32 = 0;
const esp_partition_type_t_ESP_PARTITION_TYPE_DATA: u32 = 1;
struct esp_partition_t {
    address: u32,
    size: u32,
}
thread_local! {
    static FLASH: RefCell<Vec<u8>> = RefCell::new(vec![0xff; 4 * 1024 * 1024]);
    static PARTITIONS: RefCell<Vec<*mut esp_partition_t>> = const { RefCell::new(Vec::new()) };
    static DETECTED: Cell<Option<u32>> = const { Cell::new(None) };
    static CACHED_SIZE: Cell<u32> = const { Cell::new(0) };
    static DRIVER_SIZE: Cell<u32> = const { Cell::new(0) };
    static MUTATIONS: Cell<usize> = const { Cell::new(0) };
}
mod platform {
    pub unsafe fn cordial_esp_storage_capacity(
        minimum: u32,
        capacity: *mut u32,
        addressable: *mut u32,
    ) -> i32 {
        assert_eq!(minimum, super::board::MINIMUM_FLASH_BYTES);
        let detected = super::DETECTED
            .get()
            .unwrap_or_else(|| super::FLASH.with_borrow(|f| f.len() as u32));
        let limit = super::DRIVER_SIZE
            .get()
            .max(super::CACHED_SIZE.get())
            .max(detected)
            .max(minimum);
        super::DRIVER_SIZE.set(limit);
        unsafe {
            *capacity = detected;
            *addressable = limit;
        }
        0
    }
}
unsafe fn esp_partition_register_external(
    chip: *mut c_void,
    offset: usize,
    size: usize,
    label: *const c_char,
    kind: u32,
    subtype: u32,
    output: *mut *const esp_partition_t,
) -> i32 {
    assert!(chip.is_null());
    assert_eq!(kind, 1);
    assert!(matches!(subtype, 0x40 | 0x83));
    assert!(!label.is_null());
    assert!(offset + size <= DRIVER_SIZE.get() as usize);
    FLASH.with_borrow(|f| assert!(offset + size <= f.len()));
    PARTITIONS.with_borrow_mut(|partitions| {
        if offset < board::STORAGE_START as usize
            || partitions.iter().any(|p| {
                let p = unsafe { &**p };
                offset < (p.address + p.size) as usize && offset + size > p.address as usize
            })
        {
            return -1;
        }
        let p = Box::into_raw(Box::new(esp_partition_t {
            address: offset as u32,
            size: size as u32,
        }));
        unsafe {
            *output = p;
        }
        partitions.push(p);
        ESP_OK
    })
}
unsafe fn esp_partition_deregister_external(partition: *const esp_partition_t) -> i32 {
    PARTITIONS.with_borrow_mut(|partitions| {
        let index = partitions
            .iter()
            .position(|p| p.cast_const() == partition)
            .unwrap();
        let p = partitions.remove(index);
        drop(unsafe { Box::from_raw(p) });
    });
    ESP_OK
}
unsafe fn esp_partition_read(
    p: &esp_partition_t,
    off: usize,
    bytes: *mut c_void,
    len: usize,
) -> i32 {
    assert!(off + len <= p.size as usize);
    FLASH.with_borrow(|flash| {
        unsafe { std::slice::from_raw_parts_mut(bytes.cast::<u8>(), len) }
            .copy_from_slice(&flash[p.address as usize + off..p.address as usize + off + len])
    });
    ESP_OK
}
unsafe fn esp_partition_write(
    p: &esp_partition_t,
    off: usize,
    bytes: *const c_void,
    len: usize,
) -> i32 {
    MUTATIONS.set(MUTATIONS.get() + 1);
    assert!(off + len <= p.size as usize);
    FLASH.with_borrow_mut(|flash| {
        for (old, new) in flash[p.address as usize + off..p.address as usize + off + len]
            .iter_mut()
            .zip(unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), len) })
        {
            assert_eq!(*old & *new, *new);
            *old &= *new;
        }
    });
    ESP_OK
}
unsafe fn esp_partition_erase_range(p: &esp_partition_t, off: usize, len: usize) -> i32 {
    MUTATIONS.set(MUTATIONS.get() + 1);
    assert!(off + len <= p.size as usize && off.is_multiple_of(4096) && len.is_multiple_of(4096));
    FLASH.with_borrow_mut(|flash| {
        flash[p.address as usize + off..p.address as usize + off + len].fill(0xff)
    });
    ESP_OK
}
fn open() -> Result<adapter::Storage, Error> {
    adapter::open([7; 32])
}
#[test]
fn common_filesystem_uses_only_application_partitions() {
    FLASH.with_borrow_mut(|f| f[..4096].fill(0x42));
    let mut fs = open().unwrap();
    let address = [1, 2, 3, 4, 5, 6];
    let identity = block_on(Identity::initialize(&mut fs, address, || {
        0x0123_4567_89ab_cdef
    }))
    .unwrap();
    drop(fs);
    let mut fs = open().unwrap();
    assert_eq!(block_on(Identity::load(&mut fs)).unwrap(), Some(identity));
    let mut bytes = [0; 100];
    let n = fs.read_file("/identity.pb", 0, &mut bytes).unwrap();
    let saved = cordial_protocol::storage::Identity::decode(&bytes[..n]).unwrap();
    assert_eq!(saved.address, address);
    FLASH.with_borrow(|f| assert!(f[..4096].iter().all(|b| *b == 0x42)));
}
#[test]
fn startup_recovers_nonblank_storage_without_touching_firmware() {
    for offset in [2 * 1024 * 1024, 2 * 1024 * 1024 + 100, 4 * 1024 * 1024 - 1] {
        FLASH.with_borrow_mut(|f| {
            f.fill(0xff);
            f[..board::STORAGE_START as usize].fill(0x42);
            f[offset] = 0;
        });
        let mut fs = open().unwrap();
        assert!(block_on(cordial_core::storage::format(&mut fs)).is_ok());
        FLASH.with_borrow(|f| {
            assert!(
                f[..board::STORAGE_START as usize]
                    .iter()
                    .all(|v| *v == 0x42)
            )
        });
    }
}
#[test]
fn runtime_partitions_use_detected_capacity_and_release_registration() {
    for capacity in [4, 6, 8, 16, 32] {
        FLASH.with_borrow_mut(|f| *f = vec![0xff; capacity * 1024 * 1024]);
        let mut fs = open().unwrap();
        assert_eq!(
            fs.total_bytes().unwrap(),
            (capacity - 2) * 1024 * 1024 - 4096
        );
        PARTITIONS.with_borrow(|p| assert_eq!(p.len(), 2));
        let mut flash = fs.into_flash();
        use embedded_storage_async::nor_flash::{NorFlash, NorFlashErrorKind, ReadNorFlash};
        let end = (capacity * 1024 * 1024) as u32;
        assert_eq!(
            block_on(flash.write(end, &[0; 4])),
            Err(NorFlashErrorKind::OutOfBounds)
        );
        assert_eq!(
            block_on(flash.erase(end, end + 4096)),
            Err(NorFlashErrorKind::OutOfBounds)
        );
        assert_eq!(
            block_on(flash.read(u32::MAX, &mut [0; 4])),
            Err(NorFlashErrorKind::OutOfBounds)
        );
        drop(flash);
        PARTITIONS.with_borrow(|p| assert!(p.is_empty()));
    }
}

#[test]
fn partitions_and_records_survive_smaller_and_unavailable_probes() {
    FLASH.with_borrow_mut(|f| *f = vec![0xff; 16 * 1024 * 1024]);
    DETECTED.set(Some(4 * 1024 * 1024));
    let mut fs = open().unwrap();
    let identity = block_on(Identity::initialize(&mut fs, [1; 6], || 7)).unwrap();
    fs.replace_file("/saved.bin", &[42; 9000]).unwrap();
    drop(fs);
    for (step, capacity) in [8, 0, 4, 8, 16].into_iter().enumerate() {
        DETECTED.set(Some(capacity * 1024 * 1024));
        let before = FLASH.with_borrow(Clone::clone);
        let mut fs = open().unwrap();
        let end = if capacity == 16 { 16 } else { 8 } * 1024 * 1024;
        assert_eq!(fs.total_bytes().unwrap(), end - 2 * 1024 * 1024 - 4096);
        assert_eq!(
            block_on(Identity::load(&mut fs)).unwrap(),
            Some(identity.clone())
        );
        let mut bytes = [0; 9000];
        assert_eq!(
            fs.read_file("/saved.bin", 0, &mut bytes).unwrap(),
            bytes.len()
        );
        assert_eq!(bytes, [42; 9000]);
        PARTITIONS.with_borrow(|p| {
            let app = unsafe { &*p[1] };
            assert_eq!((app.address + app.size) as usize, end);
        });
        drop(fs);
        PARTITIONS.with_borrow(|p| assert!(p.is_empty()));
        if (1..4).contains(&step) {
            FLASH.with_borrow(|f| assert_eq!(*f, before));
        }
    }
}

#[test]
fn cold_probe_failure_and_smaller_measurement_keep_expanded_filesystem_readable() {
    FLASH.with_borrow_mut(|f| *f = vec![0xff; 8 * 1024 * 1024]);
    let mut fs = open().unwrap();
    let identity = block_on(Identity::initialize(&mut fs, [1; 6], || 7)).unwrap();
    fs.replace_file("/saved.bin", &vec![42; 3 * 1024 * 1024])
        .unwrap();
    drop(fs);
    let before = FLASH.with_borrow(Clone::clone);
    CACHED_SIZE.set(8 * 1024 * 1024);
    for capacity in [0, 1, 2, 4, 8] {
        DRIVER_SIZE.set(board::MINIMUM_FLASH_BYTES);
        DETECTED.set(Some(capacity * 1024 * 1024));
        let mut fs = open().unwrap();
        assert_eq!(fs.total_bytes().unwrap(), 6 * 1024 * 1024 - 4096);
        assert_eq!(
            block_on(Identity::load(&mut fs)).unwrap(),
            Some(identity.clone())
        );
        let mut bytes = vec![0; 3 * 1024 * 1024];
        assert_eq!(
            fs.read_file("/saved.bin", 0, &mut bytes).unwrap(),
            bytes.len()
        );
        assert!(bytes.iter().all(|b| *b == 42));
        drop(fs);
        FLASH.with_borrow(|f| assert_eq!(*f, before));
        PARTITIONS.with_borrow(|p| assert!(p.is_empty()));
    }
    DETECTED.set(None);
    let mut fs = open().unwrap();
    assert_eq!(block_on(Identity::load(&mut fs)).unwrap(), Some(identity));
    let mut bytes = vec![0; 3 * 1024 * 1024];
    assert_eq!(
        fs.read_file("/saved.bin", 0, &mut bytes).unwrap(),
        bytes.len()
    );
    assert!(bytes.iter().all(|b| *b == 42));
}

#[test]
fn fallback_formats_only_minimum_despite_cached_larger_extent() {
    FLASH.with_borrow_mut(|f| *f = vec![0xff; 8 * 1024 * 1024]);
    CACHED_SIZE.set(8 * 1024 * 1024);
    DETECTED.set(Some(0));
    FLASH.with_borrow_mut(|f| f[4 * 1024 * 1024..].fill(0x42));
    let mut fs = open().unwrap();
    assert_eq!(fs.total_bytes().unwrap(), 2 * 1024 * 1024 - 4096);
    FLASH.with_borrow(|f| assert!(f[4 * 1024 * 1024..].iter().all(|b| *b == 0x42)));
    fs.replace_file("/saved.bin", &[7; 9000]).unwrap();
    drop(fs);
    DETECTED.set(Some(8 * 1024 * 1024));
    let mut fs = open().unwrap();
    assert_eq!(fs.total_bytes().unwrap(), 6 * 1024 * 1024 - 4096);
    assert_eq!(fs.file_size("/saved.bin").unwrap(), Some(9000));
    fs.replace_file("/format.pb", &[8, 99]).unwrap();
    drop(fs);
    DETECTED.set(Some(0));
    let tail = FLASH.with_borrow(|f| f[4 * 1024 * 1024..].to_vec());
    let mut fs = open().unwrap();
    assert_eq!(fs.total_bytes().unwrap(), 2 * 1024 * 1024 - 4096);
    assert_eq!(fs.file_size("/saved.bin").unwrap(), None);
    FLASH.with_borrow(|f| assert_eq!(f[4 * 1024 * 1024..], tail));
}

#[test]
fn detected_below_minimum_never_formats_fresh_or_invalid_filesystems() {
    for fill in [0xff, 0x42] {
        FLASH.with_borrow_mut(|f| *f = vec![fill; 8 * 1024 * 1024]);
        CACHED_SIZE.set(8 * 1024 * 1024);
        for capacity in [1, 2] {
            DETECTED.set(Some(capacity * 1024 * 1024));
            let before = FLASH.with_borrow(Clone::clone);
            MUTATIONS.set(0);
            assert!(matches!(open(), Err(Error::Bounds)));
            assert_eq!(MUTATIONS.get(), 0);
            FLASH.with_borrow(|f| assert_eq!(*f, before));
            PARTITIONS.with_borrow(|p| assert!(p.is_empty()));
        }
    }
}

#[test]
fn detected_below_minimum_forbids_root_reset_but_missing_detection_allows_it() {
    for root in ["/format.pb", "/identity.pb", "/sequence.pb"] {
        FLASH.with_borrow_mut(|f| *f = vec![0xff; 8 * 1024 * 1024]);
        DETECTED.set(Some(8 * 1024 * 1024));
        CACHED_SIZE.set(8 * 1024 * 1024);
        let mut fs = open().unwrap();
        block_on(Identity::initialize(&mut fs, [1; 6], || 7)).unwrap();
        fs.replace_file("/devices/1/device.pb", &[42; 100]).unwrap();
        if root == "/format.pb" {
            fs.replace_file(root, &[8, 99]).unwrap();
        } else {
            fs.remove_file(root).unwrap();
        }
        drop(fs);
        let before = FLASH.with_borrow(Clone::clone);
        for capacity in [1, 2] {
            DETECTED.set(Some(capacity * 1024 * 1024));
            DRIVER_SIZE.set(board::MINIMUM_FLASH_BYTES);
            MUTATIONS.set(0);
            assert!(matches!(open(), Err(Error::Bounds)));
            assert_eq!(MUTATIONS.get(), 0);
            FLASH.with_borrow(|f| assert_eq!(*f, before));
            PARTITIONS.with_borrow(|p| assert!(p.is_empty()));
        }
        DETECTED.set(Some(0));
        let mut fs = open().unwrap();
        assert!(block_on(Identity::load(&mut fs)).unwrap().is_none());
        assert_eq!(fs.file_size("/devices/1/device.pb").unwrap(), None);
    }
}
