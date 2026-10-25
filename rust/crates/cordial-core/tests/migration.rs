//! Storage inspection preserves the source bytes and never invokes a mutation API.
use cordial_core::{
    bonds::{self, Bond, Keys},
    devices::{Peer, Policy},
    identity::Identity,
    migration::{self, Dataset, Disposition, Entry, Inventory, Obsolete, RecordFormat, Root},
    model::identifiers::Transport,
    profiles,
    storage::{Error, FileEntry, FileType, RecordKey, RecordStore, record_key},
};
use cordial_protocol::storage as saved;
use embassy_futures::block_on;
use prost::{
    Message,
    encoding::{DecodeContext, decode_key, encode_key, skip_field},
};
use std::collections::BTreeMap;
#[allow(dead_code)]
mod support;

#[derive(Clone, Default)]
struct RawStore {
    files: BTreeMap<String, Vec<u8>>,
    writes: usize,
    fault: Option<String>,
    empty_directories: Vec<String>,
    read_limit: Option<usize>,
}
impl RecordStore for RawStore {
    async fn file_entry(&mut self, path: &str, index: usize) -> Result<Option<FileEntry>, Error> {
        let mut entries = BTreeMap::new();
        let prefix = if path == "/" {
            "/".into()
        } else {
            format!("{path}/")
        };
        for (name, bytes) in &self.files {
            if let Some(rest) = name.strip_prefix(&prefix) {
                let (name, kind, size) = match rest.split_once('/') {
                    Some((name, _)) => (name, FileType::Directory, 0),
                    None => (rest, FileType::File, bytes.len()),
                };
                entries.insert(
                    name.to_string(),
                    FileEntry {
                        name: name.into(),
                        kind,
                        size,
                    },
                );
            }
        }
        for directory in &self.empty_directories {
            if let Some(rest) = directory.strip_prefix(&prefix) {
                let name = rest.split('/').next().unwrap();
                entries.insert(
                    name.into(),
                    FileEntry {
                        name: name.into(),
                        kind: FileType::Directory,
                        size: 0,
                    },
                );
            }
        }
        Ok(entries.into_values().nth(index))
    }
    async fn file_read(
        &mut self,
        path: &str,
        offset: u32,
        bytes: &mut [u8],
    ) -> Result<usize, Error> {
        if self.fault.as_deref() == Some(path) {
            return Err(Error::Io);
        }
        let file = self.files.get(path).ok_or(Error::Missing)?;
        let tail = file.get(offset as usize..).ok_or(Error::Bounds)?;
        let n = bytes
            .len()
            .min(tail.len())
            .min(self.read_limit.unwrap_or(usize::MAX));
        bytes[..n].copy_from_slice(&tail[..n]);
        Ok(n)
    }
    async fn keys(&mut self) -> Result<Vec<RecordKey>, Error> {
        panic!("raw inventory must not use logical keys")
    }
    async fn available(&mut self) -> Result<usize, Error> {
        panic!("validation does not count free space")
    }
    async fn load(&mut self, _: RecordKey, _: &mut [u8]) -> Result<Option<usize>, Error> {
        panic!("raw inventory must not use logical reads")
    }
    async fn save(&mut self, _: RecordKey, _: &[u8]) -> Result<(), Error> {
        self.writes += 1;
        Err(Error::Unavailable)
    }
    async fn remove(&mut self, _: RecordKey) -> Result<(), Error> {
        self.writes += 1;
        Err(Error::Unavailable)
    }
}
fn fixture() -> RawStore {
    let mut roots = support::Store::default();
    block_on(Identity::initialize(&mut roots, [1; 6], || 3)).unwrap();
    let mut store = RawStore::default();
    for (key, bytes) in roots.records {
        store
            .files
            .insert(migration::record_path(key).unwrap(), bytes);
    }
    let peer = Peer {
        address: [2; 6],
        random: false,
        transport: Transport::Classic,
    };
    let mut policy = Policy::paired(1, peer, b"Keyboard");
    policy.profiles = vec![1];
    let bond = Bond {
        owner: 1,
        identity: peer,
        complete: true,
        keys: Keys::Classic {
            key: [7; 16],
            kind: 4,
        },
    };
    store.files.insert(
        "/devices/1/device.pb".into(),
        bonds::record(&policy, &bond).unwrap(),
    );
    store.files.insert(
        "/sequence.pb".into(),
        saved::Sequence {
            format: 2,
            next_device: 2,
            next_profile: 2,
        }
        .encode_to_vec(),
    );
    store.files.insert(
        "/adapter.pb".into(),
        saved::Adapter {
            format: 2,
            name: Some("Adapter".into()),
            transports: vec![saved::TransportPreference {
                transport: saved::Transport::Ble.into(),
                enabled: true,
            }],
            configuration_interfaces: vec![saved::InterfacePreference {
                interface: saved::ConfigurationInterface::Via.into(),
                enabled: true,
                profile: Some(1),
            }],
            ..Default::default()
        }
        .encode_to_vec(),
    );
    store.files.insert(
        "/devices/1/settings.pb".into(),
        saved::Settings {
            format: 2,
            preferences: vec![saved::Preference {
                integration: saved::Integration::Hidpp.into(),
                metadata: Some(saved::SettingMetadata {
                    key: "backlight.level".into(),
                    feature: 0x1982,
                    revision: 3,
                    scope: saved::SettingScope::Device.into(),
                    choices: vec![],
                    range: Some(saved::Range {
                        min: 0,
                        max: 7,
                        step: 1,
                    }),
                }),
                value: 5,
            }],
        }
        .encode_to_vec(),
    );
    store.files.insert(
        "/devices/1/layout.pb".into(),
        saved::Layout {
            format: 2,
            maps: vec![vec![1]],
            ..Default::default()
        }
        .encode_to_vec(),
    );
    store.files.insert(
        "/profiles/1/profile.pb".into(),
        saved::Profile {
            format: 2,
            name: "Work".into(),
            roles: vec![saved::Role::Keyboard.into()],
        }
        .encode_to_vec(),
    );
    store.files.insert(
        "/profiles/1/rules.pb".into(),
        saved::Rules {
            format: 2,
            rules: vec![saved::Rule {
                input: profiles::usage(7, 4),
                collection: profiles::KEYBOARD,
                effect: Some(saved::rule::Effect::Remap(saved::Remap {
                    outputs: vec![saved::Output {
                        usage: profiles::usage(7, 5),
                        collection: profiles::KEYBOARD,
                    }],
                })),
            }],
        }
        .encode_to_vec(),
    );
    store
}
fn legacy(mut bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let (tag, wire) = decode_key(&mut bytes).unwrap();
        let value = bytes;
        skip_field(wire, tag, &mut bytes, DecodeContext::default()).unwrap();
        if tag != 1 {
            encode_key(tag - 1, wire, &mut out);
            out.extend_from_slice(&value[..value.len() - bytes.len()]);
        }
    }
    out
}
fn validate(store: &mut RawStore) -> Result<migration::Summary, Error> {
    let before = store.files.clone();
    let result = block_on(migration::validate_dataset(&mut Inventory::new(store)));
    assert_eq!(store.files, before);
    assert_eq!(store.writes, 0);
    result
}
fn replace<M: Message>(store: &mut RawStore, path: &str, value: M) {
    store.files.insert(path.into(), value.encode_to_vec());
}
fn edit<M: Message + Default>(store: &mut RawStore, path: &str, f: impl FnOnce(&mut M)) {
    let mut message = M::decode(store.files[path].as_slice()).unwrap();
    f(&mut message);
    replace(store, path, message);
}

