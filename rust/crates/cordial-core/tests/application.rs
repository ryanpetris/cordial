#[path = "application/gamepad_setup.rs"]
mod gamepad_setup;
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
    bluetooth::{Event, InputReport, Layout, LayoutReport, ReportMap, ReportType},
    compact::{Metadata, Preference},
    devices::{Device as SavedDevice, Live, Peer, Policies, Policy},
    interfaces::Interface,
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
    storage,
};
use embassy_futures::block_on;
use prost::Message;
use support::*;

const SAVED: u32 = 77;
/// The first delay after a failure, which doubles per further failure.
/// The profile memory budget of the test board.
const PROFILE_BUDGET: u32 = 4096;
const FIRST_RETRY: u64 = cordial_core::devices::RETRY_DELAY_MS as u64;

thread_local! {
    /// How many of the next allocations on this thread fail.
    static FAILS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FAIL_LAYOUT: std::cell::Cell<Option<std::alloc::Layout>> = const { std::cell::Cell::new(None) };
}
/// Whether this allocation is one asked to fail.
fn fails(layout: std::alloc::Layout) -> bool {
    if FAIL_LAYOUT
        .try_with(|f| f.get() == Some(layout))
        .unwrap_or(false)
    {
        FAIL_LAYOUT.with(|f| f.set(None));
        return true;
    }
    FAILS
        .try_with(|f| {
            let fails = f.get();
            f.set(fails.saturating_sub(1));
            fails > 0
        })
        .unwrap_or(false)
}
/// Makes the next `count` allocations fail. They must be ones the code under test handles.
fn fail_allocations(count: usize) {
    FAILS.with(|f| f.set(count));
}
/// Makes the next allocation fail. It must be one the code under test handles.
fn fail_next_allocation() {
    fail_allocations(1);
}
/// Whether every allocation asked to fail has been made.
fn allocation_failed() -> bool {
    FAILS.with(|f| f.get()) == 0
}

fn build(development: bool, bootloader: Option<fn() -> !>) -> Build {
    Build {
        development,
        version: "test",
        board: "test_board",
        default_adapter_name: "Test adapter",
        adapter_id: "adapter".into(),
        profile_memory_budget: Some(PROFILE_BUDGET),
        bootloader: bootloader.map(|enter| Bootloader { enter }),
    }
}

struct Test {
    app: Application,
    store: Store,
    radio: Radio,
    now: u64,
    decoder: Decoder,
    /// The keys the adapter's USB keyboard report holds, as last sent.
    keys: Vec<u16>,
}
impl Test {
    fn new(paired: bool) -> Self {
        Self::with_budget(paired, Some(PROFILE_BUDGET))
    }
    /// A test board with `budget` bytes for loaded profiles, or without profile support.
    fn with_budget(paired: bool, budget: Option<u32>) -> Self {
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
            all_transports(&mut store);
        }
        let mut build = build(false, None);
        build.profile_memory_budget = budget;
        Self::with(store, radio, build)
    }
    fn with(store: Store, radio: Radio, build: Build) -> Self {
        let mut t = Self {
            app: Application::new(build),
            store,
            radio,
            now: 0,
            decoder: Decoder::new(None),
            keys: Vec::new(),
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
    /// Lets input stay quiet for as long as deferred saves wait for, then polls.
    fn quiet(&mut self) {
        self.now += cordial_core::deferred::QUIET_MS;
        self.poll();
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
        for _ in 0..100_000 {
            for m in self.output() {
                if let M::Response(r) = m {
                    return r;
                }
            }
            assert!(self.app.command_pending(), "no response");
            block_on(self.app.proceed(&mut self.store, &mut self.radio, self.now));
        }
        panic!("response did not finish")
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
    fn device(&mut self, id: u32) -> p::Device {
        match self.ok(Command::GetDevice(p::GetDevice { device: id })) {
            Some(R::Device(d)) => d,
            other => panic!("{other:?}"),
        }
    }
    fn settings(&mut self, id: u32) -> Vec<p::SettingsObject> {
        match self.ok(Command::ListSettings(p::ListSettings { device: id })) {
            Some(R::Settings(list)) => {
                assert_eq!(list.device, id);
                list.objects
            }
            other => panic!("{other:?}"),
        }
    }
    fn device_entries(&mut self) -> Vec<p::DeviceListEntry> {
        match self.ok(Command::ListDevices(p::ListDevices {})) {
            Some(R::Devices(list)) => list.entries,
            other => panic!("{other:?}"),
        }
    }
    /// Applies `args`, which responds with no result, and reads the device back.
    fn set_device(&mut self, args: p::SetDevice) -> p::Device {
        let id = args.device;
        assert_eq!(self.ok(Command::SetDevice(args)), None);
        self.device(id)
    }
    /// Applies `args`, which responds with no result, and reads the status back.
    fn set_adapter(&mut self, args: p::SetAdapter) -> p::Status {
        assert_eq!(self.ok(Command::SetAdapter(args)), None);
        self.status()
    }
    /// Applies `args`, which responds with no result, and reads the settings back.
    fn set_settings(&mut self, args: p::SetSettings) -> Vec<p::SettingsObject> {
        let id = args.device;
        assert_eq!(self.ok(Command::SetSettings(args)), None);
        self.settings(id)
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
    fn candidate(&mut self, identity: Peer, address: Peer) -> u32 {
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
    fn pair(&mut self, candidate: u32) {
        self.ok(Command::StartPairing(p::StartPairing { candidate }));
    }
    /// Saves device `76 + n` with identity `peer(n)`, resident and paused when the stack has room
    /// for it.
    fn add_saved(&mut self, n: u8) -> u32 {
        let id = 76 + u32::from(n);
        let mut policy = Policy::paired(id.into(), peer(n), b"Keyboard");
        policy.setup_pending = false;
        block_on(cordial_core::bonds::commit(
            &mut self.store,
            &policy,
            &bond(policy.id, policy.peer),
        ))
        .unwrap();
        block_on(self.app.manager.fill(&mut self.store, &mut self.radio)).unwrap();
        if let Some(slot) = self.app.manager.find(id.into()) {
            self.app.manager.devices[slot].as_mut().unwrap().paused = true;
        }
        id
    }
    /// Device `id`'s saved policy.
    fn saved(&mut self, id: u32) -> Policy {
        block_on(
            Policies {
                store: &mut self.store,
            }
            .load(id.into()),
        )
        .unwrap()
    }
    /// Saves a changed copy of device `id`'s policy behind the application's back, as another
    /// session's earlier write would have.
    fn write_policy(&mut self, id: u32, f: impl FnOnce(&mut Policy)) {
        let mut policy = self.saved(id);
        f(&mut policy);
        block_on(
            Policies {
                store: &mut self.store,
            }
            .save(&policy),
        )
        .unwrap();
    }
    /// What resident device `id` keeps for its connection.
    fn live(&mut self, id: u32) -> &mut Live {
        let slot = self.app.manager.find(id.into()).unwrap();
        self.app.manager.devices[slot]
            .as_mut()
            .unwrap()
            .live
            .as_deref_mut()
            .unwrap()
    }
    /// Resident device `id`'s reconnection entry.
    fn entry(&mut self, id: u32) -> &mut SavedDevice {
        let slot = self.app.manager.find(id.into()).unwrap();
        self.app.manager.devices[slot].as_mut().unwrap()
    }
    /// Waits out a new connection's wait for its first input and polls, so background storage
    /// work goes ahead without input.
    fn settle(&mut self) {
        self.now += cordial_core::manager::FIRST_INPUT_WAIT_MS;
        self.poll();
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

fn backlight_update(change: Option<p::bool_setting_update::Change>) -> p::SettingsUpdate {
    p::SettingsUpdate {
        integration: p::IntegrationKind::Hidpp as i32,
        group: Some(p::settings_update::Group::Backlight(
            p::BacklightSettingsUpdate {
                enabled: Some(p::BoolSettingUpdate { change }),
                ..Default::default()
            },
        )),
    }
}
fn backlight_value(value: bool) -> p::SettingsUpdate {
    backlight_update(Some(p::bool_setting_update::Change::Value(value)))
}
fn backlight_forget() -> p::SettingsUpdate {
    backlight_update(Some(p::bool_setting_update::Change::Forget(
        p::SettingForget {},
    )))
}
fn backlight_enabled(object: &p::SettingsObject) -> &p::BoolSetting {
    let Some(p::settings_object::Group::Backlight(settings)) = &object.group else {
        panic!("{object:?}")
    };
    settings.enabled.as_ref().unwrap()
}

fn hidpp(device: &p::Device) -> &p::Integration {
    device
        .integrations
        .iter()
        .find(|i| i.kind == p::IntegrationKind::Hidpp as i32)
        .unwrap()
}

fn hidpp_state(device: &p::Device) -> p::integration::Status {
    hidpp(device).status.clone().unwrap()
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
                resources: vec![
                    cordial_core::wire::resource(
                        p::ResourceType::Devices,
                        Some(0),
                        Some(cordial_core::devices::CLASSIC_CONNECTIONS as u32)
                    ),
                    cordial_core::wire::resource(
                        p::ResourceType::Connections,
                        Some(0),
                        Some(cordial_core::devices::CLASSIC_CONNECTIONS as u32)
                    )
                ],
                enabled: Some(true),
            },
            p::TransportSupport {
                transport: p::Transport::Ble as i32,
                resources: vec![
                    cordial_core::wire::resource(
                        p::ResourceType::Devices,
                        Some(0),
                        Some(cordial_core::devices::BLE_CONNECTIONS as u32)
                    ),
                    cordial_core::wire::resource(
                        p::ResourceType::Connections,
                        Some(0),
                        Some(cordial_core::devices::BLE_CONNECTIONS as u32)
                    )
                ],
                enabled: Some(true),
            },
        ]
    );
    let information = status.information.unwrap();
    assert!(information.firmware_version.is_some());
    assert!(information.board_name.is_some());
    assert_eq!(information.development_build, Some(false));
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
    assert_eq!(
        t.device_entries(),
        vec![p::DeviceListEntry {
            entry: Some(p::device_list_entry::Entry::Device(d)),
        }]
    );
    assert_eq!(
        t.code(Command::GetDevice(p::GetDevice { device: 999 })),
        p::ErrorCode::NotFound
    );
}

#[test]
fn set_device_changes_only_the_fields_sent() {
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    let d = t.set_device(p::SetDevice {
        device: SAVED,
        trusted: Some(false),
        ..Default::default()
    });
    assert!(!d.trusted && d.enabled && !d.blocked && hidpp(&d).enabled);
    let d = t.set_device(p::SetDevice {
        device: SAVED,
        integrations: vec![p::IntegrationUpdate {
            kind: p::IntegrationKind::Hidpp as i32,
            enabled: Some(false),
        }],
        ..Default::default()
    });
    assert!(
        !d.trusted,
        "an earlier change survives a later partial update"
    );
    // HID++ stays listed while turned off, since the device has a saved preference for it.
    assert!(!hidpp(&d).enabled);
    assert_eq!(
        hidpp_state(&d),
        p::integration::Status::State(p::IntegrationState::Off as i32)
    );
    let policy = t.saved(SAVED);
    assert!(!policy.trusted && !policy.hidpp_enabled());
    assert!(!t.entry(SAVED).trusted && !t.entry(SAVED).hidpp_enabled);
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED,
            integrations: vec![p::IntegrationUpdate {
                kind: 42,
                enabled: Some(true),
            }],
            ..Default::default()
        })),
        p::ErrorCode::Unsupported
    );
    // Updates apply in order, so the last one for an integration wins.
    let hidpp_update = |enabled| p::IntegrationUpdate {
        kind: p::IntegrationKind::Hidpp as i32,
        enabled: Some(enabled),
    };
    for (updates, enabled) in [([false, true], true), ([true, false], false)] {
        {
            let d = t.set_device(p::SetDevice {
                device: SAVED,
                integrations: updates.map(hidpp_update).to_vec(),
                ..Default::default()
            });
            assert_eq!(hidpp(&d).enabled, enabled);
        }
        assert_eq!(t.saved(SAVED).hidpp_enabled(), enabled);
        assert_eq!(t.saved(SAVED).integrations.len(), 1);
    }
}

#[test]
fn enabling_past_the_stack_limit_is_refused() {
    let mut t = Test::new(true);
    let ids: Vec<u32> = (2..=8).map(|n| t.add_saved(n)).collect();
    let last = *ids.last().unwrap();
    // Saved devices beyond the normal allowance wait for a resident entry.
    assert_eq!(
        t.app.manager.devices.iter().flatten().count(),
        cordial_core::devices::CLASSIC_CONNECTIONS
    );
    assert!(t.app.manager.find(last.into()).is_none());
    assert_eq!(
        t.device(last).inactive,
        Some(p::InactiveReason::Capacity as i32)
    );
    t.ok(Command::SetDevice(p::SetDevice {
        device: last,
        enabled: Some(false),
        ..Default::default()
    }));
    let error = t.error(Command::SetDevice(p::SetDevice {
        device: last,
        enabled: Some(true),
        ..Default::default()
    }));
    assert_eq!(error.code, p::ErrorCode::NoCapacity as i32);
    assert_eq!(error.reason, p::ErrorReason::EnabledDevices as i32);
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
    // An unsupported or unknown transport beside a usable one is left out.
    t.ok(Command::StartScan(p::StartScan {
        transports: vec![p::Transport::Classic as i32, 7, p::Transport::Ble as i32],
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
        error: None
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
    t.pair(candidate);
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
    let device = t.app.manager.devices[0].as_ref().unwrap().id as u32;
    assert_eq!(
        pairing_steps(&events),
        [p::pairing::Step::Done(p::PairingDone { device })]
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
        layout: None,
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
        t.code(Command::StartPairing(p::StartPairing { candidate: 9 })),
        p::ErrorCode::NotFound
    );
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(candidate);
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
        Some(&p::pairing::Step::Error(p::Error {
            code: p::ErrorCode::Rejected as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }))
    );
    assert!(t.app.manager.devices.iter().all(Option::is_none));
}

#[test]
fn ending_the_session_stops_scanning_and_pairing() {
    let mut t = Test::new(false);
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(candidate);
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
    t.pair(candidate);
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Bonded { link, identity });
    let steps = pairing_steps(&t.events());
    assert_eq!(
        steps.last(),
        Some(&p::pairing::Step::Done(p::PairingDone { device: SAVED }))
    );
    assert_eq!(t.app.manager.devices.iter().flatten().count(), 1);
}

#[test]
fn failed_stranger_bond_cleanup_is_retried_before_the_next_pair() {
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.add_saved(2);
    let candidate = t.candidate(peer(1), peer(1));
    t.pair(candidate);
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
    // The attempt fails for its own reason; the stranger's bond is removed by a later sync.
    assert_eq!(
        pairing_steps(&t.events()).last(),
        Some(&p::pairing::Step::Error(p::Error {
            code: p::ErrorCode::AuthFailed as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }))
    );
    assert!(t.radio.bonds.contains(&stranger));
    t.radio.reject_forget = false;
    t.pair(candidate);
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
    assert_eq!(error.reason, p::ErrorReason::Storage as i32);
    assert!(t.radio.connects.is_empty());
    assert!(
        t.status()
            .errors
            .iter()
            .any(|e| e.code == p::ErrorCode::StorageFull as i32)
    );
}

#[test]
fn connect_and_disconnect_respond_with_the_device() {
    let mut t = Test::new(true);
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    // An explicit connect joins the automatic attempt already running.
    match t.ok(Command::ConnectDevice(p::ConnectDevice { device: SAVED })) {
        Some(R::Device(d)) => assert_eq!(d.state, p::DeviceState::Connecting as i32),
        other => panic!("{other:?}"),
    }
    assert_eq!(t.radio.connects.len(), 1);
    match t.ok(Command::DisconnectDevice(p::DisconnectDevice {
        device: SAVED,
    })) {
        Some(R::Device(d)) => assert!(d.paused),
        other => panic!("{other:?}"),
    }
    assert_eq!(t.radio.closes.len(), 1);
}

#[test]
fn a_connect_that_times_out_keeps_automatic_reconnection() {
    let mut t = Test::new(true);
    t.ok(Command::ConnectDevice(p::ConnectDevice { device: SAVED }));
    t.now += 30_001;
    t.poll();
    t.finish_disconnects();
    let d = t.device(SAVED);
    assert!(
        !d.paused,
        "a timeout must not pause trusted automatic reconnection"
    );
    assert_eq!(
        d.errors.first().map(|e| e.code),
        Some(p::ErrorCode::Timeout as i32)
    );
}

#[test]
fn blocking_keeps_an_earlier_reconnect_pause() {
    let mut t = Test::new(true);
    t.ok(Command::ConnectDevice(p::ConnectDevice { device: SAVED }));
    assert!(!t.app.manager.devices[0].as_ref().unwrap().paused);
    t.ok(Command::SetDevice(p::SetDevice {
        device: SAVED,
        blocked: Some(true),
        ..Default::default()
    }));
    assert!(!t.app.manager.devices[0].as_ref().unwrap().paused);
    assert_eq!(t.radio.closes.len(), 1, "blocking closes the live link");
}

#[test]
fn unpair_finishes_once_the_link_is_gone() {
    let mut t = Test::new(true);
    put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
    t.poll();
    let link = t.radio.connects[0].0;
    assert_eq!(
        t.ok(Command::UnpairDevice(p::UnpairDevice { device: SAVED })),
        None
    );
    assert!(t.app.manager.devices[0].is_some());
    t.event(Event::Disconnected { link, error: None });
    let events = t.events();
    assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED })));
    assert!(t.app.manager.devices.iter().all(Option::is_none));
    assert!(
        !t.store
            .records
            .contains_key(&cordial_core::storage::record_key(2, 77))
    );
    assert!(!t.store.records.contains_key(&layout_key()));
    assert_eq!(
        t.code(Command::UnpairDevice(p::UnpairDevice { device: SAVED })),
        p::ErrorCode::NotFound
    );
}

#[test]
fn lost_records_are_deleted_at_startup() {
    let (_, mut store, radio) = setup();
    // A record that does not decode, and one whose bond belongs to another device.
    store.records.insert(
        cordial_core::storage::record_key(2, 90),
        UNDECODABLE.to_vec(),
    );
    let mut other = Policy::paired(91, peer(5), b"Other");
    other.bond = 91;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &other,
        &bond(91, peer(5)),
    ))
    .unwrap();
    let key = cordial_core::storage::record_key(2, 91);
    let mut record = storage::Device::decode(store.records[&key].as_slice()).unwrap();
    record.bond.as_mut().unwrap().owner = 5;
    store.records.insert(key, record.encode_to_vec());
    // Layouts of the lost devices and of the saved one. A layout whose device record is gone
    // is the record store's to reclaim with the device's directory.
    let layout = cordial_core::layouts::encode(&classic_layout(KEYBOARD_MAP)).unwrap();
    for id in [77, 90, 91] {
        store
            .records
            .insert(cordial_core::storage::record_key(5, id), layout.clone());
    }
    let mut t = Test::with(store, radio, build(false, None));
    assert_eq!(t.app.manager.devices.iter().flatten().count(), 1);
    let layouts: Vec<_> = t
        .store
        .records
        .keys()
        .filter(|key| key[0] == 5)
        .copied()
        .collect();
    assert_eq!(layouts, [layout_key()]);
    assert!(
        !t.store
            .records
            .contains_key(&cordial_core::storage::record_key(2, 90))
    );
    assert!(!t.store.records.contains_key(&key));
    assert!(t.status().ready);
}

#[test]
fn a_lost_record_that_cannot_be_removed_at_startup_is_removed_later() {
    let (_, mut store, radio) = setup();
    let key = cordial_core::storage::record_key(2, 90);
    store.records.insert(key, UNDECODABLE.to_vec());
    store.fail_remove = Some(key);
    let mut t = Test::with(store, radio, build(false, None));
    // Startup leaves the lost record out and completes.
    assert!(t.status().ready);
    assert_eq!(t.app.manager.devices.iter().flatten().count(), 1);
    assert!(t.store.records.contains_key(&key));
    // The background cleanup fails, then waits for its backoff before trying again.
    t.poll();
    assert!(t.store.records.contains_key(&key));
    t.store.fail_remove = None;
    t.poll();
    assert!(t.store.records.contains_key(&key));
    t.now += u64::from(cordial_core::devices::RETRY_DELAY_MS);
    t.poll();
    assert!(!t.store.records.contains_key(&key));
    assert!(t.app.manager.lost_devices.is_empty());
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
        t.code(Command::ListSettings(p::ListSettings { device: SAVED })),
        p::ErrorCode::NotReady
    );
}

#[test]
fn a_record_lost_while_running_removes_the_device() {
    let mut t = Test::new(true);
    t.store.records.insert(
        cordial_core::storage::record_key(2, 77),
        UNDECODABLE.to_vec(),
    );
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Disconnected { link, error: None });
    let events = t.events();
    assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED })));
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
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    // A disconnected device's settings are its saved ones, read from flash.
    let settings = t.settings(SAVED);
    assert_eq!(settings.len(), 1);
    let enabled = backlight_enabled(&settings[0]);
    assert_eq!(
        enabled.status.as_ref().unwrap().result,
        Some(p::setting_status::Result::State(
            p::SettingState::Pending as i32
        ))
    );
    assert_eq!(enabled.value, None);
    assert_eq!(enabled.saved, Some(true));
    let settings = t.set_settings(p::SetSettings {
        device: SAVED,
        changes: vec![backlight_value(false)],
    });
    assert_eq!(backlight_enabled(&settings[0]).value, None);
    assert_eq!(backlight_enabled(&settings[0]).saved, Some(false));
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
            vec![p::SettingsUpdate {
                integration: 99,
                ..backlight_value(true)
            }],
            p::ErrorCode::NotFound,
        ),
        (vec![backlight_update(None)], p::ErrorCode::BadArgs),
        (
            vec![backlight_value(true), backlight_update(None)],
            p::ErrorCode::BadArgs,
        ),
        (
            vec![p::SettingsUpdate {
                integration: p::IntegrationKind::Hidpp as i32,
                group: None,
            }],
            p::ErrorCode::BadArgs,
        ),
    ] {
        assert_eq!(
            t.code(Command::SetSettings(p::SetSettings {
                device: SAVED,
                changes
            })),
            code
        );
    }
    assert_eq!(load(&mut t)[0].value, 0, "a refused change saves nothing");
    // Changes apply in order: a later change to the same setting replaces an earlier one.
    for (changes, saved) in [
        (vec![backlight_value(false), backlight_value(true)], Some(1)),
        (vec![backlight_forget(), backlight_value(false)], Some(0)),
        (vec![backlight_value(true), backlight_forget()], None),
    ] {
        t.ok(Command::SetSettings(p::SetSettings {
            device: SAVED,
            changes,
        }));
        assert_eq!(load(&mut t).first().map(|p| p.value), saved);
    }
    // Forgetting removes the saved value; forgetting it again is accepted.
    for _ in 0..2 {
        {
            let settings = t.set_settings(p::SetSettings {
                device: SAVED,
                changes: vec![backlight_forget()],
            });
            assert!(settings.is_empty());
        }
        assert!(load(&mut t).is_empty());
    }
    assert!(t.radio.writes.is_empty());
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
    connect_saved(&mut t, descriptor());
    t.poll();
    t.live(SAVED)
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
    let info = d.information.unwrap();
    let wheel = info.wheel.unwrap();
    assert_eq!(wheel.resolution_multiplier, Some(8));
    assert_eq!(wheel.ratchets_per_rotation, Some(0x18));
    assert_eq!(wheel.diameter, Some(0x32));
    assert_eq!(info.backlight.unwrap().current_brightness, Some(3));
}

#[test]
fn adapter_name_and_platform_are_partial_updates() {
    let mut t = Test::new(false);
    let s = t.set_adapter(p::SetAdapter {
        name: Some("Desk".into()),
        platform: None,
        ..Default::default()
    });
    assert_eq!(
        (s.name.as_str(), s.platform),
        ("Desk", p::Platform::Linux as i32)
    );
    assert!(
        t.events()
            .iter()
            .any(|e| matches!(e, Ev::Adapter(s) if s.name == "Desk"))
    );
    let s = t.set_adapter(p::SetAdapter {
        name: None,
        platform: Some(p::Platform::Mac as i32),
        ..Default::default()
    });
    assert_eq!(
        (s.name.as_str(), s.platform),
        ("Desk", p::Platform::Mac as i32)
    );
    let s = t.set_adapter(p::SetAdapter {
        name: Some(String::new()),
        platform: None,
        ..Default::default()
    });
    assert_eq!(s.name, "Test adapter", "an empty name restores the default");
    assert_eq!(s.platform, p::Platform::Mac as i32);
    for name in ["\u{7}bell", &"x".repeat(65)] {
        assert_eq!(
            t.code(Command::SetAdapter(p::SetAdapter {
                name: Some(name.into()),
                platform: None,
                ..Default::default()
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
        ..Default::default()
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
        Command::ListFeatures(p::ListFeatures { device: SAVED }),
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
    // More files than one page holds.
    for n in (0..20).rev() {
        t.store.files.insert(format!("/f{n:02}"), vec![n]);
    }
    let Some(R::Files(list)) = t.ok(Command::ListFiles(p::ListFiles { path: "/".into() })) else {
        panic!("missing files")
    };
    let entries = list.entries;
    assert_eq!(entries.len(), 21);
    assert!(entries.windows(2).all(|w| w[0].name < w[1].name));
    assert_eq!(
        entries[0],
        p::FileEntry {
            name: "device.json".into(),
            directory: false,
            size: 1500
        }
    );
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
    assert_eq!(
        t.status().information.unwrap().development_build,
        Some(true)
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
        .connect(0, true, 30_000, None, &mut t.radio)
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
    let mut other = t.saved(SAVED);
    other.id = 78;
    other.peer.address[0] ^= 1;
    let second = other.peer;
    block_on(cordial_core::bonds::commit(
        &mut t.store,
        &other,
        &bond(78, second),
    ))
    .unwrap();
    block_on(t.app.manager.fill(&mut t.store, &mut t.radio)).unwrap();
    assert!(t.radio.bonds.contains(&second));
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
    assert_eq!(t.entry(78).state, ConnectionState::Connecting);
    t.poll();
    assert!(t.radio.reconnect.is_empty(), "pause during HID setup");
    assert_eq!(t.store.records, saved);
    assert_eq!(t.app.serial.queued(), 0);
}

/// Moves the saved device to `peer`, in memory and in its saved record.
fn move_saved(t: &mut Test, peer: Peer) {
    let mut policy = t.saved(SAVED);
    policy.peer = peer;
    block_on(cordial_core::bonds::commit(
        &mut t.store,
        &policy,
        &bond(policy.id, peer),
    ))
    .unwrap();
    t.entry(SAVED).peer = peer;
    t.radio.bonds = vec![peer];
}

fn saved_ble(t: &mut Test) -> Peer {
    let peer = Peer {
        transport: Transport::Ble,
        ..t.entry(SAVED).peer
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
        let now = t.now;
        match reason {
            "paused" => t.entry(SAVED).paused = true,
            "untrusted" => t.entry(SAVED).trusted = false,
            // A blocked or disabled device leaves the resident set.
            "blocked" | "disabled" => {
                t.ok(Command::SetDevice(p::SetDevice {
                    device: SAVED,
                    blocked: (reason == "blocked").then_some(true),
                    enabled: (reason == "disabled").then_some(false),
                    ..Default::default()
                }));
                assert!(t.app.manager.find(SAVED.into()).is_none());
            }
            _ => t.entry(SAVED).connection(
                ConnectionState::Disconnected,
                Some(if reason == "authentication" {
                    ErrorCode::AuthenticationFailed
                } else {
                    ErrorCode::Timeout
                }),
                now,
            ),
        }
        t.event(Event::Incoming { attempt: 1, peer });
        assert_eq!(t.radio.incoming.last(), Some(&None), "{reason}");
        t.poll();
        assert!(t.radio.reconnect.is_empty(), "{reason}");
        // A failure, authentication included, only delays the next attempt.
        if matches!(reason, "authentication" | "cooldown") {
            t.now += FIRST_RETRY;
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
    for n in 2..=5.min(cordial_core::devices::CLASSIC_CONNECTIONS as u8) {
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
        peer: peer(2),
    });
    assert_eq!(t.radio.connects.len(), 2);
    t.poll();
    assert_eq!(t.radio.connects.len(), 2, "the pairing reserve stays free");
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
        layout: None,
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
    t.now += FIRST_RETRY;
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
fn repeated_changes_are_reported_once_with_the_latest_state() {
    let mut t = Test::new(true);
    for paused in [true, false, true] {
        t.app.manager.devices[0].as_mut().unwrap().paused = paused;
        // Each change marks the device; only the state when output frees is written.
        t.ok(Command::SetDevice(p::SetDevice {
            device: SAVED,
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
/// Makes the saved keyboard a newly paired device: no integration preference, setup pending.
fn pending_setup(t: &mut Test) {
    t.write_policy(SAVED, |p| {
        p.integrations.clear();
        p.setup_pending = true;
    });
    t.entry(SAVED).hidpp_enabled = false;
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
        layout: None,
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
fn saved_policy(t: &mut Test) -> Policy {
    t.saved(SAVED)
}

#[test]
fn first_connection_setup_turns_hidpp_on_for_a_hidpp_2_device() {
    let mut t = Test::new(true);
    pending_setup(&mut t);
    let link = connect_saved(&mut t, hidpp_descriptor());
    t.poll();
    answer_protocol(&mut t, link, 4);
    // Setup is saved once the connection's wait for its first input has passed.
    assert!(saved_policy(&mut t).setup_pending);
    t.settle();
    let events = t.events();
    assert!(saved_policy(&mut t).hidpp_enabled() && !saved_policy(&mut t).setup_pending);
    let record = storage::Device::decode(
        t.store.records[&cordial_core::storage::record_key(2, 77)].as_slice(),
    )
    .unwrap()
    .policy
    .unwrap();
    assert_eq!(
        record.integrations,
        [storage::IntegrationPreference {
            integration: storage::Integration::Hidpp.into(),
            enabled: true,
        }]
    );
    assert!(!record.setup_pending);
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
        t.settle();
        assert!(!saved_policy(&mut t).hidpp_enabled() && !saved_policy(&mut t).setup_pending);
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

/// Counts allocations while asked to, and fails the next one once asked to.
struct CountAllocations;
thread_local! {
    static TRACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HEAP: std::cell::Cell<(isize, isize)> = const { std::cell::Cell::new((0, 0)) };
}
fn heap_change(bytes: isize) {
    let _ = HEAP.try_with(|heap| {
        let (live, peak) = heap.get();
        let live = live + bytes;
        heap.set((live, peak.max(live)));
    });
}
unsafe impl std::alloc::GlobalAlloc for CountAllocations {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if fails(layout) {
            return std::ptr::null_mut();
        }
        if TRACK.try_with(|v| v.get()).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        let ptr = unsafe { std::alloc::System.alloc(layout) };
        if !ptr.is_null() {
            heap_change(layout.size() as isize);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        heap_change(-(layout.size() as isize));
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        if fails(std::alloc::Layout::from_size_align(size, layout.align()).unwrap()) {
            return std::ptr::null_mut();
        }
        if TRACK.try_with(|v| v.get()).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        let ptr = unsafe { std::alloc::System.realloc(ptr, layout, size) };
        if !ptr.is_null() {
            heap_change(size as isize - layout.size() as isize);
        }
        ptr
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
        layout: None,
    });
    t.settle();
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
        device: SAVED,
        blocked: Some(true),
        ..Default::default()
    }));
    assert_eq!(
        t.code(Command::ConnectDevice(p::ConnectDevice { device: SAVED })),
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
    t.store.records.insert(
        cordial_core::storage::record_key(2, 77),
        UNDECODABLE.to_vec(),
    );
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED,
            trusted: Some(false),
            ..Default::default()
        })),
        p::ErrorCode::NotFound
    );
    assert!(
        t.events()
            .contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED }))
    );
    assert!(t.app.manager.devices.iter().all(Option::is_none));
}

#[test]
fn a_pairing_cancelled_by_the_session_ending_reports_nothing_to_the_next_one() {
    let mut t = Test::new(false);
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(candidate);
    t.poll();
    let link = t.radio.connects[0].0;
    t.app.session(false, &mut t.radio);
    t.app.session(true, &mut t.radio);
    t.event(Event::Disconnected { link, error: None });
    t.poll();
    assert!(pairing_steps(&t.events()).is_empty());
    assert!(t.app.manager.devices.iter().all(Option::is_none));
}

#[test]
fn a_connected_device_going_away_records_no_error() {
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, hidpp_descriptor());
    t.poll();
    assert_eq!(t.device(SAVED).state, p::DeviceState::Connected as i32);
    t.event(Event::Disconnected {
        link,
        error: Some(cordial_core::model::errors::ErrorCode::ConnectionFailed),
    });
    let d = t.device(SAVED);
    assert_eq!(d.state, p::DeviceState::Disconnected as i32);
    assert_eq!(d.errors.first().map(|e| e.code), None);
}

#[test]
fn a_link_lost_while_connecting_records_its_error() {
    let mut t = Test::new(true);
    t.poll();
    let link = t.radio.connects.last().unwrap().0;
    t.event(Event::Disconnected {
        link,
        error: Some(cordial_core::model::errors::ErrorCode::ConnectionFailed),
    });
    assert_eq!(
        t.device(SAVED).errors.first().map(|e| e.code),
        Some(p::ErrorCode::ConnectionFailed as i32)
    );
}

#[test]
fn a_device_failing_authentication_keeps_reconnecting() {
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, hidpp_descriptor());
    t.poll();
    assert_eq!(t.device(SAVED).state, p::DeviceState::Connected as i32);
    t.event(Event::Disconnected {
        link,
        error: Some(cordial_core::model::errors::ErrorCode::AuthenticationFailed),
    });
    let failed = t.now;
    assert_eq!(
        t.device(SAVED).errors.first().map(|e| e.code),
        Some(p::ErrorCode::AuthFailed as i32)
    );
    // Anyone can fail a handshake with the device's address, so this only backs off.
    let attempts = t.radio.connects.len();
    t.now = failed + FIRST_RETRY - 2;
    t.poll();
    assert_eq!(t.radio.connects.len(), attempts);
    t.poll();
    assert_eq!(t.radio.connects.len(), attempts + 1);
}

#[test]
fn no_error_stops_automatic_reconnection() {
    for error in [
        ErrorCode::UnsupportedHid,
        ErrorCode::UnsupportedTransport,
        ErrorCode::AuthenticationFailed,
        ErrorCode::AuthenticationRejected,
    ] {
        let mut t = Test::new(true);
        t.poll();
        let link = t.radio.connects[0].0;
        t.event(Event::Security {
            link,
            security: cordial_core::bluetooth::ConnectionSecurity {
                encrypted: Some(true),
                authenticated: None,
                secure_connections: None,
                key_size: Some(16),
                bonded: Some(true),
            },
        });
        t.event(Event::Disconnected {
            link,
            error: Some(error),
        });
        let failed = t.now;
        assert!(
            t.device(SAVED).errors.first().map(|e| e.code).is_some(),
            "{error:?}"
        );
        t.now = failed + FIRST_RETRY - 2;
        t.poll();
        assert_eq!(t.radio.connects.len(), 1, "{error:?}");
        t.poll();
        assert_eq!(t.radio.connects.len(), 2, "{error:?}");
    }
}

const KEYBOARD_MAP: &[u8] = &[
    5, 1, 9, 6, 0xa1, 1, 5, 7, 0x19, 4, 0x29, 11, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8, 0x81, 2, 0xc0,
];
/// The same keyboard sending report ID 1.
const NUMBERED_MAP: &[u8] = &[
    5, 1, 9, 6, 0xa1, 1, 0x85, 1, 5, 7, 0x19, 4, 0x29, 11, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8,
    0x81, 2, 0xc0,
];
fn classic_layout(map: &[u8]) -> Layout {
    Layout {
        maps: vec![ReportMap(map.to_vec())],
        reports: Vec::new(),
        hash: None,
    }
}
fn ble_layout() -> Layout {
    Layout {
        maps: vec![ReportMap(KEYBOARD_MAP.to_vec())],
        reports: vec![LayoutReport {
            service: 0,
            kind: ReportType::Input,
            id: 0,
            value: 0x10,
            properties: 0x12,
            cccd: 0x11,
        }],
        hash: None,
    }
}
fn layout_key() -> cordial_core::storage::RecordKey {
    cordial_core::storage::record_key(5, 77)
}
fn put_layout(t: &mut Test, layout: &Layout) {
    t.store
        .records
        .insert(layout_key(), cordial_core::layouts::encode(layout).unwrap());
}
fn saved_layout(t: &Test) -> Option<Layout> {
    t.store
        .records
        .get(&layout_key())
        .map(|bytes| cordial_core::layouts::decode(bytes).unwrap())
}
fn drain_forward(t: &mut Test) {
    while t.app.manager.forward.packet().is_some() {
        t.app.manager.forward.complete();
    }
}
/// Adds a saved BLE device after the Classic one.
fn add_ble(t: &mut Test) -> Peer {
    let peer = Peer {
        transport: Transport::Ble,
        ..peer(2)
    };
    let mut policy = Policy::paired(78, peer, b"Mouse");
    policy.setup_pending = false;
    block_on(cordial_core::bonds::commit(
        &mut t.store,
        &policy,
        &bond(78, peer),
    ))
    .unwrap();
    block_on(t.app.manager.fill(&mut t.store, &mut t.radio)).unwrap();
    assert!(t.radio.bonds.contains(&peer));
    peer
}

#[test]
fn saved_layouts_are_supplied_to_reconnections() {
    let mut t = Test::new(true);
    let layout = classic_layout(KEYBOARD_MAP);
    put_layout(&mut t, &layout);
    t.poll();
    assert_eq!(t.radio.layouts, [Some(layout.clone())], "background paging");
    let link = t.radio.connects[0].0;
    t.event(Event::Disconnected {
        link,
        error: Some(ErrorCode::ConnectionFailed),
    });
    t.ok(Command::ConnectDevice(p::ConnectDevice { device: SAVED }));
    assert_eq!(
        t.radio.layouts.last(),
        Some(&Some(layout)),
        "explicit connect"
    );

    let mut t = Test::new(true);
    let peer = saved_ble(&mut t);
    put_layout(&mut t, &ble_layout());
    t.poll();
    t.event(Event::Incoming { attempt: 1, peer });
    assert_eq!(t.radio.layouts, [Some(ble_layout())], "incoming link");
}

#[test]
fn an_unusable_saved_layout_is_ignored_and_removed() {
    for bytes in [
        UNDECODABLE.to_vec(),
        cordial_core::layouts::encode(&ble_layout()).unwrap(),
    ] {
        let mut t = Test::new(true);
        t.store.records.insert(layout_key(), bytes);
        t.poll();
        assert_eq!(t.radio.layouts, [None]);
        assert_eq!(saved_layout(&t), None);
    }
}

#[test]
fn pairing_supplies_no_layout_and_saves_the_discovered_one() {
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
    let candidate = t.candidate(peer(1), peer(1));
    t.pair(candidate);
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    assert!(t.radio.connects[0].1, "a pairing link");
    assert_eq!(t.radio.layouts, [None]);
    let link = t.radio.connects[0].0;
    t.event(Event::Bonded {
        link,
        identity: peer(1),
    });
    // The secondary loop finds the saved device and saves the bond.
    t.poll();
    assert_eq!(
        saved_layout(&t),
        None,
        "the new bond's layout is not known yet"
    );
    let discovered = classic_layout(NUMBERED_MAP);
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: Some(discovered.clone()),
    });
    // The layout is saved once the connection forwards input, after its policy is read.
    t.poll();
    t.poll();
    assert_eq!(saved_layout(&t), None);
    drain_forward(&mut t);
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    let reads = t.store.reads.len();
    t.poll();
    assert_eq!(
        t.store.reads[reads],
        cordial_core::storage::record_key(2, SAVED.into()),
        "the policy is read first"
    );
    // The layout is saved once input pauses.
    assert_eq!(saved_layout(&t), None);
    t.quiet();
    assert_eq!(saved_layout(&t), Some(discovered));
}

#[test]
fn only_a_usable_discovered_layout_is_saved() {
    for (layout, available, saved) in [
        (None, None, false),
        (Some(classic_layout(KEYBOARD_MAP)), None, true),
        (Some(ble_layout()), None, false),
        // The space kept for maintenance and for pairing another device stays free.
        (
            Some(classic_layout(KEYBOARD_MAP)),
            Some(cordial_core::bonds::MAINTENANCE_BYTES + cordial_core::bonds::PAIR_BYTES),
            false,
        ),
        (Some(classic_layout(KEYBOARD_MAP)), Some(60_000), true),
    ] {
        let mut t = Test::new(true);
        t.store.available = available;
        t.poll();
        let link = t.radio.connects[0].0;
        t.event(Event::Connected {
            link,
            descriptors: descriptor(),
            max_output: 255,
            layout: layout.clone(),
        });
        t.settle();
        t.poll();
        assert_eq!(t.device(SAVED).state, p::DeviceState::Connected as i32);
        assert_eq!(saved_layout(&t), layout.filter(|_| saved));
    }
}

#[test]
fn a_changed_layout_moves_the_live_link_and_is_saved() {
    let mut t = Test::new(true);
    t.poll();
    let link = t.radio.connects[0].0;
    let changed = classic_layout(NUMBERED_MAP);
    // A link still in setup has no layout to change.
    t.event(Event::Layout {
        link,
        descriptors: descriptor(),
        layout: changed.clone(),
    });
    assert_eq!(saved_layout(&t), None);
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    drain_forward(&mut t);
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes()[0], 16);
    t.app.manager.forward.complete();
    let numbered =
        vec![cordial_core::bluetooth::Descriptor::from_slice(ServiceId(7), NUMBERED_MAP).unwrap()];
    t.event(Event::Layout {
        link,
        descriptors: numbered,
        layout: changed.clone(),
    });
    // The old layout's held key is released, and the device stays connected.
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes(), &[0; 32]);
    drain_forward(&mut t);
    assert_eq!(t.device(SAVED).state, p::DeviceState::Connected as i32);
    assert!(t.radio.closes.is_empty());
    t.poll();
    t.poll();
    assert_eq!(saved_layout(&t), None, "input has not paused");
    t.quiet();
    assert_eq!(saved_layout(&t), Some(changed));
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 1, &[1]).unwrap(),
    ));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes()[0], 16);
    t.app.manager.forward.complete();
    // A layout the link cannot use closes it, and the saved one no longer applies.
    t.event(Event::Layout {
        link,
        descriptors: Vec::new(),
        layout: classic_layout(KEYBOARD_MAP),
    });
    assert_eq!(t.radio.closes, [link]);
    assert_eq!(saved_layout(&t), None);
}

