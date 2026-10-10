//! Application-owned policy and live links. Bluetooth adapters own security and
//! vendor procedures; this module owns admission, persistence and HID forwarding.
//!
//! Only enabled devices the Bluetooth stack has room for are resident, as reconnection entries.
//! A connected device's policy, settings, readings and warnings live with its connection. Every
//! other saved device is read from flash when a command needs it.
use crate::model::{
    errors::ErrorCode as Error,
    identifiers::{ConnectionState as State, HostPlatform, Transport},
};
use crate::{
    bluetooth::{Bluetooth, Capabilities, ConnectionSecurity, Descriptor, Layout},
    devices::{
        ACTIVE_CONNECTIONS, AdapterPreference, Device, Live, Peer, Policies, Policy, Setup,
        Transports,
    },
    forward::Forwarder,
    link::{Link, LinkId, Profile},
    profiles::{self, LoadError},
    storage::{Preferences, RecordStore},
};
use alloc::{boxed::Box, vec::Vec};

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
    /// The `layouts::maps` fingerprint of the layout the link uses, when known.
    pub maps: Option<u64>,
    /// A discovered layout to save once the connection forwards input.
    pub layout: Option<Layout>,
    /// While a connected link has forwarded no input: when background storage work goes ahead
    /// anyway.
    pub starting: Option<u64>,
}

/// Saved device IDs read from flash one page at a time.
const SCAN_PAGE: usize = 16;
/// How long background storage work, including reading a connected device's policy and
/// preferences, waits for a new connection's first input before going ahead.
pub const FIRST_INPUT_WAIT_MS: u64 = 1000;

pub struct Manager {
    /// Resident reconnection entries.
    pub devices: Vec<Option<Device>>,
    pub connections: [Option<Connection>; ACTIVE_CONNECTIONS],
    pub forward: Forwarder,
    pub profiles: profiles::Cache,
    /// Bytes of memory for loaded profiles, or `None` on a board without profile support.
    pub profile_budget: Option<usize>,
    pub preference: AdapterPreference,
    pub storage_ready: bool,
    pub write_uncertain: bool,
    /// A deferred save whose outcome is unknown made storage not ready; its first later success
    /// makes it ready again, unless another failure has made it not ready since.
    pub unsaved_unready: bool,
    /// A deferred save found the filesystem full, so the status reports storage full until it
    /// succeeds.
    pub unsaved_full: bool,
    /// When input last reached the forwarder.
    pub last_input: Option<u64>,
    pub radio_ready: bool,
    generation: u64,
    /// What the radio supports, whether or not a transport is enabled.
    caps: Capabilities,
    pub available_bytes: usize,
    native_limits: [usize; 2],
    pending_device: Option<u64>,
    /// Devices deleted because their saved record was lost, not yet reported.
    pub removed: Vec<u64>,
    /// Devices whose record changed without a command, such as becoming resident, not yet
    /// reported.
    pub changed: Vec<u64>,
    /// Profiles found undecodable, to delete once no connection is starting.
    pub lost_profiles: Vec<u64>,
    /// Undecodable device records startup could not remove, to remove in the background.
    pub lost_devices: Vec<u64>,
    /// A resident entry left, so another enabled device may now fit in the stack.
    pub vacated: bool,
    /// The stack's bonds may differ from the resident entries until they are synced again.
    pub bonds_pending: bool,
}
impl Default for Manager {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            connections: core::array::from_fn(|_| None),
            forward: Forwarder::default(),
            profiles: profiles::Cache::default(),
            profile_budget: None,
            preference: AdapterPreference::default(),
            storage_ready: false,
            write_uncertain: false,
            unsaved_unready: false,
            unsaved_full: false,
            last_input: None,
            radio_ready: false,
            generation: 0,
            caps: Capabilities {
                classic: false,
                ble: false,
                ble_scan_and_connect: false,
            },
            available_bytes: 0,
            native_limits: [0; 2],
            pending_device: None,
            removed: Vec::new(),
            changed: Vec::new(),
            lost_profiles: Vec::new(),
            lost_devices: Vec::new(),
            vacated: false,
            bonds_pending: false,
        }
    }
}
fn kind(transport: Transport) -> usize {
    usize::from(transport == Transport::Ble)
}
/// Every saved profile ID.
async fn profile_ids<S: RecordStore>(store: &mut S) -> Result<Vec<u64>, Error> {
    let mut ids = Vec::new();
    loop {
        let page = store
            .record_ids(
                profiles::METADATA,
                ids.last().copied().unwrap_or(0),
                SCAN_PAGE,
            )
            .await
            .map_err(|_| Error::StorageFailed)?;
        let done = page.len() < SCAN_PAGE;
        ids.try_reserve(page.len()).map_err(|_| Error::Capacity)?;
        ids.extend(page);
        if done {
            return Ok(ids);
        }
    }
}
impl Manager {
    /// Whether the board supports profiles.
    pub fn profiles_supported(&self) -> bool {
        self.profile_budget.is_some()
    }
    /// Whether a saved device belongs in the resident set, given room in the stack.
    pub fn eligible(&self, policy: &Policy) -> bool {
        self.effective(self.caps).supports(policy.peer.transport)
            && policy.enabled
            && !policy.blocked
    }
    fn resident(&self, transport: Transport) -> usize {
        self.devices
            .iter()
            .flatten()
            .filter(|d| d.peer.transport == transport && !d.retiring)
            .count()
    }
    /// Whether another device of `transport` fits in the stack.
    pub fn room(&self, transport: Transport) -> bool {
        self.resident(transport) < self.max_enabled(transport)
    }

