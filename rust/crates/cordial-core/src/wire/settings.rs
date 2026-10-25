//! Typed serial settings over the device catalog and its persisted metadata.
use crate::{
    compact::{Metadata, Record},
    model::settings::{SettingKey, SettingState, SettingValue},
    settings::{Catalog, Change},
};
use alloc::vec::Vec;
use cordial_protocol as p;

/// Public enum members follow the documented persisted choice order, starting after UNKNOWN.
pub(super) fn enum_value(key: SettingKey, value: &SettingValue) -> Option<i32> {
    match value {
        SettingValue::Null => None,
        SettingValue::Text(value) => Some(
            key.enum_values()
                .iter()
                .position(|v| *v == value)
                .map_or(0, |i| i as i32 + 1),
        ),
        _ => Some(0),
    }
}
pub(super) fn enum_observed(record: &Record) -> Option<i32> {
    match record.observed {
        crate::compact::Observed::Missing => None,
        _ => Some(enum_value(record.metadata.key, &observed(record)).unwrap_or(0)),
    }
}
fn enum_input(key: SettingKey, value: i32) -> Result<SettingValue, p::Error> {
    value
        .checked_sub(1)
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| key.enum_values().get(i))
        .map(|v| SettingValue::Text((*v).into()))
        .ok_or_else(bad_args)
}
fn bad_args() -> p::Error {
    p::Error {
        code: p::ErrorCode::BadArgs as i32,
        ..Default::default()
    }
}
fn too_long() -> p::Error {
    p::Error {
        code: p::ErrorCode::TooLong as i32,
        ..Default::default()
    }
}

