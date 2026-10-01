mod support;
use cordial_core::model::{
    errors::ErrorCode,
    hidpp::{FeatureId, FeatureRevision},
    identifiers::{ConnectionState, Transport},
    link::{DeviceKind, PromptMethod},
    settings::{SettingKey, SettingScope},
};
use cordial_core::{
    application::{Application, Bootloader, Build},
    bluetooth::{Event, InputReport},
    compact::{Metadata, Preference},
    devices::{Device as SavedDevice, Peer, Policy},
    link::ServiceId,
    settings::PreferenceStore,
    storage::Preferences,
};
use cordial_protocol::{
    self as p,
    event::Kind as Ev,
    frame::{self, Decoder},
    message::Kind as M,
    request::Command,
    response::Result as R,
};
use embassy_futures::block_on;
use prost::Message;
use support::*;

const SAVED: &str = "d_000000000000004d";

fn build(development: bool, bootloader: Option<fn() -> !>) -> Build {
    Build {
        development,
        version: "test",
        board: "test_board",
        default_adapter_name: "Test adapter",
        adapter_id: "adapter".into(),
        bootloader: bootloader.map(|enter| Bootloader { enter }),
    }
}

struct Test {
    app: Application,
    store: Store,
    radio: Radio,
    now: u64,
    decoder: Decoder,
}
impl Test {
    fn new(paired: bool) -> Self {
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
        Self::with(store, radio, build(false, None))
    }
    fn with(store: Store, radio: Radio, build: Build) -> Self {
        let mut t = Self {
            app: Application::new(build),
            store,
            radio,
            now: 0,
            decoder: Decoder::new(None),
        };
        t.app.session(true, &mut t.radio);
        t.event(Event::Ready);
        t
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
    /// Every complete frame written so far.
    fn output(&mut self) -> Vec<M> {
        let mut messages = Vec::new();
        while let Some((token, data)) = self.app.serial.output(64) {
            let data = data.to_vec();
            self.app.serial.output_complete(token, data.len());
            for byte in data {
                if let Some(frame) = self.decoder.push(byte) {
                    let message = p::Message::decode(frame.unwrap()).unwrap();
                    messages.push(message.kind.unwrap());
                }
            }
        }
        messages
    }
    /// Polls until nothing more is written, returning the events.
    fn events(&mut self) -> Vec<Ev> {
        let mut events = Vec::new();
        let mut quiet = 0;
        while quiet < 3 {
            self.poll();
            let out = self.output();
            if out.is_empty() {
                quiet += 1;
            }
            for m in out {
                match m {
                    M::Event(e) => events.push(e.kind.unwrap()),
                    M::Response(r) => panic!("unexpected response {r:?}"),
                }
            }
        }
        events
    }
    fn send(&mut self, bytes: &[u8]) -> Option<p::Request> {
        let (n, request) = self.app.serial.feed(bytes);
        assert_eq!(n, bytes.len());
        request
    }
    fn response(&mut self) -> p::Response {
        for m in self.output() {
            if let M::Response(r) = m {
                return r;
            }
        }
        panic!("no response")
    }
    fn request(&mut self, command: Command) -> p::Response {
        self.now += 1;
        let mut bytes = Vec::new();
        frame::encode(
            &p::Request {
                command: Some(command),
            },
            &mut bytes,
        );
        let request = self.send(&bytes).unwrap();
        block_on(
            self.app
                .dispatch(request, &mut self.store, &mut self.radio, self.now),
        );
        self.response()
    }
    fn ok(&mut self, command: Command) -> Option<R> {
        match self.request(command).result {
            Some(R::Error(e)) => panic!("request failed: {e:?}"),
            result => result,
        }
    }
    fn error(&mut self, command: Command) -> p::Error {
        match self.request(command).result {
            Some(R::Error(e)) => e,
            result => panic!("request succeeded: {result:?}"),
        }
    }
    fn code(&mut self, command: Command) -> p::ErrorCode {
        p::ErrorCode::try_from(self.error(command).code).unwrap()
    }
    fn status(&mut self) -> p::Status {
        match self.ok(Command::GetStatus(p::GetStatus {})) {
            Some(R::Status(s)) => s,
            other => panic!("{other:?}"),
        }
    }
    fn device(&mut self, id: &str) -> p::Device {
        match self.ok(Command::GetDevice(p::GetDevice { device: id.into() })) {
            Some(R::Device(d)) => d,
            other => panic!("{other:?}"),
        }
    }
    fn settings(&mut self, id: &str) -> Vec<p::Setting> {
        match self.ok(Command::ListSettings(p::ListSettings { device: id.into() })) {
            Some(R::Settings(s)) => s.settings,
            other => panic!("{other:?}"),
        }
    }
    fn scan(&mut self, transports: &[p::Transport]) {
        self.ok(Command::StartScan(p::StartScan {
            transports: transports.iter().map(|t| *t as i32).collect(),
            seconds: 60,
        }));
    }
    fn found(&mut self, peer: Peer, address: Option<Peer>, name: &str) {
        let scan = self.radio.scans.last().unwrap().0;
        self.event(Event::Found {
            kind: DeviceKind::Unknown,
            scan,
            peer,
            address,
            connectable: true,
            name: name.into(),
            rssi: Some(-40),
        });
    }
    fn candidate(&mut self, identity: Peer, address: Peer) -> String {
        self.scan(&[p::Transport::Classic, p::Transport::Ble]);
        self.found(identity, Some(address), "Keyboard");
        self.events()
            .into_iter()
            .find_map(|e| match e {
                Ev::ScanFound(c) => Some(c.id),
                _ => None,
            })
            .unwrap()
    }
    fn pair(&mut self, candidate: &str) {
        self.ok(Command::StartPairing(p::StartPairing {
            candidate: candidate.into(),
        }));
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

fn pairing_steps(events: &[Ev]) -> Vec<p::pairing::Step> {
    events
        .iter()
        .filter_map(|e| match e {
            Ev::Pairing(p) => p.step.clone(),
            _ => None,
        })
        .collect()
}

fn info(device: &p::Device, key: &str) -> Option<p::value::Value> {
    device
        .info
        .iter()
        .find(|i| i.key == key)
        .and_then(|i| i.value.clone()?.value)
}

fn hidpp(device: &p::Device) -> &p::Integration {
    device
        .integrations
        .iter()
        .find(|i| i.kind == p::IntegrationKind::Hidpp as i32)
        .unwrap()
}

fn hidpp_state(device: &p::Device) -> p::integration::Status {
    hidpp(device).status.unwrap()
}

fn preference(key: SettingKey, feature: FeatureId, value: u16) -> Preference {
    Preference {
        metadata: Metadata {
            key,
            feature,
            revision: FeatureRevision(3),
            scope: SettingScope::Device,
            choices: Box::new([]),
            range: None,
        },
        value,
    }
}

#[test]
fn status_reports_identity_transports_and_facts() {
    let mut t = Test::new(false);
    let status = t.status();
    assert_eq!(status.id, "adapter");
    assert_eq!(status.name, "Test adapter");
    assert!(status.ready);
    assert_eq!(status.platform, p::Platform::Linux as i32);
    assert_eq!(
        status.transports,
        vec![
            p::TransportSupport {
                transport: p::Transport::Classic as i32,
                max_enabled: Some(7),
            },
            p::TransportSupport {
                transport: p::Transport::Ble as i32,
                max_enabled: Some(7),
            },
        ]
    );
    let facts: Vec<_> = status.info.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(
        facts,
        [p::keys::FIRMWARE_VERSION, p::keys::BOARD_NAME],
        "production firmware with free storage reports neither development nor storage.full"
    );
}

#[test]
fn requests_are_answered_in_order_and_bad_frames_get_errors() {
    let mut t = Test::new(false);
    // Bytes left by an earlier client end at the first delimiter.
    let mut bytes = vec![7, 7, 7, 0];
    frame::encode(
        &p::Request {
            command: Some(Command::GetStatus(p::GetStatus {})),
        },
        &mut bytes,
    );
    let (n, request) = t.app.serial.feed(&bytes);
    assert!(request.is_none());
    // The malformed leftover is answered; nothing more is read until that answer is written.
    assert_eq!(n, 4);
    let error = |r: p::Response| match r.result {
        Some(R::Error(e)) => p::ErrorCode::try_from(e.code).unwrap(),
        other => panic!("{other:?}"),
    };
    assert_eq!(error(t.response()), p::ErrorCode::BadRequest);
    let request = t.send(&bytes[4..]).unwrap();
    block_on(t.app.dispatch(request, &mut t.store, &mut t.radio, 1));
    assert!(matches!(t.response().result, Some(R::Status(_))));
    // A request whose command this firmware does not know.
    let mut unknown = Vec::new();
    let mut body = Vec::new();
    prost::encoding::message::encode(99, &p::GetStatus {}, &mut body);
    frame::encode_bytes(&body, &mut unknown);
    assert!(t.app.serial.feed(&unknown).1.is_none());
    assert_eq!(error(t.response()), p::ErrorCode::UnknownCommand);
    let mut long = Vec::new();
    frame::encode_bytes(&[1; cordial_protocol::MAX_REQUEST_BYTES + 1], &mut long);
    assert!(t.app.serial.feed(&long).1.is_none());
    assert_eq!(error(t.response()), p::ErrorCode::TooLong);
}

#[test]
fn devices_report_the_saved_record() {
    let mut t = Test::new(true);
    let d = t.device(SAVED);
    assert_eq!(d.name, "Keyboard");
    assert_eq!(d.transport, p::Transport::Classic as i32);
    assert!(d.enabled && d.trusted && !d.blocked && !d.paused);
    assert_eq!(d.inactive, None);
    let integration = hidpp(&d);
    assert!(integration.enabled);
    assert_eq!(
        integration.status,
        Some(p::integration::Status::State(
            p::IntegrationState::Disconnected as i32
        ))
    );
    match t.ok(Command::ListDevices(p::ListDevices {})) {
        Some(R::Devices(list)) => assert_eq!(list.devices, vec![d]),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        t.code(Command::GetDevice(p::GetDevice {
            device: "d_missing".into()
        })),
        p::ErrorCode::NotFound
    );
}

#[test]
fn set_device_changes_only_the_fields_sent() {
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    let d = match t.ok(Command::SetDevice(p::SetDevice {
        device: SAVED.into(),
        trusted: Some(false),
        ..Default::default()
    })) {
        Some(R::Device(d)) => d,
        other => panic!("{other:?}"),
    };
    assert!(!d.trusted && d.enabled && !d.blocked && hidpp(&d).enabled);
    let d = match t.ok(Command::SetDevice(p::SetDevice {
        device: SAVED.into(),
        integrations: vec![p::IntegrationUpdate {
            kind: p::IntegrationKind::Hidpp as i32,
            enabled: Some(false),
        }],
        ..Default::default()
    })) {
        Some(R::Device(d)) => d,
        other => panic!("{other:?}"),
    };
    assert!(
        !d.trusted,
        "an earlier change survives a later partial update"
    );
    // HID++ is listed while turned off only once it has been detected.
    assert!(d.integrations.is_empty());
    let policy = &t.app.manager.devices[0].as_ref().unwrap().policy;
    assert!(!policy.trusted && !policy.hidpp_enabled);
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED.into(),
            integrations: vec![p::IntegrationUpdate {
                kind: 42,
                enabled: Some(true),
            }],
            ..Default::default()
        })),
        p::ErrorCode::Unsupported
    );
    let twice = p::IntegrationUpdate {
        kind: p::IntegrationKind::Hidpp as i32,
        enabled: Some(true),
    };
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED.into(),
            integrations: vec![twice, twice],
            ..Default::default()
        })),
        p::ErrorCode::BadArgs
    );
}

