mod common;
use cordial_client::client::Wait;
use cordial_protocol::{identifiers::*, messages::*};
use serde_json::json;
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn multiplexes_pairing_commands_and_stops_only_control_on_exit() {
    let (client, firmware) = common::connect();
    let wait = Wait::timeout(Duration::from_secs(2));
    let scan = client
        .start(
            Command::Scan(Scan {
                transport: ScanTransport::Both,
                duration_ms: 0,
            }),
            false,
            &wait,
        )
        .unwrap();
    firmware.receive();
    let pair = client
        .start(
            Command::Pair(Pair {
                candidate_id: CandidateId("c_test".into()),
                timeout_ms: 1000,
            }),
            false,
            &wait,
        )
        .unwrap();
    let request = firmware.receive();
    firmware.event(
        "pairing.prompt",
        Some(pair.id),
        json!({"prompt_id":"p_test","method":"enter_passkey","expires_in_ms":1000}),
    );
    client
        .call(Command::Status(Empty {}), false, &wait)
        .unwrap();
    firmware.reply(&request, json!({"device_id":"d_test"}));
    assert!(client.wait(pair, &wait).unwrap().last().unwrap().done());
    let mut prompt = false;
    while let Some(envelope) = client.next(Duration::ZERO).unwrap() {
        prompt |= envelope.event() == Some("pairing.prompt");
        assert!(!envelope.raw.is_empty());
    }
    assert!(prompt);
    client.shutdown();
    assert!(client.wait(scan, &wait).is_err());
    drop(client);
    assert_eq!(*firmware.dtr.lock().unwrap(), [false, true, false]);
    assert!(firmware.requests.try_recv().is_err());
}

#[test]
fn concurrent_starts_write_ids_in_order_and_abandoned_waiters_do_not_break_session() {
    let (client, firmware) = common::connect();
    let client = Arc::new(client);
    let mut jobs = Vec::new();
    for _ in 0..4 {
        let client = client.clone();
        jobs.push(thread::spawn(move || {
            client
                .start(
                    Command::Devices(DeviceFilter::default()),
                    false,
                    &Wait::timeout(Duration::from_secs(2)),
                )
                .unwrap()
        }));
    }
    let requests: Vec<_> = (0..4).map(|_| firmware.receive()).collect();
    assert!(requests.windows(2).all(|w| w[0].id < w[1].id));
    for job in jobs {
        drop(job.join().unwrap());
    }
    for request in requests {
        firmware.reply(&request, json!({"count":0,"revision":0}));
    }
    client
        .call(
            Command::Status(Empty {}),
            false,
            &Wait::timeout(Duration::from_secs(2)),
        )
        .unwrap();
    assert!(client.error().is_none());
    client.shutdown();
}

#[test]
fn invalid_reply_ends_session_without_replaying_a_mutation() {
    let (client, firmware) = common::connect();
    let wait = Wait::timeout(Duration::from_secs(2));
    let request = client
        .start(
            Command::Unpair(DeviceRef {
                device_id: DeviceId("d_test".into()),
            }),
            false,
            &wait,
        )
        .unwrap();
    firmware.receive();
    firmware
        .send(&json!({"v":2,"type":"response","id":request.id,"ok":true,"done":true,"result":{}}));
    assert!(client.wait(request, &wait).is_err());
    assert!(
        firmware
            .requests
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );
    assert!(client.error().is_some());
}

#[test]
fn unplug_and_drop_release_the_session_without_waiting_for_heartbeat_interval() {
    let (client, firmware) = common::connect();
    firmware.disconnect();
    let until = Instant::now() + Duration::from_secs(2);
    while client.error().is_none() && Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(client.error().is_some());
    let start = Instant::now();
    drop(client);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(*firmware.dtr.lock().unwrap(), [false, true, false]);
}

#[test]
fn serial_short_writes_and_transient_stalls_resume_without_replaying_bytes() {
    use common::WriteStep;
    use std::io::ErrorKind;
    let (client, firmware) = common::connect();
    firmware.writes.lock().unwrap().extend([
        WriteStep::Bytes(5),
        WriteStep::Error(ErrorKind::TimedOut),
        WriteStep::Zero,
        WriteStep::Error(ErrorKind::WouldBlock),
        WriteStep::Error(ErrorKind::Interrupted),
    ]);
    let wait = Wait::timeout(Duration::from_secs(2));
    let request = client
        .start(
            Command::Info(cordial_protocol::messages::DeviceRef {
                device_id: DeviceId("d_test".into()),
            }),
            false,
            &wait,
        )
        .unwrap();
    let wire = firmware.receive();
    assert!(matches!(wire.command, Command::Info(_)));
    firmware.reply(&wire, json!({}));
    client.wait(request, &wait).unwrap();
    assert!(client.error().is_none());
    assert!(firmware.requests.try_recv().is_err());
    client.shutdown();
}

#[test]
fn devices_retries_a_changed_snapshot_and_discards_partial_rows() {
    let (client, firmware) = common::connect();
    let client = Arc::new(client);
    let c = client.clone();
    let job = thread::spawn(move || {
        c.call(
            Command::Devices(DeviceFilter::default()),
            true,
            &Wait::timeout(Duration::from_secs(2)),
        )
        .unwrap()
    });
    let first = firmware.receive();
    firmware.send(&Message::<serde_json::Value, ()>::success(
        first.id,
        json!({"stale":true}),
        false,
    ));
    firmware.send(&Message::<serde_json::Value, ()>::failure(
        first.id,
        WireError {
            code: cordial_protocol::errors::ErrorCode::Busy,
            details: None,
        },
    ));
    let second = firmware.receive();
    assert_ne!(first.id, second.id);
    firmware.reply(&second, json!({"count":0,"revision":2}));
    let rows = job.join().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].data().unwrap()["revision"], 2);
    client.shutdown();
}
