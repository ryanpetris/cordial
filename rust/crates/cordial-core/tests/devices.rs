use cordial_core::{
    compact::{Metadata, Preference},
    devices::*,
    settings::PreferenceStore,
    storage::{Error, Preferences, RecordKey, RecordStore},
};
use cordial_protocol::{
    errors::ErrorCode,
    hidpp::{FeatureId, FeatureRevision},
    identifiers::{ConnectionState, HostPlatform, Transport},
    settings::{SettingKey, SettingScope},
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
            })
            .await
            .unwrap();
        for slot in 0..8 {
            let p = Policy::paired(slot as u64 + 1, peer(slot as u8), &[b'"'; 128]);
            assert!(p.trusted && p.hidpp_enabled && !p.blocked);
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
        assert_eq!(policies.load_devices().await.unwrap().len(), 8);
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
        assert_eq!(policies.remove(0, 1).await, Err(Error::Io));
        assert_eq!(policies.load_devices().await.unwrap().len(), 8);
        policies.store.fail_remove = false;
        policies.remove(0, 1).await.unwrap();
        policies.remove(0, 1).await.unwrap();
        assert_eq!(policies.load_devices().await.unwrap().len(), 7);
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
        let invalid = Policy::paired(99, peer(1), b"duplicate identity");
        let mut bond = cordial_core::bonds::load(policies.store, 2)
            .await
            .unwrap()
            .unwrap();
        bond.owner = invalid.id;
        cordial_core::bonds::commit(policies.store, &invalid, &bond)
            .await
            .unwrap();
        assert_eq!(policies.load_devices().await, Err(Error::Corrupt));
    });
}
#[test]
fn reconnect_policy_and_names_survive_normal_runtime_transitions() {
    let p = Policy::paired(10, peer(1), b"Example\x1b\xff keyboard");
    assert_eq!(&*p.name, "Example ? keyboard");
    let text = "é".repeat(65);
    assert_eq!(display_name(text.as_bytes()).len(), 128);
    let mut d = Device::new(p);
    assert!(d.reconnect_due(0));
    d.connection(ConnectionState::Connecting, None, 0);
    assert!(!d.reconnect_due(100));
    d.connection(
        ConnectionState::Disconnected,
        Some(ErrorCode::ConnectionFailed),
        100,
    );
    assert!(!d.reconnect_due(5099));
    assert!(d.reconnect_due(5100));
    d.policy.trusted = false;
    assert!(!d.allow_incoming());
    d.explicit_connect().unwrap();
    d.connection(ConnectionState::Connected, None, 5101);
    assert_eq!(d.error, None);
    d.policy.trusted = true;
    d.paused = true;
    assert!(!d.allow_incoming());
    d.explicit_connect().unwrap();
    assert!(d.allow_incoming());
    d.policy.blocked = true;
    assert_eq!(d.explicit_connect(), Err(ErrorCode::Blocked));
    assert!(!d.allow_incoming());
    d.policy.blocked = false;
    d.connection(
        ConnectionState::Disconnected,
        Some(ErrorCode::AuthenticationFailed),
        6000,
    );
    assert!(!d.reconnect_due(60_000));
    d.explicit_connect().unwrap();
    assert!(d.reconnect_due(60_000));
}
