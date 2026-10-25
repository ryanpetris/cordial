mod support;
use cordial_core::model::{
    errors::ErrorCode as Error,
    identifiers::{ConnectionState, HostPlatform, Transport},
};
use cordial_core::{
    bluetooth::{Capabilities, InputReport},
    devices::{AdapterPreference, Peer, Policies, Policy},
    interfaces::{Interface, InterfacePreference},
    link::ServiceId,
    manager::Manager,
    storage::{RecordKey, RecordStore, record_key},
};
use cordial_protocol as p;
use embassy_futures::block_on;
use prost::Message;
use support::*;

/// The saved device with identity `peer`, read from every saved device record.
fn saved_peer(store: &mut Store, peer: Peer) -> Option<Policy> {
    let ids = block_on(store.record_ids(2, 0, usize::MAX)).unwrap();
    ids.into_iter()
        .filter_map(|id| block_on(Policies { store: &mut *store }.load(id)).ok())
        .find(|p| p.peer == peer)
}

fn policy(store: &mut Store, id: u64) -> Policy {
    block_on(Policies { store }.load(id)).unwrap()
}
fn record(manager: &Manager, store: &mut Store, id: u64) -> p::Device {
    let policy = policy(store, id);
    cordial_core::wire::device(manager, &policy, manager.find(id))
}
/// Saves a changed copy of device `id`'s policy through the manager.
fn change(
    manager: &mut Manager,
    store: &mut Store,
    radio: &mut Radio,
    id: u64,
    f: impl FnOnce(&mut Policy),
) -> Result<(), p::Error> {
    let mut policy = policy(store, id);
    f(&mut policy);
    block_on(manager.save_policy(policy, None, store, radio))
}
/// Commits a saved device with a bond, as pairing would.
fn add(store: &mut Store, id: u64, peer: Peer, f: impl FnOnce(&mut Policy)) {
    let mut policy = Policy::paired(id, peer, b"Saved");
    policy.setup_pending = false;
    f(&mut policy);
    block_on(cordial_core::bonds::commit(store, &policy, &bond(id, peer))).unwrap();
}
fn resident(manager: &Manager) -> Vec<u64> {
    let mut ids: Vec<u64> = manager.devices.iter().flatten().map(|d| d.id).collect();
    ids.sort();
    ids
}
fn classic_unsupported() -> Option<Capabilities> {
    Some(Capabilities {
        classic: false,
        ble: true,
        ble_scan_and_connect: false,
    })
}

#[test]
fn disconnect_releases_input_and_generation_blocks_late_events() {
    let (mut manager, _, mut radio) = setup();
    while manager.forward.packet().is_some() {
        manager.forward.complete();
    }
    let old = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    assert_eq!(manager.connected(old, descriptor(), 255, 0), Ok(Some(0)));
    let input = InputReport::new(old, ServiceId(7), 0, &[1]).unwrap();
    manager.input(&input, 1).unwrap();
    assert_eq!(manager.forward.packet().unwrap().bytes()[0], 16);
    manager.forward.complete();
    manager.disconnect(0, &mut radio).unwrap();
    assert_eq!(radio.closes, vec![old]);
    manager.disconnected(old, None, 2).unwrap();
    assert_eq!(manager.forward.packet().unwrap().bytes(), &[0; 32]);
    manager.forward.complete();
    // The connection's state is dropped with it.
    assert!(manager.devices[0].as_ref().unwrap().live.is_none());
    assert!(!manager.devices[0].as_ref().unwrap().admit_due(60_000));
    let new = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    assert_eq!(old.slot, new.slot);
    assert_ne!(old.generation, new.generation);
    manager.connected(new, descriptor(), 255, 3).unwrap();
    assert!(!manager.input(&input, 4).unwrap());
    assert!(manager.disconnected(old, None, 4).is_none());
    assert_eq!(
        manager.devices[0].as_ref().unwrap().state,
        ConnectionState::Connected
    );
    assert!(manager.forward.packet().is_none());
}
#[test]
fn policies_save_before_publish_and_unpair_keeps_adapter_preferences() {
    let (mut manager, mut store, mut radio) = setup();
    store.fail_save = Some((record_key(2, 77), false));
    assert_eq!(
        change(&mut manager, &mut store, &mut radio, 77, |p| p
            .set_hidpp(false)),
        Err(cordial_core::wire::error(Error::StorageFailed, None, false))
    );
    assert!(manager.devices[0].as_ref().unwrap().hidpp_enabled);
    store.fail_save = None;
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.set_hidpp(false)
    })
    .unwrap();
    assert!(!manager.devices[0].as_ref().unwrap().hidpp_enabled);
    assert!(!policy(&mut store, 77).hidpp_enabled());
    block_on(manager.platform(HostPlatform::Mac, &mut store)).unwrap();
    let id = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(id, descriptor(), 255, 0).unwrap();
    assert_eq!(
        block_on(manager.unpair(77, peer(1), &mut store, &mut radio)),
        Err(cordial_core::wire::error(Error::Busy, None, false))
    );
    manager.disconnect(0, &mut radio).unwrap();
    manager.disconnected(id, None, 1).unwrap();
    block_on(manager.unpair(77, peer(1), &mut store, &mut radio)).unwrap();
    assert!(radio.bonds.is_empty());
    assert!(manager.devices[0].is_none());
    let mut reloaded = Manager::default();
    block_on(reloaded.load(&mut store, &mut radio)).unwrap();
    assert_eq!(
        reloaded.preference,
        AdapterPreference {
            name: None,
            host_platform: HostPlatform::Mac,
            transports: manager.preference.transports,
            ..Default::default()
        }
    );
    assert!(reloaded.devices.iter().all(Option::is_none));
}
#[test]
fn security_is_observed_per_connection_and_never_carried_across_generations() {
    use cordial_core::bluetooth::ConnectionSecurity;
    let (mut manager, mut store, mut radio) = setup();
    let id = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    let initial = ConnectionSecurity {
        encrypted: Some(true),
        authenticated: Some(false),
        secure_connections: Some(true),
        key_size: Some(16),
        bonded: Some(true),
    };
    let wire = p::Security {
        encrypted: Some(true),
        authenticated: Some(false),
        secure_connections: Some(true),
        key_size: Some(16),
    };
    assert_eq!(manager.security(id, initial), None);
    assert_eq!(record(&manager, &mut store, 77).security, None);
    manager.connected(id, descriptor(), 255, 0).unwrap();
    assert_eq!(record(&manager, &mut store, 77).security, Some(wire));
    assert_eq!(manager.security(id, initial), None); // No redundant notification.
    let changed = ConnectionSecurity {
        authenticated: Some(true),
        ..initial
    };
    assert_eq!(manager.security(id, changed), Some(0));
    manager.disconnect(0, &mut radio).unwrap();
    assert_eq!(record(&manager, &mut store, 77).security, None);
    assert_eq!(manager.security(id, initial), None);
    manager.disconnected(id, None, 1).unwrap();
    let next = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    assert_ne!(id, next);
    assert_eq!(manager.security(id, changed), None);
    manager.connected(next, descriptor(), 255, 2).unwrap();
    assert_eq!(record(&manager, &mut store, 77).security, None);
    assert_eq!(
        manager.security(next, ConnectionSecurity::default()),
        Some(0)
    );
    assert_eq!(
        record(&manager, &mut store, 77)
            .security
            .unwrap()
            .authenticated,
        None
    );
}

