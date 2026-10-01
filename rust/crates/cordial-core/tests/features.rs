use cordial_core::{
    compact::{Preference, Record},
    features::Engine,
    hid::HIDPP_LONG,
    hidpp::{Client, TIMEOUT_MS},
    settings::{Catalog, Error as StoreError, PreferenceStore, Saved},
};
use cordial_protocol::{
    errors::ErrorCode,
    hidpp::FeatureId,
    identifiers::SettingsState,
    settings::{
        ObservationSource, SettingKey as K, SettingOutcome, SettingState, SettingValue as V,
    },
};
use embassy_futures::block_on;

#[derive(Default)]
struct Store {
    saved: Vec<Preference>,
    writes: usize,
}
impl PreferenceStore for Store {
    async fn save(&mut self, p: &Preference) -> Result<(), StoreError> {
        self.writes += 1;
        self.saved.retain(|v| v.metadata.key != p.metadata.key);
        self.saved.push(p.clone());
        Ok(())
    }
    async fn remove(&mut self, k: K) -> Result<(), StoreError> {
        self.saved.retain(|p| p.metadata.key != k);
        Ok(())
    }
    async fn remove_all(&mut self) -> Result<(), StoreError> {
        self.saved.clear();
        Ok(())
    }
}
struct Peer {
    battery: [u8; 3],
    features: Vec<(u16, u8, u8)>,
    backlight: [u8; 16],
    function: u8,
    mode: u8,
    threshold: u8,
    hires: u8,
    thumb: u8,
    dpi: [u16; 2],
    effect: u8,
    backlight_levels: u8,
    writes: Vec<(u16, u8, [u8; 16])>,
    requests: Vec<(u16, u8)>,
    ignored_setter: bool,
    hardware_entity: bool,
}
impl Default for Peer {
    fn default() -> Self {
        Self {
            battery: [100, 6, 70],
            features: vec![
                (1, 2, 0),
                (3, 4, 0),
                (5, 2, 0),
                (0x1000, 0, 0),
                (0x40a2, 0, 0),
                (0x1982, 3, 0),
                (0x2201, 1, 0),
                (0x2110, 0, 0),
                (0x2121, 1, 0),
                (0x2150, 0, 0),
                (0x40a0, 0, 0),
                (0xdead, 1, 0x40),
            ],
            backlight: [1, 0x0d, 0x3d, 7, 0, 3, 6, 0, 12, 0, 18, 0, 0, 0, 0, 0],
            function: 0,
            mode: 2,
            threshold: 20,
            hires: 0,
            thumb: 0,
            dpi: [800, 1600],
            effect: 0,
            backlight_levels: 8,
            writes: vec![],
            requests: vec![],
            ignored_setter: false,
            hardware_entity: false,
        }
    }
}
impl Peer {
    fn answer(&mut self, packet: [u8; 19]) -> [u8; 19] {
        let mut p = [0u8; 16];
        let command = packet[2] >> 4;
        let a = &packet[3..];
        let id = if packet[1] == 0 {
            0
        } else {
            self.features[packet[1] as usize - 1].0
        };
        self.requests.push((id, command));
        let mut write = false;
        let version = if packet[1] == 0 {
            0
        } else {
            self.features[packet[1] as usize - 1].1
        };
        match (id, command) {
            (0, 0) => {
                let id = u16::from_be_bytes([a[0], a[1]]);
                let i = self.features.iter().position(|f| f.0 == id).unwrap();
                assert_eq!(self.features[i].2 & 0x60, 0);
                p[..3].copy_from_slice(&[(i + 1) as u8, self.features[i].2, self.features[i].1]);
            }
            (1, 0) => p[0] = self.features.len() as u8,
            (1, 1) => {
                let f = self.features[a[0] as usize - 1];
                p[..2].copy_from_slice(&f.0.to_be_bytes());
                p[2] = f.2;
                p[3] = f.1;
            }
            (3, 0) => {
                p[0] = if self.hardware_entity { 3 } else { 2 };
                p[1..5].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
                p[6] = 2;
                p[7] = 0xab;
                p[8] = 0xcd;
                p[13] = 3;
                if version >= 4 {
                    p[14] = 1;
                }
            }
            (3, 1) => {
                p[..8].copy_from_slice(&[a[0], b'A', b'B', b'C', 0x12, 0x34, 0x56, 0x78]);
                if a[0] == 2 {
                    p[1] = 7;
                }
            }
            (3, 2) => p[..12].copy_from_slice(b"ABCD12345678"),
            (5, 0) => p[0] = 22,
            (5, 1) => {
                let name = b"Example Bluetooth keys";
                let n = (name.len() - a[0] as usize).min(16);
                p[..n].copy_from_slice(&name[a[0] as usize..a[0] as usize + n]);
            }
            (5, 2) => p[0] = 0,
            (0x1000, 1) => p[..2].copy_from_slice(&self.battery[..2]),
            (0x1000, 0) => p[..3].copy_from_slice(&[self.battery[2], 50, 0]),
            (0x1004, 0) => p[..2].copy_from_slice(&[15, 2]),
            (0x1004, 1) => p[..4].copy_from_slice(&[51, 4, 1, 0]),
            (0x1001, 0) => p[..3].copy_from_slice(&[0x10, 0, 0x81]),
            (0x1f20, 0) => p[..3].copy_from_slice(&[0x10, 0, 3]),
            (0x4301, 0) => {
                assert_eq!(&a[..2], &[1, 1]);
            }
            (0x40a2, 0) => p[..2].copy_from_slice(&[self.function, 1]),
            (0x40a2, 1) => {
                self.function = a[0];
                write = true;
            }
            (0x40a3, 0) => {
                assert_eq!(a[0], 0xff);
                p[..4].copy_from_slice(&[1, self.function, 1, 1]);
            }
            (0x1982, 0) => p = self.backlight,
            (0x1982, 2) => {
                p[..4].copy_from_slice(&[
                    self.backlight_levels,
                    3.min(self.backlight_levels - 1),
                    if self.backlight[1] & 0x18 == 0x18 {
                        5
                    } else {
                        2
                    },
                    self.effect,
                ]);
            }
            (0x1982, 1) => {
                self.backlight[0] = a[0];
                self.backlight[1] = a[1];
                if version >= 2 {
                    assert!(a[2] == 0xff || a[2] < 7);
                    if a[2] != 0xff {
                        self.effect = a[2];
                    }
                }
                if version >= 3 {
                    self.backlight[5] = a[3];
                    self.backlight[6..12].copy_from_slice(&a[4..10]);
                }
                write = true;
            }
            (0x2201, 0) => p[0] = 2,
            (0x2201, 1) => p[..7].copy_from_slice(&[a[0], 1, 0x90, 3, 0x20, 6, 0x40]),
            (0x2201, 2) => {
                p[0] = a[0];
                p[1..3].copy_from_slice(&self.dpi[a[0] as usize].to_be_bytes());
                p[3] = 3;
                p[4] = 0x20;
            }
            (0x2201, 3) => {
                self.dpi[a[0] as usize] = u16::from_be_bytes([a[1], a[2]]);
                write = true;
            }
            (0x2110, 0) => p[..3].copy_from_slice(&[self.mode, self.threshold, 18]),
            (0x2110, 1) => {
                assert_eq!(a[2], 0);
                if !self.ignored_setter {
                    if a[0] != 0 {
                        self.mode = a[0];
                    }
                    if a[1] != 0 {
                        self.threshold = a[1];
                    }
                }
                write = true;
            }
            (0x2121, 0) => p[..4].copy_from_slice(&[8, 12, 0, 0]),
            (0x2121, 1) => p[0] = self.hires,
            (0x2121, 2) => {
                self.hires = a[0];
                write = true;
            }
            (0x2150, 0) => p[..8].copy_from_slice(&[0, 18, 0, 90, 0, 3, 0, 1]),
            (0x2150, 1) => p[..2].copy_from_slice(&[0, self.thumb | 6]),
            (0x2150, 2) => {
                assert_eq!(a[0], 0);
                self.thumb = a[1];
                write = true;
            }
            _ => panic!("unknown or hidden request {id:04x}/{command}"),
        }
        if write {
            self.writes.push((id, command, a.try_into().unwrap()));
        }
        let mut reply = packet;
        reply[3..].copy_from_slice(&p);
        reply
    }
}
struct Device {
    catalog: Catalog,
    engine: Engine,
    client: Client,
    peer: Peer,
    store: Store,
    now: u64,
}
impl Device {
    fn new(peer: Peer, enabled: bool) -> Self {
        let mut catalog = Catalog::default();
        catalog
            .info
            .battery
            .configure(cordial_protocol::identifiers::Transport::Classic, true);
        catalog.connection(true, enabled);
        let mut engine = Engine::default();
        engine.activate(&mut catalog, 100).unwrap();
        let mut client = Client::new(HIDPP_LONG);
        client.protocol = cordial_protocol::hidpp::ProtocolState::Detected { major: 2, minor: 0 };
        Self {
            catalog,
            engine,
            client,
            peer,
            store: Store::default(),
            now: 100,
        }
    }
    fn get(&self, key: K) -> &Record {
        self.catalog
            .records()
            .iter()
            .find(|r| r.metadata.key == key)
            .unwrap()
    }
    fn packet(&mut self) -> Option<[u8; 19]> {
        self.now += 1;
        self.client.tick(self.now);
        self.engine
            .poll(&mut self.catalog, &mut self.client, self.now);
        self.client.next_output(self.now)
    }
    fn respond(&mut self, packet: [u8; 19]) {
        let reply = self.peer.answer(packet);
        self.now += 1;
        self.client.receive(0x11, &reply, self.now);
        // Let the settings owner consume a reply before transport completion.
        self.engine
            .poll(&mut self.catalog, &mut self.client, self.now);
        assert!(self.client.next_output(self.now).is_none());
        self.client.tx_complete(true, self.now);
    }
    fn info(&self, key: cordial_protocol::info::InfoKey) -> cordial_protocol::info::InfoField {
        self.catalog
            .info
            .snapshot()
            .into_iter()
            .find(|f| f.key == key && f.instance == 0)
            .unwrap()
    }
    fn run(&mut self) {
        for _ in 0..1000 {
            if let Some(p) = self.packet() {
                self.respond(p);
            }
            if !self.engine.busy() || self.engine.done() {
                assert!(self.client.idle());
                return;
            }
        }
        panic!("job did not finish {:?}", self.engine.state);
    }
    fn set(&mut self, key: K, value: V) {
        let saved = block_on(self.catalog.set(key, value, &mut self.store)).unwrap();
        if saved == Saved::Apply {
            self.engine
                .start(&mut self.catalog, true, Some(key), false, false, self.now)
                .unwrap();
        }
    }
    fn event(&mut self, id: u16, event: u8, data: &[u8]) {
        let i = self.peer.features.iter().position(|f| f.0 == id).unwrap() + 1;
        let mut packet = [0; 19];
        packet[..3].copy_from_slice(&[0xff, i as u8, event << 4]);
        packet[3..3 + data.len()].copy_from_slice(data);
        self.now += 1;
        assert!(
            self.engine
                .receive(&mut self.catalog, 0x11, &packet, self.now)
        );
    }
}

