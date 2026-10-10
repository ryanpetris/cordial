//! The session's view of the Dongle, built from responses and events in the order the Dongle
//! sent them. A device, profile or status record arrives whole and replaces what the view held. A
//! listing page replaces what the view held in the range it covers, and change events update the
//! settings and warnings the view holds. An accepted Set request carries no result: the Dongle
//! now holds what was sent in the form the command saves it, so the view takes the sent values in
//! that form.
use cordial_client::paging::{self, Page};
use cordial_protocol::{
    self as p, device_list_entry, event, profile_list_entry, request::Command, response, setting,
    setting_change, value::Value,
};
use std::collections::{BTreeMap, BTreeSet};

pub type SessionId = u64;

/// A selectable row: a saved device or a scan candidate of the device list, or the connected
/// adapter. Device and candidate IDs are separate sequences, so the kind is part of the identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Item {
    Device(u32),
    Candidate(u32),
    Adapter,
}

/// A command this session is running, for progress shown beside its target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pending {
    pub command: &'static str,
    /// The device, candidate or profile ID the command acts on; 0 for none.
    pub target: u32,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub session: SessionId,
    pub port: String,
    pub status: p::Status,
    /// Saved devices in ascending ID order.
    pub devices: Vec<p::Device>,
    /// Saved devices whose records the Dongle couldn't read when it last listed them.
    pub unreadable: BTreeSet<u32>,
    /// Candidates of the current scan, in the order found.
    pub candidates: Vec<p::Candidate>,
    /// The transports of the running scan, from its start until scan_done.
    pub scanning: Option<Vec<p::Transport>>,
    /// How the last scan ended.
    pub last_scan: Option<p::ScanDone>,
    /// The latest pairing event.
    pub pairing: Option<p::Pairing>,
    /// Each device's settings, once listed or reported.
    pub settings: BTreeMap<u32, Vec<p::Setting>>,
    /// Each device's warnings, once listed or reported.
    pub warnings: BTreeMap<u32, Vec<p::DeviceWarning>>,
    /// Profiles by ID, from listed pages, results and events. It holds only the profiles this
    /// session has seen, never necessarily all of them.
    pub profiles: BTreeMap<u32, p::Profile>,
    pub pending: Vec<Pending>,
    /// The connection is open.
    pub available: bool,
    /// The saved devices have been listed since the Dongle became ready.
    pub loaded: bool,
    /// Candidates hidden locally until the next scan.
    pub hidden: BTreeSet<u32>,
}

/// The cursor an ID listing was read after, with 0 for the start.
fn id_after(after: u32) -> Option<u32> {
    (after != 0).then_some(after)
}

/// Replaces the entries of `list` in the range `page` covers with the page's entries, keeping the
/// list in the listing's order.
fn merge_page<P: Page>(list: &mut Vec<P::Entry>, after: Option<&P::Key>, page: &P) {
    list.retain(|e| !page.covers(after, &P::key(e)));
    list.extend(page.entries().iter().cloned());
    list.sort_by(|a, b| P::order(&P::key(a), &P::key(b)));
}

/// Sets a setting's saved value, when the value fits its type.
fn set_saved(s: &mut p::Setting, value: Option<Value>) {
    match (&mut s.r#type, value) {
        (Some(setting::Type::Bool(t)), Some(Value::Bool(v))) => t.saved = Some(v),
        (Some(setting::Type::Integer(t)), Some(Value::Integer(v))) => t.saved = Some(v),
        (Some(setting::Type::Enum(t)), Some(Value::Text(v))) => t.saved = Some(v),
        (Some(setting::Type::Text(t)), Some(Value::Text(v))) => t.saved = Some(v),
        (Some(setting::Type::Color(t)), Some(Value::Color(v))) => t.saved = Some(v),
        (Some(setting::Type::Bool(t)), None) => t.saved = None,
        (Some(setting::Type::Integer(t)), None) => t.saved = None,
        (Some(setting::Type::Enum(t)), None) => t.saved = None,
        (Some(setting::Type::Text(t)), None) => t.saved = None,
        (Some(setting::Type::Color(t)), None) => t.saved = None,
        _ => {}
    }
}

