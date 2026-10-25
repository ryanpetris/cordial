//! Listing entry identities and ordering.
use cordial_protocol::{self as p, device_list_entry, profile_list_entry};
use std::cmp::Ordering;

/// The ID a device listing entry names; 0 for an entry variant newer than this client.
pub fn device_entry_id(entry: &p::DeviceListEntry) -> u32 {
    match entry.entry {
        Some(device_list_entry::Entry::Device(ref d)) => d.id,
        Some(device_list_entry::Entry::Error(ref error)) => error.id,
        None => 0,
    }
}

/// The ID a profile listing entry names; 0 for an entry variant newer than this client.
pub fn profile_entry_id(entry: &p::ProfileListEntry) -> u32 {
    match entry.entry {
        Some(profile_list_entry::Entry::Profile(ref profile)) => profile.id,
        Some(profile_list_entry::Entry::Error(ref error)) => error.id,
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
