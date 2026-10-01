use crate::model::{
    errors::ErrorCode,
    identifiers::{ConnectionState, DeviceId, HostPlatform, Transport},
};
use crate::{
    settings::Catalog,
    storage::{self, RecordStore, record_key},
};
use alloc::{boxed::Box, format, string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// Live Bluetooth connections, shared across transports. Records are allocated on demand.
pub const ACTIVE_CONNECTIONS: usize = 4;
pub const SCAN_CANDIDATES: usize = 32;

/// A resolved bonded identity, or a transport-specific discovery address.
/// Backends resolve private BLE addresses before matching a saved policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub address: [u8; 6],
    pub random: bool,
    pub transport: Transport,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub id: u64,
    pub peer: Peer,
    pub name: Box<str>,
    pub trusted: bool,
    pub blocked: bool,
    pub hidpp_enabled: bool,
    pub enabled: bool,
    /// Set from pairing until the device's first-connection setup completes;
    /// saved only while set.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub setup_pending: bool,
    #[serde(skip)]
    pub bond: u64,
    #[serde(skip)]
    pub deleting: bool,
}
impl Policy {
    pub fn paired(id: u64, peer: Peer, name: &[u8]) -> Self {
        Self {
            id,
            peer,
            name: display_name(name),
            trusted: true,
            blocked: false,
            hidpp_enabled: false,
            enabled: true,
            setup_pending: true,
            bond: 0,
            deleting: false,
        }
    }
    pub fn device_id(&self) -> DeviceId {
        DeviceId(format!("d_{:016x}", self.id))
    }
    pub fn valid(&self) -> bool {
        self.id != 0
            && self.name.len() <= 128
            && !self.name.bytes().any(|b| b < 0x20 || b == 0x7f)
            && (self.peer.transport != Transport::Classic || !self.peer.random)
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

pub struct Device {
    pub effective_enabled: bool,
    pub transport_supported: bool,
    pub policy: Policy,
    pub catalog: Catalog,
    pub state: ConnectionState,
    pub roles: u8,
    pub warnings: alloc::vec::Vec<crate::model::errors::DeviceWarning>,
    pub warnings_changed: bool,
    pub paused: bool,
    pub error: Option<ErrorCode>,
    pub setup: Setup,
    retry_at: u64,
    last_failure: u64,
    retry_delay: u32,
}
impl Device {
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
    pub fn new(policy: Policy) -> Self {
        let mut catalog = Catalog::default();
        catalog.info.battery.configure(policy.peer.transport, false);
        catalog.connection(false, policy.hidpp_enabled);
        Self {
            effective_enabled: true,
            transport_supported: true,
            policy,
            catalog,
            state: ConnectionState::Disconnected,
            roles: 0,
            warnings: alloc::vec::Vec::new(),
            warnings_changed: false,
            paused: false,
            error: None,
            setup: Setup::default(),
            retry_at: 0,
            last_failure: 0,
            retry_delay: 0,
        }
    }
    pub fn allow_incoming(&self) -> bool {
        self.effective_enabled && self.policy.trusted && !self.policy.blocked && !self.paused
    }
    pub fn reconnect_due(&self, now: u64) -> bool {
        self.allow_incoming() && self.state == ConnectionState::Disconnected && now >= self.retry_at
    }
    pub fn watch_for_return(&self) -> bool {
        self.policy.peer.transport == Transport::Ble
            && self.allow_incoming()
            && self.state == ConnectionState::Disconnected
            && self.retry_at != u64::MAX
    }
    pub fn seen(&mut self, now: u64) {
        if self.watch_for_return() {
            self.retry_at = self
                .retry_at
                .min(now.max(self.last_failure.saturating_add(5000)));
        }
    }
    pub fn connection(&mut self, state: ConnectionState, error: Option<ErrorCode>, now: u64) {
        self.state = state;
        self.catalog.connection(
            state == ConnectionState::Connected,
            self.policy.hidpp_enabled,
        );
        if error.is_some() || state == ConnectionState::Connected {
            self.error = error;
        }
        if state == ConnectionState::Connected {
            self.retry_delay = 0;
        }
        if state == ConnectionState::Disconnected {
            self.last_failure = now;
            self.retry_delay = if self.retry_delay == 0 {
                5000
            } else {
                (self.retry_delay * 2).min(300_000)
            };
            self.retry_at = if matches!(
                error,
                Some(
                    ErrorCode::UnsupportedHid
                        | ErrorCode::UnsupportedTransport
                        | ErrorCode::AuthenticationFailed
                )
            ) {
                u64::MAX
            } else {
                now.saturating_add(self.retry_delay.into())
            };
        }
    }
    pub fn explicit_connect(&mut self) -> Result<(), ErrorCode> {
        if self.policy.blocked {
            return Err(ErrorCode::Blocked);
        }
        self.paused = false;
        self.retry_at = 0;
        self.retry_delay = 0;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AdapterPreference {
    pub name: Option<alloc::string::String>,
    pub host_platform: HostPlatform,
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
    pub async fn load_devices(&mut self) -> Result<Vec<(usize, Policy)>, storage::Error> {
        let mut result = Vec::new();
        let mut previous = None;
        while let Some(key) = self.store.next_key(previous).await? {
            previous = Some(key);
            if key[0] != 2 {
                continue;
            }
            if let Some(bytes) = self.store.load_owned(key).await? {
                let policy: Policy = crate::codec::read_policy(
                    u64::from_be_bytes(key[1..].try_into().unwrap()),
                    &bytes,
                )?;
                if !policy.valid()
                    || result
                        .iter()
                        .any(|(_, p): &(usize, Policy)| p.id == policy.id || p.peer == policy.peer)
                {
                    return Err(storage::Error::Corrupt);
                }
                result
                    .try_reserve_exact(1)
                    .map_err(|_| storage::Error::Unavailable)?;
                if u64::from_be_bytes(key[1..].try_into().unwrap()) != policy.id {
                    return Err(storage::Error::Corrupt);
                }
                result.push((result.len(), policy));
            }
        }
        Ok(result)
    }
    pub async fn save(&mut self, _slot: usize, policy: &Policy) -> Result<(), storage::Error> {
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
    /// The device file is the deletion commit. Leftover preferences are inactive
    /// and cleanup can be retried without restoring a deleted device.
    pub async fn remove(&mut self, _slot: usize, device: u64) -> Result<(), storage::Error> {
        self.store.remove(record_key(2, device)).await?;
        let _ = self.store.remove(record_key(4, device)).await;
        Ok(())
    }
}
