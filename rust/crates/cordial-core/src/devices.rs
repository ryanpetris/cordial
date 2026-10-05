//! Saved device policies, the resident reconnection entries of enabled devices and the adapter
//! preference.
use crate::model::{
    errors::ErrorCode,
    identifiers::{ConnectionState, HostPlatform, Transport},
};
use crate::{
    interfaces::InterfacePreference,
    settings::Catalog,
    storage::{self, RecordStore, record_key},
};
use alloc::{boxed::Box, string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// The first retry delay after a failure, doubling per further failure up to
/// `RETRY_DELAY_MAX_MS`.
pub const RETRY_DELAY_MS: u32 = 2000;
pub const RETRY_DELAY_MAX_MS: u32 = 300_000;

/// When failed background storage work runs again: `RETRY_DELAY_MS` after the first failure,
/// doubling per further failure up to `RETRY_DELAY_MAX_MS`. Work is never given up.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Backoff {
    at: u64,
    delay: u32,
}
impl Backoff {
    /// Whether the work may run at `now`.
    pub fn due(&self, now: u64) -> bool {
        now >= self.at
    }
    /// Records a failure at `now`.
    pub fn failed(&mut self, now: u64) {
        self.delay = if self.delay == 0 {
            RETRY_DELAY_MS
        } else {
            self.delay.saturating_mul(2).min(RETRY_DELAY_MAX_MS)
        };
        self.at = now.saturating_add(self.delay.into());
    }
    /// Records a success, so the next failure starts from the first delay.
    pub fn succeeded(&mut self) {
        *self = Self::default();
    }
}
/// A connected device lost this soon after connecting has dropped rapidly.
pub const RAPID_DROP_MS: u64 = 1000;
/// Consecutive rapid drops that are still readmitted at once.
pub const RAPID_DROPS_ADMITTED: u32 = 2;
/// The admission delay after the first rapid drop beyond those, doubling per
/// further rapid drop up to `RAPID_DROP_DELAY_MAX_MS`.
pub const RAPID_DROP_DELAY_MS: u64 = 1000;
pub const RAPID_DROP_DELAY_MAX_MS: u64 = 5000;
/// Live Bluetooth connections, shared across transports. Records are allocated on demand.
pub const ACTIVE_CONNECTIONS: usize = 4;
pub const SCAN_CANDIDATES: usize = 32;
/// Saved devices returned by one listing page.
pub const PAGE_SIZE: usize = 8;

/// A resolved bonded identity, or a transport-specific discovery address.
/// Backends resolve private BLE addresses before matching a saved policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub address: [u8; 6],
    pub random: bool,
    pub transport: Transport,
}

/// The integrations a device can have a saved preference for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationKind {
    Hidpp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedIntegration {
    pub kind: IntegrationKind,
    pub enabled: bool,
}

/// The input roles of a device's HID descriptor, as `hid` role bits, saved as their names.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Roles(pub u8);
impl Roles {
    const NAMES: [(u8, &'static str); 4] = [
        (crate::hid::KEYBOARD, "keyboard"),
        (crate::hid::MOUSE, "mouse"),
        (crate::hid::CONSUMER, "consumer_control"),
        (crate::hid::SYSTEM, "system_control"),
    ];
}
impl Serialize for Roles {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(None)?;
        for (bit, name) in Self::NAMES {
            if self.0 & bit != 0 {
                seq.serialize_element(name)?;
            }
        }
        seq.end()
    }
}
impl<'de> Deserialize<'de> for Roles {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut roles = 0;
        for name in Vec::<String>::deserialize(deserializer)? {
            let (bit, _) = Self::NAMES
                .into_iter()
                .find(|(_, n)| *n == name)
                .ok_or_else(|| serde::de::Error::custom("unknown role"))?;
            roles |= bit;
        }
        Ok(Self(roles))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub id: u64,
    pub peer: Peer,
    pub name: Box<str>,
    pub trusted: bool,
    pub blocked: bool,
    pub enabled: bool,
    /// Set from pairing until the device's first-connection setup completes;
    /// saved only while set.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub setup_pending: bool,
    pub integrations: Vec<SavedIntegration>,
    #[serde(default, skip_serializing_if = "is_empty_roles")]
    pub roles: Roles,
    /// The device's profile layers, applied in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<u64>,
    #[serde(skip)]
    pub bond: u64,
}
fn is_empty_roles(roles: &Roles) -> bool {
    roles.0 == 0
}
impl Policy {
    pub fn paired(id: u64, peer: Peer, name: &[u8]) -> Self {
        Self {
            id,
            peer,
            name: display_name(name),
            trusted: true,
            blocked: false,
            enabled: true,
            setup_pending: true,
            integrations: Vec::new(),
            roles: Roles::default(),
            profiles: Vec::new(),
            bond: 0,
        }
    }
    pub fn valid(&self) -> bool {
        self.id != 0
            && self.id <= u64::from(u32::MAX)
            && self.name.len() <= 128
            && !self.name.bytes().any(|b| b < 0x20 || b == 0x7f)
            && (self.peer.transport != Transport::Classic || !self.peer.random)
            && self
                .integrations
                .iter()
                .enumerate()
                .all(|(i, a)| !self.integrations[..i].iter().any(|b| b.kind == a.kind))
            && self.profiles.iter().all(|&id| id != 0)
    }
    pub fn hidpp_enabled(&self) -> bool {
        self.integrations
            .iter()
            .any(|i| i.kind == IntegrationKind::Hidpp && i.enabled)
    }
    /// Saves the HID++ preference, adding the integration's entry when it has none.
    pub fn set_hidpp(&mut self, enabled: bool) {
        match self
            .integrations
            .iter_mut()
            .find(|i| i.kind == IntegrationKind::Hidpp)
        {
            Some(entry) => entry.enabled = enabled,
            None => self.integrations.push(SavedIntegration {
                kind: IntegrationKind::Hidpp,
                enabled,
            }),
        }
    }
}
pub fn display_name(bytes: &[u8]) -> Box<str> {
    let mut name = String::new();
    // Lossy UTF-8 conversion preserves valid characters; terminal controls are
    // escaped by the host too, but never enter saved adapter names.
    for ch in String::from_utf8_lossy(bytes).chars() {
        let ch = if ch == '\u{fffd}' {
            '?'
        } else if ch.is_ascii_control() {
            ' '
        } else {
            ch
        };
        if name.len() + ch.len_utf8() > 128 {
            break;
        }
        name.push(ch);
    }
    name.into_boxed_str()
}