#[test]
fn enabling_past_the_stack_limit_is_refused() {
    let mut t = Test::new(true);
    for n in 2..=8 {
        t.add_saved(n);
    }
    t.app.manager.refresh_enabled();
    let ids: Vec<_> = t
        .app
        .manager
        .devices
        .iter()
        .flatten()
        .map(|d| d.policy.device_id().0)
        .collect();
    let last = ids.last().unwrap().clone();
    // Eight saved Classic devices: seven fit the stack's table, the eighth waits.
    assert_eq!(
        t.device(&last).inactive,
        Some(p::InactiveReason::Capacity as i32)
    );
    t.ok(Command::SetDevice(p::SetDevice {
        device: last.clone(),
        enabled: Some(false),
        ..Default::default()
    }));
    let error = t.error(Command::SetDevice(p::SetDevice {
        device: last,
        enabled: Some(true),
        ..Default::default()
    }));
    assert_eq!(error.code, p::ErrorCode::NoCapacity as i32);
    assert_eq!(error.reason, p::CapacityReason::Enabled as i32);
}

#[test]
fn scans_validate_their_transports_and_report_candidates() {
    let mut t = Test::new(false);
    t.radio.transports = Some(cordial_core::bluetooth::Capabilities {
        classic: false,
        ble: true,
        ble_scan_and_connect: false,
    });
    for (transports, seconds, code) in [
        (vec![], 0, p::ErrorCode::BadArgs),
        (vec![p::Transport::Ble as i32], 61, p::ErrorCode::BadArgs),
        (
            vec![p::Transport::Classic as i32],
            0,
            p::ErrorCode::Unsupported,
        ),
        (vec![7], 0, p::ErrorCode::Unsupported),
    ] {
        assert_eq!(
            t.code(Command::StartScan(p::StartScan {
                transports,
                seconds
            })),
            code
        );
    }
    assert!(t.radio.scans.is_empty());
    t.ok(Command::StartScan(p::StartScan {
        transports: vec![p::Transport::Ble as i32],
        seconds: 0,
    }));
    let (token, classic, ble) = *t.radio.scans.last().unwrap();
    assert!(!classic && ble);
    let ble_peer = Peer {
        transport: Transport::Ble,
        ..peer(3)
    };
    t.found(ble_peer, Some(ble_peer), "Mouse");
    t.found(ble_peer, Some(ble_peer), "Mouse");
    let found: Vec<_> = t
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Ev::ScanFound(c) => Some(c),
            _ => None,
        })
        .collect();
    // A repeated advertisement with nothing new is not reported again.
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "Mouse");
    assert_eq!(found[0].rssi, Some(-40));
    t.now += 10_000;
    let events = t.events();
    assert!(events.contains(&Ev::ScanDone(p::ScanDone {
        count: 1,
        truncated: false
    })));
    assert_eq!(t.radio.scans.last(), Some(&(token, false, false)));
    // Stopping an idle scan answers and reports nothing.
    t.ok(Command::StopScan(p::StopScan {}));
    assert!(t.events().is_empty());
}

