//! CDC-only management with no application or radio owner.
use crate::{Io, SerialRx, SerialTx, Status};
use cordial_core::{migration::management::Management, storage::RecordStore};
use cordial_protocol::frame::DELIMITER;
use embassy_futures::select::select;
use embassy_time::Timer;

pub async fn run<S: RecordStore>(
    io: &Io,
    mut management: Management,
    store: &mut S,
    reset: fn() -> !,
) -> ! {
    let mut seen = Status::default();
    let mut pending = false;
    let mut separator = false;
    let mut input: Option<SerialRx> = None;
    let mut offset = 0;
    loop {
        while let Ok(done) = io.serial_done.try_receive() {
            if done.session == seen.session {
                pending = false;
                if let Some(token) = done.token {
                    management.output_complete(token, done.length);
                } else if done.length != 0 {
                    separator = false;
                }
            }
        }
        if management.reset_ready() {
            reset();
        }
        let status = io.status();
        if status.session != seen.session {
            management.session(status.serial_open());
            pending = false;
            separator = status.serial_open();
            input = None;
            offset = 0;
            seen = status;
        }
        management.proceed(store).await;
        if !management.restarting() {
            if input.is_none() {
                input = io.serial_rx.try_receive().ok();
            }
            if input
                .as_ref()
                .is_some_and(|packet| packet.session != seen.session || !management.serial.open())
            {
                input = None;
                offset = 0;
            }
            if let Some(packet) = &input {
                let (used, request) = management.serial.feed(&packet.bytes[offset..packet.length]);
                offset += used;
                if offset == packet.length {
                    input = None;
                    offset = 0;
                }
                if let Some(request) = request {
                    management.begin(request, store).await;
                }
            }
        }
        let status = io.status();
        if status.serial_open()
            && status.session == seen.session
            && !pending
            && !io.serial_tx.is_full()
        {
            let mut tx = SerialTx {
                session: status.session,
                token: None,
                bytes: [0; 63],
                length: 0,
            };
            if separator {
                tx.bytes[0] = DELIMITER;
                tx.length = 1;
            } else if let Some((token, bytes)) = management.serial.output(63) {
                tx.token = Some(token);
                tx.length = bytes.len();
                tx.bytes[..tx.length].copy_from_slice(bytes);
            }
            if tx.length != 0 {
                io.serial_tx.try_send(tx).ok().unwrap();
                pending = true;
            }
        }
        select(io.changed(), Timer::after_millis(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SerialDone,
        tests::{client_hello, complete_serial, support::Store},
    };
    use cordial_core::{application::Build, migration::Startup};
    use cordial_protocol::{self as p, request::Command};
    use core::{
        future::Future,
        pin::pin,
        task::{Context, Waker},
    };
    use embassy_futures::block_on;

    fn management(startup: Startup) -> Management {
        Management::new(
            Build {
                development: false,
                version: "test",
                board: "test",
                default_adapter_name: "Test adapter",
                adapter_id: "adapter".into(),
                profile_memory_budget: None,
                restart: || panic!("restarted"),
                bootloader: None,
            },
            startup,
        )
    }

    fn negotiate(management: &mut Management) {
        let hello = client_hello();
        assert_eq!(management.serial.feed(&hello), (hello.len(), None));
        while let Some((token, bytes)) = management.serial.output(63) {
            let length = bytes.len();
            management.output_complete(token, length);
            assert!(!management.reset_ready());
        }
        assert!(management.serial.open());
    }

    #[test]
    fn management_runtime_replies_to_optional_queries_before_restart() {
        use prost::Message;
        for startup in [
            Startup::Assisted,
            Startup::Blocked(cordial_core::storage::Error::Io),
        ] {
            for query in [false, true] {
                let io = Io::new();
                io.update(|status| {
                    status.configured = true;
                    status.dtr = true;
                    status.session = 1;
                });
                let mut bytes = if query { client_hello() } else { vec![] };
                p::frame::encode(
                    &p::Request {
                        command: Some(Command::Restart(p::Restart {})),
                    },
                    &mut bytes,
                );
                let mut packet = SerialRx {
                    session: 1,
                    bytes: [0; 64],
                    length: bytes.len(),
                };
                packet.bytes[..bytes.len()].copy_from_slice(&bytes);
                io.serial_rx.try_send(packet).ok().unwrap();
                let mut store = Store::default();
                let mut running = pin!(run(&io, management(startup), &mut store, || {
                    std::panic::panic_any("reset")
                }));
                let mut step = || {
                    io.changed.signal(());
                    assert!(
                        running
                            .as_mut()
                            .poll(&mut Context::from_waker(Waker::noop()))
                            .is_pending()
                    );
                };
                step();
                let separator = io.serial_tx.try_receive().unwrap();
                assert_eq!(&separator.bytes[..separator.length], &[DELIMITER]);
                complete_serial(&io, &separator, separator.length);
                step();
                if query {
                    let hello = io.serial_tx.try_receive().unwrap();
                    let mut decoder = p::frame::Decoder::new(Some(p::MAX_HANDSHAKE_BYTES));
                    let identity = hello.bytes[..hello.length]
                        .iter()
                        .find_map(|&byte| {
                            decoder.push(byte).map(|frame| {
                                p::handshake::decode_adapter_hello(frame.unwrap()).unwrap()
                            })
                        })
                        .unwrap();
                    assert_eq!(identity.protocol_version, Some(p::CURRENT_PROTOCOL_VERSION));
                    assert_eq!(identity.firmware_version.as_deref(), Some("test"));
                    assert_eq!(identity.serial_number.as_deref(), Some("adapter"));
                    complete_serial(&io, &hello, 0);
                    step();
                    let retry = io.serial_tx.try_receive().unwrap();
                    assert_eq!(&retry.bytes[..retry.length], &hello.bytes[..hello.length]);
                    complete_serial(&io, &retry, retry.length);
                    step();
                }
                let ack = io.serial_tx.try_receive().unwrap();
                let mut decoder = p::frame::Decoder::new(None);
                let message = ack.bytes[..ack.length]
                    .iter()
                    .find_map(|&byte| {
                        decoder
                            .push(byte)
                            .map(|frame| p::Message::decode(frame.unwrap()).unwrap())
                    })
                    .unwrap();
                assert_eq!(
                    message.kind,
                    Some(p::message::Kind::Response(p::Response { result: None }))
                );
                complete_serial(&io, &ack, 1);
                step();
                let tail = io.serial_tx.try_receive().unwrap();
                assert_eq!(&tail.bytes[..tail.length], &ack.bytes[1..ack.length]);
                complete_serial(&io, &tail, tail.length);
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)).unwrap_err();
                assert_eq!(outcome.downcast_ref::<&str>(), Some(&"reset"));
            }
        }
    }

    #[test]
    fn disconnect_preserves_only_a_fully_drained_restart_ack() {
        for completion in [None, Some(0), Some(1), Some(usize::MAX)] {
            let mut store = Store::default();
            store.files.insert("/format.pb".into(), alloc::vec![8, 2]);
            let mut management = management(Startup::Assisted);
            management.session(true);
            negotiate(&mut management);
            block_on(management.begin(
                p::Request {
                    command: Some(Command::Restart(p::Restart {})),
                },
                &mut store,
            ));
            assert!(management.restarting());
            let (token, bytes) = management.serial.output(63).unwrap();
            let length = bytes.len();
            assert!(length > 1);
            let io = Io::new();
            io.update(|status| status.session = 1);
            if let Some(completed) = completion {
                io.serial_done
                    .try_send(SerialDone {
                        session: 0,
                        token: Some(token),
                        length: completed.min(length),
                    })
                    .ok()
                    .unwrap();
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut running = pin!(run(&io, management, &mut store, || std::panic::panic_any(
                    "reset"
                )));
                assert!(
                    running
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending()
                );
            }));
            match completion {
                Some(usize::MAX) => {
                    assert_eq!(outcome.unwrap_err().downcast_ref::<&str>(), Some(&"reset"))
                }
                _ => assert!(outcome.is_ok()),
            }
        }
    }
}