#[test]
fn replacement_commit_preserves_policy_and_reboot_selects_new_bond() {
    let (mut manager, mut store, mut radio) = setup();
    let original = policy(&mut store, 77);
    block_on(manager.prepare_pair(peer(1), Some(peer(1)), &mut store, &mut radio)).unwrap();
    assert_eq!(policy(&mut store, 77), original);
    assert!(radio.bonds.is_empty());
    let link = manager.pair(peer(1), 90_000, &mut radio).unwrap();
    radio.bonds.push(peer(1));
    let (id, slot, _) = block_on(manager.bonded(
        link,
        peer(1),
        saved_peer(&mut store, peer(1)),
        b"new name",
        &mut store,
        &mut radio,
    ))
    .unwrap();
    assert_eq!((id, slot), (77, Some(0)));
    let updated = policy(&mut store, 77);
    assert_eq!(updated.name, original.name);
    assert_eq!(updated.bond, original.bond);
    let mut reboot = Manager::default();
    block_on(reboot.load(&mut store, &mut radio)).unwrap();
    assert_eq!(resident(&reboot), [77]);
    assert_eq!(radio.bonds, vec![peer(1)]);
}
#[test]
fn interrupted_pairing_leaves_the_committed_device_unchanged() {
    let (mut manager, mut store, mut radio) = setup();
    let original = store.records[&record_key(2, 77)].clone();
    block_on(manager.prepare_pair(peer(1), Some(peer(1)), &mut store, &mut radio)).unwrap();
    let mut reboot = Manager::default();
    block_on(reboot.load(&mut store, &mut radio)).unwrap();
    assert_eq!(store.records[&record_key(2, 77)], original);
    assert_eq!(radio.bonds, vec![peer(1)]);
}
#[test]
fn unsupported_transport_keeps_the_device_and_a_lost_bond_deletes_it() {
    let (mut manager, mut store, mut radio) = setup();
    radio.transports = classic_unsupported();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    // Saved, but not resident: the stack cannot hold it.
    assert!(manager.find(77).is_none());
    let d = record(&manager, &mut store, 77);
    assert_eq!(
        d.inactive,
        Some(p::InactiveReason::UnsupportedTransport as i32)
    );
    assert!(d.enabled);
    let key = record_key(2, 77);
    lose_bond(&mut store.records, key);
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert!(manager.devices.iter().all(Option::is_none));
    assert!(!store.records.contains_key(&key));
}
/// Gives the saved device record at `key` a bond of another device, so the record is lost.
fn lose_bond(records: &mut std::collections::BTreeMap<RecordKey, Vec<u8>>, key: RecordKey) {
    let mut record = cordial_protocol::storage::Device::decode(records[&key].as_slice()).unwrap();
    record.bond.as_mut().unwrap().owner += 1;
    records.insert(key, record.encode_to_vec());
}
/// The last device ID the saved sequence allocated.
fn last_device(store: &Store) -> u64 {
    cordial_protocol::storage::Sequence::decode(store.records[&record_key(7, 0)].as_slice())
        .unwrap()
        .next_device
        - 1
}
#[test]
fn a_lost_record_whose_cleanup_fails_is_still_reported_removed() {
    let (mut manager, mut store, mut radio) = setup();
    let key = record_key(2, 77);
    lose_bond(&mut store.records, key);
    store.fail_remove = Some(key);
    assert_eq!(
        block_on(manager.sync_bonds(&mut store, &mut radio)),
        Err((Error::StorageFailed, p::ErrorReason::Unknown))
    );
    assert!(manager.devices.iter().all(Option::is_none));
    assert_eq!(manager.removed, [77]);
}
#[test]
fn additional_pairs_are_saved_disabled_and_enable_does_not_evict() {
    let (mut manager, mut store, mut radio) = setup();
    let mut ids = Vec::new();
    for n in 2..=9 {
        block_on(manager.prepare_pair(peer(n), None, &mut store, &mut radio)).unwrap();
        let link = manager.pair(peer(n), 90_000, &mut radio).unwrap();
        radio.bonds.push(peer(n));
        let (id, slot, _) = block_on(manager.bonded(
            link,
            peer(n),
            saved_peer(&mut store, peer(n)),
            b"extra",
            &mut store,
            &mut radio,
        ))
        .unwrap();
        // Devices beyond the normal allowance are saved disabled.
        assert_eq!(
            policy(&mut store, id).enabled,
            usize::from(n) <= cordial_core::devices::CLASSIC_CONNECTIONS
        );
        assert_eq!(
            slot.is_some(),
            usize::from(n) <= cordial_core::devices::CLASSIC_CONNECTIONS
        );
        manager.disconnected(link, None, 0);
        block_on(manager.finish_pair(&mut store)).unwrap();
        block_on(manager.sync_bonds(&mut store, &mut radio)).unwrap();
        ids.push(id);
    }
    assert_eq!(
        manager.devices.iter().flatten().count(),
        cordial_core::devices::CLASSIC_CONNECTIONS
    );
    assert_eq!(
        radio.bonds.len(),
        cordial_core::devices::CLASSIC_CONNECTIONS
    );
    let last = *ids.last().unwrap();
    assert_eq!(
        change(&mut manager, &mut store, &mut radio, last, |p| p.enabled =
            true),
        Err(p::Error {
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..cordial_core::wire::error(
                Error::Capacity,
                Some(p::ErrorReason::EnabledDevices),
                false
            )
        })
    );
    assert!(!policy(&mut store, last).enabled);
    assert!(manager.find(last).is_none());
    assert_eq!(
        record(&manager, &mut store, last).inactive,
        Some(p::InactiveReason::Disabled as i32)
    );
}

#[test]
fn ambiguous_device_commit_is_resolved_from_storage() {
    for committed in [false, true] {
        let (mut manager, mut store, mut radio) = setup();
        let original = policy(&mut store, 77);
        block_on(manager.prepare_pair(peer(1), Some(peer(1)), &mut store, &mut radio)).unwrap();
        let link = manager.pair(peer(1), 90_000, &mut radio).unwrap();
        store.fail_save = Some((record_key(2, 77), committed));
        let result = block_on(manager.bonded(
            link,
            peer(1),
            saved_peer(&mut store, peer(1)),
            b"new",
            &mut store,
            &mut radio,
        ));
        assert_eq!(result.is_ok(), committed);
        assert_eq!(radio.adopted, [link]);
        assert_eq!(radio.promoted.contains(&link), committed);
        assert_eq!(manager.connection(link).unwrap().pairing, !committed);
        manager.close(link, None, &mut radio);
        assert_eq!(
            manager.connection_count(Transport::Classic),
            usize::from(committed)
        );
        manager.disconnected(link, None, 0);
        block_on(manager.finish_pair(&mut store)).unwrap();
        block_on(manager.sync_bonds(&mut store, &mut radio)).unwrap();
        assert_eq!(policy(&mut store, 77).bond, original.bond);
        assert_eq!(resident(&manager), [77]);
    }
}
#[test]
fn malformed_device_record_is_deleted() {
    let (mut manager, mut store, mut radio) = setup();
    let key = record_key(2, 77);
    store.records.insert(key, vec![0xff]);
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert!(manager.devices.iter().all(Option::is_none));
    assert!(!store.records.contains_key(&key));
}
#[test]
fn a_later_record_for_a_saved_identity_is_deleted_at_startup() {
    let (mut manager, mut store, mut radio) = setup();
    add(&mut store, 90, peer(1), |_| {});
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert_eq!(resident(&manager), [77]);
    assert!(!store.records.contains_key(&record_key(2, 90)));
    assert!(store.records.contains_key(&record_key(2, 77)));
}
#[test]
fn a_read_error_fails_the_load_and_deletes_nothing() {
    let (mut manager, mut store, mut radio) = setup();
    let saved = store.records.clone();
    store.fail = true;
    assert_eq!(
        block_on(manager.load(&mut store, &mut radio)),
        Err(Error::StorageFailed)
    );
    store.fail = false;
    assert_eq!(store.records, saved);
    assert!(!manager.storage_ready);
}
#[test]
fn pairing_needs_new_device_budget_even_for_a_duplicate() {
    let (mut manager, mut store, mut radio) = setup();
    store.available = Some(cordial_core::bonds::MAINTENANCE_BYTES);
    assert_eq!(
        block_on(manager.prepare_pair(peer(1), Some(peer(1)), &mut store, &mut radio)),
        Err(Error::StorageFull)
    );
    assert_eq!(radio.bonds, vec![peer(1)]);
}
#[test]
fn changed_final_ble_identity_does_not_replace_the_provisional_owner() {
    let (mut manager, mut store, mut radio) = setup();
    let mut original = policy(&mut store, 77);
    original.peer.transport = Transport::Ble;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &original,
        &bond(original.id, original.peer),
    ))
    .unwrap();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    let final_peer = Peer {
        transport: Transport::Ble,
        ..peer(9)
    };
    block_on(manager.prepare_pair(original.peer, Some(original.peer), &mut store, &mut radio))
        .unwrap();
    let link = manager.pair(original.peer, 90_000, &mut radio).unwrap();
    let (id, slot, _) = block_on(manager.bonded(
        link,
        final_peer,
        saved_peer(&mut store, final_peer),
        b"different",
        &mut store,
        &mut radio,
    ))
    .unwrap();
    assert_ne!(id, 77);
    assert_ne!(slot, Some(0));
    assert_eq!(policy(&mut store, 77), original);
    assert_eq!(manager.devices[0].as_ref().unwrap().peer, original.peer);
    // Ending the pairing leaves the stack's bonds to the next sync, which restores the original's.
    assert!(manager.bonds_pending);
    block_on(manager.sync_bonds(&mut store, &mut radio)).unwrap();
    assert!(radio.bonds.contains(&original.peer));
}