/// The first-connection setup steps a device has settled since the adapter
/// started. Its saved `setup_pending` clears once every step is settled; until
/// then, each connection resumes the steps that are not.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Setup {
    /// HID++ detection, which turns HID++ on for a device that answers as HID++
    /// 2.0, or a user choice of HID++ made first.
    pub hidpp: bool,
}
impl Setup {
    pub fn complete(self) -> bool {
        self.hidpp
    }
}

/// What a connected device keeps for its connection. Dropped when the connection ends.
pub struct Live {
    /// The device's saved policy, read from flash once the connection forwards input, or once
    /// `manager::FIRST_INPUT_WAIT_MS` have passed without input.
    pub policy: Option<Policy>,
    pub catalog: Catalog,
    pub roles: u8,
    pub warnings: Vec<crate::model::errors::DeviceWarning>,
    pub warnings_changed: bool,
    pub setup: Setup,
    /// Why the device's profiles are not loaded, while they are not.
    pub profile_error: Option<ErrorCode>,
    /// When loading profiles that could not be read is tried again, and the delay after that.
    pub profile_retry: Option<(u64, u32)>,
    /// When reading the policy and preferences runs again after a failed read.
    pub hydrate_retry: Backoff,
}
impl Live {
    pub fn new(transport: Transport, hidpp_enabled: bool) -> Self {
        let mut catalog = Catalog::default();
        catalog.info.battery.configure(transport, false);
        catalog.connection(true, hidpp_enabled);
        Self {
            policy: None,
            catalog,
            roles: 0,
            warnings: Vec::new(),
            warnings_changed: false,
            setup: Setup::default(),
            profile_error: None,
            profile_retry: None,
            hydrate_retry: Backoff::default(),
        }
    }
    pub fn update_warnings(
        &mut self,
        warnings: &[crate::model::errors::DeviceWarning],
    ) -> Result<bool, ErrorCode> {
        if self.warnings == warnings {
            return Ok(false);
        }
        self.warnings
            .try_reserve_exact(warnings.len().saturating_sub(self.warnings.len()))
            .map_err(|_| ErrorCode::Capacity)?;
        self.warnings.clear();
        self.warnings.extend_from_slice(warnings);
        self.warnings_changed = true;
        Ok(true)
    }
}

