//! Paged listings. Every List command pages the same way: a request names the key of the last
//! entry the client received, and the reply holds the entries that follow it in the listing's
//! order, with `end` set when nothing follows the last of them. The Dongle chooses how many
//! entries a page holds.
use cordial_protocol::{self as p, device_list_entry, feature, profile_list_entry, response};
use std::cmp::Ordering;

/// One page of a listing.
pub trait Page: Sized {
    /// One entry of the listing.
    type Entry: Clone;
    /// What identifies an entry and orders the listing.
    type Key: Clone + PartialEq;

    /// The page a response result carries, when it is one.
    fn from_result(result: response::Result) -> Option<Self>;
    fn entries(&self) -> &[Self::Entry];
    fn into_entries(self) -> Vec<Self::Entry>;
    /// Nothing follows the page's last entry.
    fn end(&self) -> bool;
    fn key(entry: &Self::Entry) -> Self::Key;
    /// The listing's order.
    fn order(a: &Self::Key, b: &Self::Key) -> Ordering;

    /// The key to read the next page after, or `None` when the page ends the listing. A page
    /// without entries that does not end the listing has no next key either; [`read_pages`]
    /// refuses it.
    fn next(&self) -> Option<Self::Key> {
        if self.end() {
            return None;
        }
        self.entries().last().map(Self::key)
    }

    /// Whether `key` falls in the range this page covers, read after `after`: past `after`, and
    /// up to the page's last entry unless the page ends the listing. A client replaces what it
    /// holds in this range with the page's entries.
    fn covers(&self, after: Option<&Self::Key>, key: &Self::Key) -> bool {
        if after.is_some_and(|a| Self::order(key, a) != Ordering::Greater) {
            return false;
        }
        if self.end() {
            return true;
        }
        self.entries()
            .last()
            .is_some_and(|last| Self::order(key, &Self::key(last)) != Ordering::Greater)
    }
}

/// Reads a listing from the start: `read` fetches the page after a key, or the first page for
/// `None`, until a page ends the listing. A page that does not move past the key it was read
/// after, including one without entries that does not end the listing, would never end it, so
/// it fails with `stalled`.
pub fn read_pages<P: Page, E>(
    mut read: impl FnMut(Option<&P::Key>) -> Result<P, E>,
    stalled: impl Fn() -> E,
) -> Result<Vec<P::Entry>, E> {
    let mut all = Vec::new();
    let mut after: Option<P::Key> = None;
    loop {
        let page = read(after.as_ref())?;
        if page.end() {
            all.extend(page.into_entries());
            return Ok(all);
        }
        let Some(next) = page.next() else {
            return Err(stalled());
        };
        if after
            .as_ref()
            .is_some_and(|a| P::order(&next, a) != Ordering::Greater)
        {
            return Err(stalled());
        }
        all.extend(page.into_entries());
        after = Some(next);
    }
}

/// The ID a device listing entry names.
pub fn device_entry_id(entry: &p::DeviceListEntry) -> u32 {
    match entry.entry {
        Some(device_list_entry::Entry::Device(ref d)) => d.id,
        Some(device_list_entry::Entry::Unreadable(id)) => id,
        None => 0,
    }
}

/// The ID a profile listing entry names.
pub fn profile_entry_id(entry: &p::ProfileListEntry) -> u32 {
    match entry.entry {
        Some(profile_list_entry::Entry::Profile(ref profile)) => profile.id,
        Some(profile_list_entry::Entry::Unreadable(id)) => id,
        None => 0,
    }
}

/// What identifies a setting.
pub fn setting_ref(setting: &p::Setting) -> p::SettingRef {
    p::SettingRef {
        integration: setting.integration,
        key: setting.key.clone(),
    }
}

/// The order of settings: integration, then key compared bytewise.
pub fn setting_order(a: &p::SettingRef, b: &p::SettingRef) -> Ordering {
    a.integration
        .cmp(&b.integration)
        .then_with(|| a.key.as_bytes().cmp(b.key.as_bytes()))
}

/// The order of warnings: service, report type, report ID, bit offset, usage page, usage and
/// code, a missing field ordering before any value.
pub fn warning_order(a: &p::DeviceWarning, b: &p::DeviceWarning) -> Ordering {
    let key = |w: &p::DeviceWarning| {
        (
            w.service,
            w.report_type,
            w.report_id,
            w.bit_offset,
            w.usage_page,
            w.usage,
            w.code,
        )
    };
    key(a).cmp(&key(b))
}