#[test]
fn unblock_at_capacity_preserves_the_working_selection() {
    let (mut manager, mut store, mut radio) = setup();
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.blocked = true
    })
    .unwrap();
    assert!(manager.find(77).is_none());
    for n in 2..=8 {
        add(&mut store, 100 + u64::from(n), peer(n), |_| {});
    }
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    manager.radio_ready = true;
    let end = 102 + cordial_core::devices::CLASSIC_CONNECTIONS as u64;
    assert_eq!(resident(&manager), (102..end).collect::<Vec<_>>());
    let slot = manager.find(end - 1).unwrap();
    let link = manager
        .connect(slot, true, 90_000, None, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(link, descriptor(), 255, 0).unwrap();
    assert_eq!(
        change(&mut manager, &mut store, &mut radio, 77, |p| p.blocked =
            false),
        Err(p::Error {
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..cordial_core::wire::error(
                Error::Capacity,
                Some(p::ErrorReason::EnabledDevices),
                false
            )
        })
    );
    assert!(policy(&mut store, 77).blocked);
    assert_eq!(
        manager.devices[slot].as_ref().unwrap().state,
        ConnectionState::Connected
    );
}
#[test]
fn failed_delete_keeps_the_previous_complete_device() {
    let (mut manager, mut store, mut radio) = setup();
    store.fail_remove = Some(record_key(2, 77));
    assert_eq!(
        block_on(manager.unpair(77, peer(1), &mut store, &mut radio)),
        Err(cordial_core::wire::error(Error::StorageFailed, None, false))
    );
    assert!(radio.bonds.contains(&peer(1)));
    assert!(!manager.devices[0].as_ref().unwrap().deleting);
    assert!(policy(&mut store, 77).enabled);
    store.fail_remove = None;
    radio.transports = classic_unsupported();
    let mut reboot = Manager::default();
    block_on(reboot.load(&mut store, &mut radio)).unwrap();
    assert!(reboot.devices.iter().all(Option::is_none));
    assert!(store.records.contains_key(&record_key(2, 77)));
}

#[test]
fn policy_and_adapter_commit_errors_are_read_back_before_publishing() {
    for committed in [false, true] {
        let (mut manager, mut store, mut radio) = setup();
        let trusted = manager.devices[0].as_ref().unwrap().trusted;
        store.fail_save = Some((record_key(2, 77), committed));
        assert_eq!(
            change(&mut manager, &mut store, &mut radio, 77, |p| p.trusted =
                !trusted)
            .is_ok(),
            committed
        );
        assert_eq!(
            manager.devices[0].as_ref().unwrap().trusted != trusted,
            committed
        );
        assert!(!manager.write_uncertain);
        store.fail_save = Some((record_key(1, 0), committed));
        assert_eq!(
            block_on(manager.platform(HostPlatform::Mac, &mut store)).is_ok(),
            committed
        );
        assert_eq!(
            manager.preference.host_platform == HostPlatform::Mac,
            committed
        );
    }
    let (mut manager, mut store, _) = setup();
    store.fail = true;
    assert_eq!(
        block_on(manager.platform(HostPlatform::Mac, &mut store)),
        Err(Error::StorageFailed)
    );
    assert!(!manager.write_uncertain);
}
#[test]
fn deleting_highest_device_does_not_reuse_its_id() {
    let (mut manager, mut store, mut radio) = setup();
    block_on(manager.prepare_pair(peer(2), None, &mut store, &mut radio)).unwrap();
    let first = last_device(&store);
    let link = manager.pair(peer(2), 90_000, &mut radio).unwrap();
    let (id, _, _) = block_on(manager.bonded(
        link,
        peer(2),
        saved_peer(&mut store, peer(2)),
        b"new",
        &mut store,
        &mut radio,
    ))
    .unwrap();
    assert_eq!(id, first);
    manager.disconnected(link, None, 0);
    block_on(manager.unpair(id, peer(2), &mut store, &mut radio)).unwrap();
    assert!(!store.records.contains_key(&record_key(2, first)));
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    block_on(manager.prepare_pair(peer(3), None, &mut store, &mut radio)).unwrap();
    let next = last_device(&store);
    assert!(next > first);
}
#[test]
fn late_identity_cannot_take_over_an_existing_live_link() {
    let (mut manager, mut store, mut radio) = setup();
    let mut original = policy(&mut store, 77);
    original.peer.transport = Transport::Ble;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &original,
        &bond(original.id, original.peer),
    ))
    .unwrap();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    let old = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(old, descriptor(), 255, 0).unwrap();
    let candidate = Peer {
        transport: original.peer.transport,
        ..peer(2)
    };
    block_on(manager.prepare_pair(candidate, None, &mut store, &mut radio)).unwrap();
    let new = manager.pair(candidate, 90_000, &mut radio).unwrap();
    // Use BLE: Classic identities cannot change after pairing.
    assert_eq!(
        block_on(manager.bonded(
            new,
            original.peer,
            saved_peer(&mut store, original.peer),
            b"new",
            &mut store,
            &mut radio
        )),
        Err(p::Error {
            code: p::ErrorCode::Busy as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        })
    );
    assert_eq!(manager.link_for(0), Some(old));
    assert_eq!(policy(&mut store, 77), original);
}
#[test]
fn device_and_bond_are_one_record() {
    let (_, store, _) = setup();
    assert!(store.records.keys().all(|key| !matches!(key[0], 5 | 6)));
    let record =
        cordial_protocol::storage::Device::decode(store.records[&record_key(2, 77)].as_slice())
            .unwrap();
    assert_eq!(record.policy.unwrap().id, 77);
    let Some(cordial_protocol::storage::bond::Keys::Classic(keys)) = record.bond.unwrap().keys
    else {
        panic!("a Classic bond");
    };
    assert_eq!(keys.link_key.len(), 16);
}

#[test]
fn disabling_ble_hidpp_requests_fresh_standard_battery() {
    use cordial_core::model::info::InfoKey;
    let (mut manager, mut store, mut radio) = setup();
    let mut original = policy(&mut store, 77);
    original.peer.transport = Transport::Ble;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &original,
        &bond(original.id, original.peer),
    ))
    .unwrap();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    let id = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(id, descriptor(), 255, 0).unwrap();
    fn battery(manager: &mut Manager) -> &mut cordial_core::battery::Battery {
        &mut manager.devices[0]
            .as_mut()
            .unwrap()
            .live
            .as_mut()
            .unwrap()
            .catalog
            .info
            .battery
    }
    battery(&mut manager).hidpp_reports(true);
    battery(&mut manager).vendor_reading(Some(75), Some(true));
    assert!(
        battery(&mut manager)
            .field(InfoKey::BatteryPercent)
            .available
    );
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.set_hidpp(false)
    })
    .unwrap();
    assert!(
        !battery(&mut manager)
            .field(InfoKey::BatteryPercent)
            .available
    );
    radio.reject_info_refresh = true;
    manager
        .poll_link(id.slot as usize, 0, 1, &mut radio)
        .unwrap();
    assert!(radio.info_refreshes.is_empty());
    radio.reject_info_refresh = false;
    manager
        .poll_link(id.slot as usize, 0, 2, &mut radio)
        .unwrap();
    manager
        .poll_link(id.slot as usize, 0, 3, &mut radio)
        .unwrap();
    assert_eq!(radio.info_refreshes, vec![id]);
}