/// The resident reconnection entry of an enabled device the Bluetooth stack has room for: what
/// accepting and making its connections needs, and its state while connected.
pub struct Device {
    pub id: u64,
    pub peer: Peer,
    pub trusted: bool,
    pub hidpp_enabled: bool,
    /// The device's profile layers, applied in order.
    pub layers: Vec<u64>,
    pub state: ConnectionState,
    pub paused: bool,
    pub error: Option<ErrorCode>,
    /// The entry leaves the resident set once its link has closed, because the device was
    /// disabled, blocked or its transport turned off.
    pub retiring: bool,
    /// An unpair is waiting for the device's link to close.
    pub deleting: bool,
    pub live: Option<Box<Live>>,
    /// When the device may next reconnect through the BLE accept list or an
    /// incoming link.
    admit_at: u64,
    /// When background paging may next start an outgoing Classic connection.
    page_at: u64,
    /// When the last connection ended.
    ended_at: u64,
    retry_delay: u32,
    /// When the current connection reached Connected.
    connected_at: Option<u64>,
    /// Consecutive clean losses within `RAPID_DROP_MS` of connecting.
    rapid_drops: u32,
}
impl Device {
    pub fn new(policy: &Policy) -> Self {
        Self {
            id: policy.id,
            peer: policy.peer,
            trusted: policy.trusted,
            hidpp_enabled: policy.hidpp_enabled(),
            layers: policy.profiles.clone(),
            state: ConnectionState::Disconnected,
            paused: false,
            error: None,
            retiring: false,
            deleting: false,
            live: None,
            admit_at: 0,
            page_at: 0,
            ended_at: 0,
            retry_delay: 0,
            connected_at: None,
            rapid_drops: 0,
        }
    }
    /// Takes the saved fields a reconnection entry keeps from `policy`. A connection whose policy
    /// has been read keeps the new one; a connection that has not read it yet reads the saved
    /// policy, with its preferences, once it forwards input.
    pub fn update(&mut self, policy: &Policy) {
        self.trusted = policy.trusted;
        self.hidpp_enabled = policy.hidpp_enabled();
        self.layers.clone_from(&policy.profiles);
        if let Some(saved) = self.live.as_mut().and_then(|l| l.policy.as_mut()) {
            saved.clone_from(policy);
        }
    }
    pub fn allow_incoming(&self) -> bool {
        !self.retiring && !self.deleting && self.trusted && !self.paused
    }
    fn reconnectable(&self) -> bool {
        self.allow_incoming() && self.state == ConnectionState::Disconnected
    }
    /// Whether the device may reconnect through the BLE accept list or an incoming link.
    pub fn admit_due(&self, now: u64) -> bool {
        self.reconnectable() && now >= self.admit_at
    }
    /// Whether background paging may start an outgoing Classic connection.
    pub fn page_due(&self, now: u64) -> bool {
        self.reconnectable() && now >= self.page_at
    }
    pub fn watch_for_return(&self) -> bool {
        self.peer.transport == Transport::Ble && self.reconnectable()
    }
    /// A BLE device seen advertising may reconnect early, but not sooner than
    /// the first retry delay after its last connection ended.
    pub fn seen(&mut self, now: u64) {
        if self.watch_for_return() {
            self.admit_at = self
                .admit_at
                .min(now.max(self.ended_at.saturating_add(RETRY_DELAY_MS.into())));
        }
    }
    /// Every failure backs off; only the user's own choices stop automatic
    /// reconnection, since anyone can claim a device's address and fail a
    /// connection on purpose.
    pub fn connection(&mut self, state: ConnectionState, error: Option<ErrorCode>, now: u64) {
        self.state = state;
        if let Some(live) = &mut self.live {
            live.catalog
                .connection(state == ConnectionState::Connected, self.hidpp_enabled);
        }
        if error.is_some() || state == ConnectionState::Connected {
            self.error = error;
        }
        if state == ConnectionState::Connected {
            self.connected_at = Some(now);
            self.retry_delay = 0;
        }
        if state == ConnectionState::Disconnected {
            self.live = None;
            if !self.rapid(now) && self.connected_at.is_some() {
                self.rapid_drops = 0;
            }
            self.connected_at = None;
            self.ended_at = now;
            self.retry_delay = if self.retry_delay == 0 {
                RETRY_DELAY_MS
            } else {
                (self.retry_delay * 2).min(RETRY_DELAY_MAX_MS)
            };
            let retry_at = now.saturating_add(self.retry_delay.into());
            self.admit_at = retry_at;
            self.page_at = retry_at;
        }
    }
    fn rapid(&self, now: u64) -> bool {
        self.connected_at
            .is_some_and(|at| now.saturating_sub(at) < RAPID_DROP_MS)
    }
    /// A connected device went away without a failure, such as by sleeping or
    /// being switched off. It may reconnect by itself at once, unless it keeps
    /// dropping right after connecting. Background paging waits for the retry
    /// delay.
    pub fn lost(&mut self, now: u64) {
        if self.rapid(now) {
            self.rapid_drops = self.rapid_drops.saturating_add(1);
        }
        self.connection(ConnectionState::Disconnected, None, now);
        let delay = match self.rapid_drops.checked_sub(RAPID_DROPS_ADMITTED + 1) {
            None => 0,
            Some(extra) => (RAPID_DROP_DELAY_MS << extra.min(8)).min(RAPID_DROP_DELAY_MAX_MS),
        };
        self.admit_at = now.saturating_add(delay);
    }
    /// Holds background paging until `at`.
    pub fn defer_page(&mut self, at: u64) {
        self.page_at = self.page_at.max(at);
    }
    pub fn explicit_connect(&mut self) {
        self.paused = false;
        self.admit_at = 0;
        self.page_at = 0;
        self.retry_delay = 0;
        self.rapid_drops = 0;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterPreference {
    pub name: Option<alloc::string::String>,
    pub host_platform: HostPlatform,
    /// The enabled transports. BLE alone when absent.
    #[serde(default)]
    pub transports: Transports,
    /// Saved preferences of each configuration interface that has any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configuration_interfaces: Vec<InterfacePreference>,
}

/// A set of transports, saved as the list of their names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transports(u8);
impl Transports {
    pub const ALL: [Transport; 2] = [Transport::Classic, Transport::Ble];
    pub const NONE: Self = Self(0);
    fn bit(transport: Transport) -> u8 {
        match transport {
            Transport::Classic => 1,
            Transport::Ble => 2,
        }
    }
    pub fn contains(self, transport: Transport) -> bool {
        self.0 & Self::bit(transport) != 0
    }
    pub fn set(&mut self, transport: Transport, member: bool) {
        if member {
            self.0 |= Self::bit(transport);
        } else {
            self.0 &= !Self::bit(transport);
        }
    }
}
impl Default for Transports {
    fn default() -> Self {
        Self(Self::bit(Transport::Ble))
    }
}
impl Serialize for Transports {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(None)?;
        for transport in Self::ALL.into_iter().filter(|t| self.contains(*t)) {
            seq.serialize_element(&transport)?;
        }
        seq.end()
    }
}
impl<'de> Deserialize<'de> for Transports {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut set = Self::NONE;
        for transport in Vec::<Transport>::deserialize(deserializer)? {
            set.set(transport, true);
        }
        Ok(set)
    }
}

