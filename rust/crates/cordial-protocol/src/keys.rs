//! Information and setting keys from `proto/keys.toml`. A key containing `{n}` is a template for
//! a part of the device that repeats; [`lookup`] matches a concrete key such as
//! `pointer.sensor.1.dpi` to its template and index.
use alloc::string::String;

/// The value type of a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Bool,
    Integer,
    Enum,
    Text,
    Color,
}

/// One catalog entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Key {
    /// The key, or its template with `{n}` in place of an index.
    pub key: &'static str,
    /// Whether the key can appear in `Status.info`.
    pub adapter: bool,
    /// Whether the key can appear in `Device.info`.
    pub device: bool,
    /// Whether the key can be a setting.
    pub setting: bool,
    pub kind: Kind,
    pub unit: Option<&'static str>,
    /// Enum values in display order.
    pub values: &'static [&'static str],
}

include!(concat!(env!("OUT_DIR"), "/keys.rs"));

/// The catalog entry for `key`, and the index it carries when the entry is a template.
pub fn lookup(key: &str) -> Option<(&'static Key, Option<u32>)> {
    KEYS.iter().find_map(|entry| {
        let mut index = None;
        let mut levels = key.split('.');
        for pattern in entry.key.split('.') {
            let level = levels.next()?;
            if pattern == "{n}" {
                if level.is_empty() || !level.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                index = Some(level.parse().ok()?);
            } else if pattern != level {
                return None;
            }
        }
        levels.next().is_none().then_some((entry, index))
    })
}

/// The concrete key for index `n` of a template such as `pointer.sensor.{n}.dpi`.
pub fn indexed(template: &str, n: u32) -> String {
    template.replace("{n}", &alloc::format!("{n}"))
}
