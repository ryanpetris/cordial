mod common;
use common::{Firmware, connect, status};
use cordial_client::{
    client::Wait,
    controller::{Command, Controller, Event, OpenOptions, Outcome, Phase, RunOptions},
};
use cordial_protocol::{
    identifiers::*,
    messages::{self, Message},
    settings::SettingKey,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Mutex,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

fn device() -> Value {
    json!({"device_id":"d_test","pairing_state":"paired","name":"Test keyboard","transport":"ble","roles":["keyboard"],
    "state":"connected","security":{"encrypted":true,"authenticated":false,"secure_connections":true,"key_size":16,"bonded":true},
    "enabled":true,"effective_enabled":true,"enabled_reason":null,"transport_supported":true,"validation_error":null,
    "trusted":true,"blocked":false,"reconnect":"auto","last_error":null,
    "hidpp_enabled":true,"normalization_state":"active","normalization_error":null,
    "settings_state":"ready","settings_error":null,"settings_revision":0})
}
fn setting(value: bool) -> Value {
    json!({"key":"backlight.enabled","type":"bool","writable":true,
    "feature":8192,"feature_version":0,"scope":"device","choices":[],"min":null,"max":null,"step":null,
    "managed":true,"desired":false,"observed":value,"fresh":true,"state":"changed_on_device",
    "observed_at_ms":1,"observation_source":"read","error":null})
}
fn setup() -> (Controller, Receiver<Event>, Firmware) {
    let (client, firmware) = connect();
    let client = Mutex::new(Some(client));
    let (sink, events) = mpsc::channel();
    let controller = Controller::with_connector(
        move |event| {
            let _ = sink.send(event);
        },
        move |_, _| Ok(client.lock().unwrap().take().unwrap()),
    );
    controller.open(
        "simulated".into(),
        OpenOptions {
            wait: Wait::timeout(Duration::from_secs(2)),
            keep_unready: true,
        },
    );
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Ready(_)));
    firmware.send(&Message::<Value, ()>::success(
        request.id,
        json!({"state":"initializing"}),
        false,
    ));
    until(&events, |event| {
        matches!(
            event,
            Event::Connection {
                phase: Phase::Waiting,
                ..
            }
        )
    });
    assert!(controller.state().unwrap().waiting);
    firmware.reply(&request, json!({"state":"ready","status":status()}));
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Devices(_)));
    firmware.send(&Message::<Value, ()>::success(
        request.id,
        json!({"revision":0,"device":device()}),
        false,
    ));
    firmware.reply(&request, json!({"revision":0,"count":1}));
    until(&events, |event| {
        matches!(
            event,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    let state = controller.state().unwrap();
    assert!(state.current && state.ready && state.monitor);
    (controller, events, firmware)
}
fn until(events: &Receiver<Event>, mut wanted: impl FnMut(&Event) -> bool) -> Event {
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
#[allow(clippy::result_large_err)]
fn done(
    events: &Receiver<Event>,
    ticket: u64,
) -> Result<Outcome, cordial_client::controller::Failure> {
    let Event::Done { result, .. } = until(
        events,
        |event| matches!(event,Event::Done{ticket:t,..} if *t==ticket),
    ) else {
        unreachable!()
    };
    result
}
#[test]
fn readiness_scan_pair_and_bond_survives_failed_connect() {
    let (controller, events, firmware) = setup();
    let scan = controller.run(Command::Scan(ScanTransport::Ble), RunOptions::default());
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Scan(_)));
    assert!(matches!(
        done(&events, scan).unwrap(),
        Outcome::ScanStarted { .. }
    ));
    firmware.event(
        "discovery.result",
        Some(request.id),
        json!({"candidate_id":"c_new","name":"New keyboard","kind":"keyboard","transport":"ble","rssi":-25}),
    );
    until(
        &events,
        |event| matches!(event,Event::Notice{notice:cordial_client::controller::Notice::Message{envelope,..},..} if envelope.event()==Some("discovery.result")),
    );
    let pair = controller.run(Command::Pair("New keyboard".into()), RunOptions::default());
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Pair(_)));
    firmware.event("pairing.prompt",Some(request.id),json!({"candidate_id":"c_new","prompt_id":"p1","method":"enter_passkey","expires_in_ms":10000,"value":null}));
    until(
        &events,
        |event| matches!(event,Event::Notice{notice:cordial_client::controller::Notice::Message{envelope,..},..} if envelope.event()==Some("pairing.prompt")),
    );
    assert_eq!(
        controller.state().unwrap().auth.unwrap().prompt.prompt_id,
        "p1"
    );
    let mut new = device();
    new["device_id"] = json!("d_new");
    new["name"] = json!("New keyboard");
    firmware.event(
        "device.paired",
        Some(request.id),
        json!({"revision":1,"device":new}),
    );
    firmware.reply(&request, json!({"device":new}));
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Connect(_)));
    firmware.send(&json!({"v":1,"type":"response","id":request.id,"ok":false,"done":true,"error":{"code":"timeout"}}));
    let failure = done(&events, pair).unwrap_err();
    assert_eq!(failure.bonded, Some(DeviceId("d_new".into())));
    assert!(controller.state().unwrap().auth.is_none());
    controller.close();
    until(&events, |event| matches!(event, Event::Closed));
    assert_eq!(*firmware.dtr.lock().unwrap(), vec![false, true, false]);
}
#[test]
fn settings_snapshot_keeps_newer_notification_and_unpair_removes_cache() {
    let (controller, events, firmware) = setup();
    let ticket = controller.run(Command::Settings("d_test".into()), RunOptions::default());
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Settings(_)));
    firmware.send(&Message::<Value, ()>::success(
        request.id,
        json!({"revision":0,"device_id":"d_test","setting":setting(false)}),
        false,
    ));
    firmware.event(
        "hidpp.setting.changed",
        None,
        json!({"revision":1,"device_id":"d_test","setting":setting(true)}),
    );
    firmware.reply(&request,json!({"revision":0,"device_id":"d_test","count":1,"settings_state":"ready","settings_error":null}));
    done(&events, ticket).unwrap();
    let state = controller.state().unwrap();
    let cache = &state.settings[&DeviceId("d_test".into())];
    assert_eq!(
        cache.settings[0].observed,
        cordial_protocol::settings::SettingValue::Bool(true)
    );
    firmware.event(
        "device.unpaired",
        None,
        json!({"revision":2,"device_id":"d_test"}),
    );
    until(
        &events,
        |event| matches!(event,Event::Notice{notice:cordial_client::controller::Notice::Message{envelope,..},..} if envelope.event()==Some("device.unpaired")),
    );
    assert!(controller.state().unwrap().settings.is_empty());
    controller.close();
    until(&events, |event| matches!(event, Event::Closed));
}
#[test]
fn forget_setting_sends_no_device_read_and_command_done_follows_cache_update() {
    let (controller, events, firmware) = setup();
    let ticket = controller.run(
        Command::SettingForget("d_test".into(), SettingKey::BacklightEnabled),
        RunOptions::default(),
    );
    let request = firmware.receive();
    assert!(matches!(
        request.command,
        messages::Command::SettingsForget(_)
    ));
    let mut row = setting(false);
    row["managed"] = json!(false);
    row["desired"] = Value::Null;
    row["state"] = json!("unmanaged");
    firmware.reply(
        &request,
        json!({"revision":0,"device_id":"d_test","setting":row}),
    );
    assert!(matches!(
        done(&events, ticket).unwrap(),
        Outcome::Setting { .. }
    ));
    controller.close();
    until(&events, |event| matches!(event, Event::Closed));
}