pub struct Policies<'a, S> {
    pub store: &'a mut S,
}
impl<S: RecordStore> Policies<'_, S> {
    pub async fn load_adapter(&mut self) -> Result<AdapterPreference, storage::Error> {
        match self.store.load_owned(record_key(1, 0)).await? {
            Some(bytes) => {
                let value: AdapterPreference =
                    serde_json::from_slice(&bytes).map_err(|_| storage::Error::Corrupt)?;
                if value
                    .name
                    .as_deref()
                    .is_some_and(|name| crate::model::adapter_name(name) != Some(name))
                    || !crate::interfaces::valid(&value.configuration_interfaces)
                {
                    return Err(storage::Error::Corrupt);
                }
                Ok(value)
            }
            None => Ok(AdapterPreference::default()),
        }
    }
    pub async fn save_adapter(&mut self, value: &AdapterPreference) -> Result<(), storage::Error> {
        self.store
            .save(record_key(1, 0), &storage::json(&value)?)
            .await
    }
    /// The saved policy of device `id`. `Missing` when the device does not exist; `Corrupt`
    /// when its record is undecodable or does not match its ID.
    pub async fn load(&mut self, id: u64) -> Result<Policy, storage::Error> {
        let bytes = self
            .store
            .load_owned(record_key(2, id))
            .await?
            .ok_or(storage::Error::Missing)?;
        crate::codec::read_policy(id, &bytes)
    }
    pub async fn save(&mut self, policy: &Policy) -> Result<(), storage::Error> {
        if !policy.valid() {
            return Err(storage::Error::Corrupt);
        }
        // A bond that is gone, unreadable or another device's means the record was lost.
        let bond = match crate::bonds::load(self.store, policy.id).await {
            Ok(Some(bond))
                if bond.valid() && bond.owner == policy.id && bond.identity == policy.peer =>
            {
                bond
            }
            Ok(_) | Err(storage::Error::Corrupt) => return Err(storage::Error::Missing),
            // The store's own errors are read and write failures, never a lost record.
            Err(storage::Error::Missing) => return Err(storage::Error::Io),
            Err(error) => return Err(error),
        };
        match crate::bonds::commit(self.store, policy, &bond).await {
            Err(storage::Error::Missing) => Err(storage::Error::Io),
            result => result,
        }
    }
    /// The device file is the deletion commit. Leftover preferences and layout
    /// are inactive and cleanup can be retried without restoring a deleted device.
    pub async fn remove(&mut self, device: u64) -> Result<(), storage::Error> {
        self.store.remove(record_key(2, device)).await?;
        let _ = self.store.remove(record_key(4, device)).await;
        crate::layouts::remove(self.store, device).await;
        Ok(())
    }
}
