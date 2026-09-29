use cordial_core::control::*;
use cordial_protocol::{MAX_LINE_BYTES, identifiers::RequestId};
use serde_json::{Value, json};

fn id(n: u32) -> RequestId {
    n.try_into().unwrap()
}
fn drain(s: &mut Session<'_>, now: u64) -> Vec<Value> {
    let mut bytes = vec![];
    while let Some((token, chunk)) = s.output(17, now) {
        bytes.extend_from_slice(chunk);
        let n = chunk.len();
        s.output_complete(token, n);
    }
    bytes
        .split(|&b| b == b'\n')
        .filter(|b| !b.is_empty())
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect()
}

#[test]
fn configured_frames_backpressure_commands_and_keep_optional_events_before_required_ack() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut s = Session::new(&mut input);
    s.session(true, 0);
    s.monitor(id(1), true, 0).unwrap();
    drain(&mut s, 0);
    for n in 0..OUTPUT_FRAMES + 1 {
        s.event("device.changed", None, json!({"revision":n}), true, 1)
            .unwrap();
    }
    assert_eq!(s.queued(), OUTPUT_FRAMES - 2);
    let request =
        b"{\"v\":1,\"id\":2,\"cmd\":\"session.monitor.set\",\"args\":{\"enabled\":false}}\n";
    let (n, r) = s.feed(request, 2);
    assert_eq!(n, request.len());
    let r = r.unwrap();
    s.monitor(r.id, false, 2).unwrap();
    assert_eq!(s.queued(), OUTPUT_FRAMES);
    assert_eq!(s.feed(request, 2), (0, None));
    let out = drain(&mut s, 2);
    assert_eq!(out.len(), OUTPUT_FRAMES);
    assert_eq!(out[OUTPUT_FRAMES - 2]["event"], "events.lost");
    assert_eq!(out[OUTPUT_FRAMES - 2]["data"]["dropped"], 3);
    assert_eq!(out[OUTPUT_FRAMES - 1]["id"], 2);
    assert_eq!(out[OUTPUT_FRAMES - 1]["result"]["enabled"], false);
    for n in 3..3 + OUTPUT_FRAMES as u32 {
        s.response(id(n), true, json!({}), 3).unwrap();
    }
    let next = 3 + OUTPUT_FRAMES as u32;
    assert_eq!(
        s.response(id(next), true, json!({}), 3),
        Err(EmitError::Full)
    );
    assert_eq!(s.queued(), OUTPUT_FRAMES);
    drain(&mut s, 3);
    s.response(id(next), true, json!({}), 3).unwrap();
    assert_eq!(drain(&mut s, 3)[0]["id"], next);
}

#[test]
fn invalid_commands_consume_ids_but_protocol_errors_do_not() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut s = Session::new(&mut input);
    s.session(true, 0);
    let cases = [
        (
            r#"{"v":1,"id":1,"cmd":"missing","args":{}}"#,
            "response",
            "unknown_command",
        ),
        (
            r#"{"v":1,"id":1,"cmd":"adapter.status","args":{}}"#,
            "event",
            "invalid_request",
        ),
        (
            r#"{"v":1,"id":2,"cmd":"session.heartbeat","args":{"extra":true}}"#,
            "response",
            "invalid_args",
        ),
        (
            r#"{"v":2,"id":2,"cmd":"adapter.status","args":{}}"#,
            "event",
            "invalid_request",
        ),
        (
            r#"{"v":2,"id":3,"cmd":"adapter.status","args":{}}"#,
            "event",
            "unsupported_version",
        ),
        (
            r#"{"v":1,"id":3,"cmd":"adapter.status","args":{},"extra":1}"#,
            "event",
            "invalid_request",
        ),
    ];
    for (line, kind, code) in cases {
        let line = format!("{line}\n");
        assert!(s.feed(line.as_bytes(), 0).1.is_none());
        let out = drain(&mut s, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["type"], kind);
        assert_eq!(
            out[0][if kind == "event" { "data" } else { "error" }]["code"],
            code
        );
    }
    let request = b"{\"v\":1,\"id\":3,\"cmd\":\"adapter.status\",\"args\":{}}\r\n";
    assert!(s.feed(&request[..20], 0).1.is_none());
    let (n, r) = s.feed(&request[20..], 0);
    assert_eq!(n, request.len() - 20);
    assert_eq!(r.unwrap().id, id(3));
    s.session(false, 1);
    s.session(true, 2);
    assert_eq!(s.feed(request, 2).1.unwrap().id, id(3));
}