#[test]
fn a_changed_layout_that_cannot_be_saved_removes_the_saved_one() {
    for (layout, available) in [
        (ble_layout(), None),
        (
            classic_layout(NUMBERED_MAP),
            Some(cordial_core::bonds::MAINTENANCE_BYTES - 1),
        ),
    ] {
        let mut t = Test::new(true);
        put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
        let link = connect_saved(&mut t, descriptor());
        t.store.available = available;
        t.event(Event::Layout {
            link,
            descriptors: descriptor(),
            layout,
        });
        t.settle();
        t.poll();
        assert_eq!(t.device(SAVED).state, p::DeviceState::Connected as i32);
        assert_eq!(saved_layout(&t), None);
    }
}

#[test]
fn a_busy_radio_delays_the_page_without_reading_the_layout_each_poll() {
    let mut t = Test::new(true);
    put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
    t.radio.reject_connect = Some(ErrorCode::Busy);
    t.poll();
    let loads = t.store.loads;
    t.poll();
    assert_eq!(t.store.loads, loads);
    t.radio.reject_connect = None;
    t.now += 1000;
    t.poll();
    assert_eq!(t.radio.layouts, [Some(classic_layout(KEYBOARD_MAP))]);
}

#[test]
fn a_ble_device_that_went_away_may_return_at_once() {
    let mut t = Test::new(true);
    let peer = saved_ble(&mut t);
    t.poll();
    t.event(Event::Incoming { attempt: 1, peer });
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    t.event(Event::Disconnected {
        link,
        error: Some(ErrorCode::ConnectionFailed),
    });
    t.poll();
    assert_eq!(t.radio.reconnect, [peer]);
    t.event(Event::Incoming { attempt: 2, peer });
    assert!(t.radio.incoming.last().unwrap().is_some());
    // A connection that fails before it is connected still backs off.
    let link = t.radio.connects[1].0;
    t.event(Event::Disconnected {
        link,
        error: Some(ErrorCode::ConnectionFailed),
    });
    t.poll();
    assert!(t.radio.reconnect.is_empty());
    t.event(Event::Incoming { attempt: 3, peer });
    assert_eq!(t.radio.incoming.last(), Some(&None));
}

#[test]
fn a_classic_device_that_went_away_is_paged_after_the_first_delay() {
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, descriptor());
    t.event(Event::Disconnected {
        link,
        error: Some(ErrorCode::ConnectionFailed),
    });
    let lost = t.now;
    t.now = lost + FIRST_RETRY - 2;
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    t.poll();
    assert_eq!(t.radio.connects.len(), 2);
    // A failed page doubles the delay.
    t.event(Event::Disconnected {
        link: t.radio.connects[1].0,
        error: Some(ErrorCode::Timeout),
    });
    let failed = t.now;
    t.now = failed + 2 * FIRST_RETRY - 2;
    t.poll();
    assert_eq!(t.radio.connects.len(), 2);
    t.poll();
    assert_eq!(t.radio.connects.len(), 3);
}

#[test]
fn an_outgoing_classic_page_does_not_hold_up_ble_reconnection() {
    let mut t = Test::new(true);
    let ble = add_ble(&mut t);
    t.poll();
    assert_eq!(t.radio.connects.len(), 1, "the Classic page");
    assert_eq!(t.radio.reconnect, [ble]);
    t.event(Event::Incoming {
        attempt: 1,
        peer: ble,
    });
    assert!(t.radio.incoming.last().unwrap().is_some());
    t.poll();
    assert!(t.radio.reconnect.is_empty(), "BLE setups run one at a time");
    assert_eq!(t.radio.connects.len(), 2);
}

#[test]
fn a_ble_link_with_a_changed_layout_reads_its_information_again() {
    let mut t = Test::new(true);
    let peer = saved_ble(&mut t);
    t.poll();
    t.event(Event::Incoming { attempt: 1, peer });
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    t.poll();
    assert!(t.radio.info_refreshes.is_empty());
    t.event(Event::Layout {
        link,
        descriptors: descriptor(),
        layout: ble_layout(),
    });
    t.poll();
    assert_eq!(t.radio.info_refreshes, [link]);
}

fn update(transport: p::Transport, enabled: bool) -> p::TransportUpdate {
    p::TransportUpdate {
        transport: transport as i32,
        enabled: Some(enabled),
    }
}
fn set_transports(t: &mut Test, updates: &[(p::Transport, bool)]) -> p::Status {
    t.set_adapter(p::SetAdapter {
        transports: updates.iter().map(|(t, on)| update(*t, *on)).collect(),
        ..Default::default()
    })
}
/// The supported transports, each with whether it is enabled.
fn transports(s: &p::Status) -> Vec<(i32, bool)> {
    s.transports
        .iter()
        .map(|t| (t.transport, t.enabled.expect("always set")))
        .collect()
}
const CLASSIC: i32 = p::Transport::Classic as i32;
const BLE: i32 = p::Transport::Ble as i32;
const SAVED_BLE: u32 = 78;

#[test]
fn classic_starts_disabled_and_ble_enabled() {
    let mut store = Store::default();
    block_on(cordial_core::identity::Identity::initialize(
        &mut store,
        [2; 6],
        || 42,
    ))
    .unwrap();
    let mut t = Test::with(store, Radio::default(), build(false, None));
    let s = t.status();
    assert_eq!(transports(&s), [(CLASSIC, false), (BLE, true)]);
    assert_eq!(
        t.radio.applied,
        [(Transport::Classic, false), (Transport::Ble, true)]
    );
    assert_eq!(
        t.code(Command::StartScan(p::StartScan {
            transports: vec![CLASSIC],
            seconds: 0,
        })),
        p::ErrorCode::Unsupported
    );

    // Only supported transports are listed and applied.
    let (_, store, mut radio) = setup();
    radio.applied.clear();
    radio.transports = Some(cordial_core::bluetooth::Capabilities {
        classic: false,
        ble: true,
        ble_scan_and_connect: false,
    });
    let mut t = Test::with(store, radio, build(false, None));
    assert_eq!(transports(&t.status()), [(BLE, true)]);
    assert_eq!(t.radio.applied, [(Transport::Ble, true)]);
}

#[test]
fn an_unsupported_transport_cannot_be_changed() {
    let (_, store, mut radio) = setup();
    radio.transports = Some(cordial_core::bluetooth::Capabilities {
        classic: false,
        ble: true,
        ble_scan_and_connect: false,
    });
    let mut t = Test::with(store, radio, build(false, None));
    assert_eq!(
        t.device(SAVED).inactive,
        Some(p::InactiveReason::UnsupportedTransport as i32)
    );
    let saved = t.store.records.clone();
    for (transport, code) in [
        (CLASSIC, p::ErrorCode::Unsupported),
        (99, p::ErrorCode::Unsupported),
        (p::Transport::Unspecified as i32, p::ErrorCode::BadArgs),
    ] {
        assert_eq!(
            t.code(Command::SetAdapter(p::SetAdapter {
                name: Some("Desk".into()),
                transports: vec![p::TransportUpdate {
                    transport,
                    enabled: Some(false),
                }],
                ..Default::default()
            })),
            code
        );
    }
    assert_eq!(t.store.records, saved);
}

#[test]
fn disabling_classic_closes_its_links_and_refuses_classic_work() {
    let mut t = Test::new(true);
    let ble = add_ble(&mut t);
    t.radio.applied.clear();
    t.poll();
    let link = t.radio.connects[0].0;
    t.scan(&[p::Transport::Classic, p::Transport::Ble]);
    let token = t.radio.scans.last().unwrap().0;
    let saved = t.store.records.clone();
    let s = set_transports(&mut t, &[(p::Transport::Classic, false)]);
    assert_ne!(t.store.records, saved);
    assert_eq!(transports(&s), [(CLASSIC, false), (BLE, true)]);
    assert_eq!(t.radio.applied, [(Transport::Classic, false)]);
    assert_eq!(t.radio.closes, [link]);
    t.finish_disconnects();
    // The scan continues over BLE only.
    assert_eq!(t.radio.scans.last(), Some(&(token, false, true)));
    let d = t.device(SAVED);
    assert_eq!(
        d.inactive,
        Some(p::InactiveReason::TransportDisabled as i32)
    );
    assert_eq!(d.errors.first().map(|e| e.code), None);
    assert!(!t.radio.bonds.contains(&peer(1)));
    // Classic devices are neither paged nor admitted, and Classic commands are refused as for
    // an unsupported transport. BLE is unaffected.
    t.now += 600_000;
    t.poll();
    assert_eq!(t.radio.connects.len(), 1);
    assert_eq!(t.radio.reconnect, [ble]);
    t.event(Event::Incoming {
        attempt: 1,
        peer: peer(1),
    });
    assert_eq!(t.radio.incoming.last(), Some(&None));
    assert_eq!(
        t.code(Command::ConnectDevice(p::ConnectDevice { device: SAVED })),
        p::ErrorCode::Unsupported
    );
    assert_eq!(
        t.code(Command::StartScan(p::StartScan {
            transports: vec![CLASSIC],
            seconds: 0,
        })),
        p::ErrorCode::Unsupported
    );
    t.event(Event::Incoming {
        attempt: 2,
        peer: ble,
    });
    assert!(t.radio.incoming.last().unwrap().is_some());

    // Enabling it makes the saved device eligible again; background work reads its record.
    let s = set_transports(&mut t, &[(p::Transport::Classic, true)]);
    assert_eq!(transports(&s), [(CLASSIC, true), (BLE, true)]);
    assert_eq!(
        t.radio.applied,
        [(Transport::Classic, false), (Transport::Classic, true)]
    );
    t.poll();
    assert_eq!(t.device(SAVED).inactive, None);
    assert!(t.radio.bonds.contains(&peer(1)));
    t.ok(Command::ConnectDevice(p::ConnectDevice { device: SAVED }));
    assert_eq!(t.radio.addresses.last(), Some(&peer(1)));
}

#[test]
fn disabling_ble_closes_its_links_and_stops_reconnecting() {
    let mut t = Test::new(true);
    let ble = add_ble(&mut t);
    t.radio.applied.clear();
    t.poll();
    let classic = t.radio.connects[0].0;
    assert_eq!(t.radio.reconnect, [ble]);
    t.event(Event::Incoming {
        attempt: 1,
        peer: ble,
    });
    let link = t.radio.connects[1].0;
    t.scan(&[p::Transport::Classic, p::Transport::Ble]);
    let token = t.radio.scans.last().unwrap().0;
    let s = set_transports(&mut t, &[(p::Transport::Ble, false)]);
    assert_eq!(transports(&s), [(CLASSIC, true), (BLE, false)]);
    assert_eq!(t.radio.applied, [(Transport::Ble, false)]);
    assert_eq!(t.radio.closes, [link]);
    assert!(t.app.manager.connection(classic).is_some());
    t.finish_disconnects();
    assert_eq!(t.radio.scans.last(), Some(&(token, true, false)));
    assert!(t.radio.reconnect.is_empty());
    assert_eq!(
        t.device(SAVED_BLE).inactive,
        Some(p::InactiveReason::TransportDisabled as i32)
    );
    assert_eq!(t.device(SAVED).inactive, None);
    t.event(Event::Incoming {
        attempt: 2,
        peer: ble,
    });
    assert_eq!(t.radio.incoming.last(), Some(&None));
    for command in [
        Command::ConnectDevice(p::ConnectDevice { device: SAVED_BLE }),
        Command::StartScan(p::StartScan {
            transports: vec![BLE],
            seconds: 0,
        }),
    ] {
        assert_eq!(t.code(command), p::ErrorCode::Unsupported);
    }
    set_transports(&mut t, &[(p::Transport::Ble, true)]);
    t.poll();
    assert_eq!(t.device(SAVED_BLE).inactive, None);
    // The closed link's backoff still applies.
    t.now += FIRST_RETRY;
    t.poll();
    assert_eq!(t.radio.reconnect, [ble]);
}

#[test]
fn both_transports_can_be_disabled() {
    let mut t = Test::new(true);
    add_ble(&mut t);
    t.radio.applied.clear();
    let s = set_transports(
        &mut t,
        &[(p::Transport::Classic, false), (p::Transport::Ble, false)],
    );
    assert_eq!(transports(&s), [(CLASSIC, false), (BLE, false)]);
    assert_eq!(
        t.radio.applied,
        [(Transport::Classic, false), (Transport::Ble, false)]
    );
    t.now += 600_000;
    t.poll();
    assert!(t.radio.connects.is_empty());
    assert!(t.radio.reconnect.is_empty());
    for id in [SAVED, SAVED_BLE] {
        assert_eq!(
            t.device(id).inactive,
            Some(p::InactiveReason::TransportDisabled as i32)
        );
    }
    let mut reloaded = cordial_core::manager::Manager::default();
    block_on(reloaded.load(&mut t.store, &mut t.radio)).unwrap();
    assert_eq!(
        reloaded.preference.transports,
        cordial_core::devices::Transports::NONE
    );
}

#[test]
fn transport_updates_apply_in_order() {
    let mut t = Test::new(true);
    t.radio.applied.clear();
    let s = set_transports(
        &mut t,
        &[
            (p::Transport::Classic, false),
            (p::Transport::Classic, true),
        ],
    );
    assert_eq!(transports(&s), [(CLASSIC, true), (BLE, true)]);
    assert!(t.radio.applied.is_empty(), "unchanged");
    // An update without a value changes nothing.
    let saved = t.store.records.clone();
    t.ok(Command::SetAdapter(p::SetAdapter {
        transports: vec![p::TransportUpdate {
            transport: CLASSIC,
            enabled: None,
        }],
        ..Default::default()
    }));
    assert_eq!(t.store.records, saved);
}

#[test]
fn disabling_a_transport_ends_its_scan_and_pairing() {
    for (transport, candidate) in [
        (p::Transport::Classic, peer(2)),
        (
            p::Transport::Ble,
            Peer {
                transport: Transport::Ble,
                ..peer(2)
            },
        ),
    ] {
        let mut t = Test::new(false);
        let id = t.candidate(candidate, candidate);
        t.pair(id);
        t.poll();
        let link = t.radio.connects[0].0;
        t.ok(Command::StopScan(p::StopScan {}));
        t.scan(&[transport]);
        t.events();
        set_transports(&mut t, &[(transport, false)]);
        assert_eq!(t.radio.closes, [link]);
        t.finish_disconnects();
        let events = t.events();
        assert!(events.iter().any(|e| matches!(e, Ev::ScanDone(_))));
        assert_eq!(
            pairing_steps(&events).last(),
            Some(&p::pairing::Step::Error(p::Error {
                code: p::ErrorCode::Unsupported as i32,
                outcome: p::ErrorOutcome::NoEffect as i32,
                ..Default::default()
            }))
        );
        assert_eq!(
            t.radio.scans.last().map(|s| (s.1, s.2)),
            Some((false, false))
        );
    }
}

#[test]
fn disabling_a_transport_ends_a_pairing_that_has_not_started_its_link() {
    let mut t = Test::new(false);
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(candidate);
    set_transports(&mut t, &[(p::Transport::Classic, false)]);
    let events = t.events();
    assert!(t.radio.connects.is_empty());
    assert_eq!(
        pairing_steps(&events).last(),
        Some(&p::pairing::Step::Error(p::Error {
            code: p::ErrorCode::Unsupported as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }))
    );
}

#[test]
fn adapter_updates_keep_the_transport_settings() {
    let mut t = Test::new(true);
    let s = t.set_adapter(p::SetAdapter {
        platform: Some(p::Platform::Mac as i32),
        transports: vec![update(p::Transport::Classic, false)],
        ..Default::default()
    });
    assert_eq!(s.platform, p::Platform::Mac as i32);
    assert_eq!(transports(&s), [(CLASSIC, false), (BLE, true)]);
    let s = t.set_adapter(p::SetAdapter {
        name: Some("Desk".into()),
        ..Default::default()
    });
    assert_eq!(transports(&s), [(CLASSIC, false), (BLE, true)]);
    let mut reloaded = cordial_core::manager::Manager::default();
    block_on(reloaded.load(&mut t.store, &mut t.radio)).unwrap();
    assert_eq!(
        reloaded.preference,
        cordial_core::devices::AdapterPreference {
            name: Some("Desk".into()),
            host_platform: cordial_core::model::identifiers::HostPlatform::Mac,
            transports: Default::default(),
            ..Default::default()
        }
    );
}

#[test]
fn a_radio_that_cannot_apply_a_transport_fails_but_the_setting_is_saved() {
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, descriptor());
    t.radio.reject_transport = Some(ErrorCode::RadioUnavailable);
    let s = set_transports(&mut t, &[(p::Transport::Classic, false)]);
    assert!(!s.ready);
    assert!(
        !t.app
            .manager
            .preference
            .transports
            .contains(Transport::Classic)
    );
    assert_eq!(
        t.app.manager.devices[0].as_ref().unwrap().state,
        ConnectionState::Disconnected
    );
    assert!(t.app.manager.connection(link).is_none());
}

#[test]
fn a_saved_layout_refreshes_the_free_space_estimate() {
    let mut t = Test::new(true);
    t.poll();
    let link = t.radio.connects[0].0;
    t.store.available = Some(60_000);
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: Some(classic_layout(KEYBOARD_MAP)),
    });
    t.settle();
    t.poll();
    assert_eq!(t.app.manager.available_bytes, 60_000);
}

#[test]
fn layouts_are_not_written_while_storage_is_not_ready() {
    let mut t = Test::new(true);
    put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
    t.poll();
    let link = t.radio.connects[0].0;
    t.app.manager.storage_ready = false;
    let saved = t.store.records.clone();
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: Some(classic_layout(NUMBERED_MAP)),
    });
    t.event(Event::Layout {
        link,
        descriptors: Vec::new(),
        layout: ble_layout(),
    });
    t.event(Event::Disconnected {
        link,
        error: Some(ErrorCode::UnsupportedHid),
    });
    assert_eq!(t.store.records, saved);
}

#[test]
fn a_link_ending_with_an_unusable_layout_removes_the_saved_one() {
    for (error, kept) in [
        (ErrorCode::ConnectionFailed, true),
        (ErrorCode::Timeout, true),
        (ErrorCode::UnsupportedHid, false),
    ] {
        let mut t = Test::new(true);
        put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
        t.poll();
        let link = t.radio.connects[0].0;
        t.event(Event::Disconnected {
            link,
            error: Some(error),
        });
        assert_eq!(saved_layout(&t).is_some(), kept, "{error:?}");
    }
    // A link the adapter closes because the HID layout cannot be used.
    let mut t = Test::new(true);
    put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: Vec::new(),
        max_output: 255,
        layout: None,
    });
    assert_eq!(t.radio.closes, [link]);
    t.finish_disconnects();
    assert_eq!(saved_layout(&t), None);
}

#[test]
fn a_layout_with_the_same_maps_keeps_the_live_link() {
    let mut t = Test::new(true);
    put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
    let link = connect_saved(&mut t, descriptor());
    drain_forward(&mut t);
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    assert_eq!(t.app.manager.forward.packet().unwrap().bytes()[0], 16);
    t.app.manager.forward.complete();
    let moved = Layout {
        hash: Some(cordial_core::bluetooth::DatabaseHash([7; 16])),
        ..classic_layout(KEYBOARD_MAP)
    };
    t.event(Event::Layout {
        link,
        descriptors: descriptor(),
        layout: moved.clone(),
    });
    // The held key is not released, and the new layout is saved once input pauses.
    assert!(t.app.manager.forward.packet().is_none());
    t.poll();
    t.poll();
    t.quiet();
    assert_eq!(saved_layout(&t), Some(moved));
}

#[test]
fn a_ble_device_that_keeps_dropping_right_after_connecting_waits() {
    let mut t = Test::new(true);
    let peer = saved_ble(&mut t);
    t.poll();
    for attempt in 1..=3 {
        t.event(Event::Incoming { attempt, peer });
        let link = t.radio.connects.last().unwrap().0;
        t.event(Event::Connected {
            link,
            descriptors: descriptor(),
            max_output: 255,
            layout: None,
        });
        t.event(Event::Disconnected {
            link,
            error: Some(ErrorCode::ConnectionFailed),
        });
        t.poll();
    }
    // Two rapid drops are readmitted at once; the third waits a second.
    assert_eq!(t.radio.incoming.iter().flatten().count(), 3);
    assert!(t.radio.reconnect.is_empty());
    t.event(Event::Incoming { attempt: 4, peer });
    assert_eq!(t.radio.incoming.last(), Some(&None));
    t.now += 1000;
    t.poll();
    assert_eq!(t.radio.reconnect, [peer]);
}

#[test]
fn a_ble_device_power_cycled_every_few_seconds_is_always_readmitted() {
    let mut t = Test::new(true);
    let peer = saved_ble(&mut t);
    t.poll();
    for (attempt, connected_for) in [2000, 6000, 3000, 2500, 4000, 2000].into_iter().enumerate() {
        t.event(Event::Incoming {
            attempt: attempt as u32,
            peer,
        });
        assert!(t.radio.incoming.last().unwrap().is_some(), "{attempt}");
        let link = t.radio.connects.last().unwrap().0;
        t.event(Event::Connected {
            link,
            descriptors: descriptor(),
            max_output: 255,
            layout: None,
        });
        t.now += connected_for;
        t.event(Event::Disconnected { link, error: None });
        t.poll();
        assert_eq!(t.radio.reconnect, [peer], "{attempt}");
    }
}

#[test]
fn a_layout_never_takes_the_room_kept_for_pairing() {
    let mut t = Test::new(true);
    t.poll();
    let link = t.radio.connects[0].0;
    // The file fits by size, but its block does not.
    let block = 4096;
    t.store.block = Some(block);
    let used: usize = t
        .store
        .records
        .values()
        .map(|r| r.len().div_ceil(block) * block)
        .sum();
    t.store.capacity =
        Some(used + cordial_core::bonds::MAINTENANCE_BYTES + cordial_core::bonds::PAIR_BYTES + 200);
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: Some(classic_layout(KEYBOARD_MAP)),
    });
    assert_eq!(saved_layout(&t), None);
    assert!(!t.app.manager.storage_full());
}

#[test]
fn free_space_is_counted_after_writes_in_a_later_step() {
    let mut t = Test::new(true);
    let id = create(&mut t, "Keys");
    t.store.available = Some(1 << 20);
    t.poll();
    assert!(!t.app.manager.storage_full());
    let counts = t.store.counts;
    t.poll();
    assert_eq!(
        t.store.counts, counts,
        "nothing was written since the count"
    );
    // The next write fills the storage.
    t.store.available = Some(0);
    assert_eq!(
        t.ok(Command::SetProfileRules(p::SetProfileRules {
            profile: id,
            changes: vec![remap(KEYBOARD_COLLECTION, key(4), &[key(5)])],
        })),
        None
    );
    assert_eq!(t.store.counts, counts, "the write's step does not count");
    // Admission checks count for themselves.
    let error = t.error(Command::CreateProfile(p::CreateProfile {
        name: "More".into(),
    }));
    assert_eq!(error.reason, p::ErrorReason::Storage as i32);
    assert_eq!(t.store.counts, counts + 1);
    let counts = t.store.counts;
    t.poll();
    assert!(t.app.manager.storage_full());
    assert_eq!(t.store.counts, counts + 1);
    t.poll();
    assert_eq!(t.store.counts, counts + 1);
}

#[test]
fn a_discovered_layout_the_adapter_cannot_use_removes_the_saved_one() {
    let mut t = Test::new(true);
    put_layout(&mut t, &classic_layout(KEYBOARD_MAP));
    t.poll();
    let link = t.radio.connects[0].0;
    // The device was disabled while its link was set up.
    t.app.manager.devices[0].as_mut().unwrap().retiring = true;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: Some(classic_layout(NUMBERED_MAP)),
    });
    assert_eq!(t.radio.closes, [link]);
    assert_eq!(saved_layout(&t), None);
}

#[test]
fn a_transport_change_the_radio_missed_is_applied_when_it_is_ready() {
    let mut t = Test::new(true);
    t.radio.reject_transport = Some(ErrorCode::RadioUnavailable);
    let s = set_transports(&mut t, &[(p::Transport::Classic, false)]);
    assert!(!s.ready);
    t.radio.reject_transport = None;
    t.radio.applied.clear();
    t.event(Event::Ready);
    assert!(t.status().ready);
    assert_eq!(
        t.radio.applied,
        [(Transport::Classic, false), (Transport::Ble, true)]
    );
    assert_eq!(transports(&t.status()), [(CLASSIC, false), (BLE, true)]);
}

#[test]
fn scans_leave_out_disabled_transports() {
    let mut t = Test::new(false);
    set_transports(&mut t, &[(p::Transport::Classic, false)]);
    t.ok(Command::StartScan(p::StartScan {
        transports: vec![CLASSIC, BLE],
        seconds: 0,
    }));
    assert_eq!(
        t.radio.scans.last().map(|s| (s.1, s.2)),
        Some((false, true))
    );
    t.ok(Command::StopScan(p::StopScan {}));
    set_transports(&mut t, &[(p::Transport::Ble, false)]);
    let scans = t.radio.scans.len();
    assert_eq!(
        t.code(Command::StartScan(p::StartScan {
            transports: vec![CLASSIC, BLE],
            seconds: 0,
        })),
        p::ErrorCode::Unsupported
    );
    assert_eq!(t.radio.scans.len(), scans);
}

// ---------------------------------------------------------------------------
// Device residency, listings and saved records
// ---------------------------------------------------------------------------

#[test]
fn disabled_devices_are_saved_but_not_resident() {
    let mut t = Test::new(true);
    let other = t.add_saved(2);
    assert!(t.app.manager.find(other.into()).is_some());
    t.events();
    let d = t.set_device(p::SetDevice {
        device: other,
        enabled: Some(false),
        ..Default::default()
    });
    assert!(!d.enabled);
    assert_eq!(d.inactive, Some(p::InactiveReason::Disabled as i32));
    assert!(t.app.manager.find(other.into()).is_none());
    assert!(!t.radio.bonds.contains(&peer(2)));
    // The device is still listed and reported, from flash.
    assert_eq!(t.device(other), d);
    let events = t.events();
    assert!(events.contains(&Ev::Device(d.clone())), "{events:?}");
    assert_eq!(
        t.code(Command::ConnectDevice(p::ConnectDevice { device: other })),
        p::ErrorCode::Disabled
    );
    // Enabling it reads its policy again and makes it resident.
    t.ok(Command::SetDevice(p::SetDevice {
        device: other,
        enabled: Some(true),
        ..Default::default()
    }));
    assert!(t.app.manager.find(other.into()).is_some());
    assert!(t.radio.bonds.contains(&peer(2)));
    assert_eq!(t.device(other).inactive, None);
}

#[test]
fn enabling_a_transport_fills_the_stack_with_its_devices() {
    let mut t = Test::new(true);
    set_transports(&mut t, &[(p::Transport::Classic, false)]);
    t.finish_disconnects();
    t.events();
    assert!(t.app.manager.find(SAVED.into()).is_none());
    assert_eq!(
        t.device(SAVED).inactive,
        Some(p::InactiveReason::TransportDisabled as i32)
    );
    set_transports(&mut t, &[(p::Transport::Classic, true)]);
    t.poll();
    assert!(t.app.manager.find(SAVED.into()).is_some());
    assert!(t.radio.bonds.contains(&peer(1)));
    assert_eq!(t.device(SAVED).inactive, None);
}

