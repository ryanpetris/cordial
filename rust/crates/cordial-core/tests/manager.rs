mod support;
use cordial_core::model::{
    errors::ErrorCode as Error,
    identifiers::{ConnectionState, HostPlatform},
};
use cordial_core::{
    bluetooth::InputReport, devices::AdapterPreference, link::ServiceId, manager::Manager,
};
use cordial_protocol as p;
use embassy_futures::block_on;
use support::*;

fn record(manager: &Manager, slot: usize) -> Option<p::Device> {
    cordial_core::wire::device(manager, slot)
}

#[test]
fn disconnect_releases_input_and_generation_blocks_late_events() {
    let (mut manager, _, mut radio) = setup();
    while manager.forward.packet().is_some() {
        manager.forward.complete();
    }
    let old = manager
        .connect(0, true, 30_000, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(old, descriptor(), 255, 0).unwrap();
    let input = InputReport::new(old, ServiceId(7), 0, &[1]).unwrap();
    manager.input(&input, 1).unwrap();
    assert_eq!(manager.forward.packet().unwrap().bytes()[0], 16);
    manager.forward.complete();
    manager.disconnect(0, &mut radio).unwrap();
    assert_eq!(radio.closes, vec![old]);
    manager.disconnected(old, None, 2).unwrap();
    assert_eq!(manager.forward.packet().unwrap().bytes(), &[0; 32]);
    manager.forward.complete();
    assert!(!manager.devices[0].as_ref().unwrap().reconnect_due(60_000));
    let new = manager
        .connect(0, true, 30_000, &mut radio)
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
    let mut changed = manager.devices[0].as_ref().unwrap().policy.clone();
    changed.hidpp_enabled = false;
    store.fail_save = Some((cordial_core::storage::record_key(2, 77), false));
    assert_eq!(
        block_on(manager.policy(0, changed.clone(), &mut store)),
        Err(Error::StorageFailed)
    );
    assert!(manager.devices[0].as_ref().unwrap().policy.hidpp_enabled);
    store.fail_save = None;
    block_on(manager.policy(0, changed, &mut store)).unwrap();
    assert!(!manager.devices[0].as_ref().unwrap().policy.hidpp_enabled);
    block_on(manager.platform(HostPlatform::Mac, &mut store)).unwrap();
    let id = manager
        .connect(0, true, 30_000, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(id, descriptor(), 255, 0).unwrap();
    assert_eq!(
        block_on(manager.unpair(0, &mut store, &mut radio)),
        Err(Error::Busy)
    );
    manager.disconnect(0, &mut radio).unwrap();
    manager.disconnected(id, None, 1).unwrap();
    block_on(manager.unpair(0, &mut store, &mut radio)).unwrap();
    assert!(radio.bonds.is_empty());
    assert!(manager.devices[0].is_none());
    let mut reloaded = Manager::default();
    block_on(reloaded.load(&mut store, &mut radio)).unwrap();
    assert_eq!(
        reloaded.preference,
        AdapterPreference {
            name: None,
            host_platform: HostPlatform::Mac
        }
    );
    assert!(reloaded.devices.iter().all(Option::is_none));
}
#[test]
fn security_is_observed_per_connection_and_never_carried_across_generations() {
    use cordial_core::bluetooth::ConnectionSecurity;
    let (mut manager, _, mut radio) = setup();
    let id = manager
        .connect(0, true, 30_000, &mut radio)
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
    assert_eq!(record(&manager, 0).unwrap().security, None);
    manager.connected(id, descriptor(), 255, 0).unwrap();
    assert_eq!(record(&manager, 0).unwrap().security, Some(wire));
    assert_eq!(manager.security(id, initial), None); // No redundant notification.
    let changed = ConnectionSecurity {
        authenticated: Some(true),
        ..initial
    };
    assert_eq!(manager.security(id, changed), Some(0));
    manager.disconnect(0, &mut radio).unwrap();
    assert_eq!(record(&manager, 0).unwrap().security, None);
    assert_eq!(manager.security(id, initial), None);
    manager.disconnected(id, None, 1).unwrap();
    let next = manager
        .connect(0, true, 30_000, &mut radio)
        .unwrap()
        .unwrap();
    assert_ne!(id, next);
    assert_eq!(manager.security(id, changed), None);
    manager.connected(next, descriptor(), 255, 2).unwrap();
    assert_eq!(record(&manager, 0).unwrap().security, None);
    assert_eq!(
        manager.security(next, ConnectionSecurity::default()),
        Some(0)
    );
    assert_eq!(
        record(&manager, 0).unwrap().security.unwrap().authenticated,
        None
    );
}

#[test]
fn replacement_commit_preserves_policy_and_reboot_selects_new_bond() {
    let (mut manager, mut store, mut radio) = setup();
    let original = manager.devices[0].as_ref().unwrap().policy.clone();
    block_on(manager.prepare_pair(peer(1), Some(peer(1)), &mut store, &mut radio)).unwrap();
    assert_eq!(manager.devices[0].as_ref().unwrap().policy, original);
    assert!(radio.bonds.is_empty());
    let link = manager.pair(peer(1), 90_000, &mut radio).unwrap();
    radio.bonds.push(peer(1));
    let slot =
        block_on(manager.bonded(link, peer(1), b"new name", &mut store, &mut radio)).unwrap();
    let updated = manager.devices[slot].as_ref().unwrap().policy.clone();
    assert_eq!(updated.id, original.id);
    assert_eq!(updated.name, original.name);
    assert_eq!(updated.bond, original.bond);
    let mut reboot = Manager::default();
    block_on(reboot.load(&mut store, &mut radio)).unwrap();
    assert_eq!(reboot.devices[0].as_ref().unwrap().policy, updated);
    assert_eq!(radio.bonds, vec![peer(1)]);
}
#[test]
fn interrupted_pairing_leaves_the_committed_device_unchanged() {
    let (mut manager, mut store, mut radio) = setup();
    let original = store.records[&cordial_core::storage::record_key(2, 77)].clone();
    block_on(manager.prepare_pair(peer(1), Some(peer(1)), &mut store, &mut radio)).unwrap();
    let mut reboot = Manager::default();
    block_on(reboot.load(&mut store, &mut radio)).unwrap();
    assert_eq!(
        store.records[&cordial_core::storage::record_key(2, 77)],
        original
    );
    assert_eq!(radio.bonds, vec![peer(1)]);
}
#[test]
fn unsupported_transport_keeps_the_device_and_a_lost_bond_deletes_it() {
    use cordial_core::model::identifiers::Transport;
    let (mut manager, mut store, mut radio) = setup();
    radio.transports = Some(cordial_core::bluetooth::Capabilities {
        classic: false,
        ble: true,
        ble_scan_and_connect: false,
    });
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    let d = record(&manager, 0).unwrap();
    assert_eq!(
        d.inactive,
        Some(p::InactiveReason::UnsupportedTransport as i32)
    );
    assert!(d.enabled);
    assert_eq!(
        manager.devices[0].as_ref().unwrap().policy.peer.transport,
        Transport::Classic
    );
    let key = cordial_core::storage::record_key(2, 77);
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
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    let id = manager.devices[0].as_ref().unwrap().policy.device_id();
    let key = cordial_core::storage::record_key(2, 77);
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
    assert_eq!(manager.removed, [id]);
}
#[test]
fn additional_pairs_are_saved_disabled_and_enable_does_not_evict() {
    let (mut manager, mut store, mut radio) = setup();
    for n in 2..=9 {
        block_on(manager.prepare_pair(peer(n), None, &mut store, &mut radio)).unwrap();
        let link = manager.pair(peer(n), 90_000, &mut radio).unwrap();
        radio.bonds.push(peer(n));
        let slot =
            block_on(manager.bonded(link, peer(n), b"extra", &mut store, &mut radio)).unwrap();
        assert_eq!(
            manager.devices[slot].as_ref().unwrap().policy.enabled,
            n <= 7
        );
        manager.disconnected(link, None, 0);
        block_on(manager.finish_pair(&mut store, &mut radio)).unwrap();
    }
    assert_eq!(manager.devices.iter().flatten().count(), 9);
    assert_eq!(radio.bonds.len(), 7);
    let mut policy = manager.devices[8].as_ref().unwrap().policy.clone();
    policy.enabled = true;
    assert_eq!(
        block_on(manager.policy(8, policy, &mut store)),
        Err(Error::Capacity)
    );
    assert!(!manager.devices[8].as_ref().unwrap().policy.enabled);
}

#[test]
fn ambiguous_device_commit_is_resolved_from_storage() {
    use cordial_core::storage::record_key;
    for committed in [false, true] {
        let (mut manager, mut store, mut radio) = setup();
        let original = manager.devices[0].as_ref().unwrap().policy.clone();
        block_on(manager.prepare_pair(peer(1), Some(peer(1)), &mut store, &mut radio)).unwrap();
        let link = manager.pair(peer(1), 90_000, &mut radio).unwrap();
        store.fail_save = Some((record_key(2, 77), committed));
        let result = block_on(manager.bonded(link, peer(1), b"new", &mut store, &mut radio));
        assert_eq!(result.is_ok(), committed);
        manager.disconnected(link, None, 0);
        block_on(manager.finish_pair(&mut store, &mut radio)).unwrap();
        assert_eq!(
            manager.devices[0].as_ref().unwrap().policy.bond,
            original.bond
        );
    }
}
#[test]
fn malformed_device_record_is_deleted() {
    let (mut manager, mut store, mut radio) = setup();
    let key = cordial_core::storage::record_key(2, 77);
    store.records.insert(key, vec![0xff]);
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert!(manager.devices.iter().all(Option::is_none));
    assert!(!store.records.contains_key(&key));
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
    use cordial_core::model::identifiers::Transport;
    let (mut manager, mut store, mut radio) = setup();
    let mut policy = manager.devices[0].as_ref().unwrap().policy.clone();
    policy.peer.transport = Transport::Ble;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &policy,
        &bond(policy.id, policy.peer),
    ))
    .unwrap();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    let final_peer = cordial_core::devices::Peer {
        transport: Transport::Ble,
        ..peer(9)
    };
    block_on(manager.prepare_pair(policy.peer, Some(policy.peer), &mut store, &mut radio)).unwrap();
    let link = manager.pair(policy.peer, 90_000, &mut radio).unwrap();
    let slot =
        block_on(manager.bonded(link, final_peer, b"different", &mut store, &mut radio)).unwrap();
    assert_ne!(slot, 0);
    assert_eq!(manager.devices[0].as_ref().unwrap().policy, policy);
    assert!(radio.bonds.contains(&policy.peer));
}