/// Identity of the complete object containing this internal field.
pub fn settings_ref(key: SettingKey) -> p::SettingsRef {
    use SettingKey::*;
    let (group, index) = match key {
        KeyboardPlatform | FnRowDefault => (p::SettingsGroup::Keyboard, None),
        PowerAutoOff => (p::SettingsGroup::Power, None),
        PointerDpi0 | PointerDpi1 => (
            p::SettingsGroup::PointerSensor,
            Some(u32::from(key == PointerDpi1)),
        ),
        WheelMode | WheelThreshold | WheelInvert | WheelInfo => (p::SettingsGroup::Wheel, None),
        ThumbwheelInvert => (p::SettingsGroup::Thumbwheel, None),
        _ => (p::SettingsGroup::Backlight, None),
    };
    p::SettingsRef {
        integration: p::IntegrationKind::Hidpp as i32,
        group: group as i32,
        index,
    }
}
pub fn settings_order(reference: &p::SettingsRef) -> (i32, i32, Option<u32>) {
    (reference.integration, reference.group, reference.index)
}
pub fn object_ref(object: &p::SettingsObject) -> p::SettingsRef {
    use p::settings_object::Group;
    let (group, index) = match object.group.as_ref() {
        Some(Group::Keyboard(_)) => (p::SettingsGroup::Keyboard, None),
        Some(Group::Power(_)) => (p::SettingsGroup::Power, None),
        Some(Group::Backlight(_)) => (p::SettingsGroup::Backlight, None),
        Some(Group::PointerSensor(s)) => (p::SettingsGroup::PointerSensor, Some(s.index)),
        Some(Group::Wheel(_)) => (p::SettingsGroup::Wheel, None),
        Some(Group::Thumbwheel(_)) => (p::SettingsGroup::Thumbwheel, None),
        None => (p::SettingsGroup::Unknown, None),
    };
    p::SettingsRef {
        integration: object.integration,
        group: group as i32,
        index,
    }
}
/// One bit per domain, with separate bits for both supported sensors.
pub fn settings_bit(reference: &p::SettingsRef) -> u32 {
    if reference.integration != p::IntegrationKind::Hidpp as i32 {
        return 0;
    }
    match p::SettingsGroup::try_from(reference.group) {
        Ok(p::SettingsGroup::Keyboard) if reference.index.is_none() => 1,
        Ok(p::SettingsGroup::Power) if reference.index.is_none() => 2,
        Ok(p::SettingsGroup::Backlight) if reference.index.is_none() => 4,
        Ok(p::SettingsGroup::PointerSensor) if reference.index == Some(0) => 8,
        Ok(p::SettingsGroup::PointerSensor) if reference.index == Some(1) => 16,
        Ok(p::SettingsGroup::Wheel) if reference.index.is_none() => 32,
        Ok(p::SettingsGroup::Thumbwheel) if reference.index.is_none() => 64,
        _ => 0,
    }
}
fn status(record: &Record) -> Option<p::SettingStatus> {
    record.preference.as_ref()?;
    use p::setting_status::Result as R;
    let result = if !record.writable {
        R::State(p::SettingState::Unsupported as i32)
    } else {
        match record.state {
            SettingState::Unmanaged | SettingState::Pending | SettingState::Applying => {
                R::State(p::SettingState::Pending as i32)
            }
            SettingState::Applied => R::State(p::SettingState::Applied as i32),
            SettingState::ChangedOnDevice => R::State(p::SettingState::ChangedOnDevice as i32),
            SettingState::Unsupported => R::State(p::SettingState::Unsupported as i32),
            SettingState::Uncertain => R::Error(p::ErrorCode::Timeout as i32),
            SettingState::Error => R::Error(super::error_code(
                record
                    .error
                    .unwrap_or(crate::model::errors::ErrorCode::InternalError),
            ) as i32),
        }
    };
    Some(p::SettingStatus {
        result: Some(result),
    })
}
fn observed(record: &Record) -> SettingValue {
    record.observed.wire(record.metadata.key)
}
fn saved(record: &Record) -> SettingValue {
    record.preference.as_ref().map_or(SettingValue::Null, |p| {
        crate::compact::scalar(record.metadata.key, p.value)
    })
}
fn metadata(record: &Record) -> &Metadata {
    if !record.available || !record.writable {
        record
            .preference
            .as_ref()
            .map_or(&record.metadata, |p| &p.metadata)
    } else {
        &record.metadata
    }
}
fn bool_setting(record: &Record) -> p::BoolSetting {
    p::BoolSetting {
        value: super::boolean(&observed(record)),
        saved: super::boolean(&saved(record)),
        status: status(record),
    }
}
fn integer_setting(record: &Record) -> Result<p::IntegerSetting, p::Error> {
    let metadata = metadata(record);
    let mut choices = metadata.choices.to_vec();
    choices.sort_unstable();
    choices.dedup();
    let limits = if !choices.is_empty() {
        let step = choices.get(1).map(|v| v - choices[0]);
        if step.is_some_and(|step| choices.windows(2).all(|w| w[1] - w[0] == step)) {
            Some(p::integer_setting::Limits::Range(p::IntegerRange {
                min: choices[0].into(),
                max: (*choices.last().unwrap()).into(),
                step: step.unwrap().into(),
            }))
        } else {
            if choices.len() > 6 {
                return Err(too_long());
            }
            Some(p::integer_setting::Limits::Choices(p::IntegerChoices {
                values: choices.into_iter().map(i64::from).collect(),
            }))
        }
    } else {
        metadata.range.map(|r| {
            p::integer_setting::Limits::Range(p::IntegerRange {
                min: r.min.into(),
                max: r.max.into(),
                step: r.step.into(),
            })
        })
    };
    let integer = |v| match v {
        SettingValue::Integer(n) => Some(n),
        _ => None,
    };
    Ok(p::IntegerSetting {
        value: integer(observed(record)),
        saved: integer(saved(record)),
        status: status(record),
        limits,
    })
}
macro_rules! enum_setting {
    ($record:expr, $kind:ident) => {{
        let record = $record;
        let key = record.metadata.key;
        p::$kind {
            value: enum_observed(record),
            saved: enum_value(key, &saved(record)),
            status: status(record),
            choices: metadata(record)
                .choices
                .iter()
                .filter_map(|v| enum_value(key, &crate::compact::scalar(key, *v)))
                .filter(|v| *v != 0)
                .collect(),
        }
    }};
}

/// Complete objects in integration, group and component order. Metadata is never truncated.
pub fn settings(catalog: &Catalog) -> Result<Vec<p::SettingsObject>, p::Error> {
    let mut objects = Vec::new();
    while let Some(object) = next_settings(catalog, objects.last().map(object_ref).as_ref())? {
        objects.push(object);
    }
    Ok(objects)
}