#[test]
fn root_classification_keeps_missing_empty_and_unsupported_distinct() {
    assert_eq!(migration::classify_root(None), Ok(Root::Missing));
    for bytes in [
        vec![],
        vec![8, 1, 8, 1],
        vec![8, 1, 16, 2],
        vec![8, 1, 0xff],
    ] {
        assert_eq!(migration::classify_root(Some(&bytes)), Err(Error::Corrupt));
    }
    for format in [1, 2, 3, 99] {
        let bytes = saved::Format {
            format,
            initialized: true,
        }
        .encode_to_vec();
        assert_eq!(
            migration::classify_root(Some(&bytes)),
            Ok(match format {
                1 => Root::Legacy { initialized: true },
                2 => Root::Current { initialized: true },
                _ => Root::Unsupported {
                    format,
                    initialized: true
                },
            })
        );
    }
}
#[test]
fn sequence_false_header_is_a_complete_legacy_record() {
    let key = record_key(7, 0);
    let bytes = [8, 2, 16, 1];
    assert_eq!(saved::extract_format(&bytes), Ok(2));
    assert_eq!(
        migration::validate_current_record(key, &bytes),
        Err(Error::Corrupt)
    );
    let normalized = migration::normalize_record(1, key, &bytes).unwrap();
    assert_eq!(normalized.source, RecordFormat::Legacy);
    assert_eq!(normalized.bytes, [8, 2, 16, 2, 24, 1]);
    assert_eq!(
        migration::normalize_record(1, key, &normalized.bytes)
            .unwrap()
            .source,
        RecordFormat::Current
    );
}
#[test]
fn every_file_kind_converts_without_reserializing_nested_fields() {
    let store = fixture();
    for (path, bytes) in &store.files {
        if path == "/format.pb" {
            continue;
        }
        let source = legacy(bytes);
        let out =
            migration::normalize_path(1, path, &source).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        assert_eq!(out.source, RecordFormat::Legacy, "{path}");
        assert_eq!(&out.bytes, bytes, "{path}");
        assert_eq!(
            migration::normalize_path(1, path, bytes).unwrap().source,
            RecordFormat::Current
        );
    }
    let mut policy = saved::Device::decode(store.files["/devices/1/device.pb"].as_slice())
        .unwrap()
        .policy
        .unwrap()
        .encode_to_vec();
    policy.extend_from_slice(&[0xa0, 0x06, 0x81, 0]);
    let bond = saved::Device::decode(store.files["/devices/1/device.pb"].as_slice())
        .unwrap()
        .bond
        .unwrap();
    let mut source = Vec::new();
    prost::encoding::bytes::encode(1, &policy, &mut source);
    prost::encoding::message::encode(2, &bond, &mut source);
    let out = migration::normalize_path(1, "/devices/1/device.pb", &source).unwrap();
    let mut expected = vec![8, 2];
    prost::encoding::bytes::encode(2, &policy, &mut expected);
    prost::encoding::message::encode(3, &bond, &mut expected);
    assert_eq!(out.bytes, expected);
    // Unknown v2 outer fields are retained; v1 outer extensions cannot be interpreted safely.
    let mut current = store.files["/sequence.pb"].clone();
    current.extend_from_slice(&[0xa0, 0x06, 0x81, 0]);
    assert_eq!(
        migration::normalize_path(1, "/sequence.pb", &current)
            .unwrap()
            .bytes,
        current
    );
    let mut old = legacy(&store.files["/sequence.pb"]);
    old.extend_from_slice(&[0xa0, 0x06, 1]);
    assert_eq!(
        migration::normalize_path(1, "/sequence.pb", &old),
        Err(Error::Corrupt)
    );
}
#[test]
fn all_partial_replacement_states_validate_before_root_commit() {
    let base = fixture();
    for (path, bytes) in &base.files {
        if path != "/format.pb" {
            migration::normalize_path(2, path, bytes).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        }
    }
    let paths: Vec<_> = base
        .files
        .keys()
        .filter(|p| p.as_str() != "/format.pb")
        .cloned()
        .collect();
    assert_eq!(paths.len(), 8);
    for mask in 0u32..(1 << paths.len()) {
        let mut store = base.clone();
        replace(
            &mut store,
            "/format.pb",
            saved::Format {
                format: 1,
                initialized: true,
            },
        );
        for (index, path) in paths.iter().enumerate() {
            if mask & (1 << index) == 0 {
                store.files.insert(path.clone(), legacy(&base.files[path]));
            }
        }
        let summary = validate(&mut store).unwrap();
        assert_eq!(summary.records, 8);
        assert_eq!(summary.current_records, mask.count_ones() as usize);
        assert_eq!(summary.legacy_records, 8 - summary.current_records);
    }
    assert_eq!(validate(&mut base.clone()).unwrap().current_records, 8);
    let mut premature = base.clone();
    premature
        .files
        .insert("/sequence.pb".into(), legacy(&base.files["/sequence.pb"]));
    assert_eq!(validate(&mut premature), Err(Error::Corrupt));
}
#[test]
fn malformed_active_records_stop_without_writes() {
    let base = fixture();
    for path in base.files.keys().filter(|p| !p.ends_with("/layout.pb")) {
        for bytes in [
            vec![0xff],
            vec![8, 2, 18, 4, 1],
            vec![8, 2, 0],
            vec![8, 2, 8, 2],
        ] {
            let mut store = base.clone();
            store.files.insert(path.clone(), bytes);
            assert_eq!(validate(&mut store), Err(Error::Corrupt), "{path}");
        }
    }
}
#[test]
fn duplicate_outer_and_nested_fields_and_oneofs_are_rejected() {
    let base = fixture();
    for (path, bytes) in &base.files {
        if path == "/format.pb" {
            continue;
        }
        let mut twice = bytes.clone();
        twice.extend_from_slice(bytes);
        assert_eq!(
            migration::normalize_path(1, path, &twice),
            Err(Error::Corrupt)
        );
    }
    let mut bad = base.clone();
    let mut device = saved::Device::decode(bad.files["/devices/1/device.pb"].as_slice()).unwrap();
    let mut policy = device.policy.take().unwrap().encode_to_vec();
    policy.extend_from_slice(&[8, 1]);
    let mut out = vec![8, 2];
    prost::encoding::bytes::encode(2, &policy, &mut out);
    prost::encoding::message::encode(3, &device.bond.unwrap(), &mut out);
    bad.files.insert("/devices/1/device.pb".into(), out);
    assert_eq!(validate(&mut bad), Err(Error::Corrupt));
    let mut rule = saved::Rule {
        input: profiles::usage(7, 4),
        collection: profiles::KEYBOARD,
        effect: Some(saved::rule::Effect::Remap(saved::Remap::default())),
    }
    .encode_to_vec();
    prost::encoding::message::encode(
        4,
        &saved::Scale {
            numerator: 1,
            denominator: 1,
        },
        &mut rule,
    );
    let mut rules = vec![8, 2];
    prost::encoding::bytes::encode(2, &rule, &mut rules);
    assert_eq!(
        migration::validate_current_record(record_key(9, 1), &rules),
        Err(Error::Corrupt)
    );
}

