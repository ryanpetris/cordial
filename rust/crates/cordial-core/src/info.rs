//! Volatile observations only. This module deliberately has no storage API or
//! serialization implementation; battery information must NEVER reach flash.
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use cordial_protocol::{
    info::{InfoField, InfoKey as K, MAX_TEXT},
    settings::SettingValue as V,
};

#[derive(Default)]
pub struct Information {
    standard: Vec<InfoField>,
    vendor: Vec<InfoField>,
    published: Vec<InfoField>,
    dirty: bool,
    pub battery: crate::battery::Battery,
    connected: bool,
    enabled: bool,
    kind_hint: Option<&'static str>,
}
impl Information {
    pub fn connection(&mut self, connected: bool, enabled: bool) {
        self.dirty |= self.connected != connected || self.enabled != enabled;
        self.battery.connection(connected, enabled);
        if self.connected != connected {
            for field in self.standard.iter_mut().chain(self.vendor.iter_mut()) {
                field.fresh = false;
            }
        }
        if self.enabled != enabled {
            // An old HID++ observation must not regain precedence on re-enable.
            for field in &mut self.vendor {
                field.fresh = false;
            }
        }
        self.connected = connected;
        self.enabled = enabled;
    }
    pub fn kind_hint(&mut self, kind: cordial_protocol::messages::DeviceKind) {
        let hint = match kind {
            cordial_protocol::messages::DeviceKind::Keyboard => Some("keyboard"),
            cordial_protocol::messages::DeviceKind::Mouse => Some("mouse"),
            cordial_protocol::messages::DeviceKind::KeyboardMouse => Some("keyboard_mouse"),
            _ => None,
        };
        self.dirty |= self.kind_hint != hint;
        self.kind_hint = hint;
    }
    pub fn observe(&mut self, vendor: bool, key: K, instance: u8, value: V) {
        if matches!(key, K::BatteryPercent | K::BatteryCharging) {
            self.battery.observe(vendor, instance, key, value);
            self.dirty = true;
            return;
        }
        let value = if matches!(&value, V::Text(s) if s.is_empty()) {
            V::Null
        } else {
            value
        };
        // Missing/blank names cannot replace a name already supplied by this
        // device. Only a valid new name updates the observation.
        if key == K::Name && value == V::Null {
            return;
        }
        let field = InfoField {
            key,
            instance,
            available: value != V::Null,
            fresh: value != V::Null,
            value,
        };
        if !field.valid() {
            return;
        }
        let fields = if vendor {
            &mut self.vendor
        } else {
            &mut self.standard
        };
        if let Some(old) = fields
            .iter_mut()
            .find(|f| f.key == key && f.instance == instance)
        {
            self.dirty |= *old != field;
            *old = field;
        } else {
            self.dirty = true;
            fields.push(field);
        }
    }
    pub fn invalidate_vendor(&mut self, keys: &[K]) {
        self.battery.invalidate_vendor(keys);
        self.dirty = true;
        for f in &mut self.vendor {
            if keys.contains(&f.key) {
                self.dirty |= f.fresh;
                f.fresh = false;
            }
        }
    }
    pub fn snapshot(&self) -> Vec<InfoField> {
        let mut fields = Vec::new();
        for key in K::ALL {
            if matches!(key, K::BatteryPercent | K::BatteryCharging) {
                fields.push(self.battery.field(key));
                continue;
            }
            let count = self
                .standard
                .iter()
                .filter(|f| f.key == key)
                .map(|f| f.instance + 1)
                .chain(
                    self.vendor
                        .iter()
                        .chain(self.published.iter())
                        .filter(|f| f.key == key)
                        .map(|f| f.instance + 1),
                )
                .max()
                .unwrap_or(1);
            for instance in 0..count {
                let standard = self
                    .standard
                    .iter()
                    .find(|f| f.key == key && f.instance == instance && f.available);
                let vendor = self
                    .vendor
                    .iter()
                    .find(|f| f.key == key && f.instance == instance && f.available)
                    .filter(|_| self.enabled);
                let selected = vendor
                    .filter(|f| f.fresh)
                    .or(standard.filter(|f| f.fresh))
                    .or(vendor)
                    .or(standard);
                let mut field = selected
                    .cloned()
                    .unwrap_or_else(|| InfoField::unknown(key, instance));
                field.instance = instance;
                field.fresh &= self.connected;
                if key == K::Kind
                    && (self.connected || !field.available || vendor.is_none())
                    && !vendor.is_some_and(|f| f.fresh)
                    && let Some(kind) = self.kind_hint
                {
                    field.value = V::Text(kind.into());
                    field.available = true;
                    field.fresh = self.connected;
                }
                fields.push(field);
            }
        }
        fields
    }
    pub fn changes(&mut self) -> Vec<InfoField> {
        if !self.dirty && !self.battery.take_dirty() {
            return Vec::new();
        }
        self.dirty = false;
        self.battery.take_dirty();
        if self.published.is_empty()
            && self.standard.is_empty()
            && self.vendor.is_empty()
            && self.kind_hint.is_none()
            && !self.battery.has_observations()
        {
            return Vec::new();
        }
        let snapshot = self.snapshot();
        let changes = snapshot
            .iter()
            .filter(|f| !self.published.contains(f))
            .cloned()
            .collect();
        self.published = snapshot;
        changes
    }
}
pub fn text(bytes: &[u8]) -> String {
    let mut s = String::new();
    for c in String::from_utf8_lossy(bytes).chars() {
        if c == '\0' {
            break;
        }
        if matches!(c, '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
        {
            continue;
        }
        let c = if c.is_control() { ' ' } else { c };
        if s.len() + c.len_utf8() > MAX_TEXT {
            break;
        }
        s.push(c);
    }
    s.trim().to_string()
}
/// Decode optional GATT observations. Battery observations stay in RAM.
pub fn standard(info: &mut Information, uuid: u16, instance: u8, bytes: &[u8]) {
    let text_key = match uuid {
        0x2a00 => Some(K::Name),
        0x2a29 => Some(K::Manufacturer),
        0x2a24 => Some(K::Model),
        0x2a25 => Some(K::Serial),
        0x2a26 => Some(K::Firmware),
        0x2a27 => Some(K::Hardware),
        0x2a28 => Some(K::Software),
        _ => None,
    };
    if let Some(key) = text_key {
        let s = text(bytes);
        info.observe(
            false,
            key,
            0,
            if s.is_empty() { V::Null } else { V::Text(s) },
        );
    } else {
        match uuid {
            0x180f | 0x2a19 | 0x2bed | 0x2be9 | 0x2bf0 | 0x2904 => {
                info.battery.gatt(uuid, instance, bytes);
            }
            0x2a01 => {
                let kind = match bytes {
                    [0xc1, 3] => V::Text("keyboard".into()),
                    [0xc2, 3] => V::Text("mouse".into()),
                    _ => V::Null,
                };
                info.observe(false, K::Kind, 0, kind);
            }
            0x2a50 => {
                info.observe(
                    false,
                    K::VendorIdNamespace,
                    0,
                    if bytes.len() == 7 && matches!(bytes[0], 1 | 2) {
                        V::Text(if bytes[0] == 1 { "bluetooth" } else { "usb" }.into())
                    } else {
                        V::Null
                    },
                );
                for (offset, key) in [(1, K::VendorId), (3, K::ProductId), (5, K::ProductVersion)] {
                    let value = if bytes.len() == 7 && matches!(bytes[0], 1 | 2) {
                        V::Integer(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]).into())
                    } else {
                        V::Null
                    };
                    info.observe(false, key, 0, value);
                }
            }
            _ => {}
        }
    }
}

/// Transport failures retain the last value, marked stale. A successful read
/// containing an explicit unknown still clears the field through `standard`.
pub fn standard_failed(info: &mut Information, uuid: u16, instance: u8) {
    if matches!(uuid, 0x2a19 | 0x2bed | 0x2be9 | 0x2bf0 | 0x2904) {
        info.battery.gatt(uuid, instance, &[]);
        return;
    }
    let keys: &[K] = match uuid {
        0x2a00 => &[K::Name],
        0x2a01 => &[K::Kind],
        0x2a29 => &[K::Manufacturer],
        0x2a24 => &[K::Model],
        0x2a25 => &[K::Serial],
        0x2a26 => &[K::Firmware],
        0x2a27 => &[K::Hardware],
        0x2a28 => &[K::Software],
        0x2a50 => &[
            K::VendorIdNamespace,
            K::VendorId,
            K::ProductId,
            K::ProductVersion,
        ],
        _ => &[],
    };
    for f in &mut info.standard {
        if keys.contains(&f.key) && f.instance == instance {
            info.dirty |= f.fresh;
            f.fresh = false;
        }
    }
}