/// Builds only the next complete object from the current catalog.
pub fn next_settings(
    catalog: &Catalog,
    after: Option<&p::SettingsRef>,
) -> Result<Option<p::SettingsObject>, p::Error> {
    use p::settings_object::Group;
    let visible = || {
        catalog
            .records()
            .iter()
            .filter(|r| (r.writable && r.available) || r.preference.is_some())
    };
    let Some(reference) = visible()
        .map(|r| settings_ref(r.metadata.key))
        .filter(|r| after.is_none_or(|after| settings_order(r) > settings_order(after)))
        .min_by_key(settings_order)
    else {
        return Ok(None);
    };
    let group = match p::SettingsGroup::try_from(reference.group).unwrap() {
        p::SettingsGroup::Keyboard => Group::Keyboard(Default::default()),
        p::SettingsGroup::Power => Group::Power(Default::default()),
        p::SettingsGroup::Backlight => Group::Backlight(Default::default()),
        p::SettingsGroup::PointerSensor => Group::PointerSensor(p::PointerSensorSettings {
            index: reference.index.unwrap(),
            ..Default::default()
        }),
        p::SettingsGroup::Wheel => Group::Wheel(Default::default()),
        p::SettingsGroup::Thumbwheel => Group::Thumbwheel(Default::default()),
        p::SettingsGroup::Unknown => unreachable!(),
    };
    let mut object = p::SettingsObject {
        integration: reference.integration,
        group: Some(group),
    };
    for record in visible().filter(|r| settings_ref(r.metadata.key) == reference) {
        match (record.metadata.key, object.group.as_mut().unwrap()) {
            (SettingKey::KeyboardPlatform, Group::Keyboard(s)) => {
                s.platform = Some(enum_setting!(record, KeyboardPlatformSetting))
            }
            (SettingKey::FnRowDefault, Group::Keyboard(s)) => {
                s.function_row = Some(enum_setting!(record, FunctionRowModeSetting))
            }
            (SettingKey::PowerAutoOff, Group::Power(s)) => {
                s.auto_off = Some(integer_setting(record)?)
            }
            (SettingKey::BacklightEnabled, Group::Backlight(s)) => {
                s.enabled = Some(bool_setting(record))
            }
            (SettingKey::BacklightMode, Group::Backlight(s)) => {
                s.mode = Some(enum_setting!(record, BacklightModeSetting))
            }
            (SettingKey::BacklightLevel, Group::Backlight(s)) => {
                s.brightness = Some(integer_setting(record)?)
            }
            (SettingKey::BacklightDelayHandsOut, Group::Backlight(s)) => {
                s.delay.get_or_insert_default().hands_away = Some(integer_setting(record)?)
            }
            (SettingKey::BacklightDelayHandsIn, Group::Backlight(s)) => {
                s.delay.get_or_insert_default().hands_nearby = Some(integer_setting(record)?)
            }
            (SettingKey::BacklightDelayPowered, Group::Backlight(s)) => {
                s.delay.get_or_insert_default().external_power = Some(integer_setting(record)?)
            }
            (SettingKey::BacklightPowerOn, Group::Backlight(s)) => {
                s.startup_effect = Some(bool_setting(record))
            }
            (SettingKey::BacklightCrown, Group::Backlight(s)) => {
                s.dial_effect = Some(bool_setting(record))
            }
            (SettingKey::BacklightPowerSave, Group::Backlight(s)) => {
                s.power_save = Some(bool_setting(record))
            }
            (SettingKey::BacklightEffect, Group::Backlight(s)) => {
                s.effect = Some(enum_setting!(record, BacklightEffectSetting))
            }
            (SettingKey::PointerDpi0, Group::PointerSensor(s)) => {
                s.dpi = Some(integer_setting(record)?)
            }
            (SettingKey::PointerDpi1, Group::PointerSensor(s)) => {
                s.dpi = Some(integer_setting(record)?)
            }
            (SettingKey::WheelMode, Group::Wheel(s)) => {
                s.mode = Some(enum_setting!(record, WheelModeSetting))
            }
            (SettingKey::WheelThreshold, Group::Wheel(s)) => {
                s.free_spin_threshold = Some(integer_setting(record)?)
            }
            (SettingKey::WheelInvert, Group::Wheel(s)) => s.invert = Some(bool_setting(record)),
            (SettingKey::ThumbwheelInvert, Group::Thumbwheel(s)) => {
                s.invert = Some(bool_setting(record))
            }
            _ => continue,
        }
    }
    Ok(Some(object))
}

