mod common;

use common::{Dongle, Plan};
use cordial_cli::{
    controller::Cancellation,
    error::Error,
    runner::{Options, Script},
};
use cordial_protocol::{self as p, ErrorCode};
use serde_json::Value;
use std::{fs, io::Cursor};

/// Runs a script against the simulated Dongle and returns its result, output and
/// diagnostics.
fn script(
    dongle: &Dongle,
    args: &[&str],
    json: bool,
    input: &'static [u8],
) -> (Result<(), Error>, String, String) {
    let mut out = Vec::new();
    let mut diagnostics = Vec::new();
    let result = Script {
        options: Options {
            port: Some("simulated".into()),
            json,
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
        },
        cancellation: Cancellation::default(),
    }
    .with_connector(
        Cursor::new(input),
        &mut out,
        &mut diagnostics,
        dongle.connector(),
    );
    (
        result,
        String::from_utf8(out).unwrap(),
        String::from_utf8(diagnostics).unwrap(),
    )
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
    let dongle = Dongle::with(|sim| sim.status = common::status(false));
    let (result, out, _) = script(&dongle, &["adapter", "status"], false, b"");
    result.unwrap();
    assert!(
        out.contains("Adapter ADAPTER01\n  Port: simulated"),
        "{out}"
    );
    assert!(out.contains("  Firmware: 1.2.3"), "{out}");
    assert!(out.contains("  Ready: no"), "{out}");
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
        "disabled: the device is disabled; enable it before connecting"
    );
    let dongle = Dongle::with(|sim| {
        sim.refuse.insert("set_adapter", ErrorCode::StorageFailed);
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
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn production_firmware_refuses_development_commands_locally() {
    let dongle = Dongle::with(|sim| sim.status.info.clear());
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
        sim.refuse.insert("connect_device", ErrorCode::Unsupported);
    });
    let (result, _, _) = script(&dongle, &["device", "connect", "Old Keyboard"], false, b"");
    assert_eq!(
        result.unwrap_err().message,
        "Bluetooth Classic is disabled on the adapter"
    );
    assert!(dongle.sent().contains(&"connect_device"));
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
profile rule remap 'Mouse Fix' 09:04 09:01\n\
profile rule scale 'Mouse Fix' 01:38 -1/1\n\
profile rule list 'Mouse Fix'\n\
profile list\n\
profile list --after 0\n\
profile list --after 2\n\
device set 'Office Mouse' profiles Work 'Mouse Fix'\n\
adapter set interface via on Games\n\
device get 1\n\
adapter status\n\
profile rule forget 2 01:38\n\
profile show 2\n",
    );
    result.unwrap();
    assert!(err.is_empty(), "{err}");
    for line in [
        "Created profile Work (1).",
        "Copied profile to Mouse Fix (2).",
        "Created profile Games (3).",
        "Saved in Mouse Fix: 09:04 remap 09:01@01:02\n",
        "Saved in Mouse Fix: 01:38 scale -1/1",
        "Rules of Mouse Fix\n  01:38 scale -1/1\n  09:04 remap 09:01@01:02\n",
        "Profiles\n  1\tnone\tWork\n  2\tmouse\tMouse Fix\n  3\tnone\tGames\n",
        "Profiles\n  1\tnone\tWork\n  2\tmouse\tMouse Fix\nNext page: profile list --after 2\n",
        "Profiles\n  3\tnone\tGames\n",
        "Profiles of Office Mouse set to Work (1), Mouse Fix (2).",
        "VIA on, profile Games (3).",
        "  Profiles: Work (1), Mouse Fix (2)",
        "  Profile Memory: 1.0 KiB of 8.0 KiB in use\n  Profiles Per Device: 3\n  VIA: on, profile Games (3)\n  Vial: off, profile none",
        "Profile Mouse Fix\n  ID: 2\n  Roles: Mouse",
        "Mouse Fix has no rule for 01:38.",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
    assert!(
        out.contains("[CHG] Profile 2 09:04 remap 09:01@01:02"),
        "{out}"
    );
    assert!(out.contains("[DEL] Profile 2 01:38"), "{out}");
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
fn json_prints_every_page_of_a_listing() {
    let dongle = Dongle::with(|sim| {
        sim.devices
            .push(common::device(5, "Pad", p::Transport::Ble));
    });
    let (result, out, _) = script(&dongle, &["device", "list"], true, b"");
    result.unwrap();
    let pages: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| !v["response"]["devices"].is_null())
        .collect();
    assert_eq!(pages.len(), 2, "{out}");
    let entries = |page: &Value| {
        page["response"]["devices"]["entries"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(entries(&pages[0]), 2);
    assert_eq!(entries(&pages[1]), 1);
    assert_eq!(pages[1]["response"]["devices"]["end"], true);
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
        &["profile", "rule", "scale", "1", "07:04", "2/1"],
        false,
        b"",
    );
    assert_eq!(
        result.unwrap_err().message,
        "this adapter can't scale 07:04"
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
