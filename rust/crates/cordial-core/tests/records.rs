//! Every saved record round-trips through its `proto/storage.proto` message.
use cordial_core::{
    bluetooth::{DatabaseHash, Layout, LayoutReport, ReportMap, ReportType},
    bonds::{self, Bond, Keys, Security},
    compact::{Metadata, Preference, Range},
    devices::{AdapterPreference, Peer, Policies, Policy, Roles, Transports},
    identity::Identity,
    interfaces::{Interface, InterfacePreference},
    layouts,
    model::{
        hidpp::{FeatureId, FeatureRevision},
        identifiers::{HostPlatform, Transport},
        settings::{SettingKey, SettingScope},
    },
    profiles,
    settings::PreferenceStore,
    storage::{self, Preferences, RecordStore, record_key},
};
use cordial_protocol::storage as saved;
use embassy_futures::block_on;
use prost::Message;
#[allow(dead_code)]
mod support;

/// The saved bytes decode as `M` and encode back to the same bytes.
fn canonical<M: Message + Default>(bytes: &[u8]) -> M {
    let message = M::decode(bytes).unwrap();
    assert_eq!(message.encode_to_vec(), bytes);
    message
}

#[test]
fn startup_records_round_trip() {
    let mut store = support::Store::default();
    let identity = block_on(Identity::initialize(&mut store, [1, 2, 3, 4, 5, 6], || {
        0x0102_0304_0506_0708
    }))
    .unwrap();
    let format: saved::Format = canonical(&store.records[&record_key(0, 0)]);
    assert_eq!(
        format,
        saved::Format {
            format: 1,
            initialized: true
        }
    );
    let saved: saved::Identity = canonical(&store.records[&cordial_core::identity::KEY]);
    assert_eq!(saved.address, [1, 2, 3, 4, 5, 6]);
    assert_eq!(saved.irk, identity.0[40..]);
    assert_eq!(block_on(Identity::load(&mut store)), Ok(Some(identity)));
    // A key of another length is undecodable.
    let mut short = saved.clone();
    short.ir.pop();
    store
        .records
        .insert(cordial_core::identity::KEY, short.encode_to_vec());
    assert_eq!(
        block_on(Identity::load(&mut store)),
        Err(storage::Error::Corrupt)
    );
    // Device and profile IDs have their own sequences and are allocated from 1.
    assert_eq!(block_on(storage::allocate(&mut store, false)), Ok(1));
    assert_eq!(block_on(storage::allocate(&mut store, true)), Ok(1));
    assert_eq!(block_on(storage::allocate(&mut store, false)), Ok(2));
    let sequence: saved::Sequence = canonical(&store.records[&record_key(7, 0)]);
    assert_eq!(
        sequence,
        saved::Sequence {
            next_device: 3,
            next_profile: 2
        }
    );
    // Once every 32-bit ID has been used, the sequence is full.
    let last = saved::Sequence {
        next_device: u64::from(u32::MAX),
        next_profile: 1,
    };
    store.records.insert(record_key(7, 0), last.encode_to_vec());
    assert_eq!(
        block_on(storage::allocate(&mut store, false)),
        Ok(u64::from(u32::MAX))
    );
    assert_eq!(
        block_on(storage::allocate(&mut store, false)),
        Err(storage::Error::Full)
    );
    assert_eq!(block_on(storage::allocate(&mut store, true)), Ok(1));
}

#[test]
fn a_store_from_another_format_is_refused() {
    // A root record that is not this format's leaves storage unopened and changes nothing.
    for root in [
        &br#"{"format":1,"initialized":true}"#[..],
        &saved::Format {
            format: 2,
            initialized: true,
        }
        .encode_to_vec(),
    ] {
        let mut store = support::Store::default();
        store.records.insert(record_key(0, 0), root.to_vec());
        let before = store.records.clone();
        assert_eq!(
            block_on(Identity::initialize(&mut store, [1; 6], || 7)),
            Err(storage::Error::Layout)
        );
        assert_eq!(store.records, before);
    }
}

