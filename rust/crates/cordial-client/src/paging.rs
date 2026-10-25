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
    /// Whether the entry carries its key. An entry of a variant newer than this client reads
    /// as unset and carries none.
    fn keyed(_entry: &Self::Entry) -> bool {
        true
    }
    /// The listing's order.
    fn order(a: &Self::Key, b: &Self::Key) -> Ordering;

    /// The key of the page's last entry that has one.
    fn last_key(&self) -> Option<Self::Key> {
        self.entries()
            .iter()
            .rev()
            .find(|e| Self::keyed(e))
            .map(Self::key)
    }

    /// The key to read the next page after, or `None` when the page ends the listing. A page
    /// without an entry whose key can be read that does not end the listing has no next key
    /// either; [`read_pages`] refuses it.
    fn next(&self) -> Option<Self::Key> {
        if self.end() {
            return None;
        }
        self.last_key()
    }

    /// Whether `key` falls in the range this page covers, read after `after`: past `after`, and
    /// up to the page's last keyed entry unless the page ends the listing. A client replaces
    /// what it holds in this range with the page's entries.
    fn covers(&self, after: Option<&Self::Key>, key: &Self::Key) -> bool {
        if after.is_some_and(|a| Self::order(key, a) != Ordering::Greater) {
            return false;
        }
        if self.end() {
            return true;
        }
        self.last_key()
            .is_some_and(|last| Self::order(key, &last) != Ordering::Greater)
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

/// The ID a device listing entry names; 0 for an entry variant newer than this client.
pub fn device_entry_id(entry: &p::DeviceListEntry) -> u32 {
    match entry.entry {
        Some(device_list_entry::Entry::Device(ref d)) => d.id,
        Some(device_list_entry::Entry::Unreadable(id)) => id,
        None => 0,
    }
}

/// The ID a profile listing entry names; 0 for an entry variant newer than this client.
pub fn profile_entry_id(entry: &p::ProfileListEntry) -> u32 {
    match entry.entry {
        Some(profile_list_entry::Entry::Profile(ref profile)) => profile.id,
        Some(profile_list_entry::Entry::Unreadable(id)) => id,
        None => 0,
    }
}

/// What identifies a complete settings object. An unknown group has no usable identity.
pub fn settings_ref(object: &p::SettingsObject) -> p::SettingsRef {
    use p::{SettingsGroup as G, settings_object::Group};
    let (group, index) = match &object.group {
        Some(Group::Keyboard(_)) => (G::Keyboard, None),
        Some(Group::Power(_)) => (G::Power, None),
        Some(Group::Backlight(_)) => (G::Backlight, None),
        Some(Group::PointerSensor(sensor)) => (G::PointerSensor, Some(sensor.index)),
        Some(Group::Wheel(_)) => (G::Wheel, None),
        Some(Group::Thumbwheel(_)) => (G::Thumbwheel, None),
        None => (G::Unknown, None),
    };
    p::SettingsRef {
        integration: object.integration,
        group: group as i32,
        index,
    }
}

/// The order of settings objects: integration, group, then component index.
pub fn settings_order(a: &p::SettingsRef, b: &p::SettingsRef) -> Ordering {
    (a.integration, a.group, a.index).cmp(&(b.integration, b.group, b.index))
}

/// The order of usages: usage page, then usage.
pub fn usage_order(a: &p::Usage, b: &p::Usage) -> Ordering {
    (a.usage_page, a.usage).cmp(&(b.usage_page, b.usage))
}

/// What identifies a rule: its input and the input's collection.
pub fn rule_ref(rule: &p::ProfileRule) -> p::ProfileRuleRef {
    p::ProfileRuleRef {
        input: rule.input,
        collection: rule.collection,
    }
}

/// The order of rules: input, then collection, a missing usage reading as usage 0 of page 0.
pub fn rule_order(a: &p::ProfileRuleRef, b: &p::ProfileRuleRef) -> Ordering {
    let key = |u: &Option<p::Usage>| u.unwrap_or_default();
    usage_order(&key(&a.input), &key(&b.input))
        .then_with(|| usage_order(&key(&a.collection), &key(&b.collection)))
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
    fn keyed(entry: &Self::Entry) -> bool {
        entry.entry.is_some()
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
    fn keyed(entry: &Self::Entry) -> bool {
        entry.entry.is_some()
    }
    fn order(a: &u32, b: &u32) -> Ordering {
        a.cmp(b)
    }
}

impl Page for p::ProfileRules {
    type Entry = p::ProfileRule;
    type Key = p::ProfileRuleRef;
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
    fn key(entry: &Self::Entry) -> p::ProfileRuleRef {
        rule_ref(entry)
    }
    fn order(a: &p::ProfileRuleRef, b: &p::ProfileRuleRef) -> Ordering {
        rule_order(a, b)
    }
}

impl Page for p::DeviceSettings {
    type Entry = p::SettingsObject;
    type Key = p::SettingsRef;
    fn from_result(result: response::Result) -> Option<Self> {
        match result {
            response::Result::Settings(settings) => Some(settings),
            _ => None,
        }
    }
    fn entries(&self) -> &[Self::Entry] {
        &self.objects
    }
    fn into_entries(self) -> Vec<Self::Entry> {
        self.objects
    }
    fn end(&self) -> bool {
        self.end
    }
    fn key(entry: &Self::Entry) -> p::SettingsRef {
        settings_ref(entry)
    }
    fn keyed(entry: &Self::Entry) -> bool {
        entry.group.is_some()
    }
    fn order(a: &p::SettingsRef, b: &p::SettingsRef) -> Ordering {
        settings_order(a, b)
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
    fn entries_of_unknown_variants_are_paged_past() {
        let page = |ids: &[Option<u32>], end: bool| p::DeviceList {
            entries: ids
                .iter()
                .map(|id| p::DeviceListEntry {
                    entry: id.map(device_list_entry::Entry::Unreadable),
                })
                .collect(),
            end,
        };
        let mut asked = Vec::new();
        let all = read_pages(
            |after: Option<&u32>| {
                asked.push(after.copied());
                Ok::<_, ()>(match after {
                    None => page(&[Some(1), Some(3), None], false),
                    Some(3) => page(&[None, Some(7), None], false),
                    _ => page(&[None], true),
                })
            },
            || (),
        )
        .unwrap();
        assert_eq!(asked, [None, Some(3), Some(7)]);
        assert_eq!(all.len(), 7);
        let first = page(&[Some(4), None], false);
        assert!(first.covers(None, &4) && !first.covers(None, &5));
        let unknown = read_pages(|_: Option<&u32>| Ok(page(&[None], false)), || "stalled");
        assert_eq!(unknown.unwrap_err(), "stalled");
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
    fn settings_objects_derive_identity_and_skip_unknown_groups_for_cursors() {
        let object = p::SettingsObject {
            integration: p::IntegrationKind::Hidpp as i32,
            group: Some(p::settings_object::Group::PointerSensor(
                p::PointerSensorSettings {
                    index: 0,
                    dpi: Some(p::IntegerSetting::default()),
                },
            )),
        };
        let key = p::SettingsRef {
            integration: p::IntegrationKind::Hidpp as i32,
            group: p::SettingsGroup::PointerSensor as i32,
            index: Some(0),
        };
        assert_eq!(settings_ref(&object), key);
        let page = p::DeviceSettings {
            device: 1,
            objects: vec![
                object,
                p::SettingsObject {
                    integration: 1,
                    group: None,
                },
            ],
            end: false,
        };
        assert_eq!(page.next(), Some(key));
        let unknown = p::DeviceSettings {
            objects: vec![p::SettingsObject {
                integration: 1,
                group: None,
            }],
            ..Default::default()
        };
        assert!(read_pages(|_| Ok::<_, ()>(unknown.clone()), || ()).is_err());
    }

    #[test]
    fn settings_order_by_integration_group_and_index() {
        let r = |integration, group, index| p::SettingsRef {
            integration,
            group,
            index,
        };
        assert_eq!(
            settings_order(&r(1, 2, None), &r(1, 1, None)),
            Ordering::Greater
        );
        assert_eq!(
            settings_order(&r(0, 6, None), &r(1, 1, None)),
            Ordering::Less
        );
        assert_eq!(
            settings_order(&r(1, 4, Some(0)), &r(1, 4, Some(1))),
            Ordering::Less
        );
    }
}
