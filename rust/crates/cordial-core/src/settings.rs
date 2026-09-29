use crate::compact::{FeatureEntry, Observed, Preference, Record, number, scalar};
use alloc::vec::Vec;
use alloc::{boxed::Box, rc::Rc};
use cordial_protocol::errors::ErrorCode;
use cordial_protocol::{
    hidpp::Feature,
    hidpp::{FeatureId, FeatureRevision},
    settings::{
        ObservationSource, Setting, SettingKey, SettingScope, SettingState, SettingType,
        SettingValue,
    },
};
use serde::{Deserialize, Serialize};

#[allow(async_fn_in_trait)]
pub trait PreferenceStore {
    async fn save(&mut self, value: &Preference) -> Result<(), Error>;
    async fn remove(&mut self, key: SettingKey) -> Result<(), Error>;
    async fn remove_all(&mut self) -> Result<(), Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Saved {
    SavedOnly,
    Apply,
}

#[derive(Default)]
pub struct Catalog {
    pub info: crate::info::Information,
    pub(crate) records: Vec<Record>,
    pub(crate) features: Vec<FeatureEntry>,
    pub(crate) connected: bool,
    pub(crate) enabled: bool,
    pub(crate) busy: bool,
    pub(crate) discovered: bool,
    pub(crate) catalog_changed: bool,
}
impl Catalog {
    pub fn take_changed(&mut self) -> impl Iterator<Item = &Record> {
        self.records.iter().filter(|r| r.changed.replace(false))
    }
    pub fn records(&self) -> &[Record] {
        &self.records
    }
    /// Keep the live catalog in place while a listing owns its stable snapshot.
    pub fn snapshot(&self) -> Result<Box<[Record]>, Error> {
        let mut records = Vec::new();
        records
            .try_reserve_exact(self.records.len())
            .map_err(|_| Error::Resource)?;
        records.extend(self.records.iter().cloned());
        Ok(records.into_boxed_slice())
    }
    pub fn features(&self) -> &[FeatureEntry] {
        &self.features
    }
    pub fn preferences(&self) -> impl Iterator<Item = &Preference> {
        self.records.iter().filter_map(|r| r.preference.as_deref())
    }
    pub fn connection(&mut self, connected: bool, enabled: bool) {
        self.info.connection(connected, enabled);
        if self.connected != connected {
            self.discovered = false;
            for record in &mut self.records {
                record.available = false;
            }
        }
        if !connected || (self.enabled && !enabled) {
            self.invalidate();
        }
        self.connected = connected;
        self.enabled = enabled;
        if !connected {
            self.busy = false;
        }
    }
    pub fn invalidate(&mut self) {
        self.catalog_changed = true;
        for record in &mut self.records {
            record.changed.set(true);
            record.fresh = false;
            if record.preference.is_some()
                && matches!(
                    record.state,
                    SettingState::Applied | SettingState::Applying | SettingState::ChangedOnDevice
                )
            {
                record.changed.set(true);
                record.state = SettingState::Pending;
                record.error = None;
            }
        }
    }
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }
    pub fn can_refresh(&self) -> bool {
        self.connected && !self.busy
    }
    pub fn can_apply(&self) -> bool {
        self.can_refresh() && self.enabled
    }
    pub fn restore_preferences(&mut self, saved: Vec<Preference>) -> Result<(), Error> {
        if saved.len() > MAX_SAVED {
            return Err(Error::Limit);
        }
        let mut records = Vec::new();
        records
            .try_reserve_exact(saved.len())
            .map_err(|_| Error::Resource)?;
        for pref in saved {
            if records
                .iter()
                .any(|r: &Record| r.metadata.key == pref.metadata.key)
            {
                return Err(Error::InvalidValue);
            }
            if !pref.valid() {
                return Err(Error::SettingsUnavailable);
            }
            let mut record = Record::new(pref.metadata.clone(), true);
            record.available = false;
            record.preference = Some(Rc::new(pref));
            record.changed.set(true);
            record.state = SettingState::Pending;
            records.push(record);
        }
        self.records = records;
        self.discovered = false;
        self.catalog_changed = true;
        Ok(())
    }
    pub fn replace_discovery(
        &mut self,
        mut records: Vec<Record>,
        features: Vec<Feature>,
    ) -> Result<(), Error> {
        if records.len() > MAX_RECORDS
            || features.len() > MAX_FEATURES
            || records.iter().any(|r| {
                r.metadata.choices.len() > MAX_CHOICES
                    || matches!(&r.observed, Observed::Text(s) if s.len() > MAX_TEXT_BYTES)
            })
        {
            return Err(Error::Limit);
        }
        if features
            .iter()
            .enumerate()
            .any(|(i, f)| usize::from(f.index.0) != i)
        {
            return Err(Error::InvalidValue);
        }
        let mut compact = Vec::new();
        compact
            .try_reserve_exact(features.len())
            .map_err(|_| Error::Resource)?;
        compact.extend(features.into_iter().map(|f| FeatureEntry {
            id: f.id,
            version: f.version,
            flags: f.flags,
        }));
        let features = compact;
        for (i, record) in records.iter().enumerate() {
            if records[..i]
                .iter()
                .any(|r| r.metadata.key == record.metadata.key)
            {
                return Err(Error::InvalidValue);
            }
        }
        let missing = self
            .records
            .iter()
            .filter(|r| {
                !records
                    .iter()
                    .any(|next| next.metadata.key == r.metadata.key)
            })
            .count();
        if records.len() + missing > MAX_RECORDS {
            return Err(Error::Limit);
        }
        records
            .try_reserve_exact(missing)
            .map_err(|_| Error::Resource)?;
        self.catalog_changed = true;
        for record in &mut records {
            record.available = true;
            record.preference = None;
            record.changed.set(true);
            record.state = SettingState::Unmanaged;
        }
        let merge = |mut previous: Record| {
            if let Some(record) = records
                .iter_mut()
                .find(|r| r.metadata.key == previous.metadata.key)
            {
                if let Some(preference) = previous.preference.take() {
                    let compatible = record.writable
                        && record.metadata.feature == preference.metadata.feature
                        && record.metadata.scope == preference.metadata.scope;
                    if !compatible {
                        *record = Record::new(preference.metadata.clone(), true);
                        record.available = false;
                    }
                    record.state = if compatible && record.metadata.accepts(preference.value) {
                        SettingState::Pending
                    } else {
                        SettingState::Unsupported
                    };
                    record.error = (record.state == SettingState::Unsupported)
                        .then_some(ErrorCode::UnsupportedSetting);
                    record.preference = Some(preference);
                }
            } else {
                previous.available = false;
                previous.changed.set(true);
                previous.fresh = false;
                if previous.preference.is_some() {
                    previous.state = SettingState::Unsupported;
                    previous.error = Some(ErrorCode::UnsupportedSetting);
                }
                records.push(previous);
            }
        };
        self.records.drain(..).for_each(merge);
        if self.records.capacity() >= records.len() {
            self.records.extend(records);
        } else {
            self.records = records;
        }
        if self.features.capacity() >= features.len() {
            self.features.clear();
            self.features.extend(features);
        } else {
            self.features = features;
        }
        self.discovered = true;
        Ok(())
    }
    fn editable(&self, key: SettingKey) -> Result<usize, Error> {
        if self.busy {
            return Err(Error::Busy);
        }
        if !self.discovered && self.records.is_empty() {
            return Err(Error::SettingsUnavailable);
        }
        let index = self
            .records
            .iter()
            .position(|r| r.metadata.key == key)
            .ok_or(Error::NotFound)?;
        if !self.records[index].writable {
            return Err(Error::ReadOnly);
        }
        Ok(index)
    }
    /// The owner keeps this future alive until the storage outcome is known.
    /// Reserve the device before awaiting storage; a setter is returned only after success.
    pub async fn set<S: PreferenceStore>(
        &mut self,
        key: SettingKey,
        value: SettingValue,
        store: &mut S,
    ) -> Result<Saved, Error> {
        if !self.connected {
            return Err(Error::NotConnected);
        }
        let index = self.editable(key)?;
        let record = &self.records[index];
        let value = number(key, &value)?;
        if !record.metadata.accepts(value) {
            return Err(Error::InvalidValue);
        }
        if !self.discovered {
            return Err(Error::SettingsUnavailable);
        }
        if !record.available {
            return Err(
                if record
                    .error
                    .is_some_and(|e| e != ErrorCode::UnsupportedSetting)
                {
                    Error::SettingsUnavailable
                } else {
                    Error::UnsupportedSetting
                },
            );
        }
        let candidate = Preference {
            metadata: record.metadata.clone(),
            value,
        };
        if !candidate.metadata.wire().valid(&scalar(key, value)) {
            return Err(Error::UnsupportedSetting);
        }
        let unchanged = record.preference.as_deref() == Some(&candidate);
        if !unchanged && record.preference.is_none() && self.preferences().count() >= MAX_SAVED {
            return Err(Error::Limit);
        }
        let record = &mut self.records[index];
        if !unchanged {
            let candidate = Rc::new(candidate);
            self.busy = true;
            let result = store.save(&candidate).await;
            self.busy = false;
            result?;
            record.preference = Some(candidate);
        }
        record.changed.set(true);
        record.state = SettingState::Pending;
        record.error = None;
        Ok(if self.enabled {
            Saved::Apply
        } else {
            Saved::SavedOnly
        })
    }
    pub async fn forget<S: PreferenceStore>(
        &mut self,
        key: SettingKey,
        store: &mut S,
    ) -> Result<(), Error> {
        let index = self.editable(key)?;
        let record = &mut self.records[index];
        if record.preference.is_some() {
            self.busy = true;
            let result = store.remove(key).await;
            self.busy = false;
            result?;
        }
        record.preference = None;
        record.changed.set(true);
        record.state = SettingState::Unmanaged;
        record.error = None;
        Ok(())
    }
    /// Observations never update saved preferences or schedule corrective writes.
    pub fn observe(
        &mut self,
        key: SettingKey,
        value: SettingValue,
        now_ms: u64,
        source: ObservationSource,
    ) -> Result<(), Error> {
        if !self.connected {
            return Err(Error::NotConnected);
        }
        if !self.discovered {
            return Err(Error::SettingsUnavailable);
        }
        let record = self
            .records
            .iter_mut()
            .find(|r| r.metadata.key == key)
            .ok_or(Error::NotFound)?;
        if !record.available {
            return Err(Error::UnsupportedSetting);
        }
        if value == SettingValue::Null {
            return Err(Error::InvalidValue);
        }
        record.observe(Observed::from_wire(key, &value)?, now_ms, source);
        Ok(())
    }
    /// Invoke after stopping reconnect/input and removing the backend bond.
    /// A storage failure leaves the cache available for an explicit retry.
    pub async fn unpair<S: PreferenceStore>(&mut self, store: &mut S) -> Result<(), Error> {
        if self.busy {
            return Err(Error::Busy);
        }
        self.busy = true;
        let result = store.remove_all().await;
        self.busy = false;
        result?;
        *self = Self::default();
        Ok(())
    }
}