/// The order of usages: usage page, then usage.
pub fn usage_order(a: &p::Usage, b: &p::Usage) -> Ordering {
    (a.usage_page, a.usage).cmp(&(b.usage_page, b.usage))
}

/// The input a rule is for; a rule without one reads as usage 0 of page 0.
pub fn rule_input(rule: &p::ProfileRule) -> p::Usage {
    rule.input.unwrap_or_default()
}

/// What identifies a feature table entry.
pub fn feature_ref(feature: &p::Feature) -> p::FeatureRef {
    p::FeatureRef {
        integration: feature.integration,
        index: match &feature.detail {
            Some(feature::Detail::Hidpp(hidpp)) => hidpp.index,
            None => 0,
        },
    }
}

impl Page for p::DeviceList {
    type Entry = p::DeviceListEntry;
    type Key = u32;
    fn from_result(result: response::Result) -> Option<Self> {
        match result {
            response::Result::Devices(list) => Some(list),
            _ => None,
        }
    }
    fn entries(&self) -> &[Self::Entry] {
        &self.entries
    }
    fn into_entries(self) -> Vec<Self::Entry> {
        self.entries
    }
    fn end(&self) -> bool {
        self.end
    }
    fn key(entry: &Self::Entry) -> u32 {
        device_entry_id(entry)
    }
    fn order(a: &u32, b: &u32) -> Ordering {
        a.cmp(b)
    }
}

impl Page for p::ProfileList {
    type Entry = p::ProfileListEntry;
    type Key = u32;
    fn from_result(result: response::Result) -> Option<Self> {
        match result {
            response::Result::Profiles(list) => Some(list),
            _ => None,
        }
    }
    fn entries(&self) -> &[Self::Entry] {
        &self.entries
    }
    fn into_entries(self) -> Vec<Self::Entry> {
        self.entries
    }
    fn end(&self) -> bool {
        self.end
    }
    fn key(entry: &Self::Entry) -> u32 {
        profile_entry_id(entry)
    }
    fn order(a: &u32, b: &u32) -> Ordering {
        a.cmp(b)
    }
}

impl Page for p::ProfileRules {
    type Entry = p::ProfileRule;
    type Key = p::Usage;
    fn from_result(result: response::Result) -> Option<Self> {
        match result {
            response::Result::ProfileRules(rules) => Some(rules),
            _ => None,
        }
    }
    fn entries(&self) -> &[Self::Entry] {
        &self.rules
    }
    fn into_entries(self) -> Vec<Self::Entry> {
        self.rules
    }
    fn end(&self) -> bool {
        self.end
    }
    fn key(entry: &Self::Entry) -> p::Usage {
        rule_input(entry)
    }
    fn order(a: &p::Usage, b: &p::Usage) -> Ordering {
        usage_order(a, b)
    }
}

impl Page for p::DeviceSettings {
    type Entry = p::Setting;
    type Key = p::SettingRef;
    fn from_result(result: response::Result) -> Option<Self> {
        match result {
            response::Result::Settings(settings) => Some(settings),
            _ => None,
        }
    }
    fn entries(&self) -> &[Self::Entry] {
        &self.settings
    }
    fn into_entries(self) -> Vec<Self::Entry> {
        self.settings
    }
    fn end(&self) -> bool {
        self.end
    }
    fn key(entry: &Self::Entry) -> p::SettingRef {
        setting_ref(entry)
    }
    fn order(a: &p::SettingRef, b: &p::SettingRef) -> Ordering {
        setting_order(a, b)
    }
}

impl Page for p::DeviceWarnings {
    type Entry = p::DeviceWarning;
    type Key = p::DeviceWarning;
    fn from_result(result: response::Result) -> Option<Self> {
        match result {
            response::Result::Warnings(warnings) => Some(warnings),
            _ => None,
        }
    }
    fn entries(&self) -> &[Self::Entry] {
        &self.warnings
    }
    fn into_entries(self) -> Vec<Self::Entry> {
        self.warnings
    }
    fn end(&self) -> bool {
        self.end
    }
    fn key(entry: &Self::Entry) -> p::DeviceWarning {
        *entry
    }
    fn order(a: &p::DeviceWarning, b: &p::DeviceWarning) -> Ordering {
        warning_order(a, b)
    }
}

