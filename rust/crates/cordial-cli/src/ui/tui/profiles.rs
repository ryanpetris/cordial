//! Profiles: an adapter's profile memory, its configuration interfaces, each with a switch and a
//! profile staged until Save, and the listed profiles a page at a time, each labelled with its
//! roles, with creating, copying and deleting them at once. The picker of a profile for an
//! interface or a device's layers lists and pages on its own. Profile rules are never read or
//! changed here.
use super::{
    Action, Dialog, Job, Kind, Menu, Model, PickFor, Spot,
    fleet::Fleet,
    layout::{self, Layout, Tone, bold, dim, err, styled},
    render::{DialogView, button_if, staged_label},
    words,
    world::AdapterView,
};
use crate::{
    controller::{Command, Outcome, Target},
    error::Error,
    profiles,
};
use cordial_client::order::profile_entry_id;
use cordial_protocol::{self as p, ConfigurationInterface, event, profile_list_entry};
use ratatui::{style::Style, text::Line};

/// Profiles shown on one page of the Profiles tab or the picker.
const PER_PAGE: usize = 10;

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
    fn pages(&self) -> usize {
        self.entries.len().div_ceil(PER_PAGE).max(1)
    }

    /// Moves back to the last page when the listing no longer reaches the shown one.
    fn clamp(&mut self) {
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
        let start = self.index * PER_PAGE;
        let mut profiles = Vec::new();
        let mut unreadable = Vec::new();
        for e in self.entries.iter().skip(start).take(PER_PAGE) {
            match &e.entry {
                Some(profile_list_entry::Entry::Profile(profile)) => profiles.push(profile),
                Some(profile_list_entry::Entry::Error(r)) => unreadable.push(r),
                None => {}
            }
        }
        (profiles, unreadable)
    }

    fn step(&mut self, forward: bool) {
        self.index = if forward {
            self.index + 1
        } else {
            self.index.saturating_sub(1)
        };
        self.clamp();
    }

    fn put(&mut self, profile: &p::Profile) {
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

impl<F: Fleet> Model<F> {
    fn page_of(&self, adapter: &str, picker: bool) -> Option<&ProfilePage> {
        let s = self.session_of(adapter)?;
        Some(if picker { &s.picker } else { &s.list })
    }

    fn page_mut(&mut self, adapter: &str, picker: bool) -> Option<&mut ProfilePage> {
        let s = self.session_of_mut(adapter)?;
        Some(if picker { &mut s.picker } else { &mut s.list })
    }

    /// The page shown while the adapter is held, else its live page.
    fn shown_page(&self, adapter: &str, picker: bool) -> Option<ProfilePage> {
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
    pub(super) fn read_page(&mut self, adapter: &str, picker: bool, first: bool) {
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
    pub(super) fn step_page(&mut self, adapter: &str, picker: bool, forward: bool) {
        if let Some(page) = self.page_mut(adapter, picker) {
            page.step(forward);
        }
    }

    pub(super) fn open_picker(&mut self, adapter: &str) {
        self.read_page(adapter, true, true);
    }

    pub(super) fn picker_open(&self, adapter: &str) -> bool {
        matches!(&self.dialog, Some(Dialog::Pick { adapter: a, .. }) if a == adapter)
    }

    /// Reads the names of profiles in use that nothing has named yet, each once per session.
    pub(super) fn sync_profiles(&mut self) {
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
    pub(super) fn profile_event(&mut self, adapter: &str, kind: &event::Kind) {
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
    pub(super) fn profiles_listed(&mut self, adapter: &str, list: p::ProfileList) {
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

    pub(super) fn page_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
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

    fn profiles_busy(&self, adapter: &str) -> bool {
        self.running(adapter, |k| matches!(k, Kind::Profile))
    }

    /// Handles a profile action; false when `action` isn't one.
    pub(super) fn profiles_action(&mut self, adapter: &str, action: &Action) -> bool {
        let Some(a) = self.adapter(adapter).cloned() else {
            return false;
        };
        let ready = a.live().is_some();
        let disabled = self.profiles_busy(adapter) || self.adapter_locked(&a) || !ready;
        match action {
            Action::ProfilePage(next) => {
                if ready {
                    self.step_page(adapter, false, *next);
                }
            }
            Action::ProfilesRetry if !disabled => self.read_page(adapter, false, false),
            Action::ProfileMenu(id) if !disabled => {
                let (x, y) = self
                    .hits
                    .iter()
                    .find(|h| h.action == *action)
                    .map_or((0, 0), |h| (h.x, h.y + 1));
                self.menu = Some(Menu {
                    x,
                    y,
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
                self.form_focused = true;
                self.notes.remove(&Spot::Dialog);
                self.dialog = Some(Dialog::ProfileName(adapter.to_owned(), None));
            }
            Action::ProfileCopy(id) if !disabled => {
                self.menu = None;
                let name = self.profile_text(adapter, *id);
                self.form.limit = 64;
                self.form.set_value(&profiles::copy_name(&name));
                self.form_focused = true;
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
    pub(super) fn submit_profile_name(&mut self, adapter: &str, copy: Option<u32>) {
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

    pub(super) fn delete_profile(&mut self, adapter: &str, id: u32) {
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

    pub(super) fn profile_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let dialog_of_adapter = matches!(
            &self.dialog,
            Some(Dialog::ProfileName(a, _) | Dialog::ProfileDelete(a, _)) if *a == job.adapter
        );
        match result {
            Ok(_) => {
                if dialog_of_adapter {
                    self.dialog = None;
                    self.form_focused = false;
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
    pub(super) fn choose_profile(&mut self, adapter: &str, purpose: PickFor, chosen: u32) {
        self.dialog = None;
        match purpose {
            PickFor::Interface(i) => self.stage_interface(adapter, i, None, Some(chosen)),
            PickFor::Layer(device) if chosen != 0 => self.add_layer(adapter, device, chosen),
            PickFor::Layer(_) => {}
        }
    }

    /// The Profiles tab: memory, configuration interfaces and the profiles.
    pub(super) fn adapter_profiles(&self, a: &AdapterView, b: &mut Layout) {
        let Some(view) = a.view() else {
            b.row();
            b.line(styled(a.status_text().0, bold()));
            return;
        };
        if let Some(percent) = words::memory_percent(view) {
            b.section("Profile Memory");
            let look = if words::memory_alert(view) {
                err()
            } else {
                Style::new()
            };
            let filled = (percent.min(100) as usize * 20).div_ceil(100);
            let meter = Line::from(vec![
                layout::span(
                    "█".repeat(filled),
                    if look == err() {
                        err()
                    } else {
                        layout::accent()
                    },
                ),
                layout::span("░".repeat(20 - filled), dim()),
                layout::span(format!(" {percent}%"), look),
            ]);
            b.labelled(styled("In Use", Style::new()), {
                let mut r = Layout::new(b.width);
                r.width = layout::line_width(&meter);
                r.lines.push(meter);
                r
            });
        }
        let locked = self.adapter_locked(a);
        let changes = self.adapter_changes(a);
        let known: Vec<ConfigurationInterface> = profiles::interfaces(view)
            .iter()
            .filter_map(|s| ConfigurationInterface::try_from(s.interface).ok())
            .filter(|i| profiles::INTERFACES.contains(i))
            .collect();
        if !known.is_empty() {
            b.section("Configuration Interfaces");
            let staged_all = profiles::configured(
                view,
                &self
                    .adapter_drafts
                    .get(&a.id)
                    .map(|d| d.interfaces.clone())
                    .unwrap_or_default(),
            );
            for i in known {
                let Some(s) = staged_all.iter().find(|s| s.interface == i as i32) else {
                    continue;
                };
                let staged = changes.interfaces.iter().any(|u| u.interface == i);
                let conflict = staged_all.iter().any(|o| {
                    o.enabled
                        && o.interface != s.interface
                        && (s.conflicts.contains(&o.interface)
                            || o.conflicts.contains(&s.interface))
                });
                let can_enable = s.profile != 0 && !conflict;
                let mut controls = Layout::new(b.width);
                let label = if s.profile == 0 {
                    "Choose Profile".to_owned()
                } else {
                    self.profile_text(&a.id, s.profile)
                };
                button_if(
                    &mut controls,
                    &label,
                    Action::InterfacePick(i),
                    Tone::Normal,
                    !locked,
                );
                let sw = layout::switch(
                    Some(s.enabled),
                    Action::InterfaceEnabled(i, !s.enabled),
                    !locked && (s.enabled || can_enable),
                );
                controls.append(sw);
                let mut label = staged_label(profiles::interface_label(i), staged);
                if !staged && s.enabled && !s.active {
                    label.spans.push(layout::span(" Inactive", dim()));
                }
                b.labelled(label, controls);
            }
        }

        b.section("Profiles");
        let page = self.shown_page(&a.id, false).unwrap_or_default();
        let ready = a.live().is_some();
        let disabled = self.profiles_busy(&a.id) || locked || !ready;
        if let Some(e) = &page.error {
            let mut row = Layout::new(b.width);
            button_if(
                &mut row,
                "Retry",
                Action::ProfilesRetry,
                Tone::Normal,
                !disabled,
            );
            b.labelled(
                styled(
                    if e == words::READ_FAILED {
                        "The adapter couldn't read its profiles. Try again.".to_owned()
                    } else {
                        format!("The adapter couldn't read its profiles. {e}")
                    },
                    err(),
                ),
                row,
            );
        }
        let (list, unreadable) = page.shown();
        for profile in list {
            let mut label = Line::from(layout::span(words::clean(&profile.name), Style::new()));
            let marks: Vec<&str> = profiles::roles(profile)
                .into_iter()
                .filter_map(words::role_text)
                .collect();
            if !marks.is_empty() {
                label
                    .spans
                    .push(layout::span(format!("  {}", marks.join(", ")), dim()));
            }
            let mut row = Layout::new(b.width);
            button_if(
                &mut row,
                "…",
                Action::ProfileMenu(profile.id),
                Tone::Normal,
                !disabled,
            );
            b.labelled(label, row);
        }
        for r in unreadable {
            b.line(Line::from(vec![
                layout::span(format!("Profile {}", r.id), dim()),
                layout::span(
                    format!("  {}", words::couldnt_read(r.error.as_ref())),
                    dim(),
                ),
            ]));
        }
        let mut footer = Layout::new(b.width);
        if page.reading || self.profiles_busy(&a.id) {
            footer.label(&format!("{}", super::spinner()), layout::accent());
        }
        if page.previous() {
            button_if(
                &mut footer,
                "Previous",
                Action::ProfilePage(false),
                Tone::Normal,
                ready,
            );
        }
        if page.next() {
            button_if(
                &mut footer,
                "Next",
                Action::ProfilePage(true),
                Tone::Normal,
                ready,
            );
        }
        let tone = if page.entries.is_empty() {
            Tone::Primary
        } else {
            Tone::Normal
        };
        button_if(
            &mut footer,
            "+ New Profile",
            Action::ProfileNew,
            tone,
            !disabled && page.listed,
        );
        b.add(footer);
    }

    pub(super) fn profile_name_dialog(
        &mut self,
        adapter: &str,
        copy: Option<u32>,
        w: usize,
    ) -> DialogView {
        let mut body = Layout::new(w);
        let line = self.field_line(w);
        body.control(line, Action::Field);
        if let Some(e) = self.notes.get(&Spot::Dialog) {
            body.para(e, err());
        }
        let busy = self.profiles_busy(adapter);
        let available = self.adapter(adapter).and_then(AdapterView::live).is_some();
        let valid = profiles::valid_name(self.form.value().trim());
        let mut buttons = Layout::new(w);
        let mut right = Layout::new(w);
        button_if(&mut right, "Cancel", Action::Cancel, Tone::Normal, !busy);
        let label = if copy.is_some() { "Copy" } else { "Create" };
        button_if(
            &mut right,
            label,
            Action::Submit,
            Tone::Primary,
            !busy && available && valid,
        );
        buttons.align_right(right);
        let title = match copy {
            Some(id) => format!("Copy “{}”", self.profile_text(adapter, id)),
            None => "New Profile".into(),
        };
        DialogView {
            title,
            body,
            buttons,
        }
    }

    pub(super) fn profile_delete_dialog(&mut self, adapter: &str, id: u32, w: usize) -> DialogView {
        let mut body = Layout::new(w);
        if let Some(e) = self.notes.get(&Spot::Dialog) {
            body.para(e, err());
        }
        let busy = self.profiles_busy(adapter);
        let ready = self.adapter(adapter).and_then(AdapterView::live).is_some();
        let mut buttons = Layout::new(w);
        let mut right = Layout::new(w);
        button_if(&mut right, "Cancel", Action::Cancel, Tone::Normal, !busy);
        button_if(
            &mut right,
            "Delete",
            Action::Confirm,
            Tone::Danger,
            !busy && ready,
        );
        buttons.align_right(right);
        DialogView {
            title: format!("Delete “{}”?", self.profile_text(adapter, id)),
            body,
            buttons,
        }
    }

    pub(super) fn pick_dialog(
        &mut self,
        adapter: &str,
        purpose: PickFor,
        chosen: u32,
        w: usize,
    ) -> DialogView {
        let page = self.shown_page(adapter, true).unwrap_or_default();
        let none = match purpose {
            PickFor::Interface(i) => self
                .adapter(adapter)
                .and_then(|a| self.staged_interface(a, i))
                .is_some_and(|s| !s.enabled),
            PickFor::Layer(_) => false,
        };
        let title = match purpose {
            PickFor::Interface(i) => format!("{} Profile", profiles::interface_label(i)),
            PickFor::Layer(_) => "Add Profile".into(),
        };
        let current = match purpose {
            PickFor::Interface(i) => self
                .adapter(adapter)
                .and_then(|a| self.staged_interface(a, i))
                .map_or(0, |s| s.profile),
            PickFor::Layer(_) => 0,
        };
        let mut body = Layout::new(w);
        let choice = |b: &mut Layout, label: String, marks: String, id: u32| {
            let mark = if id == chosen { "(•) " } else { "( ) " };
            let look = if id == chosen {
                layout::title()
            } else {
                Style::new()
            };
            let mut line = Line::from(vec![layout::span(mark, look), layout::span(label, look)]);
            if !marks.is_empty() {
                line.spans.push(layout::span(format!("  {marks}"), dim()));
            }
            b.control(line, Action::Pick(id));
        };
        if none {
            choice(&mut body, "None".into(), String::new(), 0);
        }
        let (list, _) = page.shown();
        for profile in &list {
            let marks = profiles::roles(profile)
                .into_iter()
                .filter_map(words::role_text)
                .collect::<Vec<_>>()
                .join(", ");
            choice(&mut body, words::clean(&profile.name), marks, profile.id);
        }
        if current != 0 && !list.iter().any(|p| p.id == current) {
            choice(
                &mut body,
                self.profile_text(adapter, current),
                String::new(),
                current,
            );
        }
        if let Some(e) = &page.error {
            body.para(e, err());
        }
        let mut buttons = Layout::new(w);
        let loading = page.reading;
        if loading {
            buttons.label(&format!("{}", super::spinner()), layout::accent());
        }
        if page.previous() {
            button_if(
                &mut buttons,
                "Previous",
                Action::PickPage(false),
                Tone::Normal,
                !loading,
            );
        }
        if page.next() {
            button_if(
                &mut buttons,
                "Next",
                Action::PickPage(true),
                Tone::Normal,
                !loading,
            );
        }
        let mut right = Layout::new(w);
        right.button("Cancel", Action::Cancel, Tone::Normal);
        button_if(
            &mut right,
            "Choose",
            Action::Submit,
            Tone::Primary,
            none || chosen != 0,
        );
        buttons.align_right(right);
        DialogView {
            title,
            body,
            buttons,
        }
    }
}
