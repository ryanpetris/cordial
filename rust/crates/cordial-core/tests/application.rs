mod support;
use cordial_core::{
    application::{Application, Build},
    bluetooth::{Event, InputReport},
    compact::{Metadata, Preference},
    link::ServiceId,
    settings::PreferenceStore,
    storage::Preferences,
};
use cordial_protocol::{
    MAX_LINE_BYTES,
    errors::ErrorCode,
    hidpp::{FeatureId, FeatureRevision},
    messages::{PromptMethod, Status},
    settings::{SettingKey, SettingScope},
};
use embassy_futures::block_on;
use serde_json::{Value, json};
use support::*;

struct Test<'a> {
    app: Application<'a>,
    store: Store,
    radio: Radio,
    now: u64,
    commands: std::collections::BTreeMap<u32, String>,
}
impl<'a> Test<'a> {
    fn new(input: &'a mut [u8], paired: bool) -> Self {
        let (_, mut store, radio) = if paired {
            setup()
        } else {
            (Default::default(), Store::default(), Radio::default())
        };
        if !paired {
            block_on(cordial_core::identity::Identity::initialize(
                &mut store,
                [2; 6],
                || 42,
            ))
            .unwrap();
        }
        let mut app = Application::new(
            input,
            Build {
                profile: cordial_protocol::messages::BuildProfile::Production,
                version: "test",
                hardware: "test",
                default_adapter_name: "Test adapter",
                digest: "test",
                radio_backend: "pico-sdk-cyw43",
                adapter_id: "adapter".into(),
                boot_id: "boot".into(),
                bootloader: None,
            },
        );
        let mut radio = radio;
        app.session(true, &mut radio, 0);
        let mut test = Self {
            app,
            store,
            radio,
            now: 0,
            commands: Default::default(),
        };
        test.event(Event::Ready);
        test
    }
    fn event(&mut self, event: Event) {
        self.now += 1;
        block_on(
            self.app
                .event(event, &mut self.store, &mut self.radio, self.now),
        );
    }
    fn poll(&mut self) {
        self.now += 1;
        block_on(self.app.poll(&mut self.store, &mut self.radio, 0, self.now));
    }
    fn drain(&mut self) -> Vec<Value> {
        let mut bytes = vec![];
        while let Some((token, data)) = self.app.serial.output(64, self.now) {
            bytes.extend_from_slice(data);
            let n = data.len();
            self.app.serial.output_complete(token, n);
        }
        bytes
            .split(|b| *b == b'\n')
            .filter(|b| !b.is_empty())
            .map(|b| {
                let message: Value = serde_json::from_slice(b).unwrap();
                let definition = if message["type"] == "event" {
                    format!("{}.event", message["event"].as_str().unwrap())
                } else if let Some(command) = message["id"]
                    .as_u64()
                    .and_then(|id| self.commands.get(&(id as u32)))
                {
                    if cordial_protocol::messages::COMMANDS
                        .iter()
                        .any(|c| c.as_str() == command)
                    {
                        format!("{command}.response")
                    } else {
                        "Response".into()
                    }
                } else {
                    "Response".into()
                };
                cordial_schema::assert_valid(&definition, &message);
                message
            })
            .collect()
    }
    fn command(&mut self, id: u32, cmd: &str, args: Value) -> Vec<Value> {
        self.commands.insert(id, cmd.into());
        self.now += 1;
        let mut bytes = serde_json::to_vec(&json!({"v":if cmd == "adapter.protocol" { 0 } else { 1 },"id":id,"cmd":cmd,"args":args})).unwrap();
        bytes.push(b'\n');
        let (n, request) = self.app.serial.feed(&bytes, self.now);
        assert_eq!(n, bytes.len());
        let request = request.unwrap();
        block_on(
            self.app
                .dispatch(&request, &mut self.store, &mut self.radio, self.now),
        )
        .unwrap();
        let mut replies = vec![];
        for _ in 0..12 {
            self.poll();
            replies.extend(self.drain());
        }
        replies
    }
}
#[test]
fn pairing_prompts_preserve_request_order_and_forwarding_survives_cli_exit() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    let status = t.command(1, "adapter.wait_ready", json!({}));
    let status: Status = serde_json::from_value(status[0]["result"]["status"].clone()).unwrap();
    assert!(status.storage_ready && status.radio_ready);
    assert!(
        !t.app
            .capabilities(&t.radio)
            .contains(cordial_protocol::messages::Capability::Debug)
    );
    assert!(
        t.command(2, "discovery.scan", json!({"duration_ms":0}))
            .is_empty()
    );
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        connectable: true,
        scan: 1,
        address: Some(peer(2)),
        peer: peer(2),
        name: "Keyboard".into(),
        rssi: Some(-40),
    });
    t.poll();
    assert_eq!(t.drain()[0]["data"]["candidate_id"], "c_1");
    assert!(
        t.command(3, "pairing.start", json!({"candidate_id":"c_1"}))
            .is_empty()
    );
    let link = t.radio.connects[0].0;
    t.event(Event::Prompt {
        link,
        method: PromptMethod::EnterPasskey,
        value: None,
    });
    t.poll();
    let prompt = t.drain();
    assert_eq!(prompt[0]["event"], "pairing.prompt");
    let reply = t.command(
        4,
        "pairing.reply",
        json!({"request_id":3,"prompt_id":"p_1","action":"accept","value":"001234"}),
    );
    assert_eq!(reply[0]["result"]["accepted"], true);
    t.radio.bonds.push(peer(2));
    t.event(Event::Bonded {
        link,
        identity: peer(2),
    });
    t.poll();
    let paired = t.drain();
    let paired = paired.iter().find(|r| r["id"] == 3).unwrap();
    assert_eq!(paired["result"]["device"]["hidpp_enabled"], true);
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
    });
    for _ in 0..5 {
        t.poll();
    }
    while t.app.manager.forward.packet().is_some() {
        t.app.manager.forward.complete();
    }
    t.app.session(false, &mut t.radio, t.now);
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes()[0], 16);
    t.app.manager.forward.complete();
    t.event(Event::Failed(ErrorCode::RadioUnavailable));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes(), &[0; 32]);
}
#[test]
fn cancellation_ack_precedes_scan_result_and_old_pairing_cannot_save_after_session_exit() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.command(1, "discovery.scan", json!({"duration_ms":0}));
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        connectable: true,
        scan: 1,
        address: Some(peer(2)),
        peer: peer(2),
        name: "Keyboard".into(),
        rssi: None,
    });
    let replies = t.command(2, "request.cancel", json!({"request_id":1}));
    assert_eq!(replies[0]["id"], 2);
    assert_eq!(replies.last().unwrap()["id"], 1);
    assert_eq!(replies.last().unwrap()["error"]["code"], "cancelled");
    t.command(3, "pairing.start", json!({"candidate_id":"c_1"}));
    let link = t.radio.connects[0].0;
    t.app.session(false, &mut t.radio, t.now);
    t.radio.bonds.push(peer(2));
    t.event(Event::Bonded {
        link,
        identity: peer(2),
    });
    assert!(t.app.manager.devices.iter().all(Option::is_none));
    t.event(Event::Disconnected { link, error: None });
    t.poll();
    assert!(t.radio.bonds.is_empty());
    t.app.session(true, &mut t.radio, t.now);
    assert_eq!(
        t.command(1, "pairing.start", json!({"candidate_id":"c_1"}))[0]["error"]["code"],
        "candidate_expired"
    );
}
#[test]
fn offline_settings_list_and_forget_use_saved_rows_without_bluetooth_writes() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    let p = Preference {
        metadata: Metadata {
            key: SettingKey::BacklightEnabled,
            feature: FeatureId::BACKLIGHT,
            revision: FeatureRevision(3),
            scope: SettingScope::Device,
            choices: Box::new([]),
            range: None,
        },
        value: 1,
    };
    block_on(
        Preferences {
            store: &mut t.store,
            device: 77,
        }
        .save(&p),
    )
    .unwrap();
    t.app.manager.devices[0]
        .as_mut()
        .unwrap()
        .catalog
        .restore_preferences(vec![p])
        .unwrap();
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    let id = "d_000000000000004d";
    let list = t.command(1, "hidpp.setting.list", json!({"device_id":id}));
    assert_eq!(list[0]["result"]["setting"]["managed"], true);
    assert_eq!(list[1]["result"]["count"], 1);
    assert_eq!(
        t.command(
            2,
            "hidpp.setting.set",
            json!({"device_id":id,"key":"backlight.enabled","value":false})
        )[0]["error"]["code"],
        "not_connected"
    );
    let forgotten = t.command(
        3,
        "hidpp.setting.forget",
        json!({"device_id":id,"key":"backlight.enabled"}),
    );
    assert_eq!(forgotten[0]["result"]["setting"]["managed"], false);
    assert!(t.radio.writes.is_empty());
    assert!(
        block_on(
            Preferences {
                store: &mut t.store,
                device: 77
            }
            .load_all()
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        t.app.manager.devices[0]
            .as_ref()
            .unwrap()
            .catalog
            .records()
            .len(),
        1
    );
}