#[test]
fn device_listings_stream_and_report_unreadable_and_lost_records() {
    let mut t = Test::new(true);
    for n in 2..=11 {
        t.add_saved(n);
    }
    t.events();
    // 77 and 78..=87.
    t.store.fail_load = Some(cordial_core::storage::record_key(2, 80));
    t.store.records.insert(
        cordial_core::storage::record_key(2, 82),
        UNDECODABLE.to_vec(),
    );
    let list = |t: &mut Test| match t.ok(Command::ListDevices(p::ListDevices {})) {
        Some(R::Devices(list)) => list,
        r => panic!("{r:?}"),
    };
    use p::device_list_entry::Entry;
    let ids = |list: &p::DeviceList| -> Vec<(u32, bool)> {
        list.entries
            .iter()
            .map(|e| match e.entry.as_ref().unwrap() {
                Entry::Device(d) => (d.id, true),
                Entry::Error(error) => {
                    assert_eq!(
                        error.error,
                        Some(p::Error {
                            code: p::ErrorCode::StorageFailed as i32,
                            reason: p::ErrorReason::Storage as i32,
                            outcome: p::ErrorOutcome::NoEffect as i32,
                            related: vec![],
                        })
                    );
                    (error.id, false)
                }
            })
            .collect()
    };
    let first = list(&mut t);
    assert_eq!(
        ids(&first),
        [
            (77, true),
            (78, true),
            (79, true),
            (80, false),
            (81, true),
            (83, true),
            (84, true),
            (85, true),
            (86, true),
            (87, true)
        ]
    );
    // A failed read is never proof of loss; the undecodable record is deleted.
    assert!(
        t.store
            .records
            .contains_key(&cordial_core::storage::record_key(2, 80))
    );
    assert!(
        !t.store
            .records
            .contains_key(&cordial_core::storage::record_key(2, 82))
    );
    assert!(t.app.manager.find(82).is_none());
    let events = t.events();
    assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: 82 })));
    assert_eq!(
        t.code(Command::GetDevice(p::GetDevice { device: 80 })),
        p::ErrorCode::StorageFailed
    );
    t.store.fail_load = None;
    assert_eq!(t.device(80).id, 80);
}

#[test]
fn a_connected_device_reads_its_policy_after_its_first_input() {
    let mut t = Test::new(true);
    // Another session saved a change the resident entry does not keep.
    t.write_policy(SAVED, |p| p.name = "Renamed".into());
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    assert!(t.live(SAVED).policy.is_none());
    // Input is forwarded before the policy is read.
    assert_eq!(press(&mut t, link, &[4]), [4]);
    assert!(t.live(SAVED).policy.is_none());
    t.poll();
    assert_eq!(&*t.live(SAVED).policy.as_ref().unwrap().name, "Renamed");
    assert_eq!(t.device(SAVED).name, "Renamed");
}

#[test]
fn descriptor_roles_are_saved_and_give_the_kinds_while_disconnected() {
    let mut t = Test::new(true);
    let d = t.device(SAVED);
    assert!(d.roles.is_empty() && d.kinds.is_empty());
    let link = connect_saved(&mut t, descriptor());
    t.settle();
    t.events();
    assert_eq!(
        t.saved(SAVED).roles,
        cordial_core::devices::Roles(cordial_core::hid::KEYBOARD)
    );
    let d = t.device(SAVED);
    assert_eq!(d.roles, [p::Role::Keyboard as i32]);
    assert_eq!(d.kinds, [p::Kind::Keyboard as i32]);
    t.ok(Command::DisconnectDevice(p::DisconnectDevice {
        device: SAVED,
    }));
    t.event(Event::Disconnected { link, error: None });
    t.events();
    let d = t.device(SAVED);
    assert_eq!(d.state, p::DeviceState::Disconnected as i32);
    assert_eq!(d.roles, [p::Role::Keyboard as i32]);
    assert_eq!(d.kinds, [p::Kind::Keyboard as i32]);
}

#[test]
fn forgetting_a_setting_without_a_saved_value_is_accepted() {
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    let settings = t.set_settings(p::SetSettings {
        device: SAVED,
        changes: vec![backlight_forget()],
    });
    assert!(settings.is_empty());
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

fn usage(page: u32, id: u32) -> p::Usage {
    p::Usage {
        usage_page: page,
        usage: id,
    }
}
fn key(id: u32) -> p::Usage {
    usage(7, id)
}
const KEYBOARD_COLLECTION: p::Usage = p::Usage {
    usage_page: 1,
    usage: 6,
};
const MOUSE_COLLECTION: p::Usage = p::Usage {
    usage_page: 1,
    usage: 2,
};
const CONSUMER_COLLECTION: p::Usage = p::Usage {
    usage_page: 0x0c,
    usage: 1,
};
const SYSTEM_COLLECTION: p::Usage = p::Usage {
    usage_page: 1,
    usage: 0x80,
};
fn rule(
    collection: p::Usage,
    input: p::Usage,
    effect: p::profile_rule::Effect,
) -> p::ProfileRuleChange {
    p::ProfileRuleChange {
        change: Some(p::profile_rule_change::Change::Rule(p::ProfileRule {
            input: Some(input),
            collection: Some(collection),
            effect: Some(effect),
        })),
    }
}
/// A rule that makes `input` hold `outputs`, whose collections the adapter resolves.
fn remap(collection: p::Usage, input: p::Usage, outputs: &[p::Usage]) -> p::ProfileRuleChange {
    rule(
        collection,
        input,
        p::profile_rule::Effect::Remap(p::profile_rule::Remap {
            outputs: outputs
                .iter()
                .map(|u| p::profile_rule::Output {
                    usage: Some(*u),
                    collection: None,
                })
                .collect(),
        }),
    )
}
fn scale(
    collection: p::Usage,
    input: p::Usage,
    numerator: i32,
    denominator: u32,
) -> p::ProfileRuleChange {
    rule(
        collection,
        input,
        p::profile_rule::Effect::Scale(p::profile_rule::Scale {
            numerator,
            denominator,
        }),
    )
}
fn forget_rule(collection: p::Usage, input: p::Usage) -> p::ProfileRuleChange {
    p::ProfileRuleChange {
        change: Some(p::profile_rule_change::Change::Forget(p::ProfileRuleRef {
            input: Some(input),
            collection: Some(collection),
        })),
    }
}
/// The saved form of a remap of `input` to keyboard keys.
fn saved_remap(collection: p::Usage, input: p::Usage, keys: &[u32]) -> p::ProfileRule {
    p::ProfileRule {
        input: Some(input),
        collection: Some(collection),
        effect: Some(p::profile_rule::Effect::Remap(p::profile_rule::Remap {
            outputs: keys
                .iter()
                .map(|k| p::profile_rule::Output {
                    usage: Some(key(*k)),
                    collection: Some(KEYBOARD_COLLECTION),
                })
                .collect(),
        })),
    }
}
fn create(t: &mut Test, name: &str) -> u32 {
    match t.ok(Command::CreateProfile(p::CreateProfile {
        name: name.into(),
    })) {
        Some(R::ProfileCreated(created)) => created.profile,
        r => panic!("{r:?}"),
    }
}
fn profile(t: &mut Test, id: u32) -> p::Profile {
    match t.ok(Command::GetProfile(p::GetProfile { profile: id })) {
        Some(R::Profile(profile)) => profile,
        r => panic!("{r:?}"),
    }
}
fn set_rules(
    t: &mut Test,
    profile: u32,
    changes: Vec<p::ProfileRuleChange>,
) -> Vec<p::ProfileRule> {
    assert_eq!(
        t.ok(Command::SetProfileRules(p::SetProfileRules {
            profile,
            changes,
        })),
        None
    );
    rules_of(t, profile)
}
/// Every rule of `profile`, read page by page.
/// Profile `profile`'s rules as its saved file holds them.
fn saved_rules(t: &mut Test, profile: u32) -> Vec<cordial_core::profiles::Rule> {
    block_on(cordial_core::profiles::saved_page(
        &mut t.store,
        profile.into(),
        None,
        usize::MAX,
    ))
    .unwrap()
}
/// A remap of `input` to keyboard `keys`.
fn remap_rule(collection: p::Usage, input: p::Usage, keys: &[u32]) -> cordial_core::profiles::Rule {
    use cordial_core::profiles;
    profiles::Rule {
        input: profiles::usage(input.usage_page as u16, input.usage as u16),
        collection: profiles::usage(collection.usage_page as u16, collection.usage as u16),
        effect: profiles::Effect::Remap(
            keys.iter()
                .map(|k| profiles::Output {
                    usage: profiles::usage(profiles::KEYBOARD_PAGE, *k as u16),
                    collection: profiles::KEYBOARD,
                })
                .collect(),
        ),
    }
}
fn rules_of(t: &mut Test, profile: u32) -> Vec<p::ProfileRule> {
    match t.ok(Command::ListProfileRules(p::ListProfileRules { profile })) {
        Some(R::ProfileRules(list)) => {
            assert_eq!(list.profile, profile);
            list.rules
        }
        r => panic!("{r:?}"),
    }
}
/// A profile whose rules remap each of `keys` to the key after it.
fn profile_with(t: &mut Test, name: &str, keys: impl IntoIterator<Item = u32>) -> u32 {
    let id = create(t, name);
    let changes: Vec<_> = keys
        .into_iter()
        .map(|k| remap(KEYBOARD_COLLECTION, key(k), &[key(k + 1)]))
        .collect();
    if !changes.is_empty() {
        for changes in changes.chunks(16) {
            set_rules(t, id, changes.to_vec());
        }
    }
    id
}
fn set_layers(t: &mut Test, device: u32, profiles: &[u32]) -> p::Device {
    t.set_device(p::SetDevice {
        device,
        profiles: Some(p::ProfileLayers {
            profiles: profiles.to_vec(),
        }),
        ..Default::default()
    })
}
fn interface(
    interface: p::ConfigurationInterface,
    enabled: Option<bool>,
    profile: Option<u32>,
) -> p::ConfigurationInterfaceUpdate {
    p::ConfigurationInterfaceUpdate {
        interface: interface as i32,
        enabled,
        profile,
    }
}
fn set_interfaces(t: &mut Test, updates: Vec<p::ConfigurationInterfaceUpdate>) -> p::Response {
    t.request(Command::SetAdapter(p::SetAdapter {
        configuration_interfaces: updates,
        ..Default::default()
    }))
}
fn memory_used(t: &mut Test) -> u32 {
    t.status()
        .profile_support
        .unwrap()
        .resources
        .into_iter()
        .find(|r| r.r#type == p::ResourceType::Memory as i32)
        .unwrap()
        .count
        .unwrap()
}
/// Presses exactly `keys` (4..=11) on the test keyboard and returns the keys the adapter's USB
/// keyboard report holds afterwards.
fn press(t: &mut Test, link: cordial_core::link::LinkId, keys: &[u16]) -> Vec<u16> {
    let bits = keys.iter().fold(0u8, |bits, k| bits | 1 << (k - 4));
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[bits]).unwrap(),
    ));
    sent_keys(t)
}
/// The keys of the last keyboard report the adapter sent.
fn sent_keys(t: &mut Test) -> Vec<u16> {
    while let Some(packet) = t.app.manager.forward.packet() {
        if packet.id == cordial_core::forward::REPORT_KEYBOARD {
            t.keys = (0..256u16)
                .filter(|k| packet.bytes()[usize::from(k / 8)] & (1 << (k % 8)) != 0)
                .collect();
        }
        t.app.manager.forward.complete();
    }
    t.keys.clone()
}
fn profile_error(d: &p::Device) -> Option<p::ErrorCode> {
    d.errors
        .iter()
        .filter_map(|e| p::ErrorCode::try_from(e.code).ok())
        .find(|e| {
            matches!(
                e,
                p::ErrorCode::ProfileMemoryExceeded | p::ErrorCode::ProfileLoadFailed
            )
        })
}

#[test]
fn status_reports_profile_support_and_configuration_interfaces() {
    let mut t = Test::new(false);
    let status = t.status();
    let support = status.profile_support.unwrap();
    assert_eq!(support.resources[0].limit, Some(PROFILE_BUDGET));
    assert_eq!(support.resources[0].count, Some(0));
    assert_eq!(support.resources[2].limit, Some(8));
    assert_eq!(support.resources[1].limit, Some(8));
    for kind in [
        p::usage_range::Kind::RemapInput,
        p::usage_range::Kind::ScaleInput,
    ] {
        assert!(support.ranges.iter().any(|r| r.kind == kind as i32));
    }
    let via = p::ConfigurationInterface::Via as i32;
    let vial = p::ConfigurationInterface::Vial as i32;
    assert_eq!(
        status.configuration_interfaces,
        [
            p::ConfigurationInterfaceSupport {
                active: false,
                interface: via,
                enabled: false,
                profile: 0,
                conflicts: vec![vial],
            },
            p::ConfigurationInterfaceSupport {
                active: false,
                interface: vial,
                enabled: false,
                profile: 0,
                conflicts: vec![via],
            },
        ]
    );
}

#[test]
fn a_board_without_profile_support_refuses_profile_work() {
    let mut t = Test::with_budget(true, None);
    let status = t.status();
    assert!(status.profile_support.is_none());
    assert!(status.configuration_interfaces.is_empty());
    for command in [
        Command::ListProfiles(p::ListProfiles {}),
        Command::GetProfile(p::GetProfile { profile: 1 }),
        Command::CreateProfile(p::CreateProfile { name: "A".into() }),
        Command::CopyProfile(p::CopyProfile {
            profile: 1,
            name: "A".into(),
        }),
        Command::DeleteProfile(p::DeleteProfile { profile: 1 }),
        Command::ListProfileRules(p::ListProfileRules { profile: 1 }),
        Command::SetProfileRules(p::SetProfileRules {
            profile: 1,
            changes: vec![forget_rule(KEYBOARD_COLLECTION, key(4))],
        }),
    ] {
        assert_eq!(t.code(command), p::ErrorCode::UnknownCommand);
    }
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED,
            profiles: Some(p::ProfileLayers::default()),
            ..Default::default()
        })),
        p::ErrorCode::Unsupported
    );
    let response = set_interfaces(
        &mut t,
        vec![interface(p::ConfigurationInterface::Via, Some(false), None)],
    );
    assert!(matches!(
        response.result,
        Some(R::Error(e)) if e.code == p::ErrorCode::Unsupported as i32
    ));
    assert!(t.device(SAVED).profiles.is_none());
}

#[test]
fn profiles_are_created_copied_read_and_deleted() {
    let mut t = Test::new(false);
    let first = create(&mut t, "Keys");
    assert_eq!(first, 1);
    let events = t.events();
    assert!(events.contains(&Ev::Profile(p::Profile {
        id: first,
        name: "Keys".into(),
        roles: vec![],
    })));
    for name in ["", "\u{7}bell", &"x".repeat(65)] {
        assert_eq!(
            t.code(Command::CreateProfile(p::CreateProfile {
                name: name.into()
            })),
            p::ErrorCode::BadArgs
        );
    }
    set_rules(
        &mut t,
        first,
        vec![remap(KEYBOARD_COLLECTION, key(4), &[key(5)])],
    );
    assert_eq!(profile(&mut t, first).roles, [p::Role::Keyboard as i32]);
    let copy = match t.ok(Command::CopyProfile(p::CopyProfile {
        profile: first,
        name: "Copy".into(),
    })) {
        Some(R::ProfileCreated(created)) => profile(&mut t, created.profile),
        r => panic!("{r:?}"),
    };
    assert_eq!(copy.name, "Copy");
    assert_eq!(copy.roles, [p::Role::Keyboard as i32]);
    assert_eq!(rules_of(&mut t, copy.id), rules_of(&mut t, first));
    // The copy is independent.
    set_rules(
        &mut t,
        copy.id,
        vec![forget_rule(KEYBOARD_COLLECTION, key(4))],
    );
    assert!(rules_of(&mut t, copy.id).is_empty());
    assert_eq!(
        rules_of(&mut t, first),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert_eq!(profile(&mut t, copy.id).roles, Vec::<i32>::new());
    for command in [
        Command::GetProfile(p::GetProfile { profile: 99 }),
        Command::GetProfile(p::GetProfile { profile: 0 }),
        Command::CopyProfile(p::CopyProfile {
            profile: 99,
            name: "Missing".into(),
        }),
        Command::DeleteProfile(p::DeleteProfile { profile: 99 }),
        Command::ListProfileRules(p::ListProfileRules { profile: 99 }),
    ] {
        assert_eq!(t.code(command), p::ErrorCode::NotFound);
    }
    t.events();
    t.ok(Command::DeleteProfile(p::DeleteProfile {
        profile: copy.id,
    }));
    assert!(
        t.events()
            .contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: copy.id }))
    );
    assert_eq!(
        t.code(Command::GetProfile(p::GetProfile { profile: copy.id })),
        p::ErrorCode::NotFound
    );
    // IDs are never reused, and are separate from device IDs.
    assert_eq!(create(&mut t, "Next"), copy.id + 1);
}

#[test]
fn creating_a_profile_that_does_not_fit_in_flash_is_refused() {
    let mut t = Test::new(false);
    t.store.available = Some(cordial_core::bonds::MAINTENANCE_BYTES);
    let error = t.error(Command::CreateProfile(p::CreateProfile {
        name: "Full".into(),
    }));
    assert_eq!(error.code, p::ErrorCode::NoCapacity as i32);
    assert_eq!(error.reason, p::ErrorReason::Storage as i32);
    t.store.available = None;
    assert_eq!(create(&mut t, "Fits"), 1);
}

#[test]
fn profile_listings_stream_and_report_unreadable_and_lost_records() {
    let mut t = Test::new(false);
    for i in 1..=20 {
        create(&mut t, &format!("Profile {i}"));
    }
    t.events();
    t.store.fail_load = Some(cordial_core::storage::record_key(8, 3));
    t.store.records.insert(
        cordial_core::storage::record_key(8, 5),
        UNDECODABLE.to_vec(),
    );
    let list = |t: &mut Test| match t.ok(Command::ListProfiles(p::ListProfiles {})) {
        Some(R::Profiles(list)) => list,
        r => panic!("{r:?}"),
    };
    use p::profile_list_entry::Entry;
    let ids = |list: &p::ProfileList| -> Vec<(u32, bool)> {
        list.entries
            .iter()
            .map(|e| match e.entry.as_ref().unwrap() {
                Entry::Profile(p) => (p.id, true),
                Entry::Error(error) => {
                    assert_eq!(
                        error.error,
                        Some(p::Error {
                            code: p::ErrorCode::StorageFailed as i32,
                            reason: p::ErrorReason::Storage as i32,
                            outcome: p::ErrorOutcome::NoEffect as i32,
                            related: vec![],
                        })
                    );
                    (error.id, false)
                }
            })
            .collect()
    };
    let first = list(&mut t);
    let mut expected: Vec<(u32, bool)> = vec![(1, true), (2, true), (3, false)];
    expected.extend((4..=20).filter(|id| *id != 5).map(|id| (id, true)));
    assert_eq!(ids(&first), expected);
    assert_eq!(
        t.code(Command::GetProfile(p::GetProfile { profile: 3 })),
        p::ErrorCode::StorageFailed
    );
    // The undecodable record is deleted in the background; the unreadable one stays.
    let events = t.events();
    assert!(events.contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: 5 })));
    assert!(
        !t.store
            .records
            .contains_key(&cordial_core::storage::record_key(8, 5))
    );
    t.store.fail_load = None;
    assert_eq!(profile(&mut t, 3).name, "Profile 3");
}

#[test]
fn set_profile_rules_normalizes_rules_and_forgets_identities() {
    let mut t = Test::new(false);
    let id = create(&mut t, "Rules");
    // Output collections are resolved, and outputs are kept once, in one order.
    let rules = set_rules(
        &mut t,
        id,
        vec![rule(
            KEYBOARD_COLLECTION,
            key(4),
            p::profile_rule::Effect::Remap(p::profile_rule::Remap {
                outputs: vec![
                    p::profile_rule::Output {
                        usage: Some(key(0xe0)),
                        collection: None,
                    },
                    p::profile_rule::Output {
                        usage: Some(key(5)),
                        collection: Some(KEYBOARD_COLLECTION),
                    },
                    p::profile_rule::Output {
                        usage: Some(key(5)),
                        collection: None,
                    },
                ],
            }),
        )],
    );
    assert_eq!(
        rules,
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5, 0xe0])]
    );
    // A remap of an input to only itself forgets its rule.
    assert!(
        set_rules(
            &mut t,
            id,
            vec![remap(KEYBOARD_COLLECTION, key(4), &[key(4)])]
        )
        .is_empty()
    );
    // Ratios are kept in lowest terms; a ratio of 1 forgets the rule; a negative one inverts.
    let x = usage(1, 0x30);
    let wheel = usage(1, 0x38);
    let rules = set_rules(
        &mut t,
        id,
        vec![
            scale(MOUSE_COLLECTION, x, 4, 8),
            scale(MOUSE_COLLECTION, wheel, -2, 2),
            scale(MOUSE_COLLECTION, usage(1, 0x31), 3, 3),
        ],
    );
    let scales: Vec<_> = rules
        .iter()
        .filter_map(|r| match r.effect {
            Some(p::profile_rule::Effect::Scale(s)) => {
                Some((r.input.unwrap(), s.numerator, s.denominator))
            }
            _ => None,
        })
        .collect();
    assert_eq!(scales, [(x, 1, 2), (wheel, -1, 1)]);
    assert_eq!(profile(&mut t, id).roles, [p::Role::Mouse as i32]);
    assert!(set_rules(&mut t, id, vec![scale(MOUSE_COLLECTION, x, 5, 5)]).len() == 1);
}

#[test]
fn rule_changes_apply_in_order_and_invalid_requests_save_nothing() {
    let mut t = Test::new(false);
    let id = create(&mut t, "Rules");
    let c = key(6);
    assert!(
        set_rules(
            &mut t,
            id,
            vec![
                remap(KEYBOARD_COLLECTION, c, &[key(7)]),
                forget_rule(KEYBOARD_COLLECTION, c)
            ]
        )
        .is_empty()
    );
    assert_eq!(
        set_rules(
            &mut t,
            id,
            vec![
                forget_rule(KEYBOARD_COLLECTION, c),
                remap(KEYBOARD_COLLECTION, c, &[key(7)])
            ]
        ),
        [saved_remap(KEYBOARD_COLLECTION, c, &[7])]
    );
    assert_eq!(
        set_rules(
            &mut t,
            id,
            vec![
                remap(KEYBOARD_COLLECTION, c, &[key(8)]),
                remap(KEYBOARD_COLLECTION, c, &[key(9)])
            ]
        ),
        [saved_remap(KEYBOARD_COLLECTION, c, &[9])]
    );
    // Forgetting a rule that does not exist is accepted.
    assert_eq!(
        set_rules(&mut t, id, vec![forget_rule(KEYBOARD_COLLECTION, key(30))]).len(),
        1
    );
    t.events();
    let saved = t.store.records.clone();
    let nine: Vec<_> = (4..13).map(key).collect();
    for changes in [
        vec![],
        vec![p::ProfileRuleChange { change: None }],
        // An input outside the remap inputs, or a key as a scale input.
        vec![remap(KEYBOARD_COLLECTION, key(2), &[key(5)])],
        vec![scale(KEYBOARD_COLLECTION, key(5), 1, 2)],
        // An output outside the remap outputs, or too many outputs.
        vec![remap(KEYBOARD_COLLECTION, key(5), &[key(2)])],
        vec![remap(KEYBOARD_COLLECTION, key(5), &nine)],
        // A zero ratio.
        vec![scale(MOUSE_COLLECTION, usage(1, 0x30), 0, 1)],
        vec![scale(MOUSE_COLLECTION, usage(1, 0x30), 1, 0)],
        // A missing effect or input.
        vec![p::ProfileRuleChange {
            change: Some(p::profile_rule_change::Change::Rule(p::ProfileRule {
                input: Some(key(5)),
                collection: Some(KEYBOARD_COLLECTION),
                ..Default::default()
            })),
        }],
        vec![p::ProfileRuleChange {
            change: Some(p::profile_rule_change::Change::Forget(p::ProfileRuleRef {
                input: None,
                collection: Some(KEYBOARD_COLLECTION),
            })),
        }],
        // One invalid change refuses every change in the request.
        vec![
            remap(KEYBOARD_COLLECTION, key(10), &[key(11)]),
            remap(KEYBOARD_COLLECTION, key(2), &[key(5)]),
        ],
    ] {
        assert_eq!(
            t.code(Command::SetProfileRules(p::SetProfileRules {
                profile: id,
                changes,
            })),
            p::ErrorCode::BadArgs
        );
    }
    assert_eq!(t.store.records, saved);
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, c, &[9])]
    );
    assert!(t.events().is_empty());
    assert_eq!(
        t.code(Command::SetProfileRules(p::SetProfileRules {
            profile: 99,
            changes: vec![forget_rule(KEYBOARD_COLLECTION, c)],
        })),
        p::ErrorCode::NotFound
    );
}

#[test]
fn rule_changes_that_change_the_roles_report_the_profile() {
    let mut t = Test::new(false);
    let id = create(&mut t, "Media");
    t.events();
    set_rules(
        &mut t,
        id,
        vec![remap(CONSUMER_COLLECTION, usage(0x0c, 0xcd), &[key(5)])],
    );
    assert!(t.events().contains(&Ev::Profile(p::Profile {
        id,
        name: "Media".into(),
        roles: vec![p::Role::ConsumerControl as i32],
    })));
    // A change that keeps the roles reports no profile.
    set_rules(
        &mut t,
        id,
        vec![
            remap(CONSUMER_COLLECTION, usage(0x0c, 0xe9), &[key(6)]),
            forget_rule(CONSUMER_COLLECTION, usage(0x0c, 0xcd)),
            forget_rule(CONSUMER_COLLECTION, usage(0x0c, 0xea)),
        ],
    );
    assert!(t.events().iter().all(|e| !matches!(e, Ev::Profile(_))));
}

#[test]
fn system_controls_are_rule_inputs_and_outputs() {
    let mut t = Test::new(false);
    let support = t.status().profile_support.unwrap();
    let power = usage(1, 0x81);
    let system = usage(1, 0x80);
    let collection = |kind: p::usage_range::Kind, u: p::Usage| {
        support
            .ranges
            .iter()
            .find(|r| {
                r.kind == kind as i32
                    && r.usage_page == u.usage_page
                    && (r.min..=r.max).contains(&u.usage)
            })
            .map(|r| r.collection)
    };
    assert_eq!(
        collection(p::usage_range::Kind::RemapInput, power),
        Some(Some(system))
    );
    assert_eq!(
        collection(p::usage_range::Kind::RemapOutput, power),
        Some(Some(system))
    );
    let id = create(&mut t, "Power");
    t.events();
    let rules = set_rules(
        &mut t,
        id,
        vec![
            remap(SYSTEM_COLLECTION, power, &[key(0x45)]),
            remap(KEYBOARD_COLLECTION, key(0x39), &[power]),
        ],
    );
    assert_eq!(rules[0], saved_remap(SYSTEM_COLLECTION, power, &[0x45]));
    assert_eq!(
        rules[1].effect,
        Some(p::profile_rule::Effect::Remap(p::profile_rule::Remap {
            outputs: vec![p::profile_rule::Output {
                usage: Some(power),
                collection: Some(system),
            }],
        }))
    );
    assert_eq!(
        profile(&mut t, id).roles,
        [p::Role::Keyboard as i32, p::Role::SystemControl as i32]
    );
}

#[test]
fn a_profile_larger_than_the_memory_budget_cannot_be_saved() {
    // An empty table takes 80 bytes and each single-output remap 20 more.
    let mut t = Test::with_budget(false, Some(210));
    let id = create(&mut t, "Big");
    assert_eq!(
        set_rules(
            &mut t,
            id,
            (4..10)
                .map(|k| remap(KEYBOARD_COLLECTION, key(k), &[key(k + 1)]))
                .collect()
        )
        .len(),
        6
    );
    let error = t.error(Command::SetProfileRules(p::SetProfileRules {
        profile: id,
        changes: vec![remap(KEYBOARD_COLLECTION, key(20), &[key(21)])],
    }));
    assert_eq!(error.code, p::ErrorCode::NoCapacity as i32);
    assert_eq!(error.reason, p::ErrorReason::ProfileMemory as i32);
    assert_eq!(rules_of(&mut t, id).len(), 6);
}

#[test]
fn layers_are_validated_and_saved_without_loading() {
    let mut t = Test::new(true);
    let a = create(&mut t, "A");
    let d = set_layers(&mut t, SAVED, &[a, a]);
    assert_eq!(
        d.profiles,
        Some(p::ProfileLayers {
            profiles: vec![a, a]
        })
    );
    assert_eq!(t.saved(SAVED).profiles, [u64::from(a), u64::from(a)]);
    assert_eq!(t.entry(SAVED).layers, [u64::from(a), u64::from(a)]);
    for (profiles, code) in [
        (vec![a; 9], p::ErrorCode::BadArgs),
        (vec![a, 0], p::ErrorCode::BadArgs),
        (vec![a, 99], p::ErrorCode::NotFound),
    ] {
        assert_eq!(
            t.code(Command::SetDevice(p::SetDevice {
                device: SAVED,
                profiles: Some(p::ProfileLayers { profiles }),
                ..Default::default()
            })),
            code
        );
    }
    assert_eq!(t.saved(SAVED).profiles, [u64::from(a), u64::from(a)]);
    // An empty list passes everything through.
    assert_eq!(
        set_layers(&mut t, SAVED, &[]).profiles,
        Some(p::ProfileLayers::default())
    );
    assert!(t.saved(SAVED).profiles.is_empty());
    assert_eq!(memory_used(&mut t), 0);
}