impl State {
    pub fn device(&self, id: u32) -> Option<&p::Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn candidate(&self, id: u32) -> Option<&p::Candidate> {
        self.candidates.iter().find(|c| c.id == id)
    }

    /// The device's settings, or none when they have not been listed.
    pub fn settings_of(&self, id: u32) -> &[p::Setting] {
        self.settings.get(&id).map_or(&[], Vec::as_slice)
    }

    pub fn warnings_of(&self, id: u32) -> &[p::DeviceWarning] {
        self.warnings.get(&id).map_or(&[], Vec::as_slice)
    }

    pub fn profile(&self, id: u32) -> Option<&p::Profile> {
        self.profiles.get(&id)
    }

    /// Whether this session runs `command`, for `target` unless it is 0.
    pub fn pending_for(&self, command: &str, target: u32) -> bool {
        self.pending
            .iter()
            .any(|p| p.command == command && (target == 0 || p.target == target))
    }

    pub fn ready(&self) -> bool {
        self.available && self.status.ready && self.loaded
    }

    fn put_device(&mut self, device: p::Device) {
        self.unreadable.remove(&device.id);
        match self.devices.binary_search_by_key(&device.id, |d| d.id) {
            Ok(i) => self.devices[i] = device,
            Err(i) => self.devices.insert(i, device),
        }
    }

    fn remove_device(&mut self, id: u32) {
        self.devices.retain(|d| d.id != id);
        self.unreadable.remove(&id);
        self.settings.remove(&id);
        self.warnings.remove(&id);
    }

    /// Replaces the devices in a page's range with the page's devices.
    fn device_page(&mut self, after: u32, list: &p::DeviceList) {
        let after = id_after(after);
        let listed: Vec<u32> = list.entries.iter().map(paging::device_entry_id).collect();
        let gone: Vec<u32> = self
            .devices
            .iter()
            .map(|d| d.id)
            .chain(self.unreadable.iter().copied())
            .filter(|id| list.covers(after.as_ref(), id) && !listed.contains(id))
            .collect();
        for id in gone {
            self.remove_device(id);
        }
        for entry in &list.entries {
            match &entry.entry {
                Some(device_list_entry::Entry::Device(d)) => self.put_device(d.clone()),
                Some(device_list_entry::Entry::Unreadable(id)) => {
                    self.devices.retain(|d| d.id != *id);
                    self.unreadable.insert(*id);
                }
                None => {}
            }
        }
    }

    /// Replaces the known profiles in a page's range with the page's profiles.
    fn profile_page(&mut self, after: u32, list: &p::ProfileList) {
        let after = id_after(after);
        self.profiles
            .retain(|id, _| !list.covers(after.as_ref(), id));
        for entry in &list.entries {
            if let Some(profile_list_entry::Entry::Profile(profile)) = &entry.entry {
                self.profiles.insert(profile.id, profile.clone());
            }
        }
    }

    /// Applies settings changes to a device whose settings the view holds.
    fn settings_changed(&mut self, changed: &p::SettingsChanged) {
        let Some(list) = self.settings.get_mut(&changed.device) else {
            return;
        };
        list.retain(|s| {
            let key = paging::setting_ref(s);
            !changed.removed.contains(&key)
                && !changed
                    .changed
                    .iter()
                    .any(|c| paging::setting_ref(c) == key)
        });
        list.extend(changed.changed.iter().cloned());
        list.sort_by(|a, b| {
            paging::setting_order(&paging::setting_ref(a), &paging::setting_ref(b))
        });
    }

    /// Applies warning changes. A device has warnings only while it has a link, and those of
    /// every linked device are listed once the devices are, so a device the view holds none for
    /// has none.
    fn warnings_changed(&mut self, changed: &p::WarningsChanged) {
        let list = self.warnings.entry(changed.device).or_default();
        list.retain(|w| !changed.removed.contains(w));
        for w in &changed.added {
            if !list.contains(w) {
                list.push(*w);
            }
        }
        list.sort_by(paging::warning_order);
    }

