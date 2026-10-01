//! Application-owned policy and live links. Bluetooth adapters own security and
//! vendor procedures; this module owns admission, persistence and HID forwarding.
use crate::{
    bluetooth::{Bluetooth, ConnectionSecurity, Descriptor, ReportType},
    devices::{ACTIVE_CONNECTIONS, AdapterPreference, Device, Peer, Policies, Policy, Setup},
    forward::Forwarder,
    link::{Link, LinkId, Profile},
    storage::{Preferences, RecordStore},
};
use alloc::{boxed::Box, string::ToString, vec::Vec};
use cordial_protocol::{
    errors::{ErrorCode as Error, WarningCode},
    identifiers::{
        ConnectionState as State, DeviceId, HostPlatform, NormalizationState, PairingState,
        Reconnect, Role, SettingsState, Transport,
    },
    messages::{self, WireError},
};

pub struct Connection {
    pub id: LinkId,
    pub peer: Peer,
    pub device: Option<usize>,
    pub runtime: Option<Box<Link>>,
    pub security: Option<ConnectionSecurity>,
    pub closing: bool,
    pub error: Option<Error>,
    pub deadline: u64,
    /// A setup save failed; setup resumes on the device's next connection.
    pub setup_failed: bool,
}

pub struct Manager {
    pub devices: Vec<Option<Device>>,
    pub connections: [Option<Connection>; ACTIVE_CONNECTIONS],
    pub forward: Forwarder,
    pub preference: AdapterPreference,
    pub revision: u64,
    pub storage_ready: bool,
    pub write_uncertain: bool,
    pub radio_ready: bool,
    generation: u64,
    caps: crate::bluetooth::Capabilities,
    pub available_bytes: usize,
    native_limits: [usize; 2],
    pending_device: Option<u64>,
}
impl Default for Manager {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            connections: core::array::from_fn(|_| None),
            forward: Forwarder::default(),
            preference: AdapterPreference::default(),
            revision: 0,
            storage_ready: false,
            write_uncertain: false,
            radio_ready: false,
            generation: 0,
            caps: crate::bluetooth::Capabilities {
                classic: false,
                ble: false,
                ble_scan_and_connect: false,
            },
            available_bytes: 0,
            native_limits: [0; 2],
            pending_device: None,
        }
    }
}
impl Manager {
    /// Load complete records before publishing readiness or admitting a peer.
    /// Load each document separately; never format a failed store here.
    pub async fn load<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        if self.connections.iter().any(Option::is_some) {
            return Err(Error::Busy);
        }
        self.storage_ready = false;
        self.write_uncertain = false;
        crate::storage::open(store)
            .await
            .map_err(|_| Error::StorageFailed)?;
        let preference = Policies { store }
            .load_adapter()
            .await
            .map_err(|_| Error::StorageFailed)?;
        let mut devices: Vec<Option<Device>> = Vec::new();
        self.caps = radio.capabilities();
        self.native_limits = [
            radio.bond_capacity(cordial_protocol::identifiers::Transport::Classic),
            radio.bond_capacity(cordial_protocol::identifiers::Transport::Ble),
        ];
        let mut previous = None;
        while let Some(key) = store
            .next_key(previous)
            .await
            .map_err(|_| Error::StorageFailed)?
        {
            previous = Some(key);
            if key[0] != 2 {
                continue;
            }
            let (policy, record_error) = Policies { store }.load_record(key).await;
            use cordial_protocol::errors::ValidationError;
            let mut validation =
                record_error.or(match crate::bonds::load(store, policy.bond).await {
                    Ok(Some(bond)) if !bond.valid() => Some(ValidationError::BondCorrupt),
                    Ok(Some(bond)) if bond.owner != policy.id || bond.identity != policy.peer => {
                        Some(ValidationError::BondMismatch)
                    }
                    Ok(Some(_)) => None,
                    Ok(None) => Some(ValidationError::BondMissing),
                    Err(crate::storage::Error::Corrupt) => Some(ValidationError::BondCorrupt),
                    Err(_) => Some(ValidationError::ReadFailed),
                });
            let preferences = match (Preferences {
                store,
                device: policy.id,
            })
            .load_all()
            .await
            {
                Ok(p) => p,
                Err(_) => {
                    validation = Some(ValidationError::ReadFailed);
                    Vec::new()
                }
            };
            let mut device = Device::new(policy);
            device.validation_error = validation;
            device.pairing_state = if matches!(
                validation,
                Some(
                    ValidationError::BondMissing
                        | ValidationError::BondCorrupt
                        | ValidationError::BondMismatch
                )
            ) {
                PairingState::NeedsPairing
            } else {
                PairingState::Paired
            };
            device
                .catalog
                .restore_preferences(preferences)
                .map_err(|_| Error::StorageFailed)?;
            devices.try_reserve(1).map_err(|_| Error::Capacity)?;
            for other in devices.iter_mut().flatten() {
                if other.policy.peer == device.policy.peer {
                    other.validation_error = Some(ValidationError::DeviceCorrupt);
                    device.validation_error = Some(ValidationError::DeviceCorrupt);
                }
            }
            devices.push(Some(device));
        }
        self.devices = devices;
        self.refresh_enabled();
        self.sync_bonds(store, radio).await?;
        self.pending_device = None;
        self.available_bytes = store.available().await.map_err(|_| Error::StorageFailed)?;
        self.preference = preference;
        self.storage_ready = true;
        Ok(())
    }
    pub fn refresh_enabled(&mut self) {
        let eligible = |d: &Device| {
            self.caps.supports(d.policy.peer.transport)
                && d.policy.enabled
                && !d.policy.blocked
                && !d.policy.deleting
                && d.validation_error.is_none()
                && d.pairing_state == PairingState::Paired
        };
        // Keep current selections; repairing another preferred record must not
        // evict a working device. Startup records are sorted by stable ID.
        let mut used = [0usize; 2];
        for d in self.devices.iter_mut().flatten() {
            let kind = usize::from(
                d.policy.peer.transport == cordial_protocol::identifiers::Transport::Ble,
            );
            d.transport_supported = self.caps.supports(d.policy.peer.transport);
            d.effective_enabled = d.effective_enabled
                && eligible(d)
                && used[kind] < self.native_limits[kind].saturating_sub(1);
            used[kind] += usize::from(d.effective_enabled);
        }
        loop {
            let next = self
                .devices
                .iter()
                .enumerate()
                .filter_map(|(slot, d)| d.as_ref().map(|d| (slot, d)))
                .filter(|(_, d)| {
                    !d.effective_enabled
                        && eligible(d)
                        && used[usize::from(
                            d.policy.peer.transport
                                == cordial_protocol::identifiers::Transport::Ble,
                        )] < self.native_limits[usize::from(
                            d.policy.peer.transport
                                == cordial_protocol::identifiers::Transport::Ble,
                        )]
                        .saturating_sub(1)
                })
                .min_by_key(|(_, d)| d.policy.id)
                .map(|(slot, _)| slot);
            let Some(slot) = next else {
                break;
            };
            let d = self.devices[slot].as_mut().unwrap();
            d.effective_enabled = true;
            used[usize::from(
                d.policy.peer.transport == cordial_protocol::identifiers::Transport::Ble,
            )] += 1;
        }
    }
    pub fn capacity(&self, caps: crate::bluetooth::Capabilities) -> messages::Capacity {
        use cordial_protocol::{errors::PairUnavailable, identifiers::Transport};
        let mut enabled = Vec::new();
        let mut pairing = Vec::new();
        for transport in [Transport::Classic, Transport::Ble]
            .into_iter()
            .filter(|t| caps.supports(*t))
        {
            enabled.push(messages::EnabledCapacity {
                transports: alloc::vec![transport],
                limit: self.native_limits[usize::from(transport == Transport::Ble)]
                    .saturating_sub(1),
                enabled: self
                    .devices
                    .iter()
                    .flatten()
                    .filter(|d| d.effective_enabled && d.policy.peer.transport == transport)
                    .count(),
            });
            let reason = if !self.storage_ready {
                Some(PairUnavailable::StorageUnavailable)
            } else if self.native_limits[usize::from(transport == Transport::Ble)] == 0 {
                Some(PairUnavailable::SetupCapacity)
            } else if !self.radio_ready {
                Some(PairUnavailable::RadioUnavailable)
            } else if self.available_bytes
                < crate::bonds::MAINTENANCE_BYTES + crate::bonds::PAIR_BYTES
            {
                Some(PairUnavailable::StorageFull)
            } else if self
                .connections
                .iter()
                .flatten()
                .any(|c| c.device.is_none())
            {
                Some(PairUnavailable::PairingActive)
            } else if self.connections.iter().all(Option::is_some) {
                Some(PairUnavailable::ConnectionsFull)
            } else {
                None
            };
            pairing.push(messages::PairingCapacity {
                transport,
                available: reason.is_none(),
                reason,
                estimated_additional: (self
                    .available_bytes
                    .saturating_sub(crate::bonds::MAINTENANCE_BYTES)
                    / crate::bonds::PAIR_BYTES),
            });
        }
        messages::Capacity { enabled, pairing }
    }
    pub fn changed(&mut self) -> u64 {
        self.revision = self
            .revision
            .saturating_add(1)
            .min(cordial_protocol::MAX_REVISION);
        self.revision
    }
    pub fn find(&self, id: &DeviceId) -> Result<usize, Error> {
        self.devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.policy.device_id() == *id))
            .ok_or(Error::NotFound)
    }
    pub fn peer(&self, peer: Peer) -> Option<usize> {
        self.devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.policy.peer == peer))
    }
    pub fn connection(&self, id: LinkId) -> Option<&Connection> {
        self.connections
            .get(id.slot as usize)?
            .as_ref()
            .filter(|c| c.id == id)
    }
    pub fn connection_mut(&mut self, id: LinkId) -> Option<&mut Connection> {
        self.connections
            .get_mut(id.slot as usize)?
            .as_mut()
            .filter(|c| c.id == id)
    }
    pub fn link_for(&self, device: usize) -> Option<LinkId> {
        self.connections
            .iter()
            .flatten()
            .find(|c| c.device == Some(device))
            .map(|c| c.id)
    }
    fn allocate(
        &mut self,
        peer: Peer,
        device: Option<usize>,
        deadline: u64,
    ) -> Result<LinkId, Error> {
        if self.connections.iter().flatten().any(|c| c.peer == peer) {
            return Err(Error::Busy);
        }
        let slot = self
            .connections
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        self.generation = self.generation.checked_add(1).ok_or(Error::InternalError)?;
        let id = LinkId {
            slot: slot as u8,
            generation: self.generation,
        };
        self.connections[slot] = Some(Connection {
            id,
            peer,
            device,
            runtime: None,
            security: None,
            closing: false,
            error: None,
            deadline,
            setup_failed: false,
        });
        Ok(id)
    }
    pub fn connect<B: Bluetooth>(
        &mut self,
        device: usize,
        explicit: bool,
        deadline: u64,
        radio: &mut B,
    ) -> Result<Option<LinkId>, Error> {
        if !self.radio_ready {
            return Err(Error::RadioUnavailable);
        }
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        let d = self
            .devices
            .get_mut(device)
            .and_then(Option::as_mut)
            .ok_or(Error::NotFound)?;
        if !radio.capabilities().supports(d.policy.peer.transport) {
            return Err(Error::UnsupportedTransport);
        }
        if d.validation_error.is_some() {
            return Err(if d.pairing_state == PairingState::NeedsPairing {
                Error::PairingRequired
            } else {
                Error::StorageFailed
            });
        }
        if !d.policy.enabled {
            return Err(Error::Disabled);
        }
        if !d.effective_enabled {
            return Err(Error::Capacity);
        }
        if d.pairing_state == PairingState::NeedsPairing {
            return Err(Error::PairingRequired);
        }
        if explicit {
            d.explicit_connect()?;
        }
        if d.policy.blocked {
            return Err(Error::Blocked);
        }
        if d.state == State::Connected {
            return Ok(None);
        }
        if explicit && d.state == State::Connecting {
            return self.link_for(device).map(Some).ok_or(Error::Busy);
        }
        if d.state != State::Disconnected {
            return Err(Error::Busy);
        }
        let peer = d.policy.peer;
        let id = self.allocate(peer, Some(device), deadline)?;
        if let Err(error) = radio.connect(id, peer, false) {
            self.connections[id.slot as usize] = None;
            return Err(error);
        }
        self.devices[device].as_mut().unwrap().state = State::Connecting;
        Ok(Some(id))
    }
    pub fn pair<B: Bluetooth>(
        &mut self,
        peer: Peer,
        deadline: u64,
        radio: &mut B,
    ) -> Result<LinkId, Error> {
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        if !self.radio_ready {
            return Err(Error::RadioUnavailable);
        }
        if !radio.capabilities().supports(peer.transport) {
            return Err(Error::UnsupportedTransport);
        }
        if self.native_limits
            [usize::from(peer.transport == cordial_protocol::identifiers::Transport::Ble)]
            == 0
        {
            return Err(Error::Capacity);
        }
        if self.available_bytes < crate::bonds::MAINTENANCE_BYTES + crate::bonds::PAIR_BYTES {
            return Err(Error::StorageFull);
        }
        if self.devices.iter().all(Option::is_some) {
            self.devices.try_reserve(1).map_err(|_| Error::Capacity)?;
            self.devices.push(None);
        }
        if let Some(slot) = self.peer(peer) {
            let d = self.devices[slot].as_ref().unwrap();
            if d.policy.blocked {
                return Err(Error::Blocked);
            }
        }
        if self.devices.iter().all(Option::is_some)
            && !self
                .devices
                .iter()
                .flatten()
                .any(|d| d.pairing_state == PairingState::NeedsPairing && !d.policy.blocked)
        {
            return Err(Error::Capacity);
        }
        if self
            .connections
            .iter()
            .flatten()
            .any(|c| c.device.is_none())
        {
            return Err(Error::Busy);
        }
        let id = self.allocate(peer, None, deadline)?;
        if let Err(error) = radio.connect(id, peer, true) {
            self.connections[id.slot as usize] = None;
            return Err(error);
        }
        Ok(id)
    }
    /// Rebuild the native active view from committed records. Import existing
    /// entries in place so unrelated live connections keep their database index.
    pub async fn sync_bonds<S: RecordStore, B: Bluetooth>(
        &self,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        for peer in radio.bonds().await? {
            if !self
                .devices
                .iter()
                .flatten()
                .any(|d| d.effective_enabled && d.policy.peer == peer)
                && !self.connections.iter().flatten().any(|c| c.peer == peer)
            {
                radio.forget(peer).await?;
            }
        }
        for d in self
            .devices
            .iter()
            .flatten()
            .filter(|d| d.effective_enabled)
        {
            let bond = crate::bonds::load(store, d.policy.bond)
                .await
                .map_err(|_| Error::StorageFailed)?
                .ok_or(Error::StorageFailed)?;
            radio.import_bond(&bond).await?;
        }
        Ok(())
    }
    pub async fn prepare_pair<S: RecordStore, B: Bluetooth>(
        &mut self,
        peer: Peer,
        identity: Option<Peer>,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        if self.native_limits
            [usize::from(peer.transport == cordial_protocol::identifiers::Transport::Ble)]
            == 0
        {
            return Err(Error::Capacity);
        }
        self.sync_bonds(store, radio).await?;
        self.available_bytes = store.available().await.map_err(|_| Error::StorageFailed)?;
        if self.available_bytes < crate::bonds::MAINTENANCE_BYTES + crate::bonds::PAIR_BYTES {
            return Err(Error::StorageFull);
        }
        // IDs are never reused, including after deleting the highest saved ID.
        let counter_key = crate::storage::record_key(7, 0);
        let previous: u64 = match store
            .load_owned(counter_key)
            .await
            .map_err(|_| Error::StorageFailed)?
        {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| Error::StorageFailed)?,
            None => return Err(Error::StorageFailed),
        };
        let nonce = previous.checked_add(1).ok_or(Error::Capacity)?;
        store
            .save(
                counter_key,
                &crate::storage::json(&nonce).map_err(|_| Error::StorageFailed)?,
            )
            .await
            .map_err(|_| Error::StorageFailed)?;
        if self.devices.iter().all(Option::is_some) {
            self.devices.try_reserve(1).map_err(|_| Error::Capacity)?;
            self.devices.push(None);
        }
        self.pending_device = Some(nonce);
        radio.forget(identity.unwrap_or(peer)).await?;
        Ok(())
    }
    pub async fn finish_pair<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        self.sync_bonds(store, radio).await?;
        self.pending_device = None;
        self.available_bytes = store.available().await.map_err(|_| Error::StorageFailed)?;
        Ok(())
    }
    pub fn incoming<B: Bluetooth>(
        &mut self,
        attempt: u32,
        peer: Peer,
        now: u64,
        radio: &mut B,
    ) -> Result<Option<usize>, Error> {
        let device = self.peer(peer).filter(|&slot| {
            self.storage_ready
                && self.radio_ready
                && self.devices[slot].as_ref().is_some_and(|d| {
                    d.allow_incoming()
                        && d.state == State::Disconnected
                        && (peer.transport != Transport::Ble
                            || (d.reconnect_due(now)
                                && self.connections.iter().flatten().count()
                                    < ACTIVE_CONNECTIONS - 1
                                && !self
                                    .connections
                                    .iter()
                                    .flatten()
                                    .any(|c| c.runtime.is_none() || c.closing)))
                })
        });
        let id = device.and_then(|slot| {
            self.allocate(peer, Some(slot), now.saturating_add(30_000))
                .ok()
        });
        if let Err(e) = radio.incoming(attempt, id) {
            if let Some(id) = id {
                self.connections[id.slot as usize] = None;
            }
            return Err(e);
        }
        if id.is_none() {
            return Ok(None);
        }
        let slot = device.unwrap();
        self.devices[slot].as_mut().unwrap().state = State::Connecting;
        Ok(Some(slot))
    }
    pub fn close<B: Bluetooth>(&mut self, id: LinkId, error: Option<Error>, radio: &mut B) {
        let Some(c) = self.connection_mut(id) else {
            return;
        };
        if error.is_some() {
            c.error = error;
        }
        if c.closing {
            return;
        }
        c.closing = true;
        radio.disconnect(id);
        let device = c.device;
        self.forward.remove(id.slot as usize);
        if let Some(slot) = device {
            let d = self.devices[slot].as_mut().unwrap();
            d.state = State::Disconnecting;
            d.catalog.connection(false, d.policy.hidpp_enabled);
        }
    }
    pub fn disconnect<B: Bluetooth>(&mut self, slot: usize, radio: &mut B) -> Result<(), Error> {
        let d = self
            .devices
            .get_mut(slot)
            .and_then(Option::as_mut)
            .ok_or(Error::NotFound)?;
        d.paused = true;
        if let Some(id) = self.link_for(slot) {
            self.close(id, None, radio);
        }
        Ok(())
    }
    /// Caller verifies the live pairing request, heartbeat and deadline first.
    /// New native keys remain provisional until the device record is committed.
    pub async fn bonded<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: LinkId,
        identity: Peer,
        name: &[u8],
        store: &mut S,
        radio: &mut B,
    ) -> Result<usize, Error> {
        let c = self.connection(id).ok_or(Error::NotConnected)?;
        if c.closing || c.device.is_some() {
            return Err(Error::NotPending);
        }
        if identity.transport != c.peer.transport
            || (identity.transport == cordial_protocol::identifiers::Transport::Classic
                && identity != c.peer)
        {
            return Err(Error::AuthenticationFailed);
        }
        let pending = self.pending_device.ok_or(Error::NotPending)?;
        let retained = self.peer(identity);
        if retained.is_some_and(|slot| self.devices[slot].as_ref().unwrap().policy.blocked) {
            return Err(Error::Blocked);
        }
        if let Some(slot) = retained {
            if self.link_for(slot).is_some_and(|other| other != id) {
                return Err(Error::Busy);
            }
            if matches!(
                self.devices[slot].as_ref().unwrap().validation_error,
                Some(
                    cordial_protocol::errors::ValidationError::DeviceCorrupt
                        | cordial_protocol::errors::ValidationError::ReadFailed
                )
            ) {
                return Err(Error::StorageFailed);
            }
        }
        let slot = retained
            .or_else(|| self.devices.iter().position(Option::is_none))
            .ok_or(Error::Capacity)?;
        let mut policy = if let Some(slot) = retained {
            self.devices[slot].as_ref().unwrap().policy.clone()
        } else {
            let mut policy = Policy::paired(pending, identity, name);
            let used = self
                .devices
                .iter()
                .flatten()
                .filter(|d| d.effective_enabled && d.policy.peer.transport == identity.transport)
                .count();
            policy.enabled = used
                < self.native_limits[usize::from(
                    identity.transport == cordial_protocol::identifiers::Transport::Ble,
                )]
                .saturating_sub(1);
            policy
        };
        let mut bond = radio.export_bond(identity).await?;
        bond.owner = policy.id;
        bond.identity = identity;
        bond.complete = true;
        policy.bond = policy.id;
        // Adopt before commit; a failed commit is restored from the old record
        // after teardown. Resolve ambiguous write errors by reading the record.
        radio.adopt(id)?;
        if let Err(error) = crate::bonds::commit(store, &policy, &bond).await {
            self.write_uncertain = error == crate::storage::Error::Unknown;
            if self.write_uncertain {
                self.storage_ready = false;
            }
            return Err(if error == crate::storage::Error::Full {
                Error::StorageFull
            } else {
                Error::StorageFailed
            });
        }
        if retained.is_none() {
            self.devices[slot] = Some(Device::new(policy.clone()));
        }
        let d = self.devices[slot].as_mut().unwrap();
        d.policy = policy;
        d.validation_error = None;
        d.pairing_state = PairingState::Paired;
        d.paused = false;
        d.error = None;
        d.state = State::Connecting;
        let c = self.connection_mut(id).unwrap();
        c.peer = identity;
        c.device = Some(slot);
        self.refresh_enabled();
        if self.finish_pair(store, radio).await.is_err() {
            self.storage_ready = false;
        }
        Ok(slot)
    }
    pub fn security(&mut self, id: LinkId, security: ConnectionSecurity) -> Option<usize> {
        let c = self.connection_mut(id)?;
        if c.closing || c.security == Some(security) {
            return None;
        }
        c.security = Some(security);
        // Setup retains the observation for the eventual connected snapshot.
        c.runtime.as_ref()?;
        c.device
    }
    pub fn connected(
        &mut self,
        id: LinkId,
        descriptors: Vec<Descriptor>,
        max_output: usize,
        now: u64,
    ) -> Result<Option<usize>, Error> {
        let Some(c) = self.connection(id) else {
            return Ok(None);
        };
        if c.closing {
            return Ok(None);
        }
        if c.deadline != 0 && now >= c.deadline {
            return Err(Error::Timeout);
        }
        if c.runtime.is_some() {
            return Err(Error::ConnectionFailed);
        }
        let slot = c.device.ok_or(Error::AuthenticationFailed)?;
        let profiles = descriptors
            .into_iter()
            .map(|d| Profile::compile(d.service, &d.bytes))
            .collect::<Result<Vec<_>, _>>()?;
        let d = self.devices[slot].as_mut().ok_or(Error::NotFound)?;
        if !d.effective_enabled {
            return Err(Error::Disabled);
        }
        if d.pairing_state != PairingState::Paired {
            return Err(Error::PairingRequired);
        }
        let runtime = Link::new(
            id,
            profiles,
            max_output,
            d.policy.hidpp_enabled,
            self.preference.host_platform,
            &mut d.catalog,
        )?;
        d.roles = runtime.roles;
        d.catalog.info.kind_hint(
            match (
                d.roles & crate::hid::KEYBOARD != 0,
                d.roles & crate::hid::MOUSE != 0,
            ) {
                (true, true) => messages::DeviceKind::KeyboardMouse,
                (true, false) => messages::DeviceKind::Keyboard,
                (false, true) => messages::DeviceKind::Mouse,
                _ => messages::DeviceKind::Unknown,
            },
        );
        d.warnings = runtime.warnings;
        d.connection(State::Connected, None, now);
        let c = self.connections[id.slot as usize].as_mut().unwrap();
        c.runtime = Some(Box::new(runtime));
        c.deadline = 0;
        Ok(Some(slot))
    }
    /// The caller drains explicit settings results before dropping this returned
    /// runtime, or releases them if the serial session no longer owns the job.
    pub fn disconnected(
        &mut self,
        id: LinkId,
        error: Option<Error>,
        now: u64,
    ) -> Option<(Option<usize>, Option<Box<Link>>)> {
        self.connection(id)?;
        let mut c = self.connections[id.slot as usize].take().unwrap();
        if let Some(slot) = c.device {
            let d = self.devices[slot].as_mut().unwrap();
            if let Some(link) = &mut c.runtime {
                link.disconnected(&mut d.catalog, &mut self.forward);
            }
            d.connection(State::Disconnected, c.error.or(error), now);
        } else {
            self.forward.remove(id.slot as usize);
        }
        Some((c.device, c.runtime))
    }
    pub fn input(
        &mut self,
        report: &crate::bluetooth::InputReport,
        now: u64,
    ) -> Result<bool, Error> {
        let Some(c) = self.connection(report.link) else {
            return Ok(false);
        };
        if c.closing || c.runtime.is_none() {
            return Ok(false);
        }
        let slot = c.device.ok_or(Error::NotConnected)?;
        if !self.devices[slot]
            .as_ref()
            .is_some_and(|d| d.effective_enabled)
        {
            return Ok(false);
        }
        let c = self.connections[report.link.slot as usize]
            .as_mut()
            .unwrap();
        let d = self.devices[slot].as_mut().unwrap();
        let changed = c.runtime.as_mut().unwrap().input(
            report.service,
            report.report_id,
            report.payload(),
            &mut d.catalog,
            &mut self.forward,
            now,
        )?;
        Ok(changed)
    }
    pub fn poll_link<B: Bluetooth>(
        &mut self,
        index: usize,
        leds: u8,
        now: u64,
        radio: &mut B,
    ) -> Result<Option<usize>, Error> {
        let Some(c) = self.connections.get_mut(index).and_then(Option::as_mut) else {
            return Ok(None);
        };
        if c.closing {
            return Ok(None);
        }
        if c.deadline != 0 && now >= c.deadline {
            return Err(Error::Timeout);
        }
        let Some(link) = c.runtime.as_mut() else {
            return Ok(None);
        };
        let slot = c.device.unwrap();
        let d = self.devices[slot].as_mut().unwrap();
        let mut changed = link.poll(&mut d.catalog, &mut self.forward, now)?;
        if link.info_refresh_pending && radio.refresh_info(c.id).is_ok() {
            link.info_refresh_pending = false;
        }
        if radio.can_write(c.id)
            && let Some(out) = link.output(leds, now)?
        {
            let id = out.id;
            if radio
                .write(
                    id,
                    out.service,
                    ReportType::Output,
                    out.report_id,
                    out.payload,
                )
                .is_err()
            {
                changed |=
                    link.output_complete(id, false, &mut d.catalog, &mut self.forward, now)?;
            }
        }
        if radio.can_write(c.id)
            && let Some(read) = link.battery_read(&d.catalog, now)
            && let Err(e) = radio.read(read.id, read.service, read.kind, read.report_id)
        {
            link.battery_read_complete(read.id, read.kind, Err(e), &mut d.catalog);
        }
        if d.warnings != link.warnings {
            d.warnings = link.warnings;
            changed = true;
        }
        Ok(changed.then_some(slot))
    }
    pub fn written(
        &mut self,
        id: crate::link::WriteId,
        success: bool,
        now: u64,
    ) -> Result<Option<usize>, Error> {
        let Some(c) = self.connection(id.link) else {
            return Ok(None);
        };
        if c.closing {
            return Ok(None);
        }
        let Some(slot) = c.device else {
            return Ok(None);
        };
        let c = self.connections[id.link.slot as usize].as_mut().unwrap();
        let Some(link) = &mut c.runtime else {
            return Ok(None);
        };
        let d = self.devices[slot].as_mut().unwrap();
        let mut changed =
            link.output_complete(id, success, &mut d.catalog, &mut self.forward, now)?;
        if d.warnings != link.warnings {
            d.warnings = link.warnings;
            changed = true;
        }
        Ok(changed.then_some(slot))
    }
    /// The next first-connection setup progress to save: the connection and
    /// device slot of a connected device with `setup_pending`, the policy
    /// recording its newly settled steps, and the settled steps to keep once
    /// that policy is saved.
    pub fn setup(&self) -> Option<(usize, usize, Policy, Setup)> {
        self.connections.iter().enumerate().find_map(|(index, c)| {
            let c = c.as_ref()?;
            let link = c.runtime.as_ref()?;
            let slot = c.device?;
            let d = self.devices[slot].as_ref()?;
            // A requested settings job finishes before setup reconfigures the link.
            if c.closing || c.setup_failed || !d.policy.setup_pending || link.settings.explicit() {
                return None;
            }
            let mut setup = d.setup;
            let mut hidpp = false;
            if !setup.hidpp
                && let Some(found) = link.hidpp_found()
            {
                setup.hidpp = true;
                hidpp = found;
            }
            if setup == d.setup {
                return None;
            }
            let mut policy = d.policy.clone();
            policy.hidpp_enabled |= hidpp;
            policy.setup_pending = !setup.complete();
            Some((index, slot, policy, setup))
        })
    }
    pub async fn policy<S: RecordStore>(
        &mut self,
        slot: usize,
        policy: Policy,
        store: &mut S,
    ) -> Result<(), Error> {
        self.write_uncertain = false;
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        let current = self
            .devices
            .get(slot)
            .and_then(Option::as_ref)
            .ok_or(Error::NotFound)?;
        if (!current.policy.enabled || current.policy.blocked)
            && policy.enabled
            && !policy.blocked
            && self.caps.supports(policy.peer.transport)
            && current.validation_error.is_none()
            && current.pairing_state == PairingState::Paired
        {
            let used = self
                .devices
                .iter()
                .flatten()
                .filter(|d| d.effective_enabled && d.policy.peer.transport == policy.peer.transport)
                .count();
            if used
                >= self.native_limits[usize::from(
                    policy.peer.transport == cordial_protocol::identifiers::Transport::Ble,
                )]
                .saturating_sub(1)
            {
                return Err(Error::Capacity);
            }
        }
        let d = self.devices[slot].as_mut().unwrap();
        if d.policy.deleting {
            return Err(Error::Busy);
        }
        if matches!(
            d.validation_error,
            Some(
                cordial_protocol::errors::ValidationError::DeviceCorrupt
                    | cordial_protocol::errors::ValidationError::ReadFailed
            )
        ) {
            return Err(Error::StorageFailed);
        }
        if policy.id != d.policy.id || policy.peer != d.policy.peer || policy.name != d.policy.name
        {
            return Err(Error::InvalidArgs);
        }
        if d.policy == policy {
            return Ok(());
        }
        Policies { store }
            .save(slot, &policy)
            .await
            .map_err(|error| {
                self.write_uncertain = error == crate::storage::Error::Unknown;
                if self.write_uncertain {
                    self.storage_ready = false;
                }
                if error == crate::storage::Error::Full {
                    Error::StorageFull
                } else {
                    Error::StorageFailed
                }
            })?;
        let was_vendor = d.catalog.info.battery.vendor();
        d.policy = policy;
        d.catalog
            .connection(d.state == State::Connected, d.policy.hidpp_enabled);
        if let Some(c) = self
            .connections
            .iter_mut()
            .flatten()
            .find(|c| c.device == Some(slot) && !c.closing)
            && let Some(link) = &mut c.runtime
            && link.enabled() != d.policy.hidpp_enabled
        {
            // Subscribed BAS values may not notify again until the battery changes.
            link.info_refresh_pending = d.policy.peer.transport == Transport::Ble
                && was_vendor
                && !d.catalog.info.battery.vendor();
            link.reconfigure(
                d.policy.hidpp_enabled,
                self.preference.host_platform,
                &mut d.catalog,
            );
        }
        self.refresh_enabled();
        Ok(())
    }
    pub async fn name<S: RecordStore>(
        &mut self,
        name: Option<&str>,
        store: &mut S,
    ) -> Result<(), Error> {
        self.write_uncertain = false;
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        self.save_preference(
            AdapterPreference {
                name: name.map(Into::into),
                ..self.preference.clone()
            },
            store,
        )
        .await
    }
    async fn save_preference<S: RecordStore>(
        &mut self,
        preference: AdapterPreference,
        store: &mut S,
    ) -> Result<(), Error> {
        Policies { store }
            .save_adapter(&preference)
            .await
            .map_err(|error| {
                self.write_uncertain = error == crate::storage::Error::Unknown;
                if self.write_uncertain {
                    self.storage_ready = false;
                }
                if error == crate::storage::Error::Full {
                    Error::StorageFull
                } else {
                    Error::StorageFailed
                }
            })?;
        self.preference = preference;
        Ok(())
    }
    pub async fn platform<S: RecordStore>(
        &mut self,
        platform: HostPlatform,
        store: &mut S,
    ) -> Result<(), Error> {
        self.write_uncertain = false;
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        if self.preference.host_platform == platform {
            return Ok(());
        }
        let preference = AdapterPreference {
            host_platform: platform,
            ..self.preference.clone()
        };
        self.save_preference(preference, store).await?;
        for c in self.connections.iter_mut().flatten().filter(|c| !c.closing) {
            let Some(slot) = c.device else {
                continue;
            };
            let d = self.devices[slot].as_mut().unwrap();
            if d.policy.hidpp_enabled
                && let Some(link) = &mut c.runtime
            {
                link.reconfigure(true, platform, &mut d.catalog);
            }
        }
        Ok(())
    }
    pub async fn unpair<S: RecordStore, B: Bluetooth>(
        &mut self,
        slot: usize,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        self.write_uncertain = false;
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        if self.link_for(slot).is_some() {
            return Err(Error::Busy);
        }
        let d = self
            .devices
            .get(slot)
            .and_then(Option::as_ref)
            .ok_or(Error::NotFound)?;
        let previous_policy = d.policy.clone();
        let mut policy = previous_policy.clone();
        policy.deleting = true;
        policy.enabled = false;
        self.devices[slot].as_mut().unwrap().policy = policy.clone();
        self.refresh_enabled();
        if radio.capabilities().supports(policy.peer.transport)
            && let Err(error) = radio.forget(policy.peer).await
        {
            self.storage_ready = false;
            return Err(error);
        }
        if let Err(error) = (Policies { store }).remove(slot, policy.id).await {
            self.write_uncertain = error == crate::storage::Error::Unknown;
            if self.write_uncertain {
                self.storage_ready = false;
            } else {
                self.devices[slot].as_mut().unwrap().policy = previous_policy;
                self.refresh_enabled();
                if self.sync_bonds(store, radio).await.is_err() {
                    self.storage_ready = false;
                }
            }
            return Err(if error == crate::storage::Error::Full {
                Error::StorageFull
            } else {
                Error::StorageFailed
            });
        }
        self.devices[slot] = None;
        self.refresh_enabled();
        // Deletion is committed. Reclamation failure cannot turn it into a failed removal.

        match store.available().await {
            Ok(bytes) => self.available_bytes = bytes,
            Err(_) => {
                self.available_bytes = 0;
                self.storage_ready = false;
            }
        }
        Ok(())
    }
    pub fn record(&self, slot: usize) -> Option<messages::Device> {
        let d = self.devices.get(slot)?.as_ref()?;
        let runtime = self
            .connections
            .iter()
            .flatten()
            .find(|c| c.device == Some(slot) && !c.closing)
            .and_then(|c| c.runtime.as_deref());
        let pending = if d.policy.hidpp_enabled {
            NormalizationState::Pending
        } else {
            NormalizationState::Off
        };
        let mut warnings = Vec::new();
        if d.warnings & 1 != 0 {
            warnings.push(WarningCode::UnsupportedFields);
        }
        if d.warnings & 2 != 0 {
            warnings.push(WarningCode::LedOutputUnavailable);
        }
        Some(messages::Device {
            device_id: d.policy.device_id(),
            pairing_state: d.pairing_state,
            enabled: d.policy.enabled,
            effective_enabled: d.effective_enabled,
            enabled_reason: if !d.transport_supported {
                Some(cordial_protocol::errors::DisabledReason::UnsupportedTransport)
            } else if d.validation_error.is_some() {
                Some(cordial_protocol::errors::DisabledReason::Invalid)
            } else if d.policy.blocked {
                Some(cordial_protocol::errors::DisabledReason::Blocked)
            } else if !d.policy.enabled {
                Some(cordial_protocol::errors::DisabledReason::Disabled)
            } else if !d.effective_enabled {
                Some(cordial_protocol::errors::DisabledReason::Capacity)
            } else {
                None
            },
            transport_supported: d.transport_supported,
            validation_error: d.validation_error,

            name: (!d.policy.name.is_empty()).then(|| d.policy.name.to_string()),
            transport: d.policy.peer.transport,
            roles: [Role::Keyboard, Role::Mouse, Role::ConsumerControl]
                .into_iter()
                .enumerate()
                .filter_map(|(i, r)| (d.roles & (1 << i) != 0).then_some(r))
                .collect(),
            state: d.state,
            security: self
                .link_for(slot)
                .and_then(|id| self.connection(id))
                .filter(|c| !c.closing && d.state == State::Connected)
                .and_then(|c| c.security),
            trusted: d.policy.trusted,
            blocked: d.policy.blocked,
            reconnect: if d.paused {
                Reconnect::Paused
            } else {
                Reconnect::Auto
            },
            last_error: d.error.map(|code| WireError {
                code,
                details: None,
            }),
            warnings,
            hidpp_enabled: d.policy.hidpp_enabled,
            hidpp_protocol: runtime.map_or(Default::default(), |r| r.client.protocol),
            normalization_state: runtime.map_or(pending, |r| r.client.status),
            normalization_error: runtime
                .filter(|r| r.client.status != NormalizationState::Off)
                .and_then(|r| r.client.error.map(|e| e.code())),
            settings_state: runtime.map_or(
                if d.policy.hidpp_enabled {
                    SettingsState::Pending
                } else {
                    SettingsState::Off
                },
                |r| r.settings.state,
            ),
            settings_error: runtime.and_then(|r| r.settings.error),
            settings_revision: d.settings_revision,
        })
    }
}
