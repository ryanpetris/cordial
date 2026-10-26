mod common;

use common::{Dongle, Plan, refusal};
use cordial_cli::{
    controller::Cancellation,
    error::Error,
    migration::Engine,
    runner::{Options, Script},
};
use cordial_protocol::{self as p, ErrorCode, ResourceType};
use serde_json::Value;
use std::{
    fs,
    io::{self, Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Runs a script against the simulated Dongle and returns its result, output and
/// diagnostics.
fn script(
    dongle: &Dongle,
    args: &[&str],
    json: bool,
    input: &'static [u8],
) -> (Result<(), Error>, String, String) {
    run(dongle, args, json, Duration::ZERO, Cursor::new(input))
}

/// `script` with a timeout and any input.
fn run(
    dongle: &Dongle,
    args: &[&str],
    json: bool,
    timeout: Duration,
    input: impl Read + Send + 'static,
) -> (Result<(), Error>, String, String) {
    migrating(
        dongle,
        args,
        json,
        timeout,
        input,
        Cancellation::default(),
        Arc::new(|_, _, _, _| panic!("no migration is expected")),
    )
}

/// `run` with a migration engine.
fn migrating(
    dongle: &Dongle,
    args: &[&str],
    json: bool,
    timeout: Duration,
    input: impl Read + Send + 'static,
    cancellation: Cancellation,
    engine: Engine,
) -> (Result<(), Error>, String, String) {
    let mut out = Vec::new();
    let mut diagnostics = Vec::new();
    let result = Script {
        options: Options {
            port: Some("simulated".into()),
            json,
            timeout,
            args: args.iter().map(|s| (*s).to_owned()).collect(),
        },
        cancellation,
        engine,
    }
    .with_connector(input, &mut out, &mut diagnostics, dongle.connector());
    (
        result,
        String::from_utf8(out).unwrap(),
        String::from_utf8(diagnostics).unwrap(),
    )
}

/// Input that records whether anything read it.
struct Watched(Arc<AtomicBool>);

impl Read for Watched {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        self.0.store(true, Ordering::Release);
        Ok(0)
    }
}

#[test]
fn a_one_shot_command_leaves_standard_input_unread() {
    let dongle = Dongle::default();
    let read = Arc::new(AtomicBool::new(false));
    let (result, out, _) = run(
        &dongle,
        &["device", "list"],
        false,
        Duration::ZERO,
        Watched(read.clone()),
    );
    result.unwrap();
    assert!(out.contains("Office Mouse"), "{out}");
    assert!(!read.load(Ordering::Acquire));
}

#[test]
fn a_one_shot_scan_lasts_the_timeout_and_ends_normally() {
    let dongle = Dongle::with(|sim| sim.scan_ends = false);
    let started = Instant::now();
    let (result, out, _) = run(
        &dongle,
        &["scan", "start"],
        false,
        Duration::from_millis(1500),
        Cursor::new(b""),
    );
    result.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(1500));
    assert_eq!(
        out.matches("Discovery finished: 1 candidate.").count(),
        1,
        "{out}"
    );
    let sim = dongle.0.lock().unwrap();
    assert!(sim.log.iter().any(|c| matches!(
        c,
        p::request::Command::StartScan(s) if s.seconds == 2
    )));
    assert!(
        sim.log
            .iter()
            .any(|c| matches!(c, p::request::Command::StopScan(_)))
    );
}

#[test]
fn a_one_shot_pair_scans_only_until_it_finds_its_device() {
    let dongle = Dongle::with(|sim| sim.scan_ends = false);
    let (result, out, _) = run(
        &dongle,
        &["pair", "start", "New Keyboard"],
        false,
        Duration::from_secs(3),
        Cursor::new(b""),
    );
    result.unwrap();
    assert!(out.contains("Paired and saved New Keyboard."), "{out}");
    let sent = dongle.sent();
    let stop = sent.iter().position(|c| *c == "stop_scan").unwrap();
    let pair = sent.iter().position(|c| *c == "start_pairing").unwrap();
    assert!(stop < pair, "{sent:?}");
}

#[test]
fn scripts_run_direct_commands_before_the_adapter_is_ready() {
    let dongle = Dongle::with(|sim| {
        sim.status = common::status(false);
        sim.files.insert("/".into(), Vec::new());
    });
    let (result, out, diagnostics) = script(&dongle, &[], false, b"adapter status\nfile list /\n");
    result.unwrap();
    assert!(out.contains("  Ready: no"), "{out}");
    assert!(!out.contains("Waiting") && !diagnostics.contains("Waiting"));
    assert!(dongle.sent().contains(&"list_files"));
    assert!(!dongle.sent().contains(&"list_devices"));

    // Other commands wait for readiness, which is told as progress.
    let dongle = Dongle::with(|sim| sim.status = common::status(false));
    let later = dongle.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        let mut sim = later.0.lock().unwrap();
        sim.status.ready = true;
        let status = sim.status.clone();
        sim.event(p::event::Kind::Adapter(status));
    });
    let (result, out, diagnostics) = script(&dongle, &[], false, b"adapter status\ndevice list\n");
    result.unwrap();
    assert!(out.contains("  Ready: no"), "{out}");
    assert!(out.contains("Office Mouse"), "{out}");
    assert!(!out.contains("Waiting"), "{out}");
    assert!(
        diagnostics.contains("Waiting for adapter readiness"),
        "{diagnostics}"
    );
}