#[test]
fn feature_enumeration_does_not_publish_unchanged_device_state() {
    let mut d = Device::new(Peer::default(), false);
    while d.catalog.records().is_empty() {
        d.now += 1;
        d.client.tick(d.now);
        let changed = d.engine.poll(&mut d.catalog, &mut d.client, d.now);
        if d.catalog.records().is_empty() {
            assert!(
                !changed,
                "internal enumeration is not a device-state change"
            );
        } else {
            assert!(changed, "publishing settings must notify the owner");
        }
        if let Some(packet) = d.client.next_output(d.now) {
            let response = d.peer.answer(packet);
            d.client.receive(0x11, &response, d.now);
            d.client.tx_complete(true, d.now);
        }
        assert!(d.now < 1000, "enumeration did not finish");
    }
    assert_eq!(d.catalog.features().len(), d.peer.features.len() + 1);
    d.run();
    assert_eq!(d.engine.state, SettingsState::Ready);
}

#[test]
fn discovery_uses_advertised_features_revisions_and_capabilities() {
    for version in 0..=5 {
        let mut peer = Peer::default();
        peer.features[0].1 = if version == 0 { 0 } else { 2 };
        peer.features[1].1 = version;
        peer.features[5].1 = version;
        if version < 3 {
            peer.backlight[1] = 1;
            peer.backlight[2] = if version == 0 { 3 } else { 7 };
        }
        let mut d = Device::new(peer, false);
        d.run();
        assert_eq!(d.engine.state, SettingsState::Ready);
        assert!(d.peer.writes.is_empty());
        assert!(!d.info(cordial_protocol::info::InfoKey::Name).available);
        assert!(
            !d.info(cordial_protocol::info::InfoKey::BatteryPercent)
                .available
        );
        assert_eq!(d.catalog.features().len(), 13);
        assert!(
            d.catalog
                .features()
                .iter()
                .filter(|f| matches!(f.id.0, 0xdead | 0x40a0))
                .all(|f| !f.supported())
        );
        assert_eq!(
            d.catalog
                .records()
                .iter()
                .any(|r| r.metadata.key == K::BacklightMode),
            version >= 3
        );
        assert!(!d.info(cordial_protocol::info::InfoKey::Serial).available);
        assert_eq!(d.get(K::PointerDpi1).wire().observed, V::Integer(1600));
        d.set(K::FnRowDefault, V::Text("special_actions".into()));
        assert!(!d.engine.busy());
        assert_eq!(d.store.writes, 1);
        assert!(d.peer.writes.is_empty());
        d.catalog.connection(true, true);
        d.engine
            .start(&mut d.catalog, true, None, true, false, d.now)
            .unwrap();
        d.run();
        assert_eq!(d.get(K::FnRowDefault).state, SettingState::Applied);
        assert_eq!(d.peer.function, 1);
        assert_eq!(d.peer.writes.len(), 1);
        d.engine.release(&mut d.catalog);
        d.set(K::BacklightEnabled, V::Bool(false));
        d.run();
        assert_eq!(d.get(K::BacklightEnabled).state, SettingState::Applied);
    }
}