struct CountAllocations;
thread_local! {
    static TRACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
unsafe impl std::alloc::GlobalAlloc for CountAllocations {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if TRACK.try_with(|v| v.get()).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        if TRACK.try_with(|v| v.get()).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { std::alloc::System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountAllocations = CountAllocations;

#[test]
fn standalone_key_forwarding_and_idle_polling_do_not_allocate() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
    });
    for _ in 0..10 {
        t.poll();
    }
    while t.app.manager.forward.packet().is_some() {
        t.app.manager.forward.complete();
    }
    t.app.session(false, &mut t.radio, t.now);
    ALLOCS.set(0);
    TRACK.set(true);
    for n in 0..200 {
        t.event(Event::Input(
            InputReport::new(link, ServiceId(7), 0, &[n & 1]).unwrap(),
        ));
        t.poll();
        while t.app.manager.forward.packet().is_some() {
            t.app.manager.forward.complete();
        }
    }
    TRACK.set(false);
    assert_eq!(ALLOCS.get(), 0);
}

use cordial_protocol::settings::*;

fn setting() -> cordial_core::compact::Record {
    cordial_core::compact::Record::from_wire(&Setting {
        key: SettingKey::BacklightEnabled,
        kind: SettingType::Bool,
        writable: true,
        feature: FeatureId::BACKLIGHT,
        feature_version: FeatureRevision(3),
        scope: SettingScope::Device,
        choices: vec![],
        min: None,
        max: None,
        step: None,
        managed: false,
        desired: SettingValue::Null,
        observed: SettingValue::Bool(true),
        fresh: true,
        observed_at_ms: Some(42),
        observation_source: Some(ObservationSource::Read),
        state: SettingState::Unmanaged,
        error: None,
    })
    .unwrap()
}

#[test]
fn explicit_connect_joins_automatic_attempt() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    let replies = t.command(
        1,
        "device.connect",
        json!({"device_id":"d_000000000000004d"}),
    );
    assert!(
        replies.is_empty(),
        "connect should wait for the existing attempt, got {replies:?}"
    );
}
#[test]
fn connect_timeout_keeps_auto_reconnect() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    assert!(
        t.command(
            1,
            "device.connect",
            json!({"device_id":"d_000000000000004d","timeout_ms":1000})
        )
        .is_empty()
    );
    t.now += 1001;
    t.poll();
    assert!(
        !t.app.manager.devices[0].as_ref().unwrap().paused,
        "timeout must not pause trusted auto reconnect"
    );
    let replies = t.drain();
    assert_eq!(replies.last().unwrap()["error"]["code"], "timeout");
}

fn saved_ble(t: &mut Test<'_>) -> Peer {
    let d = t.app.manager.devices[0].as_mut().unwrap();
    d.policy.peer.transport = Transport::Ble;
    let peer = d.policy.peer;
    t.radio.bonds = vec![peer];
    peer
}

#[test]
fn ble_accept_list_waits_without_reserving_a_slot_or_control_session() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    let first = saved_ble(&mut t);
    let mut other = t.app.manager.devices[0].as_ref().unwrap().policy.clone();
    other.id = 78;
    other.peer.address[0] ^= 1;
    let second = other.peer;
    t.app
        .manager
        .devices
        .push(Some(cordial_core::devices::Device::new(other)));
    t.radio.bonds.push(second);
    t.app.session(false, &mut t.radio, t.now);
    let saved = t.store.records.clone();
    for _ in 0..10 {
        t.now += 400_000;
        t.poll();
        assert_eq!(t.radio.reconnect, [first, second]);
        assert!(t.radio.connects.is_empty());
        assert!(t.app.manager.connections.iter().all(Option::is_none));
        assert!(t.radio.scans.is_empty());
    }
    // The second peer wakes while the first remains asleep.
    t.event(Event::Incoming {
        attempt: 1,
        peer: second,
    });
    assert!(t.radio.incoming.last().unwrap().is_some());
    assert_eq!(
        t.app.manager.devices[1].as_ref().unwrap().state,
        cordial_protocol::identifiers::ConnectionState::Connecting
    );
    t.poll();
    assert!(t.radio.reconnect.is_empty(), "pause during HID setup");
    assert_eq!(t.store.records, saved);
    assert_eq!(t.app.serial.queued(), 0);
}

#[test]
fn ble_accept_list_rechecks_policy_on_arrival_and_respects_failure_cooldown() {
    for reason in [
        "paused",
        "untrusted",
        "blocked",
        "disabled",
        "authentication",
        "cooldown",
    ] {
        let mut input = [0; MAX_LINE_BYTES - 1];
        let mut t = Test::new(&mut input, true);
        let peer = saved_ble(&mut t);
        t.poll();
        assert_eq!(t.radio.reconnect, [peer]);
        let d = t.app.manager.devices[0].as_mut().unwrap();
        match reason {
            "paused" => d.paused = true,
            "untrusted" => d.policy.trusted = false,
            "blocked" => d.policy.blocked = true,
            "disabled" => d.effective_enabled = false,
            _ => d.connection(
                cordial_protocol::identifiers::ConnectionState::Disconnected,
                Some(if reason == "authentication" {
                    ErrorCode::AuthenticationFailed
                } else {
                    ErrorCode::Timeout
                }),
                t.now,
            ),
        }
        t.event(Event::Incoming { attempt: 1, peer });
        assert_eq!(t.radio.incoming.last(), Some(&None), "{reason}");
        t.poll();
        assert!(t.radio.reconnect.is_empty(), "{reason}");
        if reason == "cooldown" {
            t.now += 5000;
            t.poll();
            assert_eq!(t.radio.reconnect, [peer]);
            t.event(Event::Incoming { attempt: 2, peer });
            assert!(t.radio.incoming.last().unwrap().is_some());
        }
    }
}

#[test]
fn identity_only_presence_is_not_a_fresh_pair_candidate() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.command(
        1,
        "discovery.scan",
        json!({"transport":"ble","duration_ms":0}),
    );
    let scan = t.radio.scans.last().unwrap().0;
    let identity = Peer {
        transport: Transport::Ble,
        ..peer(1)
    };
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        scan,
        peer: identity,
        address: None,
        connectable: true,
        name: "Keyboard".into(),
        rssi: None,
    });
    t.poll();
    assert!(
        t.drain()
            .iter()
            .all(|m| m.get("event") != Some(&json!("discovery.result")))
    );
    assert_eq!(
        t.command(2, "pairing.start", json!({"candidate_id":"c_1"}))[0]["error"]["code"],
        "candidate_expired"
    );
    assert!(t.radio.connects.is_empty());
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        scan,
        peer: identity,
        address: Some(Peer {
            random: true,
            ..identity
        }),
        connectable: true,
        name: "Keyboard".into(),
        rssi: None,
    });
    t.poll();
    assert!(
        t.drain()
            .iter()
            .any(|m| m.get("event") == Some(&json!("discovery.result")))
    );
}

