mod common;
use common::{capabilities, connect_with, set_transports, status, try_connect};
use cordial_client::{
    client::{Client, Wait, validate_status},
    controller::{Command, Controller, Event, OpenOptions, Outcome, Phase, RunOptions},
};
use cordial_protocol::{
    identifiers::{ScanTransport, Transport},
    messages::{self as wire, BuildProfile, Capabilities, Capability, Empty, Status},
};
use serde_json::json;
use std::{
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};

fn until(events: &mpsc::Receiver<Event>, wanted: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if wanted(&event) {
            return event;
        }
    }
}

/// A controller over one prepared client, opened and ready.
fn ready_controller(
    client: Client,
    firmware: &common::Firmware,
    s: &Status,
) -> (Controller, mpsc::Receiver<Event>) {
    let client = Mutex::new(Some(client));
    let (tx, events) = mpsc::channel();
    let controller = Controller::with_connector(
        move |e| {
            let _ = tx.send(e);
        },
        move |_, _| Ok(client.lock().unwrap().take().unwrap()),
    );
    controller.open(
        "adapter".into(),
        OpenOptions {
            wait: Wait::timeout(Duration::from_secs(2)),
            keep_unready: false,
        },
    );
    let ready = firmware.receive();
    assert!(matches!(ready.command, wire::Command::Ready(_)));
    firmware.reply(&ready, json!({"state":"ready", "status":s}));
    let monitor = firmware.receive();
    assert!(matches!(monitor.command, wire::Command::Monitor(_)));
    firmware.reply(&monitor, json!({"enabled":true,"revision":0}));
    let devices = firmware.receive();
    assert!(matches!(devices.command, wire::Command::Devices(_)));
    firmware.reply(&devices, json!({"count":0,"revision":0}));
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    (controller, events)
}

fn run(
    controller: &Controller,
    events: &mpsc::Receiver<Event>,
    command: Command,
) -> Result<Outcome, cordial_client::controller::Failure> {
    let ticket = controller.run(command, RunOptions::default());
    match until(
        events,
        |e| matches!(e, Event::Done { ticket: t, .. } if *t == ticket),
    ) {
        Event::Done { result, .. } => result,
        _ => unreachable!(),
    }
}

#[test]
fn capabilities_precede_status_on_every_connection() {
    for _ in 0..2 {
        let (client, firmware) = common::connect();
        assert_eq!(
            firmware.log.lock().unwrap()[..3],
            [
                "adapter.capabilities",
                "adapter.status",
                "session.heartbeat"
            ]
        );
        assert_eq!(client.capabilities(), capabilities());
        client.shutdown();
    }
}

#[test]
fn independent_transport_sets_resolve_defaults_and_reject_explicit_unsupported_scans() {
    for (transports, expected) in [
        (vec![], None),
        (vec![Transport::Classic], Some(ScanTransport::Classic)),
        (vec![Transport::Ble], Some(ScanTransport::Ble)),
        (
            vec![Transport::Ble, Transport::Classic],
            Some(ScanTransport::Both),
        ),
    ] {
        let mut s = status();
        let caps = set_transports(&mut s, transports.clone());
        assert!(validate_status(&s, &caps).is_ok());
        assert_eq!(caps.scan_transport(ScanTransport::Both), expected);
        for (transport, scan) in [
            (Transport::Classic, ScanTransport::Classic),
            (Transport::Ble, ScanTransport::Ble),
        ] {
            let supported = transports.contains(&transport);
            assert_eq!(caps.scan_transport(scan).is_some(), supported);
            let command = wire::Command::Scan(wire::Scan {
                transport: scan,
                duration_ms: 1000,
            });
            assert_eq!(caps.unsupported(&command).is_none(), supported);
            if !supported {
                let (client, firmware) = connect_with(s.clone(), caps.clone());
                assert!(
                    client
                        .call(command, false, &Wait::timeout(Duration::from_secs(1)))
                        .is_err()
                );
                assert!(
                    firmware
                        .requests
                        .recv_timeout(Duration::from_millis(20))
                        .is_err(),
                    "unsupported scan reached the wire"
                );
                drop(firmware);
                client.shutdown();
            }
        }
    }
}

#[test]
fn malformed_capability_lists_and_inconsistent_status_are_rejected() {
    for (caps, why) in [
        (json!(["ble", "ble"]), "duplicate"),
        (json!(["ble", "radio"]), "unknown value"),
        (json!({"capabilities": ["ble"]}), "object instead of a list"),
        (json!("ble"), "not a list"),
    ] {
        let error = try_connect(status(), caps).err();
        assert!(error.is_some(), "{why} accepted");
    }
    // Status names pairing capacity for Classic, which is not advertised.
    let caps = Capabilities(vec![Capability::Ble]);
    assert!(validate_status(&status(), &caps).is_err());
    assert!(try_connect(status(), json!(caps)).is_err());
    // A consistent BLE-only adapter opens.
    let mut s = status();
    let caps = set_transports(&mut s, vec![Transport::Ble]);
    let (client, _firmware) = try_connect(s, json!(caps)).unwrap();
    client.shutdown();
    // Capability names use the wire protocol's snake_case spelling.
    assert_eq!(
        serde_json::to_value(capabilities()).unwrap(),
        json!(["classic", "ble", "debug", "storage_management"])
    );
}