pub const MAX_RECORDS: usize = cordial_protocol::settings::SettingKey::ALL.len();
pub const MAX_SAVED: usize = MAX_RECORDS;
pub const MAX_FEATURES: usize = 256;
pub const MAX_CHOICES: usize = u16::MAX as usize + 1;
pub const MAX_TEXT_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    NotConnected,
    Busy,
    NotFound,
    ReadOnly,
    InvalidValue,
    Limit,
    Resource,
    Storage,
    StorageFull,
    StorageUnknown,
    SettingsUnavailable,
    UnsupportedSetting,
}

/// Persistent metadata contains no observation, transient state or presentation text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingMetadata {
    pub key: SettingKey,
    pub kind: SettingType,
    pub feature: FeatureId,
    pub feature_version: FeatureRevision,
    pub scope: SettingScope,
    pub choices: Vec<SettingValue>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub step: Option<i64>,
}
impl SettingMetadata {
    pub fn valid(&self, value: &SettingValue) -> bool {
        if (self.kind != SettingType::Integer || !self.choices.is_empty())
            && (self.min.is_some() || self.max.is_some() || self.step.is_some())
        {
            return false;
        }
        if self.kind != self.key.kind()
            || self.scope != SettingScope::Device
            || !self
                .key
                .writable_feature(self.feature, self.feature_version)
            || self.choices.len() > MAX_CHOICES
        {
            return false;
        }
        for (i, choice) in self.choices.iter().enumerate() {
            if self.choices[..i].contains(choice) {
                return false;
            }
            match (self.kind, choice) {
                (SettingType::Integer, SettingValue::Integer(n)) if (1..0xe000).contains(n) => {}
                (SettingType::Enum, SettingValue::Text(text))
                    if self.key.enum_values().contains(&text.as_str()) =>
                {
                    if self.key == SettingKey::BacklightMode
                        && !["automatic", "permanent_manual"].contains(&text.as_str())
                    {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        if self.kind == SettingType::Enum && self.choices.is_empty() {
            return false;
        }
        if self.kind == SettingType::Integer && self.choices.is_empty() {
            let (Some(min), Some(max), Some(step)) = (self.min, self.max, self.step) else {
                return false;
            };
            if min < 0 || max > u16::MAX.into() || min > max || step <= 0 || (max - min) % step != 0
            {
                return false;
            }
            match self.key {
                SettingKey::PointerDpi0 | SettingKey::PointerDpi1
                    if min == 0 || max >= 0xe000 || step > 0x1fff =>
                {
                    return false;
                }
                SettingKey::BacklightLevel if min != 0 || max > 7 || step != 1 => return false,
                SettingKey::BacklightDelayHandsIn
                | SettingKey::BacklightDelayHandsOut
                | SettingKey::BacklightDelayPowered
                    if (min, max, step) != (5, 7200, 5) =>
                {
                    return false;
                }
                SettingKey::WheelThreshold if (min, max, step) != (1, 255, 1) => return false,
                _ => {}
            }
        }
        if matches!(self.key, SettingKey::PointerDpi0 | SettingKey::PointerDpi1)
            && self.choices.len() > 6
        {
            return false;
        }
        self.record().accepts(value)
    }
    pub(crate) fn record(&self) -> Setting {
        Setting {
            key: self.key,
            kind: self.kind,
            writable: true,
            feature: self.feature,
            feature_version: self.feature_version,
            scope: self.scope,
            choices: self.choices.clone(),
            min: self.min,
            max: self.max,
            step: self.step,
            managed: false,
            desired: SettingValue::Null,
            observed: SettingValue::Null,
            fresh: false,
            observed_at_ms: None,
            observation_source: None,
            state: SettingState::Unmanaged,
            error: None,
        }
    }
}

impl Error {
    pub fn code(self) -> ErrorCode {
        match self {
            Self::NotConnected => ErrorCode::NotConnected,
            Self::Busy => ErrorCode::Busy,
            Self::NotFound => ErrorCode::NotFound,
            Self::ReadOnly => ErrorCode::ReadOnly,
            Self::InvalidValue => ErrorCode::InvalidArgs,
            Self::Limit => ErrorCode::SettingsLimit,
            Self::Resource => ErrorCode::Capacity,
            Self::Storage | Self::StorageUnknown => ErrorCode::StorageFailed,
            Self::StorageFull => ErrorCode::StorageFull,
            Self::SettingsUnavailable => ErrorCode::SettingsUnavailable,
            Self::UnsupportedSetting => ErrorCode::UnsupportedSetting,
        }
    }
}