#[test]
fn identity_only_presence_is_not_a_fresh_pair_candidate() {
    let mut t = Test::new(false);
    t.scan(&[p::Transport::Ble]);
    let identity = Peer {
        transport: Transport::Ble,
        ..peer(1)
    };
    t.found(identity, None, "Keyboard");
    assert!(!t.events().iter().any(|e| matches!(e, Ev::ScanFound(_))));
    t.found(
        identity,
        Some(Peer {
            random: true,
            ..identity
        }),
        "Keyboard",
    );
    assert!(t.events().iter().any(|e| matches!(e, Ev::ScanFound(_))));
}

#[test]
fn pairing_saves_the_device_and_forwarding_survives_the_session() {
    let mut t = Test::new(false);
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(&candidate);
    t.poll();
    let link = t.radio.connects[0].0;
    assert!(t.radio.connects[0].1, "a pairing link");
    t.event(Event::Prompt {
        link,
        method: PromptMethod::EnterPasskey,
        value: None,
    });
    let steps = pairing_steps(&t.events());
    assert_eq!(
        steps,
        [
            p::pairing::Step::Connecting(p::PairingConnecting {}),
            p::pairing::Step::EnterCode(p::EnterCode {
                kind: p::CodeKind::Passkey as i32
            })
        ]
    );
    assert_eq!(
        t.code(Command::AcceptPrompt(p::AcceptPrompt {
            value: "12".into()
        })),
        p::ErrorCode::BadArgs
    );
    t.ok(Command::AcceptPrompt(p::AcceptPrompt {
        value: "001234".into(),
    }));
    t.radio.bonds.push(peer(2));
    t.event(Event::Bonded {
        link,
        identity: peer(2),
    });
    let events = t.events();
    let device = t.app.manager.devices[0]
        .as_ref()
        .unwrap()
        .policy
        .device_id()
        .0;
    assert_eq!(
        pairing_steps(&events),
        [p::pairing::Step::Done(p::PairingDone {
            device: device.clone()
        })]
    );
    let record = events
        .iter()
        .find_map(|e| match e {
            Ev::Device(d) => Some(d),
            _ => None,
        })
        .unwrap();
    assert_eq!(record.id, device);
    assert!(record.integrations.is_empty(), "HID++ starts off");
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
    t.app.session(false, &mut t.radio);
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes()[0], 16);
    t.app.manager.forward.complete();
    t.event(Event::Failed(ErrorCode::RadioUnavailable));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes(), &[0; 32]);
}

#[test]
fn prompts_and_pairings_are_checked() {
    let mut t = Test::new(false);
    assert_eq!(
        t.code(Command::RejectPrompt(p::RejectPrompt {})),
        p::ErrorCode::NoPrompt
    );
    assert_eq!(
        t.code(Command::StartPairing(p::StartPairing {
            candidate: "c_9".into()
        })),
        p::ErrorCode::NotFound
    );
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(&candidate);
    assert_eq!(
        t.code(Command::StartPairing(p::StartPairing { candidate })),
        p::ErrorCode::Busy
    );
    t.poll();
    assert_eq!(
        t.code(Command::AcceptPrompt(p::AcceptPrompt::default())),
        p::ErrorCode::NoPrompt
    );
    let link = t.radio.connects[0].0;
    t.event(Event::Prompt {
        link,
        method: PromptMethod::ConfirmPasskey,
        value: Some("042731".into()),
    });
    assert_eq!(
        pairing_steps(&t.events()).last(),
        Some(&p::pairing::Step::ConfirmCode(p::ConfirmCode {
            passkey: "042731".into()
        }))
    );
    t.ok(Command::RejectPrompt(p::RejectPrompt {}));
    t.finish_disconnects();
    assert_eq!(
        pairing_steps(&t.events()).last(),
        Some(&p::pairing::Step::Failed(p::ErrorCode::Rejected as i32))
    );
    assert!(t.app.manager.devices.iter().all(Option::is_none));
}

