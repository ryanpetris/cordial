use cordial_core::model::{
    errors::ErrorCode,
    hidpp::{FeatureId, FeatureRevision},
    identifiers::{ConnectionState, HostPlatform, Transport},
    settings::{SettingKey, SettingScope},
};
use cordial_core::{
    compact::{Metadata, Preference},
    devices::*,
    settings::PreferenceStore,
    storage::{Error, Preferences, RecordKey, RecordStore},
};
use embassy_futures::block_on;
use std::collections::BTreeMap;

#[derive(Default)]
struct Memory {
    values: BTreeMap<RecordKey, Vec<u8>>,
    fail_remove: bool,
}
impl RecordStore for Memory {
    async fn keys(&mut self) -> Result<Vec<RecordKey>, Error> {
        Ok(self.values.keys().copied().collect())
    }
    async fn available(&mut self) -> Result<usize, Error> {
        Ok(65536)
    }

    async fn load(&mut self, key: RecordKey, value: &mut [u8]) -> Result<Option<usize>, Error> {
        let Some(bytes) = self.values.get(&key) else {
            return Ok(None);
        };
        if bytes.len() > value.len() {
            return Err(Error::TooLarge);
        }
        value[..bytes.len()].copy_from_slice(bytes);
        Ok(Some(bytes.len()))
    }
    async fn save(&mut self, key: RecordKey, value: &[u8]) -> Result<(), Error> {
        self.values.insert(key, value.into());
        Ok(())
    }
    async fn remove(&mut self, key: RecordKey) -> Result<(), Error> {
        if self.fail_remove {
            return Err(Error::Io);
        }
        self.values.remove(&key);
        Ok(())
    }
}
fn peer(n: u8) -> Peer {
    Peer {
        address: [1, 2, 3, 4, 5, n],
        random: false,
        transport: Transport::Ble,
    }
}
#[test]
fn policies_have_independent_records_and_unpair_clears_only_the_owner() {
    block_on(async {
        let mut memory = Memory::default();
        let mut policies = Policies { store: &mut memory };
        assert_eq!(
            policies.load_adapter().await.unwrap().host_platform,
            HostPlatform::Linux
        );
        policies
            .save_adapter(&AdapterPreference {
                name: None,
                host_platform: HostPlatform::Mac,
                transports: Transports::default(),
                ..Default::default()
            })
            .await
            .unwrap();
        for slot in 0..8 {
            let p = Policy::paired(slot as u64 + 1, peer(slot as u8), &[b'"'; 128]);
            assert!(p.trusted && !p.hidpp_enabled() && p.setup_pending && !p.blocked);
            assert!(p.valid());
            let bond = cordial_core::bonds::Bond {
                owner: p.id,
                identity: p.peer,
                complete: true,
                keys: cordial_core::bonds::Keys::Ble {
                    local: cordial_core::bonds::Security {
                        flags: 1,
                        key_size: 16,
                        ..Default::default()
                    },
                    peer: Default::default(),
                },
            };
            cordial_core::bonds::commit(policies.store, &p, &bond)
                .await
                .unwrap();
        }
        assert_eq!(policies.store.record_ids(2, 0, 100).await.unwrap().len(), 8);
        let pref = Preference {
            metadata: Metadata {
                key: SettingKey::BacklightEnabled,
                feature: FeatureId::BACKLIGHT,
                revision: FeatureRevision(0),
                scope: SettingScope::Device,
                choices: Box::new([]),
                range: None,
            },
            value: 1,
        };
        for device in [1, 2] {
            Preferences {
                store: policies.store,
                device,
            }
            .save(&pref)
            .await
            .unwrap();
        }
        policies.store.fail_remove = true;
        assert_eq!(policies.remove(1).await, Err(Error::Io));
        assert_eq!(policies.store.record_ids(2, 0, 100).await.unwrap().len(), 8);
        policies.store.fail_remove = false;
        policies.remove(1).await.unwrap();
        policies.remove(1).await.unwrap();
        assert_eq!(policies.store.record_ids(2, 0, 100).await.unwrap().len(), 7);
        assert!(
            Preferences {
                store: policies.store,
                device: 1
            }
            .load_all()
            .await
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            Preferences {
                store: policies.store,
                device: 2
            }
            .load_all()
            .await
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            policies.load_adapter().await.unwrap().host_platform,
            HostPlatform::Mac
        );
        // A record whose bond belongs to another device cannot be saved over.
        let mut moved = policies.load(3).await.unwrap();
        moved.peer = peer(9);
        assert_eq!(policies.save(&moved).await, Err(Error::Missing));
        assert_eq!(policies.load(1).await, Err(Error::Missing));
    });
}
/// The first delay after a failure, which doubles per further failure.
const FIRST: u64 = RETRY_DELAY_MS as u64;
/// Whether the device may reconnect, which after a failure is the same for
/// BLE admission and Classic paging.
fn due(d: &Device, now: u64) -> bool {
    assert_eq!(d.admit_due(now), d.page_due(now));
    d.admit_due(now)
}
#[test]
fn a_clean_loss_admits_at_once_and_pages_after_the_first_delay() {
    let mut d = Device::new(&Policy::paired(10, peer(1), b"Keyboard"));
    d.connection(ConnectionState::Connected, None, 0);
    d.lost(1000);
    assert!(d.admit_due(1000));
    let paged = 1000 + FIRST;
    assert!(!d.page_due(paged - 1));
    assert!(d.page_due(paged));
    // A failed page or setup afterwards backs off from there, as before.
    d.connection(ConnectionState::Connecting, None, paged);
    d.connection(
        ConnectionState::Disconnected,
        Some(ErrorCode::Timeout),
        paged,
    );
    assert!(!due(&d, paged + 2 * FIRST - 1));
    assert!(due(&d, paged + 2 * FIRST));
    // Seeing the device advertise does not shorten that below the first delay.
    let mut ble = Device::new(&Policy::paired(
        11,
        Peer {
            transport: Transport::Ble,
            ..peer(2)
        },
        b"Mouse",
    ));
    ble.connection(ConnectionState::Connected, None, 0);
    ble.lost(0);
    ble.connection(ConnectionState::Connecting, None, 100);
    ble.connection(ConnectionState::Disconnected, Some(ErrorCode::Timeout), 100);
    ble.seen(200);
    assert!(!ble.admit_due(100 + FIRST - 1));
    assert!(ble.admit_due(100 + FIRST));
}
#[test]
fn reconnect_policy_and_names_survive_normal_runtime_transitions() {
    let p = Policy::paired(10, peer(1), b"Example\x1b\xff keyboard");
    assert_eq!(&*p.name, "Example ? keyboard");
    let text = "é".repeat(65);
    assert_eq!(display_name(text.as_bytes()).len(), 128);
    let mut d = Device::new(&p);
    assert!(due(&d, 0));
    d.connection(ConnectionState::Connecting, None, 0);
    assert!(!due(&d, 100));
    d.connection(
        ConnectionState::Disconnected,
        Some(ErrorCode::ConnectionFailed),
        100,
    );
    assert!(!due(&d, 100 + FIRST - 1));
    assert!(due(&d, 100 + FIRST));
    d.trusted = false;
    assert!(!d.allow_incoming());
    d.explicit_connect();
    d.connection(ConnectionState::Connected, None, 5101);
    assert_eq!(d.error, None);
    d.trusted = true;
    d.paused = true;
    assert!(!d.allow_incoming());
    d.explicit_connect();
    assert!(d.allow_incoming());
    // A retiring entry, such as a device just disabled, admits nothing.
    d.retiring = true;
    assert!(!d.allow_incoming());
    d.retiring = false;
    // An authentication failure backs off like any other failure.
    d.connection(
        ConnectionState::Disconnected,
        Some(ErrorCode::AuthenticationFailed),
        6000,
    );
    assert_eq!(d.error, Some(ErrorCode::AuthenticationFailed));
    assert!(!due(&d, 6000 + FIRST - 1));
    assert!(due(&d, 6000 + FIRST));
    // No error stops automatic reconnection; failures keep doubling.
    for (at, error) in [
        (20_000, ErrorCode::UnsupportedHid),
        (40_000, ErrorCode::UnsupportedTransport),
    ] {
        d.connection(ConnectionState::Connecting, None, at);
        d.connection(ConnectionState::Disconnected, Some(error), at);
        assert_eq!(d.error, Some(error));
        assert!(d.watch_for_return() || d.peer.transport != Transport::Ble);
    }
    assert!(!due(&d, 40_000 + 4 * FIRST - 1));
    assert!(due(&d, 40_000 + 4 * FIRST));
}
#[test]
fn setup_pending_is_saved_only_while_set() {
    let mut p = Policy::paired(10, peer(1), b"Keyboard");
    let bond = cordial_core::bonds::Bond {
        owner: p.id,
        identity: p.peer,
        complete: true,
        keys: cordial_core::bonds::Keys::Ble {
            local: cordial_core::bonds::Security {
                flags: 1,
                key_size: 16,
                ..Default::default()
            },
            peer: Default::default(),
        },
    };
    let save = |p: &Policy| {
        cordial_core::storage::json(&cordial_core::bonds::DeviceRecord {
            policy: p.clone(),
            bond: bond.clone(),
        })
        .unwrap()
    };
    let pending = save(&p);
    assert!(
        std::str::from_utf8(&pending)
            .unwrap()
            .contains("\"setup_pending\":true")
    );
    assert!(
        cordial_core::codec::read_policy(10, &pending)
            .unwrap()
            .setup_pending
    );
    p.setup_pending = false;
    let done = save(&p);
    assert!(
        !std::str::from_utf8(&done)
            .unwrap()
            .contains("setup_pending")
    );
    assert_eq!(
        cordial_core::codec::read_policy(10, &done).unwrap(),
        Policy { bond: 10, ..p }
    );
}