/// Validates typed updates and keeps the final write to each internal field.
pub fn settings_changes(updates: &[p::SettingsUpdate]) -> Result<Vec<Change>, p::Error> {
    use p::settings_update::Group;
    let mut parsed: Vec<Change> = Vec::new();
    macro_rules! field {
        ($setting:expr, $key:ident, $module:ident, $value:expr, $count:ident) => {
            if let Some(setting) = $setting {
                use p::$module::Change as C;
                let key = SettingKey::$key;
                let value = match setting.change.as_ref().ok_or_else(bad_args)? {
                    C::Value(value) => Some(($value)(key, *value)?),
                    C::Forget(_) => None,
                };
                parsed.retain(|c| c.key != key);
                parsed.push(Change { key, value });
                $count += 1;
            }
        };
    }
    for update in updates {
        if update.integration != p::IntegrationKind::Hidpp as i32 {
            return Err(p::Error {
                code: p::ErrorCode::NotFound as i32,
                ..Default::default()
            });
        }
        let mut count = 0;
        match update.group.as_ref().ok_or_else(bad_args)? {
            Group::Keyboard(s) => {
                field!(
                    s.platform.as_ref(),
                    KeyboardPlatform,
                    keyboard_platform_setting_update,
                    enum_input,
                    count
                );
                field!(
                    s.function_row.as_ref(),
                    FnRowDefault,
                    function_row_mode_setting_update,
                    enum_input,
                    count
                );
            }
            Group::Power(s) => {
                field!(
                    s.auto_off.as_ref(),
                    PowerAutoOff,
                    integer_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Integer(v)),
                    count
                );
            }
            Group::Backlight(s) => {
                field!(
                    s.enabled.as_ref(),
                    BacklightEnabled,
                    bool_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Bool(v)),
                    count
                );
                field!(
                    s.mode.as_ref(),
                    BacklightMode,
                    backlight_mode_setting_update,
                    enum_input,
                    count
                );
                field!(
                    s.brightness.as_ref(),
                    BacklightLevel,
                    integer_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Integer(v)),
                    count
                );
                field!(
                    s.delay.as_ref().and_then(|d| d.hands_away.as_ref()),
                    BacklightDelayHandsOut,
                    integer_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Integer(v)),
                    count
                );
                field!(
                    s.delay.as_ref().and_then(|d| d.hands_nearby.as_ref()),
                    BacklightDelayHandsIn,
                    integer_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Integer(v)),
                    count
                );
                field!(
                    s.delay.as_ref().and_then(|d| d.external_power.as_ref()),
                    BacklightDelayPowered,
                    integer_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Integer(v)),
                    count
                );
                field!(
                    s.startup_effect.as_ref(),
                    BacklightPowerOn,
                    bool_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Bool(v)),
                    count
                );
                field!(
                    s.dial_effect.as_ref(),
                    BacklightCrown,
                    bool_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Bool(v)),
                    count
                );
                field!(
                    s.power_save.as_ref(),
                    BacklightPowerSave,
                    bool_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Bool(v)),
                    count
                );
                field!(
                    s.effect.as_ref(),
                    BacklightEffect,
                    backlight_effect_setting_update,
                    enum_input,
                    count
                );
            }
            Group::PointerSensor(s) => {
                if s.index > 1 {
                    return Err(p::Error {
                        code: p::ErrorCode::NotFound as i32,
                        ..Default::default()
                    });
                }
                if s.index == 0 {
                    field!(
                        s.dpi.as_ref(),
                        PointerDpi0,
                        integer_setting_update,
                        |_, v| Ok::<_, p::Error>(SettingValue::Integer(v)),
                        count
                    );
                }
                if s.index == 1 {
                    field!(
                        s.dpi.as_ref(),
                        PointerDpi1,
                        integer_setting_update,
                        |_, v| Ok::<_, p::Error>(SettingValue::Integer(v)),
                        count
                    );
                }
            }
            Group::Wheel(s) => {
                field!(
                    s.mode.as_ref(),
                    WheelMode,
                    wheel_mode_setting_update,
                    enum_input,
                    count
                );
                field!(
                    s.free_spin_threshold.as_ref(),
                    WheelThreshold,
                    integer_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Integer(v)),
                    count
                );
                field!(
                    s.invert.as_ref(),
                    WheelInvert,
                    bool_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Bool(v)),
                    count
                );
            }
            Group::Thumbwheel(s) => {
                field!(
                    s.invert.as_ref(),
                    ThumbwheelInvert,
                    bool_setting_update,
                    |_, v| Ok::<_, p::Error>(SettingValue::Bool(v)),
                    count
                );
            }
        }
        if count == 0 {
            return Err(bad_args());
        }
    }
    if parsed.is_empty() {
        return Err(bad_args());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compact::{Observed, Preference, Range},
        model::{
            hidpp::{FeatureId, FeatureRevision},
            settings::SettingScope,
        },
    };
    use alloc::{boxed::Box, rc::Rc, vec};

    fn record(key: SettingKey) -> Record {
        let mut metadata = Metadata {
            key,
            feature: FeatureId::BACKLIGHT,
            revision: FeatureRevision(3),
            scope: SettingScope::Device,
            choices: Box::new([]),
            range: None,
        };
        match key.kind() {
            crate::model::settings::SettingType::Enum => {
                metadata.choices = (0..key.enum_values().len())
                    .map(|i| i as u16 + u16::from(key == SettingKey::WheelMode))
                    .collect()
            }
            crate::model::settings::SettingType::Integer => {
                metadata.range = Some(Range {
                    min: 0,
                    max: u16::MAX,
                    step: 1,
                })
            }
            _ => {}
        }
        let mut record = Record::new(metadata, true);
        record.observed = Observed::Number(u16::from(key == SettingKey::WheelMode));
        record.preference = Some(Rc::new(Preference {
            metadata: record.metadata.clone(),
            value: u16::from(key == SettingKey::WheelMode),
        }));
        record.state = SettingState::Pending;
        record
    }

    #[test]
    fn complete_catalog_preserves_typed_presence() {
        let mut catalog = Catalog {
            records: SettingKey::ALL
                .into_iter()
                .filter(|key| {
                    !matches!(
                        key,
                        SettingKey::WheelInfo
                            | SettingKey::BacklightStatus
                            | SettingKey::BacklightCurrentLevel
                    )
                })
                .map(record)
                .collect(),
            ..Default::default()
        };
        let objects = settings(&catalog).unwrap();
        assert_eq!(objects.len(), 7);
        let refs: Vec<_> = objects.iter().map(object_ref).collect();
        assert!(
            refs.windows(2)
                .all(|w| settings_order(&w[0]) < settings_order(&w[1]))
        );
        assert_eq!(refs[3].index, Some(0));
        assert_eq!(refs[4].index, Some(1));
        let Some(p::settings_object::Group::Backlight(backlight)) = &objects[2].group else {
            panic!()
        };
        let enabled = backlight.enabled.as_ref().unwrap();
        assert_eq!((enabled.value, enabled.saved), (Some(false), Some(false)));
        assert!(enabled.status.is_some());
        catalog.connection(false, false);
        let objects = settings(&catalog).unwrap();
        let Some(p::settings_object::Group::Backlight(backlight)) = &objects[2].group else {
            panic!()
        };
        assert_eq!(backlight.enabled.as_ref().unwrap().value, None);
        assert_eq!(backlight.enabled.as_ref().unwrap().saved, Some(false));
    }

    #[test]
    fn read_only_saved_preferences_keep_metadata_and_unsupported_status() {
        let mut catalog = Catalog::default();
        let mut value = record(SettingKey::BacklightEnabled);
        value.writable = false;
        catalog.records.push(value);
        let objects = settings(&catalog).unwrap();
        let Some(p::settings_object::Group::Backlight(backlight)) = &objects[0].group else {
            panic!()
        };
        let enabled = backlight.enabled.as_ref().unwrap();
        assert_eq!(enabled.value, Some(false));
        assert_eq!(enabled.saved, Some(false));
        assert_eq!(
            enabled.status.as_ref().unwrap().result,
            Some(p::setting_status::Result::State(
                p::SettingState::Unsupported as i32
            ))
        );
        catalog.records[0].preference = None;
        assert!(settings(&catalog).unwrap().is_empty());
    }

    #[test]
    fn enum_readings_preserve_unknown_and_every_persisted_choice() {
        for key in [
            SettingKey::KeyboardPlatform,
            SettingKey::FnRowDefault,
            SettingKey::BacklightMode,
            SettingKey::BacklightEffect,
            SettingKey::BacklightStatus,
            SettingKey::WheelMode,
        ] {
            for (i, choice) in key.enum_values().iter().enumerate() {
                let value = SettingValue::Text((*choice).into());
                assert_eq!(enum_value(key, &value), Some(i as i32 + 1));
                assert_eq!(enum_input(key, i as i32 + 1).unwrap(), value);
            }
            assert!(enum_input(key, 0).is_err());
            assert!(enum_input(key, i32::MAX).is_err());
            let mut record = record(key);
            record.observed = Observed::Number(u16::MAX);
            assert_eq!(enum_observed(&record), Some(0));
            record.observed = Observed::Missing;
            assert_eq!(enum_observed(&record), None);
        }
    }

    #[test]
    fn integer_metadata_is_lossless_or_explicitly_refused() {
        let mut value = record(SettingKey::PointerDpi0);
        value.metadata.range = None;
        value.metadata.choices = (1..=32).map(|n| n * 100).collect();
        assert_eq!(
            integer_setting(&value).unwrap().limits,
            Some(p::integer_setting::Limits::Range(p::IntegerRange {
                min: 100,
                max: 3200,
                step: 100
            }))
        );
        value.metadata.choices = vec![100, 200, 400, 800, 1600, 3200].into_boxed_slice();
        assert_eq!(
            integer_setting(&value).unwrap().limits,
            Some(p::integer_setting::Limits::Choices(p::IntegerChoices {
                values: vec![100, 200, 400, 800, 1600, 3200]
            }))
        );
        value.metadata.choices = vec![100, 200, 400, 800, 1600, 3200, 6400].into_boxed_slice();
        assert_eq!(
            integer_setting(&value).unwrap_err().code,
            p::ErrorCode::TooLong as i32
        );
    }

    #[test]
    fn partial_updates_preserve_zero_false_forget_and_reject_empty_fields() {
        let updates = vec![
            p::SettingsUpdate {
                integration: p::IntegrationKind::Hidpp as i32,
                group: Some(p::settings_update::Group::Backlight(
                    p::BacklightSettingsUpdate {
                        enabled: Some(p::BoolSettingUpdate {
                            change: Some(p::bool_setting_update::Change::Value(false)),
                        }),
                        ..Default::default()
                    },
                )),
            },
            p::SettingsUpdate {
                integration: p::IntegrationKind::Hidpp as i32,
                group: Some(p::settings_update::Group::PointerSensor(
                    p::PointerSensorSettingsUpdate {
                        index: 0,
                        dpi: Some(p::IntegerSettingUpdate {
                            change: Some(p::integer_setting_update::Change::Value(0)),
                        }),
                    },
                )),
            },
        ];
        let changes = settings_changes(&updates).unwrap();
        assert_eq!(
            changes,
            vec![
                Change {
                    key: SettingKey::BacklightEnabled,
                    value: Some(SettingValue::Bool(false))
                },
                Change {
                    key: SettingKey::PointerDpi0,
                    value: Some(SettingValue::Integer(0))
                }
            ]
        );
        let mut updates = updates;
        let p::settings_update::Group::PointerSensor(sensor) = updates[1].group.as_mut().unwrap()
        else {
            panic!()
        };
        sensor.index = 2;
        assert_eq!(
            settings_changes(&updates).unwrap_err().code,
            p::ErrorCode::NotFound as i32
        );
        sensor_reset(&mut updates[1]);
        assert_eq!(
            settings_changes(&updates).unwrap_err().code,
            p::ErrorCode::BadArgs as i32
        );
        updates[1].group = Some(p::settings_update::Group::Backlight(
            p::BacklightSettingsUpdate {
                enabled: Some(p::BoolSettingUpdate {
                    change: Some(p::bool_setting_update::Change::Forget(p::SettingForget {})),
                }),
                ..Default::default()
            },
        ));
        assert_eq!(
            settings_changes(&updates).unwrap(),
            vec![Change {
                key: SettingKey::BacklightEnabled,
                value: None
            }]
        );
    }
    fn sensor_reset(update: &mut p::SettingsUpdate) {
        update.group = Some(p::settings_update::Group::PointerSensor(
            p::PointerSensorSettingsUpdate {
                index: 0,
                dpi: Some(p::IntegerSettingUpdate { change: None }),
            },
        ));
    }
}