    /// Takes the preferences an accepted SetAdapter saved, with the name trimmed as the adapter
    /// saves it. A name reset to the default leaves the name to the adapter event that follows.
    fn adapter_saved(&mut self, update: &p::SetAdapter) {
        let status = &mut self.status;
        if let Some(name) = update
            .name
            .as_deref()
            .and_then(crate::commands::adapter_name)
        {
            status.name = name.to_owned();
        }
        if let Some(platform) = update.platform {
            status.platform = platform;
        }
        for t in &update.transports {
            if let (Some(on), Some(support)) = (
                t.enabled,
                status
                    .transports
                    .iter_mut()
                    .find(|s| s.transport == t.transport),
            ) {
                support.enabled = Some(on);
            }
        }
        for i in &update.configuration_interfaces {
            let Some(support) = status
                .configuration_interfaces
                .iter_mut()
                .find(|s| s.interface == i.interface)
            else {
                continue;
            };
            if let Some(on) = i.enabled {
                support.enabled = on;
            }
            if let Some(profile) = i.profile {
                support.profile = profile;
            }
        }
    }

    /// Takes the preferences an accepted SetDevice saved.
    fn device_saved(&mut self, update: &p::SetDevice) {
        let Some(d) = self.devices.iter_mut().find(|d| d.id == update.device) else {
            return;
        };
        if let Some(on) = update.enabled {
            d.enabled = on;
        }
        if let Some(on) = update.trusted {
            d.trusted = on;
        }
        if let Some(on) = update.blocked {
            d.blocked = on;
        }
        if update.enabled.is_some() || update.blocked.is_some() {
            d.inactive = inactive_after(d).map(|r| r as i32);
        }
        for i in &update.integrations {
            let Some(on) = i.enabled else { continue };
            match d.integrations.iter_mut().find(|o| o.kind == i.kind) {
                Some(o) => o.enabled = on,
                None => d.integrations.push(p::Integration {
                    kind: i.kind,
                    enabled: on,
                    ..Default::default()
                }),
            }
        }
        if let Some(layers) = &update.profiles {
            d.profiles = Some(layers.clone());
        }
    }

    /// Takes the values an accepted SetSettings saved and forgot, for a device whose settings
    /// the view holds. A saved value waits to be applied.
    fn settings_saved(&mut self, update: &p::SetSettings) {
        let Some(list) = self.settings.get_mut(&update.device) else {
            return;
        };
        for change in &update.changes {
            let Some(s) = list
                .iter_mut()
                .find(|s| s.integration == change.integration && s.key == change.key)
            else {
                continue;
            };
            match &change.change {
                Some(setting_change::Change::Value(v)) => {
                    set_saved(s, v.value.clone());
                    s.status = Some(setting::Status::State(p::SettingState::Pending as i32));
                }
                Some(setting_change::Change::Forget(_)) => {
                    set_saved(s, None);
                    s.status = None;
                }
                None => {}
            }
        }
    }

    /// Takes the profile an accepted CreateProfile or CopyProfile saved: the name sent, with no
    /// roles for an empty profile and the source's roles for a copy.
    fn profile_created(&mut self, command: &Command, id: u32) {
        let (name, roles) = match command {
            Command::CreateProfile(c) => (c.name.clone(), Vec::new()),
            Command::CopyProfile(c) => (
                c.name.clone(),
                self.profiles
                    .get(&c.profile)
                    .map(|source| source.roles.clone())
                    .unwrap_or_default(),
            ),
            _ => return,
        };
        self.profiles.insert(id, p::Profile { id, name, roles });
    }

    /// Applies an event.
    pub fn event(&mut self, kind: &event::Kind) {
        match kind {
            event::Kind::Adapter(status) => self.status = status.clone(),
            event::Kind::Device(device) => self.put_device(device.clone()),
            event::Kind::DeviceRemoved(removed) => self.remove_device(removed.id),
            event::Kind::SettingsChanged(s) => self.settings_changed(s),
            event::Kind::WarningsChanged(w) => self.warnings_changed(w),
            event::Kind::ScanFound(c) => match self.candidates.iter_mut().find(|o| o.id == c.id) {
                Some(o) => *o = c.clone(),
                None => self.candidates.push(c.clone()),
            },
            event::Kind::ScanDone(done) => {
                self.scanning = None;
                self.last_scan = Some(*done);
            }
            event::Kind::Pairing(pairing) => self.pairing = Some(pairing.clone()),
            event::Kind::Profile(profile) => {
                self.profiles.insert(profile.id, profile.clone());
            }
            event::Kind::ProfileRemoved(removed) => {
                self.profiles.remove(&removed.id);
            }
        }
    }

