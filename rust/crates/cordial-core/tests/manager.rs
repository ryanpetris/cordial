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
    storage::{RecordStore, record_key},
};
use cordial_protocol as p;
use embassy_futures::block_on;
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
) -> Result<(), Error> {
    let mut policy = policy(store, id);
    f(&mut policy);
    block_on(manager.save_policy(policy, store, radio))
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
        Err(Error::StorageFailed)
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
        Err(Error::Busy)
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
    let (id, slot) = block_on(manager.bonded(
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
    let mut value: serde_json::Value = serde_json::from_slice(&store.records[&key]).unwrap();
    value["bond"]["complete"] = false.into();
    store
        .records
        .insert(key, serde_json::to_vec(&value).unwrap());
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert!(manager.devices.iter().all(Option::is_none));
    assert!(!store.records.contains_key(&key));
}
#[test]
fn a_lost_record_whose_cleanup_fails_is_still_reported_removed() {
    let (mut manager, mut store, mut radio) = setup();
    let key = record_key(2, 77);
    let mut value: serde_json::Value = serde_json::from_slice(&store.records[&key]).unwrap();
    value["bond"]["complete"] = false.into();
    store
        .records
        .insert(key, serde_json::to_vec(&value).unwrap());
    store.fail_remove = Some(key);
    assert_eq!(
        block_on(manager.sync_bonds(&mut store, &mut radio)),
        Err(Error::StorageFailed)
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
        let (id, slot) = block_on(manager.bonded(
            link,
            peer(n),
            saved_peer(&mut store, peer(n)),
            b"extra",
            &mut store,
            &mut radio,
        ))
        .unwrap();
        // The stack holds seven devices; later ones are saved disabled and are not resident.
        assert_eq!(policy(&mut store, id).enabled, n <= 7);
        assert_eq!(slot.is_some(), n <= 7);
        manager.disconnected(link, None, 0);
        block_on(manager.finish_pair(&mut store)).unwrap();
        block_on(manager.sync_bonds(&mut store, &mut radio)).unwrap();
        ids.push(id);
    }
    assert_eq!(manager.devices.iter().flatten().count(), 7);
    assert_eq!(radio.bonds.len(), 7);
    let last = *ids.last().unwrap();
    assert_eq!(
        change(&mut manager, &mut store, &mut radio, last, |p| p.enabled =
            true),
        Err(Error::Capacity)
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
    let (id, slot) = block_on(manager.bonded(
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
    assert_eq!(resident(&manager), (102..=108).collect::<Vec<_>>());
    let slot = manager.find(108).unwrap();
    let link = manager
        .connect(slot, true, 90_000, None, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(link, descriptor(), 255, 0).unwrap();
    assert_eq!(
        change(&mut manager, &mut store, &mut radio, 77, |p| p.blocked =
            false),
        Err(Error::Capacity)
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
        Err(Error::StorageFailed)
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
    let first = serde_json::from_slice::<cordial_core::storage::Sequence>(
        &store.records[&record_key(7, 0)],
    )
    .unwrap()
    .device;
    let link = manager.pair(peer(2), 90_000, &mut radio).unwrap();
    let (id, _) = block_on(manager.bonded(
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
    let next = serde_json::from_slice::<cordial_core::storage::Sequence>(
        &store.records[&record_key(7, 0)],
    )
    .unwrap()
    .device;
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
        Err(Error::Busy)
    );
    assert_eq!(manager.link_for(0), Some(old));
    assert_eq!(policy(&mut store, 77), original);
}
#[test]
fn device_and_bond_are_one_json_record() {
    let (_, store, _) = setup();
    assert!(store.records.keys().all(|key| !matches!(key[0], 5 | 6)));
    let value: serde_json::Value =
        serde_json::from_slice(&store.records[&record_key(2, 77)]).unwrap();
    assert!(value["bond"]["keys"]["Classic"]["key"].is_string());
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
    // Ten more saved devices: three disabled, one blocked and six enabled.
    for n in 2..=11u8 {
        add(&mut store, u64::from(n), peer(n), |p| {
            p.enabled = !(2..=4).contains(&n);
            p.blocked = n == 5;
        });
    }
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    // Seven fit: the lowest enabled IDs. The rest stay on flash.
    assert_eq!(resident(&manager), [6, 7, 8, 9, 10, 11, 77]);
    assert_eq!(radio.bonds.len(), 7);
    for (id, reason) in [
        (2, p::InactiveReason::Disabled),
        (5, p::InactiveReason::Blocked),
    ] {
        assert_eq!(
            record(&manager, &mut store, id).inactive,
            Some(reason as i32)
        );
    }
    assert_eq!(record(&manager, &mut store, 77).inactive, None);
    // Disabling a device without a link leaves at once and frees room for another.
    change(&mut manager, &mut store, &mut radio, 77, |p| {
        p.enabled = false
    })
    .unwrap();
    assert!(manager.find(77).is_none());
    assert!(!radio.bonds.contains(&peer(1)));
    assert!(manager.vacated);
    assert_eq!(
        record(&manager, &mut store, 77).inactive,
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
        Err(Error::Capacity)
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
    // Nine enabled devices, room for seven: the two highest IDs report capacity.
    assert_eq!(resident(&manager), [2, 3, 4, 5, 6, 7, 8]);
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
    assert_eq!(resident(&manager), [2, 4, 5, 6, 7, 8, 9]);
    assert_eq!(manager.changed, [9]);
    assert!(radio.bonds.contains(&peer(9)));
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
