//! Configuration interfaces: external editing protocols the adapter exposes over USB, each with
//! its own saved enabled flag and profile.
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interface {
    Via,
    Vial,
}
impl Interface {
    /// Every interface this firmware supports, in USB interface order.
    pub const ALL: [Interface; 2] = [Interface::Via, Interface::Vial];
    /// Interfaces this firmware cannot expose together with `self`. VIA and Vial share the Raw HID
    /// usage editors look for.
    pub fn conflicts(self) -> &'static [Interface] {
        match self {
            Interface::Via => &[Interface::Vial],
            Interface::Vial => &[Interface::Via],
        }
    }
    /// The bit of this interface in an enabled set.
    pub fn bit(self) -> u8 {
        match self {
            Interface::Via => 1,
            Interface::Vial => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfacePreference {
    pub interface: Interface,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<u64>,
}

/// The saved preference of `interface`: disabled with no profile when it has none.
pub fn preference(saved: &[InterfacePreference], interface: Interface) -> InterfacePreference {
    saved
        .iter()
        .find(|p| p.interface == interface)
        .copied()
        .unwrap_or(InterfacePreference {
            interface,
            enabled: false,
            profile: None,
        })
}

/// The bits of the enabled interfaces.
pub fn enabled(saved: &[InterfacePreference]) -> u8 {
    saved
        .iter()
        .filter(|p| p.enabled)
        .fold(0, |bits, p| bits | p.interface.bit())
}

/// Replaces `interface`'s entry, dropping it when it holds nothing worth saving.
pub fn set(saved: &mut Vec<InterfacePreference>, value: InterfacePreference) {
    saved.retain(|p| p.interface != value.interface);
    if value.enabled || value.profile.is_some() {
        saved.push(value);
        saved.sort_by_key(|p| p.interface);
    }
}

/// Whether saved interface preferences form a configuration the firmware can run: one entry per
/// interface, a profile for each enabled one, and no enabled conflicts.
pub fn valid(saved: &[InterfacePreference]) -> bool {
    saved.iter().enumerate().all(|(i, p)| {
        !saved[..i].iter().any(|q| q.interface == p.interface)
            && p.profile.is_none_or(|id| id != 0)
            && (!p.enabled
                || (p.profile.is_some()
                    && !saved
                        .iter()
                        .any(|q| q.enabled && p.interface.conflicts().contains(&q.interface))))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conflicting_or_profileless_enabled_interfaces_are_invalid() {
        let via = |enabled, profile| InterfacePreference {
            interface: Interface::Via,
            enabled,
            profile,
        };
        let vial = InterfacePreference {
            interface: Interface::Vial,
            enabled: true,
            profile: Some(2),
        };
        assert!(valid(&[via(true, Some(1))]));
        assert!(!valid(&[via(true, None)]));
        assert!(!valid(&[via(true, Some(1)), vial]));
        assert!(valid(&[via(false, Some(1)), vial]));
        let mut saved = alloc::vec![vial];
        set(&mut saved, via(false, None));
        assert_eq!(saved, [vial]);
        set(&mut saved, via(false, Some(3)));
        assert_eq!(enabled(&saved), Interface::Vial.bit());
        assert_eq!(saved.len(), 2);
    }
}