#[test]
fn only_enabled_devices_the_stack_has_room_for_are_resident() {
    let (mut manager, mut store, mut radio) = setup();
    // Saved devices include disabled and blocked entries.
    for n in 2..=10u8 {
        add(&mut store, u64::from(n), peer(n), |p| {
            p.enabled = !(2..=4).contains(&n);
            p.blocked = n == 5;
        });
    }
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    // The lowest enabled IDs fit. Disabled and blocked devices stay on flash.
    let expected: Vec<_> = [6, 7, 8, 9, 10, 77]
        .into_iter()
        .take(cordial_core::devices::CLASSIC_CONNECTIONS)
        .collect();
    assert_eq!(resident(&manager), expected);
    assert_eq!(
        radio.bonds.len(),
        cordial_core::devices::CLASSIC_CONNECTIONS
    );
    for (id, reason) in [
        (2, p::InactiveReason::Disabled),
        (5, p::InactiveReason::Blocked),
    ] {
        assert_eq!(
            record(&manager, &mut store, id).inactive,
            Some(reason as i32)
        );
    }
    let last = *expected.last().unwrap();
    assert_eq!(record(&manager, &mut store, last).inactive, None);
    // Disabling a device without a link leaves at once and frees room for another.
    let removed_peer = policy(&mut store, last).peer;
    change(&mut manager, &mut store, &mut radio, last, |p| {
        p.enabled = false
    })
    .unwrap();
    assert!(manager.find(last).is_none());
    assert!(!radio.bonds.contains(&removed_peer));
    assert!(manager.vacated);
    assert_eq!(
        record(&manager, &mut store, last).inactive,
        Some(p::InactiveReason::Disabled as i32)
    );
    change(&mut manager, &mut store, &mut radio, 2, |p| {
        p.enabled = true
    })
    .unwrap();
    assert!(manager.find(2).is_some());
    assert!(radio.bonds.contains(&peer(2)));
    // A full stack refuses another.
    assert_eq!(
        change(&mut manager, &mut store, &mut radio, 3, |p| p.enabled =
            true),
        Err(p::Error {
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..cordial_core::wire::error(
                Error::Capacity,
                Some(p::ErrorReason::EnabledDevices),
                false
            )
        })
    );
    assert!(!policy(&mut store, 3).enabled);
}
#[test]
fn a_smaller_stack_loads_the_lowest_ids_and_fill_adds_the_rest_when_room_frees() {
    let (mut manager, mut store, mut radio) = setup();
    for n in 2..=9u8 {
        add(&mut store, u64::from(n), peer(n), |_| {});
    }
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    // The lowest enabled IDs fit; the highest IDs report capacity.
    let end = 2 + cordial_core::devices::CLASSIC_CONNECTIONS as u64;
    assert_eq!(resident(&manager), (2..end).collect::<Vec<_>>());
    assert_eq!(
        record(&manager, &mut store, 77).inactive,
        Some(p::InactiveReason::Capacity as i32)
    );
    assert!(policy(&mut store, 77).enabled);
    // Disabling one frees room; filling makes the lowest waiting device resident.
    change(&mut manager, &mut store, &mut radio, 3, |p| {
        p.enabled = false
    })
    .unwrap();
    assert_eq!(block_on(manager.fill(&mut store, &mut radio)), Ok(true));
    assert_eq!(
        resident(&manager),
        (2..=end).filter(|&id| id != 3).collect::<Vec<_>>()
    );
    assert_eq!(manager.changed, [end]);
    assert!(radio.bonds.contains(&peer(end as u8)));
    assert_eq!(block_on(manager.fill(&mut store, &mut radio)), Ok(false));
}
#[test]
fn a_connected_device_retires_once_its_link_closes() {
    let (mut manager, mut store, mut radio) = setup();
    let link = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(link, descriptor(), 255, 0).unwrap();
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.enabled = false
    })
    .unwrap();
    assert_eq!(radio.closes, [link]);
    let d = manager.devices[0].as_ref().unwrap();
    assert!(d.retiring && !d.allow_incoming());
    assert_eq!(
        manager.connect(0, true, 30_000, None, &mut radio),
        Err(Error::Disabled)
    );
    manager.disconnected(link, None, 1).unwrap();
    assert!(manager.vacated);
    assert_eq!(block_on(manager.fill(&mut store, &mut radio)), Ok(true));
    assert!(manager.find(77).is_none());
    assert_eq!(manager.changed, [77]);
    assert!(!radio.bonds.contains(&peer(1)));
}
#[test]
fn re_enabling_before_the_link_closes_keeps_the_device_resident() {
    let (mut manager, mut store, mut radio) = setup();
    let link = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(link, descriptor(), 255, 0).unwrap();
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.enabled = false
    })
    .unwrap();
    assert!(manager.devices[0].as_ref().unwrap().retiring);
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.enabled = true
    })
    .unwrap();
    assert!(!manager.devices[0].as_ref().unwrap().retiring);
    manager.disconnected(link, None, 1).unwrap();
    block_on(manager.fill(&mut store, &mut radio)).unwrap();
    assert_eq!(manager.find(77), Some(0));
    assert!(radio.bonds.contains(&peer(1)));
    assert!(manager.devices[0].as_ref().unwrap().allow_incoming());
}
#[test]
fn startup_drops_references_to_profiles_that_do_not_exist() {
    let (mut manager, mut store, mut radio) = setup();
    let (kept, _) = block_on(cordial_core::profiles::create(
        &mut store,
        "Kept",
        &Default::default(),
    ))
    .unwrap();
    let mut saved = policy(&mut store, 77);
    saved.profiles = vec![kept, 500, kept + 1];
    block_on(Policies { store: &mut store }.save(&saved)).unwrap();
    let mut preference = manager.preference.clone();
    preference.configuration_interfaces = vec![
        InterfacePreference {
            interface: Interface::Via,
            enabled: true,
            profile: Some(500),
        },
        InterfacePreference {
            interface: Interface::Vial,
            enabled: false,
            profile: Some(kept),
        },
    ];
    block_on(Policies { store: &mut store }.save_adapter(&preference)).unwrap();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert_eq!(policy(&mut store, 77).profiles, [kept]);
    assert_eq!(manager.devices[0].as_ref().unwrap().layers, [kept]);
    // The interface whose profile is gone is cleared and disabled; the other is kept.
    assert_eq!(
        manager.preference.configuration_interfaces,
        [InterfacePreference {
            interface: Interface::Vial,
            enabled: false,
            profile: Some(kept),
        }]
    );
    assert_eq!(
        block_on(Policies { store: &mut store }.load_adapter()).unwrap(),
        manager.preference
    );
}

#[test]
fn loading_starts_with_nothing_queued_from_before() {
    let (mut manager, mut store, mut radio) = setup();
    manager.removed.push(5);
    manager.changed.push(6);
    manager.lost_profiles.push(7);
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert!(manager.removed.is_empty());
    assert!(manager.changed.is_empty());
    assert!(manager.lost_profiles.is_empty());
}

#[test]
fn a_failed_fill_is_left_for_the_next_one() {
    let (mut manager, mut store, mut radio) = setup();
    add(&mut store, 78, peer(2), |_| {});
    manager.vacated = true;
    store.fail = true;
    assert!(block_on(manager.fill(&mut store, &mut radio)).is_err());
    assert!(manager.vacated);
    assert_eq!(resident(&manager), [77]);
    store.fail = false;
    assert_eq!(block_on(manager.fill(&mut store, &mut radio)), Ok(true));
    assert!(!manager.vacated && !manager.bonds_pending);
    assert_eq!(resident(&manager), [77, 78]);
}

#[test]
fn only_forwarded_input_ends_the_first_input_wait() {
    use cordial_core::bluetooth::Descriptor;
    let (mut manager, _, mut radio) = setup();
    let link = manager
        .connect(0, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    // A boot keyboard as report 1, and HID++ short and long reports.
    let descriptor = Descriptor::from_slice(
        ServiceId(7),
        &[
            5, 1, 9, 6, 0xa1, 1, 0x85, 1, 5, 7, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0, 0x25, 1, 0x75, 1,
            0x95, 8, 0x81, 2, 0x95, 1, 0x75, 8, 0x81, 1, 0x95, 6, 0x75, 8, 0x15, 0, 0x25, 0x65,
            0x19, 0, 0x29, 0x65, 0x81, 0, 0xc0, 0x06, 0x00, 0xff, 0x09, 1, 0xa1, 1, 0x85, 0x10,
            0x75, 8, 0x95, 6, 0x15, 0, 0x26, 0xff, 0, 0x09, 1, 0x81, 0, 0x09, 1, 0x91, 0, 0x85,
            0x11, 0x95, 19, 0x09, 2, 0x81, 0, 0x09, 2, 0x91, 0, 0xc0,
        ],
    )
    .unwrap();
    assert_eq!(
        manager.connected(link, vec![descriptor], 255, 0),
        Ok(Some(0))
    );
    assert!(manager.starting(1));
    // Keyboard rollover and an unsolicited HID++ report forward nothing.
    let rollover = InputReport::new(link, ServiceId(7), 1, &[0, 0, 1, 1, 1, 1, 1, 1]).unwrap();
    assert!(!manager.input(&rollover, 1).unwrap());
    assert!(manager.starting(2));
    let hidpp = InputReport::new(link, ServiceId(7), 0x10, &[0xff, 0x05, 0x1a, 0, 0, 0]).unwrap();
    manager.input(&hidpp, 2).unwrap();
    assert!(manager.starting(3));
    let key = InputReport::new(link, ServiceId(7), 1, &[0, 0, 4, 0, 0, 0, 0, 0]).unwrap();
    manager.input(&key, 3).unwrap();
    assert!(!manager.starting(4));
}

#[test]
fn output_pools_count_nonresident_selections_and_keep_uncapped_counts() {
    use cordial_core::outputs::{Plan, Preference};
    use p::{OutputMode, Role};
    let (mut manager, mut store, mut radio) = setup();
    for n in 2..=6 {
        add(&mut store, n.into(), peer(n), |p| {
            p.enabled = false;
            p.roles = cordial_core::devices::Roles(cordial_core::outputs::GAMEPAD);
            p.output_preferences.push(Preference {
                role: Role::Gamepad,
                mode: OutputMode::Hid,
            });
        });
    }
    add(&mut store, 7, peer(7), |p| {
        p.blocked = true;
        p.output_preferences.push(Preference {
            role: Role::Gamepad,
            mode: OutputMode::Xinput,
        });
    });
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert_eq!(resident(&manager), [77]);
    assert_eq!(
        manager.outputs.plan(),
        Plan {
            hid_gamepads: 4,
            xinput_gamepads: 1
        }
    );
    let d = record(&manager, &mut store, 2);
    assert_eq!(d.kinds, [p::Kind::Gamepad as i32]);
    assert_eq!(
        d.outputs[0].status,
        Some(p::output_status::Status::Error(p::Error {
            code: p::ErrorCode::Unsupported as i32,
            reason: p::ErrorReason::OutputBackend as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }))
    );
    assert_eq!(d.outputs[0].slot, None);
    store.fail_remove = Some(record_key(2, 2));
    assert!(block_on(manager.unpair(2, peer(2), &mut store, &mut radio)).is_err());
    assert_eq!(manager.outputs.plan().hid_gamepads, 4);
    store.fail_remove = None;
    block_on(manager.unpair(2, peer(2), &mut store, &mut radio)).unwrap();
    assert_eq!(manager.outputs.plan().hid_gamepads, 4);
    change(&mut manager, &mut store, &mut radio, 3, |p| {
        p.output_preferences.clear()
    })
    .unwrap();
    assert_eq!(manager.outputs.plan().hid_gamepads, 3);
    // Lost nonresident records also release their saved selection.
    store.records.remove(&record_key(2, 4));
    block_on(manager.lose(4, &mut store, &mut radio)).unwrap();
    assert_eq!(manager.outputs.plan().hid_gamepads, 2);
    let transports = cordial_core::devices::Transports::NONE;
    let preference = AdapterPreference {
        transports,
        ..manager.preference.clone()
    };
    block_on(manager.adapter(preference, &mut store)).unwrap();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert!(resident(&manager).is_empty());
    assert_eq!(
        manager.outputs.plan(),
        Plan {
            hid_gamepads: 2,
            xinput_gamepads: 1
        }
    );
}

#[test]
fn renewed_pairing_preserves_output_preferences_without_counting_twice() {
    use cordial_core::outputs::Preference;
    let (mut manager, mut store, mut radio) = setup();
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.output_preferences.push(Preference {
            role: p::Role::Gamepad,
            mode: p::OutputMode::Hid,
        });
    })
    .unwrap();
    block_on(manager.prepare_pair(peer(1), Some(peer(1)), &mut store, &mut radio)).unwrap();
    let link = manager.pair(peer(1), 90_000, &mut radio).unwrap();
    block_on(manager.bonded(
        link,
        peer(1),
        saved_peer(&mut store, peer(1)),
        b"Gamepad",
        &mut store,
        &mut radio,
    ))
    .unwrap();
    assert_eq!(
        policy(&mut store, 77).output_preferences[0].mode,
        p::OutputMode::Hid
    );
    assert_eq!(manager.outputs.plan().hid_gamepads, 1);
    let mut reboot = Manager::default();
    block_on(reboot.load(&mut store, &mut radio)).unwrap();
    assert_eq!(reboot.outputs.plan(), manager.outputs.plan());
}