    /// Load complete records before publishing readiness or admitting a peer.
    /// Read each document separately; never format a failed store here. A
    /// device whose record is missing, undecodable or does not match its bond
    /// is deleted; a read error fails the load and deletes nothing.
    pub async fn load<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        if self.connections.iter().any(Option::is_some) {
            return Err(Error::Busy);
        }
        self.storage_ready = false;
        self.unsaved_unready = false;
        self.write_uncertain = false;
        self.removed.clear();
        self.changed.clear();
        self.lost_profiles.clear();
        self.lost_devices.clear();
        crate::storage::open(store)
            .await
            .map_err(|_| Error::StorageFailed)?;
        self.preference = Policies { store }
            .load_adapter()
            .await
            .map_err(|_| Error::StorageFailed)?;
        self.caps = radio.capabilities();
        self.native_limits = [
            radio.bond_capacity(Transport::Classic),
            radio.bond_capacity(Transport::Ble),
        ];
        let existing = profile_ids(store).await?;
        self.clear_interfaces(store, &existing).await;
        self.devices = Vec::new();
        // Tables still held, such as ones with edits not yet saved, are newer than their files.
        self.profiles.prune();
        // Startup reads every saved policy once: it deletes lost records, removes references to
        // profiles that no longer exist, and keeps entries for the devices that fit.
        let mut peers: Vec<Peer> = Vec::new();
        let mut lost = Vec::new();
        let mut after = 0;
        loop {
            let ids = store
                .record_ids(2, after, SCAN_PAGE)
                .await
                .map_err(|_| Error::StorageFailed)?;
            for &id in &ids {
                after = id;
                let Some(bytes) = store
                    .load_owned(crate::storage::record_key(2, id))
                    .await
                    .map_err(|_| Error::StorageFailed)?
                else {
                    continue;
                };
                let Some((mut policy, bond)) = crate::bonds::decode(id, &bytes) else {
                    lost.push(id);
                    continue;
                };
                // Records load in ID order; a later record for the same peer is the corrupt one.
                if peers.contains(&policy.peer) {
                    lost.push(id);
                    continue;
                }
                peers.try_reserve(1).map_err(|_| Error::Capacity)?;
                peers.push(policy.peer);
                // A reference that cannot be removed now is skipped, and removed again at the
                // next startup.
                if policy.profiles.iter().any(|p| !existing.contains(p)) {
                    policy.profiles.retain(|p| existing.contains(p));
                    let _ = Policies { store }.save_with(&policy, &bond).await;
                }
                if self.eligible(&policy) && self.room(policy.peer.transport) {
                    self.add(&policy)?;
                }
            }
            if ids.len() < SCAN_PAGE {
                break;
            }
        }
        // A lost record that cannot be removed now stays out of the model and is removed again in
        // the background.
        for id in lost {
            if (Policies { store }).remove(id).await.is_err() {
                self.lost_devices.push(id);
            }
        }
        // The transports in use follow the saved preference. A failure leaves storage not
        // ready, so the next Ready loads and applies it again.
        self.apply_transports(radio)?;
        self.sync_bonds(store, radio).await?;
        self.pending_device = None;
        self.available_bytes = store.available().await.map_err(|_| Error::StorageFailed)?;
        self.storage_ready = true;
        Ok(())
    }
    /// Clears and disables every configuration interface whose profile no longer exists. When the
    /// change cannot be saved, it applies until the next startup removes the reference again.
    async fn clear_interfaces<S: RecordStore>(&mut self, store: &mut S, existing: &[u64]) {
        let mut preference = self.preference.clone();
        for entry in &mut preference.configuration_interfaces {
            if entry.profile.is_some_and(|id| !existing.contains(&id)) {
                entry.profile = None;
                entry.enabled = false;
            }
        }
        preference
            .configuration_interfaces
            .retain(|p| p.enabled || p.profile.is_some());
        if preference != self.preference {
            let _ = Policies { store }.save_adapter(&preference).await;
            self.preference = preference;
        }
    }
    fn add(&mut self, policy: &Policy) -> Result<usize, Error> {
        let device = Device::new(policy);
        if let Some(slot) = self.devices.iter().position(Option::is_none) {
            self.devices[slot] = Some(device);
            return Ok(slot);
        }
        self.devices.try_reserve(1).map_err(|_| Error::Capacity)?;
        self.devices.push(Some(device));
        Ok(self.devices.len() - 1)
    }
    /// Removes a resident entry without a link.
    fn drop_entry(&mut self, slot: usize) {
        if self.devices.get_mut(slot).and_then(Option::take).is_some() {
            self.vacated = true;
        }
    }
    /// Makes resident the enabled devices that now fit in the stack, lowest IDs first, and
    /// retires entries whose link has closed. Returns whether the resident set changed.
    /// A failure leaves the work to the next fill.
    pub async fn fill<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
    ) -> Result<bool, Error> {
        let result = self.fill_entries(store, radio).await;
        if result.is_err() {
            self.vacated = true;
        }
        result
    }
    async fn fill_entries<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
    ) -> Result<bool, Error> {
        let mut changed = self.retire();
        if self.has_room() {
            let mut after = 0;
            loop {
                let ids = store
                    .record_ids(2, after, SCAN_PAGE)
                    .await
                    .map_err(|_| Error::StorageFailed)?;
                for &id in &ids {
                    after = id;
                    changed |= self.fill_record(id, store).await?;
                }
                if ids.len() < SCAN_PAGE {
                    break;
                }
            }
        }
        if changed {
            self.bonds_pending = true;
            self.sync_bonds(store, radio).await?;
        }
        Ok(changed)
    }
    /// Drops the entries of retiring devices whose link has closed, the first part of a fill.
    /// Returns whether any left.
    pub fn retire(&mut self) -> bool {
        let mut changed = false;
        for slot in 0..self.devices.len() {
            if self.devices[slot]
                .as_ref()
                .is_some_and(|d| d.retiring && self.link_for(slot).is_none())
            {
                let id = self.devices[slot].as_ref().unwrap().id;
                self.drop_entry(slot);
                self.changed.push(id);
                changed = true;
            }
        }
        self.vacated = false;
        changed
    }
    /// Whether the stack has room for another device of a transport in use.
    pub fn has_room(&self) -> bool {
        Transports::ALL
            .into_iter()
            .any(|t| self.effective(self.caps).supports(t) && self.room(t))
    }
    /// Reads saved device `id` and makes it resident when it is enabled and fits. Returns whether
    /// it became resident.
    pub async fn fill_record<S: RecordStore>(
        &mut self,
        id: u64,
        store: &mut S,
    ) -> Result<bool, Error> {
        if self.find(id).is_some() {
            return Ok(false);
        }
        let policy = match (Policies { store }).load(id).await {
            Ok(policy) => policy,
            Err(crate::storage::Error::Missing | crate::storage::Error::Corrupt) => {
                return Ok(false);
            }
            Err(_) => return Err(Error::StorageFailed),
        };
        if self.eligible(&policy)
            && self.room(policy.peer.transport)
            && self.peer(policy.peer).is_none()
        {
            self.add(&policy)?;
            self.changed.push(id);
            return Ok(true);
        }
        Ok(false)
    }
    /// Applies the saved enabled transports to every transport the radio supports.
    pub fn apply_transports<B: Bluetooth>(&self, radio: &mut B) -> Result<(), Error> {
        for transport in Transports::ALL {
            if self.caps.supports(transport) {
                radio.set_transport(transport, self.preference.transports.contains(transport))?;
            }
        }
        Ok(())
    }
    /// The transports in use: those the radio supports and the adapter has enabled.
    pub fn capabilities<B: Bluetooth>(&self, radio: &B) -> Capabilities {
        self.effective(radio.capabilities())
    }
    fn effective(&self, caps: Capabilities) -> Capabilities {
        let enabled = self.preference.transports;
        Capabilities {
            classic: caps.classic && enabled.contains(Transport::Classic),
            ble: caps.ble && enabled.contains(Transport::Ble),
            ..caps
        }
    }
    /// Whether the radio supports `transport`, enabled or not.
    pub fn supports(&self, transport: Transport) -> bool {
        self.caps.supports(transport)
    }
    /// Retires the entries of devices whose transport is now disabled. Their links close first.
    pub fn retire_disabled_transports(&mut self) {
        let caps = self.effective(self.caps);
        for slot in 0..self.devices.len() {
            let Some(d) = self.devices[slot].as_mut() else {
                continue;
            };
            if !caps.supports(d.peer.transport) {
                d.retiring = true;
                self.vacated = true;
            }
        }
    }
    /// How many devices of `transport` can be enabled at once: the native bond
    /// table less the entry kept free for pairing.
    pub fn max_enabled(&self, transport: Transport) -> usize {
        self.native_limits[kind(transport)].saturating_sub(1)
    }
    /// No room to save another device.
    /// Storage stops being ready after a failure.
    pub fn fail_storage(&mut self) {
        self.storage_ready = false;
        self.unsaved_unready = false;
    }
    /// Whether free space, as last counted, leaves no room to pair another device.
    pub fn storage_full(&self) -> bool {
        self.available_bytes < crate::bonds::MAINTENANCE_BYTES + crate::bonds::PAIR_BYTES
    }
    /// The slot of resident device `id`.
    pub fn find(&self, id: u64) -> Option<usize> {
        self.devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.id == id))
    }
    pub fn peer(&self, peer: Peer) -> Option<usize> {
        self.devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.peer == peer))
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
            maps: None,
            layout: None,
            starting: None,
        });
        Ok(id)
    }
    /// `layout` is the device's saved layout, if any.
    pub fn connect<B: Bluetooth>(
        &mut self,
        device: usize,
        explicit: bool,
        deadline: u64,
        layout: Option<&Layout>,
        radio: &mut B,
    ) -> Result<Option<LinkId>, Error> {
        if !self.radio_ready {
            return Err(Error::RadioUnavailable);
        }
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        let caps = self.capabilities(radio);
        let d = self
            .devices
            .get_mut(device)
            .and_then(Option::as_mut)
            .ok_or(Error::NotFound)?;
        if !caps.supports(d.peer.transport) {
            return Err(Error::UnsupportedTransport);
        }
        if d.retiring || d.deleting {
            return Err(Error::Disabled);
        }
        if explicit {
            d.explicit_connect();
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
        let peer = d.peer;
        let id = self.allocate(peer, Some(device), deadline)?;
        if let Err(error) = radio.connect(id, peer, false, layout) {
            self.connections[id.slot as usize] = None;
            return Err(error);
        }
        self.connection_mut(id).unwrap().maps = layout.map(crate::layouts::maps);
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
        if !self.capabilities(radio).supports(peer.transport) {
            return Err(Error::UnsupportedTransport);
        }
        if self.native_limits[kind(peer.transport)] == 0 {
            return Err(Error::UnsupportedTransport);
        }
        if self.storage_full() {
            return Err(Error::StorageFull);
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
        if let Err(error) = radio.connect(id, peer, true, None) {
            self.connections[id.slot as usize] = None;
            return Err(error);
        }
        Ok(id)
    }
    /// Rebuild the native active view from committed records. Import existing
    /// entries in place so unrelated live connections keep their database index.
    /// A device whose record turns out to be lost is deleted.
    pub async fn sync_bonds<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        for peer in radio.bonds().await? {
            self.forget_stale(peer, radio).await?;
        }
        let mut slot = 0;
        while let Some(next) = self.next_resident(slot) {
            self.import(next, store, radio).await?;
            slot = next + 1;
        }
        self.bonds_pending = false;
        Ok(())
    }
    /// Removes `peer`'s bond from the stack unless a resident entry or a link uses it.
    pub async fn forget_stale<B: Bluetooth>(&self, peer: Peer, radio: &mut B) -> Result<(), Error> {
        if self
            .devices
            .iter()
            .flatten()
            .any(|d| !d.retiring && d.peer == peer)
            || self.connections.iter().flatten().any(|c| c.peer == peer)
        {
            return Ok(());
        }
        radio.forget(peer).await
    }
    /// The first slot from `slot` on that holds a resident entry that is not retiring.
    pub fn next_resident(&self, slot: usize) -> Option<usize> {
        (slot..self.devices.len()).find(|&s| self.devices[s].as_ref().is_some_and(|d| !d.retiring))
    }
    /// Loads the saved bond of the resident entry in `slot` into the stack. A device whose record
    /// turns out to be lost is deleted.
    pub async fn import<S: RecordStore, B: Bluetooth>(
        &mut self,
        slot: usize,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        let Some((id, peer)) = self.devices[slot].as_ref().map(|d| (d.id, d.peer)) else {
            return Ok(());
        };
        match crate::bonds::load(store, id).await {
            Ok(Some(bond)) if bond.valid() && bond.owner == id && bond.identity == peer => {
                radio.import_bond(&bond).await
            }
            Ok(_) | Err(crate::storage::Error::Corrupt) => self.lose(id, store, radio).await,
            Err(_) => Err(Error::StorageFailed),
        }
    }
    /// Deletes a device whose saved record was lost, as an unpair would, and
    /// queues its removal for the client.
    pub async fn lose<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u64,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        let mut peer = None;
        if let Some(slot) = self.find(id) {
            if let Some(link) = self.link_for(slot) {
                self.forward.remove(link.slot as usize);
                radio.disconnect(link);
                if let Some(c) = self.connection_mut(link) {
                    c.device = None;
                    c.closing = true;
                }
            }
            peer = self.devices[slot].as_ref().map(|d| d.peer);
            self.drop_entry(slot);
        }
        // The device leaves the model whether or not cleanup succeeds; a record left on flash is
        // deleted again at the next startup.
        let removal = Policies { store }.remove(id).await;
        let result = match (removal, peer) {
            (Err(_), _) => Err(Error::StorageFailed),
            (Ok(()), Some(peer)) if radio.capabilities().supports(peer.transport) => {
                radio.forget(peer).await
            }
            (Ok(()), _) => Ok(()),
        };
        self.removed.push(id);
        result
    }
    /// Reserves storage and an ID for a pairing, and removes any bond the stack holds for the
    /// device. The caller syncs the stack's bonds first, so it has room for the new one.
    pub async fn prepare_pair<S: RecordStore, B: Bluetooth>(
        &mut self,
        peer: Peer,
        identity: Option<Peer>,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        if self.native_limits[kind(peer.transport)] == 0 {
            return Err(Error::Capacity);
        }
        self.available_bytes = store.available().await.map_err(|_| Error::StorageFailed)?;
        if self.available_bytes < crate::bonds::MAINTENANCE_BYTES + crate::bonds::PAIR_BYTES {
            return Err(Error::StorageFull);
        }
        // IDs are never reused, including after deleting the highest saved ID.
        let nonce = crate::storage::allocate(store, false)
            .await
            .map_err(|_| Error::StorageFailed)?;
        self.pending_device = Some(nonce);
        radio.forget(identity.unwrap_or(peer)).await?;
        Ok(())
    }
    /// Ends a pairing's reservation. The stack's bonds are synced in the background, which removes
    /// a bond the pairing left behind.
    pub async fn finish_pair<S: RecordStore>(&mut self, store: &mut S) -> Result<(), Error> {
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        self.bonds_pending = true;
        self.pending_device = None;
        self.available_bytes = store.available().await.map_err(|_| Error::StorageFailed)?;
        Ok(())
    }
    /// Whether a new BLE link may start: one connection stays free for a
    /// pairing, and BLE link setups run one at a time. A saved Classic device's
    /// link setup, which includes any outgoing page, does not hold up BLE; a
    /// pairing's setup and any closing link do.
    pub fn ble_admission(&self) -> bool {
        let mut connections = self.connections.iter().flatten();
        connections.clone().count() < ACTIVE_CONNECTIONS - 1
            && !connections.any(|c| {
                c.closing
                    || (c.runtime.is_none()
                        && (c.peer.transport == Transport::Ble || c.device.is_none()))
            })
    }
    /// The resident device an incoming link from `peer` would be admitted for.
    pub fn admits(&self, peer: Peer, now: u64) -> Option<usize> {
        self.peer(peer).filter(|&slot| {
            self.storage_ready
                && self.radio_ready
                && self.devices[slot].as_ref().is_some_and(|d| {
                    d.allow_incoming()
                        && d.state == State::Disconnected
                        && (peer.transport != Transport::Ble
                            || (d.admit_due(now) && self.ble_admission()))
                })
        })
    }
    /// `layout` is the saved layout of the device `admits` returns, if any.
    pub fn incoming<B: Bluetooth>(
        &mut self,
        attempt: u32,
        peer: Peer,
        now: u64,
        layout: Option<&Layout>,
        radio: &mut B,
    ) -> Result<Option<usize>, Error> {
        let device = self.admits(peer, now);
        let id = device.and_then(|slot| {
            self.allocate(peer, Some(slot), now.saturating_add(30_000))
                .ok()
        });
        if let Err(e) = radio.incoming(attempt, id, id.and(layout)) {
            if let Some(id) = id {
                self.connections[id.slot as usize] = None;
            }
            return Err(e);
        }
        let Some(id) = id else {
            return Ok(None);
        };
        self.connection_mut(id).unwrap().maps = layout.map(crate::layouts::maps);
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
            let hidpp = d.hidpp_enabled;
            if let Some(live) = &mut d.live {
                live.catalog.connection(false, hidpp);
            }
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
    /// Saves the bond of pairing link `id` for `saved`, the saved device with `identity`, or for a
    /// new device when there is none. Returns the device's ID and, when it is resident, its slot.
    ///
    /// The caller verifies the live pairing attempt and its deadline first. New native keys remain
    /// provisional until the device record is committed.
    pub async fn bonded<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: LinkId,
        identity: Peer,
        saved: Option<Policy>,
        name: &[u8],
        store: &mut S,
        radio: &mut B,
    ) -> Result<(u64, Option<usize>), Error> {
        let c = self.connection(id).ok_or(Error::NotConnected)?;
        if c.closing || c.device.is_some() {
            return Err(Error::NotPending);
        }
        if identity.transport != c.peer.transport
            || (identity.transport == Transport::Classic && identity != c.peer)
        {
            return Err(Error::AuthenticationFailed);
        }
        let pending = self.pending_device.ok_or(Error::NotPending)?;
        let resident = self.peer(identity);
        if let Some(slot) = resident
            && self.link_for(slot).is_some_and(|other| other != id)
        {
            return Err(Error::Busy);
        }
        // Renewing a saved device's bond keeps its ID and preferences, including whether it is
        // enabled. A blocked device is never renewed.
        let mut policy = match saved.filter(|p| p.peer == identity) {
            Some(policy) if policy.blocked => return Err(Error::Blocked),
            Some(policy) => policy,
            None => {
                let mut policy = Policy::paired(pending, identity, name);
                policy.enabled = self.room(identity.transport);
                policy
            }
        };
        let retained = policy.id != pending;
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
                self.fail_storage();
            }
            return Err(if error == crate::storage::Error::Full {
                Error::StorageFull
            } else {
                Error::StorageFailed
            });
        }
        if retained {
            // The new bond's connection discovers the device and saves its layout again.
            crate::layouts::remove(store, policy.id).await;
        }
        let slot = match resident {
            Some(slot) => Some(slot),
            None if self.eligible(&policy) && self.room(identity.transport) => {
                Some(self.add(&policy)?)
            }
            None => None,
        };
        if let Some(slot) = slot {
            let d = self.devices[slot].as_mut().unwrap();
            d.update(&policy);
            d.paused = false;
            d.error = None;
            d.state = State::Connecting;
            let c = self.connection_mut(id).unwrap();
            c.peer = identity;
            c.device = Some(slot);
        }
        if self.finish_pair(store).await.is_err() {
            self.fail_storage();
        }
        Ok((policy.id, slot))
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
        let profiles = parsed(descriptors)?;
        let d = self.devices[slot].as_mut().ok_or(Error::NotFound)?;
        if d.retiring || d.deleting {
            return Err(Error::Disabled);
        }
        let mut live = Box::new(Live::new(d.peer.transport, d.hidpp_enabled));
        let runtime = runtime(
            id,
            d.hidpp_enabled,
            &mut live,
            profiles,
            max_output,
            self.preference.host_platform,
        )?;
        d.live = Some(live);
        d.connection(State::Connected, None, now);
        let c = self.connections[id.slot as usize].as_mut().unwrap();
        c.runtime = Some(runtime);
        c.deadline = 0;
        c.starting = Some(now.saturating_add(FIRST_INPUT_WAIT_MS));
        Ok(Some(slot))
    }
    /// Moves a connected link to the layout its backend now routes reports by.
    /// The old runtime releases held input and settings work as a disconnect
    /// would, and the device stays connected. On an error the caller closes
    /// the link.
    pub fn relayout(
        &mut self,
        id: LinkId,
        descriptors: Vec<Descriptor>,
    ) -> Result<Option<usize>, Error> {
        let Some(c) = self.connection(id) else {
            return Ok(None);
        };
        let (Some(slot), Some(_), false) = (c.device, &c.runtime, c.closing) else {
            return Ok(None);
        };
        let profiles = parsed(descriptors)?;
        let c = self.connections[id.slot as usize].as_mut().unwrap();
        let d = self.devices[slot].as_mut().ok_or(Error::NotFound)?;
        let live = d.live.as_mut().ok_or(Error::NotConnected)?;
        let mut old = c.runtime.take().unwrap();
        old.disconnected(&mut live.catalog, &mut self.forward);
        old.settings.release(&mut live.catalog);
        let mut runtime = runtime(
            id,
            d.hidpp_enabled,
            live,
            profiles,
            old.max_output(),
            self.preference.host_platform,
        )?;
        runtime.continue_sequence(&old);
        // Reconnecting the catalog cleared its standard information and battery readings.
        runtime.info_refresh_pending = d.peer.transport == Transport::Ble;
        c.runtime = Some(runtime);
        Ok(Some(slot))
    }
    /// Ends a connection: releases its runtime's held input and settings work, and drops what
    /// the device kept for the connection. Returns the device slot, if any.
    pub fn disconnected(
        &mut self,
        id: LinkId,
        error: Option<Error>,
        now: u64,
    ) -> Option<Option<usize>> {
        self.connection(id)?;
        let mut c = self.connections[id.slot as usize].take().unwrap();
        if let Some(slot) = c.device {
            let d = self.devices[slot].as_mut().unwrap();
            if let (Some(link), Some(live)) = (&mut c.runtime, &mut d.live) {
                link.disconnected(&mut live.catalog, &mut self.forward);
                link.settings.release(&mut live.catalog);
            }
            // A connected device going away, such as being switched off, is not a failure. Other
            // errors, such as authentication after a virtual cable unplug, still count.
            let lost = d.state == State::Connected
                && c.error.is_none()
                && matches!(error, None | Some(Error::ConnectionFailed | Error::Timeout));
            if lost {
                d.lost(now);
            } else {
                d.connection(State::Disconnected, c.error.or(error), now);
            }
            if d.retiring {
                self.vacated = true;
            }
        }
        self.forward.remove(id.slot as usize);
        Some(c.device)
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
        let c = self.connections[report.link.slot as usize]
            .as_mut()
            .unwrap();
        let Some(live) = self.devices[slot].as_mut().and_then(|d| d.live.as_mut()) else {
            return Ok(false);
        };
        let link = c.runtime.as_mut().unwrap();
        let changed = link.input(
            report.service,
            report.report_id,
            report.payload(),
            &mut live.catalog,
            &mut self.forward,
            now,
        )?;
        // The first-input wait ends with input that reached the forwarder.
        if link.take_input_forwarded() {
            c.starting = None;
            self.last_input = Some(now);
        }
        Ok(changed)
    }
    /// Whether a connected link has forwarded no input yet and its wait for it has not passed.
    /// Background storage work waits meanwhile.
    pub fn starting(&self, now: u64) -> bool {
        self.connections.iter().flatten().any(|c| {
            !c.closing && c.runtime.is_some() && c.starting.is_some_and(|until| now < until)
        })
    }
    /// Whether resident device `id`'s connection is starting and its policy has not been read.
    /// Reading its record waits until then.
    pub fn waiting_for_input(&self, id: u64, now: u64) -> bool {
        self.find(id).is_some_and(|slot| {
            self.devices[slot]
                .as_ref()
                .and_then(|d| d.live.as_ref())
                .is_some_and(|live| live.policy.is_none())
                && self.connections.iter().flatten().any(|c| {
                    c.device == Some(slot)
                        && !c.closing
                        && c.runtime.is_some()
                        && c.starting.is_some_and(|until| now < until)
                })
        })
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
        let Some(live) = self.devices[slot].as_mut().and_then(|d| d.live.as_mut()) else {
            return Ok(None);
        };
        let mut changed = link.poll(&mut live.catalog, &mut self.forward, now)?;
        if link.info_refresh_pending && radio.refresh_info(c.id).is_ok() {
            link.info_refresh_pending = false;
        }
        if radio.can_write(c.id)
            && let Some(out) = link.output(leds, now)?
        {
            let id = out.id;
            if let Err(error) = radio.write(id, out.service, out.kind, out.report_id, out.payload) {
                changed |= link.output_complete(
                    id,
                    Err(error),
                    &mut live.catalog,
                    &mut self.forward,
                    now,
                )?;
            }
        }
        if radio.can_write(c.id)
            && let Some(read) = link.report_read(&live.catalog, now)
            && let Err(e) = radio.read(read.id, read.service, read.kind, read.report_id)
        {
            link.report_read_complete(read.id, read.kind, Err(e), &mut live.catalog, now);
        }
        changed |= live.update_warnings(&link.warnings)?;
        Ok(changed.then_some(slot))
    }
    pub fn written(
        &mut self,
        id: crate::link::WriteId,
        result: Result<(), Error>,
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
        let Some(live) = self.devices[slot].as_mut().and_then(|d| d.live.as_mut()) else {
            return Ok(None);
        };
        let mut changed =
            link.output_complete(id, result, &mut live.catalog, &mut self.forward, now)?;
        changed |= live.update_warnings(&link.warnings)?;
        Ok(changed.then_some(slot))
    }
    /// Reads a connected device's saved policy and preferences once its connection has forwarded
    /// input, or once it has waited `FIRST_INPUT_WAIT_MS` for it. Returns the slot of a device it
    /// read and, once its descriptor reported roles, those roles and whether its record holds
    /// others, for the caller to save; the read policy holds them already. On an error, returns the
    /// ID of the device it failed on: `NotFound` when its record is lost, and `StorageFailed` when
    /// the read failed, which is tried again after a backoff.
    pub async fn hydrate<S: RecordStore>(
        &mut self,
        store: &mut S,
        now: u64,
    ) -> Result<Option<(usize, Option<(crate::devices::Roles, bool)>)>, (u64, Error)> {
        let Some((slot, id)) = self
            .connections
            .iter()
            .flatten()
            .filter(|c| {
                !c.closing && c.runtime.is_some() && c.starting.is_none_or(|until| now >= until)
            })
            .filter_map(|c| c.device)
            .find_map(|slot| {
                let d = self.devices[slot].as_ref()?;
                d.live
                    .as_ref()
                    .filter(|live| live.policy.is_none() && live.hydrate_retry.due(now))
                    .map(|_| (slot, d.id))
            })
        else {
            return Ok(None);
        };
        let failed = |manager: &mut Self| {
            if let Some(live) = manager.devices[slot].as_mut().and_then(|d| d.live.as_mut()) {
                live.hydrate_retry.failed(now);
            }
            Err((id, Error::StorageFailed))
        };
        let mut policy = match (Policies { store }).load(id).await {
            Ok(policy) => policy,
            Err(crate::storage::Error::Missing | crate::storage::Error::Corrupt) => {
                return Err((id, Error::NotFound));
            }
            Err(_) => return failed(self),
        };
        let preferences = match (Preferences { store, device: id }).load_all().await {
            Ok(p) => p,
            Err(crate::storage::Error::Corrupt) => {
                // Unusable preferences are removed; the device keeps its own values.
                let _ = store.remove(crate::storage::record_key(4, id)).await;
                Vec::new()
            }
            Err(_) => return failed(self),
        };
        let d = self.devices[slot].as_mut().unwrap();
        let live = d.live.as_mut().unwrap();
        if live.catalog.restore_preferences(preferences).is_err() {
            let _ = store.remove(crate::storage::record_key(4, id)).await;
        }
        live.catalog.connection(true, d.hidpp_enabled);
        let roles = (live.roles != 0).then(|| {
            let changed = policy.roles.0 != live.roles;
            policy.roles.0 = live.roles;
            (policy.roles, changed)
        });
        live.policy = Some(policy);
        Ok(Some((slot, roles)))
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
            let live = d.live.as_ref()?;
            let saved = live.policy.as_ref()?;
            // A requested settings job finishes before setup reconfigures the link.
            if c.closing || c.setup_failed || !saved.setup_pending || link.settings.explicit() {
                return None;
            }
            let mut setup = live.setup;
            let mut hidpp = false;
            if !setup.hidpp
                && let Some(found) = link.hidpp_found()
            {
                setup.hidpp = true;
                hidpp = found;
            }
            if setup == live.setup {
                return None;
            }
            let mut policy = saved.clone();
            if hidpp {
                policy.set_hidpp(true);
            }
            policy.setup_pending = !setup.complete();
            Some((index, slot, policy, setup))
        })
    }
    /// Saves a device's policy, resident or not, and brings the resident set, the stack and the
    /// device's connection in line with it. Turning a connected device off or blocking it closes
    /// its link; its entry leaves once the link has closed. `bond` is the bond the device's
    /// saved record holds when the caller read it in this step; otherwise the record is read for
    /// it.
    pub async fn save_policy<S: RecordStore, B: Bluetooth>(
        &mut self,
        policy: Policy,
        bond: Option<&crate::bonds::Bond>,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        self.write_uncertain = false;
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        let slot = self.find(policy.id);
        let eligible = self.eligible(&policy);
        if eligible && slot.is_none() && !self.room(policy.peer.transport) {
            return Err(Error::Capacity);
        }
        if slot.is_some_and(|s| self.devices[s].as_ref().unwrap().deleting) {
            return Err(Error::Busy);
        }
        let saved = match bond {
            Some(bond) => Policies { store }.save_with(&policy, bond).await,
            None => Policies { store }.save(&policy).await,
        };
        saved.map_err(|error| {
            self.write_uncertain = error == crate::storage::Error::Unknown;
            if self.write_uncertain {
                self.fail_storage();
            }
            match error {
                crate::storage::Error::Full => Error::StorageFull,
                // The saved record is missing, undecodable or holds another device's bond.
                crate::storage::Error::Missing => Error::NotFound,
                _ => Error::StorageFailed,
            }
        })?;
        match slot {
            None if eligible => {
                let slot = self.add(&policy)?;
                // Only this entry is new; the stack's other bonds are as they were.
                if let Err(error) = self.import(slot, store, radio).await {
                    self.bonds_pending = true;
                    return Err(error);
                }
            }
            None => {}
            Some(slot) if !eligible => match self.link_for(slot) {
                Some(link) => {
                    self.devices[slot].as_mut().unwrap().retiring = true;
                    self.close(link, None, radio);
                }
                None => {
                    let peer = self.devices[slot].as_ref().unwrap().peer;
                    self.drop_entry(slot);
                    if radio.capabilities().supports(peer.transport) {
                        radio.forget(peer).await?;
                    }
                }
            },
            Some(slot) => {
                let d = self.devices[slot].as_mut().unwrap();
                let hidpp = d.hidpp_enabled;
                let layers = d.layers.clone();
                // Enabled again before its link closed: the entry stays.
                d.retiring = false;
                d.update(&policy);
                if hidpp != d.hidpp_enabled
                    && let Some(link) = self.link_for(slot)
                {
                    let platform = self.preference.host_platform;
                    let d = self.devices[slot].as_mut().unwrap();
                    if let Some(c) = self.connections[link.slot as usize]
                        .as_mut()
                        .filter(|c| c.id == link)
                        && let Some(runtime) = &mut c.runtime
                        && let Some(live) = &mut d.live
                    {
                        let was_vendor = live.catalog.info.battery.vendor();
                        live.catalog.connection(true, d.hidpp_enabled);
                        // Subscribed BAS values may not notify again until the battery changes.
                        runtime.info_refresh_pending = d.peer.transport == Transport::Ble
                            && was_vendor
                            && !live.catalog.info.battery.vendor();
                        runtime.reconfigure(d.hidpp_enabled, platform, &mut live.catalog);
                    }
                }
                if layers != policy.profiles {
                    self.load_profiles(slot, store).await;
                }
            }
        }
        Ok(())
    }
    /// Loads a connected device's layers, all or none, within the profile memory budget, and
    /// applies them to its input. When they cannot be loaded, its input passes through unchanged
    /// and its `profile_error` says why. Returns whether the device's state changed.
    pub async fn load_profiles<S: RecordStore>(&mut self, slot: usize, store: &mut S) -> bool {
        let Some(budget) = self.profile_budget else {
            return false;
        };
        let Some(link) = self.link_for(slot) else {
            return false;
        };
        if self
            .connection(link)
            .is_none_or(|c| c.closing || c.runtime.is_none())
        {
            return false;
        }
        let source = link.slot as usize;
        let d = self.devices[slot].as_ref().unwrap();
        let ids: Vec<u64> = d
            .layers
            .iter()
            .copied()
            .take(profiles::MAX_LAYERS)
            .collect();
        // The forwarder's own references, so a profile only this device holds counts as released.
        let releasing = self.forward.layers(source);
        let result = self.profiles.load(store, &ids, budget, releasing).await;
        let (layers, error) = match result {
            Ok(layers) => (layers, None),
            Err(LoadError::Capacity) => (Vec::new(), Some(Error::Capacity)),
            Err(LoadError::Storage) => (Vec::new(), Some(Error::StorageFailed)),
            Err(LoadError::Lost(id)) => {
                if !self.lost_profiles.contains(&id) {
                    self.lost_profiles.push(id);
                }
                (Vec::new(), Some(Error::StorageFailed))
            }
        };
        // Preparing allocates press-time state first, so a failure leaves the old layers in use.
        let Ok(update) = self.forward.prepare_maps(source, layers) else {
            return false;
        };
        self.forward.apply_maps(update);
        let live = self.devices[slot].as_mut().unwrap().live.as_mut().unwrap();
        let changed = live.profile_error != error;
        live.profile_error = error;
        live.profile_retry = None;
        changed
    }
    /// Retries loading the layers of one connected device whose profiles are not loaded: one that
    /// did not fit, when memory may have been released, or one that could not be read, once its
    /// backoff has passed. Only slots from `from` on are considered. Returns the slot it tried
    /// and whether its state changed.
    pub async fn retry_profiles<S: RecordStore>(
        &mut self,
        store: &mut S,
        released: bool,
        from: usize,
        now: u64,
    ) -> Option<(usize, bool)> {
        use crate::devices::{RETRY_DELAY_MAX_MS, RETRY_DELAY_MS};
        for slot in from..self.devices.len() {
            let Some(live) = self.devices[slot].as_mut().and_then(|d| d.live.as_mut()) else {
                continue;
            };
            let (due, next) = match (live.profile_error, live.profile_retry) {
                (Some(Error::Capacity), _) => (released, None),
                (Some(_), None) => {
                    live.profile_retry =
                        Some((now.saturating_add(RETRY_DELAY_MS.into()), RETRY_DELAY_MS));
                    (false, None)
                }
                (Some(_), Some((at, delay))) if now >= at => {
                    (true, Some(delay.saturating_mul(2).min(RETRY_DELAY_MAX_MS)))
                }
                _ => (false, None),
            };
            if !due {
                continue;
            }
            let changed = self.load_profiles(slot, store).await;
            if let Some(delay) = next
                && let Some(live) = self.devices[slot].as_mut().and_then(|d| d.live.as_mut())
                && live.profile_error.is_some_and(|e| e != Error::Capacity)
            {
                live.profile_retry = Some((now.saturating_add(delay.into()), delay));
            }
            return Some((slot, changed));
        }
        None
    }
    /// Saves the adapter preference. On a platform change, every ready
    /// HID++-enabled connection reconfigures for the new platform. The caller
    /// applies a transport change.
    pub async fn adapter<S: RecordStore>(
        &mut self,
        preference: AdapterPreference,
        store: &mut S,
    ) -> Result<(), Error> {
        self.write_uncertain = false;
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        let platform = preference.host_platform;
        let changed = self.preference.host_platform != platform;
        self.save_preference(preference, store).await?;
        if changed {
            for c in self.connections.iter_mut().flatten().filter(|c| !c.closing) {
                let Some(slot) = c.device else {
                    continue;
                };
                let d = self.devices[slot].as_mut().unwrap();
                if d.hidpp_enabled
                    && let Some(link) = &mut c.runtime
                    && let Some(live) = &mut d.live
                {
                    link.reconfigure(true, platform, &mut live.catalog);
                }
            }
        }
        Ok(())
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
                    self.fail_storage();
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
        let preference = AdapterPreference {
            host_platform: platform,
            ..self.preference.clone()
        };
        self.adapter(preference, store).await
    }
    /// Deletes a saved device that has no link: its bond in the stack, its resident entry and its
    /// files.
    pub async fn unpair<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u64,
        peer: Peer,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        self.write_uncertain = false;
        if !self.storage_ready {
            return Err(Error::StorageFailed);
        }
        let slot = self.find(id);
        if slot.is_some_and(|slot| self.link_for(slot).is_some()) {
            return Err(Error::Busy);
        }
        if let Some(slot) = slot {
            self.devices[slot].as_mut().unwrap().deleting = true;
        }
        if radio.capabilities().supports(peer.transport)
            && let Err(error) = radio.forget(peer).await
        {
            // Nothing was saved; the device stays as it was, and its bond is loaded again if the
            // stack lost it.
            if let Some(slot) = slot {
                self.devices[slot].as_mut().unwrap().deleting = false;
            }
            let _ = self.sync_bonds(store, radio).await;
            return Err(error);
        }
        if let Err(error) = (Policies { store }).remove(id).await {
            self.write_uncertain = error == crate::storage::Error::Unknown;
            if self.write_uncertain {
                self.fail_storage();
            } else {
                if let Some(slot) = slot {
                    self.devices[slot].as_mut().unwrap().deleting = false;
                }
                if self.sync_bonds(store, radio).await.is_err() {
                    self.fail_storage();
                }
            }
            return Err(if error == crate::storage::Error::Full {
                Error::StorageFull
            } else {
                Error::StorageFailed
            });
        }
        if let Some(slot) = slot {
            self.drop_entry(slot);
        }
        // Deletion is committed. Reclamation failure cannot turn it into a failed removal.
        match store.available().await {
            Ok(bytes) => self.available_bytes = bytes,
            Err(_) => {
                self.available_bytes = 0;
                self.fail_storage();
            }
        }
        Ok(())
    }
}

fn parsed(descriptors: Vec<Descriptor>) -> Result<Vec<Profile>, Error> {
    descriptors
        .into_iter()
        .map(|d| Profile::from_map(d.service, d.map))
        .collect()
}
/// Builds a link's runtime and records the roles, kind and warnings it reports.
fn runtime(
    id: LinkId,
    hidpp_enabled: bool,
    live: &mut Live,
    profiles: Vec<Profile>,
    max_output: usize,
    platform: HostPlatform,
) -> Result<Box<Link>, Error> {
    let runtime = Link::new(
        id,
        profiles,
        max_output,
        hidpp_enabled,
        platform,
        &mut live.catalog,
    )?;
    live.roles = runtime.roles;
    live.catalog.info.kind_hint(
        match (
            live.roles & crate::hid::KEYBOARD != 0,
            live.roles & crate::hid::MOUSE != 0,
        ) {
            (true, true) => crate::model::link::DeviceKind::KeyboardMouse,
            (true, false) => crate::model::link::DeviceKind::Keyboard,
            (false, true) => crate::model::link::DeviceKind::Mouse,
            _ => crate::model::link::DeviceKind::Unknown,
        },
    );
    live.update_warnings(&runtime.warnings)?;
    Ok(Box::new(runtime))
}
