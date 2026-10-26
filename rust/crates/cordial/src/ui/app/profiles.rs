//! Profiles: an adapter's profile memory, its configuration interfaces, each with a switch and a
//! profile staged until Save, and the listed profiles a page at a time, each labelled with its
//! roles, with creating, copying and deleting them at once. The picker of a profile for an
//! interface or a device's layers lists and pages on its own. Profile rules are never read or
//! changed here.
use super::{
    Action, App, Dialog, Job, Kind, Menu, PickFor, Spot, fleet::Fleet, words, world::AdapterView,
};
use crate::{
    controller::{Command, Outcome, Target},
    error::Error,
    profiles,
};
use cordial_client::order::profile_entry_id;
use cordial_protocol::{self as p, event, profile_list_entry};

/// Profiles shown on one page of the Profiles tab or the picker.
pub(crate) const PER_PAGE: usize = 10;

/// The adapter's profiles, as the Profiles tab or the picker shows them a page at a time.
#[derive(Clone, Debug, Default)]
pub struct ProfilePage {
    /// Every listed profile, readable or not, in ID order, kept current by profile events.
    pub entries: Vec<p::ProfileListEntry>,
    /// The shown page, kept within the listing as it shrinks.
    pub index: usize,
    /// The profiles have been listed.
    pub listed: bool,
    pub reading: bool,
    pub error: Option<String>,
}

impl ProfilePage {
    pub(crate) fn pages(&self) -> usize {
        self.entries.len().div_ceil(PER_PAGE).max(1)
    }

    /// Moves back to the last page when the listing no longer reaches the shown one.
    pub(crate) fn clamp(&mut self) {
        self.index = self.index.min(self.pages() - 1);
    }

    pub fn previous(&self) -> bool {
        self.index > 0
    }

    pub fn next(&self) -> bool {
        self.index + 1 < self.pages()
    }

    /// The shown page's readable profiles, and the read errors of those that can't be read.
    pub fn shown(&self) -> (Vec<&p::Profile>, Vec<&p::RecordError>) {
        split(
            self.entries
                .iter()
                .skip(self.index * PER_PAGE)
                .take(PER_PAGE),
        )
    }

    /// Every listed readable profile, and the read errors of those that can't be read.
    pub fn all(&self) -> (Vec<&p::Profile>, Vec<&p::RecordError>) {
        split(&self.entries)
    }

    pub(crate) fn step(&mut self, forward: bool) {
        self.index = if forward {
            self.index + 1
        } else {
            self.index.saturating_sub(1)
        };
        self.clamp();
    }

    pub(crate) fn put(&mut self, profile: &p::Profile) {
        let entry = p::ProfileListEntry {
            entry: Some(profile_list_entry::Entry::Profile(profile.clone())),
        };
        match self
            .entries
            .binary_search_by_key(&profile.id, profile_entry_id)
        {
            Ok(i) => self.entries[i] = entry,
            Err(i) => self.entries.insert(i, entry),
        }
    }
}

/// Readable profiles, and the read errors of those that can't be read.
fn split<'a>(
    entries: impl IntoIterator<Item = &'a p::ProfileListEntry>,
) -> (Vec<&'a p::Profile>, Vec<&'a p::RecordError>) {
    let mut profiles = Vec::new();
    let mut unreadable = Vec::new();
    for e in entries {
        match &e.entry {
            Some(profile_list_entry::Entry::Profile(profile)) => profiles.push(profile),
            Some(profile_list_entry::Entry::Error(r)) => unreadable.push(r),
            None => {}
        }
    }
    (profiles, unreadable)
}

