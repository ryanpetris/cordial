use crate::{
    settings::Catalog,
    storage::{self, RecordStore, record_key},
};
use alloc::{boxed::Box, format, string::String, vec::Vec};
use cordial_protocol::{
    errors::ErrorCode,
    identifiers::{ConnectionState, DeviceId, HostPlatform, PairingState, Transport},
};
use serde::{Deserialize, Serialize};

/// Wire enumeration bound; records are allocated on demand.
pub const ACTIVE_CONNECTIONS: usize = 4;
pub const SCAN_CANDIDATES: usize = 32;
pub const PENDING_REQUESTS: usize = 4;

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
            hidpp_enabled: true,
            enabled: true,
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

pub struct Device {
    pub pairing_state: PairingState,
    pub effective_enabled: bool,
    pub transport_supported: bool,
    pub validation_error: Option<cordial_protocol::errors::ValidationError>,
    pub policy: Policy,
    pub catalog: Catalog,
    pub state: ConnectionState,
    pub roles: u8,
    pub warnings: u8,
    pub paused: bool,
    pub error: Option<ErrorCode>,
    pub settings_revision: u64,
    retry_at: u64,
    last_failure: u64,
    retry_delay: u32,
}
impl Device {
    pub fn new(policy: Policy) -> Self {
        let mut catalog = Catalog::default();
        catalog.info.battery.configure(policy.peer.transport, false);
        catalog.connection(false, policy.hidpp_enabled);
        Self {
            pairing_state: PairingState::Paired,
            effective_enabled: true,
            transport_supported: true,
            validation_error: None,
            policy,
            catalog,
            state: ConnectionState::Disconnected,
            roles: 0,
            warnings: 0,
            paused: false,
            error: None,
            settings_revision: 0,
            retry_at: 0,
            last_failure: 0,
            retry_delay: 0,
        }
    }
    pub fn allow_incoming(&self) -> bool {
        self.pairing_state == PairingState::Paired
            && self.effective_enabled
            && self.policy.trusted
            && !self.policy.blocked
            && !self.paused
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
        if self.pairing_state == PairingState::NeedsPairing {
            return Err(ErrorCode::PairingRequired);
        }
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
                    .is_some_and(|name| cordial_protocol::adapter_name(name) != Some(name))
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
    pub async fn load_record(
        &mut self,
        key: storage::RecordKey,
    ) -> (Policy, Option<cordial_protocol::errors::ValidationError>) {
        use cordial_protocol::errors::ValidationError;
        let id = u64::from_be_bytes(key[1..].try_into().unwrap());
        let decoded = match self.store.load_owned(key).await {
            Ok(Some(bytes)) => {
                crate::codec::read_policy(u64::from_be_bytes(key[1..].try_into().unwrap()), &bytes)
                    .map_err(|_| ValidationError::DeviceCorrupt)
            }
            Ok(None) => Err(ValidationError::DeviceCorrupt),
            Err(_) => Err(ValidationError::ReadFailed),
        };
        match decoded {
            Ok(policy) if policy.id == id => (policy, None),
            result => (
                Policy::paired(
                    id,
                    Peer {
                        transport: Transport::Ble,
                        random: false,
                        address: [0; 6],
                    },
                    b"Unreadable device record",
                ),
                Some(result.err().unwrap_or(ValidationError::DeviceCorrupt)),
            ),
        }
    }
    /// Preserve an entry and its ID even when its payload cannot be decoded.
    pub async fn load_records(
        &mut self,
    ) -> Result<Vec<(Policy, Option<cordial_protocol::errors::ValidationError>)>, storage::Error>
    {
        use cordial_protocol::errors::ValidationError;
        let mut records: Vec<(Policy, Option<ValidationError>)> = Vec::new();
        let mut previous = None;
        while let Some(key) = self.store.next_key(previous).await? {
            previous = Some(key);
            if key[0] != 2 {
                continue;
            }
            let (policy, validation) = self.load_record(key).await;
            records
                .try_reserve(1)
                .map_err(|_| storage::Error::Unavailable)?;
            records.push((policy, validation));
        }
        records.sort_unstable_by_key(|(p, _)| p.id);
        for i in 0..records.len() {
            for j in 0..i {
                if records[i].0.peer == records[j].0.peer {
                    records[i].1 = Some(ValidationError::DeviceCorrupt);
                    records[j].1 = Some(ValidationError::DeviceCorrupt);
                }
            }
        }
        Ok(records)
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
        let bond = crate::bonds::load(self.store, policy.id)
            .await?
            .ok_or(storage::Error::Corrupt)?;
        crate::bonds::commit(self.store, policy, &bond).await
    }
    /// The device file is the deletion commit. Leftover preferences are inactive
    /// and cleanup can be retried without restoring a deleted device.
    pub async fn remove(&mut self, _slot: usize, device: u64) -> Result<(), storage::Error> {
        self.store.remove(record_key(2, device)).await?;
        let _ = self.store.remove(record_key(4, device)).await;
        Ok(())
    }
}