#[test]
fn layers_load_when_a_device_connects_and_apply_in_order() {
    let mut t = Test::new(true);
    // A -> B, then B -> C: A produces C, B produces C.
    let first = create(&mut t, "First");
    set_rules(
        &mut t,
        first,
        vec![remap(KEYBOARD_COLLECTION, key(4), &[key(5)])],
    );
    let second = create(&mut t, "Second");
    set_rules(
        &mut t,
        second,
        vec![remap(KEYBOARD_COLLECTION, key(5), &[key(6)])],
    );
    set_layers(&mut t, SAVED, &[first, second]);
    assert_eq!(memory_used(&mut t), 0, "nothing loads while disconnected");
    t.poll();
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    // The profiles load before the first input, without waiting for anything else.
    assert_eq!(press(&mut t, link, &[4]), [6]);
    assert_eq!(press(&mut t, link, &[5]), [6]);
    assert_eq!(press(&mut t, link, &[7]), [7]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    assert!(memory_used(&mut t) > 0);
    let d = t.device(SAVED);
    assert_eq!(profile_error(&d), None);
    // Disconnecting releases them.
    t.ok(Command::DisconnectDevice(p::DisconnectDevice {
        device: SAVED,
    }));
    t.event(Event::Disconnected { link, error: None });
    assert_eq!(memory_used(&mut t), 0);
}

#[test]
fn a_device_whose_layers_do_not_fit_loads_none_of_them() {
    // 240 bytes for eight rules and 180 for five: either fits, both do not.
    let mut t = Test::with_budget(true, Some(300));
    let big = profile_with(&mut t, "Big", 4..12);
    let small = profile_with(&mut t, "Small", [4, 6, 8, 10, 20]);
    set_layers(&mut t, SAVED, &[big, small]);
    let link = connect_saved(&mut t, descriptor());
    // Input passes through unchanged and the device says why.
    assert_eq!(press(&mut t, link, &[4]), [4]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    assert_eq!(memory_used(&mut t), 0);
    assert_eq!(
        profile_error(&t.device(SAVED)),
        Some(p::ErrorCode::ProfileMemoryExceeded)
    );
    let events = t.events();
    assert!(events.iter().any(|e| matches!(
        e,
        Ev::Device(d) if d.errors.iter().any(|e| e.code == p::ErrorCode::ProfileMemoryExceeded as i32)
    )));
    // Changing the layers loads the new ones at once.
    let d = set_layers(&mut t, SAVED, &[small]);
    assert_eq!(profile_error(&d), None);
    assert_eq!(press(&mut t, link, &[4]), [5]);
    assert_eq!(memory_used(&mut t), 180);
    // The device's old layers count as released before its new ones are checked.
    let d = set_layers(&mut t, SAVED, &[big]);
    assert_eq!(profile_error(&d), None);
    assert_eq!(memory_used(&mut t), 240);
}

#[test]
fn a_device_retries_its_layers_once_memory_is_released() {
    let mut t = Test::with_budget(true, Some(300));
    let big = profile_with(&mut t, "Big", 4..12);
    let small = profile_with(&mut t, "Small", [4, 6, 8, 10, 20]);
    let unused = create(&mut t, "Unused");
    set_layers(&mut t, SAVED, &[big]);
    let mouse = add_ble(&mut t);
    set_layers(&mut t, 78, &[small]);
    let keyboard = connect_saved(&mut t, descriptor());
    t.events();
    t.event(Event::Incoming {
        attempt: 1,
        peer: mouse,
    });
    let link = t.radio.connects.last().unwrap().0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    t.events();
    assert_eq!(
        profile_error(&t.device(78)),
        Some(p::ErrorCode::ProfileMemoryExceeded)
    );
    assert_eq!(press(&mut t, link, &[4]), [4]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    // Other background work is waiting when the memory is released.
    t.store.records.insert(
        cordial_core::storage::record_key(8, unused.into()),
        UNDECODABLE.to_vec(),
    );
    assert_eq!(
        t.code(Command::GetProfile(p::GetProfile { profile: unused })),
        p::ErrorCode::NotFound
    );
    t.ok(Command::DisconnectDevice(p::DisconnectDevice {
        device: SAVED,
    }));
    t.event(Event::Disconnected {
        link: keyboard,
        error: None,
    });
    t.events();
    assert_eq!(profile_error(&t.device(78)), None);
    assert_eq!(press(&mut t, link, &[4]), [5]);
    assert_eq!(memory_used(&mut t), 180);
}

#[test]
fn a_profile_that_cannot_be_read_is_retried_with_backoff() {
    let mut t = Test::new(true);
    let id = profile_with(&mut t, "Keys", [4]);
    set_layers(&mut t, SAVED, &[id]);
    let rules = cordial_core::storage::record_key(9, id.into());
    t.store.fail_load = Some(rules);
    let link = connect_saved(&mut t, descriptor());
    assert_eq!(
        profile_error(&t.device(SAVED)),
        Some(p::ErrorCode::ProfileLoadFailed)
    );
    assert_eq!(press(&mut t, link, &[4]), [4]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    t.events();
    t.store.fail_load = None;
    // The read failure is retried after the first connection backoff, never deleting anything.
    t.events();
    assert_eq!(
        profile_error(&t.device(SAVED)),
        Some(p::ErrorCode::ProfileLoadFailed)
    );
    assert!(t.store.records.contains_key(&rules));
    t.now += FIRST_RETRY;
    let events = t.events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ev::Device(d) if profile_error(d).is_none()))
    );
    assert_eq!(press(&mut t, link, &[4]), [5]);
}

#[test]
fn edits_apply_from_the_next_input_and_held_inputs_keep_their_outputs() {
    let mut t = Test::new(true);
    let id = profile_with(&mut t, "Keys", [4]);
    set_layers(&mut t, SAVED, &[id]);
    let link = connect_saved(&mut t, descriptor());
    t.events();
    assert_eq!(press(&mut t, link, &[4]), [5]);
    set_rules(
        &mut t,
        id,
        vec![remap(KEYBOARD_COLLECTION, key(4), &[key(6)])],
    );
    assert_eq!(
        press(&mut t, link, &[4]),
        [5],
        "a held input keeps its outputs"
    );
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    assert_eq!(press(&mut t, link, &[4]), [6]);
    // Layer changes behave the same way.
    set_layers(&mut t, SAVED, &[]);
    assert_eq!(press(&mut t, link, &[4]), [6]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    assert_eq!(press(&mut t, link, &[4]), [4]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    // An output several held inputs produce stays held until the last is released.
    set_rules(
        &mut t,
        id,
        vec![
            remap(KEYBOARD_COLLECTION, key(4), &[key(6)]),
            remap(KEYBOARD_COLLECTION, key(5), &[key(6)]),
        ],
    );
    set_layers(&mut t, SAVED, &[id]);
    assert_eq!(press(&mut t, link, &[4, 5]), [6]);
    assert_eq!(press(&mut t, link, &[5]), [6]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
}

#[test]
fn an_edit_that_would_overflow_loaded_profiles_is_refused() {
    let mut t = Test::with_budget(true, Some(330));
    // 240 + 80 bytes loaded.
    let big = profile_with(&mut t, "Big", 4..12);
    let empty = create(&mut t, "Empty");
    set_layers(&mut t, SAVED, &[big, empty]);
    connect_saved(&mut t, descriptor());
    assert_eq!(memory_used(&mut t), 320);
    let error = t.error(Command::SetProfileRules(p::SetProfileRules {
        profile: empty,
        changes: vec![remap(KEYBOARD_COLLECTION, key(20), &[key(21)])],
    }));
    assert_eq!(error.code, p::ErrorCode::NoCapacity as i32);
    assert_eq!(error.reason, p::ErrorReason::ProfileMemory as i32);
    assert!(rules_of(&mut t, empty).is_empty());
    assert_eq!(profile_error(&t.device(SAVED)), None);
    // Shrinking a loaded profile, and growing one that is not loaded, are fine.
    set_rules(&mut t, big, vec![forget_rule(KEYBOARD_COLLECTION, key(4))]);
    set_rules(
        &mut t,
        empty,
        vec![remap(KEYBOARD_COLLECTION, key(20), &[key(21)])],
    );
    let unused = profile_with(&mut t, "Unused", 4..15);
    assert_eq!(rules_of(&mut t, unused).len(), 11);
}

#[test]
fn deleting_a_profile_in_use_is_refused() {
    let mut t = Test::new(true);
    let id = create(&mut t, "Used");
    let other = t.add_saved(2);
    set_layers(&mut t, other, &[id]);
    // A disabled device's layers still use it.
    t.ok(Command::SetDevice(p::SetDevice {
        device: other,
        enabled: Some(false),
        ..Default::default()
    }));
    let delete = |t: &mut Test| t.request(Command::DeleteProfile(p::DeleteProfile { profile: id }));
    let in_use = |r: p::Response| matches!(r.result, Some(R::Error(e)) if e.code == p::ErrorCode::InUse as i32);
    assert!(in_use(delete(&mut t)));
    set_layers(&mut t, other, &[]);
    // So does a disabled configuration interface.
    set_interfaces(
        &mut t,
        vec![interface(p::ConfigurationInterface::Vial, None, Some(id))],
    );
    assert!(in_use(delete(&mut t)));
    set_interfaces(
        &mut t,
        vec![interface(p::ConfigurationInterface::Vial, None, Some(0))],
    );
    assert!(delete(&mut t).result.is_none());
}

#[test]
fn configuration_interfaces_need_a_profile_and_refuse_conflicts() {
    let mut t = Test::new(false);
    let (via, vial) = (
        p::ConfigurationInterface::Via,
        p::ConfigurationInterface::Vial,
    );
    let first = create(&mut t, "First");
    let second = create(&mut t, "Second");
    let code = |r: p::Response| match r.result {
        Some(R::Error(e)) => Some(p::ErrorCode::try_from(e.code).unwrap()),
        _ => None,
    };
    let state = |t: &mut Test| -> Vec<(bool, u32)> {
        t.status()
            .configuration_interfaces
            .iter()
            .map(|i| (i.enabled, i.profile))
            .collect()
    };
    t.events();
    assert_eq!(
        code(set_interfaces(
            &mut t,
            vec![interface(via, Some(true), None)]
        )),
        Some(p::ErrorCode::BadArgs)
    );
    assert_eq!(
        code(set_interfaces(&mut t, vec![interface(via, None, Some(99))])),
        Some(p::ErrorCode::NotFound)
    );
    for bad in [0, 42] {
        let update = p::ConfigurationInterfaceUpdate {
            interface: bad,
            enabled: Some(false),
            profile: None,
        };
        assert!(code(set_interfaces(&mut t, vec![update])).is_some());
    }
    assert_eq!(state(&mut t), [(false, 0), (false, 0)]);
    // Selecting a profile for a disabled interface needs no USB reconnect.
    assert_eq!(
        code(set_interfaces(
            &mut t,
            vec![interface(via, None, Some(first))]
        )),
        None
    );
    assert!(t.events().iter().any(|e| matches!(e, Ev::Adapter(_))));
    assert_eq!(
        code(set_interfaces(
            &mut t,
            vec![interface(via, Some(true), None)]
        )),
        None
    );
    assert_eq!(state(&mut t), [(true, first), (false, 0)]);
    // A configuration that enables conflicting interfaces changes nothing.
    assert_eq!(
        code(set_interfaces(
            &mut t,
            vec![interface(vial, Some(true), Some(second))]
        )),
        Some(p::ErrorCode::Unsupported)
    );
    assert_eq!(state(&mut t), [(true, first), (false, 0)]);
    // Updates apply in order, and only the result has to be valid.
    assert_eq!(
        code(set_interfaces(
            &mut t,
            vec![
                interface(vial, Some(true), Some(second)),
                interface(via, Some(false), None),
            ]
        )),
        None
    );
    assert_eq!(state(&mut t), [(false, first), (true, second)]);
    // Profile selection preserves the enabled interface set.
    assert_eq!(
        code(set_interfaces(
            &mut t,
            vec![interface(vial, None, Some(first))]
        )),
        None
    );
    assert_eq!(
        code(set_interfaces(
            &mut t,
            vec![interface(via, None, Some(second))]
        )),
        None
    );
    // An enabled interface keeps needing its profile; a disabled one can clear it.
    assert_eq!(
        code(set_interfaces(&mut t, vec![interface(vial, None, Some(0))])),
        Some(p::ErrorCode::BadArgs)
    );
    assert_eq!(
        code(set_interfaces(&mut t, vec![interface(via, None, Some(0))])),
        None
    );
    assert_eq!(state(&mut t), [(false, 0), (true, first)]);
    // The preferences are saved and survive a restart.
    let mut manager = cordial_core::manager::Manager::default();
    block_on(manager.load(&mut t.store, &mut t.radio)).unwrap();
    assert_eq!(manager.preference, t.app.manager.preference);
}

fn configure(t: &mut Test, interface: Interface, prefix: &[u8]) -> [u8; 32] {
    let mut packet = [0; 32];
    packet[..prefix.len()].copy_from_slice(prefix);
    t.now += 1;
    block_on(t.app.configure(interface, packet, &mut t.store, t.now))
}

#[test]
fn via_edits_the_interface_profile_for_every_device_using_it() {
    let mut t = Test::new(true);
    let id = create(&mut t, "Keys");
    set_layers(&mut t, SAVED, &[id]);
    let link = connect_saved(&mut t, descriptor());
    t.events();
    // Nothing answers while the interface is disabled.
    assert_eq!(configure(&mut t, Interface::Via, &[1])[0], 0xff);
    set_interfaces(
        &mut t,
        vec![interface(
            p::ConfigurationInterface::Via,
            Some(true),
            Some(id),
        )],
    );
    t.events();
    assert_eq!(&configure(&mut t, Interface::Via, &[1])[..3], &[1, 0, 9]);
    assert_eq!(configure(&mut t, Interface::Vial, &[1])[0], 0xff);
    assert_eq!(configure(&mut t, Interface::Via, &[0xfe, 0])[0], 0xff);
    // An input without a rule reads as its own usage.
    assert_eq!(
        &configure(&mut t, Interface::Via, &[4, 0, 0, 0])[4..6],
        &[0, 4]
    );
    assert_eq!(configure(&mut t, Interface::Via, &[5, 0, 0, 0, 0, 5])[0], 5);
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert_eq!(press(&mut t, link, &[4]), [5]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    // An unsupported action changes nothing.
    assert_eq!(
        configure(&mut t, Interface::Via, &[5, 0, 0, 0, 0x52, 0])[0],
        0xff
    );
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    // The edit reaches flash once the editor and input pause.
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.quiet();
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    // A save that fails after the echo keeps the edit in use and is retried.
    let rules = cordial_core::storage::record_key(9, id.into());
    t.store.fail_save = Some((rules, false));
    assert_eq!(configure(&mut t, Interface::Via, &[5, 0, 0, 0, 0, 6])[0], 5);
    assert_eq!(press(&mut t, link, &[4]), [6]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.quiet();
    assert!(t.status().ready);
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert_eq!(press(&mut t, link, &[4]), [6]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    t.store.fail_save = None;
    t.now += FIRST_RETRY;
    t.quiet();
    assert!(t.status().ready);
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[6])]
    );
    // Writing an input's own usage forgets its rule.
    assert_eq!(configure(&mut t, Interface::Via, &[5, 0, 0, 0, 0, 4])[0], 5);
    assert!(rules_of(&mut t, id).is_empty());
    assert_eq!(press(&mut t, link, &[4]), [4]);
}

#[test]
fn editors_expose_controls_and_mouse_buttons_and_reset_the_profile() {
    let mut t = Test::new(false);
    let id = profile_with(&mut t, "Keys", [4, 5]);
    let via = p::ConfigurationInterface::Via;
    set_interfaces(&mut t, vec![interface(via, Some(true), Some(id))]);
    let read = |t: &mut Test, interface, row: u8, col: u8| {
        let reply = configure(t, interface, &[4, 0, row, col]);
        assert_eq!(reply[0], 4);
        u16::from_be_bytes([reply[4], reply[5]])
    };
    // System Power, Media Select, mouse button 1 and button 6, which VIA's table cannot name.
    assert_eq!(read(&mut t, Interface::Via, 11, 0), 0xa5);
    assert_eq!(read(&mut t, Interface::Via, 11, 3), 0xaf);
    assert_eq!(read(&mut t, Interface::Via, 12, 3), 0xf4);
    assert_eq!(read(&mut t, Interface::Via, 12, 8), 0x01);
    // Vial's mouse button keycodes are not buttons in VIA's table.
    assert_eq!(
        configure(&mut t, Interface::Via, &[5, 0, 12, 8, 0, 0xd6])[0],
        0xff
    );
    assert_eq!(
        configure(&mut t, Interface::Via, &[5, 0, 12, 8, 0, 0xa9])[0],
        5
    );
    assert_eq!(read(&mut t, Interface::Via, 12, 8), 0xa9);
    assert_eq!(rules_of(&mut t, id).len(), 3);
    // Transparent forgets the input's rule.
    assert_eq!(configure(&mut t, Interface::Via, &[5, 0, 0, 0, 0, 1])[0], 5);
    assert_eq!(rules_of(&mut t, id).len(), 2);
    // An unused position reads as disabled and ignores writes.
    assert_eq!(read(&mut t, Interface::Via, 13, 3), 0);
    assert_eq!(
        configure(&mut t, Interface::Via, &[5, 0, 13, 3, 0, 4])[0],
        5
    );
    assert_eq!(rules_of(&mut t, id).len(), 2);
    // The whole keymap reads in buffer chunks.
    for start in (0..14 * 16 * 2).step_by(28) {
        let count = (14 * 16 * 2 - start).min(28) as u8;
        let [high, low] = (start as u16).to_be_bytes();
        assert_eq!(
            configure(&mut t, Interface::Via, &[0x12, high, low, count])[0],
            0x12
        );
    }
    // Vial names button 6, so writing it there forgets its rule.
    set_interfaces(
        &mut t,
        vec![
            interface(via, Some(false), None),
            interface(p::ConfigurationInterface::Vial, Some(true), Some(id)),
        ],
    );
    assert_eq!(
        configure(&mut t, Interface::Vial, &[5, 0, 12, 8, 0, 0xd6])[0],
        5
    );
    assert_eq!(rules_of(&mut t, id).len(), 1);
    assert_eq!(read(&mut t, Interface::Vial, 12, 8), 0xd6);
    assert_eq!(read(&mut t, Interface::Vial, 12, 3), 0xd1);
    assert_eq!(read(&mut t, Interface::Vial, 13, 2), 0x01);
    // Resetting the keymap forgets every rule.
    assert_eq!(&configure(&mut t, Interface::Vial, &[6])[..2], &[6, 0]);
    assert!(rules_of(&mut t, id).is_empty());
    assert_eq!(read(&mut t, Interface::Vial, 0, 0), 4);
}

#[test]
fn an_idle_editor_releases_its_profile() {
    let mut t = Test::new(false);
    let id = create(&mut t, "Keys");
    set_interfaces(
        &mut t,
        vec![interface(
            p::ConfigurationInterface::Vial,
            Some(true),
            Some(id),
        )],
    );
    assert_eq!(memory_used(&mut t), 0);
    assert_eq!(
        &configure(&mut t, Interface::Vial, &[0xfe, 0])[..4],
        &[6, 0, 0, 0]
    );
    assert_eq!(memory_used(&mut t), 80);
    t.now += 5000;
    t.poll();
    assert_eq!(memory_used(&mut t), 0);
}

#[test]
fn a_lost_profile_is_removed_with_its_references() {
    let mut t = Test::new(true);
    let lost = create(&mut t, "Lost");
    let kept = create(&mut t, "Kept");
    set_layers(&mut t, SAVED, &[lost, kept]);
    let other = t.add_saved(2);
    set_layers(&mut t, other, &[lost]);
    set_interfaces(
        &mut t,
        vec![interface(
            p::ConfigurationInterface::Via,
            Some(true),
            Some(lost),
        )],
    );
    t.events();
    t.store.records.insert(
        cordial_core::storage::record_key(8, lost.into()),
        b"[]".to_vec(),
    );
    assert_eq!(
        t.code(Command::GetProfile(p::GetProfile { profile: lost })),
        p::ErrorCode::NotFound
    );
    let events = t.events();
    assert!(events.contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: lost })));
    assert!(events.iter().any(|e| matches!(e, Ev::Adapter(_))));
    assert_eq!(t.saved(SAVED).profiles, [u64::from(kept)]);
    assert!(t.saved(other).profiles.is_empty());
    assert_eq!(t.entry(SAVED).layers, [u64::from(kept)]);
    // Disabling the interface that used it reconnects USB.
    assert_eq!(
        t.status().configuration_interfaces[0],
        p::ConfigurationInterfaceSupport {
            active: false,
            interface: p::ConfigurationInterface::Via as i32,
            enabled: false,
            profile: 0,
            conflicts: vec![p::ConfigurationInterface::Vial as i32],
        }
    );
    assert!(
        !t.store
            .records
            .contains_key(&cordial_core::storage::record_key(8, lost.into()))
    );
}

#[test]
fn an_undecodable_rules_file_leaves_the_profile_empty_after_the_first_input() {
    let mut t = Test::new(true);
    let id = profile_with(&mut t, "Keys", [4]);
    set_layers(&mut t, SAVED, &[id]);
    t.events();
    let rules = cordial_core::storage::record_key(9, id.into());
    t.store.records.insert(rules, UNDECODABLE.to_vec());
    let link = t.radio.connects[0].0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    assert_eq!(
        profile_error(&t.device(SAVED)),
        Some(p::ErrorCode::ProfileLoadFailed)
    );
    assert_eq!(press(&mut t, link, &[4]), [4]);
    // No cleanup runs between the connection starting and its policy being read, nor until
    // input pauses.
    assert!(t.store.records.contains_key(&rules));
    t.poll();
    assert!(t.store.records.contains_key(&rules));
    t.now += cordial_core::deferred::QUIET_MS;
    t.events();
    assert!(!t.store.records.contains_key(&rules));
    assert_eq!(rules_of(&mut t, id), []);
    assert_eq!(profile(&mut t, id).roles, Vec::<i32>::new());
    assert_eq!(t.saved(SAVED).profiles, [u64::from(id)]);
    // The device loads the now empty profile after its backoff.
    t.now += FIRST_RETRY;
    t.events();
    assert_eq!(profile_error(&t.device(SAVED)), None);
}

// ---------------------------------------------------------------------------
// Background storage work
// ---------------------------------------------------------------------------

fn record_key(kind: u8, id: u32) -> cordial_core::storage::RecordKey {
    cordial_core::storage::record_key(kind, id.into())
}
fn reads_of(t: &Test, key: cordial_core::storage::RecordKey) -> usize {
    t.store.reads.iter().filter(|k| **k == key).count()
}

#[test]
fn commands_read_each_record_once() {
    let mut t = Test::new(true);
    let id = create(&mut t, "Keys");
    t.store.reads.clear();
    assert_eq!(
        t.ok(Command::SetProfileRules(p::SetProfileRules {
            profile: id,
            changes: vec![remap(KEYBOARD_COLLECTION, key(4), &[key(5)])],
        })),
        None
    );
    assert_eq!(reads_of(&t, record_key(8, id)), 1);
    assert_eq!(rules_of(&mut t, id).len(), 1);

    t.store.reads.clear();
    let updates = (0..40)
        .map(|n| {
            let via = p::ConfigurationInterface::Via;
            interface(via, Some(n % 2 == 0), Some(id))
        })
        .collect();
    assert_eq!(set_interfaces(&mut t, updates).result, None);
    assert_eq!(reads_of(&t, record_key(8, id)), 1);

    t.store.reads.clear();
    let writes = t.store.writes.len();
    assert_eq!(
        t.ok(Command::SetDevice(p::SetDevice {
            device: SAVED,
            trusted: Some(false),
            ..Default::default()
        })),
        None
    );
    assert_eq!(reads_of(&t, record_key(2, SAVED)), 1);
    assert_eq!(t.store.writes[writes..], [record_key(2, SAVED)]);
    assert!(!t.device(SAVED).trusted);
}

#[test]
fn nothing_but_profile_rules_is_read_or_written_before_the_first_input() {
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
    let id = profile_with(&mut t, "Keys", [4]);
    set_layers(&mut t, SAVED, &[id]);
    t.events();
    // Device events read records too; this checks background work alone.
    t.app.session(false, &mut t.radio);
    t.poll();
    let link = t.radio.connects[0].0;
    let layout = classic_layout(KEYBOARD_MAP);
    t.store.reads.clear();
    t.store.writes.clear();
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: Some(layout.clone()),
    });
    // Only the rules of the profile in its layers are read on the connect path.
    assert_eq!(t.store.reads, [record_key(9, id)]);
    for _ in 0..5 {
        t.poll();
    }
    assert_eq!(t.store.reads, [record_key(9, id)]);
    assert!(t.store.writes.is_empty());
    assert!(t.live(SAVED).policy.is_none());
    assert_eq!(press(&mut t, link, &[4]), [5]);
    t.poll();
    assert!(t.live(SAVED).policy.is_some());
    assert!(reads_of(&t, record_key(2, SAVED)) > 0);
    assert_eq!(reads_of(&t, record_key(4, SAVED)), 1);
    assert_eq!(t.live(SAVED).catalog.preferences().count(), 1);
    t.quiet();
    assert_eq!(saved_layout(&t), Some(layout));
}

#[test]
fn a_connection_without_input_reads_its_policy_after_a_wait() {
    let mut t = Test::new(true);
    t.app.session(false, &mut t.radio);
    let link = connect_saved(&mut t, descriptor());
    let connected = t.now;
    t.now = connected + cordial_core::manager::FIRST_INPUT_WAIT_MS - 2;
    t.poll();
    assert!(t.live(SAVED).policy.is_none());
    t.poll();
    assert!(t.live(SAVED).policy.is_some());
    assert!(t.app.manager.connection(link).is_some());
}

#[test]
fn a_disconnect_syncs_bonds_once_a_starting_connection_forwards_input() {
    let mut t = Test::new(true);
    let ble = add_ble(&mut t);
    t.app.session(false, &mut t.radio);
    let classic = connect_saved(&mut t, descriptor());
    press(&mut t, classic, &[4]);
    t.poll();
    t.event(Event::Incoming {
        attempt: 1,
        peer: ble,
    });
    let link = t.radio.connects.last().unwrap().0;
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    t.store.reads.clear();
    t.event(Event::Disconnected {
        link: classic,
        error: None,
    });
    for _ in 0..5 {
        t.poll();
    }
    assert!(t.store.reads.is_empty(), "{:?}", t.store.reads);
    press(&mut t, link, &[4]);
    for _ in 0..3 {
        t.poll();
    }
    assert!(
        reads_of(&t, record_key(2, SAVED)) > 0,
        "the bonds are synced"
    );
    assert!(!t.app.manager.bonds_pending);
}

#[test]
fn a_failed_policy_read_backs_off_without_holding_up_other_work() {
    let mut t = Test::new(true);
    t.app.session(false, &mut t.radio);
    t.poll();
    let link = t.radio.connects[0].0;
    let layout = classic_layout(KEYBOARD_MAP);
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: Some(layout.clone()),
    });
    t.store.fail_load = Some(record_key(2, SAVED));
    t.store.reads.clear();
    press(&mut t, link, &[4]);
    t.poll();
    let reads = reads_of(&t, record_key(2, SAVED));
    assert_eq!(reads, 1);
    for _ in 0..20 {
        t.poll();
    }
    assert_eq!(reads_of(&t, record_key(2, SAVED)), reads);
    t.quiet();
    assert_eq!(reads_of(&t, record_key(2, SAVED)), reads);
    assert_eq!(
        saved_layout(&t),
        Some(layout),
        "the layout is saved meanwhile"
    );
    assert!(t.live(SAVED).policy.is_none());
    t.store.fail_load = None;
    t.now += FIRST_RETRY;
    t.poll();
    assert!(t.live(SAVED).policy.is_some());
}

#[test]
fn a_lost_record_found_by_the_policy_read_removes_only_that_device() {
    let mut t = Test::new(true);
    let ble = add_ble(&mut t);
    let classic = connect_saved(&mut t, descriptor());
    t.event(Event::Incoming {
        attempt: 1,
        peer: ble,
    });
    let link = t.radio.connects.last().unwrap().0;
    assert_ne!(link, classic);
    t.event(Event::Connected {
        link,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    // The first connection closes before its policy is read; the second device's record is lost.
    t.ok(Command::DisconnectDevice(p::DisconnectDevice {
        device: SAVED,
    }));
    t.store.records.remove(&record_key(2, 78));
    t.settle();
    let events = t.events();
    assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: 78 })));
    assert!(!events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED })));
    assert!(t.store.records.contains_key(&record_key(2, SAVED)));
    assert!(t.app.manager.find(SAVED.into()).is_some());
    assert!(t.app.manager.find(78).is_none());
}

#[test]
fn a_change_saved_before_the_policy_is_read_keeps_the_saved_settings() {
    let mut t = Test::new(true);
    for preference in [
        preference(SettingKey::BacklightEnabled, FeatureId::BACKLIGHT, 1),
        preference(SettingKey::WheelInvert, FeatureId::HIRES_WHEEL, 1),
    ] {
        block_on(
            Preferences {
                store: &mut t.store,
                device: 77,
            }
            .save(&preference),
        )
        .unwrap();
    }
    let link = connect_saved(&mut t, descriptor());
    t.ok(Command::SetDevice(p::SetDevice {
        device: SAVED,
        trusted: Some(false),
        ..Default::default()
    }));
    assert!(!t.saved(SAVED).trusted);
    assert!(t.live(SAVED).policy.is_none());
    press(&mut t, link, &[4]);
    t.events();
    assert!(!t.live(SAVED).policy.as_ref().unwrap().trusted);
    assert_eq!(t.live(SAVED).catalog.preferences().count(), 2);
    t.ok(Command::SetSettings(p::SetSettings {
        device: SAVED,
        changes: vec![backlight_value(false)],
    }));
    let saved = block_on(
        Preferences {
            store: &mut t.store,
            device: 77,
        }
        .load_all(),
    )
    .unwrap();
    let saved: Vec<_> = saved.iter().map(|p| (p.metadata.key, p.value)).collect();
    assert_eq!(saved.len(), 2);
    assert!(saved.contains(&(SettingKey::BacklightEnabled, 0)));
    assert!(saved.contains(&(SettingKey::WheelInvert, 1)));
}

#[test]
fn a_full_two_byte_editor_write_replaces_a_rule_the_editor_cannot_show() {
    let mut t = Test::new(false);
    let id = create(&mut t, "Keys");
    // Mouse button 9 has no VIA keycode.
    set_rules(
        &mut t,
        id,
        vec![remap(KEYBOARD_COLLECTION, key(4), &[usage(9, 9)])],
    );
    set_interfaces(
        &mut t,
        vec![interface(
            p::ConfigurationInterface::Via,
            Some(true),
            Some(id),
        )],
    );
    // Reading it, or changing one byte of it, fails.
    assert_eq!(configure(&mut t, Interface::Via, &[0x12, 0, 0, 2])[0], 0xff);
    assert_eq!(
        configure(&mut t, Interface::Via, &[0x13, 0, 1, 1, 5])[0],
        0xff
    );
    assert_eq!(
        configure(&mut t, Interface::Via, &[0x13, 0, 0, 2, 0, 5])[0],
        0x13
    );
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

#[test]
fn editors_change_rules_while_a_pairing_runs() {
    let mut t = Test::new(false);
    let id = create(&mut t, "Keys");
    set_interfaces(
        &mut t,
        vec![interface(
            p::ConfigurationInterface::Via,
            Some(true),
            Some(id),
        )],
    );
    let candidate = t.candidate(peer(3), peer(3));
    t.pair(candidate);
    assert_eq!(configure(&mut t, Interface::Via, &[5, 0, 0, 0, 0, 5])[0], 5);
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

#[test]
fn rules_saved_without_their_roles_summary_take_effect_and_the_summary_is_repaired() {
    let mut t = Test::new(true);
    let id = create(&mut t, "Keys");
    set_layers(&mut t, SAVED, &[id]);
    let link = connect_saved(&mut t, descriptor());
    t.events();
    let record = record_key(8, id);
    t.store.fail_save = Some((record, false));
    t.store.writes.clear();
    set_rules(
        &mut t,
        id,
        vec![remap(KEYBOARD_COLLECTION, key(4), &[key(5)])],
    );
    assert_eq!(press(&mut t, link, &[4]), [5]);
    assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    // The profile reports its rules' roles while the summary waits to be saved.
    assert_eq!(profile(&mut t, id).roles, [p::Role::Keyboard as i32]);
    assert!(
        block_on(cordial_core::profiles::metadata(&mut t.store, id.into()))
            .unwrap()
            .roles
            .0
            == 0
    );
    // The repair waits for input to pause, fails too and backs off.
    for _ in 0..5 {
        t.poll();
    }
    assert_eq!(t.store.writes.iter().filter(|k| **k == record).count(), 1);
    t.quiet();
    let writes = t.store.writes.iter().filter(|k| **k == record).count();
    assert_eq!(writes, 2);
    // A failed roles summary is not reported as a storage failure.
    assert!(t.status().ready);
    for _ in 0..10 {
        t.poll();
    }
    assert_eq!(
        t.store.writes.iter().filter(|k| **k == record).count(),
        writes
    );
    t.store.fail_save = None;
    t.now += FIRST_RETRY;
    let events = t.events();
    assert!(events.iter().any(
        |e| matches!(e, Ev::Profile(p) if p.id == id && p.roles == [p::Role::Keyboard as i32])
    ));
    assert_eq!(profile(&mut t, id).roles, [p::Role::Keyboard as i32]);
}

/// Makes profile `id`'s record undecodable and has a command find it lost.
fn lose_profile(t: &mut Test, id: u32) {
    t.store.records.insert(record_key(8, id), b"[]".to_vec());
    assert_eq!(
        t.code(Command::GetProfile(p::GetProfile { profile: id })),
        p::ErrorCode::NotFound
    );
}

#[test]
fn a_lost_profile_is_removed_from_an_enabled_device_without_room_in_the_stack() {
    let mut t = Test::new(true);
    let lost = create(&mut t, "Lost");
    // Enabled devices beyond the native allowance remain nonresident.
    for n in 2..=8 {
        t.add_saved(n);
    }
    let outside = 84;
    assert!(t.app.manager.find(outside.into()).is_none());
    assert!(t.saved(outside).enabled);
    t.write_policy(outside, |p| p.profiles = vec![lost.into()]);
    t.events();
    lose_profile(&mut t, lost);
    let events = t.events();
    assert!(events.contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: lost })));
    assert!(t.saved(outside).profiles.is_empty());
    assert!(!t.store.records.contains_key(&record_key(8, lost)));
}

#[test]
fn a_failed_lost_profile_cleanup_is_retried_after_a_backoff() {
    let mut t = Test::new(true);
    let lost = create(&mut t, "Lost");
    set_layers(&mut t, SAVED, &[lost]);
    t.events();
    t.store.fail_save = Some((record_key(2, SAVED), false));
    lose_profile(&mut t, lost);
    t.events();
    assert!(t.store.records.contains_key(&record_key(8, lost)));
    assert_eq!(t.app.manager.lost_profiles, [u64::from(lost)]);
    t.store.fail_save = None;
    t.events();
    assert!(
        t.store.records.contains_key(&record_key(8, lost)),
        "backing off"
    );
    t.now += FIRST_RETRY;
    let events = t.events();
    assert!(events.contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: lost })));
    assert!(t.saved(SAVED).profiles.is_empty());
    assert!(t.app.manager.lost_profiles.is_empty());
}

#[test]
fn a_failed_background_fill_is_retried_after_a_backoff() {
    let mut t = Test::new(true);
    t.app.session(false, &mut t.radio);
    let mut policy = Policy::paired(78, peer(2), b"Keyboard");
    policy.setup_pending = false;
    block_on(cordial_core::bonds::commit(
        &mut t.store,
        &policy,
        &bond(78, peer(2)),
    ))
    .unwrap();
    t.app.manager.vacated = true;
    t.store.fail = true;
    t.poll();
    t.store.fail = false;
    assert!(t.app.manager.vacated);
    t.poll();
    assert!(t.app.manager.find(78).is_none(), "backing off");
    t.now += FIRST_RETRY;
    t.poll();
    assert!(t.app.manager.find(78).is_some());
    assert!(t.radio.bonds.contains(&peer(2)));
}

#[test]
fn startup_skips_references_to_deleted_profiles_it_cannot_remove() {
    for failing in [
        record_key(2, SAVED),
        cordial_core::storage::record_key(1, 0),
    ] {
        let mut t = Test::new(true);
        let id = create(&mut t, "Gone");
        set_layers(&mut t, SAVED, &[id]);
        set_interfaces(
            &mut t,
            vec![interface(
                p::ConfigurationInterface::Via,
                Some(true),
                Some(id),
            )],
        );
        t.store.records.remove(&record_key(8, id));
        t.store.fail_save = Some((failing, false));
        let mut manager = cordial_core::manager::Manager::default();
        block_on(manager.load(&mut t.store, &mut t.radio)).unwrap();
        assert!(manager.storage_ready);
        let slot = manager.find(SAVED.into()).unwrap();
        assert!(manager.devices[slot].as_ref().unwrap().layers.is_empty());
        assert!(manager.preference.configuration_interfaces.is_empty());
    }
}

#[test]
fn a_starting_connection_reports_its_record_after_its_first_input() {
    use cordial_core::storage::record_key;
    let mut t = Test::new(true);
    let saved = preference(SettingKey::BacklightEnabled, FeatureId::BACKLIGHT, 1);
    block_on(
        Preferences {
            store: &mut t.store,
            device: SAVED.into(),
        }
        .save(&saved),
    )
    .unwrap();
    let other = t.add_saved(2);
    t.events();
    let link = connect_saved(&mut t, descriptor());
    t.store.reads.clear();
    // Another device's change is reported while the connection waits for its first input.
    let trusted = !t.saved(other).trusted;
    t.store.reads.clear();
    t.ok(Command::SetDevice(p::SetDevice {
        device: other,
        trusted: Some(trusted),
        ..Default::default()
    }));
    let reported = |events: &[Ev], id: u32| {
        events
            .iter()
            .any(|e| matches!(e, Ev::Device(d) if d.id == id))
    };
    let events = t.events();
    assert!(reported(&events, other));
    assert!(!reported(&events, SAVED));
    assert_eq!(reads_of(&t, record_key(2, SAVED.into())), 0);
    press(&mut t, link, &[4]);
    let events = t.events();
    assert!(reported(&events, SAVED));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ev::SettingsChanged(s) if s.device == SAVED))
    );
}

#[test]
fn a_repaired_device_is_found_by_the_secondary_loop() {
    use cordial_core::storage::record_key;
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    for n in 2..=5.min(cordial_core::devices::CLASSIC_CONNECTIONS as u8) {
        t.add_saved(n);
    }
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
    // A disabled device is not resident, so only its saved record knows its identity.
    t.set_device(p::SetDevice {
        device: SAVED,
        enabled: Some(false),
        ..Default::default()
    });
    assert!(t.app.manager.find(SAVED.into()).is_none());
    t.events();
    let candidate = t.candidate(rpa, rpa);
    t.pair(candidate);
    t.poll();
    let link = t.radio.connects.last().unwrap().0;
    t.radio.bonds.push(identity);
    t.store.reads.clear();
    t.event(Event::Bonded { link, identity });
    // The priority loop reads no device record.
    assert!((77..=81).all(|id| !t.store.reads.contains(&record_key(2, id))));
    let steps = pairing_steps(&t.events());
    assert_eq!(
        steps.last(),
        Some(&p::pairing::Step::Done(p::PairingDone { device: SAVED }))
    );
    assert!(t.store.reads.contains(&record_key(2, SAVED.into())));
    let saved = t.saved(SAVED);
    assert_eq!(saved.peer, identity);
    assert!(!saved.enabled, "re-pairing keeps the saved preferences");
}