#[test]
fn ending_the_session_stops_scanning_and_pairing() {
    let mut t = Test::new(false);
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(&candidate);
    t.poll();
    let link = t.radio.connects[0].0;
    t.app.session(false, &mut t.radio);
    assert_eq!(
        t.radio.scans.last().map(|s| (s.1, s.2)),
        Some((false, false))
    );
    t.radio.bonds.push(peer(2));
    t.event(Event::Bonded {
        link,
        identity: peer(2),
    });
    assert!(t.app.manager.devices.iter().all(Option::is_none));
    t.event(Event::Disconnected { link, error: None });
    t.poll();
    assert!(t.radio.bonds.is_empty());
    t.app.session(true, &mut t.radio);
    assert_eq!(
        t.code(Command::StartPairing(p::StartPairing { candidate })),
        p::ErrorCode::NotFound
    );
}

#[test]
fn duplicate_identity_keeps_existing_device() {
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
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
    move_saved(&mut t, identity);
    let candidate = t.candidate(rpa, rpa);
    t.pair(&candidate);
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Bonded { link, identity });
    let steps = pairing_steps(&t.events());
    assert_eq!(
        steps.last(),
        Some(&p::pairing::Step::Done(p::PairingDone {
            device: SAVED.into()
        }))
    );
    assert_eq!(t.app.manager.devices.iter().flatten().count(), 1);
}

#[test]
fn failed_stranger_bond_cleanup_is_retried_before_the_next_pair() {
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.add_saved(2);
    let candidate = t.candidate(peer(1), peer(1));
    t.pair(&candidate);
    t.poll();
    let link = t.radio.connects.last().unwrap().0;
    let stranger = peer(9);
    t.radio.bonds.push(stranger);
    t.radio.reject_forget = true;
    t.event(Event::Bonded {
        link,
        identity: stranger,
    });
    t.finish_disconnects();
    assert_eq!(
        pairing_steps(&t.events()).last(),
        Some(&p::pairing::Step::Failed(
            p::ErrorCode::StorageFailed as i32
        ))
    );
    assert!(t.radio.bonds.contains(&stranger));
    t.radio.reject_forget = false;
    t.pair(&candidate);
    t.poll();
    assert!(!t.radio.bonds.contains(&stranger));
    assert!(t.radio.forgotten.contains(&stranger));
    assert!(t.radio.bonds.contains(&peer(2)));
    assert!(t.radio.connects.last().unwrap().1);
}

#[test]
fn pairing_with_full_storage_has_a_reason_and_scanning_stays_available() {
    let mut t = Test::new(false);
    t.store.available = Some(0);
    let candidate = t.candidate(peer(2), peer(2));
    let error = t.error(Command::StartPairing(p::StartPairing { candidate }));
    assert_eq!(error.code, p::ErrorCode::NoCapacity as i32);
    assert_eq!(error.reason, p::CapacityReason::Storage as i32);
    assert!(t.radio.connects.is_empty());
    assert!(
        t.status()
            .info
            .iter()
            .any(|i| i.key == p::keys::STORAGE_FULL)
    );
}

#[test]
fn connect_and_disconnect_respond_with_the_device() {
    let mut t = Test::new(true);
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    // An explicit connect joins the automatic attempt already running.
    match t.ok(Command::ConnectDevice(p::ConnectDevice {
        device: SAVED.into(),
    })) {
        Some(R::Device(d)) => assert_eq!(d.state, p::DeviceState::Connecting as i32),
        other => panic!("{other:?}"),
    }
    assert_eq!(t.radio.connects.len(), 1);
    match t.ok(Command::DisconnectDevice(p::DisconnectDevice {
        device: SAVED.into(),
    })) {
        Some(R::Device(d)) => assert!(d.paused),
        other => panic!("{other:?}"),
    }
    assert_eq!(t.radio.closes.len(), 1);
}

#[test]
fn a_connect_that_times_out_keeps_automatic_reconnection() {
    let mut t = Test::new(true);
    t.ok(Command::ConnectDevice(p::ConnectDevice {
        device: SAVED.into(),
    }));
    t.now += 30_001;
    t.poll();
    t.finish_disconnects();
    let d = t.device(SAVED);
    assert!(
        !d.paused,
        "a timeout must not pause trusted automatic reconnection"
    );
    assert_eq!(d.error, Some(p::ErrorCode::Timeout as i32));
}

#[test]
fn blocking_keeps_an_earlier_reconnect_pause() {
    let mut t = Test::new(true);
    t.ok(Command::ConnectDevice(p::ConnectDevice {
        device: SAVED.into(),
    }));
    assert!(!t.app.manager.devices[0].as_ref().unwrap().paused);
    t.ok(Command::SetDevice(p::SetDevice {
        device: SAVED.into(),
        blocked: Some(true),
        ..Default::default()
    }));
    assert!(!t.app.manager.devices[0].as_ref().unwrap().paused);
    assert_eq!(t.radio.closes.len(), 1, "blocking closes the live link");
}

#[test]
fn unpair_finishes_once_the_link_is_gone() {
    let mut t = Test::new(true);
    t.poll();
    let link = t.radio.connects[0].0;
    assert_eq!(
        t.ok(Command::UnpairDevice(p::UnpairDevice {
            device: SAVED.into()
        })),
        None
    );
    assert!(t.app.manager.devices[0].is_some());
    t.event(Event::Disconnected { link, error: None });
    let events = t.events();
    assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED.into() })));
    assert!(t.app.manager.devices.iter().all(Option::is_none));
    assert!(
        !t.store
            .records
            .contains_key(&cordial_core::storage::record_key(2, 77))
    );
    assert_eq!(
        t.code(Command::UnpairDevice(p::UnpairDevice {
            device: SAVED.into()
        })),
        p::ErrorCode::NotFound
    );
}

