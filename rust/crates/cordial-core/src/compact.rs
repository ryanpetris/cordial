use crate::model::{
    errors::ErrorCode,
    hidpp::{Feature, FeatureFlags, FeatureId, FeatureIndex, FeatureRevision},
    settings::{
        ObservationSource, Setting, SettingKey, SettingScope, SettingState, SettingType,
        SettingValue,
    },
};
use crate::settings::{Error, MAX_CHOICES, MAX_TEXT_BYTES, SettingMetadata};
use alloc::{boxed::Box, rc::Rc, string::ToString, vec::Vec};
use core::{cell::Cell, num::NonZeroU64};

/// Enumeration order supplies the feature index; support is a handler property.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeatureEntry {
    pub id: FeatureId,
    pub version: FeatureRevision,
    pub flags: FeatureFlags,
}
impl FeatureEntry {
    pub fn supported(self) -> bool {
        crate::features::supported(self.id, self.flags)
    }
    pub fn wire(self, index: usize) -> Feature {
        Feature {
            index: FeatureIndex(index as u8),
            id: self.id,
            version: self.version,
            flags: self.flags,
            supported: self.supported(),
        }
    }
}
const _: () = assert!(core::mem::size_of::<FeatureEntry>() == 4);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Range {
    pub min: u16,
    pub max: u16,
    pub step: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub key: SettingKey,
    pub feature: FeatureId,
    pub revision: FeatureRevision,
    pub scope: SettingScope,
    pub choices: Box<[u16]>,
    pub range: Option<Range>,
}

pub fn number(key: SettingKey, value: &SettingValue) -> Result<u16, Error> {
    match (key.kind(), value) {
        (SettingType::Bool, SettingValue::Bool(value)) => Ok(u16::from(*value)),
        (SettingType::Integer, SettingValue::Integer(value)) => {
            u16::try_from(*value).map_err(|_| Error::InvalidValue)
        }
        (SettingType::Enum, SettingValue::Text(value)) => key
            .enum_values()
            .iter()
            .position(|&v| v == value)
            .map(|i| i as u16 + u16::from(key == SettingKey::WheelMode))
            .ok_or(Error::InvalidValue),
        _ => Err(Error::InvalidValue),
    }
}
pub fn scalar(key: SettingKey, value: u16) -> SettingValue {
    match key.kind() {
        SettingType::Bool => SettingValue::Bool(value != 0),
        SettingType::Integer => SettingValue::Integer(value.into()),
        SettingType::Enum => value
            .checked_sub(u16::from(key == SettingKey::WheelMode))
            .and_then(|i| key.enum_values().get(i as usize))
            .map_or(SettingValue::Null, |v| SettingValue::Text((*v).into())),
        SettingType::Text => SettingValue::Null,
    }
}