#[test]
fn setters_preserve_unmanaged_fields_and_read_back_all_requested_values() {
    let mut d = Device::new(Peer::default(), true);
    d.run();
    assert!(d.peer.writes.is_empty());
    for (key, value) in [
        (K::PointerDpi0, V::Integer(1600)),
        (K::PointerDpi1, V::Integer(400)),
        (K::WheelMode, V::Text("freespin".into())),
        (K::WheelThreshold, V::Integer(77)),
        (K::WheelInvert, V::Bool(true)),
        (K::ThumbwheelInvert, V::Bool(true)),
        (K::BacklightPowerOn, V::Bool(false)),
        (K::BacklightEffect, V::Text("breathing".into())),
        (K::BacklightMode, V::Text("permanent_manual".into())),
        (K::BacklightLevel, V::Integer(6)),
        (K::BacklightDelayHandsOut, V::Integer(45)),
    ] {
        d.set(key, value);
        d.run();
        assert_eq!(
            d.get(key).state,
            SettingState::Applied,
            "{key:?}: {:?}",
            d.get(key).wire()
        );
    }
    assert_eq!(
        d.peer
            .writes
            .iter()
            .filter(|w| w.0 == 0x2110)
            .map(|w| w.2[..3].to_vec())
            .collect::<Vec<_>>(),
        vec![vec![1, 0, 0], vec![0, 77, 0]]
    );
    assert_eq!(d.peer.backlight[1] & 7, 4);
    assert_eq!(d.peer.backlight[8], 12);
    assert_eq!(d.peer.backlight[10], 18);
    let writes = d.peer.writes.len();
    d.set(K::WheelInvert, V::Bool(true));
    d.run();
    assert_eq!(d.peer.writes.len(), writes);
    // Already matching inversion needs no routing changes even in device-selected native mode.
    d.peer.hires |= 1;
    d.set(K::WheelInvert, V::Bool(true));
    d.run();
    assert_eq!(d.peer.writes.len(), writes);
}

