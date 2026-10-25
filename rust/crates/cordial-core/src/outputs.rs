//! Saved output selections and USB pool sizes, independent of Bluetooth residency.
use alloc::vec::Vec;
use cordial_protocol::{self as p, storage as saved};

use crate::{hid::Gamepad, link::LinkId, model::errors::ErrorCode};

/// Gamepad role bit. The HID radio role occupies bit 16.
pub const GAMEPAD: u8 = 32;
pub const MAX_SLOTS: u8 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Preference {
    pub role: p::Role,
    pub mode: p::OutputMode,
}
impl Preference {
    pub fn saved(self) -> saved::OutputPreference {
        saved::OutputPreference {
            role: saved::Role::Gamepad as i32,
            mode: match self.mode {
                p::OutputMode::Disabled => saved::OutputMode::Disabled,
                p::OutputMode::Hid => saved::OutputMode::Hid,
                p::OutputMode::Xinput => saved::OutputMode::Xinput,
            } as i32,
        }
    }
    /// Unknown roles and modes do not enable output when reading a newer record.
    pub fn from_saved(value: saved::OutputPreference) -> Option<Self> {
        if value.role != saved::Role::Gamepad as i32 {
            return None;
        }
        Some(Self {
            role: p::Role::Gamepad,
            mode: match saved::OutputMode::try_from(value.mode).ok()? {
                saved::OutputMode::Disabled => p::OutputMode::Disabled,
                saved::OutputMode::Hid => p::OutputMode::Hid,
                saved::OutputMode::Xinput => p::OutputMode::Xinput,
            },
        })
    }
    pub fn wire(self) -> p::OutputPreference {
        p::OutputPreference {
            role: self.role as i32,
            mode: self.mode as i32,
        }
    }
}

/// Implemented modes for a role. Board compositions leave this empty until a backend is usable.
#[derive(Clone, Copy, Debug)]
pub struct Support {
    pub role: p::Role,
    pub modes: &'static [p::OutputMode],
}

/// Validates every update and applies it in order to a private policy copy.
pub fn update(
    preferences: &mut Vec<Preference>,
    updates: &[p::OutputPreference],
    support: &[Support],
) -> Result<(), ErrorCode> {
    for value in updates {
        let role = p::Role::try_from(value.role).map_err(|_| ErrorCode::InvalidArgs)?;
        let mode = p::OutputMode::try_from(value.mode).map_err(|_| ErrorCode::InvalidArgs)?;
        if role == p::Role::Unknown {
            return Err(ErrorCode::InvalidArgs);
        }
        if role != p::Role::Gamepad
            || (mode != p::OutputMode::Disabled && !supports(support, role, mode))
        {
            return Err(ErrorCode::UnsupportedTransport);
        }
        preferences.retain(|p| p.role != role);
        if mode != p::OutputMode::Disabled {
            preferences
                .try_reserve(1)
                .map_err(|_| ErrorCode::Capacity)?;
            preferences.push(Preference { role, mode });
        }
    }
    Ok(())
}

pub fn supports(support: &[Support], role: p::Role, mode: p::OutputMode) -> bool {
    support
        .iter()
        .any(|s| s.role == role && s.modes.contains(&mode))
}

pub fn gamepad(preferences: &[Preference]) -> p::OutputMode {
    preferences
        .iter()
        .find(|p| p.role == p::Role::Gamepad)
        .map_or(p::OutputMode::Disabled, |p| p.mode)
}

/// Desired USB slots. These counts do not imply enumeration or active translation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    pub hid_gamepads: u8,
    pub xinput_gamepads: u8,
}

/// Saved USB requirements, including devices that are not Bluetooth residents.
#[derive(Clone)]
pub struct Configured {
    pub id: u64,
    pub mode: p::OutputMode,
    pub roles: u8,
    pub transport: crate::model::identifiers::Transport,
    pub enabled: bool,
    profiles: heapless::Vec<(u64, u8), { crate::profiles::MAX_LAYERS }>,
}
impl Configured {
    pub async fn load<S: crate::storage::RecordStore>(
        policy: &crate::devices::Policy,
        store: &mut S,
    ) -> Self {
        let mut profiles = heapless::Vec::new();
        for &id in &policy.profiles {
            let roles = crate::profiles::rules(store, id)
                .await
                .map_or(0, |rules| rules.output_roles());
            let _ = profiles.push((id, roles));
        }
        Self {
            id: policy.id,
            mode: gamepad(&policy.output_preferences),
            roles: policy.roles.0,
            transport: policy.peer.transport,
            enabled: policy.enabled,
            profiles,
        }
    }
    pub fn desktop(&self) -> bool {
        (self.roles | self.profiles.iter().fold(0, |roles, p| roles | p.1)) & 31 != 0
    }
}

