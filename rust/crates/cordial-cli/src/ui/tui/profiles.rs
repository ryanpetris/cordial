//! Profiles: an adapter's profile memory, its configuration interfaces, each with a switch and a
//! profile staged until Save, and one page of profiles at a time, each labelled with its roles,
//! with creating, copying and deleting them at once. The picker of a profile for an interface or
//! a device's layers pages on its own. Profile rules are never read or changed here.
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
use cordial_client::paging::Page;
use cordial_protocol::{self as p, ConfigurationInterface, event, profile_list_entry};
use ratatui::{style::Style, text::Line};

/// How a page read moves through the pages once it succeeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    /// The first page.
    Open,
    Forward,
    Back,
    /// The shown page again, or the first when none is shown.
    Again,
}

/// One page of profiles, as the Profiles tab or the picker shows it.
#[derive(Clone, Debug, Default)]
pub struct ProfilePage {
    /// The cursors of the earlier pages shown, for Previous.
    pub back: Vec<u32>,
    /// The shown page's cursor; 0 for the first page.
    pub after: u32,
    pub list: Vec<p::Profile>,
    /// The profiles on the shown page whose records can't be read, by ID.
    pub unreadable: Vec<u32>,
    /// The next page's cursor; 0 on the last page.
    pub next: u32,
    /// A page has been read.
    pub shown: bool,
    /// The read under way: how it moves, and the cursor it asked for.
    pub reading: Option<(Step, u32)>,
    pub error: Option<String>,
}

impl ProfilePage {
    fn covers(&self, id: u32) -> bool {
        id > self.after && (self.next == 0 || id <= self.next)
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

    /// Reads a page for the picker when `picker`, else for the Profiles tab.
    pub(super) fn read_page(&mut self, adapter: &str, picker: bool, step: Step) {
        let Some(page) = self.page_mut(adapter, picker) else {
            return;
        };
        let after = match step {
            Step::Open => 0,
            Step::Forward => page.next,
            Step::Back => page.back.last().copied().unwrap_or(0),
            Step::Again => page.after,
        };
        page.reading = Some((step, after));
        page.error = None;
        self.run(
            adapter,
            Kind::Page {
                picker,
                step,
                after,
            },
            Command::Profiles { after },
        );
    }

    pub(super) fn open_picker(&mut self, adapter: &str) {
        self.read_page(adapter, true, Step::Open);
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

    /// Follows a changed or removed profile on the shown pages.
    pub(super) fn profile_event(&mut self, adapter: &str, kind: &event::Kind) {
        for picker in [false, true] {
            let shown = !picker || self.picker_open(adapter);
            let Some(page) = self.page_mut(adapter, picker) else {
                continue;
            };
            let idle = page.reading.is_none();
            let again = match kind {
                event::Kind::Profile(profile) => {
                    match page.list.iter_mut().find(|p| p.id == profile.id) {
                        Some(entry) => {
                            *entry = profile.clone();
                            false
                        }
                        None => page.shown && page.covers(profile.id),
                    }
                }
                event::Kind::ProfileRemoved(removed) => {
                    let had = page.list.iter().any(|p| p.id == removed.id)
                        || page.unreadable.contains(&removed.id);
                    page.list.retain(|p| p.id != removed.id);
                    page.unreadable.retain(|id| *id != removed.id);
                    had && page.list.is_empty() && page.unreadable.is_empty()
                }
                _ => false,
            };
            if again && shown && idle {
                self.read_page(adapter, picker, Step::Again);
            }
        }
    }

    pub(super) fn page_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Kind::Page {
            picker,
            step,
            after,
        } = job.kind
        else {
            return;
        };
        let adapter = job.adapter.clone();
        let Some(page) = self.page_mut(&adapter, picker) else {
            return;
        };
        if page.reading != Some((step, after)) {
            return;
        }
        page.reading = None;
        match result {
            Ok(Outcome::Profiles { after: shown, list }) => {
                match step {
                    Step::Open => page.back.clear(),
                    Step::Forward => page.back.push(page.after),
                    Step::Back => {
                        page.back.pop();
                    }
                    Step::Again => {}
                }
                page.after = *shown;
                page.shown = true;
                page.list.clear();
                page.unreadable.clear();
                for e in &list.entries {
                    match &e.entry {
                        Some(profile_list_entry::Entry::Profile(profile)) => {
                            page.list.push(profile.clone())
                        }
                        Some(profile_list_entry::Entry::Unreadable(id)) => {
                            page.unreadable.push(*id)
                        }
                        None => {}
                    }
                }
                page.next = list.next().unwrap_or(0);
                // A page that has emptied shows the page before it instead.
                if list.entries.is_empty() && !page.back.is_empty() {
                    self.read_page(&adapter, picker, Step::Back);
                }
            }
            Ok(_) => {}
            Err(e) => page.error = Some(words::failure(e)),
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
                    let step = if *next { Step::Forward } else { Step::Back };
                    self.read_page(adapter, false, step);
                }
            }
            Action::ProfilesRetry if !disabled => self.read_page(adapter, false, Step::Again),
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
            Ok(Outcome::Profile(created)) => {
                if dialog_of_adapter {
                    self.dialog = None;
                    self.form_focused = false;
                }
                // A new profile shows on the page whose range covers it.
                let adapter = job.adapter.clone();
                if let Some(page) = self.page_mut(&adapter, false)
                    && page.shown
                    && page.covers(created.id)
                    && page.reading.is_none()
                {
                    self.read_page(&adapter, false, Step::Again);
                }
            }
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
            let look = if percent >= words::MEMORY_ALERT_PERCENT as u32 {
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
                b.labelled(staged_label(profiles::interface_label(i), staged), controls);
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
                    format!("The adapter couldn't read its profiles. {e}"),
                    err(),
                ),
                row,
            );
        }
        for profile in &page.list {
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
        for id in &page.unreadable {
            b.line(Line::from(vec![
                layout::span(format!("Profile {id}"), dim()),
                layout::span("  Couldn't Read", dim()),
            ]));
        }
        let mut footer = Layout::new(b.width);
        if page.reading.is_some() || self.profiles_busy(&a.id) {
            footer.label(&format!("{}", super::spinner()), layout::accent());
        }
        if !page.back.is_empty() {
            button_if(
                &mut footer,
                "Previous",
                Action::ProfilePage(false),
                Tone::Normal,
                ready,
            );
        }
        if page.next != 0 {
            button_if(
                &mut footer,
                "Next",
                Action::ProfilePage(true),
                Tone::Normal,
                ready,
            );
        }
        let primary = page.list.is_empty() && page.unreadable.is_empty() && page.back.is_empty();
        let tone = if primary { Tone::Primary } else { Tone::Normal };
        button_if(
            &mut footer,
            "+ New Profile",
            Action::ProfileNew,
            tone,
            !disabled && page.shown,
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
        for profile in &page.list {
            let marks = profiles::roles(profile)
                .into_iter()
                .filter_map(words::role_text)
                .collect::<Vec<_>>()
                .join(", ");
            choice(&mut body, words::clean(&profile.name), marks, profile.id);
        }
        if current != 0 && !page.list.iter().any(|p| p.id == current) {
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
        let loading = page.reading.is_some();
        if loading {
            buttons.label(&format!("{}", super::spinner()), layout::accent());
        }
        if !page.back.is_empty() {
            button_if(
                &mut buttons,
                "Previous",
                Action::PickPage(false),
                Tone::Normal,
                !loading,
            );
        }
        if page.next != 0 {
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