#[test]
fn unblock_at_capacity_preserves_the_working_selection() {
    use cordial_core::devices::Policy;
    let (mut manager, mut store, mut radio) = setup();
    let mut blocked = manager.devices[0].as_ref().unwrap().policy.clone();
    blocked.blocked = true;
    block_on(manager.policy(0, blocked.clone(), &mut store)).unwrap();
    for n in 2..=8 {
        let mut policy = Policy::paired(100 + u64::from(n), peer(n), b"selected");
        policy.bond = policy.id;
        block_on(cordial_core::bonds::commit(
            &mut store,
            &policy,
            &bond(policy.id, policy.peer),
        ))
        .unwrap();
    }
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    manager.radio_ready = true;
    let link = manager
        .connect(7, true, 90_000, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(link, descriptor(), 255, 0).unwrap();
    blocked.blocked = false;
    assert_eq!(
        block_on(manager.policy(0, blocked, &mut store)),
        Err(Error::Capacity)
    );
    assert!(manager.devices[7].as_ref().unwrap().effective_enabled);
    assert_eq!(
        manager.devices[7].as_ref().unwrap().state,
        ConnectionState::Connected
    );
}
#[test]
fn failed_delete_keeps_the_previous_complete_device() {
    let (mut manager, mut store, mut radio) = setup();
    store.fail_remove = Some(cordial_core::storage::record_key(2, 77));
    assert_eq!(
        block_on(manager.unpair(0, &mut store, &mut radio)),
        Err(Error::StorageFailed)
    );
    assert!(radio.bonds.contains(&peer(1)));
    assert!(!manager.devices[0].as_ref().unwrap().policy.deleting);
    assert!(manager.devices[0].as_ref().unwrap().policy.enabled);
    store.fail_remove = None;
    radio.transports = Some(cordial_core::bluetooth::Capabilities {
        classic: false,
        ble: true,
        ble_scan_and_connect: false,
    });
    let mut reboot = Manager::default();
    block_on(reboot.load(&mut store, &mut radio)).unwrap();
    assert_eq!(reboot.devices.iter().flatten().count(), 1);
    assert!(
        store
            .records
            .contains_key(&cordial_core::storage::record_key(2, 77))
    );
}

#[test]
fn policy_and_adapter_commit_errors_are_read_back_before_publishing() {
    use cordial_core::storage::record_key;
    for committed in [false, true] {
        let (mut manager, mut store, _) = setup();
        let mut policy = manager.devices[0].as_ref().unwrap().policy.clone();
        policy.trusted = !policy.trusted;
        store.fail_save = Some((record_key(2, 77), committed));
        assert_eq!(
            block_on(manager.policy(0, policy.clone(), &mut store)).is_ok(),
            committed
        );
        assert_eq!(
            manager.devices[0].as_ref().unwrap().policy == policy,
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
    use cordial_core::storage::record_key;
    let (mut manager, mut store, mut radio) = setup();
    block_on(manager.prepare_pair(peer(2), None, &mut store, &mut radio)).unwrap();
    let first: u64 = serde_json::from_slice(&store.records[&record_key(7, 0)]).unwrap();
    let link = manager.pair(peer(2), 90_000, &mut radio).unwrap();
    let slot = block_on(manager.bonded(link, peer(2), b"new", &mut store, &mut radio)).unwrap();
    manager.disconnected(link, None, 0);
    block_on(manager.unpair(slot, &mut store, &mut radio)).unwrap();
    assert!(!store.records.contains_key(&record_key(2, first)));
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    block_on(manager.prepare_pair(peer(3), None, &mut store, &mut radio)).unwrap();
    let next: u64 = serde_json::from_slice(&store.records[&record_key(7, 0)]).unwrap();
    assert!(next > first);
}
#[test]
fn late_identity_cannot_take_over_an_existing_live_link() {
    let (mut manager, mut store, mut radio) = setup();
    let mut policy = manager.devices[0].as_ref().unwrap().policy.clone();
    policy.peer.transport = cordial_core::model::identifiers::Transport::Ble;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &policy,
        &bond(policy.id, policy.peer),
    ))
    .unwrap();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    let old = manager
        .connect(0, true, 30_000, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(old, descriptor(), 255, 0).unwrap();
    let candidate = cordial_core::devices::Peer {
        transport: policy.peer.transport,
        ..peer(2)
    };
    block_on(manager.prepare_pair(candidate, None, &mut store, &mut radio)).unwrap();
    let new = manager.pair(candidate, 90_000, &mut radio).unwrap();
    // Use BLE: Classic identities cannot change after pairing.
    let identity = policy.peer;
    assert_eq!(
        block_on(manager.bonded(new, identity, b"new", &mut store, &mut radio)),
        Err(Error::Busy)
    );
    assert_eq!(manager.link_for(0), Some(old));
    assert_eq!(manager.devices[0].as_ref().unwrap().policy.bond, 77);
}
#[test]
fn device_and_bond_are_one_json_record() {
    let (_, store, _) = setup();
    assert!(store.records.keys().all(|key| !matches!(key[0], 5 | 6)));
    let value: serde_json::Value =
        serde_json::from_slice(&store.records[&cordial_core::storage::record_key(2, 77)]).unwrap();
    assert!(value["bond"]["keys"]["Classic"]["key"].is_string());
}

#[test]
fn disabling_ble_hidpp_requests_fresh_standard_battery() {
    use cordial_core::model::{identifiers::Transport, info::InfoKey};
    let (mut manager, mut store, mut radio) = setup();
    let d = manager.devices[0].as_mut().unwrap();
    d.policy.peer.transport = Transport::Ble;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &d.policy,
        &bond(d.policy.bond, d.policy.peer),
    ))
    .unwrap();
    d.catalog.info.battery.configure(Transport::Ble, true);
    let id = manager
        .connect(0, true, 30_000, &mut radio)
        .unwrap()
        .unwrap();
    manager.connected(id, descriptor(), 255, 0).unwrap();
    let d = manager.devices[0].as_mut().unwrap();
    d.catalog.info.battery.hidpp_reports(true);
    d.catalog.info.battery.vendor_reading(Some(75), Some(true));
    let mut policy = d.policy.clone();
    policy.hidpp_enabled = false;
    block_on(manager.policy(0, policy, &mut store)).unwrap();
    assert!(
        !manager.devices[0]
            .as_ref()
            .unwrap()
            .catalog
            .info
            .battery
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
fn preferences_without_a_device_are_removed_at_startup() {
    let (mut manager, mut store, mut radio) = setup();
    let orphan = cordial_core::storage::record_key(4, 999);
    store.records.insert(orphan, b"[]".to_vec());
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert!(!store.records.contains_key(&orphan));
    assert!(manager.devices[0].is_some());
}