#[test]
fn filling_lost_nonresident_records_releases_output_counts() {
    use cordial_core::outputs::{Plan, Preference};
    for missing in [true, false] {
        let (mut manager, mut store, mut radio) = setup();
        add(&mut store, 2, peer(2), |policy| {
            policy.enabled = false;
            policy.output_preferences.push(Preference {
                role: p::Role::Gamepad,
                mode: p::OutputMode::Hid,
            });
        });
        block_on(manager.load(&mut store, &mut radio)).unwrap();
        assert!(manager.find(2).is_none());
        let key = record_key(2, 2);
        store.fail_load = Some(key);
        assert_eq!(
            block_on(manager.fill_record(2, &mut store, &mut radio)),
            Err(Error::StorageFailed)
        );
        assert_eq!(manager.outputs.plan().hid_gamepads, 1);
        assert!(manager.removed.is_empty());
        store.fail_load = None;
        if missing {
            store.records.remove(&key);
        } else {
            store.records.insert(key, UNDECODABLE.to_vec());
            block_on(manager.fill(&mut store, &mut radio)).unwrap();
        }
        for id in [2, 0, 999] {
            assert!(!block_on(manager.fill_record(id, &mut store, &mut radio)).unwrap());
        }
        assert_eq!(manager.outputs.plan(), Plan::default());
        assert_eq!(manager.removed, [2]);
        assert!(!store.records.contains_key(&key));
    }
}

#[test]
fn saving_a_policy_with_a_lost_bond_releases_output_counts() {
    use cordial_core::outputs::{Plan, Preference};
    for missing in [true, false] {
        let (mut manager, mut store, mut radio) = setup();
        change(&mut manager, &mut store, &mut radio, 77, |policy| {
            policy.output_preferences.push(Preference {
                role: p::Role::Gamepad,
                mode: p::OutputMode::Xinput,
            });
        })
        .unwrap();
        let saved = policy(&mut store, 77);
        let key = record_key(2, 77);
        store.fail_load = Some(key);
        assert_eq!(
            block_on(manager.save_policy(saved.clone(), None, &mut store, &mut radio)),
            Err(cordial_core::wire::error(Error::StorageFailed, None, false))
        );
        assert_eq!(manager.outputs.plan().xinput_gamepads, 1);
        assert!(manager.removed.is_empty());
        store.fail_load = None;
        if missing {
            store.records.remove(&key);
        } else {
            lose_bond(&mut store.records, key);
        }
        assert_eq!(
            block_on(manager.save_policy(saved, None, &mut store, &mut radio)),
            Err(cordial_core::wire::error(Error::NotFound, None, false))
        );
        assert_eq!(manager.outputs.plan(), Plan::default());
        assert_eq!(manager.removed, [77]);
        assert!(manager.find(77).is_none());
        assert!(!store.records.contains_key(&key));
    }
}

#[test]
fn a_lost_device_with_failed_cleanup_cannot_become_resident_again() {
    use cordial_core::outputs::{Plan, Preference};
    let (mut manager, mut store, mut radio) = setup();
    change(&mut manager, &mut store, &mut radio, 77, |policy| {
        policy.output_preferences.push(Preference {
            role: p::Role::Gamepad,
            mode: p::OutputMode::Hid,
        });
    })
    .unwrap();
    let key = record_key(2, 77);
    lose_bond(&mut store.records, key);
    store.fail_remove = Some(key);
    assert_eq!(
        block_on(manager.import(0, &mut store, &mut radio)),
        Err((Error::StorageFailed, p::ErrorReason::Unknown))
    );
    assert!(manager.find(77).is_none());
    assert_eq!(manager.outputs.plan(), Plan::default());
    assert_eq!(manager.lost_devices, [77]);
    // The policy still decodes even though its bond does not belong to it.
    assert!(block_on(Policies { store: &mut store }.load_record(77)).is_ok());
    for _ in 0..2 {
        assert!(!block_on(manager.fill_record(77, &mut store, &mut radio)).unwrap());
        assert!(!block_on(manager.fill(&mut store, &mut radio)).unwrap());
        assert!(manager.find(77).is_none());
        assert_eq!(manager.outputs.plan(), Plan::default());
        assert_eq!(manager.removed, [77]);
        assert_eq!(manager.lost_devices, [77]);
    }
    store.fail_remove = None;
    block_on(manager.lose(77, &mut store, &mut radio)).unwrap();
    assert!(manager.lost_devices.is_empty());
    assert!(!store.records.contains_key(&key));
    assert_eq!(manager.removed, [77]);
}

fn pad_descriptor() -> Vec<cordial_core::bluetooth::Descriptor> {
    use cordial_core::{bluetooth::Descriptor, hid::Map};
    // Eight buttons, one unsigned axis and a nullable hat.
    let bytes = [
        5, 1, 9, 5, 0xa1, 1, 5, 9, 0x19, 1, 0x29, 8, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8, 0x81, 2,
        5, 1, 9, 0x30, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 1, 0x81, 2, 9, 0x39, 0x15, 1, 0x25, 8,
        0x81, 0x42, 0xc0,
    ];
    vec![Descriptor {
        service: ServiceId(7),
        map: Map::compile(&bytes).unwrap(),
    }]
}

fn output_manager(modes: &[p::OutputMode]) -> (Manager, Store, Radio) {
    use cordial_core::outputs::{Preference, Support};
    let (_, mut store, mut radio) = setup();
    for (index, &mode) in modes.iter().enumerate() {
        add(
            &mut store,
            77 + index as u64,
            Peer {
                transport: if index < cordial_core::devices::CLASSIC_CONNECTIONS {
                    Transport::Classic
                } else {
                    Transport::Ble
                },
                ..peer(1 + index as u8)
            },
            |policy| {
                policy.roles.0 = cordial_core::outputs::GAMEPAD;
                if mode != p::OutputMode::Disabled {
                    policy.output_preferences.push(Preference {
                        role: p::Role::Gamepad,
                        mode,
                    });
                }
            },
        );
    }
    let mut manager = Manager::default();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    manager.radio_ready = true;
    manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid, p::OutputMode::Xinput],
    }];
    (manager, store, radio)
}

fn connect_pad(manager: &mut Manager, radio: &mut Radio, id: u64) -> cordial_core::link::LinkId {
    let slot = manager.find(id).unwrap();
    let link = manager
        .connect(slot, true, 30_000, None, radio)
        .unwrap()
        .unwrap();
    manager.connected(link, pad_descriptor(), 255, 0).unwrap();
    link
}