#[test]
fn notifications_do_not_save_or_correct_values_and_stale_reads_are_retried() {
    let mut d = Device::new(Peer::default(), true);
    d.run();
    d.set(K::WheelMode, V::Text("ratchet".into()));
    d.run();
    let writes = d.store.writes;
    let setters = d.peer.writes.len();
    d.peer.mode = 1;
    d.event(0x2121, 1, &[0]);
    assert_eq!(d.get(K::WheelMode).state, SettingState::ChangedOnDevice);
    assert_eq!(d.get(K::WheelMode).source, Some(ObservationSource::Event));
    assert!(d.packet().is_none());
    assert_eq!(d.store.writes, writes);
    assert_eq!(d.peer.writes.len(), setters);
    d.set(K::BacklightEnabled, V::Bool(true));
    d.run();
    let saves = d.store.writes;
    let setters = d.peer.writes.len();
    d.peer.backlight[0] = 0;
    d.event(0x1982, 0, &[8, 3, 0, 0]);
    d.run();
    assert_eq!(
        d.get(K::BacklightEnabled).state,
        SettingState::ChangedOnDevice
    );
    assert_eq!(d.store.writes, saves);
    assert_eq!(d.peer.writes.len(), setters);
    // Deliver an older read reply, then an event, before the settings owner consumes it.
    d.engine
        .start(&mut d.catalog, false, None, true, false, d.now)
        .unwrap();
    let mut injected = false;
    for _ in 0..1000 {
        if let Some(p) = d.packet() {
            if p[1] == 8 && p[2] >> 4 == 0 && !injected {
                let reply = d.peer.answer(p);
                d.client.receive(0x11, &reply, d.now);
                d.client.tx_complete(true, d.now);
                d.peer.mode = 2;
                d.event(0x2121, 1, &[1]);
                injected = true;
            } else {
                d.respond(p);
            }
        }
        if d.engine.done() {
            break;
        }
    }
    assert!(injected && d.engine.done());
    assert_eq!(
        d.get(K::WheelMode).wire().observed,
        V::Text("ratchet".into())
    );
    assert_eq!(d.store.writes, saves);
}