#[test]
fn foreground_discovery_and_session_exit_preserve_accept_list() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    let peer = saved_ble(&mut t);
    t.poll();
    t.command(
        1,
        "discovery.scan",
        json!({"transport":"classic","duration_ms":0}),
    );
    assert!(matches!(t.radio.scans.last(), Some((_, true, false))));
    assert_eq!(t.radio.reconnect, [peer]);
    t.app.session(false, &mut t.radio, t.now);
    t.poll();
    assert!(matches!(t.radio.scans.last(), Some((_, false, false))));
    assert_eq!(t.radio.reconnect, [peer]);
    let scans = t.radio.scans.len();
    t.poll();
    assert_eq!(t.radio.scans.len(), scans);
}
#[test]
fn discovery_shares_time_with_reconnection_only_for_exclusive_backends() {
    for concurrent in [false, true] {
        let mut input = [0; MAX_LINE_BYTES - 1];
        let mut t = Test::new(&mut input, true);
        t.radio.transports = Some(cordial_core::bluetooth::Capabilities {
            classic: true,
            ble: true,
            ble_scan_and_connect: concurrent,
        });
        let peer = saved_ble(&mut t);
        t.poll();
        t.command(
            1,
            "discovery.scan",
            json!({"transport":"ble","duration_ms":0}),
        );
        let token = t.radio.scans.last().unwrap().0;
        for now in [1000, 2000, 3000, 4000] {
            t.now = now;
            t.poll();
            assert_eq!(t.radio.reconnect, [peer]);
            assert_eq!(
                t.radio.scans.last(),
                Some(&(token, false, concurrent || now % 2000 == 0))
            );
        }
        // A live request keeps its original token through pause/resume; cancellation
        // during an exclusive reconnect window must never resurrect it.
        t.now = 5000;
        t.poll();
        t.command(2, "request.cancel", json!({"request_id":1}));
        t.now = 6000;
        t.poll();
        assert_eq!(t.radio.scans.last(), Some(&(token, false, false)));
        assert_eq!(t.radio.reconnect, [peer]);
    }
}

#[test]
fn discovery_does_not_pause_without_eligible_saved_ble_devices() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.command(
        1,
        "discovery.scan",
        json!({"transport":"ble","duration_ms":0}),
    );
    let scan = *t.radio.scans.last().unwrap();
    t.now = 1000;
    t.poll();
    assert!(t.radio.reconnect.is_empty());
    assert_eq!(t.radio.scans.last(), Some(&scan));
}

#[test]
fn settings_set_during_teardown_does_not_save() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
    });
    for _ in 0..5 {
        t.poll();
    }
    t.app.manager.devices[0]
        .as_mut()
        .unwrap()
        .catalog
        .replace_discovery(vec![setting()], vec![])
        .unwrap();
    // Actual input failure closes the link before its Disconnected callback.
    t.event(Event::Input(
        InputReport::new(link, ServiceId(99), 0, &[1]).unwrap(),
    ));
    assert!(t.app.manager.connection(link).unwrap().closing);
    let replies = t.command(
        1,
        "hidpp.setting.set",
        json!({"device_id":"d_000000000000004d","key":"backlight.enabled","value":false}),
    );
    assert_eq!(replies[0]["error"]["code"], "not_connected");
    let prefs = block_on(
        Preferences {
            store: &mut t.store,
            device: 77,
        }
        .load_all(),
    )
    .unwrap();
    assert!(
        prefs.is_empty(),
        "failed offline set durably saved {prefs:?}"
    );
}

#[test]
fn block_keeps_prior_reconnect_pause() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.command(
        1,
        "device.connect",
        json!({"device_id":"d_000000000000004d"}),
    );
    assert!(!t.app.manager.devices[0].as_ref().unwrap().paused);
    t.command(
        2,
        "device.blocked.set",
        json!({"device_id":"d_000000000000004d","blocked":true}),
    );
    assert!(
        !t.app.manager.devices[0].as_ref().unwrap().paused,
        "block should keep the existing auto reconnect policy"
    );
}
#[test]
fn each_notification_gets_its_own_revision() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    let p = Preference {
        metadata: Metadata {
            key: SettingKey::BacklightEnabled,
            feature: FeatureId::BACKLIGHT,
            revision: FeatureRevision(3),
            scope: SettingScope::Device,
            choices: Box::new([]),
            range: None,
        },
        value: 1,
    };
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.app.manager.devices[0]
        .as_mut()
        .unwrap()
        .catalog
        .restore_preferences(vec![p])
        .unwrap();
    t.command(
        1,
        "device.trusted.set",
        json!({"device_id":"d_000000000000004d","trusted":true}),
    );
    t.command(2, "session.monitor.set", json!({"enabled":true}));
    let replies = t.command(
        3,
        "hidpp.setting.forget",
        json!({"device_id":"d_000000000000004d","key":"backlight.enabled"}),
    );
    let events: Vec<_> = replies.iter().filter(|r| r["type"] == "event").collect();
    assert_eq!(events.len(), 2);
    assert!(
        events[0]["data"]["revision"].as_u64().unwrap()
            < events[1]["data"]["revision"].as_u64().unwrap(),
        "event revisions are duplicated: {events:?}"
    );
}
#[test]
fn manual_settings_refresh_announces_start_before_failure() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
    });
    for _ in 0..5 {
        t.poll();
    }
    t.drain();
    t.command(1, "session.monitor.set", json!({"enabled":true}));
    let replies = t.command(
        2,
        "hidpp.setting.refresh",
        json!({"device_id":"d_000000000000004d"}),
    );
    let states: Vec<_> = replies
        .iter()
        .filter(|r| r["event"] == "device.changed")
        .map(|r| r["data"]["device"]["settings_state"].clone())
        .collect();
    assert_eq!(states, vec![json!("discovering"), json!("unsupported")]);
}
#[test]
fn bootloader_releases_input_before_reboot() {
    use cordial_core::application::Bootloader;
    let mut input = [0; MAX_LINE_BYTES - 1];
    let (manager, mut store, mut radio) = setup();
    let mut app = Application::new(
        &mut input,
        Build {
            profile: cordial_protocol::messages::BuildProfile::Development,
            version: "test",
            hardware: "test",
            default_adapter_name: "Test adapter",
            digest: "test",
            radio_backend: "pico-sdk-cyw43",
            adapter_id: "adapter".into(),
            boot_id: "boot".into(),
            bootloader: Some(Bootloader {
                mode: "bootsel",
                enter: || panic!("unexpected early reboot"),
            }),
        },
    );
    app.manager = manager;
    app.session(true, &mut radio, 0);
    let link = app
        .manager
        .connect(0, true, 30_000, &mut radio)
        .unwrap()
        .unwrap();
    app.manager.connected(link, descriptor(), 255, 0).unwrap();
    while app.manager.forward.packet().is_some() {
        app.manager.forward.complete();
    }
    block_on(app.event(
        Event::Input(InputReport::new(link, ServiceId(7), 0, &[1]).unwrap()),
        &mut store,
        &mut radio,
        1,
    ));
    assert_eq!(app.manager.forward.packet().unwrap().bytes()[0], 16);
    app.manager.forward.complete();
    let (_, request) = app.serial.feed(
        b"{\"v\":1,\"id\":1,\"cmd\":\"adapter.bootloader.enter\",\"args\":{}}\n",
        2,
    );
    block_on(app.dispatch(&request.unwrap(), &mut store, &mut radio, 2)).unwrap();
    block_on(app.poll(&mut store, &mut radio, 0, 3));
    assert!(
        app.manager.forward.packet().is_some(),
        "accepted reboot leaves the host key held and forwarding enabled"
    );
}

