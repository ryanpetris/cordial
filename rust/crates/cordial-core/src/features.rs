//! HID++ settings jobs share the normalization client's single request slot.
//! The owner keeps polling Bluetooth/USB while this state machine waits for replies.
mod handlers;

use crate::{
    compact::{Metadata, Observed, Range, Record},
    hidpp::{Client, Error as ExchangeError},
    settings::{Catalog, MAX_FEATURES, MAX_RECORDS},
};
use alloc::{boxed::Box, format, string::String, vec::Vec};
use cordial_protocol::{
    errors::ErrorCode as Error,
    hidpp::{Feature, FeatureFlags, FeatureId as Id, FeatureRevision},
    identifiers::SettingsState,
    settings::{
        ObservationSource, SettingKey as Key, SettingOutcome as Outcome, SettingScope, SettingState,
    },
};

const JOB_MS: u64 = 90_000;
fn bit(key: Key) -> u32 {
    1 << key as u8
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Handler {
    Firmware,
    Name,
    Battery,
    Fn,
    Backlight,
    Dpi0,
    Dpi1,
    Wheel,
    Hires,
    Thumb,
}
impl Handler {
    const ALL: [Self; 10] = [
        Self::Firmware,
        Self::Name,
        Self::Battery,
        Self::Fn,
        Self::Backlight,
        Self::Dpi0,
        Self::Dpi1,
        Self::Wheel,
        Self::Hires,
        Self::Thumb,
    ];
    fn key(key: Key) -> Option<Self> {
        use Key::*;
        Some(match key {
            FnRowDefault => Self::Fn,
            PointerDpi0 => Self::Dpi0,
            PointerDpi1 => Self::Dpi1,
            WheelMode | WheelThreshold => Self::Wheel,
            WheelInvert | WheelInfo => Self::Hires,
            ThumbwheelInvert => Self::Thumb,
            _ => Self::Backlight,
        })
    }
    fn feature(self, catalog: &Catalog) -> Option<Feature> {
        let id = match self {
            Self::Firmware => Id::DEVICE_INFORMATION,
            Self::Name => Id::DEVICE_NAME,
            Self::Battery => {
                return [
                    Id::UNIFIED_BATTERY,
                    Id::BATTERY,
                    Id::SOLAR,
                    Id::BATTERY_VOLTAGE,
                    Id::ADC_MEASUREMENT,
                ]
                .into_iter()
                .find_map(|id| feature(catalog, id));
            }
            Self::Fn => {
                if let Some(f) = feature(catalog, Id::FN_INVERSION_MULTI_HOST) {
                    return Some(f);
                }
                Id::FN_INVERSION
            }
            Self::Backlight => Id::BACKLIGHT,
            Self::Dpi0 | Self::Dpi1 => Id::ADJUSTABLE_DPI,
            Self::Wheel => Id::SMART_SHIFT,
            Self::Hires => Id::HIRES_WHEEL,
            Self::Thumb => Id::THUMBWHEEL,
        };
        feature(catalog, id)
    }
}
pub(crate) fn supported(id: Id, flags: FeatureFlags) -> bool {
    flags.public()
        && matches!(
            id,
            Id::ROOT
                | Id::FEATURE_SET
                | Id::DEVICE_INFORMATION
                | Id::DEVICE_NAME
                | Id::CONFIG_CHANGE
                | Id::REPROG_CONTROLS
                | Id::BATTERY
                | Id::UNIFIED_BATTERY
                | Id::SOLAR
                | Id::BATTERY_VOLTAGE
                | Id::ADC_MEASUREMENT
                | Id::BACKLIGHT
                | Id::ADJUSTABLE_DPI
                | Id::FN_INVERSION
                | Id::FN_INVERSION_MULTI_HOST
                | Id::SMART_SHIFT
                | Id::HIRES_WHEEL
                | Id::THUMBWHEEL
        )
}
fn feature(catalog: &Catalog, id: Id) -> Option<Feature> {
    catalog
        .features
        .iter()
        .enumerate()
        .find(|(_, f)| f.id == id && f.supported())
        .map(|(index, f)| f.wire(index))
}
fn row(catalog: &mut Catalog, key: Key) -> Option<&mut Record> {
    let index = catalog.records.iter().position(|r| r.metadata.key == key)?;
    Some(&mut catalog.records[index])
}
fn observation(record: &mut Record, value: Observed, now: u64, source: ObservationSource) {
    record.observe(value, now, source);
}
fn stale(catalog: &mut Catalog, key: Key) {
    if let Some(r) = row(catalog, key) {
        r.changed.set(true);
        r.fresh = false;
    }
}
fn failed(record: &mut Record, error: Error, uncertain: bool) {
    record.changed.set(true);
    record.fresh = false;
    record.error = Some(error);
    record.state = if record.preference.is_none() {
        SettingState::Unmanaged
    } else if error == Error::UnsupportedSetting {
        SettingState::Unsupported
    } else if uncertain || record.state == SettingState::Uncertain {
        SettingState::Uncertain
    } else {
        SettingState::Error
    };
}
fn unsupported(catalog: &mut Catalog, key: Key) {
    if let Some(r) = row(catalog, key) {
        r.available = false;
        failed(r, Error::UnsupportedSetting, false);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Phase {
    #[default]
    Idle,
    Root,
    Count,
    Feature,
    Version,
    Handlers,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Stage {
    #[default]
    Initial,
    Second,
    Third,
    Config,
    Write,
    Readback,
    Serial,
    BacklightVerify,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobResult {
    pub key: Key,
    pub outcome: Outcome,
}

#[derive(Default)]
pub struct Engine {
    pub state: SettingsState,
    pub error: Option<Error>,
    phase: Phase,
    stage: Stage,
    deadline: u64,
    requested: u32,
    read_failures: u32,
    completed_keys: u32,
    event_epoch: u32,
    read_epoch: u32,
    feature_set: u8,
    feature_set_version: u8,
    enumerate_count: u16,
    enumerate_index: u16,
    handler: usize,
    sensor_count: u8,
    firmware_count: u8,
    firmware_index: u8,
    firmware_seen: u8,
    name_length: u8,
    name: Vec<u8>,
    fn_host: u8,
    battery_levels: u8,
    battery_flags: u8,
    backlight_levels: u8,
    backlight_effect: u8,
    raw: [u8; 16],
    results: Vec<JobResult>,
    discover: bool,
    apply: bool,
    waiting: bool,
    explicit: bool,
    completed: bool,
    wrote: bool,
    setter_sent: bool,
    serial_supported: bool,
    refresh_backlight: bool,
    targeted: bool,
    information_only: bool,
    cancelled: Option<Error>,
}
impl Engine {
    pub fn busy(&self) -> bool {
        self.phase != Phase::Idle || self.explicit
    }
    pub fn done(&self) -> bool {
        self.explicit && self.completed
    }
    /// Whether a requested job owns the engine until its results are released.
    pub fn explicit(&self) -> bool {
        self.explicit
    }
    pub fn results(&self) -> &[JobResult] {
        &self.results
    }
    /// Release only after all result chunks and the final response have been queued.
    pub fn release(&mut self, catalog: &mut Catalog) {
        if self.completed {
            self.explicit = false;
            catalog.busy = false;
        }
    }
    fn begin(&mut self, catalog: &mut Catalog, discover: bool, apply: bool, now: u64) {
        self.phase = if discover {
            Phase::Root
        } else {
            Phase::Handlers
        };
        self.stage = Stage::Initial;
        self.handler = 0;
        self.discover = discover;
        self.apply = apply;
        self.waiting = false;
        self.completed = false;
        self.cancelled = None;
        self.wrote = false;
        self.setter_sent = false;
        self.targeted = false;
        self.information_only = false;
        self.deadline = now.saturating_add(JOB_MS);
        self.read_failures = 0;
        self.completed_keys = 0;
        self.results.clear();
        self.requested = if apply {
            catalog
                .preferences()
                .fold(0, |mask, p| mask | bit(p.metadata.key))
        } else {
            0
        };
        catalog.busy = true;
        if discover {
            catalog.invalidate();
            catalog.discovered = false;
            catalog.features.clear();
            catalog.features.push(crate::compact::FeatureEntry {
                id: Id::ROOT,
                version: FeatureRevision(0),
                flags: FeatureFlags(0),
            });
            for r in &mut catalog.records {
                r.available = false;
            }
        }
        self.state = if discover || !apply {
            SettingsState::Discovering
        } else {
            SettingsState::Applying
        };
        self.error = None;
    }
    /// Called after the client's connect/enable normalization sequence has settled.
    pub fn activate(&mut self, catalog: &mut Catalog, now: u64) -> Result<(), Error> {
        self.start(catalog, catalog.enabled, None, false, true, now)
    }
    /// Apply is explicit or part of activation. Notifications never start Apply.
    pub fn start(
        &mut self,
        catalog: &mut Catalog,
        apply: bool,
        key: Option<Key>,
        explicit: bool,
        rediscover: bool,
        now: u64,
    ) -> Result<(), Error> {
        if !catalog.connected {
            return Err(Error::NotConnected);
        }
        if apply && !catalog.enabled {
            return Err(Error::HidppDisabled);
        }
        if self.busy() || catalog.busy {
            return Err(Error::Busy);
        }
        if let Some(key) = key {
            let r = catalog
                .records
                .iter()
                .find(|r| r.metadata.key == key)
                .ok_or(Error::NotFound)?;
            if !apply || r.preference.is_none() {
                return Err(Error::InvalidArgs);
            }
        }
        self.explicit = explicit;
        self.begin(catalog, rediscover || !catalog.discovered, apply, now);
        if let Some(key) = key {
            self.requested = bit(key);
        }
        Ok(())
    }
    pub fn start_information(&mut self, catalog: &mut Catalog, now: u64) -> Result<(), Error> {
        if !catalog.discovered {
            return Ok(());
        }
        let state = self.state;
        let error = self.error;
        self.start(catalog, false, None, false, false, now)?;
        self.state = state;
        self.error = error;
        self.information_only = true;
        Ok(())
    }
    pub fn cancel(&mut self, reason: Error) {
        if self.phase != Phase::Idle {
            self.cancelled = Some(reason);
            self.refresh_backlight = false;
        }
    }
    /// The owner discards the transport client after this call, so no reply can arrive later.
    pub fn disconnected(&mut self, catalog: &mut Catalog, client: &Client) {
        if self.wrote && self.stage == Stage::Write && client.exchange_sent {
            self.setter_sent = true;
        }
        self.cancel(Error::NotConnected);
        self.waiting = false;
        self.finish_cancel(catalog);
        catalog.connection(false, catalog.enabled);
        self.error = None;
        self.state = if catalog.enabled {
            SettingsState::Pending
        } else {
            SettingsState::Off
        };
    }
    fn result(&mut self, key: Key, outcome: Outcome) {
        self.completed_keys |= bit(key);
        if self.explicit && !self.results.iter().any(|r| r.key == key) {
            self.results.push(JobResult { key, outcome });
        }
    }
    fn current(&self) -> Option<Handler> {
        Handler::ALL.get(self.handler).copied()
    }
    fn matches(&self, r: &Record, requested: bool) -> bool {
        Handler::key(r.metadata.key) == self.current()
            && (!requested || self.requested & bit(r.metadata.key) != 0)
    }
    fn handler_failed(&mut self, catalog: &mut Catalog, error: Error, uncertain: bool) {
        use cordial_protocol::info::InfoKey as I;
        let keys: &[I] = match self.current() {
            Some(Handler::Firmware) if self.stage == Stage::Serial => &[I::Serial],
            Some(Handler::Firmware) => &[I::Firmware, I::Hardware, I::Serial],
            Some(Handler::Name) if self.stage == Stage::Third => &[I::Kind],
            Some(Handler::Name) => &[I::Name, I::Kind],
            Some(Handler::Battery) => &[I::BatteryPercent, I::BatteryCharging],
            _ => &[],
        };
        catalog.info.invalidate_vendor(keys);
        for r in &mut catalog.records {
            let matches = {
                self.matches(r, self.wrote && self.current() == Some(Handler::Wheel))
                    || (self.current() == Some(Handler::Dpi0)
                        && self.stage == Stage::Initial
                        && r.metadata.key == Key::PointerDpi1)
            };
            if matches {
                self.read_failures |= bit(r.metadata.key);
                failed(r, error, uncertain);
                if !self.apply || self.requested & bit(r.metadata.key) != 0 {
                    self.result(
                        r.metadata.key,
                        if r.state == SettingState::Uncertain {
                            Outcome::Uncertain
                        } else if error == Error::UnsupportedSetting {
                            Outcome::Unsupported
                        } else {
                            Outcome::Failed
                        },
                    );
                }
            }
        }
    }
    fn handler_results(&mut self, catalog: &Catalog, applied: bool) {
        for r in catalog.records.iter() {
            if !self.matches(r, self.apply)
                || (!self.apply && !r.available && r.preference.is_none())
            {
                continue;
            }
            let outcome = if !r.available
                || (self.apply
                    && (!r.writable
                        || !r
                            .preference
                            .as_ref()
                            .is_some_and(|p| r.metadata.accepts(p.value))))
            {
                Outcome::Unsupported
            } else if !self.apply && r.error.is_none() {
                Outcome::Read
            } else if !r.fresh || r.error.is_some() {
                Outcome::Failed
            } else if r.differs_from_saved() == Some(false) {
                if applied {
                    Outcome::Applied
                } else {
                    Outcome::Unchanged
                }
            } else {
                Outcome::Failed
            };
            self.result(r.metadata.key, outcome);
        }
    }
    fn advance(&mut self) {
        self.handler += 1;
        self.stage = Stage::Initial;
        self.wrote = false;
        self.setter_sent = false;
    }
    fn finish(&mut self, catalog: &mut Catalog) {
        if self.information_only {
            self.phase = Phase::Idle;
            self.completed = true;
            catalog.busy = false;
            return;
        }
        for r in &mut catalog.records {
            if r.preference.is_some() && !r.available {
                if self.discover && self.read_failures & bit(r.metadata.key) == 0 {
                    failed(r, Error::UnsupportedSetting, false);
                }
                if !self.apply || self.requested & bit(r.metadata.key) != 0 {
                    self.result(
                        r.metadata.key,
                        if r.error == Some(Error::UnsupportedSetting) {
                            Outcome::Unsupported
                        } else {
                            Outcome::Failed
                        },
                    );
                }
            }
        }
        self.phase = Phase::Idle;
        self.completed = true;
        catalog.discovered = true;
        catalog.busy = self.explicit;
        self.state = SettingsState::Ready;
        self.error = None;
    }
    fn finish_cancel(&mut self, catalog: &mut Catalog) {
        let Some(reason) = self.cancelled.take() else {
            return;
        };
        if self.information_only {
            self.handler_failed(catalog, reason, false);
            self.phase = Phase::Idle;
            self.completed = true;
            catalog.busy = false;
            return;
        }
        for r in &mut catalog.records {
            if (!self.apply || self.requested & bit(r.metadata.key) != 0)
                && self.completed_keys & bit(r.metadata.key) == 0
            {
                let uncertain = self.setter_sent && self.matches(r, false);
                if uncertain || self.explicit || reason == Error::Timeout {
                    failed(r, reason, uncertain);
                }
                self.result(
                    r.metadata.key,
                    if r.state == SettingState::Uncertain {
                        Outcome::Uncertain
                    } else {
                        Outcome::Failed
                    },
                );
            }
        }
        self.phase = Phase::Idle;
        self.completed = true;
        catalog.busy = self.explicit;
        self.state = SettingsState::Error;
        self.error = Some(reason);
        if matches!(
            reason,
            Error::NotConnected | Error::HidppDisabled | Error::Cancelled
        ) {
            self.state = if catalog.enabled {
                SettingsState::Pending
            } else {
                SettingsState::Off
            };
            self.error = None;
        }
    }
    fn discovery_failure(&mut self, catalog: &mut Catalog, error: Error) {
        catalog
            .info
            .invalidate_vendor(&cordial_protocol::info::InfoKey::ALL);
        for r in &mut catalog.records {
            failed(r, error, false);
            if !self.apply || self.requested & bit(r.metadata.key) != 0 {
                self.result(r.metadata.key, Outcome::Failed);
            }
        }
        self.phase = Phase::Idle;
        self.completed = true;
        self.refresh_backlight = false;
        catalog.busy = self.explicit;
        self.state = SettingsState::Error;
        self.error = Some(error);
    }
    fn send(
        &mut self,
        client: &mut Client,
        feature: u8,
        function: u8,
        parameters: &[u8],
        now: u64,
    ) -> bool {
        if !client.exchange(feature, function, parameters, now) {
            return false;
        }
        self.waiting = true;
        true
    }
    fn discovery_response(&mut self, catalog: &mut Catalog, p: &[u8]) -> Result<(), Error> {
        match self.phase {
            Phase::Root => {
                if p.len() < 3 || p[0] == 0 || !FeatureFlags(p[1]).public() {
                    return Err(Error::FeatureSetUnavailable);
                }
                self.feature_set = p[0];
                self.feature_set_version = p[2];
                self.phase = Phase::Count;
            }
            Phase::Count => {
                if p.is_empty() || p[0] == 0 {
                    return Err(Error::HidppInvalidResponse);
                }
                self.enumerate_count = p[0].into();
                catalog
                    .features
                    .try_reserve_exact(self.enumerate_count as usize)
                    .map_err(|_| Error::Capacity)?;
                self.enumerate_index = 1;
                self.phase = Phase::Feature;
            }
            Phase::Feature => {
                if p.len() < if self.feature_set_version == 0 { 3 } else { 4 } {
                    return Err(Error::HidppInvalidResponse);
                }
                let id = Id(u16::from_be_bytes([p[0], p[1]]));
                if id == Id::ROOT
                    || catalog.features.len() >= MAX_FEATURES
                    || catalog.features.iter().any(|f| f.id == id)
                {
                    return Err(Error::HidppInvalidResponse);
                }
                let flags = FeatureFlags(p[2]);
                catalog.features.push(crate::compact::FeatureEntry {
                    id,
                    version: FeatureRevision(if self.feature_set_version == 0 {
                        0
                    } else {
                        p[3]
                    }),
                    flags,
                });
                if self.feature_set_version == 0 && flags.public() {
                    self.phase = Phase::Version;
                    return Ok(());
                }
                self.next_feature();
            }
            Phase::Version => {
                let f = catalog
                    .features
                    .last_mut()
                    .ok_or(Error::HidppInvalidResponse)?;
                if p.len() < 3 || p[0] != self.enumerate_index as u8 || p[1] != f.flags.0 {
                    return Err(Error::HidppInvalidResponse);
                }
                f.version = FeatureRevision(p[2]);
                self.next_feature();
            }
            _ => return Err(Error::InternalError),
        }
        Ok(())
    }
    fn next_feature(&mut self) {
        self.enumerate_index += 1;
        self.phase = if self.enumerate_index > self.enumerate_count {
            Phase::Handlers
        } else {
            Phase::Feature
        };
    }
    /// Returns whether catalog/state may have changed. Call after Client::tick.
    pub fn poll(&mut self, catalog: &mut Catalog, client: &mut Client, now: u64) -> bool {
        if self.wrote && self.stage == Stage::Write && client.exchange_sent {
            self.setter_sent = true;
        }
        if self.apply && !catalog.enabled {
            self.cancel(Error::HidppDisabled);
        }
        if self.phase != Phase::Idle && now >= self.deadline {
            self.cancel(Error::Timeout);
        }
        let reply = if self.waiting {
            client.response()
        } else {
            None
        };
        let observed = reply.is_some() && self.phase == Phase::Handlers;
        if reply.is_some() {
            self.waiting = false;
        }
        if self.cancelled.is_some() {
            if self.waiting {
                // Unsent requests may be discarded; sent requests retain the transport slot.
                client.quiesce();
                if client.response().is_some() {
                    self.waiting = false;
                }
            }
            if !self.waiting {
                self.finish_cancel(catalog);
            }
            return true;
        }
        if !catalog.connected {
            return false;
        }
        if self.phase == Phase::Idle {
            if self.refresh_backlight && !self.explicit && catalog.discovered && !catalog.busy {
                self.begin(catalog, false, false, now);
                self.targeted = true;
                self.refresh_backlight = false;
            } else {
                return false;
            }
        }
        if !client.idle() && reply.is_none() {
            return false;
        }
        if let Some(error) = client.feature_error() {
            self.discovery_failure(catalog, error);
            self.state = if matches!(
                error,
                Error::HidppReportsUnavailable | Error::HidppProtocolUnsupported
            ) {
                SettingsState::Unsupported
            } else {
                SettingsState::Error
            };
            return true;
        }
        if self.phase != Phase::Handlers {
            if let Some(reply) = reply {
                let result = reply
                    .map_err(ExchangeError::code)
                    .and_then(|r| self.discovery_response(catalog, r.bytes()));
                if let Err(e) = result {
                    self.discovery_failure(catalog, e);
                    return true;
                }
            }
            match self.phase {
                Phase::Root => {
                    self.send(client, 0, 0, &[0, 1], now);
                }
                Phase::Count => {
                    self.send(client, self.feature_set, 0, &[], now);
                }
                Phase::Feature => {
                    self.send(
                        client,
                        self.feature_set,
                        1,
                        &[self.enumerate_index as u8],
                        now,
                    );
                }
                Phase::Version => {
                    let id = catalog.features.last().unwrap().id.0;
                    self.send(client, 0, 0, &id.to_be_bytes(), now);
                }
                Phase::Handlers => {}
                Phase::Idle => {}
            }
            if self.phase != Phase::Handlers {
                // Enumeration advances internally; state/errors and published
                // setting rows notify the owner when they actually change.
                return false;
            }
        } else if let Some(reply) = reply {
            // Capabilities are static; events must not discard their response.
            if self.current() == Some(Handler::Battery)
                && self.stage == Stage::Initial
                && Handler::Battery
                    .feature(catalog)
                    .is_some_and(|f| matches!(f.id, Id::BATTERY | Id::UNIFIED_BATTERY))
            {
                self.read_epoch = self.event_epoch;
            }
            match reply {
                _ if self.current() == Some(Handler::Battery)
                    && self.read_epoch != self.event_epoch =>
                {
                    self.advance();
                }
                Err(e) => {
                    let uncertain = self.setter_sent
                        && (self.stage != Stage::Write || !matches!(e, ExchangeError::Device(_)));
                    self.handler_failed(catalog, e.code(), uncertain);
                    self.advance();
                }
                Ok(_) if self.stage == Stage::Write => self.stage = Stage::Readback,
                Ok(_) if self.read_epoch != self.event_epoch => {}
                Ok(response) => match self.parse(catalog, response.bytes(), now) {
                    Err(e) => {
                        self.handler_failed(catalog, e, self.setter_sent);
                        self.advance();
                    }
                    Ok(false) => {}
                    Ok(true) => {
                        self.raw.fill(0);
                        self.raw[..response.bytes().len()].copy_from_slice(response.bytes());
                        if self.wrote {
                            for r in &mut catalog.records {
                                if self.matches(r, true) && r.differs_from_saved() != Some(false) {
                                    r.changed.set(true);
                                    r.state = SettingState::Error;
                                    r.error = Some(Error::ReadbackMismatch);
                                }
                            }
                            self.handler_results(catalog, true);
                            self.advance();
                        } else if self.apply
                            && catalog.records.iter().any(|r| self.matches(r, true))
                        {
                            match self.validate_requested(catalog) {
                                Err(e) => {
                                    self.handler_failed(catalog, e, false);
                                    self.advance();
                                }
                                Ok(_)
                                    if !catalog.records.iter().any(|r| {
                                        self.matches(r, true)
                                            && r.differs_from_saved() != Some(false)
                                    }) =>
                                {
                                    self.handler_results(catalog, false);
                                    self.advance();
                                }
                                Ok(_) => self.stage = Stage::Write,
                            }
                        } else {
                            self.handler_results(catalog, false);
                            self.advance();
                        }
                    }
                },
            }
        }
        if !client.idle() {
            return true;
        }
        while let Some(handler) = self.current() {
            if handler.feature(catalog).is_some()
                && (handler != Handler::Battery || catalog.info.battery.vendor())
                && (catalog.enabled
                    || !matches!(
                        handler,
                        Handler::Firmware | Handler::Name | Handler::Battery
                    ))
                && (handler != Handler::Dpi1 || self.sensor_count >= 2)
                && (!self.targeted || handler == Handler::Backlight)
                && (!self.information_only
                    || matches!(
                        handler,
                        Handler::Firmware | Handler::Name | Handler::Battery
                    ))
                && (self.discover
                    || !self.apply
                    || catalog.records.iter().any(|r| self.matches(r, true)))
            {
                break;
            }
            self.advance();
        }
        let Some(handler) = self.current() else {
            self.finish(catalog);
            return true;
        };
        let f = handler.feature(catalog).unwrap();
        if self.stage == Stage::Write {
            if !catalog.enabled {
                self.handler_failed(catalog, Error::HidppDisabled, false);
                self.advance();
                return true;
            }
            if self.read_epoch != self.event_epoch {
                self.stage = Stage::Config;
            } else {
                match self.setter(catalog) {
                    Err(e) => {
                        self.handler_failed(catalog, e, false);
                        self.advance();
                    }
                    Ok((function, packet, length)) => {
                        if self.send(client, f.index.0, function, &packet[..length], now) {
                            self.wrote = true;
                            for r in &mut catalog.records {
                                if self.matches(r, handler == Handler::Wheel) {
                                    r.changed.set(true);
                                    r.fresh = false;
                                    if self.requested & bit(r.metadata.key) != 0 {
                                        r.state = SettingState::Applying;
                                        r.error = None;
                                    }
                                }
                            }
                        }
                    }
                }
                return true;
            }
        }
        let (function, parameter) = self.read(handler, f);
        if self.send(
            client,
            f.index.0,
            function,
            if f.id == Id::SOLAR {
                &[1, 1]
            } else {
                parameter.as_slice()
            },
            now,
        ) {
            self.read_epoch = self.event_epoch;
        }
        observed
    }
}

fn metadata(key: Key, f: Feature) -> Metadata {
    Metadata {
        key,
        feature: f.id,
        revision: f.version,
        scope: SettingScope::Device,
        choices: Box::new([]),
        range: None,
    }
}
fn choices(mut m: Metadata, values: &[u16]) -> Metadata {
    m.choices = values.into();
    m
}
fn range(mut m: Metadata, min: u16, max: u16, step: u16) -> Metadata {
    m.range = Some(Range { min, max, step });
    m
}
fn publish(
    catalog: &mut Catalog,
    metadata: Metadata,
    writable: bool,
    value: Option<Observed>,
    now: u64,
    source: ObservationSource,
) -> Result<(), Error> {
    let index = if let Some(i) = catalog
        .records
        .iter()
        .position(|r| r.metadata.key == metadata.key)
    {
        i
    } else {
        if catalog.records.len() == MAX_RECORDS {
            return Err(Error::SettingsLimit);
        }
        catalog
            .records
            .try_reserve_exact(1)
            .map_err(|_| Error::Capacity)?;
        let mut record = Record::new(metadata, writable);
        if let Some(value) = value {
            observation(&mut record, value, now, source);
        }
        catalog.records.push(record);
        catalog.catalog_changed = true;
        return Ok(());
    };
    let r = &mut catalog.records[index];
    if r.metadata != metadata || r.writable != writable || !r.available {
        catalog.catalog_changed = true;
    }
    r.changed.set(true);
    if let Some(p) = &r.preference
        && (!writable
            || metadata.feature != p.metadata.feature
            || metadata.scope != p.metadata.scope)
    {
        r.metadata = p.metadata.clone();
        r.writable = true;
        r.available = false;
        failed(r, Error::UnsupportedSetting, false);
        return Ok(());
    }
    if r.metadata != metadata {
        r.metadata = metadata;
    }
    r.writable = writable;
    r.available = true;
    if let Some(value) = value {
        observation(r, value, now, source);
    }
    if let Some(p) = &r.preference
        && !r.metadata.accepts(p.value)
    {
        r.state = SettingState::Unsupported;
        r.error = Some(Error::UnsupportedSetting);
    }
    Ok(())
}
fn number(
    catalog: &mut Catalog,
    m: Metadata,
    writable: bool,
    value: u16,
    now: u64,
    source: ObservationSource,
) -> Result<(), Error> {
    publish(
        catalog,
        m,
        writable,
        Some(Observed::Number(value)),
        now,
        source,
    )
}
fn observe_key(catalog: &mut Catalog, key: Key, value: u16, now: u64, source: ObservationSource) {
    if let Some(r) = row(catalog, key)
        && r.available
    {
        observation(r, Observed::Number(value), now, source);
    }
}
