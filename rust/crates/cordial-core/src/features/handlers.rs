use super::*;
use crate::model::{info::InfoKey as I, settings::SettingValue as V};

fn be(p: &[u8]) -> u16 {
    u16::from_be_bytes([p[0], p[1]])
}
fn le(p: &[u8]) -> u16 {
    u16::from_le_bytes([p[0], p[1]])
}
fn require(condition: bool) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::HidppInvalidResponse)
    }
}
fn bytes(c: &mut Catalog, key: Key, f: Feature, p: &[u8], now: u64) -> Result<(), Error> {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(2 * p.len());
    for b in p {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    publish(
        c,
        metadata(key, f),
        false,
        Some(Observed::Text(s.into())),
        now,
        ObservationSource::Read,
    )
}

impl Engine {
    fn publish_platform(
        &self,
        c: &mut Catalog,
        f: Feature,
        platform: u8,
        now: u64,
        source: ObservationSource,
    ) -> Result<(), Error> {
        let mut m = metadata(Key::KeyboardPlatform, f);
        if f.id == Id::MULTI_PLATFORM {
            m.scope = SettingScope::CurrentHost;
        }
        m.choices = self
            .platforms
            .iter()
            .enumerate()
            .filter_map(|(i, &p)| {
                (p < 254 && self.platform_all_versions & (1 << i) != 0).then_some(i as u16)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if m.choices.is_empty() {
            return Err(Error::UnsupportedSetting);
        }
        let preferred = row(c, Key::KeyboardPlatform)
            .and_then(|r| r.preference.as_ref())
            .map(|p| p.value);
        let value = preferred
            .filter(|i| m.choices.contains(i) && self.platforms[usize::from(*i)] == platform)
            .or_else(|| {
                m.choices
                    .iter()
                    .copied()
                    .find(|i| self.platforms[usize::from(*i)] == platform)
            });
        publish(
            c,
            m,
            self.platform_writable,
            value.map(Observed::Number),
            now,
            source,
        )?;
        if value.is_none() {
            stale(c, Key::KeyboardPlatform);
        }
        Ok(())
    }
    pub(super) fn read(&self, h: Handler, f: Feature) -> (u8, Option<u8>) {
        match h {
            Handler::Firmware => match self.stage {
                Stage::Initial => (0, None),
                Stage::Serial => (2, None),
                _ => (1, Some(self.firmware_index)),
            },
            Handler::Name => match self.stage {
                Stage::Second => (1, Some(self.name.len() as u8)),
                Stage::Third => (2, None),
                _ => (0, None),
            },
            Handler::Battery => (
                match f.id {
                    Id::BATTERY => u8::from(self.stage == Stage::Initial),
                    Id::UNIFIED_BATTERY => u8::from(self.stage != Stage::Initial),
                    _ => 0,
                },
                (f.id == Id::SOLAR).then_some(1),
            ),
            Handler::Platform => {
                if f.id == Id::DUAL_PLATFORM {
                    (1, None)
                } else {
                    match self.stage {
                        Stage::Initial | Stage::Config => (0, None),
                        Stage::Second => (1, Some(self.platform_descriptor)),
                        _ => (2, Some(0xff)),
                    }
                }
            }
            Handler::Power => (1, None),
            Handler::Fn => (0, (f.id == Id::FN_INVERSION_MULTI_HOST).then_some(0xff)),
            Handler::Backlight => (
                if matches!(self.stage, Stage::Initial | Stage::Readback) {
                    2
                } else {
                    0
                },
                None,
            ),
            Handler::Dpi0 | Handler::Dpi1 => {
                if h == Handler::Dpi0 && self.stage == Stage::Initial {
                    (0, None)
                } else {
                    (
                        if matches!(self.stage, Stage::Initial | Stage::Second) {
                            1
                        } else {
                            2
                        },
                        Some(u8::from(h == Handler::Dpi1)),
                    )
                }
            }
            Handler::Wheel => (0, None),
            Handler::Hires | Handler::Thumb => (u8::from(self.stage != Stage::Initial), None),
        }
    }
    fn battery(
        &mut self,
        c: &mut Catalog,
        _f: Feature,
        p: &[u8],
        _now: u64,
        _source: ObservationSource,
    ) -> Result<(), Error> {
        if !c.info.battery.vendor() {
            return Ok(());
        }
        let (percent, charging) = match _f.id {
            Id::BATTERY => {
                require(p.len() >= 3 && p[0] <= 100 && p[1] <= 100 && p[2] <= 7)?;
                let percent = (p[0] != 0).then(|| {
                    if self.battery_flags & 2 != 0 && self.battery_levels >= 10 {
                        p[0]
                    } else {
                        crate::battery::coarse(p[0])
                    }
                });
                (
                    percent,
                    match p[2] {
                        0 | 3 => Some(false),
                        1 | 2 | 4 => Some(true),
                        _ => None,
                    },
                )
            }
            Id::UNIFIED_BATTERY => {
                require(p.len() >= 4)?;
                let percent = if self.battery_flags & 2 != 0 {
                    (p[0] <= 100).then_some(p[0])
                } else {
                    match p[1] {
                        v if v & 8 != 0 => Some(100),
                        v if v & 4 != 0 => Some(75),
                        v if v & 2 != 0 => Some(20),
                        v if v & 1 != 0 => Some(5),
                        _ => None,
                    }
                };
                (
                    percent,
                    match p[2] {
                        0 | 3 => Some(false),
                        1 | 2 => Some(true),
                        _ => None,
                    },
                )
            }
            Id::BATTERY_VOLTAGE => {
                require(p.len() >= 3)?;
                let charging = if p[2] & 0x80 == 0 {
                    Some(false)
                } else {
                    match p[2] & 7 {
                        0 => Some(true),
                        1 | 2 => Some(false),
                        _ => None,
                    }
                };
                let percent = if p[2] & 0x87 == 0x81 {
                    Some(100)
                } else if p[2] & 0x20 != 0 {
                    Some(5)
                } else {
                    None
                };
                (percent, charging)
            }
            Id::ADC_MEASUREMENT => {
                require(p.len() >= 3)?;
                (
                    (p[2] == 7).then_some(100),
                    match p[2] {
                        1 | 7 => Some(false),
                        3 => Some(true),
                        _ => None,
                    },
                )
            }
            Id::SOLAR => {
                require(!p.is_empty())?;
                ((p[0] <= 100).then_some(p[0]), None)
            }
            _ => return Ok(()),
        };
        c.info.battery.vendor_reading(percent, charging);
        Ok(())
    }
    fn backlight(&mut self, c: &mut Catalog, f: Feature, p: &[u8], now: u64) -> Result<(), Error> {
        let v = f.version.0;
        require(
            p.len()
                >= if v >= 3 {
                    12
                } else if v >= 2 {
                    5
                } else {
                    3
                },
        )?;
        require(p[0] <= 1)?;
        let option_mask = if v >= 3 {
            0x1f
        } else if v >= 1 {
            7
        } else {
            3
        };
        let capability_mask = if v >= 3 {
            0x3f
        } else if v >= 1 {
            7
        } else {
            3
        };
        require(v > 3 || (p[1] & !option_mask == 0 && p[2] & !capability_mask == 0))?;
        require((p[1] & 7) & !(p[2] & 7) == 0)?;
        let options = p[1] as u16 | ((p[2] as u16) << 8);
        let mode = (p[1] >> 3) & 3;
        if v >= 3 {
            require(
                !(mode == 0 && options & 0x3800 != 0)
                    && (mode == 0 || options & (0x400 << mode) != 0)
                    && p[5] <= 7,
            )?;
            for i in 0..3 {
                require((1..=1440).contains(&le(&p[6 + 2 * i..])))?;
            }
        }
        let read = ObservationSource::Read;
        number(
            c,
            metadata(Key::BacklightEnabled, f),
            true,
            p[0].into(),
            now,
            read,
        )?;
        for (i, key) in [
            Key::BacklightPowerOn,
            Key::BacklightCrown,
            Key::BacklightPowerSave,
        ]
        .into_iter()
        .enumerate()
        {
            if p[2] & capability_mask & (1 << i) != 0 {
                number(
                    c,
                    metadata(key, f),
                    true,
                    ((p[1] >> i) & 1).into(),
                    now,
                    read,
                )?;
            } else {
                unsupported(c, key);
            }
        }
        if v >= 2 && p[3] & 0x7f != 0 {
            let mut m = metadata(Key::BacklightEffect, f);
            m.choices = (0..7)
                .filter(|i| p[3] & (1 << i) != 0)
                .collect::<Vec<_>>()
                .into_boxed_slice();
            publish(
                c,
                m,
                true,
                (self.backlight_effect < 7)
                    .then_some(Observed::Number(self.backlight_effect.into())),
                now,
                read,
            )?;
            if self.backlight_effect >= 7 {
                stale(c, Key::BacklightEffect);
            }
        } else {
            unsupported(c, Key::BacklightEffect);
        }
        if v < 3 {
            for key in [
                Key::BacklightMode,
                Key::BacklightLevel,
                Key::BacklightDelayHandsOut,
                Key::BacklightDelayHandsIn,
                Key::BacklightDelayPowered,
            ] {
                unsupported(c, key);
            }
            return Ok(());
        }
        let mut m = metadata(Key::BacklightMode, f);
        m.choices = [1, 3]
            .into_iter()
            .filter(|i| options & (0x400 << i) != 0)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        number(c, m, options & 0x2800 != 0, mode.into(), now, read)?;
        if options & 0x2000 != 0 {
            let max = self.backlight_levels.saturating_sub(1).min(7) as u16;
            publish(
                c,
                range(metadata(Key::BacklightLevel, f), 0, max, 1),
                true,
                (mode == 3).then_some(Observed::Number(p[5].into())),
                now,
                read,
            )?;
            if mode != 3
                && let Some(r) = row(c, Key::BacklightLevel)
                && !r
                    .preference
                    .as_ref()
                    .is_some_and(|p| !r.metadata.accepts(p.value))
            {
                r.changed.set(true);
                r.fresh = false;
                r.error = None;
                r.state = if r.preference.is_some() {
                    SettingState::Pending
                } else {
                    SettingState::Unmanaged
                };
            }
        } else {
            unsupported(c, Key::BacklightLevel);
        }
        for (i, key) in [
            Key::BacklightDelayHandsOut,
            Key::BacklightDelayHandsIn,
            Key::BacklightDelayPowered,
        ]
        .into_iter()
        .enumerate()
        {
            number(
                c,
                range(metadata(key, f), 5, 7200, 5),
                true,
                le(&p[6 + 2 * i..]) * 5,
                now,
                read,
            )?;
        }
        Ok(())
    }
    pub(super) fn parse(&mut self, c: &mut Catalog, p: &[u8], now: u64) -> Result<bool, Error> {
        let h = self.current().ok_or(Error::InternalError)?;
        let f = h.feature(c).ok_or(Error::UnsupportedSetting)?;
        let read = ObservationSource::Read;
        match h {
            Handler::Firmware => {
                if self.stage == Stage::Initial {
                    let length = match f.version.0 {
                        0 => 1,
                        1 => 13,
                        2 | 3 => 14,
                        _ => 15,
                    };
                    require(p.len() >= length)?;

                    self.serial_supported = f.version.0 >= 4 && p[14] & 1 != 0;
                    self.firmware_count = p[0].min(8);
                    self.firmware_index = 0;
                    self.firmware_seen = 0;
                    if self.firmware_count > 0 {
                        self.stage = Stage::Second;
                        return Ok(false);
                    }
                    if self.serial_supported {
                        self.stage = Stage::Serial;
                        return Ok(false);
                    }
                } else if self.stage == Stage::Serial {
                    require(p.len() >= 12)?;
                    if p[..12].iter().all(|b| *b == 0) {
                        c.info.observe(true, I::Serial, 0, V::Null);
                        return Ok(true);
                    }
                    require(p[..12].iter().all(|b| {
                        b.is_ascii_digit() || (b.is_ascii_uppercase() && *b != b'I' && *b != b'O')
                    }))?;
                    let s =
                        core::str::from_utf8(&p[..12]).map_err(|_| Error::HidppInvalidResponse)?;
                    c.info.observe(true, I::Serial, 0, V::Text(s.into()));
                } else {
                    let length = if f.version.0 == 0 { 8 } else { 16 };
                    require(p.len() >= length)?;
                    let entity = p[0] & 0x0f;
                    // Keep the first entity of each supported type.
                    if entity <= 2 && self.firmware_seen & (1 << entity) == 0 {
                        self.firmware_seen |= 1 << entity;
                        match entity {
                            instance @ (0 | 1) => {
                                let prefix = crate::info::text(&p[1..4]);
                                let version = format!(
                                    "{} {:02X}.{:02X}.{:04X}",
                                    prefix,
                                    p[4],
                                    p[5],
                                    be(&p[6..8])
                                );
                                c.info.observe(
                                    true,
                                    I::Firmware,
                                    instance,
                                    V::Text(version.trim().into()),
                                );
                            }
                            2 => c
                                .info
                                .observe(true, I::Hardware, 0, V::Text(format!("{}", p[1]))),
                            _ => {}
                        }
                    }
                    self.firmware_index += 1;
                    if self.firmware_index < self.firmware_count {
                        return Ok(false);
                    }
                    if self.serial_supported {
                        self.stage = Stage::Serial;
                        return Ok(false);
                    }
                }
            }
            Handler::Name => {
                if self.stage == Stage::Third {
                    require(!p.is_empty())?;
                    c.info.observe(
                        true,
                        I::Kind,
                        0,
                        V::Text(
                            match p[0] {
                                0 | 2 => "keyboard",
                                3..=5 => "mouse",
                                _ => "other",
                            }
                            .into(),
                        ),
                    );
                } else if self.stage == Stage::Initial {
                    require(!p.is_empty() && p[0] != 0)?;
                    self.name_length = p[0].min(64);
                    self.name.clear();
                    self.stage = Stage::Second;
                    return Ok(false);
                } else {
                    require(!p.is_empty())?;
                    let remaining = self.name_length as usize - self.name.len();
                    self.name.extend_from_slice(&p[..p.len().min(remaining)]);
                    if self.name.len() < self.name_length as usize {
                        return Ok(false);
                    }
                    c.info
                        .observe(true, I::Name, 0, V::Text(crate::info::text(&self.name)));
                    self.stage = Stage::Third;
                    return Ok(false);
                }
            }
            Handler::Battery => {
                if self.stage == Stage::Initial && matches!(f.id, Id::BATTERY | Id::UNIFIED_BATTERY)
                {
                    require(p.len() >= 2)?;
                    self.battery_levels = p[0];
                    self.battery_flags = p[1];
                    self.stage = Stage::Second;
                    return Ok(false);
                }
                if f.id != Id::SOLAR {
                    self.battery(c, f, p, now, read)?;
                }
            }
            Handler::Fn => {
                let multi = f.id == Id::FN_INVERSION_MULTI_HOST;
                require(
                    p.len()
                        >= if multi {
                            4
                        } else if f.id == Id::FN_INVERSION_LEGACY {
                            1
                        } else {
                            2
                        },
                )?;
                let i = usize::from(multi);
                require(p[i] <= 1 && (f.id == Id::FN_INVERSION_LEGACY || p[i + 1] <= 1))?;
                let mut m = choices(metadata(Key::FnRowDefault, f), &[0, 1]);
                if multi {
                    m.scope = SettingScope::CurrentHost;
                    self.fn_host = p[0];
                }
                number(c, m, true, p[i].into(), now, read)?;
            }
            Handler::Platform => {
                if f.id == Id::DUAL_PLATFORM {
                    require(!p.is_empty() && p[0] <= 1)?;
                    self.platforms = [1, 255, 255, 255, 1, 0, 0, 255, 255];
                    self.platform_all_versions = 0x71;
                    self.platform_writable = true;
                    self.publish_platform(c, f, p[0], now, read)?;
                } else if matches!(self.stage, Stage::Initial | Stage::Config) {
                    require(
                        p.len() >= 7
                            && p[2] != 0
                            && p[3] != 0
                            && p[4] != 0
                            && p[5] < p[4]
                            && p[6] < p[2],
                    )?;
                    self.platforms = [255; 9];
                    self.platform_all_versions = 0;
                    self.platform_count = p[2];
                    self.platform_descriptors = p[3];
                    self.platform_descriptor = 0;
                    self.platform_host = p[5];
                    self.platform_writable = p[0] & 2 != 0;
                    self.stage = Stage::Second;
                    return Ok(false);
                } else if self.stage == Stage::Second {
                    require(
                        p.len() >= 8
                            && p[0] < self.platform_count
                            && p[1] == self.platform_descriptor,
                    )?;
                    // A named OS choice is safe only when it covers all versions.
                    // Conflicting platform indices for one OS cannot be guessed.
                    {
                        let mask = be(&p[2..4]);
                        for (i, flag) in [
                            0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000, 1,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            if mask & flag != 0 {
                                let old = self.platforms[i];
                                self.platforms[i] =
                                    if old == 255 || old == p[0] { p[0] } else { 254 };
                                if p[4..8] == [0; 4] {
                                    self.platform_all_versions |= 1 << i;
                                }
                            }
                        }
                    }
                    self.platform_descriptor += 1;
                    if self.platform_descriptor == self.platform_descriptors {
                        self.stage = Stage::Third;
                    }
                    return Ok(false);
                } else {
                    require(
                        p.len() >= 6
                            && (p[0] == 0xff || p[0] == self.platform_host)
                            && p[1] == 1
                            && p[2] < self.platform_count,
                    )?;
                    self.publish_platform(c, f, p[2], now, read)?;
                }
            }
            Handler::Power => {
                require(!p.is_empty())?;
                let keyboard = c.info.snapshot().iter().any(|field| field.key == I::Kind
                    && field.available && matches!(&field.value, V::Text(kind) if kind == "keyboard" || kind == "keyboard_mouse"));
                if !keyboard {
                    return Err(Error::UnsupportedSetting);
                }
                number(
                    c,
                    range(metadata(Key::PowerAutoOff, f), 0, 15300, 60),
                    true,
                    u16::from(p[0]) * 60,
                    now,
                    read,
                )?;
            }
            Handler::Backlight => {
                if matches!(self.stage, Stage::Initial | Stage::Readback) {
                    require(p.len() >= if f.version.0 >= 2 { 4 } else { 3 })?;
                    require(p[0] != 0 && p[1] < p[0])?;
                    self.backlight_levels = p[0];
                    self.backlight_effect = if f.version.0 >= 2 { p[3] } else { 0 };
                    number(
                        c,
                        range(
                            metadata(Key::BacklightCurrentLevel, f),
                            0,
                            (p[0] - 1).into(),
                            1,
                        ),
                        false,
                        p[1].into(),
                        now,
                        read,
                    )?;
                    let max = if f.version.0 >= 3 { 5 } else { 4 };
                    if p[2] <= max {
                        let mut m = metadata(Key::BacklightStatus, f);
                        m.choices = (0..=max as u16).collect::<Vec<_>>().into_boxed_slice();
                        number(c, m, false, p[2].into(), now, read)?;
                    } else {
                        stale(c, Key::BacklightStatus);
                    }
                    self.stage = if self.stage == Stage::Readback {
                        Stage::BacklightVerify
                    } else {
                        Stage::Config
                    };
                    return Ok(false);
                }
                self.backlight(c, f, p, now)?;
            }
            Handler::Dpi0 | Handler::Dpi1 => {
                if h == Handler::Dpi0 && self.stage == Stage::Initial {
                    require(!p.is_empty() && p[0] != 0)?;
                    self.sensor_count = p[0].min(2);
                    if self.sensor_count < 2 {
                        unsupported(c, Key::PointerDpi1);
                    }
                    self.stage = Stage::Second;
                    return Ok(false);
                }
                require(p.len() >= 3 && p[0] == u8::from(h == Handler::Dpi1))?;
                let key = if h == Handler::Dpi0 {
                    Key::PointerDpi0
                } else {
                    Key::PointerDpi1
                };
                if matches!(self.stage, Stage::Initial | Stage::Second) {
                    require(p.len() >= 15)?;
                    let (first, second) = (be(&p[1..]), be(&p[3..]));
                    require(first != 0 && first < 0xe000)?;
                    let mut m = metadata(key, f);
                    if second >= 0xe000 {
                        let (max, step) = (be(&p[5..]), second & 0x1fff);
                        require(
                            step != 0
                                && max >= first
                                && max < 0xe000
                                && be(&p[7..]) == 0
                                && (max - first).is_multiple_of(step),
                        )?;
                        m = range(m, first, max, step);
                    } else {
                        let mut values = Vec::new();
                        let mut ended = false;
                        for i in 0..7 {
                            let value = be(&p[1 + 2 * i..]);
                            if value == 0 {
                                ended = true;
                                break;
                            }
                            require(
                                i < 6
                                    && value < 0xe000
                                    && values.last().is_none_or(|last| value > *last),
                            )?;
                            values.push(value);
                        }
                        require(ended)?;
                        m.choices = values.into_boxed_slice();
                    }
                    publish(c, m, true, None, now, read)?;
                    self.stage = Stage::Config;
                    return Ok(false);
                }
                require(p.len() >= 5 && be(&p[1..]) != 0 && be(&p[1..]) < 0xe000)?;
                observe_key(c, key, be(&p[1..]), now, read);
            }
            Handler::Wheel => {
                require(p.len() >= 3 && (1..=2).contains(&p[0]) && p[1] != 0 && p[2] != 0)?;
                number(
                    c,
                    choices(metadata(Key::WheelMode, f), &[1, 2]),
                    true,
                    p[0].into(),
                    now,
                    read,
                )?;
                number(
                    c,
                    range(metadata(Key::WheelThreshold, f), 1, 255, 1),
                    true,
                    p[1].into(),
                    now,
                    read,
                )?;
            }
            Handler::Hires => {
                if self.stage == Stage::Initial {
                    let length = if f.version.0 == 0 { 2 } else { 4 };
                    require(p.len() >= length && p[0] != 0)?;
                    bytes(c, Key::WheelInfo, f, &p[..length], now)?;
                    if p[1] & 8 == 0 {
                        unsupported(c, Key::WheelInvert);
                        return Ok(true);
                    }
                    publish(c, metadata(Key::WheelInvert, f), true, None, now, read)?;
                    self.stage = Stage::Config;
                    return Ok(false);
                }
                require(
                    !p.is_empty()
                        && (f.version.0 > 1
                            || p[0] & if f.version.0 == 0 { 0xf8 } else { 0xf0 } == 0),
                )?;
                observe_key(c, Key::WheelInvert, ((p[0] >> 2) & 1).into(), now, read);
            }
            Handler::Thumb => {
                if self.stage == Stage::Initial {
                    require(p.len() >= 8 && be(p) != 0 && be(&p[2..]) != 0 && p[4] <= 1)?;
                    publish(c, metadata(Key::ThumbwheelInvert, f), true, None, now, read)?;
                    self.stage = Stage::Config;
                    return Ok(false);
                }
                require(p.len() >= 2 && p[0] <= 1 && p[1] & 0xf8 == 0)?;
                observe_key(c, Key::ThumbwheelInvert, (p[1] & 1).into(), now, read);
            }
        }
        Ok(true)
    }

    pub(super) fn validate_requested(&self, c: &Catalog) -> Result<(), Error> {
        for r in c.records.iter().filter(|r| self.matches(r, true)) {
            if !r.available
                || !r.writable
                || !r
                    .preference
                    .as_ref()
                    .is_some_and(|p| r.metadata.accepts(p.value))
            {
                return Err(Error::UnsupportedSetting);
            }
        }
        Ok(())
    }
    pub(super) fn setter(&self, c: &Catalog) -> Result<(u8, [u8; 16], usize), Error> {
        self.validate_requested(c)?;
        let h = self.current().ok_or(Error::InternalError)?;
        let f = h.feature(c).ok_or(Error::UnsupportedSetting)?;
        let requested = || c.records.iter().filter(|r| self.matches(r, true));
        let desired = |key| {
            c.records
                .iter()
                .find(|r| r.metadata.key == key)
                .and_then(|r| r.preference.as_ref())
                .map(|p| p.value)
                .ok_or(Error::UnsupportedSetting)
        };
        let mut out = self.raw;
        let (function, length) = match h {
            Handler::Fn => {
                let value = desired(Key::FnRowDefault)? as u8;
                if f.id == Id::FN_INVERSION_MULTI_HOST {
                    // The current-host selector cannot target another paired host.
                    // Devices rejecting it fail the write; no alternate byte order is tried.
                    out[0] = 0xff;
                    out[1] = value;
                    (1, 2)
                } else {
                    out[0] = value;
                    (1, 1)
                }
            }
            Handler::Platform => {
                let selected = usize::from(desired(Key::KeyboardPlatform)?);
                let platform = *self
                    .platforms
                    .get(selected)
                    .filter(|&&p| p < 254)
                    .ok_or(Error::UnsupportedSetting)?;
                if f.id == Id::MULTI_PLATFORM {
                    out[0] = 0xff;
                    out[1] = platform;
                    (3, 2)
                } else {
                    out[0] = platform;
                    (2, 1)
                }
            }
            Handler::Power => {
                out[0] = (desired(Key::PowerAutoOff)? / 60) as u8;
                (2, 1)
            }
            Handler::Backlight => {
                out[2] = 0xff;
                let length = if f.version.0 >= 3 {
                    out[3] = self.raw[5];
                    out[4..10].copy_from_slice(&self.raw[6..12]);
                    10
                } else if f.version.0 >= 2 {
                    3
                } else {
                    2
                };
                let mut level = false;
                for r in requested() {
                    let value = r.preference.as_ref().unwrap().value;
                    match r.metadata.key {
                        Key::BacklightEnabled => out[0] = (out[0] & 0xfe) | value as u8,
                        Key::BacklightPowerOn | Key::BacklightCrown | Key::BacklightPowerSave => {
                            let i = r.metadata.key as u8 - Key::BacklightPowerOn as u8;
                            out[1] = (out[1] & !(1 << i)) | ((value as u8) << i);
                        }
                        Key::BacklightEffect => out[2] = value as u8,
                        Key::BacklightMode => out[1] = (out[1] & 0xe7) | ((value as u8) << 3),
                        Key::BacklightLevel => {
                            out[3] = value as u8;
                            level = true;
                        }
                        Key::BacklightDelayHandsOut
                        | Key::BacklightDelayHandsIn
                        | Key::BacklightDelayPowered => {
                            let i = 4 + 2
                                * (r.metadata.key as usize - Key::BacklightDelayHandsOut as usize);
                            out[i..i + 2].copy_from_slice(&(value / 5).to_le_bytes());
                        }
                        _ => return Err(Error::UnsupportedSetting),
                    }
                }
                let mode = (out[1] >> 3) & 3;
                if f.version.0 >= 3 && mode == 2 {
                    return Err(Error::BacklightModeSelectionRequired);
                }
                if level && mode != 3 {
                    return Err(Error::BacklightPermanentManualRequired);
                }
                (1, length)
            }
            Handler::Dpi0 | Handler::Dpi1 => {
                let sensor = u8::from(h == Handler::Dpi1);
                out[0] = sensor;
                out[1..3].copy_from_slice(
                    &desired(if sensor == 0 {
                        Key::PointerDpi0
                    } else {
                        Key::PointerDpi1
                    })?
                    .to_be_bytes(),
                );
                (3, 3)
            }
            Handler::Wheel => {
                out[..3].fill(0);
                for r in requested() {
                    out[usize::from(r.metadata.key != Key::WheelMode)] =
                        r.preference.as_ref().unwrap().value as u8;
                }
                (1, 3)
            }
            Handler::Hires => {
                if self.raw[0] & 3 != 0 {
                    return Err(Error::NativeStandardResolutionRequired);
                }
                out[0] = (self.raw[0] & 0xfb) | ((desired(Key::WheelInvert)? as u8) << 2);
                (2, 1)
            }
            Handler::Thumb => {
                if self.raw[0] != 0 {
                    return Err(Error::NativeRoutingRequired);
                }
                out[0] = 0;
                out[1] = desired(Key::ThumbwheelInvert)? as u8;
                (2, 2)
            }
            _ => return Err(Error::ReadOnly),
        };
        Ok((function, out, length))
    }

    /// Only advertised public, implemented features contribute observations.
    pub fn receive(&mut self, c: &mut Catalog, report: u8, p: &[u8], now: u64) -> bool {
        if !c.connected
            || self.cancelled.is_some()
            || !matches!((report, p.len()), (0x10, 6) | (0x11, 19))
            || p[0] != 0xff
            || p[2] & 15 != 0
        {
            return false;
        }
        let Some(f) = c
            .features
            .get(p[1] as usize)
            .filter(|f| f.supported())
            .map(|f| f.wire(p[1] as usize))
        else {
            return false;
        };
        let event = p[2] >> 4;
        let p = &p[3..];
        let source = ObservationSource::Event;
        match (f.id, event) {
            (Id::BATTERY | Id::UNIFIED_BATTERY | Id::BATTERY_VOLTAGE | Id::ADC_MEASUREMENT, 0)
            | (Id::SOLAR, 0..=2) => {
                if Handler::Battery
                    .feature(c)
                    .is_none_or(|selected| selected.id != f.id)
                {
                    return false;
                }
                let _ = self.battery(c, f, p, now, source);
                self.event_epoch = self.event_epoch.wrapping_add(1);
                return false; // Information changes use their own field-only event.
            }
            (Id::DUAL_PLATFORM, 0) if !p.is_empty() && p[0] <= 1 => {
                if Handler::Platform
                    .feature(c)
                    .is_none_or(|selected| selected.id != f.id)
                {
                    return false;
                }
                if self.platform_descriptor == self.platform_descriptors
                    && row(c, Key::KeyboardPlatform).is_some_and(|r| r.available)
                {
                    let _ = self.publish_platform(c, f, p[0], now, source);
                }
            }
            (Id::MULTI_PLATFORM, 0)
                if p.len() >= 3
                    && (p[0] == 0xff || p[0] == self.platform_host)
                    && p[1] < self.platform_count =>
            {
                if self.platform_descriptor == self.platform_descriptors
                    && row(c, Key::KeyboardPlatform).is_some_and(|r| r.available)
                {
                    let _ = self.publish_platform(c, f, p[1], now, source);
                }
            }
            (Id::FN_INVERSION_MULTI_HOST, 0)
                if p.len() >= 4
                    && p[1] <= 1
                    && p[2] <= 1
                    && (self.fn_host == 0xff || p[0] == 0xff || p[0] == self.fn_host) =>
            {
                observe_key(c, Key::FnRowDefault, p[1].into(), now, source);
            }
            (Id::HIRES_WHEEL, 1) if !p.is_empty() && p[0] <= 1 => {
                observe_key(
                    c,
                    Key::WheelMode,
                    if p[0] == 0 { 1 } else { 2 },
                    now,
                    source,
                );
            }
            (Id::BACKLIGHT, 0) if p.len() >= 3 && p[0] != 0 && p[1] < p[0] => {
                self.refresh_backlight = true;
                observe_key(c, Key::BacklightCurrentLevel, p[1].into(), now, source);
                if p[2] <= if f.version.0 >= 3 { 5 } else { 4 } {
                    observe_key(c, Key::BacklightStatus, p[2].into(), now, source);
                } else {
                    stale(c, Key::BacklightStatus);
                }
                if f.version.0 >= 3 && matches!(p[2], 4 | 5) {
                    observe_key(c, Key::BacklightMode, (p[2] - 2).into(), now, source);
                }
                if f.version.0 >= 2 && p.len() >= 4 {
                    self.backlight_effect = p[3];
                    if p[3] < 7 {
                        observe_key(c, Key::BacklightEffect, p[3].into(), now, source);
                    } else {
                        stale(c, Key::BacklightEffect);
                    }
                }
                stale(c, Key::BacklightLevel);
            }
            _ => return false,
        }
        self.event_epoch = self.event_epoch.wrapping_add(1);
        true
    }
}