#[test]
fn timed_out_setter_is_uncertain_and_disable_cancels_without_further_writes() {
    let mut d = Device::new(Peer::default(), true);
    d.run();
    d.set(K::FnRowDefault, V::Text("special_actions".into()));
    loop {
        let p = d.packet().unwrap();
        if p[1] == 5 && p[2] >> 4 == 1 {
            d.peer.answer(p);
            d.client.tx_complete(true, d.now);
            d.engine.poll(&mut d.catalog, &mut d.client, d.now);
            d.now += TIMEOUT_MS;
            d.client.tick(d.now);
            d.engine.poll(&mut d.catalog, &mut d.client, d.now);
            break;
        } else {
            d.respond(p);
        }
    }
    d.run();
    assert_eq!(d.get(K::FnRowDefault).state, SettingState::Uncertain);
    assert_eq!(d.get(K::FnRowDefault).error, Some(ErrorCode::HidppTimeout));
    assert_eq!(d.store.saved[0].value, 1);
    d.set(K::FnRowDefault, V::Text("function_keys".into()));
    let p = d.packet().unwrap();
    d.catalog.connection(true, false);
    d.engine.poll(&mut d.catalog, &mut d.client, d.now);
    d.respond(p);
    d.run();
    assert_eq!(d.peer.writes.len(), 1);
    assert_eq!(d.get(K::FnRowDefault).error, None);
    assert_eq!(d.get(K::FnRowDefault).state, SettingState::Pending);
    assert_eq!(d.engine.error, None);
    assert_eq!(d.engine.state, SettingsState::Off);
}

