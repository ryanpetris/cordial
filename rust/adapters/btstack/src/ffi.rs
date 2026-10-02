//! ABI declarations for our C adapter. Vendor structs stay behind this boundary.
use crate::storage::Tlv;

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Link {
    pub generation: u64,
    pub slot: u8,
}
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Peer {
    pub address: [u8; 6],
    pub transport: u8,
    pub random: u8,
}
#[repr(C)]
pub struct Event {
    pub link: Link,
    pub peer: Peer,
    pub address: Peer,
    pub operation: u64,
    pub number: u32,
    pub service: u16,
    pub length: u16,
    pub rssi: i16,
    pub kind: u8,
    pub code: u8,
    pub report_id: u8,
    pub report_type: u8,
    pub data: *const u8,
    /// A GATT Database Hash of 16 bytes, or null.
    pub hash: *const u8,
}
pub type Emit = unsafe extern "C" fn(*mut c_void, *const Event) -> c_int;
/// One Report characteristic of a BLE layout, as C stores and reports it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct LayoutReport {
    pub value: u16,
    pub cccd: u16,
    pub properties: u8,
    pub service: u8,
    pub id: u8,
    pub kind: u8,
}
const _: () = assert!(core::mem::size_of::<LayoutReport>() == 8);
/// Report characteristics C keeps per link; the application limit agrees.
pub const LAYOUT_REPORTS: usize = 32;
const _: () = assert!(LAYOUT_REPORTS == cordial_core::bluetooth::LAYOUT_REPORTS);
/// A saved layout lent to C for one call: the BLE report table or the
/// Classic report map.
#[repr(C)]
pub struct Layout {
    pub reports: *const LayoutReport,
    pub descriptor: *const u8,
    /// The saved GATT Database Hash of 16 bytes, or null.
    pub hash: *const u8,
    pub count: u16,
    pub length: u16,
}
use core::ffi::{c_char, c_int, c_void};

#[repr(C)]
pub struct RuntimeCallbacks {
    pub context: *mut c_void,
    pub time_ms: unsafe extern "C" fn(*mut c_void) -> u32,
    pub wake: unsafe extern "C" fn(*mut c_void),
    pub can_send: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub send: unsafe extern "C" fn(*mut c_void, u8, *const u8, u16) -> c_int,
    pub fatal: unsafe extern "C" fn(*mut c_void),
}

#[repr(C)]
pub struct Chipset {
    pub name: *const c_char,
    pub init: Option<unsafe extern "C" fn(*const c_void)>,
    pub next_command: Option<unsafe extern "C" fn(*mut u8) -> c_int>,
    pub set_baudrate: Option<unsafe extern "C" fn(u32, *mut u8)>,
    pub set_address: Option<unsafe extern "C" fn(*const u8, *mut u8)>,
}

unsafe extern "C" {
    pub static cordial_layout_report_capacity: u16;
    pub fn cordial_bond_capacity(transport: u32) -> u32;
    pub fn cordial_bond_import(bytes: *const u8, size: u32) -> c_int;
    pub fn cordial_bond_export(
        transport: u32,
        random: u32,
        address: *const u8,
        bytes: *mut u8,
    ) -> c_int;

    pub fn cordial_runtime_init(
        callbacks: *const RuntimeCallbacks,
        tlv: *const Tlv,
        storage: *mut c_void,
        chipset: *const Chipset,
    );
    pub fn cordial_runtime_poll();
    pub fn cordial_runtime_timeout() -> i32;
    pub fn cordial_runtime_receive(kind: u8, data: *mut u8, size: u16);
    pub fn cordial_runtime_sent();
    pub fn cordial_profiles_init(context: *mut c_void, emit: Emit);
    pub fn cordial_profiles_poll(now: u64);
    pub fn cordial_profiles_start(address: *const u8) -> c_int;
    pub fn cordial_profiles_stop();
    pub fn cordial_profiles_scan_and_connect() -> bool;
    pub fn cordial_profiles_scan(id: u64, classic: bool, ble: bool) -> c_int;
    pub fn cordial_profiles_set_transport(transport: u8, enabled: bool) -> c_int;
    pub fn cordial_profiles_connect(
        link: Link,
        peer: Peer,
        pairing: bool,
        layout: *const Layout,
    ) -> c_int;
    pub fn cordial_profiles_reconnect(peers: *const Peer, count: usize) -> c_int;
    pub fn cordial_profiles_incoming(
        attempt: u32,
        accept: *const Link,
        layout: *const Layout,
    ) -> c_int;
    pub fn cordial_profiles_disconnect(link: Link);
    pub fn cordial_profiles_adopt(link: Link) -> c_int;
    pub fn cordial_profiles_reply(
        link: Link,
        method: u8,
        accept: bool,
        value: *const c_char,
    ) -> c_int;
    pub fn cordial_profiles_write(
        link: Link,
        sequence: u32,
        service: u16,
        kind: u8,
        report_id: u16,
        data: *const u8,
        size: u16,
    ) -> c_int;
    pub fn cordial_profiles_read(
        link: Link,
        sequence: u32,
        service: u16,
        kind: u8,
        report_id: u16,
    ) -> c_int;
    pub fn cordial_profiles_info_refresh(link: Link) -> c_int;
    pub fn cordial_profiles_info_busy(link: Link) -> c_int;
    pub fn cordial_profiles_can_write(link: Link) -> c_int;
    pub fn cordial_profiles_bonds(peers: *mut Peer, capacity: u32) -> c_int;
    pub fn cordial_profiles_forget(peer: Peer) -> c_int;
}
