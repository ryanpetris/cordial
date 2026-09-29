mod common;
use common::{Firmware, connect_info, status};
use cordial_client::{
    client::Wait,
    controller::{Command, Controller, Event, Notice, OpenOptions, Outcome, Phase, RunOptions},
};
use cordial_protocol::{
    identifiers::DeviceId,
    info::{InfoField, InfoKey},
    messages::{self, Message, Request},
    settings::SettingValue,
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
    json!({"device_id":"d_test","pairing_state":"paired","name":"Test mouse","transport":"ble","roles":["mouse"],
    "state":"connected","security":null,
    "enabled":true,"effective_enabled":true,"enabled_reason":null,"transport_supported":true,"validation_error":null,
    "trusted":true,"blocked":false,"reconnect":"auto","last_error":null,
    "hidpp_enabled":false,"normalization_state":"off","normalization_error":null,
    "settings_state":"off","settings_error":null,"settings_revision":0})
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
fn seen(events: &Receiver<Event>, name: &str) {
    until(
        events,
        |e| matches!(e, Event::Notice { notice: Notice::Message { envelope, .. }, .. } if envelope.event() == Some(name)),
    );
}
fn field(key: &str, instance: u8, value: Value) -> Value {
    json!({"key":key,"instance":instance,"value":value,"available":true,"fresh":true})
}
fn cleared(key: &str, instance: u8) -> Value {
    json!({"key":key,"instance":instance,"value":null,"available":false,"fresh":false})
}
fn snapshot(revision: u64, fields: Vec<Value>) -> Value {
    json!({"revision":revision,"device_id":"d_test","fields":fields})
}
fn reply_devices(firmware: &Firmware, request: &Request, revision: u64) {
    assert!(matches!(request.command, messages::Command::Devices(_)));
    firmware.send(&Message::<Value, ()>::success(
        request.id,
        json!({"revision":revision,"device":device()}),
        false,
    ));
    firmware.reply(request, json!({"revision":revision,"count":1}));
}
fn info_request(firmware: &Firmware) -> Request {
    let request = firmware.receive();
    assert!(
        matches!(&request.command, messages::Command::DeviceInfo(r) if r.device_id.0 == "d_test"),
        "{:?}",
        request.command
    );
    request
}
fn fields(controller: &Controller) -> (bool, Vec<InfoField>) {
    let state = controller.state().unwrap();
    let info = &state.info[&DeviceId("d_test".into())];
    (info.current, info.fields.clone())
}
fn value(fields: &[InfoField], key: InfoKey, instance: u8) -> Option<SettingValue> {
    fields
        .iter()
        .find(|f| f.key == key && f.instance == instance && f.available)
        .map(|f| f.value.clone())
}

/// Opens a session and answers readiness and the device list, leaving the
/// device's information request to the test.
fn setup() -> (Controller, Receiver<Event>, Firmware) {
    let (client, firmware) = connect_info();
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
    firmware.reply(&request, json!({"state":"ready","status":status()}));
    reply_devices(&firmware, &firmware.receive(), 0);
    (controller, events, firmware)
}

#[test]
fn initial_sync_reads_info_after_the_device_list_and_keeps_newer_changes() {
    let (controller, events, firmware) = setup();
    let request = info_request(&firmware);
    // Snapshots follow monitoring, status and the device list.
    let log: Vec<String> = firmware
        .log
        .lock()
        .unwrap()
        .iter()
        .filter(|c| *c != "session.heartbeat")
        .cloned()
        .collect();
    assert_eq!(
        log,
        [
            "adapter.protocol",
            "adapter.capabilities",
            "adapter.status",
            "adapter.wait_ready",
            "session.monitor.set",
            "adapter.status",
            "device.list",
            "device.info"
        ]
    );
    // A change newer than the snapshot arrives first and is kept.
    firmware.event(
        "device.info.changed",
        None,
        snapshot(1, vec![field("battery_percent", 0, json!(79))]),
    );
    firmware.reply(
        &request,
        snapshot(
            0,
            vec![
                field("battery_percent", 0, json!(80)),
                field("battery_charging", 0, json!(false)),
                field("model", 0, json!("M1")),
                field("vendor_id", 0, json!(0x046d)),
            ],
        ),
    );
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    let (current, f) = fields(&controller);
    assert!(current);
    assert_eq!(
        value(&f, InfoKey::BatteryPercent, 0),
        Some(SettingValue::Integer(79))
    );
    assert_eq!(
        value(&f, InfoKey::BatteryCharging, 0),
        Some(SettingValue::Bool(false))
    );

    // Omitted fields are unchanged; an unavailable value clears one.
    firmware.event(
        "device.info.changed",
        None,
        snapshot(
            2,
            vec![
                cleared("battery_charging", 0),
                field("firmware", 1, json!("2.0")),
            ],
        ),
    );
    seen(&events, "device.info.changed");
    let (current, f) = fields(&controller);
    assert!(current);
    assert_eq!(value(&f, InfoKey::BatteryCharging, 0), None);
    assert_eq!(
        value(&f, InfoKey::BatteryPercent, 0),
        Some(SettingValue::Integer(79))
    );
    assert_eq!(
        value(&f, InfoKey::Model, 0),
        Some(SettingValue::Text("M1".into()))
    );
    assert_eq!(
        value(&f, InfoKey::Firmware, 1),
        Some(SettingValue::Text("2.0".into()))
    );
    // Display order follows the key order, not arrival.
    let keys: Vec<InfoKey> = f.iter().map(|f| f.key).collect();
    assert!(keys.windows(2).all(|w| {
        let i = |k| InfoKey::ALL.iter().position(|x| *x == k).unwrap();
        i(w[0]) <= i(w[1])
    }));

    // A gap makes the information not current until the device list and
    // then a new snapshot are read.
    firmware.event(
        "device.info.changed",
        None,
        snapshot(5, vec![field("battery_percent", 0, json!(70))]),
    );
    seen(&events, "device.info.changed");
    assert!(!fields(&controller).0);
    reply_devices(&firmware, &firmware.receive(), 5);
    let request = info_request(&firmware);
    assert!(!fields(&controller).0);
    firmware.reply(
        &request,
        snapshot(5, vec![field("battery_charging", 0, json!(true))]),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !fields(&controller).0 {
        assert!(Instant::now() < deadline, "info never became current");
        std::thread::sleep(Duration::from_millis(10));
    }
    let (_, f) = fields(&controller);
    // The snapshot is complete: fields it omits are unknown.
    assert_eq!(value(&f, InfoKey::BatteryPercent, 0), None);
    assert_eq!(value(&f, InfoKey::Model, 0), None);
    assert_eq!(
        value(&f, InfoKey::BatteryCharging, 0),
        Some(SettingValue::Bool(true))
    );
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn refresh_reads_a_new_snapshot_and_bad_info_is_rejected() {
    let (controller, events, firmware) = setup();
    let request = info_request(&firmware);
    firmware.reply(&request, snapshot(0, vec![]));
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    let ticket = controller.run(
        Command::DeviceInfoRefresh("Test mouse".into()),
        RunOptions::default(),
    );
    let request = firmware.receive();
    assert!(
        matches!(&request.command, messages::Command::DeviceInfoRefresh(r) if r.device_id.0 == "d_test")
    );
    firmware.reply(
        &request,
        snapshot(0, vec![field("battery_percent", 0, json!(55))]),
    );
    let Event::Done { result, .. } = until(
        &events,
        |e| matches!(e, Event::Done { ticket: t, .. } if *t == ticket),
    ) else {
        unreachable!()
    };
    assert!(matches!(result, Ok(Outcome::DeviceInfo(s)) if s.id == "d_test"));
    let (_, f) = fields(&controller);
    assert_eq!(
        value(&f, InfoKey::BatteryPercent, 0),
        Some(SettingValue::Integer(55))
    );
    // Out-of-range values end the session rather than being shown.
    firmware.event(
        "device.info.changed",
        None,
        snapshot(1, vec![field("battery_percent", 0, json!(101))]),
    );
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Lost(_),
                ..
            }
        )
    });
}