    /// Applies the records a response carries. An accepted scan or pairing starts here, so
    /// events before its response still belong to the earlier one.
    pub fn response(&mut self, request: &p::Request, response: &p::Response) {
        let accepted = !matches!(response.result, Some(response::Result::Error(_)));
        match &request.command {
            Some(Command::StartScan(scan)) if accepted => self.scan_started(
                scan.transports
                    .iter()
                    .filter_map(|t| p::Transport::try_from(*t).ok())
                    .collect(),
            ),
            Some(Command::StartPairing(_)) if accepted => self.pairing = None,
            Some(Command::DeleteProfile(d)) if accepted => {
                self.profiles.remove(&d.profile);
            }
            Some(Command::SetAdapter(update)) if accepted => self.adapter_saved(update),
            Some(Command::SetDevice(update)) if accepted => self.device_saved(update),
            Some(Command::SetSettings(update)) if accepted => self.settings_saved(update),
            _ => {}
        }
        match (&request.command, &response.result) {
            (_, Some(response::Result::Status(status))) => self.status = status.clone(),
            (_, Some(response::Result::Device(device))) => self.put_device(device.clone()),
            (Some(Command::ListDevices(l)), Some(response::Result::Devices(list))) => {
                self.device_page(l.after, list)
            }
            (Some(Command::ListSettings(l)), Some(response::Result::Settings(s)))
                if self.device(s.device).is_some() =>
            {
                let list = self.settings.entry(s.device).or_default();
                merge_page(list, l.after.as_ref(), s);
            }
            (Some(Command::ListWarnings(l)), Some(response::Result::Warnings(w)))
                if self.device(w.device).is_some() =>
            {
                let list = self.warnings.entry(w.device).or_default();
                merge_page(list, l.after.as_ref(), w);
            }
            (Some(Command::ListProfiles(l)), Some(response::Result::Profiles(list))) => {
                self.profile_page(l.after, list)
            }
            (_, Some(response::Result::Profile(profile))) => {
                self.profiles.insert(profile.id, profile.clone());
            }
            (Some(command), Some(response::Result::ProfileCreated(created))) => {
                self.profile_created(command, created.profile)
            }
            _ => {}
        }
    }

    /// A new scan replaces earlier candidates, except one a pairing holds.
    pub fn scan_started(&mut self, transports: Vec<p::Transport>) {
        let held = self
            .pairing
            .as_ref()
            .filter(|p| crate::model::pairing_running(p))
            .map(|p| p.candidate);
        self.candidates.retain(|c| Some(c.id) == held);
        self.hidden.clear();
        self.last_scan = None;
        self.scanning = Some(transports);
    }
}

