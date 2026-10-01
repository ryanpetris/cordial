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
    let mut app = Application::new(Build {
        development: true,
        version: "test",
        board: "test",
        default_adapter_name: "Test adapter",
        adapter_id: "adapter".into(),
        bootloader: Some(Bootloader {
            enter: || std::panic::panic_any("entered bootloader"),
        }),
    });
    app.session(true, &mut radio);
    use cordial_protocol::{self as p, request::Command, response::Result as R};
    use prost::Message;
    for command in [
        Command::GetStatus(p::GetStatus {}),
        Command::EnterBootloader(p::EnterBootloader {}),
    ] {
        let mut frame = Vec::new();
        p::frame::encode(
            &p::Request {
                command: Some(command),
            },
            &mut frame,
        );
        let (_, request) = app.serial.feed(&frame);
        block_on(app.dispatch(request.unwrap(), &mut store, &mut radio, 1));
        let mut output = Vec::new();
        while let Some((token, bytes)) = app.serial.output(64) {
            output.extend_from_slice(bytes);
            let length = bytes.len();
            app.serial.output_complete(token, length);
        }
        let mut decoder = p::frame::Decoder::new(None);
        let message = output
            .iter()
            .find_map(|&b| {
                decoder
                    .push(b)
                    .map(|f| p::Message::decode(f.unwrap()).unwrap())
            })
            .unwrap();
        let Some(p::message::Kind::Response(response)) = message.kind else {
            panic!("expected a response");
        };
        match response.result {
            Some(R::Status(status)) => assert!(!status.ready),
            None => {}
            other => panic!("{other:?}"),
        }
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