#[test]
fn connect_completion_after_request_deadline_is_timeout() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.command(
        1,
        "device.connect",
        json!({"device_id":"d_000000000000004d","timeout_ms":1000}),
    );
    let link = t.radio.connects[0].0;
    t.now += 1001;
    // Backend events can arrive before the next periodic app poll.
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
    });
    t.poll();
    let replies = t.drain();
    assert_eq!(
        replies.last().unwrap()["error"]["code"],
        "timeout",
        "late callback completed expired request: {replies:?}"
    );
}

const SAVED: &str = "d_000000000000004d";
fn many(input: &mut [u8], saved: u8, bootloader: bool) -> Test<'_> {
    use cordial_core::devices::{Policies, Policy};
    let mut store = Store::default();
    let mut radio = Radio::default();
    block_on(cordial_core::storage::open(&mut store)).unwrap();
    for n in 0..saved {
        let mut policy = Policy::paired(100 + n as u64, peer(10 + n), b"Sleeper");
        policy.bond = policy.id;
        block_on(cordial_core::bonds::commit(
            &mut store,
            &policy,
            &bond(policy.id, policy.peer),
        ))
        .unwrap();
        block_on(Policies { store: &mut store }.save(n as usize, &policy)).unwrap();
        radio.bonds.push(peer(10 + n));
    }
    fn enter() -> ! {
        panic!("rebooted")
    }
    let mut app = Application::new(
        input,
        Build {
            profile: if bootloader {
                cordial_protocol::messages::BuildProfile::Development
            } else {
                cordial_protocol::messages::BuildProfile::Production
            },
            version: "test",
            hardware: "test",
            default_adapter_name: "Test adapter",
            digest: "test",
            radio_backend: "pico-sdk-cyw43",
            adapter_id: "adapter".into(),
            boot_id: "boot".into(),
            bootloader: bootloader.then_some(cordial_core::application::Bootloader {
                mode: "bootsel",
                enter,
            }),
        },
    );
    app.session(true, &mut radio, 0);
    let mut t = Test {
        app,
        store,
        radio,
        now: 0,
        commands: Default::default(),
    };
    t.event(Event::Ready);
    t
}

#[test]
fn cancelling_other_connect_preserves_pairing_prompt() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.command(1, "discovery.scan", json!({"duration_ms":0}));
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        connectable: true,
        scan: 1,
        address: Some(peer(2)),
        peer: peer(2),
        name: "New".into(),
        rssi: None,
    });
    t.poll();
    t.drain();
    t.command(2, "pairing.start", json!({"candidate_id":"c_1"}));
    let pair_link = t.radio.connects[0].0;
    t.event(Event::Prompt {
        link: pair_link,
        method: PromptMethod::EnterPasskey,
        value: None,
    });
    t.poll();
    let prompt = t.drain();
    assert_eq!(prompt[0]["event"], "pairing.prompt");
    // Unrelated saved device: explicit connect, then cancel it.
    t.command(3, "device.connect", json!({"device_id":SAVED}));
    t.command(4, "request.cancel", json!({"request_id":3}));
    let reply = t.command(
        5,
        "pairing.reply",
        json!({"request_id":2,"prompt_id":"p_1","action":"accept","value":"123456"}),
    );
    assert_eq!(reply[0]["result"]["accepted"], true);
}

#[test]
fn duplicate_identity_keeps_existing_device() {
    use cordial_core::devices::Peer;
    use cordial_protocol::identifiers::Transport;
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.command(1, "discovery.scan", json!({"duration_ms":0}));
    let rpa = Peer {
        address: [0x55; 6],
        random: true,
        transport: Transport::Ble,
    };
    let identity = Peer {
        address: [0x66; 6],
        random: false,
        transport: Transport::Ble,
    };
    // Replace the saved device with a BLE identity.
    t.app.manager.devices[0].as_mut().unwrap().policy.peer = identity;
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        connectable: true,
        scan: 1,
        address: Some(rpa),
        peer: rpa,
        name: "Mouse".into(),
        rssi: None,
    });
    t.poll();
    t.drain();
    t.command(2, "pairing.start", json!({"candidate_id":"c_1"}));
    let link = t.radio.connects[0].0;
    t.event(Event::Bonded { link, identity });
    t.event(Event::Disconnected { link, error: None });
    t.poll();
    let out = t.drain();
    assert_eq!(out.last().unwrap()["result"]["device"]["device_id"], SAVED);
    assert_eq!(t.app.manager.devices.iter().flatten().count(), 1);
}

#[test]
fn background_reconnect_leaves_room_and_does_not_block_development_reboot() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = many(&mut input, 5, true);
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    t.event(Event::Incoming {
        attempt: 1,
        peer: peer(14),
    });
    assert_eq!(t.radio.connects.len(), 2);
    assert!(
        t.command(1, "adapter.bootloader.enter", json!({}))[0]["ok"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn monitored_teardown_has_contiguous_revisions_and_one_catalog_invalidation() {
    for (command, extra) in [
        ("device.disconnect", json!({})),
        ("device.blocked.set", json!({"blocked":true})),
    ] {
        let mut input = [0; MAX_LINE_BYTES - 1];
        let mut t = Test::new(&mut input, true);
        let saved = [
            SettingKey::BacklightEnabled,
            SettingKey::WheelInvert,
            SettingKey::ThumbwheelInvert,
        ]
        .into_iter()
        .zip([
            FeatureId::BACKLIGHT,
            FeatureId::HIRES_WHEEL,
            FeatureId::THUMBWHEEL,
        ])
        .map(|(key, feature)| Preference {
            metadata: Metadata {
                key,
                feature,
                revision: FeatureRevision(0),
                scope: SettingScope::Device,
                choices: Box::new([]),
                range: None,
            },
            value: 1,
        })
        .collect();
        t.app.manager.devices[0]
            .as_mut()
            .unwrap()
            .catalog
            .restore_preferences(saved)
            .unwrap();
        t.poll();
        let link = t.radio.connects[0].0;
        t.event(Event::Connected {
            link,
            descriptors: descriptor(),
            max_output: 255,
        });
        for _ in 0..10 {
            t.poll();
            t.drain();
        }
        let monitor = t.command(1, "session.monitor.set", json!({"enabled":true}));
        let mut revision = monitor[0]["result"]["revision"].as_u64().unwrap();
        let output = {
            let mut args = json!({"device_id":SAVED});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            t.command(2, command, args)
        };
        let events: Vec<_> = output.iter().filter(|v| v["type"] == "event").collect();
        assert!(events.iter().any(|e| e["event"] == "device.disconnected"
            && e["data"]["device"]["state"] == "disconnecting"));
        for event in events {
            assert!(
                event["event"] == "device.changed"
                    || event["event"] == "device.disconnected"
                    || event["event"] == "device.info.changed"
                    || event["event"] == "events.lost",
                "{output:?}"
            );
            revision += 1;
            assert_eq!(event["data"]["revision"], revision);
        }
        t.event(Event::Disconnected { link, error: None });
        t.poll();
        let output = t.drain();
        let events: Vec<_> = output.iter().filter(|v| v["type"] == "event").collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "device.changed");
        assert_eq!(events[0]["data"]["revision"], revision + 1);
        assert_eq!(
            t.command(3, "session.monitor.set", json!({"enabled":true}))[0]["result"]["revision"],
            revision + 1
        );
    }
}

#[test]
fn bonded_link_gets_a_fresh_setup_deadline_and_initial_states_are_pending() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.command(1, "discovery.scan", json!({"duration_ms":0}));
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        connectable: true,
        scan: 1,
        address: Some(peer(2)),
        peer: peer(2),
        name: "Keyboard".into(),
        rssi: None,
    });
    let pair_deadline = t.now + 1 + 1000;
    t.command(
        2,
        "pairing.start",
        json!({"candidate_id":"c_1","timeout_ms":1000}),
    );
    let link = t.radio.connects[0].0;
    t.radio.bonds.push(peer(2));
    t.now = pair_deadline - 2;
    t.event(Event::Bonded {
        link,
        identity: peer(2),
    });
    t.poll();
    t.drain();
    t.now += 5;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
    });
    let record = t.app.manager.record(0).unwrap();
    assert_eq!(
        record.state,
        cordial_protocol::identifiers::ConnectionState::Connected
    );
    assert_eq!(
        record.normalization_state,
        cordial_protocol::identifiers::NormalizationState::Pending
    );
    assert_eq!(
        record.settings_state,
        cordial_protocol::identifiers::SettingsState::Pending
    );
}