#[test]
fn lost_records_are_deleted_at_startup() {
    let (_, mut store, radio) = setup();
    // A record that does not decode, and one whose bond belongs to another device.
    store
        .records
        .insert(cordial_core::storage::record_key(2, 90), b"{".to_vec());
    let mut other = Policy::paired(91, peer(5), b"Other");
    other.bond = 91;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &other,
        &bond(91, peer(5)),
    ))
    .unwrap();
    let key = cordial_core::storage::record_key(2, 91);
    let mut record: serde_json::Value = serde_json::from_slice(&store.records[&key]).unwrap();
    record["bond"]["owner"] = 5.into();
    store
        .records
        .insert(key, serde_json::to_vec(&record).unwrap());
    let mut t = Test::with(store, radio, build(false, None));
    assert_eq!(t.app.manager.devices.iter().flatten().count(), 1);
    assert!(
        !t.store
            .records
            .contains_key(&cordial_core::storage::record_key(2, 90))
    );
    assert!(!t.store.records.contains_key(&key));
    assert!(t.status().ready);
}

#[test]
fn a_read_error_at_startup_deletes_nothing() {
    let (_, mut store, radio) = setup();
    store.fail = true;
    let saved = store.records.clone();
    let mut t = Test::with(store, radio, build(false, None));
    t.store.fail = false;
    assert!(!t.status().ready);
    assert_eq!(t.store.records, saved);
    assert_eq!(
        t.code(Command::ListSettings(p::ListSettings {
            device: SAVED.into()
        })),
        p::ErrorCode::NotFound
    );
}

#[test]
fn a_record_lost_while_running_removes_the_device() {
    let mut t = Test::new(true);
    t.store
        .records
        .insert(cordial_core::storage::record_key(2, 77), b"{".to_vec());
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Disconnected { link, error: None });
    let events = t.events();
    assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED.into() })));
    assert!(t.app.manager.devices.iter().all(Option::is_none));
}

#[test]
fn settings_are_saved_and_forgotten_while_disconnected() {
    let mut t = Test::new(true);
    let p0 = preference(SettingKey::BacklightEnabled, FeatureId::BACKLIGHT, 1);
    block_on(
        Preferences {
            store: &mut t.store,
            device: 77,
        }
        .save(&p0),
    )
    .unwrap();
    t.app.manager.devices[0]
        .as_mut()
        .unwrap()
        .catalog
        .restore_preferences(vec![p0])
        .unwrap();
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    let settings = t.settings(SAVED);
    assert_eq!(settings.len(), 1);
    assert_eq!(settings[0].key, p::keys::BACKLIGHT_ENABLED);
    assert_eq!(
        settings[0].status,
        Some(p::setting::Status::State(p::SettingState::Pending as i32))
    );
    assert_eq!(
        settings[0].r#type,
        Some(p::setting::Type::Bool(p::BoolSetting {
            value: None,
            saved: Some(true)
        }))
    );
    let change = |key: &str, value: p::value::Value| p::SettingChange {
        integration: p::IntegrationKind::Hidpp as i32,
        key: key.into(),
        value: Some(p::Value { value: Some(value) }),
    };
    // A valid value saves while the device is away; it waits as pending.
    match t.ok(Command::SetSettings(p::SetSettings {
        device: SAVED.into(),
        changes: vec![change(
            p::keys::BACKLIGHT_ENABLED,
            p::value::Value::Bool(false),
        )],
    })) {
        Some(R::Settings(s)) => assert_eq!(
            s.settings[0].r#type,
            Some(p::setting::Type::Bool(p::BoolSetting {
                value: None,
                saved: Some(false)
            }))
        ),
        other => panic!("{other:?}"),
    }
    let load = |t: &mut Test| {
        block_on(
            Preferences {
                store: &mut t.store,
                device: 77,
            }
            .load_all(),
        )
        .unwrap()
    };
    assert_eq!(load(&mut t)[0].value, 0);
    for (changes, code) in [
        (vec![], p::ErrorCode::BadArgs),
        (
            vec![change(
                p::keys::BACKLIGHT_ENABLED,
                p::value::Value::Integer(3),
            )],
            p::ErrorCode::BadArgs,
        ),
        (
            vec![change("no.such_key", p::value::Value::Bool(true))],
            p::ErrorCode::NotFound,
        ),
        (
            vec![
                change(p::keys::BACKLIGHT_ENABLED, p::value::Value::Bool(true)),
                change(p::keys::BACKLIGHT_ENABLED, p::value::Value::Bool(true)),
            ],
            p::ErrorCode::BadArgs,
        ),
    ] {
        assert_eq!(
            t.code(Command::SetSettings(p::SetSettings {
                device: SAVED.into(),
                changes
            })),
            code
        );
    }
    assert_eq!(load(&mut t)[0].value, 0, "a refused change saves nothing");
    match t.ok(Command::ForgetSettings(p::ForgetSettings {
        device: SAVED.into(),
        settings: vec![p::SettingRef {
            integration: p::IntegrationKind::Hidpp as i32,
            key: p::keys::BACKLIGHT_ENABLED.into(),
        }],
    })) {
        Some(R::Settings(s)) => assert_eq!(s.settings[0].status, None),
        other => panic!("{other:?}"),
    }
    assert!(t.radio.writes.is_empty());
    assert!(load(&mut t).is_empty());
}

#[test]
fn read_only_values_are_information_and_wheel_capabilities_are_decoded() {
    use cordial_core::model::settings::*;
    let mut t = Test::new(true);
    let record = |key, kind, observed| {
        cordial_core::compact::Record::from_wire(&Setting {
            key,
            kind,
            writable: false,
            feature: FeatureId::HIRES_WHEEL,
            feature_version: FeatureRevision(1),
            scope: SettingScope::Device,
            choices: vec![],
            min: None,
            max: None,
            step: None,
            managed: false,
            desired: SettingValue::Null,
            observed,
            fresh: true,
            observed_at_ms: Some(1),
            observation_source: Some(ObservationSource::Read),
            state: SettingState::Unmanaged,
            error: None,
        })
        .unwrap()
    };
    t.app.manager.devices[0]
        .as_mut()
        .unwrap()
        .catalog
        .replace_discovery(
            vec![
                record(
                    SettingKey::WheelInfo,
                    SettingType::Text,
                    SettingValue::Text("080c1832".into()),
                ),
                record(
                    SettingKey::BacklightCurrentLevel,
                    SettingType::Integer,
                    SettingValue::Integer(3),
                ),
            ],
            vec![],
        )
        .unwrap();
    assert!(t.settings(SAVED).is_empty());
    let d = t.device(SAVED);
    use p::value::Value as V;
    assert_eq!(
        info(&d, p::keys::WHEEL_RESOLUTION_MULTIPLIER),
        Some(V::Integer(8))
    );
    assert_eq!(
        info(&d, p::keys::WHEEL_RATCHETS_PER_ROTATION),
        Some(V::Integer(0x18))
    );
    assert_eq!(info(&d, p::keys::WHEEL_DIAMETER), Some(V::Integer(0x32)));
    assert_eq!(
        info(&d, p::keys::BACKLIGHT_CURRENT_LEVEL),
        Some(V::Integer(3))
    );
    assert!(d.info.iter().all(|i| !i.key.contains("info")));
}