#[test]
fn settings_events_carry_only_what_changed() {
    let mut t = Test::new(true);
    for preference in [
        preference(SettingKey::BacklightEnabled, FeatureId::BACKLIGHT, 1),
        preference(SettingKey::WheelInvert, FeatureId::HIRES_WHEEL, 1),
    ] {
        block_on(
            Preferences {
                store: &mut t.store,
                device: SAVED.into(),
            }
            .save(&preference),
        )
        .unwrap();
    }
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.events();
    let listed = t.settings(SAVED);
    let groups: Vec<i32> = listed
        .iter()
        .map(|s| cordial_core::wire::object_ref(s).group)
        .collect();
    assert_eq!(
        groups,
        [
            p::SettingsGroup::Backlight as i32,
            p::SettingsGroup::Wheel as i32
        ]
    );
    match t.ok(Command::ListSettings(p::ListSettings { device: SAVED })) {
        Some(R::Settings(page)) => {
            assert_eq!(page.objects, listed);
        }
        other => panic!("{other:?}"),
    }
    let changes = |t: &mut Test| -> Vec<p::SettingsChanged> {
        t.events()
            .into_iter()
            .filter_map(|e| match e {
                Ev::SettingsChanged(s) => Some(s),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        t.ok(Command::SetSettings(p::SetSettings {
            device: SAVED,
            changes: vec![backlight_value(false)],
        })),
        None
    );
    let reported = changes(&mut t);
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].device, SAVED);
    assert_eq!(reported[0].changed, t.settings(SAVED)[..1]);
    assert!(reported[0].removed.is_empty());
    // A disconnected device's forgotten setting goes away.
    assert_eq!(
        t.ok(Command::SetSettings(p::SetSettings {
            device: SAVED,
            changes: vec![p::SettingsUpdate {
                integration: p::IntegrationKind::Hidpp as i32,
                group: Some(p::settings_update::Group::Wheel(p::WheelSettingsUpdate {
                    invert: Some(p::BoolSettingUpdate {
                        change: Some(p::bool_setting_update::Change::Forget(p::SettingForget {}))
                    }),
                    ..Default::default()
                }))
            }],
        })),
        None
    );
    let reported = changes(&mut t);
    assert_eq!(reported.len(), 1);
    assert!(reported[0].changed.is_empty());
    assert_eq!(
        reported[0].removed,
        [cordial_core::wire::settings_ref(SettingKey::WheelInvert)]
    );
}

#[test]
fn settings_lists_and_events_preserve_large_whole_objects_and_order() {
    use cordial_core::compact::Record;
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, descriptor());
    press(&mut t, link, &[4]);
    t.events();
    let records = [
        SettingKey::WheelMode,
        SettingKey::BacklightMode,
        SettingKey::FnRowDefault,
    ]
    .map(|key| {
        let mut metadata = preference(key, FeatureId::BACKLIGHT, 1).metadata;
        metadata.choices = vec![1; 600].into_boxed_slice();
        Record::new(metadata, true)
    });
    t.live(SAVED)
        .catalog
        .replace_discovery(records.into(), vec![])
        .unwrap();
    let expected = cordial_core::wire::settings(&t.live(SAVED).catalog).unwrap();
    assert_eq!(expected.len(), 3);
    let listed = t.settings(SAVED);
    assert_eq!(listed, expected);
    assert!(
        p::DeviceSettings {
            device: SAVED,
            objects: listed
        }
        .encoded_len()
            > 1024
    );
    let mut changed = Vec::new();
    for event in t.events() {
        if let Ev::SettingsChanged(settings) = event {
            assert_eq!(settings.changed.len(), 3);
            assert!(settings.removed.is_empty());
            changed.extend(settings.changed.iter().cloned());
            let message = p::Message {
                kind: Some(M::Event(p::Event {
                    kind: Some(Ev::SettingsChanged(settings)),
                })),
            };
            assert!(message.encoded_len() > 1024);
        }
    }
    assert_eq!(changed, expected);

    let mut metadata = preference(SettingKey::FnRowDefault, FeatureId::BACKLIGHT, 1).metadata;
    metadata.choices = vec![1; 961].into_boxed_slice();
    t.live(SAVED)
        .catalog
        .replace_discovery(vec![Record::new(metadata, true)], vec![])
        .unwrap();
    let objects = t.settings(SAVED);
    assert_eq!(objects.len(), 1);
    assert!(objects[0].encoded_len() > 960);
}

#[test]
fn rule_listings_stream_in_input_order() {
    let mut t = Test::new(false);
    let id = profile_with(&mut t, "Many", (4..44).rev());
    let rules = rules_of(&mut t, id);
    assert_eq!(rules.len(), 40);
    let inputs: Vec<(u32, u32)> = rules
        .iter()
        .map(|r| {
            let input = r.input.as_ref().unwrap();
            (input.usage_page, input.usage)
        })
        .collect();
    assert!(inputs.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(rules_of(&mut t, id), rules);
}

#[test]
fn set_commands_respond_with_no_result_and_creation_with_the_id() {
    let mut t = Test::new(true);
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.events();
    assert_eq!(
        t.ok(Command::SetAdapter(p::SetAdapter {
            name: Some("Desk".into()),
            ..Default::default()
        })),
        None
    );
    assert_eq!(
        t.ok(Command::SetDevice(p::SetDevice {
            device: SAVED,
            trusted: Some(false),
            ..Default::default()
        })),
        None
    );
    let events = t.events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ev::Adapter(s) if s.name == "Desk"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ev::Device(d) if d.id == SAVED && !d.trusted))
    );
    let id = create(&mut t, "New");
    assert!(t.events().contains(&Ev::Profile(profile(&mut t, id))));
    assert_eq!(
        t.ok(Command::SetProfileRules(p::SetProfileRules {
            profile: id,
            changes: vec![remap(KEYBOARD_COLLECTION, key(4), &[key(5)])],
        })),
        None
    );
}

#[test]
fn change_events_track_only_connected_devices() {
    let mut t = Test::new(true);
    // Disabled and disconnected devices, listed in full, leave nothing tracked.
    let mut ids = vec![SAVED];
    for n in 2..=6 {
        let id = t.add_saved(n);
        t.write_policy(id, |p| p.enabled = false);
        ids.push(id);
    }
    t.events();
    for &id in &ids {
        t.settings(id);
    }
    t.events();
    assert_eq!(t.app.tracked_devices(), 0);
    // A connection is tracked while it lasts.
    let link = connect_saved(&mut t, descriptor());
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    t.events();
    t.settings(SAVED);
    assert_eq!(t.app.tracked_devices(), 1);
    t.event(Event::Disconnected { link, error: None });
    t.events();
    assert_eq!(t.app.tracked_devices(), 0);
    t.events();
    assert_eq!(t.app.tracked_devices(), 0);
}

#[test]
fn failed_event_reads_are_retried_after_a_backoff() {
    let mut t = Test::new(true);
    block_on(
        Preferences {
            store: &mut t.store,
            device: SAVED.into(),
        }
        .save(&preference(
            SettingKey::BacklightEnabled,
            FeatureId::BACKLIGHT,
            1,
        )),
    )
    .unwrap();
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    let id = create(&mut t, "Map");
    t.events();
    let reads = |t: &Test, key| t.store.reads.iter().filter(|k| **k == key).count();
    // A disconnected device's settings event and a profile event each read a record.
    /// A record the event reads, a change that marks the event, and the event.
    type Case = (
        cordial_core::storage::RecordKey,
        fn(&mut Test, u32),
        fn(&Ev, u32) -> bool,
    );
    let cases: [Case; 2] = [
        (
            cordial_core::storage::record_key(4, SAVED.into()),
            |t, _| {
                assert_eq!(
                    t.ok(Command::SetSettings(p::SetSettings {
                        device: SAVED,
                        changes: vec![backlight_value(false)],
                    })),
                    None
                );
            },
            |e, _| matches!(e, Ev::SettingsChanged(s) if s.device == SAVED),
        ),
        (
            cordial_core::storage::record_key(cordial_core::profiles::METADATA, id.into()),
            |t, id| {
                // A change of roles marks a profile event.
                assert_eq!(
                    t.ok(Command::SetProfileRules(p::SetProfileRules {
                        profile: id,
                        changes: vec![remap(CONSUMER_COLLECTION, usage(0x0c, 0xcd), &[key(5)])],
                    })),
                    None
                );
            },
            |e, id| matches!(e, Ev::Profile(p) if p.id == id),
        ),
    ];
    for (record, change, event) in cases {
        change(&mut t, id);
        t.store.fail_load = Some(record);
        let before = reads(&t, record);
        assert!(!t.events().iter().any(|e| event(e, id)));
        assert_eq!(reads(&t, record), before + 1);
        // The event stays pending, and nothing reads the record again until the backoff passes.
        t.store.fail_load = None;
        assert!(!t.events().iter().any(|e| event(e, id)));
        assert_eq!(reads(&t, record), before + 1);
        t.now += FIRST_RETRY;
        assert_eq!(t.events().iter().filter(|e| event(e, id)).count(), 1);
    }
}

#[test]
fn first_connection_setup_waits_for_another_connections_first_input() {
    let mut t = Test::new(true);
    let other = t.add_saved(2);
    pending_setup(&mut t);
    let link = connect_saved(&mut t, hidpp_descriptor());
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 1, &[0]).unwrap(),
    ));
    t.poll();
    // Another saved device reconnects while the new device's setup is due.
    t.entry(other).paused = false;
    t.poll();
    let starting = t.radio.connects.last().unwrap().0;
    assert_ne!(starting, link);
    t.event(Event::Connected {
        link: starting,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    answer_protocol(&mut t, link, 4);
    t.events();
    assert!(saved_policy(&mut t).setup_pending);
    t.event(Event::Input(
        InputReport::new(starting, ServiceId(7), 0, &[1]).unwrap(),
    ));
    t.events();
    // Setup progress is saved once input pauses.
    assert!(saved_policy(&mut t).setup_pending);
    t.now += cordial_core::deferred::QUIET_MS;
    t.events();
    assert!(saved_policy(&mut t).hidpp_enabled() && !saved_policy(&mut t).setup_pending);
}

#[test]
fn an_unpair_finishes_after_another_connections_first_input() {
    let mut t = Test::new(true);
    let other = t.add_saved(2);
    let link = connect_saved(&mut t, descriptor());
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(),
    ));
    t.events();
    t.entry(other).paused = false;
    t.poll();
    let starting = t.radio.connects.last().unwrap().0;
    assert_ne!(starting, link);
    t.event(Event::Connected {
        link: starting,
        descriptors: descriptor(),
        max_output: 255,
        layout: None,
    });
    assert_eq!(
        t.ok(Command::UnpairDevice(p::UnpairDevice { device: SAVED })),
        None
    );
    t.finish_disconnects();
    t.events();
    let record = cordial_core::storage::record_key(2, SAVED.into());
    assert!(t.store.records.contains_key(&record));
    t.event(Event::Input(
        InputReport::new(starting, ServiceId(7), 0, &[1]).unwrap(),
    ));
    let events = t.events();
    assert!(!t.store.records.contains_key(&record));
    assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED })));
}

/// Marks a device event for the saved keyboard and changes one of its saved settings, then
/// returns whether both of its events were written.
fn healthy_device_events(t: &mut Test, round: u64) -> bool {
    t.app.manager.changed.push(SAVED.into());
    assert_eq!(
        t.ok(Command::SetSettings(p::SetSettings {
            device: SAVED,
            changes: vec![backlight_value(round.is_multiple_of(2))],
        })),
        None
    );
    let events = t.events();
    events
        .iter()
        .any(|e| matches!(e, Ev::Device(d) if d.id == SAVED))
        && events
            .iter()
            .any(|e| matches!(e, Ev::SettingsChanged(s) if s.device == SAVED))
}
/// The saved keyboard, disconnected, with a saved backlight setting.
fn healthy_device() -> Test {
    let mut t = Test::new(true);
    block_on(
        Preferences {
            store: &mut t.store,
            device: SAVED.into(),
        }
        .save(&preference(
            SettingKey::BacklightEnabled,
            FeatureId::BACKLIGHT,
            1,
        )),
    )
    .unwrap();
    t.app.manager.devices[0].as_mut().unwrap().paused = true;
    t.events();
    t
}

#[test]
fn an_unreadable_profile_does_not_hold_up_other_events() {
    let mut t = healthy_device();
    let failing = create(&mut t, "Failing");
    let other = create(&mut t, "Other");
    t.events();
    // A change of roles marks a profile event, which reads the profile's metadata.
    let input = usage(0x0c, 0xcd);
    set_rules(
        &mut t,
        failing,
        vec![remap(CONSUMER_COLLECTION, input, &[key(5)])],
    );
    let record =
        cordial_core::storage::record_key(cordial_core::profiles::METADATA, failing.into());
    t.store.fail_load = Some(record);
    let profile = |events: &[Ev], id: u32| {
        events
            .iter()
            .any(|e| matches!(e, Ev::Profile(p) if p.id == id))
    };
    let reads = |t: &Test| t.store.reads.iter().filter(|k| **k == record).count();
    let base = reads(&t);
    assert!(!profile(&t.events(), failing));
    assert_eq!(reads(&t), base + 1);
    let mut delay = FIRST_RETRY;
    for round in 0..4 {
        let before = reads(&t);
        // Another profile's change and the device's events are written at once.
        let change = if round % 2 == 0 {
            remap(CONSUMER_COLLECTION, input, &[key(5)])
        } else {
            forget_rule(CONSUMER_COLLECTION, input)
        };
        set_rules(&mut t, other, vec![change]);
        let events = t.events();
        assert!(profile(&events, other));
        assert!(!profile(&events, failing));
        assert!(healthy_device_events(&mut t, round));
        // The failing profile is read again only once its backoff has passed.
        assert_eq!(reads(&t), before);
        t.now += delay;
        delay *= 2;
        assert!(!profile(&t.events(), failing));
        assert_eq!(reads(&t), before + 1);
    }
    t.store.fail_load = None;
    t.now += delay;
    assert!(profile(&t.events(), failing));
}

#[test]
fn an_unreadable_device_does_not_hold_up_other_devices_events() {
    let mut t = healthy_device();
    let failing = t.add_saved(2);
    t.events();
    let record = cordial_core::storage::record_key(2, failing.into());
    t.app.manager.changed.push(failing.into());
    t.store.fail_load = Some(record);
    let device = |events: &[Ev], id: u32| {
        events
            .iter()
            .any(|e| matches!(e, Ev::Device(d) if d.id == id))
    };
    let reads = |t: &Test| t.store.reads.iter().filter(|k| **k == record).count();
    let base = reads(&t);
    assert!(!device(&t.events(), failing));
    assert_eq!(reads(&t), base + 1);
    let mut delay = FIRST_RETRY;
    for round in 0..4 {
        let before = reads(&t);
        assert!(healthy_device_events(&mut t, round));
        assert_eq!(reads(&t), before);
        t.now += delay;
        delay *= 2;
        assert!(!device(&t.events(), failing));
        assert_eq!(reads(&t), before + 1);
    }
    t.store.fail_load = None;
    t.now += delay;
    assert!(device(&t.events(), failing));
}

/// Checks that the event whose read of `record` fails waits out a doubling backoff across four
/// rounds, while `other` makes another event of the same device succeed in each, then
/// that it is written once the read succeeds. `failing` and `succeeded` find the two events.
fn backoff_grows_across_other_events(
    t: &mut Test,
    record: cordial_core::storage::RecordKey,
    other: impl Fn(&mut Test, u32),
    failing: impl Fn(&Ev) -> bool,
    succeeded: impl Fn(&Ev) -> bool,
) {
    let reads = |t: &Test| t.store.reads.iter().filter(|k| **k == record).count();
    t.store.fail_load = Some(record);
    let before = reads(t);
    assert!(!t.events().iter().any(&failing));
    assert_eq!(reads(t), before + 1);
    let mut delay = FIRST_RETRY;
    for round in 0..4 {
        t.store.fail_load = None;
        other(t, round);
        t.store.fail_load = Some(record);
        let before = reads(t);
        let events = t.events();
        assert!(events.iter().any(&succeeded), "round {round}");
        assert!(!events.iter().any(&failing));
        // The other event's success leaves this event's grown backoff as it was.
        t.now += delay - 100;
        assert!(!t.events().iter().any(&failing));
        assert_eq!(reads(t), before, "round {round}");
        t.now += 100;
        assert!(!t.events().iter().any(&failing));
        assert_eq!(reads(t), before + 1, "round {round}");
        delay *= 2;
    }
    t.store.fail_load = None;
    t.now += delay;
    assert!(t.events().iter().any(&failing));
}

#[test]
fn a_settings_event_backoff_grows_across_device_events() {
    let mut t = healthy_device();
    assert_eq!(
        t.ok(Command::SetSettings(p::SetSettings {
            device: SAVED,
            changes: vec![backlight_value(false)],
        })),
        None
    );
    backoff_grows_across_other_events(
        &mut t,
        cordial_core::storage::record_key(4, SAVED.into()),
        |t, _| t.app.manager.changed.push(SAVED.into()),
        |e| matches!(e, Ev::SettingsChanged(s) if s.device == SAVED),
        |e| matches!(e, Ev::Device(d) if d.id == SAVED),
    );
}

// ---------------------------------------------------------------------------
// Deferred saves
// ---------------------------------------------------------------------------

/// A VIA editor on a new profile that `SAVED`'s connection uses. Returns the profile and the link.
fn editing(t: &mut Test) -> (u32, cordial_core::link::LinkId) {
    let id = create(t, "Keys");
    set_layers(t, SAVED, &[id]);
    let link = connect_saved(t, descriptor());
    set_interfaces(
        t,
        vec![interface(
            p::ConfigurationInterface::Via,
            Some(true),
            Some(id),
        )],
    );
    t.events();
    t.now += cordial_core::deferred::MAX_DELAY_MS;
    t.events();
    t.store.writes.clear();
    (id, link)
}
/// Sets VIA's key at row 0, column 0, which selects key 4, to `code`; returns the reply's command.
fn set_first_key(t: &mut Test, code: u8) -> u8 {
    configure(t, Interface::Via, &[5, 0, 0, 0, 0, code])[0]
}
fn rules_writes(t: &Test, id: u32) -> usize {
    t.store
        .writes
        .iter()
        .filter(|k| **k == record_key(9, id))
        .count()
}
/// Types a key on `link` and releases it.
fn tap(t: &mut Test, link: cordial_core::link::LinkId) {
    press(t, link, &[5]);
    press(t, link, &[]);
}

#[test]
fn editor_edits_are_echoed_at_once_and_saved_in_one_write() {
    let mut t = Test::new(true);
    let (id, link) = editing(&mut t);
    // Each edit is echoed and in use before anything is written.
    for code in 5..=20 {
        assert_eq!(set_first_key(&mut t, code), 5);
        assert_eq!(press(&mut t, link, &[4]), [u16::from(code)]);
        assert_eq!(press(&mut t, link, &[]), Vec::<u16>::new());
        t.poll();
    }
    assert!(t.store.writes.is_empty());
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[20])]
    );
    assert_eq!(profile(&mut t, id).roles, [p::Role::Keyboard as i32]);
    // Once the editor and input pause, the edits are written once, then the roles summary.
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert_eq!(rules_writes(&t, id), 1);
    assert_eq!(t.store.writes, [record_key(9, id), record_key(8, id)]);
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[20])]
    );
    assert_eq!(profile(&mut t, id).roles, [p::Role::Keyboard as i32]);
    // Nothing more is written.
    t.now += cordial_core::deferred::MAX_DELAY_MS;
    t.events();
    assert_eq!(t.store.writes.len(), 2);
}

#[test]
fn edits_are_saved_once_input_pauses_or_after_the_longest_wait() {
    use cordial_core::{application::EDITOR_BATCH_MS, deferred::MAX_DELAY_MS, deferred::QUIET_MS};
    let mut t = Test::new(true);
    let (id, link) = editing(&mut t);
    // Typing pauses after the editor does: the edit is written once input has been quiet.
    assert_eq!(set_first_key(&mut t, 5), 5);
    let edited = t.now;
    while t.now < edited + EDITOR_BATCH_MS + 100 {
        t.now += 50;
        tap(&mut t, link);
        t.poll();
    }
    let typed = t.now;
    assert_eq!(rules_writes(&t, id), 0);
    t.now = typed + QUIET_MS - 10;
    t.poll();
    assert_eq!(rules_writes(&t, id), 0);
    t.now = typed + QUIET_MS;
    t.poll();
    assert_eq!(rules_writes(&t, id), 1);
    // While typing goes on, an edit is written no later than the longest wait after it was made.
    assert_eq!(set_first_key(&mut t, 6), 5);
    let edited = t.now;
    while t.now < edited + MAX_DELAY_MS - 60 {
        t.now += 50;
        tap(&mut t, link);
        t.poll();
        assert_eq!(rules_writes(&t, id), 1, "at {}", t.now - edited);
    }
    t.now = edited + MAX_DELAY_MS;
    tap(&mut t, link);
    t.poll();
    assert_eq!(rules_writes(&t, id), 2);
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[6])]
    );
}

#[test]
fn a_continuous_stream_of_edits_is_handed_to_storage_every_longest_batch() {
    use cordial_core::application::EDITOR_BATCH_MAX_MS;
    let mut t = Test::new(true);
    let (id, _) = editing(&mut t);
    let mut code = 5;
    // An edit every 100 ms, with no typing: no edit waits longer than the longest batch.
    let mut first = None;
    let mut writes = Vec::new();
    for _ in 0..60 {
        assert_eq!(set_first_key(&mut t, code), 5);
        code = if code == 20 { 5 } else { code + 1 };
        first.get_or_insert(t.now);
        t.poll();
        if rules_writes(&t, id) > writes.len() {
            let waited = t.now - first.take().unwrap();
            assert!(
                (EDITOR_BATCH_MAX_MS..EDITOR_BATCH_MAX_MS + 10).contains(&waited),
                "{waited}"
            );
            writes.push(t.now);
        }
        t.now += 98;
    }
    assert_eq!(writes.len(), 2);
}

#[test]
fn a_failed_deferred_save_is_reported_and_retried_with_a_backoff() {
    let mut t = Test::new(true);
    let (id, link) = editing(&mut t);
    let rules = record_key(9, id);
    // A full filesystem: storage reports full, and stays ready so room can be made.
    t.store.full = Some(rules);
    assert_eq!(set_first_key(&mut t, 5), 5);
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    let events = t.events();
    assert!(events.iter().any(|e| matches!(e, Ev::Adapter(s)
        if s.ready && s.errors.iter().any(|e| e.code == p::ErrorCode::StorageFull as i32))));
    assert_eq!(rules_writes(&t, id), 1);
    // The edit stays in use, and nothing is tried again before the backoff.
    assert_eq!(press(&mut t, link, &[4]), [5]);
    press(&mut t, link, &[]);
    t.now += FIRST_RETRY - 300;
    t.events();
    assert_eq!(rules_writes(&t, id), 1);
    // A flash failure is retried, and storage stays ready, so the profile can still be changed.
    t.store.full = None;
    t.store.fail_save = Some((rules, false));
    t.now += 300;
    let events = t.events();
    assert_eq!(rules_writes(&t, id), 2);
    assert!(t.status().ready);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Ev::Adapter(s) if !s.ready))
    );
    assert!(matches!(
        t.ok(Command::ListProfiles(p::ListProfiles {})),
        Some(R::Profiles(_))
    ));
    assert_eq!(set_first_key(&mut t, 6), 5);
    assert_eq!(press(&mut t, link, &[4]), [6]);
    press(&mut t, link, &[]);
    // A write whose outcome is unknown makes storage not ready until the save succeeds.
    t.store.fail_save = None;
    t.store.unknown = Some(rules);
    t.now += 2 * FIRST_RETRY;
    let events = t.events();
    assert_eq!(rules_writes(&t, id), 3);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ev::Adapter(s) if !s.ready))
    );
    assert_eq!(
        t.code(Command::ListProfiles(p::ListProfiles {})),
        p::ErrorCode::NotReady
    );
    // The editor waits too.
    assert_eq!(set_first_key(&mut t, 7), 0xff);
    t.store.unknown = None;
    t.now += 4 * FIRST_RETRY;
    let events = t.events();
    assert_eq!(rules_writes(&t, id), 4);
    assert!(events.iter().any(|e| matches!(e, Ev::Adapter(s)
        if s.ready && !s.errors.iter().any(|e| e.code == p::ErrorCode::StorageFull as i32))));
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[6])]
    );
    assert_eq!(set_first_key(&mut t, 7), 5);
}

#[test]
fn a_rules_file_that_keeps_failing_can_be_overwritten_or_deleted() {
    let mut t = Test::new(true);
    let (id, link) = editing(&mut t);
    let rules = record_key(9, id);
    t.store.fail_save = Some((rules, false));
    assert_eq!(set_first_key(&mut t, 5), 5);
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert_eq!(rules_writes(&t, id), 1);
    assert!(t.status().ready);
    // Overwriting the rules writes them at once; the failure is reported to that command.
    assert_eq!(
        t.code(Command::SetProfileRules(p::SetProfileRules {
            profile: id,
            changes: vec![remap(KEYBOARD_COLLECTION, key(4), &[key(8)])],
        })),
        p::ErrorCode::StorageFailed
    );
    t.store.fail_save = None;
    set_rules(
        &mut t,
        id,
        vec![remap(KEYBOARD_COLLECTION, key(4), &[key(9)])],
    );
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[9])]
    );
    assert_eq!(press(&mut t, link, &[4]), [9]);
    press(&mut t, link, &[]);
    // Nothing is left to write.
    let writes = rules_writes(&t, id);
    t.now += 8 * FIRST_RETRY;
    t.events();
    assert_eq!(rules_writes(&t, id), writes);
    assert!(!t.app.has_unsaved());
    // A profile whose edits keep failing can be deleted, which ends the retries.
    t.store.fail_save = Some((rules, false));
    assert_eq!(set_first_key(&mut t, 5), 5);
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert_eq!(rules_writes(&t, id), writes + 1);
    set_interfaces(
        &mut t,
        vec![interface(
            p::ConfigurationInterface::Via,
            Some(false),
            Some(0),
        )],
    );
    set_layers(&mut t, SAVED, &[]);
    t.ok(Command::DeleteProfile(p::DeleteProfile { profile: id }));
    assert!(!t.store.records.contains_key(&rules));
    assert!(t.status().ready);
    let writes = rules_writes(&t, id);
    t.now += 8 * FIRST_RETRY;
    t.events();
    assert_eq!(rules_writes(&t, id), writes);
    assert!(!t.app.has_unsaved());
}

#[test]
fn a_released_editor_saves_its_edits_at_once() {
    use cordial_core::application::EDITOR_IDLE_MS;
    let mut t = Test::new(true);
    let (id, _) = editing(&mut t);
    let rules = record_key(9, id);
    t.store.fail_save = Some((rules, false));
    assert_eq!(set_first_key(&mut t, 5), 5);
    let edited = t.now;
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    t.now += FIRST_RETRY;
    t.events();
    // Two failures: the next retry waits until after the editor is released.
    assert_eq!(rules_writes(&t, id), 2);
    assert!(t.now + 2 * FIRST_RETRY > edited + EDITOR_IDLE_MS);
    t.store.fail_save = None;
    t.now = edited + EDITOR_IDLE_MS - 2;
    t.poll();
    assert_eq!(rules_writes(&t, id), 2);
    // Releasing the idle editor saves at once.
    t.poll();
    assert_eq!(rules_writes(&t, id), 3);
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

#[test]
fn space_is_checked_after_deferred_saves_are_written() {
    let mut t = Test::new(true);
    let (id, link) = editing(&mut t);
    assert_eq!(set_first_key(&mut t, 5), 5);
    tap(&mut t, link);
    // Input has not paused, but a profile copy writes the edit before counting free space.
    let copy = match t.ok(Command::CopyProfile(p::CopyProfile {
        profile: id,
        name: "Copy".into(),
    })) {
        Some(R::ProfileCreated(created)) => created.profile,
        r => panic!("{r:?}"),
    };
    assert_eq!(t.store.writes[0], record_key(9, id));
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert_eq!(
        rules_of(&mut t, copy),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

#[cfg(feature = "development")]
#[test]
fn bootloader_entry_saves_edits_first() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REBOOTED: AtomicBool = AtomicBool::new(false);
    let (_, store, radio) = setup();
    let mut t = Test::with(
        store,
        radio,
        build(
            true,
            Some(|| {
                REBOOTED.store(true, Ordering::SeqCst);
                panic!("rebooted")
            }),
        ),
    );
    let (id, link) = editing(&mut t);
    assert_eq!(set_first_key(&mut t, 5), 5);
    tap(&mut t, link);
    assert_eq!(t.ok(Command::EnterBootloader(p::EnterBootloader {})), None);
    let rebooted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for _ in 0..4 {
            t.poll();
            drain_forward(&mut t);
        }
    }));
    assert!(rebooted.is_err() && REBOOTED.load(Ordering::SeqCst));
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

#[test]
fn streamed_rules_read_the_complete_saved_table() {
    let mut t = Test::with_budget(false, Some(8192));
    let id = create(&mut t, "Many");
    let changes: Vec<_> = (4..=0xa4)
        .map(|k| remap(KEYBOARD_COLLECTION, key(k), &[key(0xe0), key(4)]))
        .collect();
    let count = changes.len();
    for chunk in changes.chunks(16) {
        set_rules(&mut t, id, chunk.to_vec());
    }
    // The profile is not loaded, so its pages come from the file.
    assert!(t.app.manager.profiles.get(id.into()).is_none());
    let file = t.store.records[&record_key(9, id)].len();
    t.store.part_bytes = 0;
    let first = match t.ok(Command::ListProfileRules(p::ListProfileRules {
        profile: id,
    })) {
        Some(R::ProfileRules(page)) => page,
        r => panic!("{r:?}"),
    };
    assert_eq!(first.rules.len(), count);
    assert!(
        t.store.part_bytes >= file,
        "{} of {file} bytes",
        t.store.part_bytes
    );
    // The merged response is the whole saved table.
    let listed = rules_of(&mut t, id);
    assert_eq!(listed.len(), count);
    let decoded: Vec<_> = block_on(cordial_core::profiles::rules(&mut t.store, id.into()))
        .unwrap()
        .iter()
        .collect();
    assert_eq!(saved_rules(&mut t, id), decoded);
    assert_eq!(listed[..first.rules.len()], first.rules[..]);
}

#[test]
fn saved_pages_match_the_decoded_file_and_find_damage() {
    use cordial_core::profiles;
    let mut t = Test::new(false);
    let id = create(&mut t, "Mixed");
    let mut changes: Vec<_> = (4..=60)
        .map(|k| remap(KEYBOARD_COLLECTION, key(k), &[key(k + 1)]))
        .collect();
    changes.push(remap(KEYBOARD_COLLECTION, key(61), &[]));
    for chunk in changes.chunks(16) {
        set_rules(&mut t, id, chunk.to_vec());
    }
    let id = u64::from(id);
    let all: Vec<_> = block_on(profiles::rules(&mut t.store, id))
        .unwrap()
        .iter()
        .collect();
    for after in [
        None,
        Some(all[0].reference()),
        Some(all[30].reference()),
        Some(profiles::RuleRef {
            input: u32::MAX,
            collection: u32::MAX,
        }),
    ] {
        for count in [1, 7, 33, 1000] {
            let page = block_on(profiles::saved_page(&mut t.store, id, after, count)).unwrap();
            let expected: Vec<_> = all
                .iter()
                .filter(|r| after.is_none_or(|a| r.reference() > a))
                .take(count)
                .cloned()
                .collect();
            assert_eq!(page, expected, "after {after:?}, {count}");
        }
    }
    // A file cut short, or with rules out of order, is undecodable once reading reaches it.
    let key = record_key(9, id as u32);
    let bytes = t.store.records[&key].clone();
    t.store
        .records
        .insert(key, bytes[..bytes.len() - 1].to_vec());
    assert!(block_on(profiles::saved_page(&mut t.store, id, None, 3)).is_ok());
    assert_eq!(
        block_on(profiles::saved_page(&mut t.store, id, None, 1000)),
        Err(cordial_core::storage::Error::Corrupt)
    );
    let rule = |input| storage::Rule {
        input,
        collection: profiles::KEYBOARD,
        effect: Some(storage::rule::Effect::Remap(storage::Remap {
            outputs: vec![],
        })),
    };
    let unsorted = storage::Rules {
        rules: vec![rule(9), rule(8)],
    };
    t.store.records.insert(key, unsorted.encode_to_vec());
    assert_eq!(
        block_on(profiles::saved_page(&mut t.store, id, None, 10)),
        Err(cordial_core::storage::Error::Corrupt)
    );
}

#[test]
fn a_command_that_matches_unsaved_edits_saves_them_before_it_responds() {
    let mut t = Test::new(true);
    let (id, link) = editing(&mut t);
    assert_eq!(set_first_key(&mut t, 5), 5);
    tap(&mut t, link);
    // The same rule through the serial API changes nothing in RAM, but its response promises
    // it is on flash.
    set_rules(
        &mut t,
        id,
        vec![remap(KEYBOARD_COLLECTION, key(4), &[key(5)])],
    );
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert_eq!(rules_writes(&t, id), 1);
    // Nothing is left to write once the editor pauses.
    t.now += cordial_core::deferred::MAX_DELAY_MS;
    t.events();
    assert_eq!(rules_writes(&t, id), 1);
    assert!(!t.app.has_unsaved());
}

#[test]
fn admission_stops_when_saving_pending_edits_makes_storage_not_ready() {
    let mut t = Test::new(true);
    let (id, link) = editing(&mut t);
    let rules = record_key(9, id);
    t.store.unknown = Some(rules);
    assert_eq!(set_first_key(&mut t, 5), 5);
    tap(&mut t, link);
    let profiles = t.store.records.keys().filter(|k| k[0] == 8).count();
    // Saving the edit before counting free space ends with an unknown outcome, so nothing new
    // is admitted.
    assert_eq!(
        t.code(Command::CreateProfile(p::CreateProfile {
            name: "New".into(),
        })),
        p::ErrorCode::NotReady
    );
    assert_eq!(rules_writes(&t, id), 1);
    assert_eq!(
        t.store.records.keys().filter(|k| k[0] == 8).count(),
        profiles
    );
    assert!(!t.status().ready);
}

#[test]
fn an_editor_moving_to_another_profile_saves_its_edits_first() {
    let mut t = Test::new(true);
    let (id, link) = editing(&mut t);
    let other = create(&mut t, "Other");
    assert_eq!(set_first_key(&mut t, 5), 5);
    tap(&mut t, link);
    assert_eq!(rules_writes(&t, id), 0);
    // The interface's saved profile changes without the editor being released first.
    let mut via = cordial_core::interfaces::preference(
        &t.app.manager.preference.configuration_interfaces,
        Interface::Via,
    );
    via.profile = Some(other.into());
    cordial_core::interfaces::set(&mut t.app.manager.preference.configuration_interfaces, via);
    // The next packet edits the other profile, after the first one's edits are written, even
    // though input has not paused.
    assert_eq!(set_first_key(&mut t, 6), 5);
    assert_eq!(rules_writes(&t, id), 1);
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert_eq!(
        rules_of(&mut t, other),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[6])]
    );
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

