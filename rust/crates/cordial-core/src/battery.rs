//! RAM-only battery normalization. A transport pins the provider; failed reads
//! never select another provider. No serialization or storage path exists here.
use crate::model::{
    identifiers::Transport,
    info::{InfoField, InfoKey as K},
    settings::SettingValue as V,
};
use alloc::vec::Vec;

#[derive(Clone, Copy, Default)]
struct Reading {
    instance: u8,
    rank: u8,
    percent: Option<u8>,
    charging: Option<bool>,
}
#[derive(Default)]
pub struct Battery {
    classic: bool,
    hidpp: bool,
    enabled: bool,
    connected: bool,
    readings: Vec<Reading>,
    absent: u8,
    main: u8,
    dirty: bool,
}
impl Battery {
    pub fn configure(&mut self, transport: Transport, hidpp: bool) {
        let classic = transport == Transport::Classic;
        if self.classic != classic || self.hidpp != hidpp {
            self.clear();
        }
        self.classic = classic;
        self.hidpp = hidpp;
    }
    pub fn hidpp_reports(&mut self, supported: bool) {
        if self.hidpp != supported {
            self.clear();
        }
        self.hidpp = supported;
    }
    pub fn vendor(&self) -> bool {
        self.hidpp && self.enabled
    }
    pub fn standard_hid(&self) -> bool {
        self.classic && !self.vendor()
    }
    pub fn connection(&mut self, connected: bool, enabled: bool) {
        if self.connected != connected || ((self.classic || self.hidpp) && self.enabled != enabled)
        {
            self.clear();
        }
        self.connected = connected;
        self.enabled = enabled;
    }
    fn clear(&mut self) {
        self.readings.clear();
        self.absent = 0;
        self.main = 0;
        self.dirty = true;
    }
    pub fn take_dirty(&mut self) -> bool {
        core::mem::take(&mut self.dirty)
    }
    pub fn has_observations(&self) -> bool {
        !self.readings.is_empty()
    }
    pub fn invalidate_vendor(&mut self, keys: &[K]) {
        if self.vendor()
            && keys
                .iter()
                .any(|k| matches!(k, K::BatteryPercent | K::BatteryCharging))
        {
            self.clear();
        }
    }
    fn update(&mut self, instance: u8, rank: u8, percent: Option<u8>, charging: Option<bool>) {
        if !self.connected || instance >= 5 {
            return;
        }
        let r = Reading {
            instance,
            rank,
            percent: percent.filter(|p| *p <= 100),
            charging,
        };
        if let Some(old) = self
            .readings
            .iter_mut()
            .find(|r| r.instance == instance && r.rank == rank)
        {
            *old = r;
        } else {
            self.readings.push(r);
        }
        self.dirty = true;
    }
    pub fn observe(&mut self, vendor: bool, instance: u8, key: K, value: V) {
        if vendor != self.vendor() {
            return;
        }
        let old = self
            .readings
            .iter()
            .find(|r| r.instance == instance && r.rank == 0)
            .copied()
            .unwrap_or_default();
        let percent = if key == K::BatteryPercent {
            match value {
                V::Integer(n) if (0..=100).contains(&n) => Some(n as u8),
                _ => None,
            }
        } else {
            old.percent
        };
        let charging = if key == K::BatteryCharging {
            match value {
                V::Bool(v) => Some(v),
                _ => None,
            }
        } else {
            old.charging
        };
        self.update(instance, 0, percent, charging);
    }
    pub fn vendor_reading(&mut self, percent: Option<u8>, charging: Option<bool>) {
        if self.vendor() {
            self.update(0, 0, percent, charging);
        }
    }
    pub fn hid_reading(&mut self, instance: u8, key: K, value: V) {
        if self.standard_hid() {
            self.observe(false, instance, key, value);
        }
    }
    fn selected(&self) -> Option<Reading> {
        let mut selected: Option<Reading> = None;
        for instance in 0..5 {
            if self.absent & (1 << instance) != 0 {
                continue;
            }
            let percent = self
                .readings
                .iter()
                .filter(|r| r.instance == instance && r.percent.is_some())
                .min_by_key(|r| r.rank)
                .and_then(|r| r.percent);
            let charging = self
                .readings
                .iter()
                .filter(|r| r.instance == instance && r.charging.is_some())
                .min_by_key(|r| r.rank)
                .and_then(|r| r.charging);
            if percent.is_none() && charging.is_none() {
                continue;
            }
            let candidate = Reading {
                instance,
                percent,
                charging,
                rank: 0,
            };
            let priority = |r: Reading| {
                (
                    self.main & (1 << r.instance) == 0,
                    r.percent.is_none(),
                    r.percent.unwrap_or(101),
                    r.instance,
                )
            };
            if selected.is_none_or(|old| priority(candidate) < priority(old)) {
                selected = Some(candidate);
            }
        }
        selected
    }
    pub fn field(&self, key: K) -> InfoField {
        let mut f = InfoField::unknown(key, 0);
        if !self.connected {
            return f;
        }
        if let Some(r) = self.selected() {
            f.value = match key {
                K::BatteryPercent => r.percent.map_or(V::Null, |v| V::Integer(v.into())),
                K::BatteryCharging => r.charging.map_or(V::Null, V::Bool),
                _ => V::Null,
            };
            f.available = f.value != V::Null;
            f.fresh = f.available;
        }
        f
    }
    pub fn gatt(&mut self, uuid: u16, instance: u8, bytes: &[u8]) {
        if self.classic || self.vendor() || !self.connected || instance >= 4 {
            return;
        }
        match uuid {
            0x180f => {
                if let [count @ 0..=4] = bytes {
                    self.readings.retain(|r| r.instance < *count);
                    self.dirty = true;
                }
            }
            0x2904 => {
                // SIG namespace description 0x0106 denotes the main battery.
                if bytes.len() == 7 && bytes[4] == 1 && bytes[5..7] == [6, 1] {
                    self.main |= 1 << instance;
                    self.dirty = true;
                }
            }
            0x2a19 => self.update(
                instance,
                0,
                match bytes {
                    [p @ 0..=100] => Some(*p),
                    _ => None,
                },
                None,
            ),
            0x2bed => {
                let valid = bytes.first().is_some_and(|f| {
                    bytes.len()
                        == 3 + usize::from(f & 1 != 0) * 2
                            + usize::from(f & 2 != 0)
                            + usize::from(f & 4 != 0)
                });
                if !valid {
                    self.update(instance, 1, None, None);
                    self.update(instance, 3, None, None);
                    return;
                }
                let state = u16::from_le_bytes([bytes[1], bytes[2]]);
                self.absent &= !(1 << instance);
                if state & 1 == 0 {
                    self.absent |= 1 << instance;
                }
                if bytes[0] & 1 != 0 && bytes[3..5] == [6, 1] {
                    self.main |= 1 << instance;
                }
                let offset = 3 + usize::from(bytes[0] & 1 != 0) * 2;
                let percent = if bytes[0] & 2 != 0 {
                    bytes.get(offset).copied().filter(|p| *p <= 100)
                } else {
                    None
                };
                let coarse = match (state >> 7) & 3 {
                    1 => Some(75),
                    2 => Some(20),
                    3 => Some(5),
                    _ => None,
                };
                let charging = match (state >> 5) & 3 {
                    1 => Some(true),
                    2 | 3 => Some(false),
                    _ => None,
                };
                self.update(instance, 1, percent, charging);
                self.update(instance, 3, coarse, None);
            }
            0x2be9 => self.update(
                instance,
                4,
                match bytes {
                    [v] if v & 1 != 0 => Some(5),
                    _ => None,
                },
                None,
            ),
            0x2bf0 => {
                let mut values = [None; 6];
                let mut offset = 1;
                let valid = if let Some(flags) = bytes.first() {
                    for (bit, v) in values.iter_mut().enumerate() {
                        if flags & (1 << bit) != 0 {
                            if let Some(pair) = bytes.get(offset..offset + 2) {
                                *v = sfloat(u16::from_le_bytes([pair[0], pair[1]]));
                            }
                            offset += 2;
                        }
                    }
                    bytes.len() == offset
                } else {
                    false
                };
                let percent = if valid {
                    values[2]
                        .zip(values[3])
                        .filter(|(e, c)| *e >= 0.0 && *c > 0.0)
                        .map(|(e, c)| ((100.0 * e / c).clamp(0.0, 100.0) + 0.5) as u8)
                } else {
                    None
                };
                let charging = if valid {
                    values[4].map(|rate| rate > 0.0)
                } else {
                    None
                };
                self.update(instance, 2, percent, charging);
            }
            _ => {}
        }
    }
}
/// IEEE-11073 16-bit decimal float, excluding reserved/special mantissas.
fn sfloat(raw: u16) -> Option<f32> {
    let m = raw & 0xfff;
    if matches!(m, 0x7fe..=0x802) {
        return None;
    }
    let mantissa = ((m << 4) as i16) >> 4;
    let exponent = (raw as i16) >> 12;
    let mut value = mantissa as f32;
    for _ in 0..exponent.unsigned_abs() {
        if exponent < 0 {
            value /= 10.0;
        } else {
            value *= 10.0;
        }
    }
    Some(value)
}
pub fn coarse(percent: u8) -> u8 {
    match percent {
        0..=10 => 5,
        11..=30 => 20,
        31..=80 => 75,
        _ => 100,
    }
}