#[test]
fn multi_host_is_read_only_and_ignored_setter_reports_readback_mismatch() {
    let mut peer = Peer::default();
    peer.features[4].0 = 0x40a3;
    peer.ignored_setter = true;
    let mut d = Device::new(peer, true);
    d.run();
    assert!(!d.get(K::FnRowDefault).writable);
    assert_eq!(
        d.get(K::FnRowDefault).metadata.feature,
        FeatureId::FN_INVERSION_MULTI_HOST
    );
    d.set(K::WheelThreshold, V::Integer(90));
    d.engine.cancel(ErrorCode::Cancelled);
    d.run();
    d.engine
        .start(&mut d.catalog, true, None, true, false, d.now)
        .unwrap();
    d.run();
    assert!(
        d.engine
            .results()
            .iter()
            .any(|r| r.key == K::WheelThreshold && r.outcome == SettingOutcome::Failed)
    );
    assert_eq!(
        d.get(K::WheelThreshold).error,
        Some(ErrorCode::ReadbackMismatch)
    );
    assert!(d.get(K::WheelThreshold).fresh);
    assert_eq!(d.get(K::WheelThreshold).differs_from_saved(), Some(true));
}

#[test]
fn disconnect_between_setter_handoff_and_next_poll_is_uncertain() {
    let mut d = Device::new(Peer::default(), true);
    d.run();
    d.set(K::FnRowDefault, V::Text("special_actions".into()));
    loop {
        let p = d.packet().unwrap();
        if p[1] == 5 && p[2] >> 4 == 1 {
            d.peer.answer(p);
            d.engine.disconnected(&mut d.catalog, &d.client);
            assert_eq!(d.peer.function, 1);
            assert_eq!(d.get(K::FnRowDefault).state, SettingState::Uncertain);
            assert_eq!(d.get(K::FnRowDefault).error, Some(ErrorCode::NotConnected));
            break;
        } else {
            d.respond(p);
        }
    }
}

#[test]
fn rediscovery_stales_unavailable_rows_and_transient_failures_are_not_unsupported() {
    let mut d = Device::new(Peer::default(), true);
    d.run();
    assert!(d.get(K::FnRowDefault).fresh);
    d.peer.features[4].2 = 0x40;
    d.engine
        .start(&mut d.catalog, false, None, true, true, d.now)
        .unwrap();
    d.run();
    assert!(!d.get(K::FnRowDefault).available);
    assert!(!d.get(K::FnRowDefault).fresh);
    d.engine.release(&mut d.catalog);
    d.peer.features[4].2 = 0;
    d.engine.activate(&mut d.catalog, d.now).unwrap();
    d.run();
    d.set(K::FnRowDefault, V::Text("special_actions".into()));
    d.run();
    d.engine.activate(&mut d.catalog, d.now).unwrap();
    let mut timed_out = false;
    for _ in 0..1000 {
        if let Some(p) = d.packet() {
            if p[1] == 5 && p[2] >> 4 == 0 && !timed_out {
                d.client.tx_complete(true, d.now);
                d.now += TIMEOUT_MS;
                d.client.tick(d.now);
                timed_out = true;
            } else {
                d.respond(p);
            }
        }
        if !d.engine.busy() {
            break;
        }
    }
    assert!(timed_out && !d.engine.busy());
    assert!(!d.get(K::FnRowDefault).available);
    assert_eq!(d.get(K::FnRowDefault).error, Some(ErrorCode::HidppTimeout));
    let writes = d.store.writes;
    assert_eq!(
        block_on(d.catalog.set(
            K::FnRowDefault,
            V::Text("function_keys".into()),
            &mut d.store
        )),
        Err(StoreError::SettingsUnavailable)
    );
    assert_eq!(d.store.writes, writes);
}

