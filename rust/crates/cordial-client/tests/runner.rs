mod common;
use common::{connect, status};
use cordial_client::{
    client::Cancellation,
    runner::{Options, Script},
};
use cordial_protocol::messages::Command;
use serde_json::{Value, json};
use std::{io::Cursor, sync::Mutex, time::Duration};

#[test]
fn flag_parser_rejects_nonfinite_and_preserves_quoted_os_arguments() {
    for value in ["NaN", "inf", "-1", "3601"] {
        assert!(Options::parse(["--timeout".into(), value.into()]).is_err());
    }
    let options = Options::parse([
        "--port=arbitrary".into(),
        "--json".into(),
        "pair".into(),
        "two words".into(),
    ])
    .unwrap();
    assert!(options.json);
    assert_eq!(options.port.as_deref(), Some("arbitrary"));
    assert_eq!(options.args, ["pair", "two words"]);
}
#[test]
fn one_shot_status_skips_readiness_and_only_explicit_json_prints_envelopes() {
    for json in [false, true] {
        let (client, firmware) = connect();
        let client = Mutex::new(Some(client));
        let mut out = Vec::new();
        let mut diagnostics = Vec::new();
        Script {
            options: Options {
                port: Some("simulated".into()),
                json,
                args: vec!["adapter".into(), "status".into()],
                ..Default::default()
            },
            cancellation: Cancellation::default(),
        }
        .with_connector(
            Cursor::new(Vec::new()),
            &mut out,
            &mut diagnostics,
            move |_, _| Ok(client.lock().unwrap().take().unwrap()),
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        if json {
            assert!(
                out.lines()
                    .all(|s| serde_json::from_str::<Value>(s).is_ok())
            );
            assert!(out.contains("\"type\":\"response\""));
        } else {
            assert!(out.contains("Adapter"));
            assert!(!out.contains("\"type\""));
        }
        assert!(firmware.requests.try_recv().is_err());
        assert_eq!(*firmware.dtr.lock().unwrap(), vec![false, true, false]);
    }
}
#[test]
fn piped_commands_wait_for_readiness_and_end_at_eof_without_disconnect() {
    let (client, firmware) = connect();
    let client = Mutex::new(Some(client));
    let server = std::thread::spawn(move || {
        let request = firmware.receive();
        assert!(matches!(request.command, Command::Ready(_)));
        firmware.reply(&request, json!({"state":"ready","status":status()}));
        for _ in 0..2 {
            let request = firmware.receive();
            assert!(matches!(request.command, Command::Devices(_)));
            firmware.reply(&request, json!({"revision":0,"count":0}));
        }
        // Keeping this owner alive until DTR drops models an attached dongle.
        let end = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < end
            && !firmware
                .dtr
                .lock()
                .unwrap()
                .ends_with(&[false, true, false])
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(firmware.requests.try_recv().is_err());
    });
    let mut out = Vec::new();
    let mut diagnostics = Vec::new();
    Script {
        options: Options {
            port: Some("simulated".into()),
            ..Default::default()
        },
        cancellation: Cancellation::default(),
    }
    .with_connector(
        Cursor::new(b"device list\nadapter status\n"),
        &mut out,
        &mut diagnostics,
        move |_, _| Ok(client.lock().unwrap().take().unwrap()),
    )
    .unwrap();
    assert!(!String::from_utf8(out).unwrap().contains("\"type\""));
    server.join().unwrap();
}

#[test]
fn final_pairing_answer_waits_for_acknowledgment_before_eof() {
    let (client, firmware) = connect();
    let client = Mutex::new(Some(client));
    let server = std::thread::spawn(move || {
        let request = firmware.receive();
        assert!(matches!(request.command, Command::Ready(_)));
        firmware.reply(&request, json!({"state":"ready","status":status()}));
        let request = firmware.receive();
        assert!(matches!(request.command, Command::Devices(_)));
        firmware.reply(&request, json!({"revision":0,"count":0}));
        let scan = firmware.receive();
        assert!(matches!(scan.command, Command::Scan(_)));
        firmware.event(
            "discovery.result",
            Some(scan.id),
            json!({"candidate_id":"c_new","name":"new","kind":"unknown","transport":"ble","rssi":-20}),
        );
        firmware.reply(&scan, json!({"count":1,"truncated":false}));
        let pair = firmware.receive();
        assert!(matches!(pair.command, Command::Pair(_)));
        firmware.event("pairing.prompt",Some(pair.id),json!({"candidate_id":"c_new","prompt_id":"p1","method":"enter_passkey","expires_in_ms":10000,"value":null}));
        let answer = firmware.receive();
        assert!(matches!(answer.command, Command::PairReply(_)));
        std::thread::sleep(Duration::from_millis(100));
        firmware.reply(&answer, json!({}));
        let device = json!({"device_id":"d_new","pairing_state":"paired","name":"new","transport":"ble","roles":["keyboard"],
            "state":"connected","security":{"encrypted":true,"authenticated":false,"secure_connections":true,"key_size":16,"bonded":true},
    "enabled":true,"effective_enabled":true,"enabled_reason":null,"transport_supported":true,"validation_error":null,
    "trusted":true,"blocked":false,"reconnect":"auto","last_error":null,
            "hidpp_enabled":true,"normalization_state":"active","normalization_error":null,
            "settings_state":"ready","settings_error":null,"settings_revision":0});
        firmware.event(
            "device.paired",
            Some(pair.id),
            json!({"revision":1,"device":device}),
        );
        firmware.reply(&pair, json!({"device":device}));
        let request = firmware.receive();
        assert!(matches!(request.command, Command::Connect(_)));
        firmware.reply(&request, json!({"device":device}));
        let end = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < end
            && !firmware
                .dtr
                .lock()
                .unwrap()
                .ends_with(&[false, true, false])
        {
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let mut out = Vec::new();
    let mut diagnostics = Vec::new();
    Script {
        options: Options {
            port: Some("simulated".into()),
            args: vec!["pairing".into(), "start".into(), "new".into()],
            ..Default::default()
        },
        cancellation: Cancellation::default(),
    }
    .with_connector(
        Cursor::new(b"042731\n"),
        &mut out,
        &mut diagnostics,
        move |_, _| Ok(client.lock().unwrap().take().unwrap()),
    )
    .unwrap();
    assert!(String::from_utf8(out).unwrap().contains("Connected to new"));
    server.join().unwrap();
}

/// Runs one script against a simulated adapter whose firmware thread `serve`
/// answers requests after the handshake.
fn script(
    args: &[&str],
    input: &'static [u8],
    serve: impl FnOnce(common::Firmware) + Send + 'static,
) -> (Result<(), cordial_client::client::Error>, String) {
    let (client, firmware) = connect();
    let client = Mutex::new(Some(client));
    let server = std::thread::spawn(move || serve(firmware));
    let mut out = Vec::new();
    let mut diagnostics = Vec::new();
    let result = Script {
        options: Options {
            port: Some("simulated".into()),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
        },
        cancellation: Cancellation::default(),
    }
    .with_connector(
        Cursor::new(input),
        &mut out,
        &mut diagnostics,
        move |_, _| Ok(client.lock().unwrap().take().unwrap()),
    );
    server.join().unwrap();
    (result, String::from_utf8(out).unwrap())
}

fn serve_listing(firmware: common::Firmware) {
    let request = firmware.receive();
    assert!(
        matches!(&request.command, Command::StorageList(p) if p.path == "/"),
        "{:?}",
        request.command
    );
    for row in [
        json!({"name":"device.json","type":"file","size":800}),
        json!({"name":"bonds","type":"directory","size":0}),
    ] {
        firmware.send(&cordial_protocol::messages::Message::<Value, ()>::success(
            request.id, row, false,
        ));
    }
    firmware.reply(&request, json!({"count":2}));
}

#[test]
fn storage_list_works_one_shot_and_interactively_without_readiness() {
    // One shot: no readiness wait; rows print as they arrive.
    let (result, out) = script(&["storage", "ls", "/"], b"", serve_listing);
    result.unwrap();
    assert_eq!(out, "file\t800\tdevice.json\ndirectory\t0\tbonds\n");
    // Piped: the same command after readiness.
    let (result, out) = script(&[], b"storage ls /\n", |firmware| {
        let request = firmware.receive();
        assert!(matches!(request.command, Command::Ready(_)));
        firmware.reply(&request, json!({"state":"ready","status":status()}));
        let request = firmware.receive();
        assert!(matches!(request.command, Command::Devices(_)));
        firmware.reply(&request, json!({"revision":0,"count":0}));
        serve_listing(firmware);
    });
    result.unwrap();
    assert!(
        out.contains("file\t800\tdevice.json\ndirectory\t0\tbonds\n"),
        "{out}"
    );
}

#[test]
fn storage_get_writes_exact_bytes_and_never_replaces_a_local_file() {
    use base64::Engine;
    let root = std::env::temp_dir().join(format!("cordial-runner-get-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let local = root.join("copy.json");
    let target: &'static str = Box::leak(local.display().to_string().into_boxed_str());
    let (result, out) = script(
        &["storage", "get", "/device.json", target],
        b"",
        |firmware| {
            let request = firmware.receive();
            assert!(
                matches!(&request.command, Command::StorageRead(p) if p.path == "/device.json")
            );
            let data = base64::engine::general_purpose::STANDARD.encode(b"hello");
            firmware.send(&cordial_protocol::messages::Message::<Value, ()>::success(
                request.id,
                json!({"offset":0,"data":data}),
                false,
            ));
            firmware.reply(&request, json!({"bytes":5}));
        },
    );
    result.unwrap();
    assert!(out.contains("Saved /device.json to"), "{out}");
    assert_eq!(std::fs::read(&local).unwrap(), b"hello");
    // The existing file is kept and nothing is requested.
    let (result, _) = script(
        &["storage", "get", "/device.json", target],
        b"",
        |firmware| {
            assert!(
                firmware
                    .requests
                    .recv_timeout(Duration::from_millis(100))
                    .is_err()
            );
        },
    );
    assert_eq!(result.unwrap_err().message, "the local file already exists");
    assert_eq!(std::fs::read(&local).unwrap(), b"hello");
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        1,
        "no temporary left"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