#[test]
fn radio_failure_finishes_scan_and_unchanged_platform_does_not_emit() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    let boundary =
        t.command(1, "session.monitor.set", json!({"enabled":true}))[0]["result"]["revision"]
            .clone();
    let unchanged = t.command(2, "adapter.platform.set", json!({"platform":"linux"}));
    assert_eq!(unchanged.len(), 1);
    assert_eq!(unchanged[0]["result"]["revision"], boundary);
    t.command(3, "discovery.scan", json!({"duration_ms":0}));
    t.event(Event::Failed(ErrorCode::RadioUnavailable));
    t.poll();
    let output = t.drain();
    assert_eq!(output.last().unwrap()["error"]["code"], "radio_unavailable");
}

#[test]
fn native_bond_storage_failure_keeps_the_management_error_specific() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.event(Event::Failed(ErrorCode::StorageFailed));
    let output = t.command(1, "adapter.wait_ready", json!({}));
    assert_eq!(output.last().unwrap()["error"]["code"], "storage_failed");
    let status = t.command(2, "adapter.status", json!({}));
    assert_eq!(status[0]["result"]["storage_ready"], false);
    assert_eq!(status[0]["result"]["radio_ready"], false);
}

#[test]
fn radio_restart_releases_keys_then_reconnects_saved_devices_after_ready() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
    });
    while t.app.manager.forward.packet().is_some() {
        t.app.manager.forward.complete();
    }
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes()[0], 16);
    t.app.manager.forward.complete();
    let saved = t.store.records.clone();
    t.event(Event::Restarting(ErrorCode::RadioUnavailable));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes(), &[0; 32]);
    while t.app.manager.forward.packet().is_some() {
        t.app.manager.forward.complete();
    }
    assert!(!t.app.manager.radio_ready && t.app.manager.storage_ready);
    t.now += 5000;
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    assert!(t.app.manager.forward.packet().is_none());
    let waiting = t.command(1, "adapter.wait_ready", json!({}));
    assert_eq!(waiting[0]["done"], false);
    assert_eq!(waiting[0]["result"]["state"], "initializing");
    t.event(Event::Ready);
    t.poll();
    let ready = t.drain();
    assert_eq!(ready[0]["id"], 1);
    assert_eq!(ready[0]["done"], true);
    assert_eq!(ready[0]["result"]["state"], "ready");
    assert!(t.app.manager.radio_ready && t.app.manager.storage_ready);
    assert_eq!(t.radio.connects.len(), 2);
    assert_ne!(t.radio.connects[1].0, link);
    assert_eq!(t.store.records, saved);
}

#[test]
fn grouped_observations_use_one_watermark_without_losing_notifications() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
    });
    for _ in 0..10 {
        t.poll();
        t.drain();
    }
    let records = [
        SettingKey::BacklightEnabled,
        SettingKey::WheelInvert,
        SettingKey::ThumbwheelInvert,
    ]
    .into_iter()
    .map(|key| {
        let mut row = setting();
        row.metadata.key = key;
        row
    })
    .collect();
    t.app.manager.devices[0]
        .as_mut()
        .unwrap()
        .catalog
        .replace_discovery(records, vec![])
        .unwrap();
    t.command(
        1,
        "device.trusted.set",
        json!({"device_id":SAVED,"trusted":true}),
    );
    let boundary =
        t.command(2, "session.monitor.set", json!({"enabled":true}))[0]["result"]["revision"]
            .as_u64()
            .unwrap();
    for key in [
        SettingKey::BacklightEnabled,
        SettingKey::WheelInvert,
        SettingKey::ThumbwheelInvert,
    ] {
        t.app.manager.devices[0]
            .as_mut()
            .unwrap()
            .catalog
            .observe(
                key,
                SettingValue::Bool(false),
                t.now,
                ObservationSource::Event,
            )
            .unwrap();
    }
    let output = t.command(
        3,
        "device.trusted.set",
        json!({"device_id":SAVED,"trusted":true}),
    );
    let events: Vec<_> = output.iter().filter(|v| v["type"] == "event").collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "device.changed");
    assert_eq!(events[0]["data"]["revision"], boundary + 1);
    assert_eq!(
        events[0]["data"]["device"]["settings_revision"],
        boundary + 1
    );
}

#[test]
fn advertised_transports_and_default_scan_follow_independent_backend_capabilities() {
    use cordial_core::bluetooth::Capabilities;
    use cordial_protocol::identifiers::Transport;
    for (classic, ble) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut input = [0; MAX_LINE_BYTES - 1];
        let mut t = Test::new(&mut input, false);
        t.radio.transports = Some(Capabilities {
            classic,
            ble,
            ble_scan_and_connect: false,
        });
        let capabilities = t.app.capabilities(&t.radio);
        assert_eq!(capabilities.supports_transport(Transport::Classic), classic);
        assert_eq!(capabilities.supports_transport(Transport::Ble), ble);
        t.command(1, "session.heartbeat", json!({}));
        let responses = t.command(2, "discovery.scan", json!({"duration_ms":0}));
        if classic || ble {
            assert_eq!(t.radio.scans.last(), Some(&(1, classic, ble)));
            assert!(responses.iter().all(|r| r.get("ok") != Some(&json!(false))));
        } else {
            assert_eq!(
                responses.last().unwrap()["error"]["code"],
                "unsupported_transport"
            );
            assert!(t.radio.scans.is_empty());
        }
    }
}

// Pair renewal keeps policy records independent of native security keys.
use cordial_core::devices::{Device as SavedDevice, Peer, Policies, Policy};
use cordial_protocol::identifiers::{PairingState, Transport};

impl Test<'_> {
    fn candidate(&mut self, identity: Peer, address: Peer) {
        self.command(40, "discovery.scan", json!({"duration_ms":0}));
        let scan = self.radio.scans.last().unwrap().0;
        self.event(Event::Found {
            kind: cordial_protocol::messages::DeviceKind::Unknown,
            scan,
            peer: identity,
            address: Some(address),
            connectable: true,
            name: "Keyboard".into(),
            rssi: None,
        });
        self.poll();
        self.drain();
    }
    fn add_saved(&mut self, n: u8) {
        let mut policy = Policy::paired(77 + u64::from(n - 1), peer(n), b"Keyboard");
        policy.bond = policy.id;
        block_on(cordial_core::bonds::commit(
            &mut self.store,
            &policy,
            &bond(policy.id, policy.peer),
        ))
        .unwrap();
        block_on(
            Policies {
                store: &mut self.store,
            }
            .save(usize::from(n - 1), &policy),
        )
        .unwrap();
        self.radio.bonds.push(policy.peer);
        let mut d = SavedDevice::new(policy);
        d.paused = true;
        self.app
            .manager
            .devices
            .resize_with(usize::from(n), || None);
        self.app.manager.devices[usize::from(n - 1)] = Some(d);
    }
    fn finish_disconnects(&mut self) {
        for link in self.radio.closes.clone() {
            self.event(Event::Disconnected { link, error: None });
        }
        self.poll();
    }
}