#[test]
fn monitor_lease_expiry_preserves_partial_output_and_late_completions_cannot_cross_sessions() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut s = Session::new(&mut input);
    s.session(true, 0);
    s.monitor(id(1), true, 0).unwrap();
    drain(&mut s, 0);
    s.event("device.changed", None, json!({"revision":1}), true, 1)
        .unwrap();
    s.event("device.changed", None, json!({"revision":2}), true, 1)
        .unwrap();
    let (token, bytes) = s.output(9, 2).unwrap();
    let mut partial = bytes.to_vec();
    let tick = s.tick(15_000);
    assert!(tick.presence_expired && !tick.session_fault);
    assert!(!s.present() && !s.monitoring());
    assert!(s.output(9, 15_000).is_none());
    s.output_complete(token, partial.len());
    while let Some((token, b)) = s.output(64, 15_000) {
        partial.extend_from_slice(b);
        let n = b.len();
        s.output_complete(token, n);
    }
    let lines = partial
        .split(|&b| b == b'\n')
        .filter(|b| !b.is_empty())
        .map(|b| serde_json::from_slice::<Value>(b).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines[0]["data"]["revision"], 1);
    assert_eq!(lines[1]["event"], "events.lost");
    s.heartbeat(id(2), 15_001).unwrap();
    assert!(s.present() && !s.monitoring());
    let (old, _) = s.output(8, 15_001).unwrap();
    s.session(false, 15_002);
    s.session(true, 15_003);
    s.response(id(1), true, json!({"new":true}), 15_003)
        .unwrap();
    let (new, b) = s.output(8, 15_003).unwrap();
    let n = b.len();
    s.output_complete(old, n);
    assert!(s.output(8, 15_003).is_none());
    s.output_complete(new, 0);
    assert_eq!(drain(&mut s, 15_003)[0]["result"]["new"], true);
}

#[test]
fn a_slow_required_reply_faults_only_the_session_and_oversized_input_recovers() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut s = Session::new(&mut input);
    s.session(true, 0);
    let mut big = vec![b'x'; MAX_LINE_BYTES + 10];
    big.push(b'\n');
    assert_eq!(s.feed(&big, 0).0, big.len());
    assert_eq!(drain(&mut s, 0)[0]["data"]["code"], "message_too_large");
    s.response(id(1), true, json!({"accepted":true}), 1)
        .unwrap();
    let tick = s.tick(5001);
    assert!(tick.session_fault);
    assert!(!s.active());
    let out = drain(&mut s, 5001);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[1]["data"]["code"], "session_fault");
    assert!(!s.tick(5002).session_fault);
    s.session(true, 5003);
    assert!(s.active());
    assert!(!s.monitoring());
}

#[test]
fn readiness_wait_is_reserved_and_returns_live_status_once_even_after_delayed_output() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut s = Session::new(&mut input);
    s.session(true, 0);
    s.ready(id(1), Readiness::Starting, &json!({}), 0).unwrap();
    assert_eq!(drain(&mut s, 0)[0]["result"]["state"], "initializing");
    s.ready(id(2), Readiness::Ready, &json!({}), 0).unwrap();
    assert_eq!(drain(&mut s, 0)[0]["error"]["code"], "busy");
    for n in 3..3 + OUTPUT_FRAMES as u32 {
        s.response(id(n), true, json!({}), 29_999).unwrap();
    }
    let status = json!({"storage_ready":true,"radio_ready":true,"name": "Test adapter", "host_platform":"mac"});
    assert_eq!(
        s.ready_poll(Readiness::Ready, &status, 30_000),
        Err(EmitError::Full)
    );
    drain(&mut s, 30_000);
    s.ready_poll(Readiness::Ready, &status, 30_001).unwrap();
    let out = drain(&mut s, 30_001);
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[0]["result"]["status"], status);
    assert_eq!(out[0]["done"], true);
    s.ready_poll(Readiness::Ready, &status, 30_002).unwrap();
    assert!(drain(&mut s, 30_002).is_empty());
    s.ready(id(7), Readiness::Starting, &status, 30_002)
        .unwrap();
    drain(&mut s, 30_002);
    s.ready_poll(Readiness::Starting, &status, 60_002).unwrap();
    assert_eq!(drain(&mut s, 60_002)[0]["error"]["code"], "timeout");
    s.ready(id(8), Readiness::Starting, &status, 60_003)
        .unwrap();
    drain(&mut s, 60_003);
    s.stop_commands();
    s.ready_poll(Readiness::Ready, &status, 60_004).unwrap();
    assert!(drain(&mut s, 60_004).is_empty());
}

#[test]
fn immediate_readiness_can_retry_after_full_without_registering_a_second_reply() {
    for state in [
        Readiness::Ready,
        Readiness::StorageFailed,
        Readiness::RadioFailed,
    ] {
        let mut input = [0; MAX_LINE_BYTES - 1];
        let mut s = Session::new(&mut input);
        s.session(true, 0);
        for n in 1..=OUTPUT_FRAMES as u32 {
            s.response(id(n), true, json!({}), 0).unwrap();
        }
        let next = OUTPUT_FRAMES as u32 + 1;
        assert_eq!(
            s.ready(id(next), state, &json!({}), 0),
            Err(EmitError::Full)
        );
        drain(&mut s, 0);
        s.ready(id(next), state, &json!({}), 0).unwrap();
        s.ready_poll(state, &json!({}), 0).unwrap();
        let messages = drain(&mut s, 0);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["id"], next);
        assert_ne!(messages[0]["error"]["code"], "busy");
    }
}