#[test]
fn storage_is_ready_again_once_no_rules_file_has_an_unknown_outcome() {
    let mut t = Test::new(true);
    let (id, _) = editing(&mut t);
    let other = create(&mut t, "Other");
    // The first profile's edits keep failing.
    t.store.fail_save = Some((record_key(9, id), false));
    assert_eq!(set_first_key(&mut t, 5), 5);
    let mut via = cordial_core::interfaces::preference(
        &t.app.manager.preference.configuration_interfaces,
        Interface::Via,
    );
    via.profile = Some(other.into());
    cordial_core::interfaces::set(&mut t.app.manager.preference.configuration_interfaces, via);
    assert_eq!(set_first_key(&mut t, 6), 5);
    assert_eq!(rules_writes(&t, id), 1);
    assert!(t.status().ready);
    // The other profile's write ends with an unknown outcome.
    t.store.unknown = Some(record_key(9, other));
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert_eq!(rules_writes(&t, other), 1);
    assert!(!t.status().ready);
    // Once it is written, storage is ready, though the first profile's edits still fail.
    t.store.unknown = None;
    t.now += 2 * FIRST_RETRY;
    t.events();
    assert_eq!(rules_writes(&t, other), 2);
    assert!(rules_writes(&t, id) > 1);
    assert!(t.status().ready);
    assert!(t.app.has_unsaved());
    assert_eq!(
        saved_rules(&mut t, other),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[6])]
    );
}

/// A VIA editor on a new profile with no device connected, so nothing has been listed to save
/// yet. Returns the profile.
fn editing_alone(t: &mut Test) -> u32 {
    let id = create(t, "Keys");
    set_interfaces(
        t,
        vec![interface(
            p::ConfigurationInterface::Via,
            Some(true),
            Some(id),
        )],
    );
    t.store.writes.clear();
    id
}
/// Points VIA at profile `id` without releasing its editor.
fn move_via(t: &mut Test, id: u32) {
    let mut via = cordial_core::interfaces::preference(
        &t.app.manager.preference.configuration_interfaces,
        Interface::Via,
    );
    via.profile = Some(id.into());
    cordial_core::interfaces::set(&mut t.app.manager.preference.configuration_interfaces, via);
}

#[test]
fn edits_there_is_no_memory_to_list_are_written_from_the_editor() {
    let mut t = Test::new(true);
    let id = editing_alone(&mut t);
    assert_eq!(set_first_key(&mut t, 5), 5);
    // Listing the edits fails, as before the bootloader, so the editor's table is written as it is.
    fail_next_allocation();
    block_on(t.app.save_all(&mut t.store, t.now));
    assert!(allocation_failed());
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert_eq!(
        block_on(cordial_core::profiles::metadata(&mut t.store, id.into()))
            .unwrap()
            .roles
            .0,
        cordial_core::hid::KEYBOARD
    );
    assert!(!t.app.has_unsaved());
}

#[test]
fn an_editor_kept_for_lack_of_memory_does_not_outlive_its_deleted_profile() {
    let mut t = Test::new(true);
    let id = editing_alone(&mut t);
    let other = create(&mut t, "Other");
    assert_eq!(set_first_key(&mut t, 5), 5);
    // Moving to another profile can neither list the edits, nor encode them to write them from
    // the editor, nor then list them again, so the editor is kept and refuses.
    move_via(&mut t, other);
    fail_allocations(3);
    assert_eq!(set_first_key(&mut t, 6), 0xff);
    assert!(allocation_failed());
    assert_eq!(rules_writes(&t, id), 0);
    assert!(t.app.has_unsaved());
    t.ok(Command::DeleteProfile(p::DeleteProfile { profile: id }));
    // Its edits are not written back as a rules file of the deleted profile.
    block_on(t.app.save_all(&mut t.store, t.now));
    t.now += cordial_core::application::EDITOR_IDLE_MS;
    t.events();
    assert!(!t.store.records.contains_key(&record_key(9, id)));
    assert_eq!(set_first_key(&mut t, 6), 5);
    assert_eq!(
        rules_of(&mut t, other),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[6])]
    );
}

#[test]
fn an_unknown_outcome_ends_only_once_the_file_is_read() {
    let mut t = Test::new(true);
    let (id, _) = editing(&mut t);
    let rules = record_key(9, id);
    t.store.unknown = Some(rules);
    assert_eq!(set_first_key(&mut t, 5), 5);
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert_eq!(rules_writes(&t, id), 1);
    assert!(!t.status().ready);
    t.store.unknown = None;
    // The unknown write did not reach the file after all.
    t.store.records.remove(&rules);
    let retry = |t: &mut Test| t.now += cordial_core::deferred::RULES_RETRY_MAX_MS as u64 + 1;
    // A retry that cannot even encode the rules leaves the outcome unknown.
    retry(&mut t);
    fail_next_allocation();
    assert!(block_on(t.app.save(&mut t.store, t.now)));
    assert!(allocation_failed());
    assert_eq!(rules_writes(&t, id), 1);
    assert!(!t.status().ready);
    // So does a failed retry whose file cannot be read.
    t.store.fail_save = Some((rules, false));
    t.store.fail_load = Some(rules);
    retry(&mut t);
    t.events();
    assert_eq!(rules_writes(&t, id), 2);
    assert!(!t.status().ready);
    // A failed retry whose file is read and lacks the edits is a definite failure.
    t.store.fail_load = None;
    retry(&mut t);
    t.events();
    assert_eq!(rules_writes(&t, id), 3);
    assert!(t.status().ready);
    assert!(t.app.has_unsaved());
    t.store.fail_save = None;
    retry(&mut t);
    t.events();
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert!(!t.app.has_unsaved());
}

#[test]
fn a_failed_retry_of_a_file_that_holds_the_edits_counts_as_saved() {
    let mut t = Test::new(true);
    let (id, _) = editing(&mut t);
    let rules = record_key(9, id);
    // The unknown write reached the file.
    t.store.unknown = Some(rules);
    assert_eq!(set_first_key(&mut t, 5), 5);
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert!(!t.status().ready);
    t.store.unknown = None;
    t.store.fail_save = Some((rules, false));
    t.now += FIRST_RETRY + cordial_core::deferred::QUIET_MS;
    t.events();
    assert_eq!(rules_writes(&t, id), 2);
    assert!(t.status().ready);
    assert!(!t.app.has_unsaved());
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

#[test]
fn radio_recovery_keeps_edits_not_yet_saved() {
    let mut t = Test::new(true);
    let id = editing_alone(&mut t);
    set_layers(&mut t, SAVED, &[id]);
    let rules = record_key(9, id);
    t.store.unknown = Some(rules);
    assert_eq!(set_first_key(&mut t, 5), 5);
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert!(!t.status().ready);
    // The radio restarts while the write remains unresolved.
    t.store.unknown = None;
    t.store.fail_save = Some((rules, false));
    t.store.records.remove(&rules);
    t.event(Event::Restarting(ErrorCode::RadioUnavailable));
    t.radio.bonds.clear();
    t.radio.applied.clear();
    let writes = t.store.writes.len();
    t.event(Event::Ready);
    assert_eq!(t.radio.bonds, [peer(1)]);
    assert_eq!(
        t.radio.applied,
        [(Transport::Classic, true), (Transport::Ble, true)]
    );
    assert_eq!(t.store.writes.len(), writes);
    assert!(t.app.manager.unsaved_unready);
    assert_eq!(t.app.manager.admits(peer(1), t.now), None);
    assert_eq!(t.status().errors[0].outcome(), p::ErrorOutcome::Unknown);
    // Storage stays unavailable until a retry reads the file back.
    assert!(!t.status().ready);
    t.now += cordial_core::deferred::RULES_RETRY_MAX_MS as u64;
    t.events();
    assert!(t.status().ready);
    assert!(!t.store.records.contains_key(&rules));
    assert_eq!(
        rules_of(&mut t, id),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    // A device that connects uses the edited table, and further edits change that same table.
    let link = connect_saved(&mut t, descriptor());
    t.events();
    assert_eq!(press(&mut t, link, &[4]), [5]);
    press(&mut t, link, &[]);
    assert_eq!(configure(&mut t, Interface::Via, &[5, 0, 0, 1, 0, 7])[0], 5);
    t.store.fail_save = None;
    t.now += cordial_core::application::EDITOR_BATCH_MS
        + cordial_core::deferred::RULES_RETRY_MAX_MS as u64;
    t.events();
    let saved = saved_rules(&mut t, id);
    assert_eq!(saved.len(), 2);
    assert!(saved.contains(&remap_rule(KEYBOARD_COLLECTION, key(4), &[5])));
    assert_eq!(press(&mut t, link, &[4]), [5]);
}

#[test]
fn radio_recovery_read_failure_keeps_profile_save_recovery_pending() {
    let mut t = Test::new(true);
    let id = editing_alone(&mut t);
    set_layers(&mut t, SAVED, &[id]);
    t.store.unknown = Some(record_key(9, id));
    assert_eq!(set_first_key(&mut t, 5), 5);
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert!(t.app.manager.unsaved_unready);
    let failure = t.app.manager.storage_error.clone();

    t.event(Event::Restarting(ErrorCode::RadioUnavailable));
    t.store.fail_load = Some(record_key(2, SAVED));
    t.event(Event::Ready);
    assert!(t.app.manager.unsaved_unready);
    assert_eq!(t.app.manager.storage_error, failure);
    assert!(!t.app.manager.storage_ready);

    t.store.fail_load = None;
    t.store.unknown = None;
    t.now += cordial_core::deferred::RULES_RETRY_MAX_MS as u64;
    t.events();
    assert!(t.status().ready);
    assert!(t.status().errors.is_empty());
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

#[test]
fn edits_that_change_a_profiles_roles_are_profile_events() {
    let mut t = Test::new(true);
    let (id, _) = editing(&mut t);
    let roles = |events: &[Ev]| {
        events
            .iter()
            .filter_map(|e| match e {
                Ev::Profile(p) if p.id == id => Some(p.roles.clone()),
                _ => None,
            })
            .next_back()
    };
    assert_eq!(set_first_key(&mut t, 5), 5);
    assert_eq!(roles(&t.events()), Some(vec![p::Role::Keyboard as i32]));
    // Restoring the key before anything is written reports the roles again.
    assert_eq!(set_first_key(&mut t, 4), 5);
    assert_eq!(roles(&t.events()), Some(vec![]));
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert!(profile(&mut t, id).roles.is_empty());
}

#[test]
fn an_editor_moving_on_does_not_edit_once_saving_leaves_storage_not_ready() {
    let mut t = Test::new(true);
    let (id, _) = editing(&mut t);
    let other = create(&mut t, "Other");
    assert_eq!(set_first_key(&mut t, 5), 5);
    move_via(&mut t, other);
    t.store.unknown = Some(record_key(9, id));
    assert_eq!(set_first_key(&mut t, 6), 0xff);
    assert_eq!(rules_writes(&t, id), 1);
    assert!(!t.status().ready);
    // The other profile was not loaded for editing, so its rules are unchanged.
    assert!(t.app.manager.profiles.get(other.into()).is_none());
}

#[test]
fn an_editor_moving_on_without_memory_writes_its_edits_from_the_table() {
    let mut t = Test::new(true);
    let id = editing_alone(&mut t);
    let other = create(&mut t, "Other");
    assert_eq!(set_first_key(&mut t, 5), 5);
    move_via(&mut t, other);
    // The edits cannot be listed, so they are written from the editor before it moves on.
    fail_next_allocation();
    assert_eq!(set_first_key(&mut t, 6), 5);
    assert!(allocation_failed());
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
    assert_eq!(
        rules_of(&mut t, other),
        [saved_remap(KEYBOARD_COLLECTION, key(4), &[6])]
    );
    // The roles summary follows the rules written.
    t.now += cordial_core::application::EDITOR_BATCH_MS;
    t.events();
    assert_eq!(profile(&mut t, id).roles, [p::Role::Keyboard as i32]);
    assert_eq!(
        block_on(cordial_core::profiles::metadata(&mut t.store, id.into()))
            .unwrap()
            .roles
            .0,
        cordial_core::hid::KEYBOARD
    );
}

#[test]
fn a_full_filesystem_for_edits_written_from_the_editor_is_reported() {
    let mut t = Test::new(true);
    let id = editing_alone(&mut t);
    assert_eq!(set_first_key(&mut t, 5), 5);
    let rules = record_key(9, id);
    t.store.full = Some(rules);
    fail_next_allocation();
    block_on(t.app.save_all(&mut t.store, t.now));
    assert!(allocation_failed());
    let full = |s: &p::Status| {
        s.errors
            .iter()
            .any(|e| e.code == p::ErrorCode::StorageFull as i32)
    };
    let status = t.status();
    assert!(status.ready && full(&status));
    t.store.full = None;
    block_on(t.app.save_all(&mut t.store, t.now));
    assert!(!full(&t.status()));
    assert_eq!(
        saved_rules(&mut t, id),
        [remap_rule(KEYBOARD_COLLECTION, key(4), &[5])]
    );
}

fn output(mode: p::OutputMode) -> p::OutputPreference {
    p::OutputPreference {
        role: p::Role::Gamepad as i32,
        mode: mode as i32,
    }
}

#[test]
fn output_planner_allocation_failure_has_no_enabled_device_capacity_reason() {
    use cordial_core::outputs::Support;
    let mut t = Test::new(true);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid],
    }];
    let original = t.saved(SAVED);
    let request = p::Request {
        command: Some(Command::SetDevice(p::SetDevice {
            device: SAVED,
            output_preferences: vec![output(p::OutputMode::Hid)],
            ..Default::default()
        })),
    };
    t.store.writes.clear();
    t.app.manager.outputs = Default::default();
    FAIL_LAYOUT.with(|f| {
        f.set(Some(
            std::alloc::Layout::array::<cordial_core::outputs::Configured>(4).unwrap(),
        ));
    });
    block_on(t.app.dispatch(request, &mut t.store, &mut t.radio, t.now));
    assert!(FAIL_LAYOUT.with(|f| f.get()).is_none());
    let Some(R::Error(error)) = t.response().result else {
        panic!("output selection succeeded without planner memory");
    };
    assert_eq!(error.code, p::ErrorCode::NoCapacity as i32);
    assert_eq!(error.reason, p::ErrorReason::Unknown as i32);
    assert!(t.store.writes.is_empty());
    assert_eq!(t.saved(SAVED), original);
    assert_eq!(t.app.manager.outputs.plan(), Default::default());
}

#[test]
fn a_lost_record_awaiting_cleanup_is_not_available_to_device_commands() {
    let mut t = nonresident_gamepad_output();
    t.set_device(p::SetDevice {
        device: SAVED,
        enabled: Some(true),
        ..Default::default()
    });
    let key = record_key(2, SAVED);
    let mut record = storage::Device::decode(t.store.records[&key].as_slice()).unwrap();
    record.bond.as_mut().unwrap().owner += 1;
    t.store.records.insert(key, record.encode_to_vec());
    let slot = t.app.manager.find(SAVED.into()).unwrap();
    t.store.fail_remove = Some(key);
    assert_eq!(
        block_on(t.app.manager.import(slot, &mut t.store, &mut t.radio)),
        Err((ErrorCode::StorageFailed, p::ErrorReason::Unknown))
    );
    assert_eq!(t.app.manager.lost_devices, [u64::from(SAVED)]);
    for command in [
        Command::GetDevice(p::GetDevice { device: SAVED }),
        Command::SetDevice(p::SetDevice {
            device: SAVED,
            trusted: Some(false),
            ..Default::default()
        }),
    ] {
        assert_eq!(t.code(command), p::ErrorCode::NotFound);
        assert!(t.app.manager.find(SAVED.into()).is_none());
        assert_eq!(t.app.manager.outputs.plan(), Default::default());
    }
}

#[test]
fn output_selections_require_advertised_support_and_exact_enum_values() {
    let mut t = Test::new(true);
    assert!(t.status().output_support.is_empty());
    let original = t.saved(SAVED);
    for mode in [p::OutputMode::Hid, p::OutputMode::Xinput] {
        assert_eq!(
            t.code(Command::SetDevice(p::SetDevice {
                device: SAVED,
                output_preferences: vec![output(mode)],
                ..Default::default()
            })),
            p::ErrorCode::Unsupported
        );
    }
    for update in [
        p::OutputPreference { role: 261, mode: 1 },
        p::OutputPreference {
            role: p::Role::Gamepad as i32,
            mode: 257,
        },
        p::OutputPreference {
            role: p::Role::Gamepad as i32,
            mode: -1,
        },
        p::OutputPreference {
            role: p::Role::Unknown as i32,
            mode: 0,
        },
    ] {
        assert_eq!(
            t.code(Command::SetDevice(p::SetDevice {
                device: SAVED,
                output_preferences: vec![update],
                ..Default::default()
            })),
            p::ErrorCode::BadArgs
        );
    }
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED,
            output_preferences: vec![p::OutputPreference {
                role: p::Role::Keyboard as i32,
                mode: 0
            }],
            ..Default::default()
        })),
        p::ErrorCode::Unsupported
    );
    t.set_device(p::SetDevice {
        device: SAVED,
        output_preferences: vec![output(p::OutputMode::Disabled)],
        ..Default::default()
    });
    assert_eq!(t.saved(SAVED), original);
}

#[test]
fn output_updates_save_once_in_order_and_failed_saves_leave_runtime_unchanged() {
    use cordial_core::{
        outputs::{Plan, Support},
        storage::record_key,
    };
    let mut t = Test::new(true);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[
            p::OutputMode::Disabled,
            p::OutputMode::Hid,
            p::OutputMode::Xinput,
        ],
    }];
    let advertised = t.status().output_support;
    assert_eq!(advertised.len(), 1);
    assert!(
        advertised[0]
            .modes
            .iter()
            .all(|m| m.resources[0].count.is_none() || m.mode == p::OutputMode::Disabled as i32)
    );
    let key = record_key(2, SAVED.into());
    t.store.writes.clear();
    let d = t.set_device(p::SetDevice {
        device: SAVED,
        output_preferences: vec![
            output(p::OutputMode::Hid),
            output(p::OutputMode::Disabled),
            output(p::OutputMode::Xinput),
        ],
        ..Default::default()
    });
    assert_eq!(d.output_preferences, [output(p::OutputMode::Xinput)]);
    assert_eq!(
        d.outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Disconnected as i32
        ))
    );
    assert_eq!(d.outputs[0].slot, None);
    assert_eq!(t.store.writes.iter().filter(|&&k| k == key).count(), 1);
    assert_eq!(
        t.app.manager.outputs.plan(),
        Plan {
            hid_gamepads: 0,
            xinput_gamepads: 1
        }
    );
    let before = t.saved(SAVED);
    let writes = t.store.writes.len();
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED,
            output_preferences: vec![
                output(p::OutputMode::Hid),
                p::OutputPreference {
                    role: p::Role::Gamepad as i32,
                    mode: 257,
                }
            ],
            ..Default::default()
        })),
        p::ErrorCode::BadArgs
    );
    assert_eq!(t.store.writes.len(), writes);
    assert_eq!(t.saved(SAVED), before);
    t.store.fail_save = Some((key, false));
    assert_eq!(
        t.code(Command::SetDevice(p::SetDevice {
            device: SAVED,
            output_preferences: vec![output(p::OutputMode::Hid)],
            ..Default::default()
        })),
        p::ErrorCode::StorageFailed
    );
    t.store.fail_save = None;
    assert_eq!(t.saved(SAVED), before);
    assert_eq!(
        t.device(SAVED).output_preferences,
        [output(p::OutputMode::Xinput)]
    );
    assert_eq!(
        t.app.manager.outputs.plan(),
        Plan {
            hid_gamepads: 0,
            xinput_gamepads: 1
        }
    );
    let connected = connect_saved(&mut t, descriptor());
    let d = t.device(SAVED);
    assert_eq!(d.state, p::DeviceState::Connected as i32);
    assert_eq!(
        d.outputs[0].status,
        Some(p::output_status::Status::Error(p::Error {
            code: p::ErrorCode::Unsupported as i32,
            reason: p::ErrorReason::InputLayout as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }))
    );
    assert_eq!(d.outputs[0].slot, None);
    assert!(t.app.manager.connection(connected).is_some());
    t.set_device(p::SetDevice {
        device: SAVED,
        output_preferences: vec![output(p::OutputMode::Disabled)],
        ..Default::default()
    });
    assert!(t.saved(SAVED).output_preferences.is_empty());
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
}

#[test]
fn disabling_a_connected_device_keeps_the_committed_policy_until_disconnect() {
    use cordial_core::outputs::{Plan, Support};
    let mut t = Test::new(true);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid],
    }];
    t.set_device(p::SetDevice {
        device: SAVED,
        output_preferences: vec![output(p::OutputMode::Hid)],
        ..Default::default()
    });
    let profile = create(&mut t, "Keys");
    let link = connect_saved(&mut t, hidpp_descriptor());
    t.event(Event::Input(
        InputReport::new(link, ServiceId(7), 1, &[1]).unwrap(),
    ));
    t.events();
    assert!(t.live(SAVED).policy.is_some());
    let device = t.set_device(p::SetDevice {
        device: SAVED,
        enabled: Some(false),
        trusted: Some(false),
        integrations: vec![p::IntegrationUpdate {
            kind: p::IntegrationKind::Hidpp as i32,
            enabled: Some(false),
        }],
        profiles: Some(p::ProfileLayers {
            profiles: vec![profile],
        }),
        output_preferences: vec![output(p::OutputMode::Disabled)],
        ..Default::default()
    });
    assert!(!device.enabled && !device.trusted && !hidpp(&device).enabled);
    assert_eq!(
        device.profiles,
        Some(p::ProfileLayers {
            profiles: vec![profile],
        })
    );
    assert!(device.output_preferences.is_empty());
    assert_eq!(device.state, p::DeviceState::Disconnecting as i32);
    let saved = t.saved(SAVED);
    assert_eq!(t.live(SAVED).policy.as_ref(), Some(&saved));
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    assert!(t.app.manager.connection(link).unwrap().closing);
    assert_eq!(t.radio.closes, [link]);

    t.set_device(p::SetDevice {
        device: SAVED,
        blocked: Some(true),
        ..Default::default()
    });
    let saved = t.saved(SAVED);
    assert!(saved.blocked && !saved.enabled && !saved.trusted && !saved.hidpp_enabled());
    assert_eq!(saved.profiles, [u64::from(profile)]);
    assert!(saved.output_preferences.is_empty());
    assert_eq!(t.live(SAVED).policy.as_ref(), Some(&saved));
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    assert_eq!(t.radio.closes, [link]);
}

#[test]
fn get_device_cleans_up_a_missing_nonresident_output_selection() {
    use cordial_core::outputs::{Plan, Support};
    let mut t = Test::new(true);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Xinput],
    }];
    t.set_device(p::SetDevice {
        device: SAVED,
        enabled: Some(false),
        output_preferences: vec![output(p::OutputMode::Xinput)],
        ..Default::default()
    });
    t.events();
    assert!(t.app.manager.find(SAVED.into()).is_none());
    let expected = Plan {
        hid_gamepads: 0,
        xinput_gamepads: 1,
    };
    assert_eq!(t.app.manager.outputs.plan(), expected);
    let key = record_key(2, SAVED);
    t.store.fail_load = Some(key);
    assert_eq!(
        t.code(Command::GetDevice(p::GetDevice { device: SAVED })),
        p::ErrorCode::StorageFailed
    );
    assert_eq!(t.app.manager.outputs.plan(), expected);
    assert!(t.store.records.contains_key(&key));
    assert!(!t.events().iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
    t.store.fail_load = None;

    t.store.records.remove(&key);
    assert_eq!(
        t.code(Command::GetDevice(p::GetDevice { device: SAVED })),
        p::ErrorCode::NotFound
    );
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    assert!(
        t.events()
            .contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED }))
    );
    for id in [0, SAVED, 999] {
        assert_eq!(
            t.code(Command::GetDevice(p::GetDevice { device: id })),
            p::ErrorCode::NotFound
        );
    }
    assert!(!t.events().iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
}

#[test]
fn profile_cleanup_removes_lost_nonresident_output_selections() {
    use cordial_core::outputs::{Plan, Support};
    for missing in [true, false] {
        let mut t = Test::new(true);
        t.app.manager.output_support = &[Support {
            role: p::Role::Gamepad,
            modes: &[p::OutputMode::Hid],
        }];
        let lost = create(&mut t, "Lost");
        t.set_device(p::SetDevice {
            device: SAVED,
            enabled: Some(false),
            profiles: Some(p::ProfileLayers {
                profiles: vec![lost],
            }),
            output_preferences: vec![output(p::OutputMode::Hid)],
            ..Default::default()
        });
        t.events();
        assert!(t.app.manager.find(SAVED.into()).is_none());
        assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
        lose_profile(&mut t, lost);
        for _ in 0..2 {
            assert!(block_on(t.app.work(&mut t.store, &mut t.radio, t.now)));
        }
        // The scan has read its page of IDs, but not the device record.
        let key = record_key(2, SAVED);
        if missing {
            t.store.records.remove(&key);
        } else {
            t.store.records.insert(key, UNDECODABLE.to_vec());
        }
        let events = t.events();
        assert_eq!(t.app.manager.outputs.plan(), Plan::default());
        assert!(!t.store.records.contains_key(&key));
        assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED })));
        assert!(events.contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: lost })));
    }
}

fn nonresident_gamepad_output() -> Test {
    use cordial_core::outputs::Support;
    let mut t = Test::new(true);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid],
    }];
    t.set_device(p::SetDevice {
        device: SAVED,
        enabled: Some(false),
        output_preferences: vec![output(p::OutputMode::Hid)],
        ..Default::default()
    });
    t.events();
    assert!(t.app.manager.find(SAVED.into()).is_none());
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
    t
}

/// Starts a command and reads its first page of device IDs, before reading any device record.
fn begin_device_scan(t: &mut Test, command: Command) {
    t.store.reads.clear();
    block_on(t.app.begin(
        p::Request {
            command: Some(command),
        },
        &mut t.store,
        &mut t.radio,
        t.now,
    ));
    assert!(t.app.command_pending());
    block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
    assert!(t.app.command_pending());
    assert!(!t.store.reads.contains(&record_key(2, SAVED)));
}

fn finish_device_scan(t: &mut Test) {
    while t.app.command_pending() {
        block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
    }
    assert_eq!(t.response().result, None);
}

#[test]
fn deleting_a_profile_cleans_up_lost_device_output_selections() {
    use cordial_core::outputs::Plan;
    for missing in [true, false] {
        let mut t = nonresident_gamepad_output();
        let profile = create(&mut t, "Unused");
        t.events();
        let command = Command::DeleteProfile(p::DeleteProfile { profile });
        let key = record_key(2, SAVED);
        t.store.fail_load = Some(key);
        assert_eq!(t.code(command.clone()), p::ErrorCode::StorageFailed);
        assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
        assert!(t.store.records.contains_key(&record_key(8, profile)));
        assert!(!t.events().iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
        t.store.fail_load = None;

        begin_device_scan(&mut t, command);
        if missing {
            t.store.records.remove(&key);
        } else {
            t.store.records.insert(key, UNDECODABLE.to_vec());
        }
        finish_device_scan(&mut t);
        assert_eq!(t.app.manager.outputs.plan(), Plan::default());
        assert!(!t.store.records.contains_key(&key));
        assert!(!t.store.records.contains_key(&record_key(8, profile)));
        assert!(
            t.events()
                .contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED }))
        );
    }
}

#[test]
fn starting_pairing_cleans_up_lost_device_output_selections() {
    use cordial_core::outputs::Plan;
    for missing in [true, false] {
        let mut t = nonresident_gamepad_output();
        let candidate = t.candidate(peer(2), peer(2));
        let command = Command::StartPairing(p::StartPairing { candidate });
        let key = record_key(2, SAVED);
        t.store.fail_load = Some(key);
        assert_eq!(t.code(command.clone()), p::ErrorCode::StorageFailed);
        assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
        assert!(!t.events().iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
        t.store.fail_load = None;

        begin_device_scan(&mut t, command);
        if missing {
            t.store.records.remove(&key);
        } else {
            t.store.records.insert(key, UNDECODABLE.to_vec());
        }
        finish_device_scan(&mut t);
        assert_eq!(t.app.manager.outputs.plan(), Plan::default());
        assert!(!t.store.records.contains_key(&key));
        assert!(
            t.events()
                .contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED }))
        );
    }
}

#[test]
fn matching_a_bond_cleans_up_lost_device_output_selections() {
    use cordial_core::outputs::Plan;
    for direct in [true, false] {
        for missing in [true, false] {
            let mut t = nonresident_gamepad_output();
            if direct {
                t.set_device(p::SetDevice {
                    device: SAVED,
                    enabled: Some(true),
                    ..Default::default()
                });
                t.entry(SAVED).paused = true;
            }
            let identity = peer(if direct { 1 } else { 2 });
            let candidate = t.candidate(identity, identity);
            t.pair(candidate);
            t.poll();
            let link = t.radio.connects.last().unwrap().0;
            t.events();
            t.radio.forgotten.clear();
            t.radio.bonds.push(identity);
            t.event(Event::Bonded { link, identity });
            let key = record_key(2, SAVED);
            if !direct {
                // The first match step reads the scan's page of IDs.
                block_on(t.app.work(&mut t.store, &mut t.radio, t.now));
            }
            if missing {
                t.store.records.remove(&key);
            } else {
                t.store.records.insert(key, UNDECODABLE.to_vec());
            }
            let events = t.events();
            assert_eq!(t.app.manager.outputs.plan(), Plan::default());
            assert!(!t.store.records.contains_key(&key));
            assert!(t.radio.bonds.contains(&identity));
            assert!(!t.radio.forgotten.contains(&identity));
            assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED })));
            assert!(matches!(
                pairing_steps(&events).last(),
                Some(p::pairing::Step::Done(_))
            ));
        }
    }
}

#[test]
fn ble_repair_preserves_the_new_bond_when_the_resident_record_is_lost() {
    use cordial_core::outputs::Plan;
    for missing in [true, false] {
        let mut t = nonresident_gamepad_output();
        t.set_device(p::SetDevice {
            device: SAVED,
            enabled: Some(true),
            ..Default::default()
        });
        t.entry(SAVED).paused = true;
        let identity = saved_ble(&mut t);
        let rpa = Peer {
            transport: Transport::Ble,
            random: true,
            ..peer(2)
        };
        let candidate = t.candidate(rpa, rpa);
        t.pair(candidate);
        t.poll();
        let link = t.radio.connects.last().unwrap().0;
        assert_eq!(t.app.manager.connection(link).unwrap().peer, rpa);
        t.events();
        t.radio.forgotten.clear();
        t.radio.bonds.push(identity);
        t.event(Event::Bonded { link, identity });
        let key = record_key(2, SAVED);
        if missing {
            t.store.records.remove(&key);
        } else {
            t.store.records.insert(key, UNDECODABLE.to_vec());
        }
        let events = t.events();
        assert!(t.radio.bonds.contains(&identity));
        assert!(!t.radio.forgotten.contains(&identity));
        assert_eq!(t.app.manager.outputs.plan(), Plan::default());
        assert!(!t.store.records.contains_key(&key));
        assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED })));
        let steps = pairing_steps(&events);
        let Some(p::pairing::Step::Done(done)) = steps.last() else {
            panic!("pairing did not finish: {steps:?}");
        };
        assert_ne!(done.device, SAVED);
        assert_eq!(t.saved(done.device).peer, identity);
        assert!(!t.app.manager.connection(link).unwrap().closing);
    }
}

#[test]
fn lost_device_cleanup_retries_a_failed_native_bond_removal() {
    use cordial_core::outputs::Plan;
    let mut t = nonresident_gamepad_output();
    t.set_device(p::SetDevice {
        device: SAVED,
        enabled: Some(true),
        ..Default::default()
    });
    t.entry(SAVED).paused = true;
    t.events();
    assert!(!t.app.manager.bonds_pending);
    assert!(t.app.manager.connections.iter().all(Option::is_none));
    let key = record_key(2, SAVED);
    t.store.records.insert(key, UNDECODABLE.to_vec());
    t.radio.reject_forget = true;
    assert_eq!(
        t.code(Command::GetDevice(p::GetDevice { device: SAVED })),
        p::ErrorCode::NotFound
    );
    assert!(t.app.manager.find(SAVED.into()).is_none());
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    assert!(!t.store.records.contains_key(&key));
    assert!(t.radio.bonds.contains(&peer(1)));
    assert!(
        t.events()
            .contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED }))
    );
    assert!(t.app.manager.lost_devices.is_empty());
    assert!(t.radio.bonds.contains(&peer(1)));

    t.radio.reject_forget = false;
    t.now += FIRST_RETRY;
    let events = t.events();
    assert!(!t.radio.bonds.contains(&peer(1)));
    assert!(t.radio.forgotten.contains(&peer(1)));
    assert!(!events.iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
    assert!(!t.app.manager.bonds_pending);
}

#[test]
fn profile_cleanup_removes_a_nonresident_device_with_another_devices_bond() {
    use cordial_core::outputs::Plan;
    let mut t = nonresident_gamepad_output();
    let profile = create(&mut t, "Lost");
    set_layers(&mut t, SAVED, &[profile]);
    t.events();
    let key = record_key(2, SAVED);
    let mut record = storage::Device::decode(t.store.records[&key].as_slice()).unwrap();
    record.bond.as_mut().unwrap().owner += 1;
    t.store.records.insert(key, record.encode_to_vec());
    lose_profile(&mut t, profile);
    let events = t.events();
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    assert!(!t.store.records.contains_key(&key));
    assert!(!t.store.records.contains_key(&record_key(8, profile)));
    assert!(events.contains(&Ev::DeviceRemoved(p::DeviceRemoved { id: SAVED })));
    assert!(events.contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: profile })));
}

#[test]
fn matching_a_bond_keeps_output_selections_on_read_failure() {
    for direct in [true, false] {
        let mut t = nonresident_gamepad_output();
        let identity = peer(if direct { 1 } else { 2 });
        let candidate = t.candidate(identity, identity);
        t.pair(candidate);
        t.poll();
        let link = t.radio.connects.last().unwrap().0;
        t.events();
        t.store.fail_load = Some(record_key(2, SAVED));
        t.radio.bonds.push(identity);
        t.event(Event::Bonded { link, identity });
        let mut events = t.events();
        t.finish_disconnects();
        events.extend(t.events());
        assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
        assert!(!events.iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
        assert_eq!(
            pairing_steps(&events).last(),
            Some(&p::pairing::Step::Error(p::Error {
                code: p::ErrorCode::StorageFailed as i32,
                reason: p::ErrorReason::Storage as i32,
                outcome: p::ErrorOutcome::NoEffect as i32,
                ..Default::default()
            }))
        );
    }
}

#[test]
fn profile_cleanup_finishes_when_lost_device_cleanup_fails() {
    use cordial_core::outputs::Plan;
    let mut t = nonresident_gamepad_output();
    let profile = create(&mut t, "Lost");
    t.events();
    lose_profile(&mut t, profile);
    let key = record_key(2, SAVED);
    t.store.records.insert(key, UNDECODABLE.to_vec());
    t.store.fail_remove = Some(key);
    let events = t.events();
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    assert!(t.store.records.contains_key(&key));
    assert!(!t.store.records.contains_key(&record_key(8, profile)));
    assert!(t.app.manager.lost_profiles.is_empty());
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Ev::DeviceRemoved(_)))
            .count(),
        1
    );
    assert!(events.contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: profile })));
    for _ in 0..3 {
        t.now += FIRST_RETRY * 4;
        assert!(!t.events().iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
        assert!(t.app.manager.lost_profiles.is_empty());
    }
}