#[test]
fn semantic_errors_in_committed_records_stop_without_writes() {
    let mut cases = Vec::new();
    let mut bad = fixture();
    edit::<saved::Identity>(&mut bad, "/identity.pb", |v| v.irk[0] ^= 1);
    cases.push(bad);
    for field in [0, 1, 2] {
        let mut bad = fixture();
        edit::<saved::Device>(&mut bad, "/devices/1/device.pb", |v| match field {
            0 => v.bond.as_mut().unwrap().owner = 2,
            1 => v.bond.as_mut().unwrap().identity.as_mut().unwrap().address[0] = 3,
            _ => {
                v.bond.as_mut().unwrap().keys =
                    Some(saved::bond::Keys::Classic(saved::ClassicKeys {
                        link_key: vec![1; 15],
                        key_type: 4,
                    }))
            }
        });
        cases.push(bad);
    }
    for counter in [0, 1, u64::from(u32::MAX) + 2] {
        for profile in [false, true] {
            let mut bad = fixture();
            edit::<saved::Sequence>(&mut bad, "/sequence.pb", |v| {
                if profile {
                    v.next_profile = counter;
                } else {
                    v.next_device = counter;
                }
            });
            cases.push(bad);
        }
    }
    let mut bad = fixture();
    edit::<saved::Adapter>(&mut bad, "/adapter.pb", |v| {
        v.transports.push(v.transports[0])
    });
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Adapter>(&mut bad, "/adapter.pb", |v| {
        v.name = Some("  Untrimmed".into())
    });
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Adapter>(&mut bad, "/adapter.pb", |v| {
        v.configuration_interfaces[0].profile = None
    });
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Device>(&mut bad, "/devices/1/device.pb", |v| {
        v.policy.as_mut().unwrap().name = "Invalid\nName".into()
    });
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Settings>(&mut bad, "/devices/1/settings.pb", |v| {
        v.preferences[0].value = 101
    });
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Settings>(&mut bad, "/devices/1/settings.pb", |v| {
        v.preferences.push(v.preferences[0].clone())
    });
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Profile>(&mut bad, "/profiles/1/profile.pb", |v| v.name.clear());
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Rules>(&mut bad, "/profiles/1/rules.pb", |v| {
        v.rules.push(v.rules[0].clone())
    });
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Rules>(&mut bad, "/profiles/1/rules.pb", |v| {
        v.rules[0].effect = Some(saved::rule::Effect::Scale(saved::Scale {
            numerator: 1,
            denominator: 0,
        }))
    });
    cases.push(bad);
    let mut bad = fixture();
    edit::<saved::Rules>(&mut bad, "/profiles/1/rules.pb", |v| {
        v.rules[0].effect = Some(saved::rule::Effect::Remap(saved::Remap {
            outputs: vec![
                saved::Output {
                    usage: profiles::usage(7, 5),
                    collection: profiles::KEYBOARD
                };
                2
            ],
        }))
    });
    cases.push(bad);
    for path in ["/identity.pb", "/sequence.pb"] {
        let mut bad = fixture();
        bad.files.remove(path);
        cases.push(bad);
    }
    for (index, mut bad) in cases.into_iter().enumerate() {
        assert_eq!(validate(&mut bad), Err(Error::Corrupt), "case {index}");
    }
}
#[test]
fn initialization_prefixes_optional_absence_and_unknown_files_are_deliberate() {
    let base = fixture();
    let mut blank = RawStore::default();
    assert_eq!(validate(&mut blank), Ok(Default::default()));
    for identity in [false, true] {
        for sequence in [false, true] {
            let mut store = RawStore::default();
            replace(
                &mut store,
                "/format.pb",
                saved::Format {
                    format: 2,
                    initialized: false,
                },
            );
            if identity {
                store
                    .files
                    .insert("/identity.pb".into(), base.files["/identity.pb"].clone());
            }
            if sequence {
                store
                    .files
                    .insert("/sequence.pb".into(), base.files["/sequence.pb"].clone());
            }
            assert_eq!(validate(&mut store).is_ok(), identity || !sequence);
        }
    }
    let mut incomplete = base.clone();
    replace(
        &mut incomplete,
        "/format.pb",
        saved::Format {
            format: 2,
            initialized: false,
        },
    );
    assert_eq!(validate(&mut incomplete), Err(Error::Corrupt));
    let mut optional = base.clone();
    for path in [
        "/adapter.pb",
        "/devices/1/settings.pb",
        "/devices/1/layout.pb",
        "/profiles/1/rules.pb",
    ] {
        optional.files.remove(path);
    }
    edit::<saved::Profile>(&mut optional, "/profiles/1/profile.pb", |v| v.roles.clear());
    optional.files.insert("/unrecognized.pb".into(), vec![]);
    optional.empty_directories.push("/extension".into());
    let result = validate(&mut optional).unwrap();
    assert_eq!(result.unknown_files, 1);
    assert_eq!(result.unknown_directories, 1);
    optional.files.remove("/format.pb");
    assert_eq!(validate(&mut optional), Err(Error::Layout));
    for format in [1, 3, 99] {
        let mut store = base.clone();
        replace(
            &mut store,
            "/format.pb",
            saved::Format {
                format,
                initialized: true,
            },
        );
        if format != 1 {
            assert_eq!(validate(&mut store), Err(Error::Layout));
        }
    }
}
#[test]
fn malformed_paths_file_types_and_inventory_read_failures_stop_without_writes() {
    for path in [
        "/devices/01/device.pb",
        "/devices/0/device.pb",
        "/devices/4294967296/device.pb",
        "/profiles/no/profile.pb",
        "/devices/1/device.pb/extra",
        "/devices/1/../device.pb",
    ] {
        assert_eq!(
            migration::classify_path(path, FileType::File),
            Err(Error::Corrupt)
        );
    }
    for path in [
        "/format.pb",
        "/identity.pb",
        "/devices/1/device.pb",
        "/profiles/1/rules.pb",
    ] {
        assert_eq!(
            migration::classify_path(path, FileType::Directory),
            Err(Error::Corrupt)
        );
    }
    for path in ["/devices", "/profiles/1"] {
        assert_eq!(
            migration::classify_path(path, FileType::File),
            Err(Error::Corrupt)
        );
    }
    for path in fixture().files.keys() {
        let mut store = fixture();
        store.fault = Some(path.clone());
        assert_eq!(validate(&mut store), Err(Error::Io));
    }
    let mut chunked = fixture();
    chunked.read_limit = Some(3);
    assert!(validate(&mut chunked).is_ok());
    chunked.read_limit = Some(0);
    assert_eq!(validate(&mut chunked), Err(Error::Corrupt));
    let mut empty = RawStore::default();
    empty.empty_directories.push("/extension".into());
    assert_eq!(validate(&mut empty), Err(Error::Layout));
}
#[test]
fn forward_compatible_enums_are_ignored_only_at_existing_compatibility_boundaries() {
    let mut store = fixture();
    edit::<saved::Adapter>(&mut store, "/adapter.pb", |v| {
        v.host_platform = 99;
        v.transports.extend(
            [saved::TransportPreference {
                transport: 99,
                enabled: true,
            }; 2],
        );
        v.configuration_interfaces.push(saved::InterfacePreference {
            interface: 99,
            enabled: true,
            profile: Some(99),
        });
    });
    edit::<saved::Settings>(&mut store, "/devices/1/settings.pb", |v| {
        v.preferences.push(saved::Preference {
            integration: 99,
            metadata: None,
            value: 0,
        })
    });
    edit::<saved::Device>(&mut store, "/devices/1/device.pb", |v| {
        let p = v.policy.as_mut().unwrap();
        p.roles.push(99);
        p.integrations.push(saved::IntegrationPreference {
            integration: 99,
            enabled: true,
        });
        p.output_preferences
            .push(saved::OutputPreference { role: 99, mode: 99 });
    });
    edit::<saved::Profile>(&mut store, "/profiles/1/profile.pb", |v| v.roles.push(99));
    assert!(validate(&mut store).is_ok());
    edit::<saved::Device>(&mut store, "/devices/1/device.pb", |v| {
        v.policy.as_mut().unwrap().peer.as_mut().unwrap().transport = 99
    });
    assert_eq!(validate(&mut store), Err(Error::Corrupt));
}
#[test]
fn inventory_includes_empty_files_and_rewinds_without_logical_keys() {
    let mut store = fixture();
    store.files.insert("/scratch".into(), vec![]);
    let mut inventory = Inventory::new(&mut store);
    for _ in 0..2 {
        inventory.rewind();
        let mut entries = Vec::new();
        while let Some(entry) = block_on(inventory.next_entry()).unwrap() {
            entries.push(entry);
        }
        assert_eq!(
            entries.iter().filter(|e| e.kind == FileType::File).count(),
            10
        );
        assert!(entries.iter().any(|e| e.path == "/scratch" && e.size == 0));
        assert_eq!(block_on(inventory.read("/scratch")), Ok(Some(vec![])));
        assert_eq!(block_on(inventory.read("/absent")), Ok(None));
    }
}