#[derive(Default)]
pub struct Planner {
    pub(crate) selections: Vec<Configured>,
}
impl Planner {
    pub(crate) fn mode(&self, id: u64) -> p::OutputMode {
        self.selections
            .iter()
            .find(|s| s.id == id)
            .map_or(p::OutputMode::Disabled, |s| s.mode)
    }
    pub(crate) fn contains(&self, id: u64) -> bool {
        self.selections.iter().any(|s| s.id == id)
    }
    /// Reserves memory before committing a saved device.
    pub fn prepare(&mut self, id: u64, _: p::OutputMode) -> Result<(), ErrorCode> {
        if !self.contains(id) {
            self.selections
                .try_reserve(1)
                .map_err(|_| ErrorCode::Capacity)?;
        }
        Ok(())
    }
    pub fn configure(&mut self, value: Configured) {
        self.remove(value.id);
        self.selections.push(value);
    }
    /// Sets an output selection for a device without changing its other requirements.
    pub fn set(&mut self, id: u64, mode: p::OutputMode) {
        if let Some(value) = self.selections.iter_mut().find(|s| s.id == id) {
            value.mode = mode;
        } else {
            self.selections.push(Configured {
                id,
                mode,
                roles: 0,
                transport: crate::model::identifiers::Transport::Ble,
                enabled: true,
                profiles: heapless::Vec::new(),
            });
        }
    }
    pub fn roles_changed(&mut self, id: u64, roles: u8) {
        if let Some(device) = self.selections.iter_mut().find(|s| s.id == id) {
            device.roles = roles;
        }
    }
    pub fn remove(&mut self, id: u64) {
        self.selections.retain(|s| s.id != id);
    }
    pub fn profile_changed(&mut self, id: u64, roles: u8) {
        for device in &mut self.selections {
            for profile in &mut device.profiles {
                if profile.0 == id {
                    profile.1 = roles;
                }
            }
        }
    }
    pub fn plan(&self) -> Plan {
        Plan {
            hid_gamepads: self
                .selections
                .iter()
                .filter(|s| s.mode == p::OutputMode::Hid)
                .count()
                .min(4) as u8,
            xinput_gamepads: self
                .selections
                .iter()
                .filter(|s| s.mode == p::OutputMode::Xinput)
                .count()
                .min(4) as u8,
        }
    }
}

/// Actual USB configuration supplied by the transport owner. Generation changes whenever
/// endpoints are rebuilt; readiness is false while USB is reset or suspended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Enumeration {
    pub plan: Plan,
    pub generation: u64,
    pub ready: bool,
}

/// Current controls for one enumerated endpoint. An empty slot has neutral controls.
/// Workers compare both occupant and generation before sending queued controls. On a change,
/// they discard old work and send neutral controls before sending the new occupant's controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SlotSnapshot {
    pub occupant: Option<LinkId>,
    /// Assignment epoch, distinct from the USB enumeration generation. It never wraps.
    pub generation: u64,
    pub gamepad: Gamepad,
}

/// Fixed USB pools. Reading a snapshot does not consume input, so workers can resend it on resume.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Snapshots {
    pub hid: [SlotSnapshot; MAX_SLOTS as usize],
    pub xinput: [SlotSnapshot; MAX_SLOTS as usize],
}

#[derive(Default)]
pub(crate) struct Runtime {
    pub enumeration: Option<Enumeration>,
    pub snapshots: Snapshots,
    epoch: u64,
}
impl Runtime {
    pub fn pool(&self, mode: p::OutputMode) -> &[SlotSnapshot] {
        match mode {
            p::OutputMode::Hid => &self.snapshots.hid,
            p::OutputMode::Xinput => &self.snapshots.xinput,
            p::OutputMode::Disabled => &[],
        }
    }
    pub fn slots(&self, mode: p::OutputMode) -> Option<u32> {
        let plan = self.enumeration?.plan;
        match mode {
            p::OutputMode::Hid => Some(plan.hid_gamepads.min(MAX_SLOTS).into()),
            p::OutputMode::Xinput => Some(plan.xinput_gamepads.min(MAX_SLOTS).into()),
            p::OutputMode::Disabled => None,
        }
    }
    pub fn release(&mut self, link: LinkId) {
        for slot in self
            .snapshots
            .hid
            .iter_mut()
            .chain(&mut self.snapshots.xinput)
        {
            if slot.occupant == Some(link) {
                slot.occupant = None;
                slot.gamepad = Gamepad::default();
            }
        }
    }
    pub fn assign(&mut self, mode: p::OutputMode, link: LinkId, gamepad: Gamepad) {
        let Some(capacity) = self.slots(mode) else {
            return;
        };
        let pool = match mode {
            p::OutputMode::Hid => &mut self.snapshots.hid,
            p::OutputMode::Xinput => &mut self.snapshots.xinput,
            p::OutputMode::Disabled => return,
        };
        if let Some(slot) = pool[..capacity as usize]
            .iter_mut()
            .find(|s| s.occupant.is_none())
            && let Some(epoch) = self.epoch.checked_add(1)
        {
            self.epoch = epoch;
            *slot = SlotSnapshot {
                occupant: Some(link),
                generation: epoch,
                gamepad,
            };
        }
    }
    pub fn update(&mut self, link: LinkId, gamepad: Gamepad) {
        for slot in self
            .snapshots
            .hid
            .iter_mut()
            .chain(&mut self.snapshots.xinput)
        {
            if slot.occupant == Some(link) {
                slot.gamepad = gamepad;
            }
        }
    }
}