#[test]
fn profile_cleanup_keeps_output_selections_on_device_read_failure() {
    let mut t = nonresident_gamepad_output();
    let profile = create(&mut t, "Lost");
    t.events();
    lose_profile(&mut t, profile);
    t.store.fail_load = Some(record_key(2, SAVED));
    let events = t.events();
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
    assert!(!events.iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
    assert_eq!(t.app.manager.lost_profiles, [u64::from(profile)]);
    assert!(t.store.records.contains_key(&record_key(8, profile)));
    t.store.fail_load = None;
    t.now += FIRST_RETRY;
    assert!(
        t.events()
            .contains(&Ev::ProfileRemoved(p::ProfileRemoved { id: profile }))
    );
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
}

#[test]
fn deferred_device_roles_clean_up_lost_output_selections() {
    use cordial_core::outputs::Plan;
    for missing in [true, false] {
        let mut t = nonresident_gamepad_output();
        t.set_device(p::SetDevice {
            device: SAVED,
            enabled: Some(true),
            ..Default::default()
        });
        let link = connect_saved(&mut t, descriptor());
        press(&mut t, link, &[4]);
        t.poll();
        assert!(t.live(SAVED).policy.is_some());
        assert!(t.app.has_unsaved());
        assert_eq!(t.saved(SAVED).roles, cordial_core::devices::Roles(0));
        let key = record_key(2, SAVED);
        t.store.fail_load = Some(key);
        block_on(t.app.save_all(&mut t.store, t.now));
        assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
        assert!(t.app.manager.lost_devices.is_empty());
        assert!(t.app.has_unsaved());
        assert!(!t.events().iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
        t.store.fail_load = None;

        if missing {
            t.store.records.remove(&key);
        } else {
            t.store.records.insert(key, UNDECODABLE.to_vec());
        }
        t.store.fail_remove = Some(key);
        block_on(t.app.save_all(&mut t.store, t.now));
        assert_eq!(t.app.manager.lost_devices, [u64::from(SAVED)]);
        t.now += cordial_core::deferred::QUIET_MS;
        let events = t.events();
        assert_eq!(t.app.manager.outputs.plan(), Plan::default());
        assert!(t.app.manager.find(SAVED.into()).is_none());
        assert!(t.app.manager.connection(link).unwrap().closing);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Ev::DeviceRemoved(_)))
                .count(),
            1
        );
        t.now += FIRST_RETRY;
        assert!(!t.events().iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
        t.store.fail_remove = None;
        t.now += FIRST_RETRY * 2;
        t.events();
        assert!(t.app.manager.lost_devices.is_empty());
        assert!(!t.store.records.contains_key(&key));
    }
}

#[test]
fn gamepad_enumeration_reports_capacity_and_device_state_changes() {
    use cordial_core::{
        bluetooth::Descriptor,
        outputs::{Enumeration, Plan, Support},
    };
    let mut t = Test::new(true);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[
            p::OutputMode::Disabled,
            p::OutputMode::Hid,
            p::OutputMode::Xinput,
        ],
    }];
    t.set_device(p::SetDevice {
        device: SAVED,
        output_preferences: vec![output(p::OutputMode::Hid)],
        ..Default::default()
    });
    let descriptor = Descriptor::from_slice(
        ServiceId(8),
        &[
            5, 1, 9, 5, 0xa1, 1, 5, 9, 0x19, 1, 0x29, 8, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8, 0x81,
            2, 0xc0,
        ],
    )
    .unwrap();
    let link = connect_saved(&mut t, vec![descriptor]);
    t.event(Event::Input(
        InputReport::new(link, ServiceId(8), 0, &[0]).unwrap(),
    ));
    t.events();
    let before = t.device(SAVED);
    assert_eq!(
        before.outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Pending as i32
        ))
    );
    let status_before = t.status();
    t.events();
    let mut enumeration = Enumeration {
        plan: Plan {
            hid_gamepads: 1,
            xinput_gamepads: 2,
        },
        generation: 4,
        ready: true,
    };
    t.app.set_output_enumeration(Some(enumeration));
    let events = t.events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ev::Device(d) if d.id == SAVED
        && d.outputs[0].status == Some(p::output_status::Status::State(p::OutputState::Active as i32)) && d.outputs[0].slot == Some(0)))
    );
    assert!(events.iter().any(|e| matches!(e, Ev::Adapter(_))));
    let mut active = t.device(SAVED);
    active.outputs.clone_from(&before.outputs);
    assert_eq!(active, before);
    let mut status = t.status();
    assert_eq!(
        status.output_support[0].modes[0].resources[0].count,
        Some(0)
    );
    assert_eq!(
        status.output_support[0].modes[1].resources[0].count,
        Some(1)
    );
    assert_eq!(
        status.output_support[0].modes[2].resources[0].count,
        Some(2)
    );
    status.output_support = status_before.output_support.clone();
    assert_eq!(status, status_before);
    let held = t.app.gamepad_outputs();
    enumeration.ready = false;
    t.app.set_output_enumeration(Some(enumeration));
    assert_eq!(t.app.gamepad_outputs(), held);
    assert!(t.events().iter().any(|e| matches!(e, Ev::Device(d)
        if d.outputs[0].status == Some(p::output_status::Status::State(p::OutputState::Pending as i32)) && d.outputs[0].slot.is_none())));
    enumeration.ready = true;
    t.app.set_output_enumeration(Some(enumeration));
    assert!(t.events().iter().any(|e| matches!(e, Ev::Device(d)
        if d.outputs[0].status == Some(p::output_status::Status::State(p::OutputState::Active as i32)))));
    t.app.set_output_enumeration(Some(enumeration));
    assert!(
        !t.events()
            .iter()
            .any(|e| matches!(e, Ev::Adapter(_) | Ev::Device(_)))
    );
    t.settle();
    t.app.session(false, &mut t.radio);
    ALLOCS.set(0);
    TRACK.set(true);
    for buttons in 0..200 {
        t.event(Event::Input(
            InputReport::new(link, ServiceId(8), 0, &[buttons]).unwrap(),
        ));
        assert_eq!(
            t.app.gamepad_outputs().hid[0].gamepad.buttons,
            u32::from(buttons)
        );
        t.poll();
    }
    TRACK.set(false);
    assert_eq!(ALLOCS.get(), 0);
    t.app.session(true, &mut t.radio);
    t.app.set_output_enumeration(None);
    assert!(
        t.app
            .gamepad_outputs()
            .hid
            .iter()
            .all(|s| s.occupant.is_none())
    );
    assert!(
        t.status().output_support[0]
            .modes
            .iter()
            .all(|m| m.resources[0].count.is_none() || m.mode == p::OutputMode::Disabled as i32)
    );
}

#[test]
fn gamepad_profile_capabilities_follow_the_board_output_support() {
    use cordial_core::{outputs::Support, profiles};
    let gamepad = usage(1, 5);
    let mut t = Test::new(false);
    let unsupported = t.status().profile_support.unwrap();
    assert!(unsupported.ranges.iter().all(|r| {
        r.kind != p::usage_range::Kind::AxisInput as i32
            && r.kind != p::usage_range::Kind::AxisOutput as i32
    }));
    assert!(
        unsupported
            .ranges
            .iter()
            .all(|range| range.collection.is_some())
    );
    assert!(
        unsupported
            .ranges
            .iter()
            .filter(|r| r.kind == p::usage_range::Kind::RemapOutput as i32)
            .all(|range| range.collection != Some(gamepad))
    );
    let id = create(&mut t, "Controls");
    let route = rule(
        gamepad,
        usage(1, 0x30),
        p::profile_rule::Effect::Axis(p::profile_rule::Axis {
            destination: Some(usage(1, 0x31)),
            invert: true,
        }),
    );
    assert_eq!(
        t.code(Command::SetProfileRules(p::SetProfileRules {
            profile: id,
            changes: vec![route.clone()]
        })),
        p::ErrorCode::BadArgs
    );
    // Gamepad input can produce keyboard output without a gamepad USB backend.
    set_rules(&mut t, id, vec![remap(gamepad, usage(9, 1), &[key(5)])]);
    assert_eq!(profile(&mut t, id).roles, [p::Role::Gamepad as i32]);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid, p::OutputMode::Xinput],
    }];
    let supported = t.status().profile_support.unwrap();
    let axes = |kind: p::usage_range::Kind| {
        supported
            .ranges
            .iter()
            .filter(|r| r.kind == kind as i32)
            .map(|r| (r.collection, r.usage_page, r.min, r.max))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        axes(p::usage_range::Kind::AxisInput).len(),
        profiles::AXIS_INPUTS.len()
    );
    assert_eq!(
        axes(p::usage_range::Kind::AxisInput),
        axes(p::usage_range::Kind::AxisOutput)
    );
    assert!(
        supported
            .ranges
            .iter()
            .filter(|r| r.kind == p::usage_range::Kind::RemapOutput as i32)
            .any(|range| range.collection == Some(gamepad))
    );
    let listed = set_rules(&mut t, id, vec![route.clone()]);
    assert_eq!(listed[0].collection, Some(gamepad));
    assert!(matches!(
        listed[0].effect,
        Some(p::profile_rule::Effect::Axis(p::profile_rule::Axis {
            invert: true,
            ..
        }))
    ));
    assert_eq!(
        saved_rules(&mut t, id)[0].effect,
        profiles::Effect::Axis(profiles::usage(1, 0x31), true)
    );
    // The same button usage has two possible USB destinations on this board.
    assert_eq!(
        t.code(Command::SetProfileRules(p::SetProfileRules {
            profile: id,
            changes: vec![remap(KEYBOARD_COLLECTION, key(4), &[usage(9, 1)])]
        })),
        p::ErrorCode::BadArgs
    );
    let explicit = rule(
        KEYBOARD_COLLECTION,
        key(4),
        p::profile_rule::Effect::Remap(p::profile_rule::Remap {
            outputs: vec![p::profile_rule::Output {
                usage: Some(usage(9, 1)),
                collection: Some(gamepad),
            }],
        }),
    );
    set_rules(&mut t, id, vec![explicit]);
    t.app.manager.profile_budget = None;
    assert!(t.status().profile_support.is_none());
}

#[test]
fn rule_commands_require_collections_and_keep_same_usage_rules_separate() {
    let mut t = Test::new(false);
    let id = create(&mut t, "Buttons");
    let gamepad = usage(1, 5);
    let button = usage(9, 1);
    set_rules(
        &mut t,
        id,
        vec![
            remap(gamepad, button, &[key(5)]),
            remap(MOUSE_COLLECTION, button, &[key(4)]),
        ],
    );
    let listed = rules_of(&mut t, id);
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].collection, Some(MOUSE_COLLECTION));
    assert_eq!(listed[1].collection, Some(gamepad));
    for collection in [None, Some(usage(0, 0))] {
        let change = p::ProfileRuleChange {
            change: Some(p::profile_rule_change::Change::Rule(p::ProfileRule {
                input: Some(button),
                collection,
                effect: Some(p::profile_rule::Effect::Remap(p::profile_rule::Remap {
                    outputs: vec![],
                })),
            })),
        };
        assert_eq!(
            t.code(Command::SetProfileRules(p::SetProfileRules {
                profile: id,
                changes: vec![change]
            })),
            p::ErrorCode::BadArgs
        );
        let reference = p::ProfileRuleRef {
            input: Some(button),
            collection,
        };
        assert_eq!(
            t.code(Command::SetProfileRules(p::SetProfileRules {
                profile: id,
                changes: vec![p::ProfileRuleChange {
                    change: Some(p::profile_rule_change::Change::Forget(reference))
                }]
            })),
            p::ErrorCode::BadArgs
        );
    }
    assert_eq!(rules_of(&mut t, id), listed);
    assert_eq!(
        set_rules(&mut t, id, vec![forget_rule(MOUSE_COLLECTION, button)]),
        listed[1..]
    );
}

#[test]
fn configuration_editors_preserve_unrelated_rules_on_writes_and_reset_the_whole_profile() {
    for (interface_kind, wire_kind) in [
        (Interface::Via, p::ConfigurationInterface::Via),
        (Interface::Vial, p::ConfigurationInterface::Vial),
    ] {
        let mut t = Test::new(true);
        let id = create(&mut t, "Mixed");
        let gamepad = usage(1, 5);
        t.app.manager.output_support = &[cordial_core::outputs::Support {
            role: p::Role::Gamepad,
            modes: &[p::OutputMode::Hid],
        }];
        let route = rule(
            gamepad,
            usage(1, 0x30),
            p::profile_rule::Effect::Axis(p::profile_rule::Axis {
                destination: Some(usage(1, 0x31)),
                invert: false,
            }),
        );
        let original = set_rules(
            &mut t,
            id,
            vec![
                route,
                remap(gamepad, usage(9, 1), &[key(5)]),
                scale(MOUSE_COLLECTION, usage(1, 0x38), -1, 1),
            ],
        );
        let other = profile_with(&mut t, "Other", [4, 5]);
        let other_rules = rules_of(&mut t, other);
        set_interfaces(&mut t, vec![interface(wire_kind, Some(true), Some(id))]);
        // Matrix position 195 names Mouse Button 1, even when a gamepad uses the same usage.
        assert_eq!(
            configure(&mut t, interface_kind, &[5, 0, 12, 3, 0, 4])[0],
            5
        );
        let listed = rules_of(&mut t, id);
        assert_eq!(listed.len(), original.len() + 1);
        assert!(original.iter().all(|rule| listed.contains(rule)));
        // A buffer write of Mouse Button 1's own keycode forgets only its mouse rule.
        let own = match interface_kind {
            Interface::Via => 0xf4,
            Interface::Vial => 0xd1,
        };
        assert_eq!(
            configure(&mut t, interface_kind, &[0x13, 1, 0x86, 2, 0, own])[0],
            0x13
        );
        assert_eq!(rules_of(&mut t, id), original);
        assert_eq!(configure(&mut t, interface_kind, &[5, 0, 0, 0, 0, 5])[0], 5);
        set_layers(&mut t, SAVED, &[id]);
        let link = connect_saved(&mut t, descriptor());
        t.events();
        assert_eq!(press(&mut t, link, &[4]), [5]);
        let before_reset = rules_of(&mut t, id);
        t.app.manager.profile_budget = Some(0);
        assert_eq!(configure(&mut t, interface_kind, &[6])[0], 0xff);
        assert_eq!(rules_of(&mut t, id), before_reset);
        t.app.manager.profile_budget = Some(80);
        assert_eq!(
            configure(&mut t, interface_kind, &[5, 0, 0, 0, 0, 6])[0],
            0xff
        );
        assert_eq!(rules_of(&mut t, id), before_reset);
        ALLOCS.set(0);
        TRACK.set(true);
        let reply = configure(&mut t, interface_kind, &[6]);
        TRACK.set(false);
        assert_eq!(reply[0], 6);
        assert_eq!(ALLOCS.get(), 0);
        t.app.manager.profile_budget = Some(PROFILE_BUDGET as usize);
        assert!(rules_of(&mut t, id).is_empty());
        assert_eq!(rules_of(&mut t, other), other_rules);
        assert_eq!(press(&mut t, link, &[4]), [5]);
        assert!(press(&mut t, link, &[]).is_empty());
        assert_eq!(press(&mut t, link, &[4]), [4]);
        assert!(t.events().iter().any(|event| {
            matches!(event, Ev::Profile(profile) if profile.id == id && profile.roles.is_empty())
        }));
        t.now += cordial_core::deferred::MAX_DELAY_MS;
        t.quiet();
        assert!(saved_rules(&mut t, id).is_empty());
    }
}

#[test]
fn disabled_nonresident_profile_outputs_reserve_usb_and_release_on_edit() {
    let mut t = nonresident_gamepad_output();
    assert!(!t.app.manager.usb_allocation().topology.desktop);
    let profile = profile_with(&mut t, "Keyboard output", [4]);
    set_layers(&mut t, SAVED, &[profile]);
    assert!(t.app.manager.find(SAVED.into()).is_none());
    assert!(t.app.manager.usb_allocation().topology.desktop);
    assert_eq!(t.app.manager.profiles.used(), 0);
    set_rules(
        &mut t,
        profile,
        vec![forget_rule(KEYBOARD_COLLECTION, key(4))],
    );
    assert!(!t.app.manager.usb_allocation().topology.desktop);
    assert!(!t.saved(SAVED).enabled);
}

#[test]
fn required_and_reserved_usb_errors_are_independent_and_resources_keep_unknown_presence() {
    let mut t = nonresident_gamepad_output();
    t.app.manager.usb_budget.interfaces = 2;
    t.app
        .manager
        .outputs
        .prepare(88, p::OutputMode::Hid)
        .unwrap();
    t.app.manager.outputs.set(88, p::OutputMode::Hid);
    let status = t.status();
    assert!(
        status
            .errors
            .iter()
            .any(|e| e.code == p::ErrorCode::UsbRequiredCapacityExceeded as i32)
    );
    assert!(
        status
            .errors
            .iter()
            .any(|e| e.code == p::ErrorCode::UsbReservedCapacityExceeded as i32)
    );
    let interfaces = status
        .resources
        .iter()
        .find(|r| r.r#type == p::ResourceType::Interfaces as i32)
        .unwrap();
    assert_eq!(interfaces.count, None);
    assert_eq!(interfaces.limit, Some(2));
    t.app.set_usb_topology(Default::default());
    let status = t.status();
    assert_eq!(status.resources[0].count, Some(2));
    t.app.manager.outputs.remove(88);
    let status = t.status();
    assert!(
        status
            .errors
            .iter()
            .any(|e| e.code == p::ErrorCode::UsbRequiredCapacityExceeded as i32)
    );
    assert!(
        !status
            .errors
            .iter()
            .any(|e| e.code == p::ErrorCode::UsbReservedCapacityExceeded as i32)
    );
}

#[test]
fn empty_lists_select_their_response_variants() {
    let mut t = Test::new(false);
    assert_eq!(
        t.ok(Command::ListDevices(p::ListDevices {})),
        Some(R::Devices(p::DeviceList::default()))
    );
    assert_eq!(
        t.ok(Command::ListProfiles(p::ListProfiles {})),
        Some(R::Profiles(p::ProfileList::default()))
    );
    let id = create(&mut t, "Empty");
    assert_eq!(
        t.ok(Command::ListProfileRules(p::ListProfileRules {
            profile: id
        })),
        Some(R::ProfileRules(p::ProfileRules {
            profile: id,
            rules: vec![]
        }))
    );
    let mut t = Test::new(true);
    assert!(t.settings(SAVED).is_empty());
    #[cfg(feature = "development")]
    {
        let (_, store, radio) = setup();
        let mut t = Test::with(store, radio, build(true, None));
        assert_eq!(
            t.ok(Command::ListFeatures(p::ListFeatures { device: SAVED })),
            Some(R::Features(p::FeatureList::default()))
        );
        assert_eq!(
            t.ok(Command::ListFiles(p::ListFiles { path: "/".into() })),
            Some(R::Files(p::FileList::default()))
        );
    }
}

#[test]
fn large_device_and_profile_lists_are_single_responses() {
    let mut t = Test::new(true);
    for n in 2..=40 {
        t.add_saved(n);
    }
    let entries = t.device_entries();
    assert_eq!(entries.len(), 40);
    assert!(p::DeviceList { entries }.encoded_len() > 1024);
    for n in 0..80 {
        create(&mut t, &format!("Profile number {n}"));
    }
    let Some(R::Profiles(list)) = t.ok(Command::ListProfiles(p::ListProfiles {})) else {
        panic!()
    };
    assert_eq!(list.entries.len(), 80);
    assert!(list.encoded_len() > 1024);
}

#[cfg(feature = "development")]
#[test]
fn large_features_and_files_are_single_ordered_responses() {
    use cordial_core::{compact::FeatureEntry, model::hidpp::FeatureFlags};
    let (_, store, radio) = setup();
    let mut t = Test::with(store, radio, build(true, None));
    let link = connect_saved(&mut t, descriptor());
    press(&mut t, link, &[4]);
    t.events();
    let features = (0..100)
        .map(|index| {
            FeatureEntry {
                id: FeatureId(0x1000 + index),
                version: FeatureRevision(1),
                flags: FeatureFlags(0),
            }
            .wire(index as usize)
        })
        .collect();
    t.live(SAVED)
        .catalog
        .replace_discovery(vec![], features)
        .unwrap();
    let Some(R::Features(list)) = t.ok(Command::ListFeatures(p::ListFeatures { device: SAVED }))
    else {
        panic!()
    };
    assert_eq!(list.features.len(), 100);
    assert!(list.encoded_len() > 1024);
    for (index, feature) in list.features.iter().enumerate() {
        let Some(p::feature::Detail::Hidpp(detail)) = &feature.detail else {
            panic!()
        };
        assert_eq!(detail.index, index as u32);
        assert_eq!(detail.id, 0x1000 + index as u32);
    }
    for n in 0..80 {
        t.store
            .files
            .insert(format!("/record-number-{n:03}"), vec![1]);
    }
    let Some(R::Files(list)) = t.ok(Command::ListFiles(p::ListFiles { path: "/".into() })) else {
        panic!()
    };
    assert_eq!(list.entries.len(), 80);
    assert!(list.encoded_len() > 1024);
    assert!(list.entries.windows(2).all(|w| w[0].name < w[1].name));
}

#[test]
fn blocked_stream_stops_reading_and_a_late_error_replaces_partial_rules() {
    let mut t = Test::with_budget(false, Some(8192));
    let id = profile_with(&mut t, "Many", 4..100);
    t.events();
    block_on(t.app.begin(
        p::Request {
            command: Some(Command::ListProfileRules(p::ListProfileRules {
                profile: id,
            })),
        },
        &mut t.store,
        &mut t.radio,
        t.now,
    ));
    block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
    assert!(t.app.command_pending());
    assert!(!t.app.serial.stream_ready());
    let reads = t.store.loads;
    let queued = t.app.serial.queued();
    ALLOCS.set(0);
    TRACK.set(true);
    for _ in 0..10_000 {
        block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
    }
    TRACK.set(false);
    assert_eq!(ALLOCS.get(), 0);
    assert_eq!(t.store.loads, reads);
    assert_eq!(t.app.serial.queued(), queued);
    assert!(t.output().is_empty(), "no delimiter before completion");
    t.store.fail_load = Some(record_key(9, id));
    let Some(R::Error(error)) = t.response().result else {
        panic!("partial list succeeded")
    };
    assert_eq!(error.code, p::ErrorCode::StorageFailed as i32);
    assert!(!t.app.command_pending());
    t.store.fail_load = None;
    assert_eq!(rules_of(&mut t, id).len(), 96);
}

#[test]
fn profile_stream_reads_current_records() {
    let mut t = Test::new(false);
    let ids: Vec<_> = (0..40)
        .map(|n| create(&mut t, &format!("Profile {n}")))
        .collect();
    t.events();
    block_on(t.app.begin(
        p::Request {
            command: Some(Command::ListProfiles(p::ListProfiles {})),
        },
        &mut t.store,
        &mut t.radio,
        t.now,
    ));
    for _ in 0..100 {
        block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
        if !t.app.serial.stream_ready() {
            break;
        }
    }
    let last = *ids.last().unwrap();
    let mut metadata =
        block_on(cordial_core::profiles::metadata(&mut t.store, last.into())).unwrap();
    metadata.name = "Changed during listing".into();
    block_on(cordial_core::profiles::save_metadata(
        &mut t.store,
        last.into(),
        &metadata,
    ))
    .unwrap();
    let Some(R::Profiles(list)) = t.response().result else {
        panic!()
    };
    let Some(p::profile_list_entry::Entry::Profile(profile)) = &list.entries.last().unwrap().entry
    else {
        panic!()
    };
    assert_eq!(profile.name, metadata.name);
    assert_eq!(list.entries.len(), 40);
}

#[test]
fn settings_changes_during_streaming_follow_the_complete_response() {
    use cordial_core::compact::Record;
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, descriptor());
    press(&mut t, link, &[4]);
    t.events();
    let records = [SettingKey::FnRowDefault, SettingKey::BacklightMode].map(|key| {
        let mut metadata = preference(key, FeatureId::BACKLIGHT, 1).metadata;
        metadata.choices = vec![1; 600].into_boxed_slice();
        Record::new(metadata, true)
    });
    t.live(SAVED)
        .catalog
        .replace_discovery(records.clone().into(), vec![])
        .unwrap();
    t.events();
    block_on(t.app.begin(
        p::Request {
            command: Some(Command::ListSettings(p::ListSettings { device: SAVED })),
        },
        &mut t.store,
        &mut t.radio,
        t.now,
    ));
    block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
    assert!(!t.app.serial.stream_ready());
    assert!(t.output().is_empty());
    let mut changed = records;
    changed[0].metadata.choices = vec![2; 600].into_boxed_slice();
    t.live(SAVED)
        .catalog
        .replace_discovery(changed.into(), vec![])
        .unwrap();
    t.poll();
    assert!(!t.app.serial.idle());
    assert!(
        t.output().is_empty(),
        "events wait for the response delimiter"
    );
    let Some(R::Settings(list)) = t.response().result else {
        panic!()
    };
    assert_eq!(list.objects.len(), 2);
    let current = cordial_core::wire::settings(&t.live(SAVED).catalog).unwrap();
    assert_ne!(list.objects[0], current[0]);
    let events = t.events();
    let event = events
        .iter()
        .find_map(|e| match e {
            Ev::SettingsChanged(s) => Some(s),
            _ => None,
        })
        .unwrap();
    assert_eq!(event.changed, current);
}

/// Synthetic flash keeps its allocation fixed throughout listing and cleanup. It has no read or
/// write log, and enumeration allocates only the requested batch, as the firmware stores do.
struct DamagedStore {
    devices: Vec<bool>,
    profiles: Vec<bool>,
    fail_load: Option<(u8, u64)>,
    fail_remove: bool,
}
impl DamagedStore {
    fn records(&self, kind: u8) -> &[bool] {
        match kind {
            2 => &self.devices,
            8 => &self.profiles,
            _ => &[],
        }
    }
}
impl cordial_core::storage::RecordStore for DamagedStore {
    async fn keys(
        &mut self,
    ) -> Result<Vec<cordial_core::storage::RecordKey>, cordial_core::storage::Error> {
        panic!("cleanup must use bounded enumeration")
    }
    async fn record_ids(
        &mut self,
        kind: u8,
        after: u64,
        limit: usize,
    ) -> Result<Vec<u64>, cordial_core::storage::Error> {
        Ok(self
            .records(kind)
            .iter()
            .enumerate()
            .skip(after as usize)
            .filter(|(_, present)| **present)
            .take(limit)
            .map(|(index, _)| index as u64 + 1)
            .collect())
    }
    async fn available(&mut self) -> Result<usize, cordial_core::storage::Error> {
        Ok(65536)
    }
    async fn load(
        &mut self,
        key: cordial_core::storage::RecordKey,
        value: &mut [u8],
    ) -> Result<Option<usize>, cordial_core::storage::Error> {
        let id = u64::from_be_bytes(key[1..].try_into().unwrap()) as usize;
        if self.fail_load == Some((key[0], id as u64)) {
            return Err(cordial_core::storage::Error::Io);
        }
        if id != 0 && self.records(key[0]).get(id - 1) == Some(&true) {
            value[0] = 0xff;
            Ok(Some(1))
        } else {
            Ok(None)
        }
    }
    async fn save(
        &mut self,
        _: cordial_core::storage::RecordKey,
        _: &[u8],
    ) -> Result<(), cordial_core::storage::Error> {
        panic!("corrupt metadata is removed")
    }
    async fn remove(
        &mut self,
        key: cordial_core::storage::RecordKey,
    ) -> Result<(), cordial_core::storage::Error> {
        if self.fail_remove {
            return Err(cordial_core::storage::Error::Io);
        }
        let id = u64::from_be_bytes(key[1..].try_into().unwrap()) as usize;
        let records = match key[0] {
            2 => &mut self.devices,
            8 => &mut self.profiles,
            _ => return Ok(()),
        };
        if let Some(present) = id.checked_sub(1).and_then(|index| records.get_mut(index)) {
            *present = false;
        }
        Ok(())
    }
}

#[test]
fn corrupt_listings_and_removal_events_have_constant_heap_high_water() {
    fn measure(count: usize, close: bool, fail_remove: bool) -> isize {
        let mut t = Test::new(false);
        t.events();
        let mut store = DamagedStore {
            devices: vec![true; count],
            profiles: vec![true; count],
            fail_load: None,
            fail_remove: false,
        };
        let mut removed_devices = vec![false; count];
        let mut removed_profiles = vec![false; count];
        let baseline = HEAP.with(|heap| {
            let live = heap.get().0;
            heap.set((live, live));
            live
        });
        for command in [
            Command::ListDevices(p::ListDevices {}),
            Command::ListProfiles(p::ListProfiles {}),
        ] {
            block_on(t.app.begin(
                p::Request {
                    command: Some(command),
                },
                &mut store,
                &mut t.radio,
                t.now,
            ));
            for _ in 0..count * 4 + 100 {
                if !t.app.command_pending() {
                    break;
                }
                block_on(t.app.proceed(&mut store, &mut t.radio, t.now));
                assert!(t.app.manager.lost_profiles.len() <= 1);
                assert!(t.app.manager.lost_devices.len() <= 1);
            }
            assert!(
                !t.app.command_pending(),
                "all corrupt entries must still finish the list"
            );
            let output = t.output();
            assert_eq!(output.len(), 1);
            for message in output {
                match message {
                    M::Response(p::Response {
                        result: Some(R::Devices(list)),
                    }) => assert!(list.entries.is_empty()),
                    M::Response(p::Response {
                        result: Some(R::Profiles(list)),
                    }) => assert!(list.entries.is_empty()),
                    other => panic!("unexpected output {other:?}"),
                }
            }
        }
        if close {
            t.app.session(false, &mut t.radio);
        }
        store.fail_remove = fail_remove;
        // The response is finished, but cleanup must also remain bounded when events stop draining.
        for _ in 0..10_000 {
            block_on(t.app.work(&mut store, &mut t.radio, t.now));
        }
        store.fail_remove = false;
        t.now += FIRST_RETRY;
        let mut events = 0;
        for _ in 0..count * 40 + 1000 {
            block_on(t.app.work(&mut store, &mut t.radio, t.now));
            assert!(t.app.manager.lost_profiles.len() <= 1);
            assert!(t.app.manager.lost_devices.len() <= 1);
            for message in t.output() {
                let (id, removed) = match message {
                    M::Event(p::Event {
                        kind: Some(Ev::DeviceRemoved(e)),
                    }) => (e.id, &mut removed_devices),
                    M::Event(p::Event {
                        kind: Some(Ev::ProfileRemoved(e)),
                    }) => (e.id, &mut removed_profiles),
                    M::Event(p::Event {
                        kind: Some(Ev::Adapter(_)),
                    }) => continue,
                    other => panic!("unexpected output {other:?}"),
                };
                assert!(
                    !std::mem::replace(&mut removed[id as usize - 1], true),
                    "duplicate removal"
                );
                events += 1;
            }
            if (close || events == count * 2)
                && store
                    .devices
                    .iter()
                    .chain(&store.profiles)
                    .all(|present| !present)
            {
                break;
            }
        }
        assert!(
            store
                .devices
                .iter()
                .chain(&store.profiles)
                .all(|present| !present)
        );
        assert_eq!(events, if close { 0 } else { count * 2 });
        HEAP.with(|heap| heap.get().1 - baseline)
    }
    for (close, fail_remove) in [(false, false), (false, true), (true, false), (true, true)] {
        let small = measure(32, close, fail_remove);
        let large = measure(2048, close, fail_remove);
        eprintln!(
            "corrupt devices + profiles, closed={close}, removal failure={fail_remove}: 32 each peak {small} bytes; 2048 each peak {large} bytes"
        );
        assert!(
            large <= small + 256,
            "heap grew with the number of corrupt records"
        );
    }
}

#[test]
fn deferred_corrupt_record_rescan_retries_unreadable_records() {
    for kind in [2, 8] {
        let mut t = Test::new(false);
        t.events();
        let mut store = DamagedStore {
            devices: vec![true; 4],
            profiles: vec![true; 4],
            fail_load: None,
            fail_remove: false,
        };
        for command in [
            Command::ListDevices(p::ListDevices {}),
            Command::ListProfiles(p::ListProfiles {}),
        ] {
            block_on(t.app.begin(
                p::Request {
                    command: Some(command),
                },
                &mut store,
                &mut t.radio,
                t.now,
            ));
            while t.app.command_pending() {
                block_on(t.app.proceed(&mut store, &mut t.radio, t.now));
            }
            assert_eq!(t.output().len(), 1);
        }
        store.fail_load = Some((kind, 2));
        let mut removed = Vec::new();
        for _ in 0..500 {
            t.now += FIRST_RETRY;
            block_on(t.app.work(&mut store, &mut t.radio, t.now));
            removed.extend(t.output());
        }
        assert_eq!(store.records(kind), [false, true, false, false]);
        assert!(!removed.iter().any(|message| match message {
            M::Event(p::Event {
                kind: Some(Ev::DeviceRemoved(e)),
            }) => kind == 2 && e.id == 2,
            M::Event(p::Event {
                kind: Some(Ev::ProfileRemoved(e)),
            }) => kind == 8 && e.id == 2,
            _ => false,
        }));
        store.fail_load = None;
        for _ in 0..500 {
            t.now += FIRST_RETRY;
            block_on(t.app.work(&mut store, &mut t.radio, t.now));
            removed.extend(t.output());
        }
        assert_eq!(store.devices, [false; 4]);
        assert_eq!(store.profiles, [false; 4]);
        assert_eq!(
            removed
                .iter()
                .filter(|message| matches!(
                    message,
                    M::Event(p::Event {
                        kind: Some(Ev::DeviceRemoved(_) | Ev::ProfileRemoved(_))
                    })
                ))
                .count(),
            8
        );
    }
}