impl Metadata {
    pub fn from_wire(record: &Setting) -> Result<Self, Error> {
        if record.kind != record.key.kind() || record.choices.len() > MAX_CHOICES {
            return Err(Error::InvalidValue);
        }
        let choices = record
            .choices
            .iter()
            .map(|v| number(record.key, v))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let range = match (record.min, record.max, record.step) {
            (None, None, None) => None,
            (Some(min), Some(max), Some(step))
                if record.kind == SettingType::Integer
                    && choices.is_empty()
                    && step > 0
                    && max >= min =>
            {
                Some(Range {
                    min: u16::try_from(min).map_err(|_| Error::InvalidValue)?,
                    max: u16::try_from(max).map_err(|_| Error::InvalidValue)?,
                    step: u16::try_from(step).map_err(|_| Error::InvalidValue)?,
                })
            }
            _ => return Err(Error::InvalidValue),
        };
        Ok(Self {
            key: record.key,
            feature: record.feature,
            revision: record.feature_version,
            scope: record.scope,
            choices,
            range,
        })
    }
    pub fn wire(&self) -> SettingMetadata {
        SettingMetadata {
            key: self.key,
            kind: self.key.kind(),
            feature: self.feature,
            feature_version: self.revision,
            scope: self.scope,
            choices: self
                .choices
                .iter()
                .map(|&value| scalar(self.key, value))
                .collect(),
            min: self.range.map(|r| r.min.into()),
            max: self.range.map(|r| r.max.into()),
            step: self.range.map(|r| r.step.into()),
        }
    }
    pub fn accepts(&self, value: u16) -> bool {
        if !self.choices.is_empty() {
            return self.choices.contains(&value);
        }
        match self.key.kind() {
            SettingType::Bool => value <= 1,
            SettingType::Integer => self.range.is_some_and(|r| {
                r.step != 0
                    && value >= r.min
                    && value <= r.max
                    && (value - r.min).is_multiple_of(r.step)
            }),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Observed {
    #[default]
    Missing,
    Number(u16),
    Text(Rc<str>),
}
impl Observed {
    pub fn from_wire(key: SettingKey, value: &SettingValue) -> Result<Self, Error> {
        if let SettingValue::Null = value {
            return Ok(Self::Missing);
        }
        if key.kind() == SettingType::Text
            && let SettingValue::Text(text) = value
        {
            if text.len() > MAX_TEXT_BYTES {
                return Err(Error::Limit);
            }
            return Ok(Self::Text(Rc::from(text.as_str())));
        }
        Ok(Self::Number(number(key, value)?))
    }
    pub fn wire(&self, key: SettingKey) -> SettingValue {
        match self {
            Self::Missing => SettingValue::Null,
            Self::Number(value) => scalar(key, *value),
            Self::Text(text) => SettingValue::Text(text.to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preference {
    pub metadata: Metadata,
    pub value: u16,
}
impl Preference {
    pub fn valid(&self) -> bool {
        self.metadata.accepts(self.value)
            && self
                .metadata
                .wire()
                .valid(&scalar(self.metadata.key, self.value))
    }
}

/// Numeric/enum rows own no text buffers. Saved metadata exists only for managed rows.
#[derive(Clone, Debug)]
pub struct Record {
    pub metadata: Metadata,
    pub observed: Observed,
    pub preference: Option<Rc<Preference>>,
    pub observed_at: Option<NonZeroU64>,
    pub source: Option<ObservationSource>,
    pub state: SettingState,
    pub error: Option<ErrorCode>,
    pub writable: bool,
    pub fresh: bool,
    pub available: bool,
    // Notification bookkeeping is not part of a command snapshot.
    pub(crate) changed: Cell<bool>,
}
impl Record {
    pub(crate) fn observe(&mut self, value: Observed, now: u64, source: ObservationSource) {
        self.changed.set(true);
        if self.observed != value {
            self.observed = value;
        }
        self.observed_at = NonZeroU64::new(now.saturating_add(1));
        self.source = Some(source);
        self.fresh = true;
        self.error = None;
        self.state = if self
            .preference
            .as_ref()
            .is_some_and(|p| !self.metadata.accepts(p.value))
        {
            self.error = Some(ErrorCode::UnsupportedSetting);
            SettingState::Unsupported
        } else {
            match self.differs_from_saved() {
                Some(true) => SettingState::ChangedOnDevice,
                Some(false) => SettingState::Applied,
                None => SettingState::Unmanaged,
            }
        };
    }
    pub fn new(metadata: Metadata, writable: bool) -> Self {
        Self {
            metadata,
            observed: Observed::Missing,
            preference: None,
            observed_at: None,
            source: None,
            state: SettingState::Unmanaged,
            error: None,
            writable,
            fresh: false,
            available: true,
            changed: Cell::new(true),
        }
    }
    pub fn differs_from_saved(&self) -> Option<bool> {
        let preference = self.preference.as_ref()?;
        if !self.fresh || self.observed == Observed::Missing {
            return None;
        }
        Some(self.observed != Observed::Number(preference.value))
    }
    pub fn from_wire(record: &Setting) -> Result<Self, Error> {
        let metadata = Metadata::from_wire(record)?;
        let preference = if record.managed {
            Some(Rc::new(Preference {
                value: number(record.key, &record.desired)?,
                metadata: metadata.clone(),
            }))
        } else {
            None
        };
        Ok(Self {
            metadata,
            observed: Observed::from_wire(record.key, &record.observed)?,
            preference,
            observed_at: record
                .observed_at_ms
                .and_then(|v| v.checked_add(1))
                .and_then(NonZeroU64::new),
            source: record.observation_source,
            state: record.state,
            error: record.error,
            writable: record.writable,
            fresh: record.fresh,
            available: true,
            changed: Cell::new(true),
        })
    }
    pub fn wire(&self) -> Setting {
        let metadata = self.metadata.wire();
        Setting {
            key: metadata.key,
            kind: metadata.kind,
            feature: metadata.feature,
            feature_version: metadata.feature_version,
            scope: metadata.scope,
            choices: metadata.choices,
            min: metadata.min,
            max: metadata.max,
            step: metadata.step,
            writable: self.writable,
            managed: self.preference.is_some(),
            fresh: self.fresh,
            desired: self
                .preference
                .as_ref()
                .map_or(SettingValue::Null, |p| scalar(self.metadata.key, p.value)),
            observed: self.observed.wire(self.metadata.key),
            observed_at_ms: self.observed_at.map(|v| v.get() - 1),
            observation_source: self.source,
            state: self.state,
            error: self.error,
        }
    }
}