impl Page for p::FeatureList {
    type Entry = p::Feature;
    type Key = p::FeatureRef;
    fn from_result(result: response::Result) -> Option<Self> {
        match result {
            response::Result::Features(features) => Some(features),
            _ => None,
        }
    }
    fn entries(&self) -> &[Self::Entry] {
        &self.features
    }
    fn into_entries(self) -> Vec<Self::Entry> {
        self.features
    }
    fn end(&self) -> bool {
        self.end
    }
    fn key(entry: &Self::Entry) -> p::FeatureRef {
        feature_ref(entry)
    }
    fn order(a: &p::FeatureRef, b: &p::FeatureRef) -> Ordering {
        (a.integration, a.index).cmp(&(b.integration, b.index))
    }
}

impl Page for p::FileList {
    type Entry = p::FileEntry;
    type Key = String;
    fn from_result(result: response::Result) -> Option<Self> {
        match result {
            response::Result::Files(files) => Some(files),
            _ => None,
        }
    }
    fn entries(&self) -> &[Self::Entry] {
        &self.entries
    }
    fn into_entries(self) -> Vec<Self::Entry> {
        self.entries
    }
    fn end(&self) -> bool {
        self.end
    }
    fn key(entry: &Self::Entry) -> String {
        entry.name.clone()
    }
    fn order(a: &String, b: &String) -> Ordering {
        a.as_bytes().cmp(b.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(ids: &[u32], end: bool) -> p::DeviceList {
        p::DeviceList {
            entries: ids
                .iter()
                .map(|id| p::DeviceListEntry {
                    entry: Some(device_list_entry::Entry::Unreadable(*id)),
                })
                .collect(),
            end,
        }
    }

    #[test]
    fn pages_are_read_after_the_last_key_until_the_end() {
        let mut asked = Vec::new();
        let all = read_pages(
            |after: Option<&u32>| {
                asked.push(after.copied());
                Ok::<_, ()>(match after {
                    None => devices(&[1, 3], false),
                    Some(3) => devices(&[7], false),
                    _ => devices(&[], true),
                })
            },
            || (),
        )
        .unwrap();
        assert_eq!(asked, [None, Some(3), Some(7)]);
        assert_eq!(
            all.iter().map(device_entry_id).collect::<Vec<_>>(),
            [1, 3, 7]
        );
    }

    #[test]
    fn a_page_that_does_not_move_on_is_refused() {
        let empty = read_pages(|_: Option<&u32>| Ok(devices(&[], false)), || "stalled");
        assert_eq!(empty.unwrap_err(), "stalled");
        let same = read_pages(
            |after: Option<&u32>| Ok(devices(&[after.copied().unwrap_or(2)], false)),
            || "stalled",
        );
        assert_eq!(same.unwrap_err(), "stalled");
    }

    #[test]
    fn a_page_covers_its_range() {
        let page = devices(&[4, 6], false);
        assert!(!page.covers(Some(&2), &2));
        assert!(page.covers(Some(&2), &3));
        assert!(page.covers(Some(&2), &6));
        assert!(!page.covers(Some(&2), &7));
        let last = devices(&[], true);
        assert!(last.covers(Some(&6), &100));
        assert!(!last.covers(Some(&6), &6));
    }

    #[test]
    fn warnings_order_missing_fields_first() {
        let a = p::DeviceWarning {
            service: 0,
            report_id: None,
            ..Default::default()
        };
        let b = p::DeviceWarning {
            service: 0,
            report_id: Some(0),
            ..Default::default()
        };
        assert_eq!(warning_order(&a, &b), Ordering::Less);
        let c = p::DeviceWarning {
            service: 1,
            ..Default::default()
        };
        assert_eq!(warning_order(&b, &c), Ordering::Less);
    }

    #[test]
    fn settings_order_by_integration_then_key_bytes() {
        let r = |integration: i32, key: &str| p::SettingRef {
            integration,
            key: key.into(),
        };
        assert_eq!(setting_order(&r(1, "b"), &r(1, "a.z")), Ordering::Greater);
        assert_eq!(setting_order(&r(0, "z"), &r(1, "a")), Ordering::Less);
        assert_eq!(setting_order(&r(1, "Z"), &r(1, "a")), Ordering::Less);
    }
}
