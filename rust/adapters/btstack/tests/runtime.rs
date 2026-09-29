#![cfg(feature = "ffi")]

use cordial_btstack::{ffi::*, storage::Tlv};
use core::ffi::{c_int, c_void};
use std::ptr;

#[derive(Default)]
struct Controller {
    busy: bool,
    commands: Vec<Vec<u8>>,
}
unsafe extern "C" fn time(_: *mut c_void) -> u32 {
    0
}
unsafe extern "C" fn wake(_: *mut c_void) {}
unsafe extern "C" fn can_send(ctx: *mut c_void) -> c_int {
    i32::from(!unsafe { &*ctx.cast::<Controller>() }.busy)
}
unsafe extern "C" fn send(ctx: *mut c_void, kind: u8, data: *const u8, n: u16) -> c_int {
    let controller = unsafe { &mut *ctx.cast::<Controller>() };
    assert_eq!(kind, 1);
    assert!(!controller.busy);
    controller.busy = true;
    controller
        .commands
        .push(unsafe { core::slice::from_raw_parts(data, n.into()) }.to_vec());
    0
}
unsafe extern "C" fn fatal(_: *mut c_void) {
    std::process::abort();
}
unsafe extern "C" fn get(_: *mut c_void, _: u32, _: *mut u8, _: u32) -> c_int {
    0
}
unsafe extern "C" fn store(_: *mut c_void, _: u32, _: *const u8, _: u32) -> c_int {
    0
}
unsafe extern "C" fn delete(_: *mut c_void, _: u32) {}
unsafe extern "C" {
    fn hci_power_control(mode: c_int) -> c_int;
    fn hci_close();
    fn le_device_db_info(index: c_int, kind: *mut c_int, address: *mut u8, irk: *mut u8);
    fn le_device_db_add(kind: c_int, address: *mut u8, irk: *mut u8) -> c_int;
    fn le_device_db_remove(index: c_int);
    fn cordial_bond_export(
        transport: c_int,
        random: c_int,
        address: *const u8,
        output: *mut u8,
    ) -> c_int;
}

#[test]
fn upstream_stack_initialization_uses_owned_transport_and_public_run_loop() {
    let mut controller = Controller::default();
    let cb = RuntimeCallbacks {
        context: ptr::from_mut(&mut controller).cast(),
        time_ms: time,
        wake,
        can_send,
        send,
        fatal,
    };
    let tlv = Tlv { get, store, delete };
    unsafe {
        cordial_runtime_init(&cb, &tlv, ptr::null_mut(), ptr::null());
        let mut kind = -1;
        let mut address = [0xff; 6];
        let mut irk = [0xff; 16];
        le_device_db_info(0, &mut kind, address.as_mut_ptr(), irk.as_mut_ptr());
        assert_eq!(kind, 0xfe);
        assert_eq!(address, [0; 6]);
        assert_eq!(irk, [0; 16]);
        address = [2; 6];
        let index = le_device_db_add(0, address.as_mut_ptr(), irk.as_mut_ptr());
        assert!(index >= 0);
        let mut record = [0; 145];
        assert_eq!(
            cordial_bond_export(2, 0, address.as_ptr(), record.as_mut_ptr()),
            145
        );
        assert_eq!(record[81] & 2, 0);
        le_device_db_remove(index);
        assert_eq!(hci_power_control(1), 0);
        assert_eq!(controller.commands, vec![vec![3, 12, 0]]);
        controller.busy = false;
        cordial_runtime_sent();
        cordial_runtime_poll();
        assert_eq!(controller.commands.len(), 1);
        let mut reset_complete = [0x0e, 4, 1, 3, 12, 0];
        cordial_runtime_receive(4, reset_complete.as_mut_ptr(), reset_complete.len() as u16);
        assert_eq!(controller.commands.last().unwrap(), &[1, 16, 0]);
        assert!(cordial_runtime_timeout() >= -1);
        hci_close();
    }
}