#[test]
fn adapter_preferences_round_trip() {
    let mut store = support::Store::default();
    let mut transports = Transports::NONE;
    transports.set(Transport::Classic, true);
    let preference = AdapterPreference {
        name: Some("Desk".into()),
        host_platform: HostPlatform::Windows,
        transports,
        configuration_interfaces: vec![
            InterfacePreference {
                interface: Interface::Via,
                enabled: false,
                profile: Some(4),
            },
            InterfacePreference {
                interface: Interface::Vial,
                enabled: true,
                profile: Some(5),
            },
        ],
    };
    let mut policies = Policies { store: &mut store };
    block_on(policies.save_adapter(&preference)).unwrap();
    assert_eq!(block_on(policies.load_adapter()), Ok(preference.clone()));
    let mut saved: saved::Adapter = canonical(&store.records[&record_key(1, 0)]);
    assert_eq!(saved.configuration_interfaces.len(), 2);
    // An interface this firmware doesn't know is left out, and an unknown platform reads as
    // Linux.
    saved
        .configuration_interfaces
        .push(saved::InterfacePreference {
            interface: 9,
            enabled: true,
            profile: Some(6),
        });
    saved.host_platform = 9;
    store
        .records
        .insert(record_key(1, 0), saved.encode_to_vec());
    assert_eq!(
        block_on(Policies { store: &mut store }.load_adapter()),
        Ok(AdapterPreference {
            host_platform: HostPlatform::Linux,
            ..preference
        })
    );
}

fn security(seed: u8) -> Security {
    Security {
        flags: 0b10_1011,
        key_size: 16,
        ediv: 0x1234 + u16::from(seed),
        rand: [seed; 8],
        ltk: [seed + 1; 16],
        irk: [seed + 2; 16],
        csrk: [seed + 3; 16],
        counter: 0x0102_0304,
    }
}

#[test]
fn device_records_keep_the_whole_portable_bond() {
    let classic = Peer {
        address: [1, 2, 3, 4, 5, 6],
        random: false,
        transport: Transport::Classic,
    };
    let ble = Peer {
        address: [0xc1, 2, 3, 4, 5, 6],
        random: true,
        transport: Transport::Ble,
    };
    for (peer, keys) in [
        (
            classic,
            Keys::Classic {
                key: [9; 16],
                kind: 8,
            },
        ),
        (
            ble,
            Keys::Ble {
                local: security(1),
                peer: security(7),
            },
        ),
    ] {
        let mut policy = Policy::paired(12, peer, b"Keyboard");
        policy.trusted = false;
        policy.blocked = true;
        policy.set_hidpp(true);
        policy.roles = Roles(cordial_core::hid::KEYBOARD | cordial_core::hid::SYSTEM);
        policy.profiles = vec![3, 1, 3];
        policy.bond = 12;
        let bond = Bond {
            owner: 12,
            identity: peer,
            complete: true,
            keys,
        };
        let bytes = bonds::record(&policy, &bond).unwrap();
        let saved: saved::Device = canonical(&bytes);
        assert_eq!(
            saved.policy.as_ref().unwrap().roles,
            [
                i32::from(saved::Role::Keyboard),
                i32::from(saved::Role::SystemControl)
            ]
        );
        assert_eq!(bonds::read_record(12, &bytes), Ok((policy.clone(), bond)));
        // The record names its device: another directory's ID is a lost record.
        assert_eq!(bonds::read_record(13, &bytes), Err(storage::Error::Corrupt));
        // A bond whose keys are missing is undecodable.
        let mut keyless = saved;
        keyless.bond.as_mut().unwrap().keys = None;
        assert!(bonds::read_record(12, &keyless.encode_to_vec()).is_err());
    }
}

#[test]
fn settings_round_trip() {
    let mut store = support::Store::default();
    let level = Preference {
        metadata: Metadata {
            key: SettingKey::BacklightLevel,
            feature: FeatureId::BACKLIGHT,
            revision: FeatureRevision(3),
            scope: SettingScope::Device,
            choices: Box::new([]),
            range: Some(Range {
                min: 0,
                max: 7,
                step: 1,
            }),
        },
        value: 5,
    };
    let effect = Preference {
        metadata: Metadata {
            key: SettingKey::BacklightEffect,
            feature: FeatureId::BACKLIGHT,
            revision: FeatureRevision(3),
            scope: SettingScope::Device,
            choices: Box::new([0, 1, 2]),
            range: None,
        },
        value: 2,
    };
    assert!(level.valid() && effect.valid());
    let mut preferences = Preferences {
        store: &mut store,
        device: 4,
    };
    block_on(preferences.replace(&[level.clone(), effect.clone()])).unwrap();
    assert_eq!(
        block_on(preferences.load_all()),
        Ok(vec![level.clone(), effect.clone()])
    );
    let mut saved: saved::Settings = canonical(&store.records[&record_key(4, 4)]);
    assert_eq!(
        saved.preferences[0].metadata.as_ref().unwrap().key,
        "backlight.level"
    );
    // A preference of an integration this firmware doesn't know is left out; an unknown key is
    // undecodable.
    let mut other = saved.preferences[0].clone();
    other.integration = 9;
    saved.preferences.push(other);
    store
        .records
        .insert(record_key(4, 4), saved.encode_to_vec());
    let mut preferences = Preferences {
        store: &mut store,
        device: 4,
    };
    assert_eq!(block_on(preferences.load_all()), Ok(vec![level, effect]));
    saved.preferences[0].metadata.as_mut().unwrap().key = "backlight.unknown".into();
    store
        .records
        .insert(record_key(4, 4), saved.encode_to_vec());
    assert_eq!(
        block_on(
            Preferences {
                store: &mut store,
                device: 4
            }
            .load_all()
        ),
        Err(storage::Error::Corrupt)
    );
}