fn enumerate_outputs(manager: &mut Manager, hid: u8, xinput: u8, generation: u64, ready: bool) {
    use cordial_core::outputs::{Enumeration, Plan};
    manager.set_output_enumeration(Some(Enumeration {
        plan: Plan {
            hid_gamepads: hid,
            xinput_gamepads: xinput,
        },
        generation,
        ready,
    }));
}

#[test]
fn connected_slot_zero_leaves_further_disconnected_slots_reserved() {
    use cordial_core::outputs::Plan;
    use p::OutputMode::{Hid, Xinput};
    for (connected, other) in [(Hid, Xinput), (Xinput, Hid)] {
        let (mut manager, _, mut radio) = output_manager(&[connected, connected, other]);
        connect_pad(&mut manager, &mut radio, 77);
        manager.usb_budget.interfaces = 4;
        let allocation = manager.usb_allocation();
        assert_eq!(
            allocation.topology.gamepads,
            Plan {
                hid_gamepads: 1,
                xinput_gamepads: 1
            }
        );
        assert!(!allocation.required_exceeded);
        assert!(allocation.reserved_exceeded);

        manager.outputs.remove(79);
        manager.usb_budget.interfaces = 3;
        let allocation = manager.usb_allocation();
        assert_eq!(
            allocation.topology.gamepads,
            Plan {
                hid_gamepads: u8::from(connected == Hid),
                xinput_gamepads: u8::from(connected == Xinput),
            }
        );
        assert!(!allocation.required_exceeded);
        assert!(allocation.reserved_exceeded);
    }
}

#[test]
fn gamepad_pools_use_actual_enumeration_and_stable_independent_slots() {
    use p::OutputMode::{Hid, Xinput};
    let (mut manager, mut store, mut radio) = output_manager(&[Hid, Xinput, Hid, Xinput]);
    let desired = manager.outputs.plan();
    let a = connect_pad(&mut manager, &mut radio, 77);
    let b = connect_pad(&mut manager, &mut radio, 78);
    let c = connect_pad(&mut manager, &mut radio, 79);
    let d = connect_pad(&mut manager, &mut radio, 80);
    assert_eq!(manager.connections.iter().flatten().count(), 4);
    assert_eq!(manager.gamepad_outputs(), Default::default());
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Pending as i32
        ))
    );
    enumerate_outputs(&mut manager, 1, 2, 1, true);
    let snapshots = manager.gamepad_outputs();
    assert_eq!(snapshots.hid[0].occupant, Some(a));
    assert_eq!(snapshots.hid[1].occupant, None);
    assert_eq!(snapshots.xinput[0].occupant, Some(b));
    assert_eq!(snapshots.xinput[1].occupant, Some(d));
    assert_eq!(
        record(&manager, &mut store, 79).outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Pending as i32
        ))
    );
    assert_eq!(record(&manager, &mut store, 80).outputs[0].slot, Some(1));
    manager.close(a, None, &mut radio);
    let replacement = manager.gamepad_outputs();
    assert_eq!(replacement.hid[0].occupant, Some(c));
    assert!(replacement.hid[0].generation > snapshots.hid[0].generation);
    assert_eq!(replacement.xinput, snapshots.xinput);
    assert_eq!(manager.outputs.plan(), desired);
    manager.disconnected(a, None, 2).unwrap();
    let reconnected = connect_pad(&mut manager, &mut radio, 77);
    assert_ne!(reconnected, a);
    assert_eq!(manager.gamepad_outputs().hid[0], replacement.hid[0]);
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Pending as i32
        ))
    );
    enumerate_outputs(&mut manager, 2, 2, 2, true);
    let rebuilt = manager.gamepad_outputs();
    assert!(rebuilt.hid.iter().any(|s| s.occupant == Some(reconnected)));
    assert!(rebuilt.hid.iter().any(|s| s.occupant == Some(c)));
    assert!(rebuilt.hid[0].generation > replacement.hid[0].generation);
    assert_eq!(manager.outputs.plan(), desired);
}

#[test]
fn output_controls_refresh_on_input_and_survive_suspend_without_stale_occupants() {
    use cordial_core::hid::Gamepad;
    use p::OutputMode::Hid;
    let (mut manager, mut store, mut radio) = output_manager(&[Hid, Hid]);
    enumerate_outputs(&mut manager, 2, 0, 7, true);
    let a = connect_pad(&mut manager, &mut radio, 77);
    let b = connect_pad(&mut manager, &mut radio, 78);
    let neutral = manager.gamepad_outputs().hid[0];
    assert_eq!(
        neutral.gamepad,
        Gamepad {
            axes_declared: 1,
            ..Gamepad::default()
        }
    );
    manager
        .input(
            &InputReport::new(a, ServiceId(7), 0, &[5, 128, 3]).unwrap(),
            1,
        )
        .unwrap();
    let held = manager.gamepad_outputs().hid[0];
    assert_eq!(held.gamepad.buttons, 5);
    assert_eq!(held.gamepad.axes[0], 32896);
    assert_eq!(held.gamepad.axes_valid, 1);
    assert_eq!(held.gamepad.hat, None);
    assert_eq!(held.gamepad.dpad, 4);
    assert_eq!(held.generation, neutral.generation);
    enumerate_outputs(&mut manager, 2, 0, 7, false);
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Pending as i32
        ))
    );
    assert_eq!(record(&manager, &mut store, 77).outputs[0].slot, None);
    assert_eq!(manager.gamepad_outputs().hid[0], held);
    enumerate_outputs(&mut manager, 2, 0, 7, true);
    assert_eq!(manager.gamepad_outputs().hid[0], held);
    manager
        .input(
            &InputReport::new(a, ServiceId(7), 0, &[0, 0, 0]).unwrap(),
            2,
        )
        .unwrap();
    let released = manager.gamepad_outputs().hid[0].gamepad;
    assert_eq!(released.buttons, 0);
    assert_eq!(released.hat, None);
    assert!(released.hat_valid);
    manager.close(a, None, &mut radio);
    let empty = manager.gamepad_outputs();
    assert_eq!(empty.hid[0].occupant, None);
    assert_eq!(empty.hid[0].gamepad, Gamepad::default());
    assert_eq!(empty.hid[1].occupant, Some(b));
    manager.disconnected(a, None, 3).unwrap();
    let new = connect_pad(&mut manager, &mut radio, 77);
    let replaced = manager.gamepad_outputs().hid[0];
    assert_eq!(replaced.occupant, Some(new));
    assert_ne!(
        (held.occupant, held.generation),
        (replaced.occupant, replaced.generation)
    );
    assert_eq!(
        replaced.gamepad,
        Gamepad {
            axes_declared: 1,
            ..Gamepad::default()
        }
    );
    assert!(
        !manager
            .input(
                &InputReport::new(a, ServiceId(7), 0, &[255, 255, 8]).unwrap(),
                4
            )
            .unwrap()
    );
    assert_eq!(manager.gamepad_outputs().hid[0], replaced);
}

#[test]
fn saturated_mode_changes_reassign_without_changing_the_usb_plan() {
    use cordial_core::outputs::{Plan, Preference};
    use p::OutputMode::{Disabled, Hid, Xinput};
    let (mut manager, mut store, mut radio) =
        output_manager(&[Hid, Hid, Hid, Hid, Hid, Xinput, Xinput, Xinput, Xinput]);
    let a = connect_pad(&mut manager, &mut radio, 77);
    enumerate_outputs(&mut manager, 4, 4, 1, true);
    let original = manager.gamepad_outputs().hid[0];
    let desired = Plan {
        hid_gamepads: 4,
        xinput_gamepads: 4,
    };
    assert_eq!(manager.outputs.plan(), desired);
    store.fail_save = Some((record_key(2, 77), false));
    assert_eq!(
        change(&mut manager, &mut store, &mut radio, 77, |p| p
            .output_preferences[0]
            .mode =
            Xinput),
        Err(cordial_core::wire::error(Error::StorageFailed, None, false))
    );
    assert_eq!(manager.outputs.plan(), desired);
    assert_eq!(manager.gamepad_outputs().hid[0], original);
    store.fail_save = None;
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.output_preferences[0].mode = Xinput
    })
    .unwrap();
    assert_eq!(manager.outputs.plan(), desired);
    let switched = manager.gamepad_outputs();
    assert_eq!(switched.hid[0].occupant, None);
    assert_eq!(switched.xinput[0].occupant, Some(a));
    assert!(switched.xinput[0].generation > original.generation);
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].mode,
        Xinput as i32
    );
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.output_preferences = vec![Preference {
            role: p::Role::Gamepad,
            mode: Disabled,
        }]
    })
    .unwrap();
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Disabled as i32
        ))
    );
    assert_eq!(manager.gamepad_outputs().xinput[0].occupant, None);
}