impl<F: Fleet> App<F> {
    pub(crate) fn page_of(&self, adapter: &str, picker: bool) -> Option<&ProfilePage> {
        let s = self.session_of(adapter)?;
        Some(if picker { &s.picker } else { &s.list })
    }
    pub(crate) fn page_mut(&mut self, adapter: &str, picker: bool) -> Option<&mut ProfilePage> {
        let s = self.session_of_mut(adapter)?;
        Some(if picker { &mut s.picker } else { &mut s.list })
    }
    /// The page shown while the adapter is held, else its live page.
    pub fn shown_page(&self, adapter: &str, picker: bool) -> Option<ProfilePage> {
        self.page_of(adapter, picker).cloned().or_else(|| {
            self.held.get(adapter).map(|h| {
                if picker {
                    h.picker.clone()
                } else {
                    h.list.clone()
                }
            })
        })
    }
    /// Lists the profiles for the picker when `picker`, else for the Profiles tab, showing the
    /// first page when `first`.
    pub(crate) fn read_page(&mut self, adapter: &str, picker: bool, first: bool) {
        let Some(page) = self.page_mut(adapter, picker) else {
            return;
        };
        if first {
            page.index = 0;
        }
        page.reading = true;
        page.error = None;
        self.run(adapter, Kind::Page { picker }, Command::Profiles);
    }
    /// Shows the next listed page when `forward`, else the previous one.
    pub(crate) fn step_page(&mut self, adapter: &str, picker: bool, forward: bool) {
        if let Some(page) = self.page_mut(adapter, picker) {
            page.step(forward);
        }
    }
    pub(crate) fn open_picker(&mut self, adapter: &str) {
        self.read_page(adapter, true, true);
    }
    pub(crate) fn picker_open(&self, adapter: &str) -> bool {
        matches!(&self.dialog, Some(Dialog::Pick { adapter: a, .. }) if a == adapter)
    }
    /// Reads the names of profiles in use that nothing has named yet, each once per session.
    pub(crate) fn sync_profiles(&mut self) {
        let mut lookups = Vec::new();
        for s in &mut self.sessions {
            let Some(st) = self.states.get(&s.slot).filter(|st| st.ready()) else {
                continue;
            };
            for id in profiles::unnamed(st) {
                if s.looked_up.insert(id) {
                    lookups.push((s.id.clone(), id));
                }
            }
        }
        for (adapter, id) in lookups {
            self.run(&adapter, Kind::Lookup, Command::ProfileLookup(id));
        }
    }
    /// Follows a changed or removed profile in the listed profiles.
    pub(crate) fn profile_event(&mut self, adapter: &str, kind: &event::Kind) {
        for picker in [false, true] {
            let Some(page) = self.page_mut(adapter, picker).filter(|p| p.listed) else {
                continue;
            };
            match kind {
                event::Kind::Profile(profile) => page.put(profile),
                event::Kind::ProfileRemoved(removed) => {
                    page.entries.retain(|e| profile_entry_id(e) != removed.id);
                    page.clamp();
                }
                _ => {}
            }
        }
    }
    /// Fills the pages waiting for a list with a profile list response. It arrives in order with
    /// the events around it, so the profile events that follow it apply on top.
    pub(crate) fn profiles_listed(&mut self, adapter: &str, list: p::ProfileList) {
        let mut entries: Vec<_> = list
            .entries
            .into_iter()
            .filter(|e| e.entry.is_some())
            .collect();
        entries.sort_by_key(profile_entry_id);
        for picker in [false, true] {
            let Some(page) = self.page_mut(adapter, picker).filter(|p| p.reading) else {
                continue;
            };
            page.entries = entries.clone();
            page.listed = true;
            page.clamp();
        }
    }
    pub(crate) fn page_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Kind::Page { picker } = job.kind else {
            return;
        };
        let Some(page) = self.page_mut(&job.adapter, picker) else {
            return;
        };
        page.reading = false;
        if let Err(e) = result {
            page.error = Some(words::read_failure(e));
        }
    }
    pub fn profiles_busy(&self, adapter: &str) -> bool {
        self.running(adapter, |k| matches!(k, Kind::Profile))
    }
    /// Handles a profile action; false when `action` isn't one.
    pub(crate) fn profiles_action(&mut self, adapter: &str, action: &Action) -> bool {
        let Some(a) = self.adapter(adapter).cloned() else {
            return false;
        };
        let ready = a.live().is_some();
        let disabled = self.profiles_busy(adapter) || !self.adapter_editable(&a) || !ready;
        match action {
            Action::ProfilePage(next) => {
                if ready {
                    self.step_page(adapter, false, *next);
                }
            }
            Action::ProfilesRetry if !disabled => self.read_page(adapter, false, false),
            Action::ProfileMenu(id) if !disabled => {
                self.menu = Some(Menu {
                    items: vec![
                        ("Copy".into(), Some(Action::ProfileCopy(*id))),
                        ("Delete".into(), Some(Action::ProfileDelete(*id))),
                    ],
                });
            }
            Action::ProfileNew if !disabled => {
                self.menu = None;
                self.form.limit = 64;
                self.form.set_value("");
                self.notes.remove(&Spot::Dialog);
                self.dialog = Some(Dialog::ProfileName(adapter.to_owned(), None));
            }
            Action::ProfileCopy(id) if !disabled => {
                self.menu = None;
                let name = self.profile_text(adapter, *id);
                self.form.limit = 64;
                self.form.set_value(&profiles::copy_name(&name));
                self.notes.remove(&Spot::Dialog);
                self.dialog = Some(Dialog::ProfileName(adapter.to_owned(), Some(*id)));
            }
            Action::ProfileDelete(id) if !disabled => {
                self.menu = None;
                self.notes.remove(&Spot::Dialog);
                self.dialog = Some(Dialog::ProfileDelete(adapter.to_owned(), *id));
            }
            _ => return false,
        }
        true
    }
    /// Creates or copies the profile the name dialog names.
    pub(crate) fn submit_profile_name(&mut self, adapter: &str, copy: Option<u32>) {
        let available = self.adapter(adapter).and_then(AdapterView::live).is_some();
        if self.profiles_busy(adapter) || !available {
            return;
        }
        let name = self.form.value();
        let trimmed = name.trim();
        if !profiles::valid_name(trimmed) {
            self.notes
                .insert(Spot::Dialog, words::PROFILE_NAME_INVALID.into());
            return;
        }
        let command = match copy {
            Some(id) => Command::ProfileCopy(Target::Id(id), trimmed.to_owned()),
            None => Command::ProfileCreate(trimmed.to_owned()),
        };
        self.notes.remove(&Spot::Dialog);
        if !self.run(adapter, Kind::Profile, command) {
            self.notes.insert(Spot::Dialog, words::ADAPTER_GONE.into());
        }
    }
    pub(crate) fn delete_profile(&mut self, adapter: &str, id: u32) {
        if self.profiles_busy(adapter) {
            return;
        }
        let in_use = self
            .state_of(adapter)
            .and_then(|st| profiles::in_use(st, id));
        if let Some(why) = in_use {
            self.notes
                .insert(Spot::Dialog, crate::ui::text::sentence(&why));
            return;
        }
        self.notes.remove(&Spot::Dialog);
        if !self.run(
            adapter,
            Kind::Profile,
            Command::ProfileDelete(Target::Id(id)),
        ) {
            self.notes.insert(Spot::Dialog, words::ADAPTER_GONE.into());
        }
    }
    pub(crate) fn profile_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let dialog_of_adapter = matches!(
            &self.dialog,
            Some(Dialog::ProfileName(a, _) | Dialog::ProfileDelete(a, _)) if *a == job.adapter
        );
        match result {
            Ok(_) => {
                if dialog_of_adapter {
                    self.dialog = None;
                }
            }
            Err(e) => {
                if dialog_of_adapter {
                    self.notes.insert(Spot::Dialog, words::failure(e));
                }
            }
        }
    }
    /// Chooses a profile in the picker.
    pub(crate) fn choose_profile(&mut self, adapter: &str, purpose: PickFor, chosen: u32) {
        self.dialog = None;
        match purpose {
            PickFor::Interface(i) => self.stage_interface(adapter, i, None, Some(chosen)),
            PickFor::Layer(device) if chosen != 0 => self.add_layer(adapter, device, chosen),
            PickFor::Layer(_) => {}
        }
    }
}