#[test]
fn final_target_validation_requires_all_replacements_before_the_root() {
    let mut store = fixture();
    replace(
        &mut store,
        "/format.pb",
        saved::Format {
            format: 1,
            initialized: true,
        },
    );
    assert!(
        block_on(migration::validate_target_dataset(&mut Inventory::new(
            &mut store
        )))
        .is_ok()
    );
    let current = store.files["/sequence.pb"].clone();
    store.files.insert("/sequence.pb".into(), legacy(&current));
    assert!(validate(&mut store).is_ok());
    assert_eq!(
        block_on(migration::validate_target_dataset(&mut Inventory::new(
            &mut store
        ))),
        Err(Error::Corrupt)
    );
    assert_eq!(store.writes, 0);
    let mut blank = RawStore::default();
    assert_eq!(
        block_on(migration::validate_target_dataset(&mut Inventory::new(
            &mut blank
        ))),
        Err(Error::Layout)
    );
}

#[test]
fn malformed_wire_and_missing_required_payloads_never_convert() {
    for bytes in [
        vec![8],
        vec![8, 0x80],
        vec![8, 1, 16],
        vec![8, 1, 16, 0x80],
        vec![8, 1, 16, 1, 0],
        vec![8, 1, 16, 1, 22],
        vec![8, 1, 16, 1, 0x19, 1],
        vec![8, 1, 16, 1, 26, 3, 0],
        vec![
            8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2, 16, 1,
        ],
        vec![8, 1, 8, 1, 16, 1],
        vec![8, 1, 16, 1, 16, 1],
        vec![8, 1],
        vec![16, 1],
        vec![8, 0, 16, 1],
    ] {
        assert_eq!(
            migration::normalize_record(1, record_key(7, 0), &bytes),
            Err(Error::Corrupt),
            "{bytes:?}"
        );
    }
    for kind in [2, 3, 4, 5, 7, 8, 9] {
        let key = record_key(kind, if matches!(kind, 3 | 7) { 0 } else { 1 });
        assert_eq!(
            migration::normalize_record(1, key, &[8, 2]),
            Err(Error::Corrupt),
            "kind {kind}"
        );
    }
    assert_eq!(
        migration::normalize_path(1, "/format.pb", &[8, 1]),
        Err(Error::Bounds)
    );
    assert_eq!(
        migration::normalize_path(1, "/unknown.pb", &[8, 1]),
        Err(Error::Bounds)
    );
    assert_eq!(
        migration::normalize_record(99, record_key(7, 0), &[8, 2, 16, 1, 24, 1]),
        Err(Error::Layout)
    );
}

