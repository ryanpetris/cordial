use cordial_core::{bonds::resolves_rpa, identity::Identity};
use embassy_futures::block_on;
#[allow(dead_code)]
mod support;
#[test]
fn bluetooth_ah_known_vector_and_wrong_key() {
    let irk = [
        0xec, 0x02, 0x34, 0xa3, 0x57, 0xc8, 0xad, 0x05, 0x34, 0x10, 0x10, 0xa6, 0x0a, 0x39, 0x7d,
        0x9b,
    ];
    assert!(resolves_rpa(&irk, &[0x70, 0x81, 0x94, 0x0d, 0xfb, 0xaa]));
    assert!(!resolves_rpa(
        &[0; 16],
        &[0x70, 0x81, 0x94, 0x0d, 0xfb, 0xaa]
    ));
    assert!(!resolves_rpa(&irk, &[0xf0, 0x81, 0x94, 0x0d, 0xfb, 0xaa]));
}
#[test]
fn identity_derived_key_matches_aes_vector_and_detects_corruption() {
    let mut store = support::Store::default();
    let id = block_on(Identity::initialize(&mut store, [2; 6], || 0)).unwrap();
    assert_eq!(
        id.0[40..],
        [
            0x58, 0xe2, 0xfc, 0xce, 0xfa, 0x7e, 0x30, 0x61, 0x36, 0x7f, 0x1d, 0x57, 0xa4, 0xe7,
            0x45, 0x5a
        ]
    );
    let mut corrupt = id.0;
    corrupt[40] ^= 1;
    assert!(Identity::decode(corrupt).is_err());
    assert!(block_on(Identity::initialize(&mut store, [3; 6], || 0)).is_err());
}

#[test]
fn setting_commit_readback_and_maintenance_reserve() {
    use cordial_core::model::{
        hidpp::{FeatureId, FeatureRevision},
        settings::{SettingKey, SettingScope},
    };
    use cordial_core::{
        compact::{Metadata, Preference},
        settings::PreferenceStore,
        storage::{Preferences, record_key},
    };
    let mut store = support::Store::default();
    let mut preference = Preference {
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
    let key = record_key(4, 77);
    store.available = Some(0);
    assert_eq!(
        block_on(
            Preferences {
                store: &mut store,
                device: 77
            }
            .save(&preference)
        ),
        Err(cordial_core::settings::Error::StorageFull)
    );
    store.available = None;
    store.fail_save = Some((key, true));
    block_on(
        Preferences {
            store: &mut store,
            device: 77,
        }
        .save(&preference),
    )
    .unwrap();
    store.available = Some(0); // Existing settings can spend the maintenance reserve.
    preference.value = 0;
    block_on(
        Preferences {
            store: &mut store,
            device: 77,
        }
        .save(&preference),
    )
    .unwrap();
    store.fail_save = Some((key, false));
    preference.value = 1;
    assert_eq!(
        block_on(
            Preferences {
                store: &mut store,
                device: 77
            }
            .save(&preference)
        ),
        Err(cordial_core::settings::Error::Storage)
    );
    let values = block_on(
        Preferences {
            store: &mut store,
            device: 77,
        }
        .load_all(),
    )
    .unwrap();
    assert_eq!(values[0].value, 0);
}

#[test]
fn missing_local_roots_in_a_populated_store_are_not_regenerated() {
    let (_, mut store, _) = support::setup();
    assert_eq!(
        block_on(Identity::initialize(&mut store, [2; 6], || 42)),
        Err(cordial_core::storage::Error::Corrupt)
    );
    assert!(!store.records.contains_key(&cordial_core::identity::KEY));
}

#[test]
fn initialized_store_requires_the_monotonic_sequence_even_without_devices() {
    let mut store = support::Store::default();
    block_on(Identity::initialize(&mut store, [2; 6], || 42)).unwrap();
    let sequence = cordial_core::storage::record_key(7, 0);
    assert_eq!(store.records[&sequence], br#"{"device":0,"profile":0}"#);
    store.records.remove(&sequence);
    assert_eq!(
        block_on(Identity::initialize(&mut store, [2; 6], || 42)),
        Err(cordial_core::storage::Error::Corrupt)
    );
    assert!(!store.records.contains_key(&sequence));
}