#[test]
fn gamepad_assignments_release_for_layout_storage_policy_and_backend_failures() {
    use p::OutputMode::Hid;
    let (mut manager, mut store, mut radio) = output_manager(&[Hid]);
    enumerate_outputs(&mut manager, 1, 0, 1, true);
    let a = connect_pad(&mut manager, &mut radio, 77);
    let original = manager.gamepad_outputs().hid[0];
    manager.relayout(a, descriptor()).unwrap();
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].status,
        Some(p::output_status::Status::Error(p::Error {
            code: p::ErrorCode::Unsupported as i32,
            reason: p::ErrorReason::InputLayout as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }))
    );
    manager.relayout(a, pad_descriptor()).unwrap();
    assert!(manager.gamepad_outputs().hid[0].generation > original.generation);
    manager.fail_storage();
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Pending as i32
        ))
    );
    manager.storage_ready = true;
    let resumed = manager.gamepad_outputs().hid[0];
    manager.output_support = &[];
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].status,
        Some(p::output_status::Status::Error(p::Error {
            code: p::ErrorCode::Unsupported as i32,
            reason: p::ErrorReason::OutputBackend as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }))
    );
    manager.output_support = &[cordial_core::outputs::Support {
        role: p::Role::Gamepad,
        modes: &[Hid],
    }];
    assert!(manager.gamepad_outputs().hid[0].generation > resumed.generation);
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.blocked = true
    })
    .unwrap();
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
    assert_eq!(
        record(&manager, &mut store, 77).outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Disconnected as i32
        ))
    );
    manager.disconnected(a, None, 3).unwrap();
    block_on(manager.unpair(77, peer(1), &mut store, &mut radio)).unwrap();
    assert_eq!(manager.outputs.plan(), Default::default());
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
}

#[test]
fn failed_relayout_releases_gamepad_controls_before_the_caller_closes() {
    use cordial_core::hid::Gamepad;
    let (mut manager, _, mut radio) = output_manager(&[p::OutputMode::Hid]);
    enumerate_outputs(&mut manager, 1, 0, 1, true);
    let a = connect_pad(&mut manager, &mut radio, 77);
    manager
        .input(
            &InputReport::new(a, ServiceId(7), 0, &[1, 255, 8]).unwrap(),
            1,
        )
        .unwrap();
    assert!(manager.relayout(a, Vec::new()).is_err());
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
    assert_eq!(manager.gamepad_outputs().hid[0].gamepad, Gamepad::default());
}

#[test]
fn disabled_gamepad_output_preserves_keyboard_forwarding_and_existing_wire_fields() {
    use p::OutputMode::{Disabled, Hid};
    let (mut manager, mut store, mut radio) = output_manager(&[Disabled]);
    enumerate_outputs(&mut manager, 1, 0, 1, true);
    let slot = manager.find(77).unwrap();
    let link = manager
        .connect(slot, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    let mut descriptors = descriptor();
    let mut pads = pad_descriptor();
    pads[0].service = ServiceId(8);
    descriptors.extend(pads);
    manager.connected(link, descriptors, 255, 0).unwrap();
    while manager.forward.packet().is_some() {
        manager.forward.complete();
    }
    manager
        .input(&InputReport::new(link, ServiceId(7), 0, &[1]).unwrap(), 1)
        .unwrap();
    assert_eq!(manager.forward.packet().unwrap().bytes()[0], 16);
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
    let before = record(&manager, &mut store, 77);
    assert_eq!(
        before.outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Disabled as i32
        ))
    );
    change(&mut manager, &mut store, &mut radio, 77, |policy| {
        policy
            .output_preferences
            .push(cordial_core::outputs::Preference {
                role: p::Role::Gamepad,
                mode: Hid,
            });
    })
    .unwrap();
    let mut after = record(&manager, &mut store, 77);
    assert_eq!(
        after.outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Active as i32
        ))
    );
    after.outputs = before.outputs.clone();
    after.output_preferences = before.output_preferences.clone();
    assert_eq!(after, before);
    assert_eq!(manager.forward.packet().unwrap().bytes()[0], 16);
}

#[test]
fn losing_a_connected_gamepad_record_releases_its_slot_and_pending_notifications() {
    let (mut manager, mut store, mut radio) = output_manager(&[p::OutputMode::Hid]);
    enumerate_outputs(&mut manager, 1, 0, 1, true);
    connect_pad(&mut manager, &mut radio, 77);
    assert!(manager.gamepad_outputs().hid[0].occupant.is_some());
    assert!(manager.changed.contains(&77));
    block_on(manager.lose(77, &mut store, &mut radio)).unwrap();
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
    assert_eq!(manager.outputs.plan(), Default::default());
    assert_eq!(manager.removed, [77]);
    assert!(!manager.changed.contains(&77));
}

#[test]
fn transport_allowances_are_independent_and_pairing_has_one_reserved_slot() {
    use cordial_core::devices::{ACTIVE_CONNECTIONS, connection_limit};
    for pairing_transport in [Transport::Classic, Transport::Ble] {
        let (mut manager, mut store, mut radio) = setup();
        block_on(Policies { store: &mut store }.remove(77)).unwrap();
        for transport in [Transport::Classic, Transport::Ble] {
            for n in 1..=connection_limit(transport) + 1 {
                let id = n as u64 + if transport == Transport::Ble { 100 } else { 0 };
                add(
                    &mut store,
                    id,
                    Peer {
                        transport,
                        ..peer(n as u8)
                    },
                    |_| {},
                );
            }
        }
        block_on(manager.load(&mut store, &mut radio)).unwrap();
        manager.radio_ready = true;
        for transport in [Transport::Classic, Transport::Ble] {
            assert_eq!(manager.max_enabled(transport), connection_limit(transport));
            let ids: Vec<_> = manager
                .devices
                .iter()
                .flatten()
                .filter(|d| d.peer.transport == transport)
                .map(|d| d.id)
                .collect();
            assert_eq!(ids.len(), connection_limit(transport));
            for id in ids {
                let slot = manager.find(id).unwrap();
                if transport == Transport::Ble {
                    assert!(manager.ble_admission());
                    let p = manager.devices[slot].as_ref().unwrap().peer;
                    assert_eq!(
                        manager.incoming(id as u32, p, 0, None, &mut radio),
                        Ok(Some(slot))
                    );
                } else {
                    manager
                        .connect(slot, false, 30_000, None, &mut radio)
                        .unwrap();
                }
                let link = manager.link_for(slot).unwrap();
                manager.connected(link, descriptor(), 255, 0).unwrap();
            }
            assert_eq!(
                manager.connection_count(transport),
                connection_limit(transport)
            );
            assert!(!manager.connection_room(transport));
        }
        assert_eq!(
            manager.connections.iter().flatten().count(),
            ACTIVE_CONNECTIONS - 1
        );
        assert!(!manager.ble_admission());
        let pairing_peer = Peer {
            transport: pairing_transport,
            ..peer(200)
        };
        block_on(manager.prepare_pair(pairing_peer, None, &mut store, &mut radio)).unwrap();
        let pairing = manager.pair(pairing_peer, 90_000, &mut radio).unwrap();
        assert_eq!(
            manager.connections.iter().flatten().count(),
            ACTIVE_CONNECTIONS
        );
        assert_eq!(
            manager.connection_count(pairing_transport),
            connection_limit(pairing_transport)
        );
        assert_eq!(
            manager.pair(
                Peer {
                    transport: Transport::Ble,
                    ..peer(201)
                },
                90_000,
                &mut radio
            ),
            Err(Error::Busy)
        );
        let (saved, slot, _) = block_on(manager.bonded(
            pairing,
            pairing_peer,
            None,
            b"Extra",
            &mut store,
            &mut radio,
        ))
        .unwrap();
        assert_eq!(slot, None);
        assert!(!policy(&mut store, saved).enabled);
        assert!(manager.connection(pairing).unwrap().pairing);
        manager.close(pairing, None, &mut radio);
        assert_eq!(
            manager.pair(pairing_peer, 90_000, &mut radio),
            Err(Error::Busy)
        );
        manager.disconnected(pairing, None, 1).unwrap();

        // Retiring a saved device frees an enabled entry, but its closing link still
        // consumes a normal slot. Neither explicit nor incoming reconnection may use the reserve.
        for transport in [Transport::Classic, Transport::Ble] {
            let base = if transport == Transport::Ble { 100 } else { 0 };
            let slot = manager.find(base + 1).unwrap();
            let old = manager.link_for(slot).unwrap();
            block_on(manager.lose(base + 1, &mut store, &mut radio)).unwrap();
            assert!(manager.connection(old).unwrap().closing);
            assert_eq!(
                manager.connection_count(transport),
                connection_limit(transport)
            );
            block_on(manager.fill(&mut store, &mut radio)).unwrap();
            let waiting = base + connection_limit(transport) as u64 + 1;
            let slot = manager.find(waiting).unwrap();
            let p = manager.devices[slot].as_ref().unwrap().peer;
            assert_eq!(
                manager.connect(slot, false, 30_000, None, &mut radio),
                Err(Error::Capacity)
            );
            assert_eq!(manager.incoming(500, p, 0, None, &mut radio), Ok(None));
            manager.disconnected(old, None, 1).unwrap();
            radio.reject_connect = Some(Error::Busy);
            assert_eq!(
                manager.connect(slot, false, 30_000, None, &mut radio),
                Err(Error::Busy)
            );
            assert_eq!(
                manager.connection_count(transport),
                connection_limit(transport) - 1
            );
            radio.reject_connect = None;
            let new = manager
                .connect(slot, false, 30_000, None, &mut radio)
                .unwrap()
                .unwrap();
            assert_ne!(new.generation, old.generation);
            assert!(manager.disconnected(old, None, 2).is_none());
            manager.connected(new, descriptor(), 255, 2).unwrap();
        }
    }
}