#[test]
fn json_prints_responses_and_events_in_the_order_they_arrived() {
    for _ in 0..50 {
        let dongle = Dongle::default();
        let (result, out, _) = script(&dongle, &["scan", "start", "ble", "5"], true, b"");
        result.unwrap();
        let lines: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let found = lines
            .iter()
            .position(|l| !l["event"]["scan_found"].is_null())
            .unwrap_or_else(|| panic!("{out}"));
        let started = lines
            .iter()
            .rposition(|l| l.get("response").is_some())
            .unwrap();
        assert!(started < found, "{out}");
        assert!(
            lines.iter().any(|l| !l["event"]["scan_done"].is_null()),
            "{out}"
        );
    }
}

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
fn one_shot_status_skips_readiness_and_json_prints_protobuf_messages() {
    let dongle = Dongle::with(|sim| {
        let mut status = common::status(false);
        status
            .resources
            .push(common::resource(ResourceType::Storage, None, Some(65_536)));
        sim.status = status;
    });
    let (result, out, _) = script(&dongle, &["adapter", "status"], false, b"");
    result.unwrap();
    assert!(
        out.contains("Adapter ADAPTER01\n  Port: simulated"),
        "{out}"
    );
    assert!(out.contains("  Firmware: 1.2.3"), "{out}");
    assert!(out.contains("  Storage Format: 2\n"), "{out}");
    assert!(out.contains("  Storage: Up to 64.0 KiB\n"), "{out}");
    assert!(out.contains("  Ready: no"), "{out}");
    // The saved devices aren't listed, so they aren't counted.
    assert!(!out.contains("Saved Devices"), "{out}");
    assert!(!dongle.sent().contains(&"list_devices"));

    let (result, out, _) = script(&dongle, &["adapter", "status"], true, b"");
    result.unwrap();
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{out}");
    assert_eq!(lines[0]["response"]["status"]["name"], "Desk");
    assert_eq!(lines[0]["response"]["status"]["id"], "ADAPTER01");
    let status = &lines[0]["response"]["status"];
    assert_eq!(status["information"]["storage_format"], 2, "{status}");
    let storage = status["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["type"] == "RESOURCE_TYPE_STORAGE")
        .unwrap();
    assert_eq!(storage["limit"], 65_536);
    assert!(storage.get("count").is_none(), "{storage}");
}

#[test]
fn piped_commands_wait_for_readiness_and_end_at_eof() {
    let dongle = Dongle::default();
    let (result, out, _) = script(
        &dongle,
        &[],
        false,
        b"device list Enabled\ndevice get 'Office Mouse'\n",
    );
    result.unwrap();
    assert!(
        out.contains("1  Office Mouse  ble  disconnected  trusted bluetooth=enabled"),
        "{out}"
    );
    assert!(!out.contains("2  Old Keyboard"), "{out}");
    assert!(
        out.contains("Device 1 (ble)\n  Name: Office Mouse"),
        "{out}"
    );
    assert!(out.contains("  HID++ Status: Waiting to Connect"), "{out}");
    assert!(!out.contains("Special-Key"), "{out}");
}

#[test]
fn one_shot_scan_prints_one_summary() {
    let dongle = Dongle::default();
    let (result, out, _) = script(&dongle, &["scan", "start", "ble", "5"], false, b"");
    result.unwrap();
    assert_eq!(
        out.matches("Discovery finished: 1 candidate.").count(),
        1,
        "{out}"
    );
    assert!(
        out.contains("[NEW] 1  New Keyboard  ble  candidate"),
        "{out}"
    );
    let sim = dongle.0.lock().unwrap();
    assert!(sim.log.iter().any(|c| matches!(
        c,
        p::request::Command::StartScan(s)
            if s.transports == [p::Transport::Ble as i32] && s.seconds == 5
    )));
}

#[test]
fn a_failed_scan_keeps_its_candidates_and_reports_every_error() {
    let dongle = Dongle::with(|sim| {
        sim.scan_error = Some(p::Error {
            code: ErrorCode::Internal as i32,
            reason: p::ErrorReason::Radio as i32,
            outcome: p::ErrorOutcome::Partial as i32,
            related: vec![p::Error {
                code: ErrorCode::NoCapacity as i32,
                reason: p::ErrorReason::ScanCandidates as i32,
                outcome: p::ErrorOutcome::Partial as i32,
                related: Vec::new(),
            }],
        });
    });
    let (result, out, _) = script(&dongle, &["scan", "start", "ble", "5"], false, b"");
    result.unwrap();
    assert!(
        out.contains(
            "Discovery finished: 1 candidate. Error: internal: the adapter hit an unexpected failure; involves the adapter's Bluetooth radio; it partly completed; also (no_capacity: the adapter had no room to list every device it found; it partly completed)"
        ),
        "{out}"
    );
    assert!(
        out.contains("[NEW] 1  New Keyboard  ble  candidate"),
        "{out}"
    );
}

#[test]
fn a_failed_pairing_reports_an_unknown_bond_and_its_cleanup_failure() {
    let dongle = Dongle::with(|sim| {
        sim.plan = Plan::EnterCode("012345".into());
        sim.pairing_error = Some(p::Error {
            code: ErrorCode::AuthFailed as i32,
            outcome: p::ErrorOutcome::Unknown as i32,
            related: vec![p::Error {
                code: ErrorCode::StorageFailed as i32,
                reason: p::ErrorReason::Storage as i32,
                outcome: p::ErrorOutcome::NoEffect as i32,
                related: Vec::new(),
            }],
            ..Default::default()
        });
    });
    let (result, _, _) = script(
        &dongle,
        &["pair", "start", "New Keyboard"],
        false,
        b"999999\n",
    );
    assert_eq!(
        result.unwrap_err().message,
        "auth_failed: Bluetooth authentication with this device failed; outcome unknown: the adapter couldn't confirm what took effect; check the current state before trying again; also (storage_failed: the adapter couldn't read or write its saved data)"
    );
    // Nothing is tried again.
    assert_eq!(
        dongle
            .sent()
            .iter()
            .filter(|c| **c == "start_pairing")
            .count(),
        1
    );
}

#[test]
fn one_shot_pair_scans_for_its_device_and_takes_the_passkey_from_input() {
    let dongle = Dongle::with(|sim| sim.plan = Plan::EnterCode("012345".into()));
    let (result, out, _) = script(
        &dongle,
        &["pair", "start", "New Keyboard"],
        false,
        b"012345\n",
    );
    result.unwrap();
    assert!(
        out.contains("[pair 1] Enter on this computer: enter_passkey"),
        "{out}"
    );
    assert!(out.contains("Paired and saved New Keyboard."), "{out}");
    let sim = dongle.0.lock().unwrap();
    assert!(sim.log.iter().any(|c| matches!(
        c,
        p::request::Command::AcceptPrompt(a) if a.value == "012345"
    )));
}

#[test]
fn refusals_name_their_code_and_explanation() {
    let dongle = Dongle::default();
    let (result, _, _) = script(&dongle, &["device", "connect", "Old Keyboard"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "disabled: the device is disabled. Enable it before connecting"
    );
    let dongle = Dongle::with(|sim| {
        sim.refuse
            .insert("set_adapter", refusal(ErrorCode::StorageFailed));
    });
    let (result, _, _) = script(&dongle, &["adapter", "set", "name", "Desk 2"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "storage_failed: the adapter couldn't read or write its saved data"
    );
}

#[test]
fn file_get_writes_the_file_and_never_replaces_one() {
    let dir = std::env::temp_dir().join(format!("cordial-runner-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let local = dir.join("copy.bin");
    let local_arg: &'static str = Box::leak(local.display().to_string().into_boxed_str());
    let dongle = Dongle::with(|sim| {
        sim.data.insert("/a.bin".into(), vec![1, 2, 3]);
        sim.files.insert(
            "/".into(),
            vec![p::FileEntry {
                name: "a.bin".into(),
                directory: false,
                size: 3,
            }],
        );
    });
    let (result, out, _) = script(&dongle, &["file", "list", "/"], false, b"");
    result.unwrap();
    assert_eq!(out, "file\t3\ta.bin\n");
    let (result, out, _) = script(&dongle, &["file", "get", "/a.bin", local_arg], false, b"");
    result.unwrap();
    assert!(out.contains("(3 bytes)"), "{out}");
    assert_eq!(fs::read(&local).unwrap(), [1, 2, 3]);
    let (result, _, _) = script(&dongle, &["file", "get", "/a.bin", local_arg], false, b"");
    assert_eq!(result.unwrap_err().message, "the local file already exists");
    fs::remove_file(&local).unwrap();
    let (result, out, diagnostics) =
        script(&dongle, &["file", "get", "/a.bin", local_arg], true, b"");
    result.unwrap();
    assert!(!out.contains("AQID") && !out.contains("\"file\""), "{out}");
    assert!(diagnostics.contains("(3 bytes)"), "{diagnostics}");
    assert_eq!(fs::read(&local).unwrap(), [1, 2, 3]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn file_get_saves_records_as_json_unless_raw() {
    use prost::Message;
    let dir = std::env::temp_dir().join(format!("cordial-records-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let arg = |name: &str| -> &'static str {
        Box::leak(dir.join(name).display().to_string().into_boxed_str())
    };
    let profile = p::storage::Profile {
        format: cordial_protocol::storage::CURRENT_FORMAT,
        name: "Work".into(),
        roles: vec![p::storage::Role::Keyboard.into()],
    }
    .encode_to_vec();
    let dongle = Dongle::with(|sim| {
        sim.data
            .insert("/profiles/3/profile.pb".into(), profile.clone());
        sim.data.insert("/devices/1/device.pb".into(), vec![0xff]);
    });
    // A record is saved as JSON under its name with .json.
    let (result, out, _) = script(
        &dongle,
        &["file", "get", "/profiles/3/profile.pb", arg("profile.pb")],
        false,
        b"",
    );
    result.unwrap();
    assert!(out.contains("as JSON to"), "{out}");
    assert!(!dir.join("profile.pb").exists());
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("profile.json")).unwrap()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"format": p::storage::CURRENT_FORMAT, "name": "Work", "roles": ["ROLE_KEYBOARD"]})
    );
    // An existing JSON file is not replaced.
    let (result, _, _) = script(
        &dongle,
        &["file", "get", "/profiles/3/profile.pb", arg("profile.pb")],
        false,
        b"",
    );
    assert_eq!(result.unwrap_err().message, "the local file already exists");
    // --raw keeps the bytes under the name given.
    let (result, out, _) = script(
        &dongle,
        &[
            "file",
            "get",
            "--raw",
            "/profiles/3/profile.pb",
            arg("profile.pb"),
        ],
        false,
        b"",
    );
    result.unwrap();
    assert!(
        !out.contains("JSON") && !out.contains("converting"),
        "{out}"
    );
    assert_eq!(fs::read(dir.join("profile.pb")).unwrap(), profile);
    // A file that doesn't decode keeps its bytes and original name, and the result says so.
    let (result, out, _) = script(
        &dongle,
        &["file", "get", "/devices/1/device.pb", arg("device.json")],
        false,
        b"",
    );
    result.unwrap();
    assert!(
        out.contains("without converting it: the file doesn't decode as its record."),
        "{out}"
    );
    assert_eq!(fs::read(dir.join("device.pb")).unwrap(), [0xff]);
    assert!(!dir.join("device.json").exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn production_firmware_refuses_development_commands_locally() {
    let dongle = Dongle::with(|sim| {
        sim.status.information.as_mut().unwrap().development_build = Some(false);
    });
    let (result, _, _) = script(&dongle, &["file", "list", "/"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "this adapter doesn't offer file access"
    );
    assert!(!dongle.sent().contains(&"list_files"));
}

#[test]
fn transports_are_shown_and_set_per_transport() {
    let dongle = Dongle::default();
    let (result, out, _) = script(&dongle, &["adapter", "status"], false, b"");
    result.unwrap();
    assert!(out.contains("  Bluetooth Classic: enabled\n"), "{out}");
    assert!(out.contains("  Bluetooth LE: enabled\n"), "{out}");
    let (result, out, _) = script(
        &dongle,
        &["adapter", "set", "transport", "classic", "enabled", "off"],
        false,
        b"",
    );
    result.unwrap();
    assert!(out.contains("Bluetooth Classic disabled."), "{out}");
    assert!(dongle.0.lock().unwrap().log.iter().any(|c| matches!(
        c,
        p::request::Command::SetAdapter(s)
            if s.transports == [p::TransportUpdate {
                transport: p::Transport::Classic as i32,
                enabled: Some(false),
            }] && s.name.is_none() && s.platform.is_none()
    )));
    let (result, out, _) = script(&dongle, &["adapter", "status"], false, b"");
    result.unwrap();
    assert!(out.contains("  Bluetooth Classic: disabled\n"), "{out}");
    assert!(out.contains("  Bluetooth LE: enabled\n"), "{out}");

    // A BLE-only adapter refuses a Classic change before sending it.
    let dongle = Dongle::with(|sim| {
        sim.status
            .transports
            .retain(|t| t.transport != p::Transport::Classic as i32)
    });
    let (result, out, _) = script(&dongle, &["adapter", "status"], false, b"");
    result.unwrap();
    assert!(!out.contains("Bluetooth Classic"), "{out}");
    let (result, _, _) = script(
        &dongle,
        &["adapter", "set", "transport", "classic", "enabled", "on"],
        false,
        b"",
    );
    assert_eq!(
        result.unwrap_err().message,
        "unsupported: the adapter or device doesn't support this"
    );
    assert!(!dongle.sent().contains(&"set_adapter"));
}

#[test]
fn a_one_shot_timeout_leaves_a_sent_save_unconfirmed() {
    let dongle = Dongle::with(|sim| {
        sim.slow.insert("set_adapter", Duration::from_secs(3));
        sim.slow.insert("list_files", Duration::from_secs(3));
    });
    let timeout = Duration::from_millis(500);
    for json in [false, true] {
        let started = Instant::now();
        let (result, out, _) = run(
            &dongle,
            &["adapter", "set", "name", "Office"],
            json,
            timeout,
            Cursor::new(b""),
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            result.unwrap_err().message,
            "the adapter couldn't confirm whether the change was saved"
        );
        assert!(!out.contains("renamed"), "{out}");
    }
    assert_eq!(dongle.0.lock().unwrap().status.name, "Office");
    let sent = dongle.sent();
    assert_eq!(sent.iter().filter(|c| **c == "set_adapter").count(), 2);
    let (result, _, _) = run(
        &dongle,
        &["file", "list", "/"],
        false,
        timeout,
        Cursor::new(b""),
    );
    assert_eq!(result.unwrap_err().message, "operation timed out");
}

#[test]
fn renaming_and_resetting_the_adapter_name_the_result() {
    let dongle = Dongle::default();
    let (result, out, _) = script(&dongle, &["adapter", "set", "name", "Office"], false, b"");
    result.unwrap();
    assert!(out.contains("Adapter renamed to Office."), "{out}");
    // The adapter saves the name trimmed, and no adapter event follows an unchanged name.
    let (result, out, _) = script(&dongle, &["adapter", "set", "name", " Office "], false, b"");
    result.unwrap();
    assert!(out.contains("Adapter renamed to Office."), "{out}");
    let (result, _, _) = script(&dongle, &["adapter", "set", "name", "  "], false, b"");
    assert_eq!(result.unwrap_err().message, "invalid adapter name");
    // The default name is known only from the adapter event that follows the reset.
    let (result, out, _) = script(&dongle, &["adapter", "reset", "name"], false, b"");
    result.unwrap();
    assert!(out.contains("Adapter name reset to Cordial."), "{out}");
}

#[test]
fn work_on_a_disabled_transport_names_it() {
    let dongle = Dongle::with(|sim| {
        let d = sim.devices.iter_mut().find(|d| d.id == 2).unwrap();
        d.enabled = true;
        d.inactive = None;
        sim.set_transport(p::Transport::Classic, false);
    });
    let (result, out, _) = script(&dongle, &["device", "list"], false, b"");
    result.unwrap();
    assert!(out.contains("inactive=transport_disabled"), "{out}");
    let (result, out, _) = script(&dongle, &["device", "get", "Old Keyboard"], false, b"");
    result.unwrap();
    assert!(
        out.contains("enabled, not active: Bluetooth Classic is disabled on the adapter"),
        "{out}"
    );
    let (result, _, _) = script(&dongle, &["device", "connect", "Old Keyboard"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "Bluetooth Classic is disabled on the adapter"
    );
    assert!(!dongle.sent().contains(&"connect_device"));
    let (result, _, _) = script(&dongle, &["scan", "start", "classic"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "Bluetooth Classic is disabled on the adapter"
    );
    assert!(!dongle.sent().contains(&"start_scan"));
    // A scan naming no transport scans only the enabled ones.
    let (result, _, _) = script(&dongle, &["scan", "start"], false, b"");
    result.unwrap();
    assert!(dongle.0.lock().unwrap().log.iter().any(|c| matches!(
        c,
        p::request::Command::StartScan(s) if s.transports == [p::Transport::Ble as i32]
    )));

    // A refusal the view couldn't predict is worded the same.
    let dongle = Dongle::with(|sim| {
        sim.status.transports[0].enabled = Some(false);
        let d = sim.devices.iter_mut().find(|d| d.id == 2).unwrap();
        d.enabled = true;
        d.inactive = None;
        sim.refuse
            .insert("connect_device", refusal(ErrorCode::Unsupported));
    });
    let (result, _, _) = script(&dongle, &["device", "connect", "Old Keyboard"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "Bluetooth Classic is disabled on the adapter"
    );
    assert!(dongle.sent().contains(&"connect_device"));
}

#[test]
fn a_refusal_with_more_to_report_on_a_disabled_transport_keeps_its_details() {
    let dongle = Dongle::with(|sim| {
        sim.status.transports[0].enabled = Some(false);
        let d = sim.devices.iter_mut().find(|d| d.id == 2).unwrap();
        d.enabled = true;
        d.inactive = None;
        sim.refuse.insert(
            "connect_device",
            p::Error {
                outcome: p::ErrorOutcome::Partial as i32,
                ..refusal(ErrorCode::Unsupported)
            },
        );
    });
    let (result, _, _) = script(&dongle, &["device", "connect", "Old Keyboard"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "unsupported: the adapter or device doesn't support this; it partly completed"
    );
}

#[test]
fn pairing_on_a_disabled_transport_names_it() {
    // A candidate found before its transport was disabled isn't paired.
    let dongle = Dongle::with(|sim| {
        sim.candidates[0].transport = p::Transport::Classic as i32;
    });
    let (result, out, _) = script(
        &dongle,
        &[],
        false,
        b"scan start\nadapter set transport classic enabled off\npair start 1\n",
    );
    assert!(out.contains("[NEW] 1  New Keyboard"), "{out}");
    assert_eq!(
        result.unwrap_err().message,
        "Bluetooth Classic is disabled on the adapter"
    );
    assert!(!dongle.sent().contains(&"start_pairing"));
}

#[test]
fn a_transport_without_its_enabled_field_is_enabled() {
    let dongle = Dongle::with(|sim| {
        for t in &mut sim.status.transports {
            t.enabled = None;
        }
    });
    let (result, _, _) = script(&dongle, &["scan", "start", "classic"], false, b"");
    result.unwrap();
    let (result, out, _) = script(
        &dongle,
        &["adapter", "set", "transport", "ble", "enabled", "off"],
        false,
        b"",
    );
    result.unwrap();
    assert!(out.contains("Bluetooth LE disabled."), "{out}");
}

fn with_profiles(sim: &mut common::Sim) {
    common::enable_profiles(&mut sim.status);
}

#[test]
fn profiles_layers_and_interfaces_from_the_shell() {
    let dongle = Dongle::with(with_profiles);
    let (result, out, err) = script(
        &dongle,
        &[],
        false,
        b"profile create Work\n\
profile copy Work 'Mouse Fix'\n\
profile create Games\n\
profile rule remap 'Mouse Fix' 09:04@01:02 09:01\n\
profile rule scale 'Mouse Fix' 01:38@01:02 -1/1\n\
profile rule list 'Mouse Fix'\n\
profile list\n\
device set 'Office Mouse' profiles Work 'Mouse Fix'\n\
adapter set interface via on Games\n\
device get 1\n\
adapter status\n\
profile rule forget 2 01:38@01:02\n\
profile show 2\n",
    );
    result.unwrap();
    assert!(err.is_empty(), "{err}");
    for line in [
        "Created profile Work (1).",
        "Copied profile to Mouse Fix (2).",
        "Created profile Games (3).",
        "Saved in Mouse Fix: 09:04@01:02 remap 09:01@01:02\n",
        "Saved in Mouse Fix: 01:38@01:02 scale -1/1",
        "Rules of Mouse Fix\n  01:38@01:02 scale -1/1\n  09:04@01:02 remap 09:01@01:02\n",
        "Profiles\n  1\tnone\tWork\n  2\tmouse\tMouse Fix\n  3\tnone\tGames\n",
        "Profiles of Office Mouse set to Work (1), Mouse Fix (2).",
        "VIA on, profile Games (3).",
        "  Profiles: Work (1), Mouse Fix (2)",
        "  Profile Memory: 1.0 KiB of 8.0 KiB in use\n  Profiles Per Device: 3\n  VIA: on, profile Games (3)\n  Vial: off, profile none",
        "Profile Mouse Fix\n  ID: 2\n  Roles: Mouse",
        "Mouse Fix has no rule for 01:38@01:02.",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
    let sim = dongle.0.lock().unwrap();
    assert_eq!(
        sim.devices[0].profiles,
        Some(p::ProfileLayers {
            profiles: vec![1, 2]
        })
    );
    // An interface change and its profile go out together in one request.
    assert!(sim.log.iter().any(|c| matches!(c,
        p::request::Command::SetAdapter(s)
            if s.configuration_interfaces == [p::ConfigurationInterfaceUpdate {
                interface: p::ConfigurationInterface::Via as i32,
                enabled: Some(true),
                profile: Some(3),
            }] && s.name.is_none() && s.transports.is_empty())));
}

#[test]
fn json_prints_a_listing_as_one_response() {
    let dongle = Dongle::with(|sim| {
        sim.devices
            .push(common::device(5, "Pad", p::Transport::Ble));
    });
    let (result, out, _) = script(&dongle, &["device", "list"], true, b"");
    result.unwrap();
    let listings: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| !v["response"]["devices"].is_null())
        .collect();
    assert_eq!(listings.len(), 1, "{out}");
    let entries = listings[0]["response"]["devices"]["entries"]
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), 3);
}

#[test]
fn deleting_a_profile_in_use_is_refused_before_sending() {
    let dongle = Dongle::with(|sim| {
        with_profiles(sim);
        let work = sim.add_profile("Work", Vec::new());
        let games = sim.add_profile("Games", Vec::new());
        sim.status.configuration_interfaces[1].profile = games;
        sim.devices[0].profiles = Some(p::ProfileLayers {
            profiles: vec![work],
        });
    });
    let (result, _, _) = script(&dongle, &["profile", "delete", "Work"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "Office Mouse is using this profile. Remove it from Office Mouse's profiles first"
    );
    let (result, _, _) = script(&dongle, &["profile", "delete", "2"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "Vial is using this profile. Pick a different profile for Vial first"
    );
    assert!(!dongle.sent().contains(&"delete_profile"));
    // An enabled interface can't lose its profile; a disabled one can.
    let (result, _, _) = script(
        &dongle,
        &[],
        false,
        b"adapter set interface vial on\nadapter reset interface vial profile\n",
    );
    assert_eq!(
        result.unwrap_err().message,
        "choose a profile for Vial before turning it on"
    );
    let (result, out, _) = script(
        &dongle,
        &[],
        false,
        b"adapter set interface vial off\nadapter reset interface vial profile\n\
device set 1 profiles none\nprofile delete Games\nprofile delete 1\n",
    );
    result.unwrap();
    for line in [
        "Vial off, profile none.",
        "Profiles of Office Mouse set to none.",
        "Deleted profile Games.",
        "Deleted profile Work.",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
}

#[test]
fn ambiguous_names_and_unsupported_rules_are_refused() {
    let dongle = Dongle::with(|sim| {
        with_profiles(sim);
        sim.add_profile("Work", Vec::new());
        sim.add_profile("Work", Vec::new());
    });
    let (result, _, _) = script(&dongle, &["profile", "delete", "Work"], false, b"");
    assert!(result.unwrap_err().to_string().contains("ambiguous"));
    let (result, _, _) = script(
        &dongle,
        &["profile", "rule", "scale", "1", "07:04@01:06", "2/1"],
        false,
        b"",
    );
    assert_eq!(
        result.unwrap_err().message,
        "this adapter can't scale 07:04@01:06"
    );
    let (result, _, _) = script(
        &dongle,
        &["device", "set", "1", "profiles", "1", "2", "1", "2"],
        false,
        b"",
    );
    assert_eq!(
        result.unwrap_err().message,
        "a device can use at most 3 profiles"
    );
    assert!(!dongle.sent().contains(&"set_profile_rules"));
    assert!(!dongle.sent().contains(&"set_device"));
    assert!(!dongle.sent().contains(&"delete_profile"));
}

#[test]
fn profile_commands_need_firmware_with_profiles() {
    let dongle = Dongle::default();
    for args in [
        &["profile", "create", "Work"][..],
        &["device", "set", "1", "profiles", "none"],
        &["adapter", "set", "interface", "via", "off"],
    ] {
        let (result, _, _) = script(&dongle, args, false, b"");
        assert!(
            result.unwrap_err().to_string().contains("support profiles"),
            "{args:?}"
        );
    }
    assert!(!dongle.sent().contains(&"create_profile"));
    assert!(!dongle.sent().contains(&"list_profiles"));
    assert!(!dongle.sent().contains(&"set_device"));
}

fn needs_migration(sim: &mut common::Sim) {
    sim.status.ready = false;
    sim.status.information.as_mut().unwrap().storage_format = Some(1);
    sim.status.errors = vec![p::Error {
        code: ErrorCode::StorageMigrationRequired as i32,
        reason: p::ErrorReason::Storage as i32,
        outcome: p::ErrorOutcome::NoEffect as i32,
        ..Default::default()
    }];
}

/// The adapter after a migration and restart.
fn restarted(sim: &mut common::Sim) {
    sim.status.ready = true;
    sim.status.errors.clear();
    sim.status.information.as_mut().unwrap().storage_format = Some(2);
}

/// An engine that finds every port released, then migrates the simulated adapter after `delay`
/// as the worker would, or fails.
fn engine(dongle: &Dongle, delay: Duration, fail: bool) -> (Engine, Arc<AtomicBool>) {
    let (dongle, called) = (dongle.clone(), Arc::new(AtomicBool::new(false)));
    let seen = called.clone();
    let engine: Engine = Arc::new(move |id, port, cancelled, report| {
        assert_eq!(dongle.handles(), 0, "a port was still open");
        assert_eq!((id, port), ("ADAPTER01", Some("simulated")));
        seen.store(true, Ordering::SeqCst);
        report(p::migration::MigrationEvent {
            event: Some(p::migration::migration_event::Event::Waiting(
                Default::default(),
            )),
        });
        let end = Instant::now() + delay;
        while Instant::now() < end {
            if cancelled.load(Ordering::Acquire) {
                return Ok(cordial_client::migration::Outcome::Cancelled);
            }
            thread::sleep(Duration::from_millis(5));
        }
        if fail {
            return Err(cordial_client::migration::Error {
                reason: "the adapter refused a write".into(),
                outcome_unknown: true,
            });
        }
        restarted(&mut dongle.0.lock().unwrap());
        Ok(cordial_client::migration::Outcome::Success)
    });
    (engine, called)
}

#[test]
fn a_one_shot_command_migrates_the_adapter_first_and_reopens_it() {
    let dongle = Dongle::with(|sim| {
        needs_migration(sim);
        sim.files.insert("/".into(), Vec::new());
    });
    // The wait outlasts the timeout: a migration has no time limit.
    let (engine, called) = engine(&dongle, Duration::from_millis(300), false);
    let (result, out, diagnostics) = migrating(
        &dongle,
        &["file", "list", "/"],
        false,
        Duration::from_millis(200),
        io::empty(),
        Cancellation::default(),
        engine,
    );
    assert!(called.load(Ordering::SeqCst));
    // The file listing runs only on the migrated adapter's new session.
    result.unwrap();
    assert_eq!(dongle.sent()[..2], ["get_status", "get_status"]);
    assert!(dongle.sent().contains(&"list_files"));
    assert_eq!(dongle.sessions(), 2);
    assert!(diagnostics.contains("Migrating the adapter's saved data"));
    assert!(diagnostics.contains("Waiting for the adapter"));
    assert!(diagnostics.contains("Migrated the adapter's saved data."));
    assert!(!out.contains("Migrat"));
}

#[test]
fn migrate_reports_its_result_or_that_nothing_needed_it() {
    let dongle = Dongle::with(needs_migration);
    let (engine, _) = engine(&dongle, Duration::ZERO, false);
    let (result, out, diagnostics) = migrating(
        &dongle,
        &["migrate"],
        false,
        Duration::ZERO,
        io::empty(),
        Cancellation::default(),
        engine,
    );
    result.unwrap();
    assert_eq!(out, "Migrated the adapter's saved data.\n");
    assert!(!diagnostics.contains("Migrated"));

    let dongle = Dongle::default();
    let (result, out, _) = script(&dongle, &["migrate"], false, b"");
    result.unwrap();
    assert_eq!(out, "The adapter's saved data doesn't need migration.\n");
    assert_eq!(dongle.sent()[0], "get_status");
}

#[test]
fn a_failed_migration_ends_the_script_without_reopening() {
    let dongle = Dongle::with(needs_migration);
    let (engine, _) = engine(&dongle, Duration::ZERO, true);
    let (result, _, _) = migrating(
        &dongle,
        &["adapter", "bootloader"],
        false,
        Duration::ZERO,
        io::empty(),
        Cancellation::default(),
        engine,
    );
    let e = result.unwrap_err();
    assert_eq!(
        e.message,
        "Cordial couldn't confirm whether the adapter saved its migrated data: the adapter refused a write"
    );
    assert_eq!(dongle.sent(), ["get_status"]);
    assert_eq!(dongle.sessions(), 1);
}

/// Runs a script whose connector opens `connect(port, opens so far)`, with an engine that
/// migrates each adapter the dongles name and records the IDs it migrated.
fn adapters(
    args: &[&str],
    input: &'static [u8],
    dongles: Vec<Dongle>,
    connect: impl Fn(&str, usize) -> usize + Send + Sync + 'static,
    migrates: bool,
) -> (Result<(), Error>, String, Vec<String>) {
    let migrated = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (seen, all) = (migrated.clone(), dongles.clone());
    let engine: Engine = Arc::new(move |id, _, _, _| {
        let dongle = all
            .iter()
            .find(|d| d.0.lock().unwrap().status.id == id)
            .unwrap();
        assert_eq!(dongle.handles(), 0, "a port was still open");
        seen.lock().unwrap().push(id.to_owned());
        if migrates {
            restarted(&mut dongle.0.lock().unwrap());
        }
        Ok(cordial_client::migration::Outcome::Success)
    });
    let opens = std::sync::atomic::AtomicUsize::new(0);
    let mut out = Vec::new();
    let result = Script {
        options: Options {
            port: Some("simulated".into()),
            json: false,
            timeout: Duration::ZERO,
            args: args.iter().map(|s| (*s).to_owned()).collect(),
        },
        cancellation: Cancellation::default(),
        engine,
    }
    .with_connector(
        Cursor::new(input),
        &mut out,
        &mut Vec::new(),
        move |port, handler| {
            let n = opens.fetch_add(1, Ordering::SeqCst);
            dongles[connect(port, n)].connect(handler)
        },
    );
    let migrated = migrated.lock().unwrap().clone();
    (result, String::from_utf8(out).unwrap(), migrated)
}

fn second(sim: &mut common::Sim) {
    needs_migration(sim);
    sim.status.id = "ADAPTER02".into();
    sim.files.insert("/".into(), Vec::new());
}

#[test]
fn a_script_migrates_each_adapter_it_selects() {
    let (a, b) = (Dongle::with(needs_migration), Dongle::with(second));
    let (result, _, migrated) = adapters(
        &[],
        b"adapter select other\nfile list /\n",
        vec![a.clone(), b.clone()],
        |port, _| usize::from(port == "other"),
        true,
    );
    result.unwrap();
    assert_eq!(migrated, ["ADAPTER01", "ADAPTER02"]);
    assert!(b.sent().contains(&"list_files"));
}

#[test]
fn an_adapter_that_still_needs_migration_is_not_migrated_again() {
    let a = Dongle::with(needs_migration);
    let (result, _, migrated) = adapters(
        &["adapter", "status"],
        b"",
        vec![a.clone()],
        |_, _| 0,
        false,
    );
    assert!(result.unwrap_err().message.contains("must be migrated"));
    assert_eq!(migrated, ["ADAPTER01"]);
    assert_eq!(a.sent(), ["get_status", "get_status"]);
}

#[test]
fn another_adapter_on_the_migrated_adapters_port_receives_only_status() {
    let a = Dongle::with(needs_migration);
    let other = Dongle::with(|sim| sim.status.id = "ADAPTER02".into());
    let (result, _, migrated) = adapters(
        &["file", "list", "/"],
        b"",
        vec![a.clone(), other.clone()],
        |_, n| usize::from(n > 0),
        true,
    );
    assert!(result.unwrap_err().message.contains("another adapter"));
    assert_eq!(migrated, ["ADAPTER01"]);
    assert_eq!(other.sent(), ["get_status"]);
    assert_eq!(other.handles(), 0);
}

#[test]
fn ctrl_c_cancels_the_migration_and_waits_for_the_worker() {
    let dongle = Dongle::with(needs_migration);
    let (engine, called) = engine(&dongle, Duration::from_secs(60), false);
    let cancellation = Cancellation::default();
    let signal = cancellation.clone();
    let watcher = called.clone();
    thread::spawn(move || {
        while !watcher.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }
        signal.cancel();
    });
    let started = Instant::now();
    let (result, _, _) = migrating(
        &dongle,
        &["adapter", "status"],
        false,
        Duration::ZERO,
        io::empty(),
        cancellation,
        engine,
    );
    assert_eq!(result.unwrap_err().message, "interrupted");
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(dongle.sessions(), 1);
}

#[test]
fn an_unsupported_adapter_takes_only_status() {
    let dongle = Dongle::with(|sim| {
        sim.status
            .information
            .as_mut()
            .unwrap()
            .target_storage_format = Some(3);
    });
    for args in [&["file", "list", "/"][..], &["adapter", "restart"]] {
        let (result, _, _) = script(&dongle, args, false, b"");
        assert!(result.unwrap_err().message.contains("Update Cordial"));
    }
    assert_eq!(dongle.sent(), ["get_status", "get_status"]);
}

#[test]
fn an_adapter_without_a_target_never_starts_migration_or_sends_commands() {
    for marker in [false, true] {
        let dongle = Dongle::with(|sim| {
            if marker {
                needs_migration(sim);
            }
            sim.status.ready = true;
            let information = sim.status.information.as_mut().unwrap();
            information.storage_format = Some(2);
            information.target_storage_format = None;
        });
        let (worker, called) = engine(&dongle, Duration::ZERO, false);
        for args in [
            &["device", "list"][..],
            &["file", "list", "/"],
            &["adapter", "restart"],
            &["migrate"],
        ] {
            let (result, _, _) = migrating(
                &dongle,
                args,
                false,
                Duration::ZERO,
                io::empty(),
                Cancellation::default(),
                worker.clone(),
            );
            assert!(result.unwrap_err().message.contains("firmware"));
        }
        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(dongle.sent(), ["get_status"; 4]);
        assert_eq!(dongle.handles(), 0);
    }
}

#[test]
fn a_blocked_adapter_reports_its_error_and_takes_only_status_and_restart() {
    for current in [None, Some(1)] {
        let dongle = Dongle::with(|sim| {
            needs_migration(sim);
            sim.status.information.as_mut().unwrap().storage_format = current;
            sim.status.errors.push(refusal(ErrorCode::StorageFailed));
            sim.files.insert("/".into(), Vec::new());
        });
        let (worker, called) = engine(&dongle, Duration::ZERO, false);
        for args in [
            &["device", "list"][..],
            &["file", "list", "/"],
            &["migrate"],
        ] {
            let (result, _, _) = migrating(
                &dongle,
                args,
                false,
                Duration::ZERO,
                io::empty(),
                Cancellation::default(),
                worker.clone(),
            );
            let e = result.unwrap_err().message;
            let plain = "the adapter couldn't read or write its saved data";
            // A refused direct command names the code, as other adapter refusals do.
            let wanted = if args[0] == "file" {
                format!("storage_failed: {plain}")
            } else {
                plain.into()
            };
            assert_eq!(e, wanted, "{args:?}");
        }
        assert!(!called.load(Ordering::SeqCst));
        let (result, out, _) = script(&dongle, &["adapter", "restart"], false, b"");
        result.unwrap();
        assert_eq!(out, "The adapter is restarting.\n");
        assert!(!dongle.sent().contains(&"list_files"));
        assert_eq!(dongle.sent().last(), Some(&"restart"));
    }
}

#[test]
fn restart_runs_without_migration_or_readiness() {
    let dongle = Dongle::with(needs_migration);
    let (worker, called) = engine(&dongle, Duration::ZERO, false);
    let (result, out, _) = migrating(
        &dongle,
        &["adapter", "restart"],
        false,
        Duration::ZERO,
        io::empty(),
        Cancellation::default(),
        worker,
    );
    result.unwrap();
    assert_eq!(out, "The adapter is restarting.\n");
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(dongle.sent(), ["get_status", "restart"]);

    // Without the marker the adapter never migrates, even while it's unready.
    let dongle = Dongle::with(|sim| {
        sim.status.ready = false;
        sim.status.errors = vec![refusal(ErrorCode::NotReady)];
        sim.status.errors[0].reason = p::ErrorReason::Storage as i32;
    });
    let (worker, called) = engine(&dongle, Duration::ZERO, false);
    let (result, _, _) = migrating(
        &dongle,
        &["adapter", "restart"],
        false,
        Duration::ZERO,
        io::empty(),
        Cancellation::default(),
        worker,
    );
    result.unwrap();
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(dongle.sent(), ["get_status", "restart"]);
}

#[test]
fn an_adapter_that_needs_a_newer_cordial_is_refused_before_any_request() {
    let dongle = Dongle::with(|sim| {
        sim.hello.protocol_version = Some(p::CURRENT_PROTOCOL_VERSION + 1);
    });
    for (args, input) in [
        (&["adapter", "set", "name", "Office"][..], &b""[..]),
        (&["adapter", "status"][..], b""),
        (&[][..], b"adapter set name Office\n"),
    ] {
        let (result, out, _) = script(&dongle, args, false, input);
        assert_eq!(
            result.unwrap_err().message,
            "this adapter requires a newer version of Cordial. Update Cordial to connect",
            "{args:?}"
        );
        assert!(out.is_empty(), "{out}");
    }
    assert!(dongle.sent().is_empty());
}

#[test]
fn an_adapter_that_refuses_is_never_migrated() {
    let dongle = Dongle::with(|sim| {
        needs_migration(sim);
        sim.hello.protocol_version = Some(p::CURRENT_PROTOCOL_VERSION + 1);
    });
    let (worker, called) = engine(&dongle, Duration::ZERO, false);
    for args in [&["migrate"][..], &["file", "list", "/"]] {
        let (result, _, _) = migrating(
            &dongle,
            args,
            false,
            Duration::ZERO,
            io::empty(),
            Cancellation::default(),
            worker.clone(),
        );
        assert_eq!(
            result.unwrap_err().message,
            "this adapter requires a newer version of Cordial. Update Cordial to connect",
            "{args:?}"
        );
    }
    assert!(!called.load(Ordering::SeqCst));
    assert!(dongle.sent().is_empty());
}