#[test]
fn ble_bonds_require_complete_security_matching_the_policy() {
    let mut store = fixture();
    edit::<saved::Device>(&mut store, "/devices/1/device.pb", |device| {
        device
            .policy
            .as_mut()
            .unwrap()
            .peer
            .as_mut()
            .unwrap()
            .transport = saved::Transport::Ble.into();
        let bond = device.bond.as_mut().unwrap();
        bond.identity.as_mut().unwrap().transport = saved::Transport::Ble.into();
        let security = saved::Security {
            flags: 1,
            key_size: 16,
            rand: vec![0; 8],
            ltk: vec![1; 16],
            irk: vec![0; 16],
            csrk: vec![0; 16],
            ..Default::default()
        };
        bond.keys = Some(saved::bond::Keys::Ble(saved::BleKeys {
            local: Some(security.clone()),
            peer: Some(security),
        }));
    });
    store.files.remove("/devices/1/layout.pb");
    assert!(validate(&mut store).is_ok());
    let base = store.clone();
    for case in 0..5 {
        let mut bad = base.clone();
        edit::<saved::Device>(&mut bad, "/devices/1/device.pb", |device| {
            let saved::bond::Keys::Ble(keys) = device.bond.as_mut().unwrap().keys.as_mut().unwrap()
            else {
                panic!()
            };
            match case {
                0 => keys.local = None,
                1 => keys.local.as_mut().unwrap().key_size = 6,
                2 => keys.local.as_mut().unwrap().flags |= 64,
                3 => keys.peer.as_mut().unwrap().ediv = 65536,
                _ => keys.peer.as_mut().unwrap().rand.clear(),
            }
        });
        assert_eq!(validate(&mut bad), Err(Error::Corrupt));
    }
}

#[test]
fn root_conversion_preserves_initialization_and_exact_unknown_fields() {
    for initialized in [false, true] {
        let mut source = vec![0xa0, 0x06, 0x81, 0];
        source.extend_from_slice(
            &saved::Format {
                format: 1,
                initialized,
            }
            .encode_to_vec(),
        );
        source.extend_from_slice(&[0xaa, 0x06, 2, 0xff, 0]);
        let result = migration::convert_root(&source).unwrap();
        assert_eq!(
            migration::classify_root(Some(&result)),
            Ok(Root::Current { initialized })
        );
        let mut expected = vec![0xa0, 0x06, 0x81, 0];
        expected.extend_from_slice(
            &saved::Format {
                format: 2,
                initialized,
            }
            .encode_to_vec(),
        );
        expected.extend_from_slice(&[0xaa, 0x06, 2, 0xff, 0]);
        assert_eq!(result, expected);
        assert_eq!(migration::convert_root(&result), Ok(result));
    }
    assert_eq!(migration::convert_root(&[8, 99]), Err(Error::Layout));
    assert_eq!(migration::convert_root(&[]), Err(Error::Corrupt));
}

#[test]
fn references_outside_the_device_id_domain_are_rejected_locally() {
    let mut store = fixture();
    edit::<saved::Device>(&mut store, "/devices/1/device.pb", |v| {
        v.policy
            .as_mut()
            .unwrap()
            .profiles
            .push(u64::from(u32::MAX) + 1)
    });
    assert_eq!(
        migration::normalize_path(
            2,
            "/devices/1/device.pb",
            &store.files["/devices/1/device.pb"]
        ),
        Err(Error::Corrupt)
    );
    assert_eq!(validate(&mut store), Err(Error::Corrupt));
    let mut store = fixture();
    edit::<saved::Adapter>(&mut store, "/adapter.pb", |v| {
        v.configuration_interfaces[0].profile = Some(u64::from(u32::MAX) + 1)
    });
    assert_eq!(
        migration::normalize_path(2, "/adapter.pb", &store.files["/adapter.pb"]),
        Err(Error::Corrupt)
    );
    assert_eq!(validate(&mut store), Err(Error::Corrupt));
}

fn inspect(store: &mut RawStore, root: u32, path: &str) -> Disposition {
    let entry = Entry {
        path: path.into(),
        kind: FileType::File,
        size: store.files[path].len(),
    };
    block_on(migration::inspect_entry(
        &mut Inventory::new(store),
        root,
        &entry,
    ))
    .unwrap()
}

fn historical(store: &mut RawStore) {
    for (path, bytes) in &mut store.files {
        if path == "/format.pb" {
            *bytes = saved::Format {
                format: 1,
                initialized: true,
            }
            .encode_to_vec();
        } else if !bytes.is_empty() {
            *bytes = legacy(bytes);
        }
    }
}

#[test]
fn empty_records_are_absent_and_initialized_essentials_still_required() {
    for root in [1, 2] {
        for path in fixture().files.keys() {
            let mut store = fixture();
            if root == 1 {
                historical(&mut store);
            }
            store.files.insert(path.clone(), vec![]);
            if matches!(
                path.as_str(),
                "/format.pb" | "/identity.pb" | "/sequence.pb"
            ) {
                assert_eq!(validate(&mut store), Err(Error::Corrupt), "{path}");
            } else {
                let summary = validate(&mut store).unwrap();
                assert!(summary.obsolete_files >= 1);
                assert_eq!(
                    inspect(&mut store, root, path),
                    Disposition::Obsolete(Obsolete::Empty)
                );
                assert_eq!(
                    block_on(migration::validate_target_dataset(&mut Inventory::new(
                        &mut store
                    ))),
                    Err(Error::Corrupt)
                );
            }
        }
    }
}

#[test]
fn inactive_children_are_classified_before_reading_their_payload() {
    for root in [1, 2] {
        for (parent, children) in [
            (
                "/devices/1/device.pb",
                &["/devices/1/settings.pb", "/devices/1/layout.pb"][..],
            ),
            ("/profiles/1/profile.pb", &["/profiles/1/rules.pb"][..]),
        ] {
            for empty_parent in [false, true] {
                let mut store = fixture();
                if root == 1 {
                    historical(&mut store);
                }
                if empty_parent {
                    store.files.insert(parent.into(), vec![]);
                } else {
                    store.files.remove(parent);
                }
                for child in children {
                    store.files.insert((*child).into(), vec![0xff]);
                    // A read fault proves the inactive payload is never opened or decoded.
                    store.fault = Some((*child).into());
                    assert_eq!(
                        inspect(&mut store, root, child),
                        Disposition::Obsolete(Obsolete::Inactive)
                    );
                    assert!(validate(&mut store).is_ok());
                }
                assert_eq!(store.writes, 0);
            }
        }
    }
}