#[test]
fn only_ble_is_enabled_unless_saved_otherwise() {
    block_on(async {
        let mut memory = Memory::default();
        let mut policies = Policies { store: &mut memory };
        let ble_only = policies.load_adapter().await.unwrap().transports;
        assert!(ble_only.contains(Transport::Ble) && !ble_only.contains(Transport::Classic));
        // A saved preference without the field.
        policies
            .store
            .save(
                cordial_core::storage::record_key(1, 0),
                br#"{"name":"Desk","host_platform":"mac"}"#,
            )
            .await
            .unwrap();
        let saved = policies.load_adapter().await.unwrap();
        assert_eq!(saved.name.as_deref(), Some("Desk"));
        assert_eq!(saved.transports, ble_only);
        // The enabled transports are saved by name, and none is a valid choice.
        for (names, enabled) in [
            (
                r#"["ble","classic"]"#,
                &[Transport::Classic, Transport::Ble][..],
            ),
            (r#"["classic"]"#, &[Transport::Classic]),
            ("[]", &[]),
        ] {
            let mut transports = Transports::NONE;
            for t in enabled {
                transports.set(*t, true);
            }
            let preference = AdapterPreference {
                transports,
                ..saved.clone()
            };
            policies.save_adapter(&preference).await.unwrap();
            assert_eq!(policies.load_adapter().await.unwrap(), preference);
            let json = format!(r#"{{"name":"Desk","host_platform":"mac","transports":{names}}}"#);
            policies
                .store
                .save(cordial_core::storage::record_key(1, 0), json.as_bytes())
                .await
                .unwrap();
            assert_eq!(policies.load_adapter().await.unwrap(), preference);
        }
    });
}

#[test]
fn only_repeated_rapid_drops_delay_readmission() {
    let ble = Peer {
        transport: Transport::Ble,
        ..peer(2)
    };
    let mut d = Device::new(&Policy::paired(11, ble, b"Mouse"));
    let mut now = 0;
    // Drops within a second of connecting: two are readmitted at once, then the wait
    // doubles from one second to at most five.
    let (first, max) = (RAPID_DROP_DELAY_MS, RAPID_DROP_DELAY_MAX_MS);
    for delay in [0, 0, first, 2 * first, 4 * first, max, max] {
        d.connection(ConnectionState::Connected, None, now);
        now += RAPID_DROP_MS - 1;
        d.lost(now);
        if delay > 0 {
            assert!(!d.admit_due(now + delay - 1), "{delay}");
        }
        assert!(d.admit_due(now + delay), "{delay}");
        now += delay;
    }
    // A connection lasting a second resets the count.
    d.connection(ConnectionState::Connected, None, now);
    now += RAPID_DROP_MS;
    d.lost(now);
    assert!(d.admit_due(now));
    for _ in 0..RAPID_DROPS_ADMITTED {
        d.connection(ConnectionState::Connected, None, now);
        d.lost(now);
        assert!(d.admit_due(now));
    }
    d.connection(ConnectionState::Connected, None, now);
    d.lost(now);
    assert!(!d.admit_due(now + RAPID_DROP_DELAY_MS - 1));
    assert!(d.admit_due(now + RAPID_DROP_DELAY_MS));
    // Paging still waits the first retry delay after any clean loss, and failures still double
    // from it.
    assert!(!d.page_due(now + FIRST - 1));
    assert!(d.page_due(now + FIRST));
    let mut d = Device::new(&Policy::paired(12, peer(3), b"Keyboard"));
    for (at, delay) in [(0, FIRST), (10_000, 2 * FIRST), (30_000, 4 * FIRST)] {
        d.connection(ConnectionState::Connecting, None, at);
        d.connection(ConnectionState::Disconnected, Some(ErrorCode::Timeout), at);
        assert!(!due(&d, at + delay - 1));
        assert!(due(&d, at + delay));
    }
    d.connection(ConnectionState::Connected, None, 100_000);
    d.lost(100_000 + 60_000);
    assert!(d.admit_due(160_000));
    assert!(!d.page_due(160_000 + FIRST - 1));
    assert!(d.page_due(160_000 + FIRST));
}

#[test]
fn an_explicit_connect_resets_every_backoff() {
    let ble = Peer {
        transport: Transport::Ble,
        ..peer(2)
    };
    let mut d = Device::new(&Policy::paired(11, ble, b"Mouse"));
    let mut now = 0;
    for _ in 0..4 {
        d.connection(ConnectionState::Connecting, None, now);
        d.connection(ConnectionState::Disconnected, Some(ErrorCode::Timeout), now);
        now += 100_000;
    }
    // Four failures leave the delay at eight times the first.
    let last = now - 100_000;
    assert!(!due(&d, last + 8 * FIRST - 1));
    assert!(due(&d, last + 8 * FIRST));
    // Rapid drops count up as well.
    for _ in 0..3 {
        d.connection(ConnectionState::Connected, None, now);
        d.lost(now);
    }
    assert!(!d.admit_due(now));
    d.explicit_connect();
    assert!(d.admit_due(now) && d.page_due(now));
    d.connection(ConnectionState::Connecting, None, now);
    d.connection(
        ConnectionState::Disconnected,
        Some(ErrorCode::AuthenticationFailed),
        now,
    );
    assert!(!due(&d, now + FIRST - 1));
    assert!(due(&d, now + FIRST));
    // The rapid-drop count starts again too.
    for _ in 0..RAPID_DROPS_ADMITTED {
        now += 10_000;
        d.explicit_connect();
        d.connection(ConnectionState::Connected, None, now);
        d.lost(now);
        assert!(d.admit_due(now));
    }
}

#[test]
fn failures_double_from_the_first_delay_up_to_the_cap() {
    let mut d = Device::new(&Policy::paired(10, peer(1), b"Keyboard"));
    let mut now = 0;
    let mut expected = FIRST;
    for _ in 0..12 {
        d.connection(ConnectionState::Connecting, None, now);
        d.connection(ConnectionState::Disconnected, Some(ErrorCode::Timeout), now);
        assert!(!due(&d, now + expected - 1), "{expected}");
        assert!(due(&d, now + expected), "{expected}");
        now += expected;
        expected = (expected * 2).min(RETRY_DELAY_MAX_MS.into());
    }
    assert_eq!(expected, u64::from(RETRY_DELAY_MAX_MS));
}