#[test]
fn failed_stranger_bond_cleanup_is_retried_before_the_next_pair() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.add_saved(2);
    t.candidate(peer(1), peer(1));
    t.command(41, "pairing.start", json!({"candidate_id":"c_1"}));
    let link = t.radio.connects.last().unwrap().0;
    let stranger = peer(9);
    t.radio.bonds.push(stranger);
    t.radio.reject_forget = true;
    t.event(Event::Bonded {
        link,
        identity: stranger,
    });
    t.finish_disconnects();
    assert!(
        t.drain()
            .iter()
            .any(|r| r["id"] == 41 && r["error"]["code"] == "storage_failed")
    );
    assert!(t.radio.bonds.contains(&stranger));
    t.radio.reject_forget = false;
    t.command(42, "pairing.start", json!({"candidate_id":"c_1"}));
    assert!(!t.radio.bonds.contains(&stranger));
    assert!(t.radio.forgotten.contains(&stranger));
    assert!(t.radio.bonds.contains(&peer(2)));
    assert_eq!(
        t.app.manager.record(0).unwrap().pairing_state,
        PairingState::Paired
    );
    assert!(t.radio.connects.last().unwrap().1);
}

#[test]
fn long_teardown_times_out_request_but_keeps_reservation_until_native_cleanup() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.candidate(peer(1), peer(1));
    t.command(41, "pairing.start", json!({"candidate_id":"c_1"}));
    let link = t.radio.connects.last().unwrap().0;
    t.command(42, "request.cancel", json!({"request_id":41}));
    assert_eq!(
        t.command(
            43,
            "device.connect",
            json!({"device_id":"d_000000000000004d"})
        )[0]["error"]["code"],
        "busy"
    );
    assert_eq!(t.radio.connects.len(), 1);
    t.now += 15_001;
    t.poll();
    assert_eq!(
        t.drain().iter().find(|r| r["id"] == 41).unwrap()["error"]["code"],
        "timeout"
    );
    assert_eq!(
        t.command(
            44,
            "device.unpair",
            json!({"device_id":"d_000000000000004d"})
        )[0]["error"]["code"],
        "busy"
    );
    assert_eq!(
        t.command(
            45,
            "device.connect",
            json!({"device_id":"d_000000000000004d"})
        )[0]["error"]["code"],
        "busy"
    );
    assert_eq!(t.radio.connects.len(), 1);
    assert!(
        !t.app.manager.devices[0]
            .as_ref()
            .unwrap()
            .reconnect_due(u64::MAX)
    );
    t.event(Event::Disconnected { link, error: None });
    t.poll();
    t.drain();
    assert_eq!(
        t.command(
            46,
            "device.unpair",
            json!({"device_id":"d_000000000000004d"})
        )
        .last()
        .unwrap()["result"]["removed"],
        true
    );
}

#[test]
fn disable_and_block_close_live_input_even_when_native_sync_fails() {
    for (command, extra) in [
        ("device.enabled.set", json!({"enabled":false})),
        ("device.blocked.set", json!({"blocked":true})),
    ] {
        let mut input = [0; MAX_LINE_BYTES - 1];
        let mut t = Test::new(&mut input, true);
        t.command(1, "device.connect", json!({"device_id":SAVED}));
        let link = t.radio.connects[0].0;
        t.event(Event::Connected {
            link,
            descriptors: descriptor(),
            max_output: 255,
        });
        t.poll();
        t.drain();
        t.radio.reject_inventory = true;
        let replies = {
            let mut args = json!({"device_id":SAVED});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            t.command(2, command, args)
        };
        assert!(
            replies
                .iter()
                .any(|r| r["error"]["code"] == "storage_failed")
        );
        assert!(t.radio.closes.contains(&link));
        assert!(t.app.manager.connection(link).unwrap().closing);
        assert!(!t.app.manager.record(0).unwrap().effective_enabled);
    }
}
#[test]
fn radio_restart_reimports_the_committed_native_view() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.event(Event::Restarting(ErrorCode::RadioUnavailable));
    t.radio.bonds.clear();
    t.event(Event::Ready);
    assert!(t.radio.bonds.contains(&peer(1)));
    assert!(t.app.manager.storage_ready);
}
#[test]
fn pairing_reserves_against_settings_mutations() {
    use cordial_core::bonds;
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.store.available = Some(bonds::MAINTENANCE_BYTES + bonds::PAIR_BYTES + 4096);
    t.command(
        1,
        "discovery.scan",
        json!({"transport":"classic","duration_ms":0}),
    );
    let scan = t.radio.scans.last().unwrap().0;
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        scan,
        peer: peer(2),
        address: Some(peer(2)),
        connectable: true,
        name: "new".into(),
        rssi: None,
    });
    t.poll();
    t.drain();
    t.command(2, "pairing.start", json!({"candidate_id":"c_1"}));
    assert!(t.radio.connects.iter().any(|(_, pairing)| *pairing));
    let replies = t.command(
        3,
        "hidpp.setting.forget",
        json!({"device_id":SAVED,"key":"backlight.enabled"}),
    );
    assert!(
        replies.iter().any(|r| r["error"]["code"] == "busy"),
        "{replies:?}"
    );
}
#[test]
fn pair_storage_capacity_has_a_reason_and_scanning_stays_available() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.store.available = Some(0);
    t.command(
        1,
        "discovery.scan",
        json!({"transport":"classic","duration_ms":0}),
    );
    let scan = t.radio.scans.last().unwrap().0;
    t.event(Event::Found {
        kind: cordial_protocol::messages::DeviceKind::Unknown,
        scan,
        peer: peer(2),
        address: Some(peer(2)),
        connectable: true,
        name: "new".into(),
        rssi: None,
    });
    t.poll();
    t.drain();
    let replies = t.command(2, "pairing.start", json!({"candidate_id":"c_1"}));
    assert!(
        replies.iter().any(|r| r["error"]["code"] == "capacity"
            && r["error"]["details"]["reason"] == "storage_full"),
        "{replies:?}"
    );
    assert!(t.radio.connects.is_empty());
}

#[cfg(feature = "development")]
#[test]
fn development_files_stream_in_chunks_and_detect_mutation() {
    use base64::Engine;
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = many(&mut input, 0, true);
    let bytes: Vec<u8> = (0..15000).map(|n| (n % 251) as u8).collect();
    t.store.files.insert("/binary.dat".into(), bytes.clone());
    let mut responses = t.command(1, "storage.read", json!({"path":"/binary.dat"}));
    while !responses.iter().any(|r| r["id"] == 1 && r["done"] == true) {
        t.poll();
        responses.extend(t.drain());
        assert!(t.now < 1000);
    }
    let mut decoded = Vec::new();
    for response in responses.iter().filter(|r| r["id"] == 1) {
        assert_eq!(response["ok"], true);
        if response["done"] == true {
            assert_eq!(response["result"]["bytes"], bytes.len());
        } else {
            assert_eq!(response["result"]["offset"], decoded.len());
            let chunk = base64::engine::general_purpose::STANDARD
                .decode(response["result"]["data"].as_str().unwrap())
                .unwrap();
            assert!(chunk.len() <= 512);
            decoded.extend_from_slice(&chunk);
        }
    }
    assert_eq!(decoded, bytes);
    let rows = t.command(2, "storage.list", json!({"path":"/"}));
    assert_eq!(rows[0]["result"]["name"], "binary.dat");
    assert_eq!(rows[1]["result"]["count"], 1);
    let start = t.command(3, "storage.read", json!({"path":"/binary.dat"}));
    assert!(start.iter().all(|r| r["done"] == false));
    t.store.generation += 1;
    t.poll();
    assert_eq!(t.drain()[0]["error"]["code"], "storage_changed");
    t.store.files.insert("/empty".into(), vec![]);
    let empty = t.command(4, "storage.read", json!({"path":"/empty"}));
    assert_eq!(empty.len(), 1);
    assert_eq!(empty[0]["result"]["bytes"], 0);
    t.command(5, "storage.read", json!({"path":"/binary.dat"}));
    let cancelled = t.command(6, "request.cancel", json!({"request_id":5}));
    assert!(
        cancelled
            .iter()
            .any(|r| r["id"] == 5 && r["error"]["code"] == "cancelled")
    );
}
#[test]
fn production_profile_does_not_offer_or_accept_filesystem_commands() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = many(&mut input, 0, false);
    let status = t.command(1, "adapter.status", json!({}));
    assert!(status[0]["result"].get("commands").is_none());
    assert!(status[0]["result"].get("transports").is_none());
    let caps = t.command(2, "adapter.capabilities", json!({}));
    assert!(
        !caps[0]["result"]
            .as_array()
            .unwrap()
            .contains(&json!("debug"))
    );
    assert!(
        !caps[0]["result"]
            .as_array()
            .unwrap()
            .contains(&json!("storage_management"))
    );
    for (id, cmd) in [(3, "storage.read"), (4, "storage.list")] {
        let response = t.command(id, cmd, json!({"path":"/"}));
        assert_eq!(response[0]["error"]["code"], "unknown_command");
    }
}