#[test]
fn layouts_round_trip_with_report_maps_as_bytes() {
    let classic = Layout {
        maps: vec![ReportMap(vec![5, 1, 9, 6, 0xa1, 1, 0xc0])],
        reports: Vec::new(),
        hash: None,
    };
    let ble = Layout {
        maps: vec![ReportMap(vec![5, 1, 9, 6]), ReportMap(vec![6, 0, 0xff])],
        reports: vec![
            LayoutReport {
                service: 0,
                kind: ReportType::Input,
                id: 0,
                value: 0x10,
                properties: 0x12,
                cccd: 0x11,
            },
            LayoutReport {
                service: 1,
                kind: ReportType::Feature,
                id: 3,
                value: 0x20,
                properties: 0x0a,
                cccd: 0,
            },
        ],
        hash: Some(DatabaseHash([0x5a; 16])),
    };
    for layout in [classic, ble] {
        let bytes = layouts::encode(&layout).unwrap();
        assert_eq!(bytes.capacity(), bytes.len());
        let saved: saved::Layout = canonical(&bytes);
        assert_eq!(saved.maps[0], layout.maps[0].0);
        assert_eq!(layouts::decode(&bytes), Ok(layout));
    }
    // A hash of another length, or a report of an unknown type, is undecodable.
    let mut bad = saved::Layout {
        maps: vec![vec![1]],
        reports: Vec::new(),
        database_hash: vec![1; 15],
    };
    assert!(layouts::decode(&bad.encode_to_vec()).is_err());
    bad.database_hash.clear();
    bad.reports.push(saved::Report {
        r#type: 9,
        value_handle: 1,
        ..saved::Report::default()
    });
    assert!(layouts::decode(&bad.encode_to_vec()).is_err());
}

#[test]
fn profile_records_round_trip() {
    let mut store = support::Store::default();
    block_on(Identity::initialize(&mut store, [1; 6], || 3)).unwrap();
    let rules = profiles::Rules::new(vec![profiles::Rule {
        input: profiles::usage(profiles::KEYBOARD_PAGE, 0x39),
        effect: profiles::Effect::Remap(vec![profiles::Output {
            usage: profiles::usage(profiles::KEYBOARD_PAGE, 0xe0),
            collection: profiles::KEYBOARD,
        }]),
    }])
    .unwrap();
    let (id, meta) = block_on(profiles::create(&mut store, "Work", &rules)).unwrap();
    let saved: saved::Profile = canonical(&store.records[&record_key(profiles::METADATA, id)]);
    assert_eq!(saved.name, "Work");
    assert_eq!(saved.roles, [i32::from(saved::Role::Keyboard)]);
    let loaded = block_on(profiles::metadata(&mut store, id)).unwrap();
    assert_eq!((loaded.name, loaded.roles), (meta.name, meta.roles));
    let file: saved::Rules = canonical(&store.records[&record_key(profiles::RULES, id)]);
    assert_eq!(file.rules.len(), 1);
    assert_eq!(block_on(profiles::rules(&mut store, id)), Ok(rules));
    // A profile name outside the rules is undecodable.
    store.records.insert(
        record_key(profiles::METADATA, id),
        saved::Profile {
            name: String::new(),
            roles: Vec::new(),
        }
        .encode_to_vec(),
    );
    assert!(block_on(profiles::metadata(&mut store, id)).is_err());
    assert!(block_on(store.load_owned(record_key(profiles::RULES, id))).is_ok());
}
