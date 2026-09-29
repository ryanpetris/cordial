#![cfg(feature = "ffi")]
mod common;
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::application::{Application, Bootloader, Build};
use embassy_futures::block_on;

#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}

#[test]
fn management_and_development_recovery_work_before_controller_start() {
    let io = Box::leak(Box::new(Io::new()));
    let storage = Box::leak(Box::new(Storage::new(support::Store::default())));
    embassy_futures::block_on(cordial_core::identity::Identity::initialize(
        &mut storage.handle(),
        [2; 6],
        || 42,
    ))
    .unwrap();
    let state = Box::leak(Box::new(
        State::new(storage, io, || 0, || std::process::abort()).unwrap(),
    ));
    let mut radio = unsafe { Backend::new(state, None) };
    let mut store = storage.handle();
    let mut input = [0; 4096];
    let mut app = Application::new(
        &mut input,
        Build {
            profile: cordial_protocol::messages::BuildProfile::Development,
            version: "test",
            hardware: "test",
            default_adapter_name: "Test adapter",
            radio_backend: "pico-sdk-cyw43",
            digest: "test",
            adapter_id: "adapter".into(),
            boot_id: "boot".into(),
            bootloader: Some(Bootloader {
                mode: "bootsel",
                enter: || std::panic::panic_any("entered bootloader"),
            }),
        },
    );
    app.session(true, &mut radio, 0);
    for (command, expected) in [
        (
            b"{\"v\":1,\"id\":1,\"cmd\":\"adapter.status\",\"args\":{}}\n".as_slice(),
            "\"radio_ready\":false",
        ),
        (
            b"{\"v\":1,\"id\":2,\"cmd\":\"adapter.wait_ready\",\"args\":{}}\n".as_slice(),
            "\"state\":\"initializing\"",
        ),
        (
            b"{\"v\":1,\"id\":3,\"cmd\":\"session.heartbeat\",\"args\":{}}\n".as_slice(),
            "\"ok\":true",
        ),
        (
            b"{\"v\":1,\"id\":4,\"cmd\":\"adapter.capabilities\",\"args\":{}}\n".as_slice(),
            "\"result\":[\"classic\",\"ble\",\"debug\",\"storage_management\"]",
        ),
        (
            b"{\"v\":1,\"id\":5,\"cmd\":\"adapter.bootloader.enter\",\"args\":{}}\n".as_slice(),
            "\"rebooting\":true",
        ),
    ] {
        let (_, request) = app.serial.feed(command, 1);
        block_on(app.dispatch(&request.unwrap(), &mut store, &mut radio, 1)).unwrap();
        let mut output = Vec::new();
        while let Some((token, bytes)) = app.serial.output(64, 1) {
            output.extend_from_slice(bytes);
            let length = bytes.len();
            app.serial.output_complete(token, length);
        }
        assert!(std::str::from_utf8(&output).unwrap().contains(expected));
        radio.poll();
        assert!(radio.next_event().is_none());
        assert!(
            common::take(io).is_none(),
            "controller started before its address was available"
        );
    }
    let reboot = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        block_on(app.poll(&mut store, &mut radio, 0, 251));
    }))
    .expect_err("development recovery was not dispatched");
    assert_eq!(reboot.downcast_ref::<&str>(), Some(&"entered bootloader"));
}