#[test]
fn no_optional_capabilities_keep_management_and_refuse_files_and_bootloader() {
    let mut s = status();
    set_transports(&mut s, Vec::new());
    let (client, firmware) = connect_with(s.clone(), Capabilities::default());
    let (controller, events) = ready_controller(client, &firmware, &s);
    let state = controller.state().unwrap();
    assert!(state.capabilities.0.is_empty());
    assert!(Command::Status.supported(&state));
    assert!(Command::Trusted("d_mouse".into(), true).supported(&state));
    for command in [
        Command::Scan(ScanTransport::Both),
        Command::Pair("mouse".into()),
        Command::Connect("d_mouse".into()),
        Command::Bootloader,
        Command::StorageList("/".into()),
    ] {
        assert!(!command.supported(&state), "{command:?}");
        assert!(run(&controller, &events, command).is_err());
    }
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
    // Only the close's monitor shutdown follows; nothing optional was sent.
    while let Ok(request) = firmware.requests.recv_timeout(Duration::from_millis(20)) {
        assert!(
            matches!(request.command, wire::Command::Monitor(_)),
            "{request:?}"
        );
    }
}

#[test]
fn build_profile_does_not_gate_advertised_functions() {
    let mut s = status();
    s.build_profile = BuildProfile::Production;
    let (client, firmware) = connect_with(s, capabilities());
    let wait = Wait::timeout(Duration::from_secs(1));
    let request = client.start(wire::Command::Bootloader(Empty {}), false, &wait);
    assert!(request.is_ok());
    let sent = firmware.receive();
    assert!(matches!(sent.command, wire::Command::Bootloader(_)));
    drop(firmware);
    client.shutdown();
    // And development status without the capability refuses it locally.
    let (client, firmware) = connect_with(
        status(),
        Capabilities(vec![Capability::Ble, Capability::Classic]),
    );
    assert!(
        client
            .call(wire::Command::Bootloader(Empty {}), false, &wait)
            .is_err()
    );
    assert!(
        firmware
            .requests
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );
    drop(firmware);
    client.shutdown();
}

#[test]
fn capabilities_command_reports_this_connection() {
    let s = status();
    let (client, firmware) = connect_with(s.clone(), capabilities());
    let (controller, events) = ready_controller(client, &firmware, &s);
    // Status and capabilities work without further readiness; the command
    // re-reads them and checks they didn't change within the connection.
    match run(&controller, &events, Command::Capabilities) {
        Ok(Outcome::Capabilities(c)) => assert_eq!(c, capabilities()),
        other => panic!("{other:?}"),
    }
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn switching_adapters_replaces_capabilities_and_default_scan_uses_new_transports() {
    use std::collections::VecDeque;
    let combinations = [
        (vec![Transport::Ble], Some(ScanTransport::Ble)),
        (vec![Transport::Classic], Some(ScanTransport::Classic)),
        (
            vec![Transport::Classic, Transport::Ble],
            Some(ScanTransport::Both),
        ),
        (vec![], None),
    ];
    let mut clients = VecDeque::new();
    let mut firmware = Vec::new();
    for (transports, _) in &combinations {
        let mut s = status();
        let caps = set_transports(&mut s, transports.clone());
        let (client, remote) = connect_with(s.clone(), caps.clone());
        clients.push_back(client);
        firmware.push((remote, s, caps));
    }
    let clients = Mutex::new(clients);
    let (tx, events) = mpsc::channel();
    let controller = Controller::with_connector(
        move |e| {
            let _ = tx.send(e);
        },
        move |_, _| Ok(clients.lock().unwrap().pop_front().unwrap()),
    );
    for (i, ((remote, s, caps), (_, expected))) in
        firmware.iter().zip(combinations.iter()).enumerate()
    {
        let session = controller.open(
            format!("adapter-{i}"),
            OpenOptions {
                wait: Wait::timeout(Duration::from_secs(2)),
                keep_unready: false,
            },
        );
        let ready = remote.receive();
        assert!(matches!(ready.command, wire::Command::Ready(_)));
        remote.reply(&ready, json!({"state":"ready", "status":s}));
        let monitor = remote.receive();
        remote.reply(&monitor, json!({"enabled":true,"revision":0}));
        let devices = remote.receive();
        remote.reply(&devices, json!({"count":0,"revision":0}));
        until(
            &events,
            |e| matches!(e, Event::Connection { session: id, phase: Phase::Ready, .. } if *id == session),
        );
        let state = controller.state().unwrap();
        assert_eq!(state.session, session);
        assert_eq!(&state.capabilities, caps);
        let result = run(&controller, &events, Command::Scan(ScanTransport::Both));
        if let Some(expected) = expected {
            assert!(
                matches!(result, Ok(Outcome::ScanStarted { transport, .. }) if transport == *expected)
            );
            let request = remote.receive();
            assert!(
                matches!(request.command, wire::Command::Scan(ref args) if args.transport == *expected)
            );
            remote.reply(&request, json!({"count":0,"truncated":false}));
        } else {
            assert!(result.is_err());
            assert!(
                remote
                    .requests
                    .recv_timeout(Duration::from_millis(20))
                    .is_err()
            );
        }
    }
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}