#[test]
fn adapter_name_and_platform_are_partial_updates() {
    let mut t = Test::new(false);
    let status = |r: Option<R>| match r {
        Some(R::Status(s)) => s,
        other => panic!("{other:?}"),
    };
    let s = status(t.ok(Command::SetAdapter(p::SetAdapter {
        name: Some("Desk".into()),
        platform: None,
    })));
    assert_eq!(
        (s.name.as_str(), s.platform),
        ("Desk", p::Platform::Linux as i32)
    );
    assert!(
        t.events()
            .iter()
            .any(|e| matches!(e, Ev::Adapter(s) if s.name == "Desk"))
    );
    let s = status(t.ok(Command::SetAdapter(p::SetAdapter {
        name: None,
        platform: Some(p::Platform::Mac as i32),
    })));
    assert_eq!(
        (s.name.as_str(), s.platform),
        ("Desk", p::Platform::Mac as i32)
    );
    let s = status(t.ok(Command::SetAdapter(p::SetAdapter {
        name: Some(String::new()),
        platform: None,
    })));
    assert_eq!(s.name, "Test adapter", "an empty name restores the default");
    assert_eq!(s.platform, p::Platform::Mac as i32);
    for name in ["\u{7}bell", &"x".repeat(65)] {
        assert_eq!(
            t.code(Command::SetAdapter(p::SetAdapter {
                name: Some(name.into()),
                platform: None,
            })),
            p::ErrorCode::BadArgs
        );
    }
    t.events();
    // An unchanged value writes nothing and reports nothing.
    let saved = t.store.records.clone();
    t.ok(Command::SetAdapter(p::SetAdapter {
        name: None,
        platform: Some(p::Platform::Mac as i32),
    }));
    assert_eq!(t.store.records, saved);
    assert!(t.events().is_empty());
}

#[test]
fn development_commands_exist_only_in_development_firmware() {
    let mut t = Test::new(false);
    for command in [
        Command::ListFiles(p::ListFiles { path: "/".into() }),
        Command::ReadFile(p::ReadFile { path: "/a".into() }),
        Command::ListFeatures(p::ListFeatures {
            device: SAVED.into(),
        }),
        Command::EnterBootloader(p::EnterBootloader {}),
    ] {
        assert_eq!(t.code(command), p::ErrorCode::UnknownCommand);
    }
}

#[cfg(feature = "development")]
#[test]
fn development_files_are_listed_and_read_whole() {
    let (_, store, radio) = setup();
    let mut t = Test::with(store, radio, build(true, None));
    let bytes: Vec<u8> = (0..1500).map(|n| (n % 251) as u8).collect();
    t.store.files.insert("/device.json".into(), bytes.clone());
    match t.ok(Command::ListFiles(p::ListFiles { path: "/".into() })) {
        Some(R::Files(f)) => assert_eq!(
            f.entries,
            vec![p::FileEntry {
                name: "device.json".into(),
                directory: false,
                size: 1500
            }]
        ),
        other => panic!("{other:?}"),
    }
    match t.ok(Command::ReadFile(p::ReadFile {
        path: "/device.json".into(),
    })) {
        Some(R::File(f)) => assert_eq!(f.data, bytes),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        t.code(Command::ReadFile(p::ReadFile {
            path: "/../x".into()
        })),
        p::ErrorCode::BadArgs
    );
    assert!(
        t.status()
            .info
            .iter()
            .any(|i| i.key == p::keys::BUILD_DEVELOPMENT)
    );
}

#[cfg(feature = "development")]
#[test]
fn bootloader_releases_input_before_reboot() {
    let (manager, store, radio) = setup();
    let mut t = Test::with(
        store,
        radio,
        build(true, Some(|| panic!("unexpected early reboot"))),
    );
    t.app.manager = manager;
    let link = t
        .app
        .manager
        .connect(0, true, 30_000, &mut t.radio)
        .unwrap()
        .unwrap();
    t.app.manager.connected(link, descriptor(), 255, 0).unwrap();
    while t.app.manager.forward.packet().is_some() {
        t.app.manager.forward.complete();
    }
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes()[0], 16);
    t.app.manager.forward.complete();
    assert_eq!(t.ok(Command::EnterBootloader(p::EnterBootloader {})), None);
    t.poll();
    assert!(
        t.app.manager.forward.packet().is_some(),
        "accepted reboot first releases the host's held key"
    );
}

#[test]
fn ble_accept_list_waits_without_reserving_a_slot_or_a_session() {
    let mut t = Test::new(true);
    let first = saved_ble(&mut t);
    let mut other = t.app.manager.devices[0].as_ref().unwrap().policy.clone();
    other.id = 78;
    other.bond = 78;
    other.peer.address[0] ^= 1;
    let second = other.peer;
    block_on(cordial_core::bonds::commit(
        &mut t.store,
        &other,
        &bond(78, second),
    ))
    .unwrap();
    t.app.manager.devices.push(Some(SavedDevice::new(other)));
    t.radio.bonds.push(second);
    t.app.session(false, &mut t.radio);
    let saved = t.store.records.clone();
    for _ in 0..10 {
        t.now += 400_000;
        t.poll();
        assert_eq!(t.radio.reconnect, [first, second]);
        assert!(t.radio.connects.is_empty());
        assert!(t.app.manager.connections.iter().all(Option::is_none));
        assert!(t.radio.scans.is_empty());
    }
    t.event(Event::Incoming {
        attempt: 1,
        peer: second,
    });
    assert!(t.radio.incoming.last().unwrap().is_some());
    assert_eq!(
        t.app.manager.devices[1].as_ref().unwrap().state,
        ConnectionState::Connecting
    );
    t.poll();
    assert!(t.radio.reconnect.is_empty(), "pause during HID setup");
    assert_eq!(t.store.records, saved);
    assert_eq!(t.app.serial.queued(), 0);
}

