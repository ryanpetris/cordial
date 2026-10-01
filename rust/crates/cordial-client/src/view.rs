//! Revision-aware device and setting snapshots shared by every host interface.
use crate::{
    client::{Envelope, Error, Result},
    controller::{DeviceInfoView, DeviceSettings},
};
use cordial_protocol::{
    MAX_REVISION,
    errors::ErrorCode,
    hidpp::Feature,
    identifiers::*,
    info::{DeviceInfo, InfoField, InfoKey},
    messages::{Device, FeatureChunk, SettingChunk},
    payloads::{
        AdapterSettings, DeviceDisconnected, DeviceListEnd, DeviceSnapshot, DeviceUnpaired,
        LostEvents, SettingsListEnd,
    },
    settings::*,
};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone)]
enum Change {
    Adapter(u64, HostPlatform, String),
    Device(u64, Device, bool),
    Unpaired(u64, DeviceId),
    Setting(u64, DeviceId, Setting),
    Info(DeviceInfo),
}
impl Change {
    fn revision(&self) -> u64 {
        match self {
            Self::Adapter(r, _, _)
            | Self::Device(r, ..)
            | Self::Unpaired(r, _)
            | Self::Setting(r, ..) => *r,
            Self::Info(info) => info.revision,
        }
    }
}
#[derive(Default)]
struct Cache {
    rows: BTreeMap<SettingKey, (u64, Setting)>,
    order: Vec<SettingKey>,
    revision: u64,
    epoch: u64,
    complete: bool,
    loaded: bool,
    state: Option<SettingsState>,
    error: Option<ErrorCode>,
    features: Vec<Feature>,
    features_loaded: bool,
    feature_revision: u64,
    feature_epoch: u64,
}
impl Cache {
    fn put(&mut self, revision: u64, setting: Setting) {
        if self
            .rows
            .get(&setting.key)
            .is_some_and(|(old, _)| *old > revision)
        {
            return;
        }
        if !self.rows.contains_key(&setting.key) {
            self.order.push(setting.key);
        }
        self.rows.insert(setting.key, (revision, setting));
    }
    fn transition(&mut self, revision: u64, device: &Device) {
        if device.state == ConnectionState::Connected
            && device.normalization_state != NormalizationState::Resetting
        {
            return;
        }
        for (old, setting) in self.rows.values_mut() {
            if *old >= revision {
                continue;
            }
            setting.fresh = false;
            if setting.managed
                && matches!(
                    setting.state,
                    SettingState::Applied | SettingState::Applying | SettingState::ChangedOnDevice
                )
            {
                setting.state = SettingState::Pending;
            }
            *old = revision;
        }
    }
}
/// Volatile observations of one device, held only in this session's memory.
/// Each field keeps the revision that last set it so a snapshot never undoes
/// a newer change.
/// Bounded by the key and instance limits, which validation enforces.
struct InfoCache {
    rows: BTreeMap<(usize, u8), (u64, InfoField)>,
    revision: u64,
    epoch: u64,
    /// Every change after the snapshot was still retained to replay.
    complete: bool,
}
impl InfoCache {
    fn merge(&mut self, revision: u64, fields: &[InfoField]) {
        for f in fields {
            let slot = (info_order(f.key), f.instance);
            if self.rows.get(&slot).is_none_or(|(old, _)| *old < revision) {
                self.rows.insert(slot, (revision, f.clone()));
            }
        }
        self.revision = self.revision.max(revision);
    }
}
pub(crate) struct View {
    pub devices: BTreeMap<DeviceId, Device>,
    pub revision: u64,
    pub valid: bool,
    pub platform: HostPlatform,
    pub name: String,
    adapter_revision: u64,
    history: VecDeque<Change>,
    trimmed: u64,
    pub epoch: u64,
    pub settings_epoch: u64,
    lost_revision: u64,
    caches: BTreeMap<DeviceId, Cache>,
    infos: BTreeMap<DeviceId, InfoCache>,
    /// The epoch in which a device's info snapshot was last requested, so a
    /// failing device is retried after the next resynchronization, not in a loop.
    info_tried: BTreeMap<DeviceId, u64>,
    /// The last valid name each saved device reported, with its revision.
    /// Only a newer valid name replaces it: an unavailable or stale report
    /// never clears a known name. Dropped with the device.
    names: BTreeMap<DeviceId, (u64, String)>,
}
impl View {
    pub fn new(revision: u64, platform: HostPlatform, name: String) -> Self {
        Self {
            devices: BTreeMap::new(),
            revision,
            valid: false,
            platform,
            name,
            adapter_revision: revision,
            history: VecDeque::new(),
            trimmed: 0,
            epoch: 0,
            settings_epoch: 0,
            lost_revision: 0,
            caches: BTreeMap::new(),
            infos: BTreeMap::new(),
            info_tried: BTreeMap::new(),
            names: BTreeMap::new(),
        }
    }
    pub fn lose(&mut self) {
        self.epoch += 1;
        self.settings_epoch += 1;
        self.valid = false;
    }
    pub fn adapter(&mut self, revision: u64, platform: HostPlatform, name: String) {
        if revision >= self.adapter_revision {
            self.platform = platform;
            self.name = name;
            self.adapter_revision = revision;
        }
    }
    pub fn event(&mut self, message: &Envelope) -> Result<()> {
        let Some(name) = message.event() else {
            return Ok(());
        };
        if name == "local.events_lost" {
            self.lose();
            return Ok(());
        }
        if name == "events.lost" {
            let r: LostEvents = message.decode()?;
            valid_revision(r.revision)?;
            if r.revision > self.lost_revision {
                self.settings_epoch += 1;
            }
            self.lost_revision = self.lost_revision.max(r.revision);
            if r.revision > self.revision {
                self.valid = false;
            }
            return Ok(());
        }
        let change = match name {
            "adapter.changed" => {
                let row: AdapterSettings = message.decode()?;
                Change::Adapter(row.revision, row.host_platform, row.name)
            }
            "device.unpaired" => {
                let row: DeviceUnpaired = message.decode()?;
                valid_id(&row.device_id.0)?;
                Change::Unpaired(row.revision, row.device_id)
            }
            "hidpp.setting.changed" => {
                let row: SettingChunk = message.decode()?;
                valid_id(&row.device_id.0)?;
                validate_setting(&row.setting, 64)?;
                Change::Setting(row.revision, row.device_id, row.setting)
            }
            "device.info.changed" => {
                let row: DeviceInfo = message.decode()?;
                validate_info(&row, false)?;
                Change::Info(row)
            }
            "device.paired" | "device.connected" | "device.changed" | "device.disconnected" => {
                let row = if name == "device.disconnected" {
                    let row: DeviceDisconnected = message.decode()?;
                    DeviceSnapshot {
                        revision: row.revision,
                        device: row.device,
                    }
                } else {
                    message.decode::<DeviceSnapshot>()?
                };
                validate_device(&row.device)?;
                Change::Device(row.revision, row.device, name == "device.paired")
            }
            _ => return Ok(()),
        };
        valid_revision(change.revision())?;
        if change.revision() == 0 {
            return Err(Error::new("invalid device event revision"));
        }
        self.apply(change);
        Ok(())
    }
    fn settings_change(&mut self, change: &Change) {
        match change {
            Change::Adapter(..) | Change::Info(_) => {}
            Change::Setting(revision, id, setting) => {
                self.put_setting(id, *revision, setting.clone())
            }
            Change::Unpaired(revision, id) => {
                if self.caches.get(id).is_some_and(|c| *revision > c.revision) {
                    self.caches.remove(id);
                }
            }
            Change::Device(revision, device, paired) => {
                if *paired {
                    if self
                        .caches
                        .get(&device.device_id)
                        .is_some_and(|c| *revision > c.revision)
                    {
                        self.caches.remove(&device.device_id);
                    }
                } else if let Some(cache) = self.caches.get_mut(&device.device_id) {
                    cache.transition(*revision, device);
                }
            }
        }
    }
    fn info_change(&mut self, change: &Change) {
        match change {
            // A delta for a device without a snapshot is dropped; the missing
            // snapshot is fetched and already includes it.
            Change::Info(info) => {
                self.remember_name(info);
                if let Some(cache) = self.infos.get_mut(&info.device_id) {
                    cache.merge(info.revision, &info.fields);
                }
            }
            Change::Unpaired(revision, id)
            | Change::Device(revision, Device { device_id: id, .. }, true) => {
                if self.infos.get(id).is_some_and(|c| *revision > c.revision) {
                    self.infos.remove(id);
                }
                if matches!(change, Change::Unpaired(..))
                    && self.names.get(id).is_some_and(|(r, _)| revision > r)
                {
                    self.names.remove(id);
                }
                self.info_tried.remove(id);
            }
            _ => {}
        }
    }
    /// Keeps a valid reported name no older than the known one.
    fn remember_name(&mut self, info: &DeviceInfo) {
        if !self.devices.contains_key(&info.device_id) {
            return;
        }
        let name = info.fields.iter().find_map(|f| match &f.value {
            SettingValue::Text(name) if f.key == InfoKey::Name && f.available => Some(name),
            _ => None,
        });
        if let Some(name) = name
            && self
                .names
                .get(&info.device_id)
                .is_none_or(|(r, _)| *r <= info.revision)
        {
            self.names
                .insert(info.device_id.clone(), (info.revision, name.clone()));
        }
    }
    fn apply(&mut self, change: Change) {
        if let Change::Adapter(revision, platform, name) = &change {
            self.adapter(*revision, *platform, name.clone());
        }
        self.history.push_back(change.clone());
        if self.history.len() > 256 {
            self.trimmed = self
                .trimmed
                .max(self.history.pop_front().unwrap().revision());
        }
        self.settings_change(&change);
        self.info_change(&change);
        let revision = change.revision();
        if revision <= self.revision {
            return;
        }
        if revision != self.revision + 1 {
            self.valid = false;
            self.settings_epoch += 1;
        }
        match change {
            Change::Device(_, device, _) => {
                self.devices.insert(device.device_id.clone(), device);
            }
            Change::Unpaired(_, id) => {
                self.devices.remove(&id);
            }
            _ => {}
        }
        self.revision = revision;
    }
    pub fn install_devices(
        &mut self,
        messages: &[Envelope],
        epoch: u64,
        limit: usize,
    ) -> Result<()> {
        if messages.is_empty() || !messages.last().unwrap().done() || messages.len() - 1 > limit {
            return Err(Error::new("incomplete device snapshot"));
        }
        let summary: DeviceListEnd = messages.last().unwrap().decode()?;
        valid_revision(summary.revision)?;
        let mut devices = BTreeMap::new();
        for message in &messages[..messages.len() - 1] {
            let row: DeviceSnapshot = message.decode()?;
            validate_device(&row.device)?;
            if message.done()
                || row.revision != summary.revision
                || devices
                    .insert(row.device.device_id.clone(), row.device)
                    .is_some()
            {
                return Err(Error::new("inconsistent device snapshot"));
            }
        }
        if summary.count != devices.len() {
            return Err(Error::new("incomplete device snapshot"));
        }
        self.devices = devices;
        self.revision = summary.revision;
        self.valid = epoch == self.epoch
            && summary.revision >= self.lost_revision
            && summary.revision >= self.trimmed;
        for change in std::mem::take(&mut self.history) {
            self.apply(change);
        }
        self.caches.retain(|id, _| self.devices.contains_key(id));
        self.infos.retain(|id, _| self.devices.contains_key(id));
        self.names.retain(|id, _| self.devices.contains_key(id));
        self.info_tried
            .retain(|id, _| self.devices.contains_key(id));
        Ok(())
    }
    fn info_current(&self, id: &DeviceId) -> bool {
        self.infos.get(id).is_some_and(|c| {
            self.valid
                && c.complete
                && c.epoch == self.settings_epoch
                && c.revision >= self.lost_revision
        })
    }
    /// The device's last reported name, even when it is only last known,
    /// else its saved name.
    fn name(&self, device: &Device) -> Option<String> {
        self.names
            .get(&device.device_id)
            .map(|(_, name)| name.clone())
            .or_else(|| device.name.clone())
    }
    /// Saved devices as every interface shows and resolves them: named by
    /// [`Self::name`], since name changes arrive only as information.
    pub fn named_devices(&self) -> Vec<Device> {
        self.devices
            .values()
            .map(|d| Device {
                name: self.name(d),
                ..d.clone()
            })
            .collect()
    }
    /// Saved devices whose info snapshot is missing or predates a loss of
    /// events, not yet requested since the last resynchronization.
    pub fn info_needed(&self) -> Vec<DeviceId> {
        if !self.valid {
            return Vec::new();
        }
        self.devices
            .keys()
            .filter(|id| {
                !self.info_current(id) && self.info_tried.get(*id) != Some(&self.settings_epoch)
            })
            .cloned()
            .collect()
    }
    pub fn info_tried(&mut self, id: &DeviceId) {
        self.info_tried.insert(id.clone(), self.settings_epoch);
    }
    pub fn infos(&self) -> BTreeMap<DeviceId, DeviceInfoView> {
        self.infos
            .iter()
            .map(|(id, c)| {
                let fields = c.rows.values().map(|(_, f)| f.clone()).collect();
                (
                    id.clone(),
                    DeviceInfoView {
                        current: self.info_current(id),
                        revision: c.revision,
                        fields,
                    },
                )
            })
            .collect()
    }
    /// Installs a complete snapshot, keeping fields changed after it and
    /// replaying retained changes newer than it.
    pub fn install_info(&mut self, id: &DeviceId, info: DeviceInfo, epoch: u64) -> Result<()> {
        validate_info(&info, true)?;
        if info.device_id != *id {
            return Err(Error::new("adapter returned another device's information"));
        }
        if !self.devices.contains_key(id) {
            return Ok(());
        }
        self.remember_name(&info);
        let old = self.infos.remove(id);
        // Changes after the snapshot come from the retained history or, once
        // evicted from it, from a complete earlier cache that merged them.
        let complete = self.trimmed <= info.revision
            || old.as_ref().is_some_and(|o| o.complete && o.epoch == epoch);
        let mut cache = InfoCache {
            rows: BTreeMap::new(),
            revision: info.revision,
            epoch,
            complete,
        };
        cache.merge(info.revision, &info.fields);
        for (revision, field) in old.into_iter().flat_map(|c| c.rows.into_values()) {
            if revision > info.revision {
                cache.merge(revision, std::slice::from_ref(&field));
            }
        }
        self.infos.insert(id.clone(), cache);
        let newer: Vec<Change> = self
            .history
            .iter()
            .filter(|c| {
                c.revision() > info.revision && matches!(c, Change::Info(i) if i.device_id == *id)
            })
            .cloned()
            .collect();
        for change in &newer {
            self.info_change(change);
        }
        Ok(())
    }
    pub fn put_setting(&mut self, id: &DeviceId, revision: u64, setting: Setting) {
        if let Some(cache) = self.caches.get_mut(id)
            && cache.loaded
        {
            cache.put(revision, setting);
        }
    }
    pub fn forget_settings(&mut self, id: &DeviceId) {
        self.caches.remove(id);
    }
    pub fn settings(&self) -> BTreeMap<DeviceId, DeviceSettings> {
        self.caches
            .iter()
            .map(|(id, c)| {
                let current = self.devices.get(id).is_some_and(|d| {
                    c.loaded
                        && c.complete
                        && c.epoch == self.settings_epoch
                        && c.revision >= self.lost_revision
                        && d.settings_revision <= c.revision
                });
                let features_current = self.devices.get(id).is_some_and(|d| {
                    c.features_loaded
                        && c.feature_epoch == self.settings_epoch
                        && c.feature_revision >= self.lost_revision
                        && d.settings_revision <= c.feature_revision
                });
                (
                    id.clone(),
                    DeviceSettings {
                        loaded: c.loaded,
                        current,
                        revision: c.revision,
                        settings: c
                            .order
                            .iter()
                            .filter_map(|k| c.rows.get(k).map(|(_, row)| row.clone()))
                            .collect(),
                        state: c.state,
                        error: c.error,
                        features_loaded: c.features_loaded,
                        features: c.features.clone(),
                        features_current,
                        feature_revision: c.feature_revision,
                    },
                )
            })
            .collect()
    }
    pub fn install_settings(
        &mut self,
        id: &DeviceId,
        messages: &[Envelope],
        epoch: u64,
        limit: usize,
        choices: usize,
    ) -> Result<()> {
        let summary = snapshot_summary(id, messages, limit)?;
        let mut rows = BTreeMap::new();
        let mut order = Vec::new();
        for message in &messages[..messages.len() - 1] {
            let row: SettingChunk = message.decode()?;
            validate_setting(&row.setting, choices)?;
            if message.done()
                || row.revision != summary.revision
                || row.device_id != *id
                || rows
                    .insert(row.setting.key, (row.revision, row.setting.clone()))
                    .is_some()
            {
                return Err(Error::new("inconsistent setting snapshot"));
            }
            order.push(row.setting.key);
        }
        let old = self.caches.remove(id);
        let mut cache = Cache {
            rows,
            order,
            revision: summary.revision,
            epoch,
            complete: self.trimmed <= summary.revision,
            loaded: true,
            state: Some(summary.settings_state),
            error: summary.settings_error,
            ..Cache::default()
        };
        if let Some(old) = old {
            cache.features_loaded = old.features_loaded;
            cache.features = old.features;
            cache.feature_revision = old.feature_revision;
            cache.feature_epoch = old.feature_epoch;
            for (revision, setting) in old
                .rows
                .into_values()
                .filter(|(r, _)| *r > summary.revision)
            {
                cache.put(revision, setting);
            }
        }
        if self.devices.contains_key(id) {
            self.caches.insert(id.clone(), cache);
            for change in self
                .history
                .clone()
                .iter()
                .filter(|c| c.revision() > summary.revision)
            {
                self.settings_change(change);
            }
        }
        Ok(())
    }
    pub fn install_features(
        &mut self,
        id: &DeviceId,
        messages: &[Envelope],
        epoch: u64,
        limit: usize,
    ) -> Result<()> {
        let summary = snapshot_summary(id, messages, limit)?;
        let mut features = Vec::new();
        for message in &messages[..messages.len() - 1] {
            let row: FeatureChunk = message.decode()?;
            if message.done()
                || row.revision != summary.revision
                || row.device_id != *id
                || features
                    .iter()
                    .any(|f: &Feature| f.index == row.feature.index)
            {
                return Err(Error::new("inconsistent feature snapshot"));
            }
            features.push(row.feature);
        }
        if self.devices.contains_key(id) {
            let cache = self.caches.entry(id.clone()).or_default();
            if !cache.features_loaded || summary.revision >= cache.feature_revision {
                cache.features = features;
                cache.feature_revision = summary.revision;
                cache.feature_epoch = epoch;
                cache.features_loaded = true;
            }
        }
        Ok(())
    }
}
/// Display order of info keys.
pub(crate) fn info_order(key: InfoKey) -> usize {
    InfoKey::ALL.iter().position(|k| *k == key).unwrap()
}
/// A snapshot or change: valid fields, each key and instance at most once.
pub(crate) fn validate_info(info: &DeviceInfo, snapshot: bool) -> Result<()> {
    valid_id(&info.device_id.0)?;
    valid_revision(info.revision)?;
    if !snapshot && info.revision == 0 {
        return Err(Error::new("invalid device information revision"));
    }
    for (i, f) in info.fields.iter().enumerate() {
        if !f.valid()
            || info.fields[..i]
                .iter()
                .any(|g| g.key == f.key && g.instance == f.instance)
        {
            return Err(Error::new("invalid device information"));
        }
    }
    Ok(())
}
fn snapshot_summary(id: &DeviceId, messages: &[Envelope], limit: usize) -> Result<SettingsListEnd> {
    if messages.is_empty() || !messages.last().unwrap().done() || messages.len() - 1 > limit {
        return Err(Error::new("incomplete or oversized settings snapshot"));
    }
    let summary: SettingsListEnd = messages.last().unwrap().decode()?;
    valid_revision(summary.revision)?;
    if summary.device_id != *id || summary.count != messages.len() - 1 {
        return Err(Error::new("inconsistent settings snapshot"));
    }
    Ok(summary)
}
pub(crate) fn valid_revision(revision: u64) -> Result<()> {
    if revision > MAX_REVISION {
        Err(Error::new("invalid revision"))
    } else {
        Ok(())
    }
}
pub(crate) fn valid_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        Err(Error::new("invalid device ID"))
    } else {
        Ok(())
    }
}
pub(crate) fn validate_device(device: &Device) -> Result<()> {
    valid_id(&device.device_id.0)?;
    valid_revision(device.settings_revision)?;
    if !device.hidpp_protocol.valid() {
        return Err(Error::new("invalid HID++ protocol state"));
    }
    if device.name.as_ref().is_some_and(|s| s.len() > 128) {
        return Err(Error::new("invalid device name"));
    }
    use cordial_protocol::errors::DisabledReason as R;
    let d = device;
    let paired = d.pairing_state == PairingState::Paired;
    let reason_holds = match d.enabled_reason {
        None => {
            d.enabled
                && d.transport_supported
                && !d.blocked
                && d.validation_error.is_none()
                && paired
        }
        Some(R::UnsupportedTransport) => !d.transport_supported,
        Some(R::Invalid) => d.validation_error.is_some() || !paired,
        Some(R::Blocked) => d.blocked,
        Some(R::Disabled) => !d.enabled,
        Some(R::Capacity) => d.enabled,
    };
    if d.effective_enabled != d.enabled_reason.is_none()
        || !reason_holds
        // Needs pairing exactly when the saved bond itself is unusable.
        || d.validation_error.is_some_and(|v| v.needs_pairing()) == paired
        || (d.state == ConnectionState::Connected && !d.effective_enabled)
    {
        return Err(Error::new("invalid device enablement"));
    }
    Ok(())
}
pub(crate) fn validate_setting(setting: &Setting, choices: usize) -> Result<()> {
    let s = setting;
    let typed = |value: &SettingValue| match (s.kind, value) {
        (SettingType::Bool, SettingValue::Bool(_)) => true,
        (SettingType::Integer, SettingValue::Integer(n)) => n.unsigned_abs() <= MAX_REVISION,
        (SettingType::Enum | SettingType::Text, SettingValue::Text(v)) => v.len() <= 128,
        _ => false,
    };
    if s.kind != s.key.kind()
        || s.choices.len() > choices
        || s.observed_at_ms.is_some_and(|r| r > MAX_REVISION)
        || s.choices
            .iter()
            .enumerate()
            .any(|(i, v)| !typed(v) || s.choices[..i].contains(v))
        || (!s.writable
            && (s.managed || s.desired != SettingValue::Null || s.state != SettingState::Unmanaged))
        || (!s.managed && s.desired != SettingValue::Null)
        || (s.managed && (s.desired == SettingValue::Null || s.state == SettingState::Unmanaged))
        || (s.desired != SettingValue::Null && !typed(&s.desired))
        || (s.observed != SettingValue::Null && !typed(&s.observed))
    {
        return Err(Error::new("invalid setting record"));
    }
    match s.kind {
        SettingType::Integer => {
            if s.min.zip(s.max).is_some_and(|(min, max)| min > max)
                || s.step.is_some_and(|step| step < 1)
            {
                return Err(Error::new("invalid setting range"));
            }
        }
        _ => {
            if s.min.is_some()
                || s.max.is_some()
                || s.step.is_some()
                || (s.kind != SettingType::Enum && !s.choices.is_empty())
                || (s.kind == SettingType::Enum && s.writable && s.choices.is_empty())
            {
                return Err(Error::new("invalid setting choices"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{View, validate_device};
    use crate::client::Envelope;
    use cordial_protocol::{
        identifiers::{DeviceId, HostPlatform},
        messages::Message,
    };
    use serde_json::{Value, json};

    fn event(name: &str, data: Value) -> Envelope {
        Envelope {
            message: Message::event(name.into(), None, data),
            raw: String::new(),
            command: None,
            internal: false,
            sequence: 0,
        }
    }
    #[test]
    fn adapter_preferences_keep_the_newest_complete_snapshot() {
        let mut v = View::new(0, HostPlatform::Linux, "Pico W".into());
        v.event(&event(
            "adapter.changed",
            json!({"revision":3,"name":"Desk","host_platform":"mac"}),
        ))
        .unwrap();
        v.adapter(2, HostPlatform::Windows, "Stale".into());
        assert_eq!(v.name, "Desk");
        assert_eq!(v.platform, HostPlatform::Mac);
        v.adapter(4, HostPlatform::Linux, "Office".into());
        assert_eq!(v.name, "Office");
        assert_eq!(v.platform, HostPlatform::Linux);
    }

    fn name_field(name: &str, fresh: bool) -> Value {
        json!({"key":"name","instance":0,"value":name,"available":true,"fresh":fresh})
    }
    fn info(revision: u64, fields: Vec<Value>) -> cordial_protocol::info::DeviceInfo {
        serde_json::from_value(json!({"revision":revision,"device_id":"d_1","fields":fields}))
            .unwrap()
    }
    /// A current view holding one saved device at revision 1.
    fn view() -> View {
        let mut v = View::new(0, HostPlatform::Linux, "Test adapter".into());
        let d = crate::ui::command::tests::device("d_1", "Saved name");
        v.event(&event(
            "device.paired",
            json!({"revision":1,"device":serde_json::to_value(&d).unwrap()}),
        ))
        .unwrap();
        v.valid = true;
        v
    }
    fn name(v: &View) -> Option<String> {
        v.named_devices()[0].name.clone()
    }

    /// A reported name names the device everywhere, also when only last
    /// known. Unavailable reports, snapshots without a name and older reports
    /// never replace a known name; unpairing forgets it.
    #[test]
    fn reported_names_are_kept_until_a_newer_valid_name() {
        let mut v = view();
        let id = DeviceId("d_1".into());
        let change = |v: &mut View, revision: u64, field: Value| {
            v.event(&event(
                "device.info.changed",
                json!({"revision":revision,"device_id":"d_1","fields":[field]}),
            ))
            .unwrap();
        };
        assert_eq!(name(&v).as_deref(), Some("Saved name"));
        let epoch = v.settings_epoch;
        v.install_info(&id, info(1, vec![name_field("Reported", true)]), epoch)
            .unwrap();
        assert_eq!(name(&v).as_deref(), Some("Reported"));
        change(&mut v, 2, name_field("Renamed", true));
        assert_eq!(name(&v).as_deref(), Some("Renamed"));
        change(&mut v, 3, name_field("Renamed", false));
        assert_eq!(name(&v).as_deref(), Some("Renamed"), "stale stays");
        change(
            &mut v,
            4,
            json!({"key":"name","instance":0,"value":null,"available":false,"fresh":false}),
        );
        assert_eq!(name(&v).as_deref(), Some("Renamed"), "unknown never clears");
        v.install_info(&id, info(4, vec![]), epoch).unwrap();
        assert_eq!(
            name(&v).as_deref(),
            Some("Renamed"),
            "snapshot without a name"
        );
        v.install_info(&id, info(1, vec![name_field("Old", true)]), epoch)
            .unwrap();
        assert_eq!(name(&v).as_deref(), Some("Renamed"), "older report");
        // Invalid names are rejected before they reach the view.
        let empty = event(
            "device.info.changed",
            json!({"revision":5,"device_id":"d_1","fields":[name_field("", true)]}),
        );
        assert!(v.event(&empty).is_err());
        assert_eq!(name(&v).as_deref(), Some("Renamed"));
        assert_eq!(v.devices[&id].name.as_deref(), Some("Saved name"));
        v.event(&event(
            "device.unpaired",
            json!({"revision":6,"device_id":"d_1"}),
        ))
        .unwrap();
        assert!(v.names.is_empty());
    }

    /// Change history stays bounded under a flood of information changes.
    /// A snapshot older than the evicted changes is current only when an
    /// earlier complete cache already merged them.
    #[test]
    fn info_history_is_bounded_and_eviction_is_detected() {
        let mut v = view();
        let id = DeviceId("d_1".into());
        let flood = |v: &mut View, from: u64| {
            for r in from..from + 300 {
                v.event(&event(
                    "device.info.changed",
                    json!({"revision":r,"device_id":"d_1","fields":[
                        {"key":"battery_percent","instance":0,"value":r % 100,"available":true,"fresh":true}]}),
                ))
                .unwrap();
            }
        };
        let epoch = v.settings_epoch;
        flood(&mut v, 2);
        assert!(v.history.len() <= 256);
        // No earlier cache: changes before the snapshot's reply may be lost.
        v.install_info(&id, info(1, vec![]), epoch).unwrap();
        assert!(!v.infos()[&id].current);
        // With a complete cache, a delayed snapshot keeps its merged changes.
        let revision = v.revision;
        v.install_info(&id, info(revision, vec![]), epoch).unwrap();
        assert!(v.infos()[&id].current);
        flood(&mut v, revision + 1);
        assert!(v.history.len() <= 256);
        v.install_info(&id, info(revision, vec![]), epoch).unwrap();
        let view = &v.infos()[&id];
        assert!(view.current);
        assert_eq!(view.fields.len(), 1, "newer battery change kept");
    }

    use cordial_protocol::errors::{DisabledReason, ValidationError};
    use cordial_protocol::identifiers::{ConnectionState, PairingState, Transport};

    #[test]
    fn protocol_evidence_is_validated_separately_from_feature_readiness() {
        use cordial_protocol::{errors::ErrorCode, hidpp::ProtocolState};
        use cordial_protocol::{identifiers::NormalizationState, messages::Device};
        let mut d = crate::ui::command::tests::device("d_1", "Kbd");
        d.normalization_state = NormalizationState::Unsupported;
        d.normalization_error = Some(ErrorCode::HidppControlsUnavailable);
        for protocol in [
            ProtocolState::Unknown,
            ProtocolState::Detected { major: 4, minor: 2 },
            ProtocolState::Error {
                code: ErrorCode::HidppTimeout,
            },
        ] {
            d.hidpp_protocol = protocol;
            assert!(validate_device(&d).is_ok());
        }
        for protocol in [
            ProtocolState::Detected { major: 0, minor: 0 },
            ProtocolState::Error {
                code: ErrorCode::HidppControlsUnavailable,
            },
        ] {
            d.hidpp_protocol = protocol;
            assert!(validate_device(&d).is_err());
        }
        let mut value = serde_json::to_value(&d).unwrap();
        value.as_object_mut().unwrap().remove("hidpp_protocol");
        let d: Device = serde_json::from_value(value).unwrap();
        assert_eq!(d.hidpp_protocol, ProtocolState::Unknown);
        assert!(validate_device(&d).is_ok());
    }

    /// Unreadable or corrupt device records stay paired but invalid; bond
    /// problems need pairing. Firmware snapshots of both are accepted.
    #[test]
    fn invalid_records_match_firmware_snapshots() {
        let mut d = crate::ui::command::tests::device("d_1", "Kbd");
        d.state = ConnectionState::Disconnected;
        d.security = None;
        d.effective_enabled = false;
        for v in [ValidationError::DeviceCorrupt, ValidationError::ReadFailed] {
            d.validation_error = Some(v);
            d.pairing_state = PairingState::Paired;
            d.enabled_reason = Some(DisabledReason::Invalid);
            assert!(validate_device(&d).is_ok(), "{v:?}");
            // An unreadable record's placeholder transport may be unsupported.
            d.transport = Transport::Classic;
            d.transport_supported = false;
            d.enabled_reason = Some(DisabledReason::UnsupportedTransport);
            assert!(validate_device(&d).is_ok(), "{v:?}");
            d.transport_supported = true;
            d.pairing_state = PairingState::NeedsPairing;
            d.enabled_reason = Some(DisabledReason::Invalid);
            assert!(validate_device(&d).is_err(), "{v:?} must stay paired");
        }
        d.validation_error = Some(ValidationError::BondMissing);
        assert!(validate_device(&d).is_ok());
        d.pairing_state = PairingState::Paired;
        assert!(validate_device(&d).is_err());
        d.validation_error = None;
        d.enabled_reason = None;
        assert!(validate_device(&d).is_err(), "inactive needs a reason");
    }
}
