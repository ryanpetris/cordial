use cordial_protocol::{self as p, frame::Decoder};
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn arguments() -> Vec<String> {
    [
        "--port",
        "missing-adapter-fixture",
        "proxy",
        "--protocol",
        &p::CURRENT_PROTOCOL_VERSION.to_string(),
        "--schema",
        p::OPERATIONAL_SCHEMA_SHA256,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn failure(args: &[String], expected: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cordial"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("proxy did not exit");
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert!(!status.success());
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    let mut decoder = Decoder::new(Some(4096));
    let frames: Vec<_> = bytes
        .into_iter()
        .filter_map(|b| decoder.push(b).map(|r| r.unwrap().to_vec()))
        .collect();
    assert_eq!(frames.len(), 1);
    let startup: Value = serde_json::from_slice(&frames[0]).unwrap();
    assert_eq!(startup["kind"], "cordial-proxy-startup");
    assert_eq!(startup["protocol"], p::CURRENT_PROTOCOL_VERSION);
    assert_eq!(startup["schema"], p::OPERATIONAL_SCHEMA_SHA256);
    assert_eq!(startup["result"], expected);
    assert!(startup["nativeHello"].is_null());
}

#[test]
fn client_version_and_schema_are_checked_before_opening_the_port() {
    let mut args = arguments();
    args[4] = (p::CURRENT_PROTOCOL_VERSION + 1).to_string();
    failure(&args, "incompatible-client");
    args = arguments();
    args[6] = "0".repeat(64);
    failure(&args, "incompatible-client");
    failure(&arguments(), "open-failed");
}

#[test]
fn malformed_missing_duplicate_and_unknown_proxy_arguments_are_rejected() {
    let mut missing_port = arguments();
    missing_port[1].clear();
    failure(&missing_port, "incompatible-client");
    for value in ["0", "-1", "1.0", "4294967296", "", "garbage"] {
        let mut args = arguments();
        args[4] = value.into();
        failure(&args, "incompatible-client");
    }
    for value in ["", "abc", &"G".repeat(64)] {
        let mut args = arguments();
        args[6] = value.into();
        failure(&args, "incompatible-client");
    }
    for tail in [
        vec!["--protocol", "1"],
        vec!["--unknown"],
        vec!["--port", "another-fixture"],
    ] {
        let mut args = arguments();
        args.extend(tail.into_iter().map(str::to_owned));
        failure(&args, "incompatible-client");
    }
    let mut args = arguments();
    args.pop();
    failure(&args, "incompatible-client");
}