#[test]
fn unsupported_classic_has_no_enabled_or_connection_allowance() {
    let (mut manager, mut store, mut radio) = setup();
    radio.transports = classic_unsupported();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert_eq!(manager.max_enabled(Transport::Classic), 0);
    assert_eq!(manager.connection_count(Transport::Classic), 0);
    assert!(!manager.connection_room(Transport::Classic));
    assert_eq!(
        manager.max_enabled(Transport::Ble),
        cordial_core::devices::BLE_CONNECTIONS
    );
}

#[test]
fn saved_disabled_pair_keeps_reserve_until_disconnect_and_normal_admission_still_fits() {
    use cordial_core::devices::connection_limit;
    for transport in [Transport::Classic, Transport::Ble] {
        for incoming in [false, true] {
            // BLE incoming is intentionally suspended during teardown.
            if incoming && transport == Transport::Ble {
                continue;
            }
            let (mut manager, mut store, mut radio) = setup();
            block_on(Policies { store: &mut store }.remove(77)).unwrap();
            let limit = connection_limit(transport);
            for n in 1..=limit {
                add(
                    &mut store,
                    n as u64,
                    Peer {
                        transport,
                        ..peer(n as u8)
                    },
                    |_| {},
                );
            }
            block_on(manager.load(&mut store, &mut radio)).unwrap();
            manager.radio_ready = true;
            radio.native_links = Some(Vec::new());
            for n in 1..limit {
                let slot = manager.find(n as u64).unwrap();
                let link = manager
                    .connect(slot, true, 30_000, None, &mut radio)
                    .unwrap()
                    .unwrap();
                manager.connected(link, descriptor(), 255, 0).unwrap();
            }
            let candidate = Peer {
                transport,
                ..peer(200)
            };
            block_on(manager.prepare_pair(candidate, None, &mut store, &mut radio)).unwrap();
            let pairing = manager.pair(candidate, 90_000, &mut radio).unwrap();
            let (saved, slot, _) = block_on(
                manager.bonded(pairing, candidate, None, b"Extra", &mut store, &mut radio),
            )
            .unwrap();
            assert_eq!(slot, None);
            assert!(!policy(&mut store, saved).enabled);
            assert_eq!(radio.adopted, [pairing]);
            assert!(radio.promoted.is_empty());
            manager.close(pairing, None, &mut radio);
            assert!(manager.connection(pairing).unwrap().closing);
            assert!(manager.connection(pairing).unwrap().pairing);
            assert!(radio.native_room(transport));
            assert!(manager.connection_room(transport));
            let slot = manager.find(limit as u64).unwrap();
            let waiting = manager.devices[slot].as_ref().unwrap().peer;
            if incoming {
                radio.incoming_peer = Some(waiting);
                assert_eq!(
                    manager.incoming(1, waiting, 1, None, &mut radio),
                    Ok(Some(slot))
                );
            } else {
                manager
                    .connect(slot, true, 30_000, None, &mut radio)
                    .unwrap()
                    .unwrap();
            }
            let normal = manager.link_for(slot).unwrap();
            manager.connected(normal, descriptor(), 255, 1).unwrap();
            assert!(!radio.native_room(transport));
            assert!(!manager.connection_room(transport));

            // Retirement frees an enabled entry; the closing normal link still owns capacity.
            block_on(manager.lose(limit as u64, &mut store, &mut radio)).unwrap();
            let extra = Peer {
                transport,
                ..peer(201)
            };
            add(&mut store, 900, extra, |_| {});
            block_on(manager.fill(&mut store, &mut radio)).unwrap();
            let next = manager.find(900).unwrap();
            assert_eq!(
                manager.connect(next, true, 30_000, None, &mut radio),
                Err(Error::Capacity)
            );
            assert_eq!(manager.incoming(2, extra, 1, None, &mut radio), Ok(None));
            assert!(!radio.native_room(transport));
            radio
                .native_links
                .as_mut()
                .unwrap()
                .retain(|(id, _, _)| *id != pairing);
            manager.disconnected(pairing, None, 2);
            assert!(!radio.native_room(transport));
            radio
                .native_links
                .as_mut()
                .unwrap()
                .retain(|(id, _, _)| *id != normal);
            manager.disconnected(normal, None, 2);
            assert!(radio.native_room(transport));
            manager
                .connect(next, true, 30_000, None, &mut radio)
                .unwrap()
                .unwrap();
        }
    }
}

#[test]
fn resident_promotion_follows_commit_and_failed_commit_keeps_reserve() {
    for transport in [Transport::Classic, Transport::Ble] {
        for commit in [false, true] {
            let (mut manager, mut store, mut radio) = setup();
            radio.native_links = Some(Vec::new());
            let candidate = Peer {
                transport,
                ..peer(200)
            };
            block_on(manager.prepare_pair(candidate, None, &mut store, &mut radio)).unwrap();
            let pairing = manager.pair(candidate, 90_000, &mut radio).unwrap();
            if !commit {
                store.fail_save = Some((record_key(2, last_device(&store)), false));
            }
            let result =
                block_on(manager.bonded(pairing, candidate, None, b"New", &mut store, &mut radio));
            if commit {
                let (_, slot, _) = result.unwrap();
                assert!(slot.is_some());
                assert_eq!(radio.promoted, [pairing]);
            } else {
                assert_eq!(
                    result,
                    Err(p::Error {
                        code: p::ErrorCode::StorageFailed as i32,
                        reason: p::ErrorReason::Storage as i32,
                        outcome: p::ErrorOutcome::NoEffect as i32,
                        ..Default::default()
                    })
                );
                assert!(radio.promoted.is_empty());
            }
            assert_eq!(radio.adopted, [pairing]);
            assert_eq!(radio.native_links.as_ref().unwrap()[0].2, !commit);
            assert_eq!(manager.connection(pairing).unwrap().pairing, !commit);
            manager.close(pairing, None, &mut radio);
            assert_eq!(manager.connection_count(transport), usize::from(commit));
            assert_eq!(radio.native_links.as_ref().unwrap()[0].2, !commit);
            radio
                .native_links
                .as_mut()
                .unwrap()
                .retain(|(id, _, _)| *id != pairing);
            manager.disconnected(pairing, None, 1);
            assert!(radio.native_links.as_ref().unwrap().is_empty());
        }
    }
}

#[test]
fn unpair_keeps_forget_and_restoration_failures_with_their_radio_context() {
    let (mut manager, mut store, mut radio) = setup();
    radio.reject_forget = true;
    radio.reject_inventory = true;
    let error = block_on(manager.unpair(77, peer(1), &mut store, &mut radio)).unwrap_err();
    assert_eq!(
        error,
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            reason: p::ErrorReason::Radio as i32,
            related: vec![p::Error {
                code: p::ErrorCode::StorageFailed as i32,
                reason: p::ErrorReason::Radio as i32,
                ..Default::default()
            }],
            ..Default::default()
        }
    );
    assert!(manager.storage_ready);
    assert!(manager.storage_error.is_none());
    assert!(manager.bonds_pending);
    assert!(!manager.devices[0].as_ref().unwrap().deleting);
    assert!(policy(&mut store, 77).enabled);
    radio.reject_forget = false;
    radio.reject_inventory = false;
    block_on(manager.sync_bonds(&mut store, &mut radio)).unwrap();
    assert!(!manager.bonds_pending);
    assert!(radio.bonds.contains(&peer(1)));
}

#[test]
fn unpair_preserves_storage_context_when_restoration_cannot_read_a_bond() {
    let (mut manager, mut store, mut radio) = setup();
    store.fail_remove = Some(record_key(2, 77));
    store.fail_load = Some(record_key(2, 77));
    let error = block_on(manager.unpair(77, peer(1), &mut store, &mut radio)).unwrap_err();
    assert_eq!(error.code(), p::ErrorCode::StorageFailed);
    assert_eq!(error.reason(), p::ErrorReason::Storage);
    assert_eq!(error.outcome(), p::ErrorOutcome::Unspecified);
    assert_eq!(
        error.related,
        [p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            reason: p::ErrorReason::Storage as i32,
            ..Default::default()
        }]
    );
    assert!(manager.bonds_pending);
    store.fail_load = None;
    block_on(manager.sync_bonds(&mut store, &mut radio)).unwrap();
    assert!(radio.bonds.contains(&peer(1)));
    assert!(!manager.bonds_pending);
}