/// The inactive reason a saved enabled or blocked change leaves, in the adapter's order: transport
/// reasons stay, then blocked, then disabled. An accepted enable was given room, so the device is
/// no longer inactive for capacity.
fn inactive_after(d: &p::Device) -> Option<p::InactiveReason> {
    use p::InactiveReason::{Blocked, Capacity, Disabled, TransportDisabled, UnsupportedTransport};
    match crate::model::inactive(d) {
        Some(r @ (UnsupportedTransport | TransportDisabled)) => Some(r),
        _ if d.blocked => Some(Blocked),
        _ if !d.enabled => Some(Disabled),
        Some(Capacity) => Some(Capacity),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p::{pairing, request::Command};

    fn accepted(command: Command) -> (p::Request, p::Response) {
        (
            p::Request {
                command: Some(command),
            },
            p::Response::default(),
        )
    }

    fn answered(command: Command, result: response::Result) -> (p::Request, p::Response) {
        let (request, mut response) = accepted(command);
        response.result = Some(result);
        (request, response)
    }

    fn device(id: u32) -> p::Device {
        p::Device {
            id,
            ..Default::default()
        }
    }

    #[test]
    fn a_scan_starts_at_its_response() {
        let mut st = State::default();
        st.event(&event::Kind::ScanFound(p::Candidate {
            id: 1,
            ..Default::default()
        }));
        st.scanning = Some(vec![p::Transport::Ble]);
        // The earlier scan's end arrives before the new scan's response.
        st.event(&event::Kind::ScanDone(p::ScanDone::default()));
        let (request, response) = accepted(Command::StartScan(p::StartScan {
            transports: vec![p::Transport::Ble as i32],
            seconds: 10,
        }));
        st.response(&request, &response);
        assert_eq!(st.scanning, Some(vec![p::Transport::Ble]));
        assert!(st.candidates.is_empty() && st.last_scan.is_none());
    }

    #[test]
    fn a_refused_scan_keeps_the_earlier_state() {
        let mut st = State {
            last_scan: Some(p::ScanDone::default()),
            ..Default::default()
        };
        let request = p::Request {
            command: Some(Command::StartScan(p::StartScan::default())),
        };
        let response = p::Response {
            result: Some(response::Result::Error(p::Error::default())),
        };
        st.response(&request, &response);
        assert!(st.scanning.is_none() && st.last_scan.is_some());
    }

    #[test]
    fn device_pages_replace_only_their_range() {
        let mut st = State::default();
        for id in [1, 3, 5, 8] {
            st.event(&event::Kind::Device(device(id)));
        }
        st.settings.insert(3, Vec::new());
        // The first page covers IDs up to 4: 3 is gone and 2 can't be read.
        let entry = |entry| p::DeviceListEntry { entry: Some(entry) };
        let (request, response) = answered(
            Command::ListDevices(p::ListDevices { after: 0 }),
            response::Result::Devices(p::DeviceList {
                entries: vec![
                    entry(device_list_entry::Entry::Device(device(1))),
                    entry(device_list_entry::Entry::Unreadable(2)),
                    entry(device_list_entry::Entry::Device(device(4))),
                ],
                end: false,
            }),
        );
        st.response(&request, &response);
        let ids = |st: &State| st.devices.iter().map(|d| d.id).collect::<Vec<_>>();
        assert_eq!(ids(&st), [1, 4, 5, 8]);
        assert!(!st.settings.contains_key(&3));
        assert_eq!(st.unreadable, BTreeSet::from([2]));
        // The last page covers everything after its cursor.
        let (request, response) = answered(
            Command::ListDevices(p::ListDevices { after: 4 }),
            response::Result::Devices(p::DeviceList {
                entries: vec![
                    entry(device_list_entry::Entry::Device(device(8))),
                    entry(device_list_entry::Entry::Device(device(9))),
                ],
                end: true,
            }),
        );
        st.response(&request, &response);
        assert_eq!(ids(&st), [1, 4, 8, 9]);
        // A device that becomes readable again leaves the unreadable list.
        st.event(&event::Kind::Device(device(2)));
        assert!(st.unreadable.is_empty());
        assert_eq!(ids(&st), [1, 2, 4, 8, 9]);
    }

    #[test]
    fn profile_names_follow_results_and_events() {
        let mut st = State::default();
        let profile = |id: u32, name: &str| p::Profile {
            id,
            name: name.into(),
            roles: vec![p::Role::Mouse as i32],
        };
        st.profiles.insert(1, profile(1, "Source"));
        // A copy takes the name sent and the source's roles.
        let (request, response) = answered(
            Command::CopyProfile(p::CopyProfile {
                profile: 1,
                name: "Work".into(),
            }),
            response::Result::ProfileCreated(p::ProfileCreated { profile: 4 }),
        );
        st.response(&request, &response);
        assert_eq!(*st.profile(4).unwrap(), profile(4, "Work"));
        // A new profile has no roles.
        let (request, response) = answered(
            Command::CreateProfile(p::CreateProfile { name: "New".into() }),
            response::Result::ProfileCreated(p::ProfileCreated { profile: 6 }),
        );
        st.response(&request, &response);
        assert!(st.profile(6).unwrap().roles.is_empty());
        st.profiles.remove(&6);
        st.event(&event::Kind::Profile(profile(4, "Play")));
        assert_eq!(st.profile(4).unwrap().name, "Play");
        st.event(&event::Kind::Profile(profile(9, "Later")));
        // A page drops the profiles of its range it doesn't list.
        let (request, response) = answered(
            Command::ListProfiles(p::ListProfiles { after: 0 }),
            response::Result::Profiles(p::ProfileList {
                entries: vec![
                    p::ProfileListEntry {
                        entry: Some(profile_list_entry::Entry::Profile(profile(2, "Desk"))),
                    },
                    p::ProfileListEntry {
                        entry: Some(profile_list_entry::Entry::Unreadable(5)),
                    },
                ],
                end: false,
            }),
        );
        st.response(&request, &response);
        assert!(st.profile(1).is_none() && st.profile(4).is_none());
        assert_eq!(st.profile(2).unwrap().name, "Desk");
        assert!(st.profile(9).is_some());
        st.event(&event::Kind::ProfileRemoved(p::ProfileRemoved { id: 9 }));
        assert!(st.profile(9).is_none());
    }

    fn integer(key: &str, saved: Option<i64>) -> p::Setting {
        p::Setting {
            integration: p::IntegrationKind::Hidpp as i32,
            key: key.into(),
            status: saved.map(|_| setting::Status::State(p::SettingState::Applied as i32)),
            r#type: Some(setting::Type::Integer(p::IntegerSetting {
                value: Some(1),
                saved,
                limits: None,
            })),
        }
    }

    fn setting_ref(key: &str) -> p::SettingRef {
        p::SettingRef {
            integration: p::IntegrationKind::Hidpp as i32,
            key: key.into(),
        }
    }

    #[test]
    fn setting_pages_and_changes_merge_into_the_listed_settings() {
        let mut st = State::default();
        st.event(&event::Kind::Device(device(1)));
        // Changes to settings that were never listed are left to the listing.
        st.event(&event::Kind::SettingsChanged(p::SettingsChanged {
            device: 1,
            changed: vec![integer("c", None)],
            removed: Vec::new(),
        }));
        assert!(!st.settings.contains_key(&1));
        let list = |after: Option<&str>, settings: Vec<p::Setting>, end: bool| {
            answered(
                Command::ListSettings(p::ListSettings {
                    device: 1,
                    after: after.map(setting_ref),
                }),
                response::Result::Settings(p::DeviceSettings {
                    device: 1,
                    settings,
                    end,
                }),
            )
        };
        let (request, response) = list(None, vec![integer("a", None), integer("b", None)], false);
        st.response(&request, &response);
        let (request, response) = list(Some("b"), vec![integer("d", None)], true);
        st.response(&request, &response);
        let keys = |st: &State| {
            st.settings_of(1)
                .iter()
                .map(|s| s.key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&st), ["a", "b", "d"]);
        st.event(&event::Kind::SettingsChanged(p::SettingsChanged {
            device: 1,
            changed: vec![integer("c", None), integer("a", Some(4))],
            removed: vec![setting_ref("b"), setting_ref("x")],
        }));
        assert_eq!(keys(&st), ["a", "c", "d"]);
        assert_eq!(st.settings_of(1)[0], integer("a", Some(4)));
    }

    #[test]
    fn warning_changes_add_and_remove_warnings() {
        let mut st = State::default();
        let warning = |service| p::DeviceWarning {
            service,
            ..Default::default()
        };
        st.event(&event::Kind::WarningsChanged(p::WarningsChanged {
            device: 1,
            added: vec![warning(2), warning(1)],
            removed: Vec::new(),
        }));
        assert_eq!(st.warnings_of(1), [warning(1), warning(2)]);
        st.event(&event::Kind::WarningsChanged(p::WarningsChanged {
            device: 1,
            added: vec![warning(1)],
            removed: vec![warning(2), warning(3)],
        }));
        assert_eq!(st.warnings_of(1), [warning(1)]);
    }

    #[test]
    fn accepted_set_requests_take_the_values_sent() {
        let mut st = State::default();
        st.status.transports = vec![p::TransportSupport {
            transport: p::Transport::Ble as i32,
            max_enabled: None,
            enabled: Some(true),
        }];
        st.status.name = "Desk".into();
        st.event(&event::Kind::Device(device(1)));
        st.settings
            .insert(1, vec![integer("a", None), integer("b", Some(3))]);
        let (request, response) = accepted(Command::SetAdapter(p::SetAdapter {
            name: Some("Office".into()),
            platform: Some(p::Platform::Mac as i32),
            transports: vec![p::TransportUpdate {
                transport: p::Transport::Ble as i32,
                enabled: Some(false),
            }],
            configuration_interfaces: Vec::new(),
        }));
        st.response(&request, &response);
        assert_eq!(st.status.name, "Office");
        assert_eq!(st.status.platform(), p::Platform::Mac);
        assert_eq!(st.status.transports[0].enabled, Some(false));
        let (request, response) = accepted(Command::SetDevice(p::SetDevice {
            device: 1,
            enabled: Some(true),
            trusted: Some(true),
            integrations: vec![p::IntegrationUpdate {
                kind: p::IntegrationKind::Hidpp as i32,
                enabled: Some(true),
            }],
            profiles: Some(p::ProfileLayers {
                profiles: vec![2, 1],
            }),
            ..Default::default()
        }));
        st.response(&request, &response);
        let d = st.device(1).unwrap();
        assert!(d.enabled && d.trusted && !d.blocked);
        assert!(d.integrations[0].enabled);
        assert_eq!(d.profiles.as_ref().unwrap().profiles, [2, 1]);
        let (request, response) = accepted(Command::SetSettings(p::SetSettings {
            device: 1,
            changes: vec![
                p::SettingChange {
                    integration: p::IntegrationKind::Hidpp as i32,
                    key: "a".into(),
                    change: Some(setting_change::Change::Value(p::Value {
                        value: Some(Value::Integer(7)),
                    })),
                },
                p::SettingChange {
                    integration: p::IntegrationKind::Hidpp as i32,
                    key: "b".into(),
                    change: Some(setting_change::Change::Forget(p::SettingForget {})),
                },
            ],
        }));
        st.response(&request, &response);
        let mut a = integer("a", Some(7));
        a.status = Some(setting::Status::State(p::SettingState::Pending as i32));
        assert_eq!(st.settings_of(1), [a, integer("b", None)]);
        // A refused request changes nothing.
        let request = p::Request {
            command: Some(Command::SetAdapter(p::SetAdapter {
                name: Some("Other".into()),
                ..Default::default()
            })),
        };
        let response = p::Response {
            result: Some(response::Result::Error(p::Error::default())),
        };
        st.response(&request, &response);
        assert_eq!(st.status.name, "Office");
    }

    #[test]
    fn an_accepted_name_is_taken_trimmed() {
        let mut st = State::default();
        st.status.name = "Desk".into();
        let (request, response) = accepted(Command::SetAdapter(p::SetAdapter {
            name: Some("  Office\u{2003}".into()),
            ..Default::default()
        }));
        st.response(&request, &response);
        assert_eq!(st.status.name, "Office");
        // A reset leaves the name to the adapter event that follows.
        let (request, response) = accepted(Command::SetAdapter(p::SetAdapter {
            name: Some(String::new()),
            ..Default::default()
        }));
        st.response(&request, &response);
        assert_eq!(st.status.name, "Office");
    }

    #[test]
    fn an_accepted_disable_or_block_leaves_the_inactive_reason() {
        let mut st = State::default();
        st.devices.push(p::Device {
            id: 1,
            enabled: true,
            ..Default::default()
        });
        let set = |st: &mut State, update: p::SetDevice| {
            let (request, response) = accepted(Command::SetDevice(update));
            st.response(&request, &response);
            crate::model::inactive(&st.devices[0])
        };
        let disabled = p::SetDevice {
            device: 1,
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(set(&mut st, disabled), Some(p::InactiveReason::Disabled));
        let blocked = p::SetDevice {
            device: 1,
            blocked: Some(true),
            ..Default::default()
        };
        assert_eq!(set(&mut st, blocked), Some(p::InactiveReason::Blocked));
        let unblocked = p::SetDevice {
            device: 1,
            blocked: Some(false),
            enabled: Some(true),
            ..Default::default()
        };
        assert_eq!(set(&mut st, unblocked), None);
    }

    #[test]
    fn an_accepted_pairing_forgets_the_previous_result() {
        let mut st = State::default();
        st.event(&event::Kind::Pairing(p::Pairing {
            candidate: 1,
            step: Some(pairing::Step::Failed(p::ErrorCode::Timeout as i32)),
        }));
        let (request, response) = accepted(Command::StartPairing(p::StartPairing { candidate: 1 }));
        st.response(&request, &response);
        assert!(st.pairing.is_none());
    }
}