#[test]
fn background_cancellation_keeps_completed_reads_and_disconnect_is_not_a_setting_error() {
    for disconnect in [false, true] {
        let mut d = Device::new(Peer::default(), true);
        for _ in 0..1000 {
            let p = d.packet().unwrap();
            if p[1] == 6 {
                break;
            } // Earlier name, battery and Fn reads completed.
            d.respond(p);
        }
        assert!(d.info(cordial_protocol::info::InfoKey::Name).fresh);
        if disconnect {
            d.engine.disconnected(&mut d.catalog, &d.client);
            assert_eq!(d.engine.error, None);
            assert_eq!(d.engine.state, SettingsState::Pending);
            assert!(!d.info(cordial_protocol::info::InfoKey::Name).fresh);
        } else {
            d.now += 90_000;
            d.client.tx_complete(true, d.now);
            d.client.tick(d.now);
            d.engine.poll(&mut d.catalog, &mut d.client, d.now);
            assert_eq!(d.engine.error, Some(ErrorCode::Timeout));
            assert!(d.info(cordial_protocol::info::InfoKey::Name).fresh);
            assert!(
                d.info(cordial_protocol::info::InfoKey::BatteryPercent)
                    .fresh
            );
        }
        assert_eq!(d.get(K::FnRowDefault).error, None);
    }
}

#[test]
fn backlight_level_unsupported_diagnostic_recovers_with_current_capabilities() {
    let mut d = Device::new(Peer::default(), true);
    d.run();
    d.set(K::BacklightMode, V::Text("permanent_manual".into()));
    d.run();
    d.set(K::BacklightLevel, V::Integer(6));
    d.run();
    d.set(K::BacklightMode, V::Text("automatic".into()));
    d.run();
    for (levels, error) in [(3, Some(ErrorCode::UnsupportedSetting)), (8, None)] {
        d.peer.backlight_levels = levels;
        d.engine
            .start(&mut d.catalog, false, None, true, true, d.now)
            .unwrap();
        d.run();
        assert_eq!(d.get(K::BacklightLevel).error, error);
        assert!(!d.get(K::BacklightLevel).fresh);
        d.engine.release(&mut d.catalog);
    }
    d.engine
        .start(&mut d.catalog, true, None, true, false, d.now)
        .unwrap();
    d.catalog.connection(true, false);
    d.engine.poll(&mut d.catalog, &mut d.client, d.now);
    assert!(d.engine.done());
    assert_eq!(d.engine.state, SettingsState::Off);
    assert_eq!(d.engine.error, None);
    assert!(
        d.engine
            .results()
            .iter()
            .any(|r| r.outcome == SettingOutcome::Failed)
    );
}