#[test]
fn devices_aborts_when_the_stream_revision_changes() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = many(&mut input, 20, false);
    for device in t.app.manager.devices.iter_mut().flatten() {
        device.paused = true;
    }
    let start = t.command(1, "device.list", json!({}));
    assert!(start.iter().any(|r| r["done"] == false));
    assert!(!start.iter().any(|r| r["done"] == true));
    t.app.manager.revision += 1;
    t.poll();
    let responses = t.drain();
    assert!(
        responses
            .iter()
            .any(|r| r["id"] == 1 && r["error"]["code"] == "busy")
    );
}

#[cfg(feature = "development")]
#[test]
fn capabilities_and_files_survive_radio_failure() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = many(&mut input, 0, true);
    let before = t.command(1, "adapter.capabilities", json!({}));
    assert_eq!(
        before[0]["result"],
        json!(["classic", "ble", "debug", "storage_management"])
    );
    t.event(Event::Failed(ErrorCode::RadioUnavailable));
    let after = t.command(2, "adapter.capabilities", json!({}));
    assert_eq!(after[0]["result"], before[0]["result"]);
    assert!(!t.app.status(&t.radio, t.now).radio_ready);
    let files = t.command(3, "storage.list", json!({"path":"/"}));
    assert!(files.last().unwrap()["ok"].as_bool().unwrap(), "{files:?}");
}

#[test]
fn connect_reports_full_connection_capacity_with_typed_details() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = many(&mut input, 5, false);
    for slot in 0..4 {
        t.app
            .manager
            .connect(slot, true, 30_000, &mut t.radio)
            .unwrap();
    }
    let device_id = t.app.manager.devices[4]
        .as_ref()
        .unwrap()
        .policy
        .device_id();
    let response = t.command(1, "device.connect", json!({"device_id":device_id}));
    assert_eq!(
        response[0]["error"],
        json!({"code":"capacity","details":{"reason":"connections_full"}})
    );
}

#[cfg(feature = "development")]
#[test]
fn storage_backpressure_leaves_control_response_capacity() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = many(&mut input, 0, true);
    t.store.files.insert("/large".into(), vec![42; 256 * 1024]);
    t.commands.insert(1, "storage.read".into());
    let (_, request) = t.app.serial.feed(
        b"{\"v\":1,\"id\":1,\"cmd\":\"storage.read\",\"args\":{\"path\":\"/large\"}}\n",
        1,
    );
    block_on(
        t.app
            .dispatch(&request.unwrap(), &mut t.store, &mut t.radio, 1),
    )
    .unwrap();
    for _ in 0..32 {
        t.poll();
    }
    assert_eq!(
        t.app.serial.queued(),
        cordial_core::control::OUTPUT_FRAMES - 2,
        "bulk output must keep two control slots free"
    );
    // Still below the distinct five-second USB output deadline.
    t.now = 4_500;
    let replies = t.command(2, "session.heartbeat", json!({}));
    assert!(replies.iter().any(|r| r["id"] == 2 && r["ok"] == true));
    assert!(!replies.iter().any(|r| r["id"] == 1 && r["done"] == true));
    let replies = t.command(3, "adapter.capabilities", json!({}));
    assert!(replies.iter().any(|r| r["id"] == 3 && r["ok"] == true));
    let replies = t.command(4, "request.cancel", json!({"request_id":1}));
    assert!(replies.iter().any(|r| r["id"] == 4 && r["ok"] == true));
    assert!(
        replies
            .iter()
            .any(|r| r["id"] == 1 && r["error"]["code"] == "cancelled")
    );
}

#[test]
fn native_diagnostics_are_available_only_in_development() {
    use cordial_protocol::messages::{AuthenticationFailure, NimbleAuthenticationStage};
    let mut input = [0; MAX_LINE_BYTES];
    let mut t = Test::new(&mut input, false);
    let failure = AuthenticationFailure::Nimble {
        attempt: 42,
        stage: NimbleAuthenticationStage::Encryption,
        status: 0x40b,
        encrypted: false,
        bonded: false,
    };
    t.radio.authentication_failure = Some(failure);
    t.radio.gatt_writes = Some(vec![cordial_protocol::messages::GattWriteDiagnostic {
        token: 42,
        request: 100,
        handle: 61,
        response: true,
        accepted: true,
        queued_ms: 1,
        started_ms: Some(2),
        completed_ms: None,
        status: None,
    }]);
    let status = t.app.status(&t.radio, t.now);
    assert_eq!(
        status.authentication_failure,
        if cfg!(feature = "development") {
            Some(failure)
        } else {
            None
        }
    );
    let json = serde_json::to_value(status).unwrap();
    if cfg!(feature = "development") {
        assert_eq!(json["gatt_writes"][0]["accepted"], true);
        assert_eq!(json["gatt_writes"][0]["request"], 100);
        assert_eq!(json["gatt_writes"][0]["started_ms"], 2);
        assert!(json["gatt_writes"][0]["completed_ms"].is_null());
        assert_eq!(json["authentication_failure"]["status"], 0x40b);
        assert_eq!(json["authentication_failure"]["stage"], "encryption");
    } else {
        assert!(json.get("authentication_failure").is_none());
        assert!(json.get("gatt_writes").is_none());
    }
}

#[test]
fn discovery_kind_updates_and_survives_reports_without_metadata() {
    use cordial_protocol::messages::DeviceKind;
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.command(
        1,
        "discovery.scan",
        json!({"transport":"ble","duration_ms":0}),
    );
    let scan = t.radio.scans.last().unwrap().0;
    let peer = Peer {
        transport: Transport::Ble,
        ..peer(1)
    };
    for (kind, name, expected) in [
        (DeviceKind::Unknown, "", "unknown"),
        (DeviceKind::Keyboard, "", "keyboard"),
        (DeviceKind::Unknown, "Keyboard name arrived", "keyboard"),
    ] {
        t.event(Event::Found {
            scan,
            peer,
            address: Some(peer),
            connectable: true,
            kind,
            name: name.into(),
            rssi: Some(-40),
        });
        t.poll();
        let messages = t.drain();
        let found = messages
            .iter()
            .find(|m| m["event"] == "discovery.result")
            .unwrap();
        assert_eq!(found["data"]["candidate_id"], "c_1");
        assert_eq!(found["data"]["kind"], expected);
    }
}