#[test]
fn a_new_device_gets_its_snapshot_and_unpairing_drops_it() {
    let (controller, events, firmware) = setup();
    firmware.reply(&info_request(&firmware), snapshot(0, vec![]));
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    let mut new = device();
    new["device_id"] = json!("d_new");
    firmware.event("device.paired", None, json!({"revision":1,"device":new}));
    // The new device's changes before its snapshot are covered by it.
    firmware.event(
        "device.info.changed",
        None,
        json!({"revision":2,"device_id":"d_new","fields":[field("model", 0, json!("New"))]}),
    );
    let request = firmware.receive();
    assert!(
        matches!(&request.command, messages::Command::DeviceInfo(r) if r.device_id.0 == "d_new"),
        "{:?}",
        request.command
    );
    firmware.reply(
        &request,
        json!({"revision":2,"device_id":"d_new","fields":[field("model", 0, json!("New"))]}),
    );
    let id = DeviceId("d_new".into());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !controller.state().unwrap().info.contains_key(&id) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    firmware.event(
        "device.unpaired",
        None,
        json!({"revision":3,"device_id":"d_new"}),
    );
    seen(&events, "device.unpaired");
    assert!(!controller.state().unwrap().info.contains_key(&id));
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn a_reported_name_names_and_resolves_the_device() {
    let (controller, events, firmware) = setup();
    firmware.reply(
        &info_request(&firmware),
        snapshot(0, vec![field("name", 0, json!("Reported mouse"))]),
    );
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    let name = |c: &Controller| c.state().unwrap().devices[0].name.clone();
    assert_eq!(name(&controller).as_deref(), Some("Reported mouse"));
    // A rename arrives only as information; the device record is not resent.
    firmware.event(
        "device.info.changed",
        None,
        snapshot(1, vec![field("name", 0, json!("Renamed mouse"))]),
    );
    seen(&events, "device.info.changed");
    assert_eq!(name(&controller).as_deref(), Some("Renamed mouse"));
    let ticket = controller.run(
        Command::DeviceInfo("Renamed mouse".into()),
        RunOptions::default(),
    );
    let request = info_request(&firmware);
    firmware.reply(
        &request,
        snapshot(1, vec![field("name", 0, json!("Renamed mouse"))]),
    );
    let Event::Done { result, .. } = until(
        &events,
        |e| matches!(e, Event::Done { ticket: t, .. } if *t == ticket),
    ) else {
        unreachable!()
    };
    assert!(
        matches!(&result, Ok(Outcome::DeviceInfo(s)) if s.id == "d_test" && s.name.as_deref() == Some("Renamed mouse")),
        "{result:?}"
    );
    // A stale or unknown name keeps the known one, in the list and results.
    firmware.event(
        "device.info.changed",
        None,
        json!({"revision":2,"device_id":"d_test","fields":[
            {"key":"name","instance":0,"value":"Renamed mouse","available":true,"fresh":false}]}),
    );
    seen(&events, "device.info.changed");
    assert_eq!(name(&controller).as_deref(), Some("Renamed mouse"));
    firmware.event(
        "device.info.changed",
        None,
        snapshot(3, vec![cleared("name", 0)]),
    );
    seen(&events, "device.info.changed");
    assert_eq!(name(&controller).as_deref(), Some("Renamed mouse"));
    let ticket = controller.run(
        Command::DeviceInfo("Renamed mouse".into()),
        RunOptions::default(),
    );
    firmware.reply(&info_request(&firmware), snapshot(3, vec![]));
    let Event::Done { result, .. } = until(
        &events,
        |e| matches!(e, Event::Done { ticket: t, .. } if *t == ticket),
    ) else {
        unreachable!()
    };
    assert!(
        matches!(&result, Ok(Outcome::DeviceInfo(s)) if s.name.as_deref() == Some("Renamed mouse")),
        "{result:?}"
    );
    assert_eq!(name(&controller).as_deref(), Some("Renamed mouse"));
    // device get shows the same name, not the record's saved one.
    let ticket = controller.run(Command::Info("Renamed mouse".into()), RunOptions::default());
    let request = firmware.receive();
    assert!(matches!(request.command, messages::Command::Info(_)));
    firmware.reply(&request, json!({"device":device()}));
    let Event::Done { result, .. } = until(
        &events,
        |e| matches!(e, Event::Done { ticket: t, .. } if *t == ticket),
    ) else {
        unreachable!()
    };
    assert!(
        matches!(&result, Ok(Outcome::Device { device: Some(d), .. }) if d.name.as_deref() == Some("Renamed mouse")),
        "{result:?}"
    );
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn explicit_event_loss_recovers_information_without_any_later_event() {
    let (controller, events, firmware) = setup();
    let request = info_request(&firmware);
    firmware.reply(
        &request,
        snapshot(0, vec![field("battery_percent", 0, json!(51))]),
    );
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    firmware.event("events.lost", None, json!({"revision":1,"dropped":1}));
    seen(&events, "events.lost");
    reply_devices(&firmware, &firmware.receive(), 1);
    let request = info_request(&firmware);
    firmware.reply(
        &request,
        snapshot(1, vec![field("battery_percent", 0, json!(50))]),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let (current, f) = fields(&controller);
        if current && value(&f, InfoKey::BatteryPercent, 0) == Some(SettingValue::Integer(50)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "lost battery event was not recovered"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    controller.close();
}