#[test]
fn interrupted_relationship_updates_preserve_committed_bytes_and_list_cache_cleanup() {
    let mut expected = fixture();
    edit::<saved::Device>(&mut expected, "/devices/1/device.pb", |v| {
        v.policy.as_mut().unwrap().profiles.push(99);
    });
    edit::<saved::Adapter>(&mut expected, "/adapter.pb", |v| {
        v.configuration_interfaces[0].profile = Some(99);
    });
    edit::<saved::Profile>(&mut expected, "/profiles/1/profile.pb", |v| v.roles.clear());
    for invalid in [
        vec![0xff],
        vec![8, 2],
        saved::Layout {
            format: 2,
            maps: vec![vec![1]],
            reports: vec![saved::Report {
                r#type: saved::ReportType::Input.into(),
                value_handle: 1,
                ..Default::default()
            }],
            ..Default::default()
        }
        .encode_to_vec(),
    ] {
        for root in [1, 2] {
            let mut store = expected.clone();
            if root == 1 {
                historical(&mut store);
            }
            // Cache damage is disposable even when its schema cannot be identified.
            store
                .files
                .insert("/devices/1/layout.pb".into(), invalid.clone());
            assert_eq!(validate(&mut store).unwrap().obsolete_files, 1);
            assert_eq!(
                inspect(&mut store, root, "/devices/1/layout.pb"),
                Disposition::Obsolete(Obsolete::LayoutCache)
            );
            let paths: Vec<_> = store
                .files
                .keys()
                .filter(|p| p.as_str() != "/format.pb")
                .cloned()
                .collect();
            for path in paths {
                match inspect(&mut store, root, &path) {
                    Disposition::Record(record) => {
                        assert_eq!(record.bytes, expected.files[&path], "{path}");
                        store.files.insert(path, record.bytes);
                    }
                    Disposition::Obsolete(_) => {
                        store.files.remove(&path);
                    }
                    other => panic!("unexpected disposition: {other:?}"),
                }
            }
            assert!(
                block_on(migration::validate_target_dataset(&mut Inventory::new(
                    &mut store
                )))
                .is_ok()
            );
            assert_eq!(store.writes, 0);
        }
    }
    expected.files.remove("/profiles/1/rules.pb");
    edit::<saved::Profile>(&mut expected, "/profiles/1/profile.pb", |v| {
        v.roles.push(saved::Role::Gamepad.into())
    });
    historical(&mut expected);
    assert!(validate(&mut expected).is_ok());
}

#[test]
fn scratch_and_unknown_entries_have_explicit_dispositions() {
    let mut store = RawStore::default();
    store.files.insert("/.file-upload".into(), vec![0xff]);
    store.fault = Some("/.file-upload".into());
    assert_eq!(validate(&mut store), Ok(Default::default()));
    assert_eq!(
        inspect(&mut store, 1, "/.file-upload"),
        Disposition::Scratch
    );
    store.files.insert("/extension.pb".into(), vec![0xff]);
    assert_eq!(
        inspect(&mut store, 1, "/extension.pb"),
        Disposition::PreserveUnknown
    );
    assert_eq!(validate(&mut store), Err(Error::Layout));
    let entry = Entry {
        path: "/extension".into(),
        kind: FileType::Directory,
        size: 0,
    };
    assert_eq!(
        block_on(migration::inspect_entry(
            &mut Inventory::new(&mut store),
            1,
            &entry
        )),
        Ok(Disposition::PreserveUnknown)
    );
}

#[derive(Clone, Copy, Debug, Default)]
struct Allocations {
    calls: usize,
    bytes: usize,
    live: isize,
    peak: usize,
}
thread_local! {
    static ALLOCATIONS: std::cell::Cell<Option<Allocations>> = const { std::cell::Cell::new(None) };
}
struct MeasuredAllocator;
#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;
fn allocated(size: usize) {
    let _ = ALLOCATIONS.try_with(|cell| {
        if let Some(mut stats) = cell.get() {
            stats.calls += 1;
            stats.bytes += size;
            stats.live += size as isize;
            stats.peak = stats.peak.max(stats.live.max(0) as usize);
            cell.set(Some(stats));
        }
    });
}
fn freed(size: usize) {
    let _ = ALLOCATIONS.try_with(|cell| {
        if let Some(mut stats) = cell.get() {
            stats.live -= size as isize;
            cell.set(Some(stats));
        }
    });
}
// Every pointer and layout is forwarded unchanged to the system allocator.
unsafe impl std::alloc::GlobalAlloc for MeasuredAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let pointer = unsafe { std::alloc::System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        freed(layout.size());
        unsafe {
            std::alloc::System.dealloc(pointer, layout);
        }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        let result = unsafe { std::alloc::System.realloc(pointer, layout, size) };
        if !result.is_null() {
            freed(layout.size());
            allocated(size);
        }
        result
    }
}
fn measured<T>(f: impl FnOnce() -> T) -> (T, Allocations) {
    ALLOCATIONS.set(Some(Allocations::default()));
    let result = f();
    let stats = ALLOCATIONS.replace(None).unwrap();
    (result, stats)
}
fn allocation_free(path: &str, bytes: &[u8], expected: Result<(), Error>) {
    let migration::Path::Record(key) = migration::classify_path(path, FileType::File).unwrap()
    else {
        panic!()
    };
    let (result, stats) = measured(|| migration::validate_current_record(key, bytes));
    assert_eq!(result, expected, "{path}");
    assert_eq!(stats.calls, 0, "{path}: {stats:?}");
}