/// Moves the saved device to `peer`, in memory and in its saved record.
fn move_saved(t: &mut Test, peer: Peer) {
    let d = t.app.manager.devices[0].as_mut().unwrap();
    d.policy.peer = peer;
    block_on(cordial_core::bonds::commit(
        &mut t.store,
        &d.policy,
        &bond(d.policy.id, peer),
    ))
    .unwrap();
    t.radio.bonds = vec![peer];
}

fn saved_ble(t: &mut Test) -> Peer {
    let peer = Peer {
        transport: Transport::Ble,
        ..t.app.manager.devices[0].as_ref().unwrap().policy.peer
    };
    move_saved(t, peer);
    peer
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
        let mut t = Test::new(true);
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
                ConnectionState::Disconnected,
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
fn foreground_discovery_and_session_exit_preserve_the_accept_list() {
    let mut t = Test::new(true);
    let peer = saved_ble(&mut t);
    t.poll();
    t.scan(&[p::Transport::Classic]);
    assert!(matches!(t.radio.scans.last(), Some((_, true, false))));
    assert_eq!(t.radio.reconnect, [peer]);
    t.app.session(false, &mut t.radio);
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
        let mut t = Test::new(true);
        t.radio.transports = Some(cordial_core::bluetooth::Capabilities {
            classic: true,
            ble: true,
            ble_scan_and_connect: concurrent,
        });
        let peer = saved_ble(&mut t);
        t.poll();
        t.scan(&[p::Transport::Ble]);
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
        // Stopping during an exclusive reconnect window never resurrects the scan.
        t.now = 5000;
        t.poll();
        t.ok(Command::StopScan(p::StopScan {}));
        t.now = 6000;
        t.poll();
        assert_eq!(t.radio.scans.last(), Some(&(token, false, false)));
        assert_eq!(t.radio.reconnect, [peer]);
    }
}

#[test]
fn background_reconnect_leaves_room_for_a_pairing() {
    let mut t = Test::new(true);
    for n in 2..=5 {
        t.add_saved(n);
        t.app.manager.devices[usize::from(n - 1)]
            .as_mut()
            .unwrap()
            .paused = false;
    }
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    t.event(Event::Incoming {
        attempt: 1,
        peer: peer(4),
    });
    assert_eq!(t.radio.connects.len(), 2);
    t.poll();
    assert_eq!(t.radio.connects.len(), 2, "one connection stays free");
}

#[test]
fn radio_failure_finishes_the_scan_and_reports_readiness() {
    let mut t = Test::new(false);
    t.scan(&[p::Transport::Classic]);
    t.event(Event::Failed(ErrorCode::RadioUnavailable));
    let events = t.events();
    assert!(events.iter().any(|e| matches!(e, Ev::ScanDone(_))));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ev::Adapter(s) if !s.ready))
    );
    assert_eq!(
        t.code(Command::StartScan(p::StartScan {
            transports: vec![p::Transport::Classic as i32],
            seconds: 0
        })),
        p::ErrorCode::NotReady
    );
}

#[test]
fn radio_restart_releases_keys_then_reconnects_saved_devices_after_ready() {
    let mut t = Test::new(true);
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
    assert!(!t.status().ready);
    t.now += 5000;
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    assert!(t.app.manager.forward.packet().is_none());
    t.event(Event::Ready);
    assert!(
        t.events()
            .iter()
            .any(|e| matches!(e, Ev::Adapter(s) if s.ready))
    );
    t.poll();
    assert_eq!(t.radio.connects.len(), 2);
    assert_ne!(t.radio.connects[1].0, link);
    assert_eq!(t.store.records, saved);
}