#[test]
fn stalled_listing_allows_pairing_to_save_the_bond_and_reports_it_afterward() {
    let mut t = Test::new(false);
    for _ in 0..40 {
        create(&mut t, "Profile");
    }
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(candidate);
    t.events();
    let link = t.radio.connects[0].0;
    block_on(t.app.begin(
        p::Request {
            command: Some(Command::ListProfiles(p::ListProfiles {})),
        },
        &mut t.store,
        &mut t.radio,
        t.now,
    ));
    for _ in 0..100 {
        block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
        if !t.app.serial.stream_ready() {
            break;
        }
    }
    assert!(t.app.streaming() && !t.app.serial.stream_ready());
    t.radio.bonds.push(peer(2));
    t.event(Event::Bonded {
        link,
        identity: peer(2),
    });
    for _ in 0..100 {
        block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
    }
    assert!(t.app.streaming() && !t.app.serial.stream_ready());
    let device = t
        .app
        .manager
        .devices
        .iter()
        .flatten()
        .find(|d| d.peer == peer(2))
        .unwrap()
        .id as u32;
    assert_eq!(t.saved(device).peer, peer(2));
    assert!(
        t.output().is_empty(),
        "pairing events wait for the response delimiter"
    );
    assert!(matches!(t.response().result, Some(R::Profiles(list)) if list.entries.len() == 40));
    assert_eq!(
        pairing_steps(&t.events()),
        [p::pairing::Step::Done(p::PairingDone { device })]
    );
}

#[test]
fn paused_profile_cleanup_allows_pairing_bond_sync_and_resident_fill() {
    for streaming in [false, true] {
        for pairing in [true, false] {
            let mut t = Test::new(false);
            let lost = create(&mut t, "Lost");
            for _ in 0..40 {
                create(&mut t, "Kept");
            }
            let first = t.add_saved(1);
            let second = t.add_saved(2);
            set_layers(&mut t, first, &[lost]);
            set_layers(&mut t, second, &[lost]);
            let candidate = t.candidate(peer(3), peer(3));
            t.events();
            lose_profile(&mut t, lost);
            for _ in 0..100 {
                block_on(t.app.work(&mut t.store, &mut t.radio, t.now));
                if t.saved(first).profiles.is_empty() {
                    break;
                }
                t.output();
            }
            assert!(t.saved(first).profiles.is_empty());
            assert_eq!(t.saved(second).profiles, [u64::from(lost)]);
            assert!(t.store.records.contains_key(&record_key(8, lost)));

            if pairing {
                t.pair(candidate);
            } else {
                let mut policy = Policy::paired(99, peer(9), b"Keyboard");
                policy.setup_pending = false;
                block_on(cordial_core::bonds::commit(
                    &mut t.store,
                    &policy,
                    &bond(99, peer(9)),
                ))
                .unwrap();
                t.app.manager.vacated = true;
            }
            if streaming {
                block_on(t.app.begin(
                    p::Request {
                        command: Some(Command::ListProfiles(p::ListProfiles {})),
                    },
                    &mut t.store,
                    &mut t.radio,
                    t.now,
                ));
            }
            for _ in 0..200 {
                t.now += 1;
                if streaming {
                    block_on(t.app.proceed(&mut t.store, &mut t.radio, t.now));
                } else {
                    block_on(t.app.work(&mut t.store, &mut t.radio, t.now));
                }
            }
            assert!(t.app.serial.queued() > 0);
            assert_eq!(
                t.saved(second).profiles,
                [u64::from(lost)],
                "cleanup waits for output"
            );
            if pairing {
                let link = t
                    .radio
                    .connects
                    .iter()
                    .find(|(_, pairing)| *pairing)
                    .expect("pairing opens its link while cleanup waits")
                    .0;
                t.radio.bonds.push(peer(3));
                t.event(Event::Bonded {
                    link,
                    identity: peer(3),
                });
                for _ in 0..100 {
                    block_on(t.app.work(&mut t.store, &mut t.radio, t.now));
                }
                let device = t
                    .app
                    .manager
                    .devices
                    .iter()
                    .flatten()
                    .find(|device| device.peer == peer(3))
                    .expect("pairing saves the device")
                    .id as u32;
                assert_eq!(t.saved(device).peer, peer(3));
            } else {
                assert!(
                    t.app.manager.find(99).is_some(),
                    "fill admits a saved device while cleanup waits"
                );
                assert!(t.radio.bonds.contains(&peer(9)), "fill imports its bond");
                assert!(!t.app.manager.bonds_pending);
            }
            assert!(t.store.records.contains_key(&record_key(8, lost)));
            if streaming {
                assert!(t.app.streaming() && !t.app.serial.stream_ready());
                assert!(
                    matches!(t.response().result, Some(R::Profiles(list)) if list.entries.len() == 40)
                );
            }
            let mut events: Vec<_> = t
                .output()
                .into_iter()
                .filter_map(|message| match message {
                    M::Event(event) => event.kind,
                    _ => panic!("only events follow the response"),
                })
                .collect();
            t.store.reads.clear();
            events.extend(t.events());
            assert!(
                !t.store.reads.contains(&record_key(8, lost)),
                "cleanup resumes its device cursor without restarting metadata cleanup"
            );
            assert!(
                t.store
                    .reads
                    .iter()
                    .filter(|key| **key == record_key(2, first))
                    .count()
                    <= 1,
                "only a pending device event may reread the already cleaned device"
            );
            assert!(t.saved(first).profiles.is_empty());
            assert!(t.saved(second).profiles.is_empty());
            assert!(!t.store.records.contains_key(&record_key(8, lost)));
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, Ev::ProfileRemoved(p) if p.id == lost))
                    .count(),
                1
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, Ev::Device(d) if d.id == second && d.profiles.as_ref().is_some_and(|p| p.profiles.is_empty())))
            );
            assert!(t.app.manager.lost_profiles.is_empty());
        }
    }
}

#[test]
fn scan_limits_keep_candidates_and_radio_failure_keeps_the_collection_error() {
    for radio_failure in [false, true] {
        let mut t = Test::new(false);
        t.scan(&[p::Transport::Classic]);
        for n in 1..=cordial_core::devices::SCAN_CANDIDATES + 1 {
            t.found(peer(n as u8), Some(peer(n as u8)), "Keyboard");
        }
        if radio_failure {
            t.event(Event::Failed(ErrorCode::ConnectionFailed));
        } else {
            t.ok(Command::StopScan(p::StopScan {}));
        }
        let events = t.events();
        let done = events
            .iter()
            .find_map(|event| match event {
                Ev::ScanDone(done) => Some(done),
                _ => None,
            })
            .unwrap();
        assert_eq!(done.count, cordial_core::devices::SCAN_CANDIDATES as u32);
        let capacity = p::Error {
            code: p::ErrorCode::NoCapacity as i32,
            reason: p::ErrorReason::ScanCandidates as i32,
            outcome: p::ErrorOutcome::Partial as i32,
            related: vec![],
        };
        assert_eq!(
            done.error,
            Some(if radio_failure {
                p::Error {
                    code: p::ErrorCode::ConnectionFailed as i32,
                    reason: p::ErrorReason::Radio as i32,
                    outcome: p::ErrorOutcome::Partial as i32,
                    related: vec![capacity],
                }
            } else {
                capacity
            })
        );
        if !radio_failure {
            let candidate = events
                .iter()
                .find_map(|event| match event {
                    Ev::ScanFound(c) => Some(c.id),
                    _ => None,
                })
                .unwrap();
            t.pair(candidate);
        }
    }
}

#[test]
fn empty_failed_scan_has_no_results_and_starting_again_clears_the_error() {
    let mut t = Test::new(false);
    t.scan(&[p::Transport::Classic]);
    t.event(Event::Failed(ErrorCode::RadioUnavailable));
    assert!(t.events().iter().any(|event| matches!(event, Ev::ScanDone(done)
        if done.count == 0 && done.error.as_ref().is_some_and(|error| error.reason() == p::ErrorReason::Radio && error.outcome() == p::ErrorOutcome::NoEffect))));
    t.event(Event::Ready);
    t.scan(&[p::Transport::Classic]);
    t.ok(Command::StopScan(p::StopScan {}));
    assert!(t.events().contains(&Ev::ScanDone(p::ScanDone {
        count: 0,
        error: None
    })));
}

#[test]
fn pairing_save_evidence_survives_bond_cleanup_and_radio_recovery() {
    for unknown in [false, true] {
        let mut t = Test::new(true);
        t.app.manager.devices[0].as_mut().unwrap().paused = true;
        let identity = peer(2);
        let candidate = t.candidate(identity, identity);
        t.pair(candidate);
        t.poll();
        let link = t.radio.connects.last().unwrap().0;
        let record = record_key(2, SAVED + 1);
        if unknown {
            t.store.unknown = Some(record);
        } else {
            t.store.full = Some(record);
        }
        t.event(Event::Bonded { link, identity });
        t.events();
        t.radio.reject_forget = true;
        t.finish_disconnects();
        let steps = pairing_steps(&t.events());
        let Some(p::pairing::Step::Error(error)) = steps.last() else {
            panic!("{steps:?}")
        };
        assert_eq!(
            error.code(),
            if unknown {
                p::ErrorCode::StorageFailed
            } else {
                p::ErrorCode::NoCapacity
            }
        );
        assert_eq!(error.reason(), p::ErrorReason::Storage);
        assert_eq!(
            error.outcome(),
            if unknown {
                p::ErrorOutcome::Unknown
            } else {
                p::ErrorOutcome::NoEffect
            }
        );
        assert!(
            error
                .related
                .iter()
                .any(|error| error.code() == p::ErrorCode::StorageFailed
                    && error.reason() == p::ErrorReason::Radio
                    && error.outcome() == p::ErrorOutcome::Unspecified)
        );
        if unknown {
            assert!(
                error
                    .related
                    .iter()
                    .any(|error| error.code() == p::ErrorCode::StorageFailed
                        && error.reason() == p::ErrorReason::Storage)
            );
            t.event(Event::Failed(ErrorCode::Timeout));
            assert!(
                t.status()
                    .errors
                    .iter()
                    .any(|error| error.reason() == p::ErrorReason::Radio)
            );
            t.radio.bonds = vec![identity];
            t.radio.applied.clear();
            let writes = t.store.writes.len();
            t.event(Event::Ready);
            assert!(t.radio.bonds.contains(&peer(1)));
            assert!(t.radio.bonds.contains(&identity));
            assert_eq!(
                t.radio.applied,
                [(Transport::Classic, true), (Transport::Ble, true)]
            );
            assert_eq!(t.store.writes.len(), writes);
            assert!(t.app.manager.bonds_pending);
            assert_eq!(t.app.manager.admits(peer(1), t.now), None);
            let error = t.error(Command::ListDevices(p::ListDevices {}));
            assert_eq!(error.reason(), p::ErrorReason::Storage);
            let status = t.status();
            assert!(!status.ready);
            assert!(
                status
                    .errors
                    .iter()
                    .all(|error| error.reason() != p::ErrorReason::Radio)
            );
            assert!(
                status
                    .errors
                    .iter()
                    .any(|error| error.reason() == p::ErrorReason::Storage
                        && error.outcome() == p::ErrorOutcome::Unknown)
            );
        } else {
            assert!(t.status().ready);
        }
    }
}

#[test]
fn adapter_startup_and_radio_storage_faults_recover_independently() {
    let app = Application::new(build(false, None));
    assert!(!app.status(&Radio::default()).ready);
    assert!(app.status(&Radio::default()).errors.is_empty());
    let mut t = Test::new(false);
    t.event(Event::Failed(ErrorCode::Timeout));
    t.event(Event::Failed(ErrorCode::StorageFailed));
    let errors = t.status().errors;
    assert_eq!(errors.len(), 2);
    assert!(
        errors
            .iter()
            .any(|error| error.reason() == p::ErrorReason::Radio
                && error.code() == p::ErrorCode::Timeout)
    );
    assert!(
        errors
            .iter()
            .any(|error| error.reason() == p::ErrorReason::Storage
                && error.code() == p::ErrorCode::StorageFailed)
    );
    t.store.fail = true;
    t.event(Event::Ready);
    let errors = t.status().errors;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].reason(), p::ErrorReason::Storage);
    t.store.fail = false;
    t.event(Event::Ready);
    assert!(t.status().errors.is_empty());
    assert!(t.status().ready);
}

#[test]
fn unsupported_integrations_report_errors_with_the_current_failure_code() {
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, descriptor());
    t.settle();
    let error = match hidpp_state(&t.device(SAVED)) {
        p::integration::Status::Error(error) => error,
        state => panic!("{state:?}"),
    };
    assert_eq!(error.code(), p::ErrorCode::ProtocolUnsupported);
    assert_eq!(error.outcome(), p::ErrorOutcome::Unspecified);
    t.app
        .manager
        .connection_mut(link)
        .unwrap()
        .runtime
        .as_mut()
        .unwrap()
        .client
        .protocol = cordial_core::model::hidpp::ProtocolState::Error {
        code: ErrorCode::HidppInvalidResponse,
    };
    assert!(
        matches!(hidpp_state(&t.device(SAVED)), p::integration::Status::Error(error)
        if error.code() == p::ErrorCode::InvalidResponse)
    );
}

#[test]
fn pairing_failure_before_saving_does_not_reuse_another_writes_uncertainty() {
    let mut t = Test::new(false);
    let identity = peer(2);
    let candidate = t.candidate(identity, identity);
    t.pair(candidate);
    t.poll();
    let link = t.radio.connects.last().unwrap().0;
    t.app.manager.write_uncertain = true;
    t.radio.reject_adoption = true;
    t.event(Event::Bonded { link, identity });
    t.events();
    t.finish_disconnects();
    let steps = pairing_steps(&t.events());
    assert!(matches!(steps.last(), Some(p::pairing::Step::Error(error))
        if error.code() == p::ErrorCode::ConnectionFailed && error.outcome() == p::ErrorOutcome::NoEffect));
}

#[test]
fn pairing_reports_a_confirmed_bond_when_subsequent_cleanup_fails() {
    for admitted in [false, true] {
        let mut t = Test::new(true);
        t.app.manager.devices[0].as_mut().unwrap().paused = true;
        let identity = peer(2);
        // Renewing a disabled saved device commits its bond without promoting the link.
        let saved = if admitted { SAVED + 1 } else { SAVED };
        if !admitted {
            let mut policy = t.saved(SAVED);
            policy.peer = identity;
            policy.enabled = false;
            block_on(cordial_core::bonds::commit(
                &mut t.store,
                &policy,
                &bond(policy.id, identity),
            ))
            .unwrap();
            block_on(t.app.manager.load(&mut t.store, &mut t.radio)).unwrap();
        }
        let candidate = t.candidate(identity, identity);
        t.pair(candidate);
        t.poll();
        let link = t.radio.connects.last().unwrap().0;
        let record = record_key(2, saved);
        t.store.fail_count = true;
        t.radio.bonds.push(identity);
        t.event(Event::Bonded { link, identity });
        let events = t.events();
        let steps = pairing_steps(&events);
        let Some(p::pairing::Step::Error(error)) = steps.last() else {
            panic!("{steps:?}")
        };
        assert_eq!(error.code(), p::ErrorCode::StorageFailed);
        assert_eq!(error.reason(), p::ErrorReason::Storage);
        assert_eq!(error.outcome(), p::ErrorOutcome::Complete);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Ev::Device(device) if device.id == saved))
        );
        assert!(t.store.records.contains_key(&record));
        assert!(t.radio.bonds.contains(&identity));
        assert_eq!(t.radio.promoted.contains(&link), admitted);
        assert_eq!(t.radio.closes.contains(&link), !admitted);
        assert!(t.app.manager.storage_ready);
        assert!(t.status().errors.is_empty());
        assert!(t.device_entries().iter().any(|entry| matches!(&entry.entry, Some(p::device_list_entry::Entry::Device(device)) if device.id == saved)));
    }
}

#[test]
fn rejected_background_scan_toggles_retry_without_dropping_live_devices() {
    for rejection in [
        ErrorCode::Busy,
        ErrorCode::Capacity,
        ErrorCode::RadioUnavailable,
    ] {
        let mut t = Test::new(true);
        let identity = Peer {
            transport: Transport::Ble,
            ..peer(2)
        };
        let policy = Policy::paired(78, identity, b"Mouse");
        block_on(cordial_core::bonds::commit(
            &mut t.store,
            &policy,
            &bond(78, identity),
        ))
        .unwrap();
        block_on(t.app.manager.load(&mut t.store, &mut t.radio)).unwrap();
        let link = connect_saved(&mut t, descriptor());
        t.scan(&[p::Transport::Ble]);
        t.radio.reject_scan = Some(rejection);
        t.now = 1499;
        t.poll();
        assert!(t.radio.reconnect.contains(&identity));
        assert_eq!(
            t.radio.scans.last().map(|scan| (scan.1, scan.2)),
            Some((false, false))
        );
        assert!(t.app.manager.connection(link).is_some_and(|c| !c.closing));
        assert!(t.status().ready);
        assert!(t.status().errors.is_empty());
        assert!(
            !t.events()
                .iter()
                .any(|event| matches!(event, Ev::ScanDone(_)))
        );
        let attempts = t.radio.scans.len();
        t.radio.reject_scan = None;
        t.poll();
        assert_eq!(t.radio.scans.len(), attempts + 1);
        t.poll();
        assert_eq!(t.radio.scans.len(), attempts + 1);
        t.event(Event::Failed(ErrorCode::RadioUnavailable));
        assert!(t.app.manager.connection(link).is_none());
        assert!(!t.status().ready);
        assert!(
            t.status()
                .errors
                .iter()
                .any(|error| error.reason() == p::ErrorReason::Radio)
        );
        assert!(
            t.events()
                .iter()
                .any(|event| matches!(event, Ev::ScanDone(done)
            if done.error.as_ref().is_some_and(|error| error.reason() == p::ErrorReason::Radio)))
        );
    }
}

#[test]
fn rejected_scan_stop_reports_the_operation_error_and_retries_the_backend_stop() {
    let mut t = Test::new(false);
    t.scan(&[p::Transport::Classic]);
    t.radio.reject_scan = Some(ErrorCode::Busy);
    t.ok(Command::StopScan(p::StopScan {}));
    assert!(
        t.events()
            .iter()
            .any(|event| matches!(event, Ev::ScanDone(done)
        if done.error.as_ref().is_some_and(|error| error.code() == p::ErrorCode::Busy
            && error.reason() == p::ErrorReason::Radio)))
    );
    assert!(t.status().ready);
    assert!(t.status().errors.is_empty());
    let attempts = t.radio.scans.len();
    t.radio.reject_scan = None;
    t.poll();
    assert_eq!(t.radio.scans.len(), attempts + 1);
    assert_eq!(
        t.radio.scans.last().map(|scan| (scan.1, scan.2)),
        Some((false, false))
    );
    t.poll();
    assert_eq!(t.radio.scans.len(), attempts + 1);
}

#[test]
fn startup_refusals_do_not_claim_radio_failure() {
    let mut t = Test::new(false);
    t.app.manager.radio_ready = false;
    let error = t.error(Command::StartScan(p::StartScan {
        transports: vec![p::Transport::Ble as i32],
        seconds: 60,
    }));
    assert_eq!(error.code(), p::ErrorCode::NotReady);
    assert_eq!(error.reason(), p::ErrorReason::Unknown);
    assert!(t.status().errors.is_empty());
}

#[test]
fn startup_memory_failure_is_not_a_radio_fault() {
    let (_, store, radio) = setup();
    let mut t = Test::with(store, radio, build(false, None));
    t.app.manager.storage_ready = false;
    FAIL_LAYOUT.with(|f| {
        f.set(Some(
            std::alloc::Layout::array::<Option<SavedDevice>>(4).unwrap(),
        ))
    });
    t.event(Event::Ready);
    assert!(FAIL_LAYOUT.with(|f| f.get()).is_none());
    assert!(t.app.manager.radio_ready);
    assert!(!t.app.manager.storage_ready);
    let errors = t.status().errors;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code(), p::ErrorCode::NoCapacity);
    assert_eq!(errors[0].reason(), p::ErrorReason::Unknown);
    t.event(Event::Ready);
    assert!(t.status().ready);
    assert!(t.status().errors.is_empty());
}

#[test]
fn unresolved_save_recovery_restores_later_bonds_without_reconciling_lost_records() {
    for corrupt in [false, true] {
        let mut t = Test::new(true);
        let later = t.add_saved(2);
        t.app.manager.fail_unknown_save();
        let failure = t.app.manager.storage_error.clone();
        t.event(Event::Failed(ErrorCode::Timeout));
        if corrupt {
            t.store
                .records
                .insert(record_key(2, SAVED), UNDECODABLE.to_vec());
        } else {
            t.store.records.remove(&record_key(2, SAVED));
        }
        t.radio.bonds.clear();
        let writes = t.store.writes.len();
        let forgotten = t.radio.forgotten.len();
        t.event(Event::Ready);
        assert_eq!(t.store.writes.len(), writes);
        assert_eq!(t.radio.forgotten.len(), forgotten);
        assert!(t.app.manager.find(SAVED as u64).is_some());
        assert!(t.app.manager.find(later as u64).is_some());
        assert_eq!(t.radio.bonds, vec![peer(2)]);
        assert!(t.app.manager.bonds_pending);
        assert!(!t.app.manager.storage_ready);
        assert!(t.app.manager.radio_ready);
        assert_eq!(t.app.manager.storage_error, failure);
        assert_eq!(t.status().errors.len(), 1);
        assert_eq!(t.status().errors[0].outcome(), p::ErrorOutcome::Unknown);
    }
}

#[test]
fn recovery_cleanup_failures_retry_without_persistent_adapter_faults() {
    for radio_failure in [false, true] {
        let mut t = Test::new(true);
        t.event(Event::Failed(ErrorCode::Timeout));
        t.store.records.remove(&record_key(2, SAVED));
        if radio_failure {
            t.radio.reject_forget = true;
        } else {
            t.store.fail_remove = Some(record_key(2, SAVED));
        }
        t.event(Event::Ready);
        assert!(t.app.manager.lost_devices.contains(&(SAVED as u64)));
        assert!(t.app.manager.storage_ready);
        assert!(t.app.manager.radio_ready);
        assert!(t.status().errors.is_empty());
        t.store.fail_remove = None;
        t.radio.reject_forget = false;
        for _ in 0..20 {
            t.now += FIRST_RETRY;
            t.poll();
        }
        assert!(t.app.manager.lost_devices.is_empty());
        assert!(t.radio.bonds.is_empty());
        assert!(!t.app.manager.bonds_pending);
        assert!(t.status().ready);
        assert!(t.status().errors.is_empty());
    }
}

#[test]
fn refused_reload_preserves_the_existing_storage_fault() {
    let mut t = Test::new(true);
    connect_saved(&mut t, descriptor());
    t.app.manager.fail_storage();
    let failure = t.app.manager.storage_error.clone();
    // Live links refuse a full reload until they close.
    t.event(Event::Ready);
    assert_eq!(t.app.manager.storage_error, failure);
    assert!(!t.app.manager.storage_ready);
    assert!(t.app.manager.radio_ready);
    assert!(t.app.manager.radio_error.is_none());
}

#[test]
fn not_ready_uses_the_refusing_gate_and_keeps_both_faults() {
    let mut t = Test::new(false);
    t.app.manager.fail_unknown_save();
    t.app.manager.radio_ready = false;
    let scan = || {
        Command::StartScan(p::StartScan {
            transports: vec![p::Transport::Ble as i32],
            seconds: 60,
        })
    };
    let error = t.error(scan());
    assert_eq!(error.reason(), p::ErrorReason::Unknown);
    assert_eq!(
        error.related,
        vec![t.app.manager.storage_error.clone().unwrap()]
    );
    t.event(Event::Failed(ErrorCode::Timeout));
    let mut faults = vec![
        t.app.manager.storage_error.clone().unwrap(),
        t.app.manager.radio_error.clone().unwrap(),
    ];
    faults.sort();
    let error = t.error(scan());
    assert_eq!(error.code(), p::ErrorCode::NotReady);
    assert_eq!(error.reason(), p::ErrorReason::Radio);
    assert_eq!(error.related, faults);
    let error = t.error(Command::ListDevices(p::ListDevices {}));
    assert_eq!(error.reason(), p::ErrorReason::Storage);
    assert_eq!(error.related, faults);
}

#[test]
fn pairing_admission_allocation_failure_still_announces_the_saved_device() {
    for recount_failed in [false, true] {
        let mut t = Test::new(true);
        t.app.manager.devices[0].as_mut().unwrap().paused = true;
        let identity = peer(2);
        let candidate = t.candidate(identity, identity);
        t.pair(candidate);
        t.poll();
        let link = t.radio.connects.last().unwrap().0;
        t.app.manager.devices.shrink_to_fit();
        t.store.fail_count = recount_failed;
        FAIL_LAYOUT.with(|f| {
            f.set(Some(
                std::alloc::Layout::array::<Option<SavedDevice>>(4).unwrap(),
            ))
        });
        t.event(Event::Bonded { link, identity });
        let events = t.events();
        assert!(FAIL_LAYOUT.with(|f| f.get()).is_none());
        let steps = pairing_steps(&events);
        let Some(p::pairing::Step::Error(error)) = steps.last() else {
            panic!("{steps:?}")
        };
        assert_eq!(error.code(), p::ErrorCode::NoCapacity);
        assert_eq!(error.reason(), p::ErrorReason::EnabledDevices);
        assert_eq!(error.outcome(), p::ErrorOutcome::Complete);
        assert_eq!(error.related.len(), usize::from(recount_failed));
        if recount_failed {
            assert_eq!(error.related[0].reason(), p::ErrorReason::Storage);
            assert_eq!(error.related[0].outcome(), p::ErrorOutcome::Unspecified);
        }
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Ev::Device(device) if device.id == SAVED + 1))
        );
        assert!(t.store.records.contains_key(&record_key(2, SAVED + 1)));
        assert!(t.app.manager.storage_ready);
        assert!(t.radio.closes.contains(&link));
        assert!(!t.radio.promoted.contains(&link));
        t.finish_disconnects();
        t.events();
        assert!(t.app.manager.find(u64::from(SAVED + 1)).is_some());
    }
}

#[test]
fn load_refused_by_a_live_connection_is_not_a_radio_fault() {
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, descriptor());
    t.app.manager.storage_ready = false;
    t.event(Event::Ready);
    assert!(t.app.manager.radio_ready);
    assert!(t.app.manager.connection(link).is_some_and(|c| !c.closing));
    assert!(!t.app.manager.storage_ready);
    let errors = t.status().errors;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code(), p::ErrorCode::Busy);
    assert_eq!(errors[0].reason(), p::ErrorReason::Unknown);
}

#[test]
fn transport_failure_during_load_keeps_its_radio_context() {
    let mut t = Test::new(false);
    t.app.manager.storage_ready = false;
    t.radio.reject_transport = Some(ErrorCode::Capacity);
    t.event(Event::Ready);
    assert!(!t.app.manager.radio_ready);
    let errors = t.status().errors;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code(), p::ErrorCode::NoCapacity);
    assert_eq!(errors[0].reason(), p::ErrorReason::Radio);
    let error = t.error(Command::StartScan(p::StartScan {
        transports: vec![p::Transport::Ble as i32],
        seconds: 60,
    }));
    assert_eq!(error.code(), p::ErrorCode::NotReady);
    assert_eq!(error.reason(), p::ErrorReason::Radio);
    assert_eq!(error.related, errors);
    t.radio.reject_transport = None;
    t.event(Event::Ready);
    assert!(t.status().ready);
    assert!(t.status().errors.is_empty());
}

#[test]
fn committed_enable_reports_complete_and_publishes_the_saved_policy_after_import_failure() {
    let mut t = Test::new(true);
    t.set_device(p::SetDevice {
        device: SAVED,
        enabled: Some(false),
        ..Default::default()
    });
    t.events();
    t.radio.reject_import = Some(ErrorCode::Capacity);
    let error = t.error(Command::SetDevice(p::SetDevice {
        device: SAVED,
        enabled: Some(true),
        ..Default::default()
    }));
    assert_eq!(error.code(), p::ErrorCode::NoCapacity);
    assert_eq!(error.reason(), p::ErrorReason::Radio);
    assert_eq!(error.outcome(), p::ErrorOutcome::Complete);
    assert!(t.saved(SAVED).enabled);
    assert!(t.app.manager.bonds_pending);
    assert!(t.app.manager.storage_ready);
    assert!(
        t.events()
            .iter()
            .any(|e| matches!(e, Ev::Device(d) if d.id == SAVED && d.enabled))
    );
    t.radio.reject_import = None;
    block_on(t.app.manager.sync_bonds(&mut t.store, &mut t.radio)).unwrap();
    assert!(t.radio.bonds.contains(&peer(1)));
    assert!(!t.app.manager.bonds_pending);
}

#[test]
fn committed_disable_and_block_publish_the_saved_policy_after_forget_failure() {
    for blocked in [false, true] {
        let mut t = Test::new(true);
        t.radio.reject_forget = true;
        let error = t.error(Command::SetDevice(p::SetDevice {
            device: SAVED,
            enabled: (!blocked).then_some(false),
            blocked: blocked.then_some(true),
            ..Default::default()
        }));
        assert_eq!(error.code(), p::ErrorCode::StorageFailed);
        assert_eq!(error.reason(), p::ErrorReason::Radio);
        assert_eq!(error.outcome(), p::ErrorOutcome::Complete);
        assert_eq!(t.saved(SAVED).blocked, blocked);
        assert_eq!(t.saved(SAVED).enabled, blocked);
        assert!(t.app.manager.find(SAVED.into()).is_none());
        assert!(t.app.manager.bonds_pending);
        assert!(t.app.manager.storage_ready);
        assert!(t.events().iter().any(|e| matches!(e, Ev::Device(d)
            if d.id == SAVED && d.blocked == blocked && d.enabled == blocked)));
        t.radio.reject_forget = false;
        block_on(t.app.manager.sync_bonds(&mut t.store, &mut t.radio)).unwrap();
        assert!(!t.radio.bonds.contains(&peer(1)));
    }
}

#[test]
fn failed_unpair_publishes_the_paused_device_and_restoration_error() {
    for deferred in [false, true] {
        let mut t = Test::new(true);
        let link = deferred.then(|| connect_saved(&mut t, descriptor()));
        if deferred {
            t.events();
        }
        assert!(!t.app.manager.devices[0].as_ref().unwrap().paused);
        t.store.fail_remove = Some(cordial_core::storage::record_key(2, SAVED.into()));
        t.radio.reject_import = Some(ErrorCode::Busy);
        let command = Command::UnpairDevice(p::UnpairDevice { device: SAVED });
        let response = if let Some(link) = link {
            t.ok(command);
            t.event(Event::Disconnected { link, error: None });
            None
        } else {
            Some(t.error(command))
        };
        let events = t.events();
        let device = events
            .iter()
            .find_map(|e| match e {
                Ev::Device(d) if d.id == SAVED && !d.errors.is_empty() => Some(d),
                _ => None,
            })
            .expect("the unpair failure has a device event");
        assert!(device.paused);
        let error = &device.errors[0];
        assert_eq!(error.code(), p::ErrorCode::StorageFailed);
        assert_eq!(error.reason(), p::ErrorReason::Storage);
        assert_eq!(error.outcome(), p::ErrorOutcome::Unspecified);
        assert_eq!(
            error.related,
            [p::Error {
                code: p::ErrorCode::Busy as i32,
                reason: p::ErrorReason::Radio as i32,
                ..Default::default()
            }]
        );
        if let Some(response) = response {
            assert_eq!(response, *error);
        }
        assert!(t.saved(SAVED).enabled);
        assert!(t.app.manager.storage_ready);
        assert!(t.app.manager.bonds_pending);
        assert!(!t.app.manager.devices[0].as_ref().unwrap().deleting);
        t.radio.reject_import = None;
        t.now += FIRST_RETRY;
        t.events();
        assert!(t.radio.bonds.contains(&peer(1)));
        assert!(!t.app.manager.bonds_pending);
    }
}

#[test]
fn deferred_unpair_publishes_unknown_deletion_without_reading_the_deleted_record() {
    let mut t = Test::new(true);
    let link = connect_saved(&mut t, descriptor());
    t.events();
    t.ok(Command::UnpairDevice(p::UnpairDevice { device: SAVED }));
    t.store.unknown = Some(cordial_core::storage::record_key(2, SAVED.into()));
    t.event(Event::Disconnected { link, error: None });
    let events = t.events();
    let device = events
        .iter()
        .find_map(|e| match e {
            Ev::Device(d) if d.id == SAVED && !d.errors.is_empty() => Some(d),
            _ => None,
        })
        .expect("the unresolved deletion has a device event");
    assert!(device.paused);
    assert_eq!(device.state(), p::DeviceState::Disconnected);
    assert_eq!(
        device.errors,
        [p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            reason: p::ErrorReason::Storage as i32,
            outcome: p::ErrorOutcome::Unknown as i32,
            ..Default::default()
        }]
    );
    assert!(!t.app.manager.storage_ready);
    assert!(!t.radio.bonds.contains(&peer(1)));
    assert!(
        !t.store
            .records
            .contains_key(&cordial_core::storage::record_key(2, SAVED.into()))
    );
    assert!(!events.iter().any(|e| matches!(e, Ev::DeviceRemoved(_))));
    assert_eq!(
        t.code(Command::GetDevice(p::GetDevice { device: SAVED })),
        p::ErrorCode::NotReady
    );
    t.event(Event::Ready);
    assert!(!t.app.manager.storage_ready);
    assert_eq!(
        t.app.manager.storage_error.as_ref().unwrap().outcome(),
        p::ErrorOutcome::Unknown
    );
}

#[test]
fn committed_enable_reports_complete_when_resident_allocation_fails() {
    let mut t = Test::new(true);
    t.set_device(p::SetDevice {
        device: SAVED,
        enabled: Some(false),
        ..Default::default()
    });
    t.events();
    t.app.manager.devices.clear();
    t.app.manager.devices.shrink_to_fit();
    let request = p::Request {
        command: Some(Command::SetDevice(p::SetDevice {
            device: SAVED,
            enabled: Some(true),
            ..Default::default()
        })),
    };
    FAIL_LAYOUT.with(|f| {
        f.set(Some(
            std::alloc::Layout::array::<Option<SavedDevice>>(4).unwrap(),
        ))
    });
    block_on(t.app.dispatch(request, &mut t.store, &mut t.radio, t.now));
    assert!(FAIL_LAYOUT.with(|f| f.get()).is_none());
    let Some(R::Error(error)) = t.response().result else {
        panic!("admission succeeded without resident memory");
    };
    assert_eq!(error.code(), p::ErrorCode::NoCapacity);
    assert_eq!(error.reason(), p::ErrorReason::Unknown);
    assert_eq!(error.outcome(), p::ErrorOutcome::Complete);
    assert!(t.saved(SAVED).enabled);
    assert!(t.app.manager.vacated);
    assert!(t.app.manager.find(SAVED.into()).is_none());
    assert!(
        t.events()
            .iter()
            .any(|e| matches!(e, Ev::Device(d) if d.id == SAVED && d.enabled))
    );
    assert!(t.app.manager.find(SAVED.into()).is_some());
    assert!(t.radio.bonds.contains(&peer(1)));
}
