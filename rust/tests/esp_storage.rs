//! Exercise the actual ESP raw adapter without hardware or NVS calls.
#![allow(non_camel_case_types, non_upper_case_globals)]
extern crate self as esp_idf_sys;
use cordial_core::storage::Error;
use std::{
    cell::RefCell,
    ffi::{c_char, c_void},
};
mod board {
    pub const STORAGE_START: u32 = 4096;
    pub const STORAGE_END: u32 = 128 * 1024;
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
static GUARD: esp_partition_t = esp_partition_t {
    address: 4096,
    size: 4096,
};
static APP: esp_partition_t = esp_partition_t {
    address: 8192,
    size: 120 * 1024,
};
thread_local! { static FLASH: RefCell<Vec<u8>> = RefCell::new(vec![0xff; 128*1024]); }
unsafe fn esp_partition_find_first(
    kind: u32,
    subtype: u32,
    name: *const c_char,
) -> *const esp_partition_t {
    assert_eq!(kind, 1);
    assert!(name.is_null());
    match subtype {
        0x40 => &GUARD,
        0x83 => &APP,
        _ => std::ptr::null(),
    }
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
    assert!(off + len <= p.size as usize && off.is_multiple_of(4096) && len.is_multiple_of(4096));
    FLASH.with_borrow_mut(|flash| {
        flash[p.address as usize + off..p.address as usize + off + len].fill(0xff)
    });
    ESP_OK
}
fn open() -> Result<adapter::Storage, Error> {
    adapter::open([7; 32], board::STORAGE_START..board::STORAGE_END)
}
#[test]
fn common_filesystem_uses_only_application_partitions() {
    FLASH.with_borrow_mut(|f| f[..4096].fill(0x42));
    let mut fs = open().unwrap();
    fs.replace_file("/devices/1/device.json", b"{\"name\":\"keyboard\"}")
        .unwrap();
    let mut fs = open().unwrap();
    let mut bytes = [0; 100];
    let n = fs
        .read_file("/devices/1/device.json", 0, &mut bytes)
        .unwrap();
    assert_eq!(&bytes[..n], b"{\"name\":\"keyboard\"}");
    FLASH.with_borrow(|f| assert!(f[..4096].iter().all(|b| *b == 0x42)));
}
#[test]
fn unknown_layout_and_nonblank_flash_are_never_formatted() {
    for offset in [4096, 4196, 8192, 128 * 1024 - 1] {
        FLASH.with_borrow_mut(|f| {
            f.fill(0xff);
            f[offset] = 0;
        });
        let before = FLASH.with_borrow(Clone::clone);
        assert!(matches!(open(), Err(Error::Layout)));
        FLASH.with_borrow(|f| assert_eq!(f, &before));
    }
}