#[test]
fn warnings_are_listed_and_reported_when_they_change() {
    use cordial_core::model::errors::{DeviceWarning, HidReportType, WarningCode};
    let mut t = Test::new(true);
    let warnings: Vec<_> = (0..200)
        .map(|bit| DeviceWarning {
            code: WarningCode::IndicatorStateUnknown,
            service: 1,
            report_type: Some(HidReportType::Output),
            report_id: Some(3),
            bit_offset: Some(bit),
            usage_page: Some(8),
            usage: None,
        })
        .collect();
    t.app.manager.devices[0]
        .as_mut()
        .unwrap()
        .update_warnings(&warnings)
        .unwrap();
    let events = t.events();
    let reported = events
        .iter()
        .find_map(|e| match e {
            Ev::Warnings(w) => Some(w.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(reported.device, SAVED);
    assert_eq!(reported.warnings.len(), 200);
    assert_eq!(
        reported.warnings[5],
        p::DeviceWarning {
            code: p::WarningCode::IndicatorStateUnknown as i32,
            service: 1,
            report_type: p::ReportType::Output as i32,
            report_id: Some(3),
            bit_offset: Some(5),
            usage_page: Some(8),
            usage: None,
        }
    );
    match t.ok(Command::ListWarnings(p::ListWarnings {
        device: SAVED.into(),
    })) {
        Some(R::Warnings(w)) => assert_eq!(w, reported),
        other => panic!("{other:?}"),
    }
}

#[test]
fn repeated_changes_are_reported_once_with_the_latest_state() {
    let mut t = Test::new(true);
    for paused in [true, false, true] {
        t.app.manager.devices[0].as_mut().unwrap().paused = paused;
        t.app.manager.devices[0].as_mut().unwrap().policy.trusted = !paused;
        // Each change marks the device; only the state when output frees is written.
        t.ok(Command::SetDevice(p::SetDevice {
            device: SAVED.into(),
            trusted: Some(!paused),
            ..Default::default()
        }));
    }
    let devices: Vec<_> = t
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Ev::Device(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(devices.len(), 1);
    assert!(devices[0].paused && !devices[0].trusted);
}

/// A keyboard descriptor with HID++ short and long reports.
fn hidpp_descriptor() -> Vec<cordial_core::bluetooth::Descriptor> {
    vec![
        cordial_core::bluetooth::Descriptor::from_slice(
            ServiceId(7),
            &[
                5, 1, 9, 6, 0xa1, 1, 0x85, 1, 5, 7, 0x19, 4, 0x29, 11, 0x15, 0, 0x25, 1, 0x75, 1,
                0x95, 8, 0x81, 2, 0xc0, 0x06, 0x00, 0xff, 0x09, 1, 0xa1, 1, 0x85, 0x10, 0x75, 8,
                0x95, 6, 0x15, 0, 0x26, 0xff, 0, 0x09, 1, 0x81, 0, 0x09, 1, 0x91, 0, 0x85, 0x11,
                0x95, 19, 0x09, 2, 0x81, 0, 0x09, 2, 0x91, 0, 0xc0,
            ],
        )
        .unwrap(),
    ]
}
fn pending_setup(t: &mut Test) {
    let policy = &mut t.app.manager.devices[0].as_mut().unwrap().policy;
    policy.hidpp_enabled = false;
    policy.setup_pending = true;
}
/// Connects the saved keyboard through automatic reconnection.
fn connect_saved(
    t: &mut Test,
    descriptors: Vec<cordial_core::bluetooth::Descriptor>,
) -> cordial_core::link::LinkId {
    t.poll();
    let link = t.radio.connects.last().unwrap().0;
    t.event(Event::Connected {
        link,
        descriptors,
        max_output: 255,
    });
    link
}
/// Completes the HID++ protocol request the adapter sent last and answers it with `major`, or
/// the HID++ 1.0 unknown-request error when `major` is 1.
fn answer_protocol(t: &mut Test, link: cordial_core::link::LinkId, major: u8) {
    let (id, request) = t.radio.writes.last().cloned().unwrap();
    assert_eq!(
        request[5], 0xa5,
        "expected the protocol request, got {request:?}"
    );
    t.event(Event::Written { id, result: Ok(()) });
    let mut reply = request.clone();
    if major == 1 {
        reply[1..5].copy_from_slice(&[0x8f, request[1], request[2], 1]);
    } else {
        (reply[3], reply[4]) = (major, 0);
    }
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0x11, &reply).unwrap(),
    ));
    t.poll();
}
fn saved_policy(t: &Test) -> &Policy {
    &t.app.manager.devices[0].as_ref().unwrap().policy
}

#[test]
fn first_connection_setup_turns_hidpp_on_for_a_hidpp_2_device() {
    let mut t = Test::new(true);
    pending_setup(&mut t);
    let link = connect_saved(&mut t, hidpp_descriptor());
    t.poll();
    answer_protocol(&mut t, link, 4);
    let events = t.events();
    assert!(saved_policy(&t).hidpp_enabled && !saved_policy(&t).setup_pending);
    let record: serde_json::Value =
        serde_json::from_slice(&t.store.records[&cordial_core::storage::record_key(2, 77)])
            .unwrap();
    assert_eq!(record["policy"]["hidpp_enabled"], true);
    assert!(record["policy"].get("setup_pending").is_none());
    let last = events
        .iter()
        .filter_map(|e| match e {
            Ev::Device(d) => Some(d),
            _ => None,
        })
        .next_back()
        .unwrap();
    let integration = hidpp(last);
    assert!(integration.enabled);
    assert_eq!(
        integration.detected,
        Some(p::IntegrationDetection {
            version: Some(p::Version { major: 4, minor: 0 })
        })
    );
    assert_eq!(
        hidpp_state(last),
        p::integration::Status::State(p::IntegrationState::Starting as i32)
    );
}

#[test]
fn first_connection_setup_leaves_hidpp_off_without_hidpp_2() {
    for hidpp in [false, true] {
        let mut t = Test::new(true);
        pending_setup(&mut t);
        let link = connect_saved(
            &mut t,
            if hidpp {
                hidpp_descriptor()
            } else {
                descriptor()
            },
        );
        t.poll();
        if hidpp {
            answer_protocol(&mut t, link, 1);
        }
        assert!(!saved_policy(&t).hidpp_enabled && !saved_policy(&t).setup_pending);
        if !hidpp {
            assert!(t.radio.writes.is_empty());
        }
    }
}

#[test]
fn integration_state_follows_the_connection() {
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, hidpp_descriptor());
    t.poll();
    assert_eq!(
        hidpp_state(&t.device(SAVED)),
        p::integration::Status::State(p::IntegrationState::Starting as i32)
    );
    t.event(Event::Disconnected { link, error: None });
    t.poll();
    assert_eq!(
        hidpp_state(&t.device(SAVED)),
        p::integration::Status::State(p::IntegrationState::Disconnected as i32)
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
    let mut t = Test::new(true);
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
    t.app.session(false, &mut t.radio);
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

#[test]
fn connecting_a_blocked_device_is_refused_as_blocked() {
    let mut t = Test::new(true);
    t.ok(Command::SetDevice(p::SetDevice {
        device: SAVED.into(),
        blocked: Some(true),
        ..Default::default()
    }));
    assert_eq!(
        t.code(Command::ConnectDevice(p::ConnectDevice {
            device: SAVED.into(),
        })),
        p::ErrorCode::Blocked
    );
}

#[test]
fn a_failed_free_space_read_keeps_storage_ready() {
    let mut t = Test::new(true);
    t.store.fail = true;
    assert!(t.status().ready);
    t.store.fail = false;
    assert!(t.app.manager.storage_ready);
}

#[test]
fn a_record_found_lost_while_saving_removes_the_device() {
    let mut t = Test::new(true);
    t.events();
    t.store
        .records
        .insert(cordial_core::storage::record_key(2, 77), b"{".to_vec());
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED.into(),
            trusted: Some(false),
            ..Default::default()
        })),
        p::ErrorCode::NotFound
    );
    assert!(
        t.events()
            .contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED.into() }))
    );
    assert!(t.app.manager.devices.iter().all(Option::is_none));
}

#[test]
fn a_pairing_cancelled_by_the_session_ending_reports_nothing_to_the_next_one() {
    let mut t = Test::new(false);
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(&candidate);
    t.poll();
    let link = t.radio.connects[0].0;
    t.app.session(false, &mut t.radio);
    t.app.session(true, &mut t.radio);
    t.event(Event::Disconnected { link, error: None });
    t.poll();
    assert!(pairing_steps(&t.events()).is_empty());
    assert!(t.app.manager.devices.iter().all(Option::is_none));
}