#[test]
fn repeated_compatibility_fields_never_allocate_during_semantic_validation() {
    let mut store = fixture();
    edit::<saved::Settings>(&mut store, "/devices/1/settings.pb", |v| {
        v.preferences.extend((0..10_000).map(|i| saved::Preference {
            integration: 99 + i,
            metadata: None,
            value: 0,
        }));
    });
    edit::<saved::Device>(&mut store, "/devices/1/device.pb", |v| {
        let p = v.policy.as_mut().unwrap();
        p.roles.extend([99; 10_000]);
        p.integrations.extend(
            [saved::IntegrationPreference {
                integration: 99,
                enabled: true,
            }; 10_000],
        );
        p.output_preferences
            .extend([saved::OutputPreference { role: 99, mode: 99 }; 10_000]);
    });
    edit::<saved::Adapter>(&mut store, "/adapter.pb", |v| {
        v.transports.extend(
            [saved::TransportPreference {
                transport: 99,
                enabled: true,
            }; 10_000],
        );
        v.configuration_interfaces.extend(
            [saved::InterfacePreference {
                interface: 99,
                enabled: true,
                profile: Some(99),
            }; 10_000],
        );
    });
    edit::<saved::Profile>(&mut store, "/profiles/1/profile.pb", |v| {
        v.roles.extend([99; 10_000])
    });
    for (path, bytes) in &store.files {
        if path == "/format.pb" {
            continue;
        }
        allocation_free(path, bytes, Ok(()));
        let source = legacy(bytes);
        let (normalized, stats) = measured(|| migration::normalize_path(1, path, &source));
        assert_eq!(normalized.unwrap().bytes, *bytes);
        assert_eq!(stats.calls, 1, "{path}: {stats:?}");
        assert_eq!(stats.bytes, source.len() + 2);
    }
    assert!(validate(&mut store).is_ok());
}

#[test]
fn amplified_invalid_fields_are_rejected_without_allocations() {
    let mut cases = Vec::new();
    let mut store = fixture();
    edit::<saved::Device>(&mut store, "/devices/1/device.pb", |v| {
        v.policy.as_mut().unwrap().profiles = vec![1; 10_000]
    });
    cases.push((
        "/devices/1/device.pb",
        store.files["/devices/1/device.pb"].clone(),
    ));
    edit::<saved::Device>(&mut store, "/devices/1/device.pb", |v| {
        let p = v.policy.as_mut().unwrap();
        p.profiles.clear();
        p.name = "a".repeat(60_000);
    });
    cases.push((
        "/devices/1/device.pb",
        store.files["/devices/1/device.pb"].clone(),
    ));
    edit::<saved::Settings>(&mut store, "/devices/1/settings.pb", |v| {
        v.preferences = vec![v.preferences[0].clone(); 10_000]
    });
    cases.push((
        "/devices/1/settings.pb",
        store.files["/devices/1/settings.pb"].clone(),
    ));
    edit::<saved::Rules>(&mut store, "/profiles/1/rules.pb", |v| {
        let saved::rule::Effect::Remap(remap) = v.rules[0].effect.as_mut().unwrap() else {
            panic!()
        };
        remap.outputs = vec![remap.outputs[0]; 10_000];
    });
    cases.push((
        "/profiles/1/rules.pb",
        store.files["/profiles/1/rules.pb"].clone(),
    ));
    edit::<saved::Layout>(&mut store, "/devices/1/layout.pb", |v| {
        v.maps = vec![vec![1]; 10_000]
    });
    cases.push((
        "/devices/1/layout.pb",
        store.files["/devices/1/layout.pb"].clone(),
    ));
    for (path, bytes) in cases {
        allocation_free(path, &bytes, Err(Error::Corrupt));
    }
}

#[test]
fn large_valid_choice_lists_use_bounded_state_and_detect_distant_duplicates() {
    let mut store = fixture();
    edit::<saved::Settings>(&mut store, "/devices/1/settings.pb", |v| {
        let m = v.preferences[0].metadata.as_mut().unwrap();
        m.range = None;
        m.choices = (1..=50_000).collect();
    });
    let path = "/devices/1/settings.pb";
    allocation_free(path, &store.files[path], Ok(()));
    edit::<saved::Settings>(&mut store, path, |v| {
        v.preferences[0].metadata.as_mut().unwrap().choices.push(1)
    });
    allocation_free(path, &store.files[path], Err(Error::Corrupt));
}

// The host retains its source dataset outside the measured validator working set.
struct Snapshot<'a> {
    files: &'a BTreeMap<String, Vec<u8>>,
    entries: Vec<Entry>,
    at: usize,
}
impl Dataset for Snapshot<'_> {
    fn rewind(&mut self) {
        self.at = 0;
    }
    async fn next_entry(&mut self) -> Result<Option<Entry>, Error> {
        let entry = self.entries.get(self.at).cloned();
        self.at += usize::from(entry.is_some());
        Ok(entry)
    }
    async fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        self.files
            .get(path)
            .map(|bytes| {
                let mut result = Vec::new();
                result
                    .try_reserve_exact(bytes.len())
                    .map_err(|_| Error::Unavailable)?;
                result.extend_from_slice(bytes);
                Ok(result)
            })
            .transpose()
    }
}

#[test]
fn final_validation_holds_one_large_file_and_conversion_holds_at_most_two() {
    let mut store = fixture();
    edit::<saved::Settings>(&mut store, "/devices/1/settings.pb", |v| {
        v.preferences.extend((0..10_000).map(|i| saved::Preference {
            integration: i + 99,
            metadata: None,
            value: 0,
        }));
    });
    for root in [2, 1] {
        if root == 1 {
            historical(&mut store);
        }
        let largest = store.files.values().map(Vec::len).max().unwrap();
        let mut inventory = Inventory::new(&mut store);
        let mut entries = Vec::new();
        while let Some(entry) = block_on(inventory.next_entry()).unwrap() {
            entries.push(entry);
        }
        let mut snapshot = Snapshot {
            files: &store.files,
            entries,
            at: 0,
        };
        let (result, stats) = measured(|| block_on(migration::validate_dataset(&mut snapshot)));
        assert!(result.is_ok());
        let buffers = if root == 2 { 1 } else { 2 };
        assert!(stats.peak <= buffers * largest + 128, "{stats:?}");
        println!(
            "root {root}: largest file {largest} bytes, peak validator heap {} bytes",
            stats.peak
        );
        if root == 2 {
            let (result, final_stats) =
                measured(|| block_on(migration::validate_target_dataset(&mut snapshot)));
            assert!(result.is_ok());
            assert!(final_stats.peak <= largest + 128, "{final_stats:?}");
        }
    }
}