#[test]
fn monitor_off_racing_heartbeat_expiry_stays_off() {
    use cordial_client::controller::Notice;
    let (client, firmware) = common::manual_monitor();
    let client = Mutex::new(Some(client));
    let (tx, events) = mpsc::channel();
    let controller = Controller::with_connector(
        move |event| {
            let _ = tx.send(event);
        },
        move |_, _| Ok(client.lock().unwrap().take().unwrap()),
    );
    controller.open(
        "simulated".into(),
        OpenOptions {
            wait: Wait::timeout(Duration::from_secs(2)),
            keep_unready: true,
        },
    );
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Ready(_)));
    firmware.reply(&request, json!({"state":"ready","status":status()}));
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Monitor(_)));
    firmware.reply(&request, json!({"enabled":true,"revision":0}));
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Devices(_)));
    firmware.reply(&request, json!({"revision":0,"count":0}));
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    let ticket = controller.run(Command::Monitor(false), RunOptions::default());
    let off = firmware.receive();
    assert!(matches!(off.command,messages::Command::Monitor(ref a) if !a.enabled));
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if matches!(
            event,
            Event::Notice {
                notice: Notice::MonitorExpired,
                ..
            }
        ) {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(50));
    firmware.reply(&off, json!({"enabled":false,"revision":0}));
    done(&events, ticket).unwrap();
    let deadline = Instant::now() + Duration::from_millis(200);
    while let Ok(request) = firmware
        .requests
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        assert!(
            matches!(request.command, messages::Command::Devices(_)),
            "unexpected renewal: {request:?}"
        );
        firmware.reply(&request, json!({"revision":0,"count":0}));
    }
    assert!(!controller.state().unwrap().monitor);
    controller.close();
    let off = firmware.receive();
    assert!(matches!(off.command,messages::Command::Monitor(ref a) if !a.enabled));
    firmware.reply(&off, json!({"enabled":false,"revision":0}));
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn forgetting_with_monitoring_off_clears_settings_and_keeps_candidates() {
    let (controller, events, firmware) = setup();
    let ticket = controller.run(Command::Settings("d_test".into()), RunOptions::default());
    let request = firmware.receive();
    firmware.send(&Message::<Value, ()>::success(
        request.id,
        json!({"revision":0,"device_id":"d_test","setting":setting(false)}),
        false,
    ));
    firmware.reply(&request,json!({"revision":0,"device_id":"d_test","count":1,"settings_state":"ready","settings_error":null}));
    done(&events, ticket).unwrap();
    let ticket = controller.run(Command::Scan(ScanTransport::Ble), RunOptions::default());
    let scan = firmware.receive();
    done(&events, ticket).unwrap();
    firmware.event(
        "discovery.result",
        Some(scan.id),
        json!({"candidate_id":"c_known","name":"Test keyboard","kind":"keyboard","transport":"ble","rssi":-25}),
    );
    until(
        &events,
        |event| matches!(event,Event::Notice{notice:cordial_client::controller::Notice::Message{envelope,..},..} if envelope.event()==Some("discovery.result")),
    );
    let ticket = controller.run(Command::Monitor(false), RunOptions::default());
    done(&events, ticket).unwrap();
    let ticket = controller.run(Command::Remove("d_test".into()), RunOptions::default());
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Unpair(_)));
    firmware.reply(&request, json!({"device_id":"d_test","removed":true}));
    done(&events, ticket).unwrap();
    let state = controller.state().unwrap();
    assert!(state.settings.is_empty());
    assert_eq!(state.candidates.len(), 1);
    let ticket = controller.run(Command::Pair("c_known".into()), RunOptions::default());
    loop {
        let request = firmware.receive();
        if matches!(request.command, messages::Command::Devices(_)) {
            firmware.reply(&request, json!({"revision":1,"count":0}));
            continue;
        }
        assert!(
            matches!(request.command, messages::Command::Pair(_)),
            "{request:?}"
        );
        firmware.send(&json!({"v":1,"type":"response","id":request.id,"ok":false,"done":true,"error":{"code":"cancelled"}}));
        break;
    }
    assert!(done(&events, ticket).is_err());
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn pair_uses_nearby_entry_and_never_saved_entry_or_connect_fallback() {
    let (controller, events, firmware) = setup();
    let ticket = controller.run(Command::Pair("d_test".into()), RunOptions::default());
    assert!(done(&events, ticket).is_err());
    assert!(firmware.requests.try_recv().is_err());
    let ticket = controller.run(Command::Scan(ScanTransport::Ble), RunOptions::default());
    let scan = firmware.receive();
    done(&events, ticket).unwrap();
    firmware.event(
        "discovery.result",
        Some(scan.id),
        json!({"candidate_id":"c_known","name":"Test keyboard","kind":"keyboard","transport":"ble","rssi":-25}),
    );
    until(&events, |e| {
        matches!(e,Event::Notice{notice:cordial_client::controller::Notice::Message{envelope,..},..}
        if envelope.event()==Some("discovery.result"))
    });
    let ticket = controller.run(Command::Pair("d_test".into()), RunOptions::default());
    assert!(done(&events, ticket).is_err());
    assert!(firmware.requests.try_recv().is_err());
    // A Nearby name remains selectable even when the Saved entry has the same name.
    let ticket = controller.run(Command::Pair("Test keyboard".into()), RunOptions::default());
    let request = firmware.receive();
    assert!(matches!(&request.command,messages::Command::Pair(p) if p.candidate_id.0=="c_known"));
    assert_eq!(
        controller
            .state()
            .unwrap()
            .pending
            .iter()
            .find(|p| p.command == "pairing.start")
            .unwrap()
            .device_id,
        None,
        "no Nearby entry is associated with a saved device"
    );
    firmware.send(
        &json!({"v":1,"type":"response","id":request.id,"ok":false,"done":true,
        "error":{"code":"capacity","details":{"reason":"storage_full"}}}),
    );
    let failure = done(&events, ticket).unwrap_err();
    assert!(failure.bonded.is_none());
    assert!(
        cordial_client::ui::text::error_line(&failure.error)
            .contains("no room for another paired device"),
        "{:?}",
        failure.error
    );
    assert!(firmware.requests.try_recv().is_err());
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn pairing_saved_disabled_reports_it_and_never_connects() {
    let (controller, events, firmware) = setup();
    let ticket = controller.run(Command::Scan(ScanTransport::Ble), RunOptions::default());
    let scan = firmware.receive();
    done(&events, ticket).unwrap();
    firmware.event(
        "discovery.result",
        Some(scan.id),
        json!({"candidate_id":"c_new","name":"New keyboard","kind":"keyboard","transport":"ble","rssi":-25}),
    );
    until(&events, |e| {
        matches!(e,Event::Notice{notice:cordial_client::controller::Notice::Message{envelope,..},..}
        if envelope.event()==Some("discovery.result"))
    });
    let ticket = controller.run(Command::Pair("c_new".into()), RunOptions::default());
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Pair(_)));
    let mut new = device();
    new["device_id"] = json!("d_new");
    new["state"] = json!("disconnected");
    new["security"] = Value::Null;
    new["enabled"] = json!(false);
    new["effective_enabled"] = json!(false);
    new["enabled_reason"] = json!("disabled");
    firmware.reply(&request, json!({"device":new}));
    let outcome = done(&events, ticket).unwrap();
    let text = cordial_client::ui::text::outcome(&Command::Pair("c_new".into()), &outcome, None);
    assert_eq!(text, "Paired and saved New keyboard; enable it to connect.");
    // No Connect follows; only background status/device refreshes may.
    while let Ok(request) = firmware.requests.recv_timeout(Duration::from_millis(200)) {
        assert!(
            matches!(
                request.command,
                messages::Command::Status(_) | messages::Command::Devices(_)
            ),
            "{request:?}"
        );
        if matches!(request.command, messages::Command::Status(_)) {
            firmware.reply(&request, serde_json::to_value(status()).unwrap());
        } else {
            firmware.reply(&request, json!({"revision":0,"count":0}));
        }
    }
    let ticket = controller.run(
        Command::Enabled("d_new".into(), true),
        RunOptions::default(),
    );
    let request = firmware.receive();
    assert!(
        matches!(&request.command, messages::Command::DeviceEnabled(d) if d.device_id.0 == "d_new" && d.enabled)
    );
    firmware.send(
        &json!({"v":1,"type":"response","id":request.id,"ok":false,"done":true,
        "error":{"code":"capacity","details":{"reason":"enabled_full"}}}),
    );
    let failure = done(&events, ticket).unwrap_err();
    let line = cordial_client::ui::text::error_line(&failure.error);
    assert!(
        line.contains("disable another device first") && !line.contains("reason"),
        "{line}"
    );
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn readiness_arriving_after_the_wait_ended_leaves_the_session_unready() {
    use cordial_client::client::Cancellation;
    let (client, firmware) = common::manual_monitor();
    let client = Mutex::new(Some(client));
    let (sink, events) = mpsc::channel();
    let controller = Controller::with_connector(
        move |event| {
            let _ = sink.send(event);
        },
        move |_, _| Ok(client.lock().unwrap().take().unwrap()),
    );
    let opening = Cancellation::default();
    controller.open(
        "simulated".into(),
        OpenOptions {
            wait: Wait {
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                cancellation: opening.clone(),
            },
            keep_unready: true,
        },
    );
    let ready = firmware.receive();
    assert!(matches!(ready.command, messages::Command::Ready(_)));
    // The local wait ends first, as when it times out; the firmware request
    // cannot be cancelled and completes later.
    opening.cancel();
    until(&events, |event| {
        matches!(
            event,
            Event::Connection {
                phase: Phase::Failed { open: true, .. },
                ..
            }
        )
    });
    firmware.reply(&ready, json!({"state":"ready","status":status()}));
    // A later status round trip proves the late report was processed.
    let ticket = controller.run(Command::Status, RunOptions::default());
    done(&events, ticket).unwrap();
    let state = controller.state().unwrap();
    assert!(!state.ready && state.ready_error.is_some() && state.available);
    // No startup snapshot or monitoring starts, and ready-only commands refuse.
    let ticket = controller.run(Command::Devices, RunOptions::default());
    assert!(done(&events, ticket).is_err());
    assert!(
        firmware
            .requests
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "nothing follows a late readiness report"
    );
    controller.close();
    until(&events, |event| matches!(event, Event::Closed));
}