#[test]
fn information_refresh_preserves_settings_and_hardware_entity_type() {
    use cordial_protocol::info::InfoKey as I;
    let mut d = Device::new(
        Peer {
            hardware_entity: true,
            ..Peer::default()
        },
        true,
    );
    d.run();
    assert_eq!(d.info(I::Hardware).value, V::Text("7".into()));
    assert_eq!(d.info(I::Firmware).value, V::Text("ABC 12.34.5678".into()));
    d.set(K::WheelInvert, V::Bool(true));
    d.run();
    let before = d.get(K::WheelInvert).wire();
    let writes = d.store.writes;
    d.peer.requests.clear();
    d.engine.start_information(&mut d.catalog, d.now).unwrap();
    d.run();
    assert_eq!(d.get(K::WheelInvert).wire(), before);
    assert_eq!(d.store.writes, writes);
    assert!(
        d.peer
            .requests
            .iter()
            .all(|(id, _)| [3, 5, 0x1000].contains(id))
    );
    let saved = d.catalog.preferences().cloned().collect();
    d.catalog = Catalog::default();
    d.catalog
        .info
        .battery
        .configure(cordial_protocol::identifiers::Transport::Classic, true);
    d.catalog.restore_preferences(saved).unwrap();
    d.catalog.connection(true, true);
    let before = d.get(K::WheelInvert).wire();
    d.engine.start_information(&mut d.catalog, d.now).unwrap();
    assert!(!d.engine.busy());
    assert_eq!(d.get(K::WheelInvert).wire(), before);
}

#[test]
fn battery_interfaces_on_classic_and_ble() {
    use cordial_protocol::{identifiers::Transport, info::InfoKey as I};
    for (feature, percentage, charging) in [
        (0x1004, V::Integer(51), V::Bool(true)),
        (0x1001, V::Integer(100), V::Bool(false)),
        (0x1f20, V::Null, V::Bool(true)),
        (0x4301, V::Null, V::Null),
    ] {
        for transport in [Transport::Classic, Transport::Ble] {
            let mut peer = Peer::default();
            peer.features[3].0 = feature;
            let mut d = Device::new(peer, true);
            d.catalog.info.battery.configure(transport, true);
            d.run();
            assert_eq!(
                d.info(I::BatteryPercent).value,
                percentage,
                "{transport:?} {feature:x}"
            );
            assert_eq!(
                d.info(I::BatteryCharging).value,
                charging,
                "{transport:?} {feature:x}"
            );
        }
    }
}

#[test]
fn battery_events_preserve_capabilities_and_supersede_pending_status() {
    use cordial_protocol::info::InfoKey as I;
    for fail in [false, true] {
        let mut peer = Peer::default();
        peer.features[3].0 = 0x1004;
        let mut d = Device::new(peer, true);
        let mut capabilities = false;
        let mut status = false;
        for _ in 0..1000 {
            let Some(packet) = d.packet() else { continue };
            if packet[1] == 4 {
                let mut event = [0u8; 19];
                event[..7].copy_from_slice(&[0xff, 4, 0, 49, 4, 1, 0]);
                d.engine.receive(&mut d.catalog, 0x11, &event, d.now);
                if packet[2] >> 4 == 0 {
                    capabilities = true;
                    d.respond(packet);
                } else {
                    status = true;
                    if fail {
                        d.client.tx_complete(false, d.now);
                        d.engine.poll(&mut d.catalog, &mut d.client, d.now);
                    } else {
                        d.respond(packet);
                    }
                    assert_eq!(d.info(I::BatteryPercent).value, V::Integer(49));
                    assert_eq!(d.info(I::BatteryCharging).value, V::Bool(true));
                    break;
                }
            } else {
                d.respond(packet);
            }
        }
        assert!(capabilities && status);
    }
}

#[test]
fn battery_status_capability_bands_and_zero_unknown() {
    use cordial_protocol::info::InfoKey as I;
    for (battery, expected) in [
        ([100, 2, 0], V::Null),
        ([100, 2, 50], V::Integer(50)),
        ([100, 0, 50], V::Integer(75)),
        ([4, 2, 25], V::Integer(20)),
        ([4, 0, 90], V::Integer(100)),
        ([4, 0, 5], V::Integer(5)),
    ] {
        let mut d = Device::new(
            Peer {
                battery,
                ..Peer::default()
            },
            true,
        );
        d.run();
        assert_eq!(d.info(I::BatteryPercent).value, expected, "{battery:?}");
    }
}