#[test]
fn borrowed_settings_checks_match_runtime_validation() {
    use cordial_core::model::{
        hidpp::{FeatureId, FeatureRevision},
        settings::{SettingKey, SettingType},
    };
    let features = [
        FeatureId::FN_INVERSION_MULTI_HOST,
        FeatureId::MULTI_PLATFORM,
        FeatureId::ADC_MEASUREMENT,
        FeatureId::BACKLIGHT,
        FeatureId::ADJUSTABLE_DPI,
        FeatureId::SMART_SHIFT,
        FeatureId::HIRES_WHEEL,
        FeatureId::THUMBWHEEL,
    ];
    for key in SettingKey::ALL {
        let Some(feature) = features
            .into_iter()
            .find(|f| key.writable_feature(*f, FeatureRevision(3)))
        else {
            continue;
        };
        let choices: Vec<u32> = match key.kind() {
            SettingType::Integer => vec![5, 10],
            SettingType::Enum if key == SettingKey::BacklightMode => vec![1, 3],
            SettingType::Enum => (0..key.enum_values().len() as u32)
                .map(|v| v + u32::from(key == SettingKey::WheelMode))
                .collect(),
            _ => vec![],
        };
        let preference = saved::Preference {
            integration: saved::Integration::Hidpp.into(),
            value: choices.first().copied().unwrap_or(1),
            metadata: Some(saved::SettingMetadata {
                key: key.name().into(),
                feature: feature.0.into(),
                revision: 3,
                scope: if matches!(
                    feature,
                    FeatureId::FN_INVERSION_MULTI_HOST | FeatureId::MULTI_PLATFORM
                ) {
                    saved::SettingScope::CurrentHost
                } else {
                    saved::SettingScope::Device
                }
                .into(),
                choices,
                range: None,
            }),
        };
        for case in 0..10 {
            let mut preference = preference.clone();
            let metadata = preference.metadata.as_mut().unwrap();
            match case {
                0 => {}
                1 => preference.value = u32::MAX,
                2 => metadata.scope = 99,
                3 => metadata.revision = 256,
                4 => metadata.feature = 65536,
                5 => metadata.choices = vec![1, 1],
                6 => metadata.choices.push(65536),
                7 => {
                    metadata.range = Some(saved::Range {
                        min: 0,
                        max: 7,
                        step: 1,
                    })
                }
                8 => {
                    metadata.choices.clear();
                    metadata.range = Some(saved::Range {
                        min: 0,
                        max: 7,
                        step: 1,
                    });
                }
                _ => {
                    metadata.choices.clear();
                    metadata.range = Some(saved::Range {
                        min: 0,
                        max: 7,
                        step: 0,
                    });
                }
            }
            let bytes = saved::Settings {
                format: 2,
                preferences: vec![preference],
            }
            .encode_to_vec();
            let mut store = support::Store::default();
            store.records.insert(record_key(4, 1), bytes.clone());
            let runtime = block_on(
                cordial_core::storage::Preferences {
                    store: &mut store,
                    device: 1,
                }
                .load_all(),
            )
            .map(|_| ());
            if case == 0 {
                assert_eq!(runtime, Ok(()), "{key:?}");
            }
            allocation_free("/devices/1/settings.pb", &bytes, runtime);
        }
    }
}

#[test]
fn scale_and_axis_rules_use_scalar_decoders_without_allocating() {
    for (input, collection, effect, valid) in [
        (
            profiles::usage(1, 0x30),
            profiles::MOUSE,
            saved::rule::Effect::Scale(saved::Scale {
                numerator: -1,
                denominator: 2,
            }),
            true,
        ),
        (
            profiles::usage(1, 0x30),
            profiles::MOUSE,
            saved::rule::Effect::Scale(saved::Scale {
                numerator: i32::MIN,
                denominator: 1,
            }),
            true,
        ),
        (
            profiles::usage(1, 0x30),
            profiles::MOUSE,
            saved::rule::Effect::Scale(saved::Scale {
                numerator: 2,
                denominator: 4,
            }),
            false,
        ),
        (
            profiles::usage(1, 0x30),
            profiles::GAMEPAD,
            saved::rule::Effect::Axis(saved::Axis {
                destination: profiles::usage(1, 0x31),
                invert: true,
            }),
            true,
        ),
        (
            profiles::usage(1, 0x30),
            profiles::GAMEPAD,
            saved::rule::Effect::Axis(saved::Axis {
                destination: profiles::usage(1, 0x38),
                invert: false,
            }),
            false,
        ),
    ] {
        let bytes = saved::Rules {
            format: 2,
            rules: vec![saved::Rule {
                input,
                collection,
                effect: Some(effect),
            }],
        }
        .encode_to_vec();
        allocation_free(
            "/profiles/1/rules.pb",
            &bytes,
            if valid { Ok(()) } else { Err(Error::Corrupt) },
        );
    }
}

#[test]
fn whole_file_debug_streams_have_bounded_memory_and_late_errors_replace_the_file() {
    use cordial_core::{control::Session, files::Reader};
    use cordial_protocol::{self as p, response::Result as R};
    for size in [0, 1, 511, 512, 513, 8192, 256 * 1024] {
        for fail in [false, true] {
            if fail && size <= 512 {
                continue;
            }
            let mut store = RawStore::default();
            let source: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            store.files.insert("/data".into(), source.clone());
            store.read_limit = Some(137);
            let mut reader = block_on(Reader::new(
                &mut store,
                p::ReadFile {
                    path: "/data".into(),
                    offset: 0,
                    length: None,
                },
            ))
            .unwrap();
            let mut serial = Session::new();
            serial.session(true);
            serial.start_stream();
            let mut wire = Vec::new();
            let mut steps = 0;
            loop {
                let (finished, stats) = measured(|| block_on(reader.step(&mut serial, &mut store)));
                assert!(stats.peak <= 2048, "size {size}: {stats:?}");
                while let Some((token, bytes)) = serial.output(63) {
                    let count = bytes.len();
                    wire.extend_from_slice(bytes);
                    serial.output_complete(token, count);
                }
                steps += 1;
                if fail && steps == 2 {
                    store.fault = Some("/data".into());
                }
                if finished {
                    break;
                }
            }
            let mut decoder = p::frame::Decoder::new(None);
            let messages: Vec<_> = wire
                .into_iter()
                .filter_map(|byte| {
                    decoder
                        .push(byte)
                        .map(|result| p::Message::decode(result.unwrap()).unwrap())
                })
                .collect();
            assert_eq!(messages.len(), 1);
            let Some(p::message::Kind::Response(response)) = &messages[0].kind else {
                panic!("response")
            };
            if fail {
                let Some(R::Error(error)) = &response.result else {
                    panic!("partial file succeeded")
                };
                assert_eq!(error.code, p::ErrorCode::StorageFailed as i32);
            } else {
                let Some(R::File(file)) = &response.result else {
                    panic!("file")
                };
                assert_eq!(file.path, "/data");
                assert_eq!(file.offset, 0);
                assert_eq!(file.size, size as u64);
                assert_eq!(file.data, source);
            }
        }
    }
}