#[test]
fn battery_events_are_delta_only_and_never_write_storage() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, true);
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 64,
    });
    t.app.manager.devices[0]
        .as_mut()
        .unwrap()
        .catalog
        .info
        .battery
        .configure(cordial_protocol::identifiers::Transport::Ble, false);
    t.command(1, "session.monitor.set", json!({"enabled":true}));
    for _ in 0..5 {
        t.poll();
        t.drain();
    }
    let stored = t.store.records.clone();
    let generation = t.store.generation;
    t.event(Event::Information {
        success: true,
        link,
        uuid: 0x2a26,
        instance: 0,
        bytes: Box::from(&b"1.2.3"[..]),
    });
    t.event(Event::Information {
        success: true,
        link,
        uuid: 0x2a19,
        instance: 0,
        bytes: Box::from([51]),
    });
    t.poll();
    t.drain();
    t.event(Event::Information {
        success: true,
        link,
        uuid: 0x2a19,
        instance: 0,
        bytes: Box::from([50]),
    });
    t.poll();
    let events = t.drain();
    assert_eq!(
        events.len(),
        1,
        "battery alone must not emit device/settings snapshots: {events:?}"
    );
    assert_eq!(events[0]["event"], "device.info.changed");
    assert_eq!(events[0]["data"]["fields"].as_array().unwrap().len(), 1);
    assert_eq!(events[0]["data"]["fields"][0]["key"], "battery_percent");
    assert_eq!(events[0]["data"]["fields"][0]["value"], 50);
    let responses = t.command(2, "device.info", json!({"device_id":"d_000000000000004d"}));
    assert!(
        responses.iter().any(
            |r| r["result"]["fields"].as_array().is_some_and(|fields| fields
                .iter()
                .any(|f| f["key"] == "firmware" && f["value"] == "1.2.3"))
        )
    );
    assert_eq!(
        t.store.generation, generation,
        "no write/delete operation is allowed for battery information"
    );
    assert_eq!(t.store.records, stored);
    // A callback from an old connection must not alter the current snapshot.
    t.event(Event::Information {
        success: true,
        link: cordial_core::link::LinkId {
            generation: link.generation + 1,
            ..link
        },
        uuid: 0x2a19,
        instance: 0,
        bytes: Box::from([99]),
    });
    t.poll();
    assert!(t.drain().is_empty());
}

#[test]
fn adapter_names_are_persisted_before_publication_and_survive_reload() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    assert_eq!(t.app.status(&t.radio, t.now).name, "Test adapter");
    t.command(1, "session.monitor.set", json!({"enabled":true}));
    let revision = t.app.manager.revision;
    let rows = t.command(
        2,
        "adapter.name.set",
        json!({"name":"  Office \u{10400}  "}),
    );
    let event = rows
        .iter()
        .find(|r| r["event"] == "adapter.changed")
        .unwrap();
    let result = &rows.iter().find(|r| r["id"] == 2).unwrap()["result"];
    assert_eq!(&event["data"], result);
    assert_eq!(result["name"], "Office \u{10400}");
    assert_eq!(result["revision"], revision + 1);
    assert_eq!(result["host_platform"], "linux");
    let generation = t.store.generation;
    let rows = t.command(3, "adapter.name.set", json!({"name":"Office \u{10400}"}));
    assert_eq!(rows.len(), 1);
    assert_eq!(t.store.generation, generation);
    assert_eq!(t.app.manager.revision, revision + 1);
    t.command(4, "adapter.platform.set", json!({"platform":"mac"}));
    assert_eq!(t.app.status(&t.radio, t.now).name, "Office \u{10400}");
    t.store.fail_save = Some((cordial_core::storage::record_key(1, 0), false));
    let rows = t.command(5, "adapter.name.set", json!({"name":"Failed"}));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["error"]["code"], "storage_failed");
    assert_eq!(t.app.status(&t.radio, t.now).name, "Office \u{10400}");
    t.store.fail_save = None;
    t.command(6, "adapter.name.set", json!({"name":"Desk"}));
    assert_eq!(
        t.app.status(&t.radio, t.now).host_platform,
        cordial_protocol::identifiers::HostPlatform::Mac
    );
    let mut reboot_input = [0; MAX_LINE_BYTES - 1];
    let mut reboot = Test::new(&mut reboot_input, false);
    reboot.store = t.store;
    reboot.app.manager = Default::default();
    reboot.event(Event::Ready);
    let status = reboot.app.status(&reboot.radio, reboot.now);
    assert_eq!(status.name, "Desk");
    assert_eq!(
        status.host_platform,
        cordial_protocol::identifiers::HostPlatform::Mac
    );
}

#[test]
fn adapter_name_validation_checks_utf8_bytes_and_storage_readiness() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    let generation = t.store.generation;
    for (i, name) in [
        "".into(),
        "   ".into(),
        "x\ny".into(),
        "x\u{85}y".into(),
        "é".repeat(33),
    ]
    .iter()
    .enumerate()
    {
        let rows = t.command(i as u32 + 1, "adapter.name.set", json!({"name":name}));
        assert_eq!(rows[0]["error"]["code"], "invalid_args");
    }
    assert_eq!(t.store.generation, generation);
    let rows = t.command(6, "adapter.name.set", json!({"name":"é".repeat(32)}));
    assert_eq!(rows[0]["result"]["name"], "é".repeat(32));
    t.app.manager.storage_ready = false;
    let rows = t.command(7, "adapter.name.set", json!({"name":"Desk"}));
    assert_eq!(rows[0]["error"]["code"], "storage_failed");
}

#[test]
fn null_name_clears_the_override_without_losing_platform() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.command(1, "adapter.platform.set", json!({"platform":"windows"}));
    t.command(2, "adapter.name.set", json!({"name":"Desk"}));
    t.store.fail_save = Some((cordial_core::storage::record_key(1, 0), false));
    let rows = t.command(3, "adapter.name.set", json!({"name":null}));
    assert_eq!(rows[0]["error"]["code"], "storage_failed");
    assert_eq!(t.app.status(&t.radio, t.now).name, "Desk");
    t.store.fail_save = None;
    // An explicit name matching today's image default must still be cleared.
    t.command(4, "adapter.name.set", json!({"name":"Test adapter"}));
    assert_eq!(
        t.app.manager.preference.name.as_deref(),
        Some("Test adapter")
    );
    let generation = t.store.generation;
    let rows = t.command(5, "adapter.name.set", json!({"name":null}));
    assert_eq!(rows[0]["result"]["name"], "Test adapter");
    assert_eq!(rows[0]["result"]["host_platform"], "windows");
    assert_eq!(t.store.generation, generation + 1);
    let saved = block_on(
        cordial_core::devices::Policies {
            store: &mut t.store,
        }
        .load_adapter(),
    )
    .unwrap();
    assert_eq!(saved.name, None);
    assert_eq!(
        saved.host_platform,
        cordial_protocol::identifiers::HostPlatform::Windows
    );
    t.app.manager = Default::default();
    t.event(Event::Ready);
    assert_eq!(t.app.status(&t.radio, t.now).name, "Test adapter");
    let generation = t.store.generation;
    let revision = t.app.manager.revision;
    t.command(6, "adapter.name.set", json!({"name":null}));
    assert_eq!(t.store.generation, generation);
    assert_eq!(t.app.manager.revision, revision);
}

#[test]
fn protocol_discovery_works_before_readiness_and_shares_request_ids() {
    let mut input = [0; MAX_LINE_BYTES - 1];
    let mut t = Test::new(&mut input, false);
    t.app.manager.storage_ready = false;
    t.app.manager.radio_ready = false;
    for (i, args) in [
        Value::Null,
        json!({}),
        json!({"future":{"values":[1,true,null]}}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = i as u32 + 1;
        let replies = t.command(id, "adapter.protocol", args);
        assert_eq!(
            replies,
            vec![
                json!({"v":0,"type":"response","id":id,"ok":true,"done":true,"result":{"protocol":1}})
            ]
        );
    }
    let (_, request) = t
        .app
        .serial
        .feed(b"{\"v\":0,\"id\":4,\"cmd\":\"adapter.protocol\"}\n", t.now);
    block_on(
        t.app
            .dispatch(&request.unwrap(), &mut t.store, &mut t.radio, t.now),
    )
    .unwrap();
    assert_eq!(t.drain()[0]["v"], 0);
    let (_, request) = t.app.serial.feed(
        b"{\"v\":1,\"id\":4,\"cmd\":\"adapter.status\",\"args\":{}}\n",
        t.now,
    );
    assert!(request.is_none());
    assert_eq!(t.drain()[0]["data"]["code"], "invalid_request");
    assert_eq!(t.command(5, "adapter.status", json!({}))[0]["v"], 1);
}
