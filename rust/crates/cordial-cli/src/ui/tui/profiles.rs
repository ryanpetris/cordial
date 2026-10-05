//! The Profiles views and dialogs. The adapter's Profiles view shows its profile memory, its
//! configuration interfaces, each with its own switch and profile staged until Save, and one
//! page of profiles at a time, each labelled with its roles, with creating, copying and
//! deleting them. A device's Profiles view shows its layers, staged until its own Save. The
//! chooser of a profile for an interface or a device's layers pages on its own. Profile rules
//! are never read or changed here.
use super::{
    Action, Dialog, Job, Model, PickFor, can_set_platform,
    layout::{Choice, Layout, Tone, accent, bold, dim, err, pad_str, styled, title, warn},
    settings::button_if,
    view::spinner,
};
use crate::{
    controller::{Command, Notice, Outcome, State, Target},
    error::Error,
    profiles,
    ui::{
        Backend,
        text::{self, display, display_name},
    },
    view::Item,
};
use cordial_client::paging::Page;
use cordial_protocol::{self as p, ConfigurationInterface, event, profile_list_entry};
use ratatui::text::Line;
use std::collections::HashSet;

/// Shown before saving a configuration interface change that reconnects USB.
pub(super) const RECONNECT_TEXT: &str =
    "The adapter will disconnect from this computer for a moment after it saves these changes.";

/// The label column of the configuration interfaces.
const INTERFACE_KEY: usize = 10;

/// How a page read moves through the pages once it succeeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Step {
    /// The first page.
    Open,
    Forward,
    Back,
    /// The shown page again.
    Again,
}

/// One page of profiles, as a Profiles view or the chooser shows it.
#[derive(Default)]
pub(super) struct ProfilePage {
    /// The cursors of the earlier pages shown, for Previous.
    pub back: Vec<u32>,
    /// The shown page's cursor; 0 for the first page.
    pub after: u32,
    pub list: Vec<p::Profile>,
    /// The profiles on the shown page whose records can't be read, by ID.
    pub unreadable: Vec<u32>,
    /// The next page's cursor; 0 on the last page.
    pub next: u32,
    /// The read under way: how it moves, and the cursor it asked for. The shown page and the
    /// cursors change only once it succeeds.
    pub reading: Option<(Step, u32)>,
    pub load_err: String,
    /// Profiles whose names were read once because nothing had named them.
    pub looked_up: HashSet<u32>,
}

impl ProfilePage {
    /// Whether a profile with this ID belongs on the shown page.
    fn covers(&self, id: u32) -> bool {
        id > self.after && (self.next == 0 || id <= self.next)
    }
}

/// How much of the adapter's profile memory loaded profiles use, in percent.
fn memory_percent(status: &p::Status) -> Option<u32> {
    let support = profiles::support(status).filter(|s| s.memory_budget > 0)?;
    let percent = u64::from(support.memory_used) * 100 / u64::from(support.memory_budget);
    let rest = u64::from(support.memory_used) * 100 % u64::from(support.memory_budget);
    // Rounded half up, as the desktop app shows it.
    let round = u64::from(rest * 2 >= u64::from(support.memory_budget));
    Some((percent + round) as u32)
}

impl<B: Backend> Model<B> {
    /// A profile command of this TUI is running.
    pub(super) fn profiles_running(&self) -> bool {
        self.running(|c| {
            matches!(
                c,
                Command::ProfileCreate(..)
                    | Command::ProfileCopy(..)
                    | Command::ProfileDelete(_)
                    | Command::AdapterSave(_)
            )
        })
    }

    fn profile_name(&self, st: &State, id: u32) -> String {
        display(&profiles::name_of(st, id))
    }

    fn page(&self, picker: bool) -> &ProfilePage {
        if picker {
            &self.picker_page
        } else {
            &self.profile_page
        }
    }

    fn page_mut(&mut self, picker: bool) -> &mut ProfilePage {
        if picker {
            &mut self.picker_page
        } else {
            &mut self.profile_page
        }
    }

    /// Reads the page after `after` for the chooser when `picker`, else for the adapter's
    /// Profiles view; `step` says where it goes once read.
    fn read_page(&mut self, picker: bool, step: Step, after: u32) {
        let page = self.page_mut(picker);
        page.reading = Some((step, after));
        page.load_err.clear();
        let mut job = Job::new(Command::Profiles { after });
        job.lookup = true;
        job.picker = picker;
        self.execute_job(job);
    }

    /// Shows the first page.
    pub(super) fn open_page(&mut self, picker: bool) {
        let page = self.page_mut(picker);
        page.back.clear();
        page.after = 0;
        page.list.clear();
        page.unreadable.clear();
        page.next = 0;
        self.read_page(picker, Step::Open, 0);
    }

    /// Selects a row; another row's Profiles view closes.
    pub(super) fn select(&mut self, item: Item) {
        if self.selected != Some(item) {
            self.profiles_open = None;
            self.profile_err.clear();
        }
        self.selected = Some(item);
        self.detail_scroll = 0;
        if item == Item::Adapter {
            // The adapter row is the Adapters pane's first line.
            self.adapter_scroll = 0;
        }
    }

    /// Returns from a Profiles view to the page it replaced. Staged changes are kept.
    pub(super) fn close_profiles(&mut self) {
        self.profiles_open = None;
        self.detail_scroll = 0;
        self.profile_err.clear();
        self.focus = None;
    }

    /// Whether the adapter's Profiles view is shown.
    pub(super) fn adapter_profiles_open(&self, st: &State) -> bool {
        self.selected == Some(Item::Adapter)
            && self.profiles_open == Some(Item::Adapter)
            && profiles::available(&st.status)
    }

    /// The device whose Profiles view is shown.
    pub(super) fn layers_open<'a>(&self, st: &'a State) -> Option<&'a p::Device> {
        let Some(Item::Device(id)) = self.selected.filter(|s| Some(*s) == self.profiles_open)
        else {
            return None;
        };
        st.device(id).filter(|d| layered(st, d))
    }

    /// Whether a page is shown: the chooser's while it is open, else the adapter's Profiles
    /// view's, including under the dialogs opened from it.
    fn page_shown(&self, picker: bool) -> bool {
        if picker {
            return matches!(self.dialog, Some(Dialog::ProfilePick(_)));
        }
        self.state()
            .is_some_and(|st| self.adapter_profiles_open(&st))
    }

    /// Reads the names of profiles in use that nothing has named yet, each once.
    pub(super) fn sync_profiles(&mut self) {
        if self.preparing {
            return;
        }
        let Some(st) = self.state().filter(State::ready) else {
            return;
        };
        for id in profiles::unnamed(&st) {
            if self.profile_page.looked_up.insert(id) {
                let mut job = Job::new(Command::ProfileLookup(id));
                job.lookup = true;
                self.execute_job(job);
            }
        }
    }

    /// Follows a changed or removed profile on the shown pages. A page is read again when a
    /// profile of its range isn't on it, or when a removal leaves it empty, which moves on to
    /// the profiles that follow or back to the page before. A read already under way is left
    /// to finish: a Next or Previous in flight keeps its result.
    pub(super) fn profile_notice(&mut self, notice: &Notice) {
        let Notice::Event { event, .. } = notice else {
            return;
        };
        for picker in [false, true] {
            let shown = self.page_shown(picker);
            let page = self.page_mut(picker);
            let idle = page.reading.is_none();
            let again = match &event.kind {
                Some(event::Kind::Profile(profile)) => {
                    match page.list.iter_mut().find(|p| p.id == profile.id) {
                        Some(entry) => {
                            *entry = profile.clone();
                            false
                        }
                        None => page.covers(profile.id),
                    }
                }
                Some(event::Kind::ProfileRemoved(removed)) => {
                    let had = page.list.iter().any(|p| p.id == removed.id)
                        || page.unreadable.contains(&removed.id);
                    page.list.retain(|p| p.id != removed.id);
                    page.unreadable.retain(|id| *id != removed.id);
                    had && page.list.is_empty() && page.unreadable.is_empty()
                }
                _ => false,
            };
            if again && shown && idle {
                let after = page.after;
                self.read_page(picker, Step::Again, after);
            }
        }
    }

    /// Records a page read.
    pub(super) fn lookup_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Command::Profiles { after } = &job.command else {
            return;
        };
        let picker = job.picker;
        let page = self.page_mut(picker);
        if page.reading.is_none_or(|(_, sent)| sent != *after) {
            return;
        }
        let Some((step, _)) = page.reading.take() else {
            return;
        };
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
                // A page that has emptied, as after deleting its last profile, shows the page
                // before it instead.
                if list.entries.is_empty()
                    && let Some(&before) = page.back.last()
                {
                    self.read_page(picker, Step::Back, before);
                }
            }
            Ok(_) => {}
            // A failed read leaves the shown page and its cursors as they were.
            Err(e) => page.load_err = text::capitalized(&text::error_words(e)),
        }
    }

    /// The adapter's Profiles view: its heading, body and pinned controls.
    pub(super) fn adapter_profiles(&self, st: &State, w: usize) -> (String, Layout, Layout) {
        let mut body = Layout::new(w.saturating_sub(4));
        let mut pinned = Layout::new(w.saturating_sub(4));
        let running = self.profiles_running();
        let idle = can_set_platform(st) && !running;
        if let Some(percent) = memory_percent(&st.status) {
            body.line(styled("Profile Memory", title()));
            let look = if percent >= 85 {
                warn()
            } else {
                super::layout::plain()
            };
            body.field_at("In Use", INTERFACE_KEY, &format!("{percent}%"), look);
            body.row();
        }
        self.interfaces_section(st, &mut body, idle);
        if running {
            body.para(&format!("{} Saving…", spinner()), warn());
        } else if let Some(e) = self.adapter_error(st) {
            body.para(&format!("✕ {e}"), err());
        } else {
            self.refusal_hint(st, &mut body);
        }
        if !profiles::interfaces(&st.status).is_empty() {
            body.row();
            body.line(styled("Profiles", title()));
        }
        if !running && !self.profile_err.is_empty() {
            body.para(&format!("✕ {}", self.profile_err), err());
        }
        for profile in &self.profile_page.list {
            body.row();
            body.label(&display(&profile.name), bold());
            let roles = text::role_names(&profiles::roles(profile));
            if !roles.is_empty() {
                body.label(&roles, dim());
            }
            if idle {
                body.button("Copy", Action::ProfileCopy(profile.id), Tone::Normal);
                let delete = Action::ProfileDelete(profile.id);
                body.button("Delete", delete, Tone::Danger);
            }
        }
        for id in &self.profile_page.unreadable {
            body.row();
            body.label(&format!("Profile {id}"), bold());
            body.label("Couldn't Read", dim());
        }
        self.page_controls(&mut body, false);
        self.adapter_buttons(st, &mut pinned);
        if idle {
            pinned.button("New Profile", Action::ProfileNew, Tone::Normal);
        }
        pinned.button_right("‹ Back", Action::ProfilesBack, Tone::Normal);
        let heading = format!("Profiles · {}", display(&st.status.name));
        (heading, body, pinned)
    }

    /// A device's Profiles view: its layers, each profile in the order it applies, with controls
    /// to reorder and remove it, and Add Profile while there is room; Save and Discard send or
    /// drop only the layers.
    pub(super) fn device_layers(
        &self,
        st: &State,
        d: &p::Device,
        w: usize,
    ) -> (String, Layout, Layout) {
        let mut body = Layout::new(w.saturating_sub(4));
        let mut pinned = Layout::new(w.saturating_sub(4));
        let saving = self.device_saving(d.id);
        let idle = !saving && can_set_platform(st);
        let layers = self.device_values(st, d).layers.unwrap_or_default();
        let max = profiles::max_layers(&st.status).unwrap_or(0) as usize;
        for (i, id) in layers.iter().enumerate() {
            body.row();
            body.label(
                &format!("{}. {}", i + 1, self.profile_name(st, *id)),
                bold(),
            );
            if let Some(profile) = st.profile(*id) {
                let roles = text::role_names(&profiles::roles(profile));
                if !roles.is_empty() {
                    body.label(&roles, dim());
                }
            }
            button_if(
                &mut body,
                "↑",
                Action::LayerUp(i),
                Tone::Normal,
                idle && i > 0,
            );
            let down = idle && i + 1 < layers.len();
            button_if(&mut body, "↓", Action::LayerDown(i), Tone::Normal, down);
            button_if(
                &mut body,
                "Remove",
                Action::LayerRemove(i),
                Tone::Normal,
                idle,
            );
        }
        let draft = self.device_draft(st, d);
        if draft.layers.is_some() {
            body.line(styled("✎ Changed", accent()));
        }
        if saving {
            body.para(&format!("{} Saving…", spinner()), warn());
        } else if let Some(e) = self.layers_error(st, d) {
            body.para(&format!("✕ Couldn't Save: {e}"), err());
        }
        body.row();
        let room = layers.len() < max;
        button_if(
            &mut body,
            "Add Profile",
            Action::LayerAdd,
            Tone::Normal,
            idle && room,
        );
        let staged = draft.layers.is_some();
        let can_save = idle && staged;
        button_if(
            &mut pinned,
            "Save",
            Action::LayersSave,
            Tone::Primary,
            can_save,
        );
        let can_discard = !saving && staged;
        button_if(
            &mut pinned,
            "Discard",
            Action::LayersDiscard,
            Tone::Normal,
            can_discard,
        );
        pinned.button_right("‹ Back", Action::ProfilesBack, Tone::Normal);
        let heading = format!("Profiles · {}", display_name(Some(&d.name)));
        (heading, body, pinned)
    }

    /// Draws the profile dialogs.
    pub(super) fn profiles_dialog(
        &mut self,
        st: &State,
        heading: &mut String,
        body: &mut Layout,
        pinned: &mut Layout,
    ) {
        let running = self.profiles_running();
        let form_err = self.form_err.clone();
        let idle = can_set_platform(st) && !running;
        match self.dialog {
            Some(Dialog::ProfilePick(target)) => {
                let page = self.picker_page.list.clone();
                let (current, none) = match target {
                    PickFor::Interface(i) => {
                        *heading = "Choose Profile".into();
                        let draft = self.adapter_draft(st);
                        let configured = profiles::configured(&st.status, &draft.interfaces);
                        let s = configured.iter().find(|s| s.interface == i as i32);
                        // An enabled interface always has a profile, so None is offered only
                        // while it is off.
                        (s.map_or(0, |s| s.profile), s.is_some_and(|s| !s.enabled))
                    }
                    PickFor::Layer => {
                        *heading = "Add Profile".into();
                        (0, false)
                    }
                };
                let mut choices = Vec::new();
                if none {
                    choices.push(Choice {
                        label: "None".into(),
                        action: Action::PickProfile(0),
                        chosen: current == 0,
                    });
                }
                if current != 0 && !page.iter().any(|p| p.id == current) {
                    choices.push(Choice {
                        label: self.profile_name(st, current),
                        action: Action::PickProfile(current),
                        chosen: true,
                    });
                }
                choices.extend(page.iter().map(|p| Choice {
                    label: display(&p.name),
                    action: Action::PickProfile(p.id),
                    chosen: p.id == current,
                }));
                let idle = match target {
                    PickFor::Interface(_) => idle,
                    PickFor::Layer => can_set_platform(st),
                };
                body.choice_if("", 0, choices, idle);
                self.page_controls(body, true);
                pinned.button("Close", Action::CancelDialog, Tone::Normal);
            }
            Some(Dialog::ProfileName(source)) => {
                *heading = match source {
                    Some(id) => format!("Copy {}", self.profile_name(st, id)),
                    None => "New Profile".into(),
                };
                let field = self.field_line(body.width.saturating_sub(3));
                body.control(field, Action::Input);
                if running {
                    body.para(&format!("{} Saving…", spinner()), warn());
                } else if !form_err.is_empty() {
                    body.para(&format!("✕ {form_err}"), err());
                }
                if !running {
                    let label = if source.is_some() { "Copy" } else { "Create" };
                    pinned.button(label, Action::SaveProfileName, Tone::Primary);
                }
                pinned.button("Cancel", Action::CancelDialog, Tone::Normal);
            }
            Some(Dialog::ProfileDelete(id)) => {
                *heading = "Delete Profile".into();
                body.para(&format!("Delete {}?", self.profile_name(st, id)), bold());
                pinned.button("Delete", Action::Confirm, Tone::Danger);
                pinned.button("Cancel", Action::CancelDialog, Tone::Normal);
            }
            Some(Dialog::SaveAdapter) => {
                *heading = "USB Reconnect Required".into();
                body.para(RECONNECT_TEXT, bold());
                pinned.button("Save", Action::Confirm, Tone::Primary);
                pinned.button("Cancel", Action::CancelDialog, Tone::Normal);
            }
            _ => {}
        }
    }

    /// Why Save is unavailable for the staged interface changes, where it is drawn.
    pub(super) fn refusal_hint(&self, st: &State, body: &mut Layout) {
        let draft = self.adapter_draft(st);
        if let Some(e) = profiles::interface_refusal(&st.status, &draft.interfaces) {
            body.para(&text::sentence(&text::error_words(&e)), dim());
        }
    }

    /// Each configuration interface the adapter supports: its switch and its profile, with
    /// staged values in place of saved ones.
    fn interfaces_section(&self, st: &State, body: &mut Layout, idle: bool) {
        let draft = self.adapter_draft(st);
        let configured = profiles::configured(&st.status, &draft.interfaces);
        let supported = profiles::interfaces(&st.status);
        if supported.is_empty() {
            return;
        }
        body.line(styled("Configuration Interfaces", title()));
        let changed = |b: &mut Layout| {
            b.hang(
                Line::from(pad_str("", INTERFACE_KEY)),
                "✎ Changed",
                accent(),
            );
        };
        for saved in supported {
            let i = saved.interface();
            let Some(s) = configured.iter().find(|s| s.interface == saved.interface) else {
                continue;
            };
            let staged = draft.interfaces.iter().find(|u| u.interface == i);
            body.row();
            let options = super::layout::on_off(
                Some(s.enabled),
                Action::InterfaceEnabled(i, true),
                Action::InterfaceEnabled(i, false),
            );
            body.choice_if(profiles::interface_label(i), INTERFACE_KEY, options, idle);
            if staged.is_some_and(|u| u.enabled.is_some()) {
                changed(body);
            }
            let profile = match s.profile {
                0 => "None".to_owned(),
                id => self.profile_name(st, id),
            };
            body.line(styled(pad_str("", INTERFACE_KEY), dim()));
            body.label("Profile", dim());
            body.label(&profile, bold());
            if idle {
                body.button("Choose Profile", Action::InterfaceChoose(i), Tone::Normal);
            }
            if staged.is_some_and(|u| u.profile.is_some()) {
                changed(body);
            }
        }
    }

    /// A page's progress, and Previous and Next.
    fn page_controls(&self, body: &mut Layout, picker: bool) {
        let page = self.page(picker);
        let loading = page.reading.is_some();
        if loading {
            body.para(&format!("{} Loading…", spinner()), warn());
        } else if !page.load_err.is_empty() {
            body.para(&format!("✕ {}", page.load_err), err());
            body.button("Retry", Action::ProfilesRetry, Tone::Normal);
        }
        if page.back.is_empty() && page.next == 0 {
            return;
        }
        body.row();
        button_if(
            body,
            "‹ Previous",
            Action::ProfilePage(false),
            Tone::Normal,
            !page.back.is_empty() && !loading,
        );
        button_if(
            body,
            "Next ›",
            Action::ProfilePage(true),
            Tone::Normal,
            page.next != 0 && !loading,
        );
    }

    /// Handles a profile action; false when `action` isn't one.
    pub(super) fn profiles_action(&mut self, action: &Action) -> bool {
        let st = self.state();
        let available = st
            .as_ref()
            .is_some_and(|st| profiles::available(&st.status));
        let idle = st.as_ref().is_some_and(can_set_platform) && !self.profiles_running();
        match action {
            Action::Profiles => {
                if !available {
                    return true;
                }
                self.select(Item::Adapter);
                self.profiles_open = Some(Item::Adapter);
                self.profile_err.clear();
                self.form_focused = false;
                self.focus = None;
                self.open_page(false);
            }
            Action::DeviceProfiles => {
                let layered = st.as_ref().is_some_and(
                    |st| matches!(Self::find(st, self.selected), (Some(d), _) if layered(st, d)),
                );
                if layered {
                    self.profiles_open = self.selected;
                    self.detail_scroll = 0;
                    self.focus = None;
                }
            }
            Action::ProfilesBack => self.close_profiles(),
            Action::InterfaceChoose(i) => {
                if !available || !idle {
                    return true;
                }
                self.dialog = Some(Dialog::ProfilePick(PickFor::Interface(*i)));
                self.dialog_scroll = 0;
                self.open_page(true);
            }
            Action::LayerAdd => {
                let room = st.as_ref().is_some_and(|st| {
                    let (d, _) = Self::find(st, self.selected);
                    let max = profiles::max_layers(&st.status).unwrap_or(0) as usize;
                    d.is_some_and(|d| {
                        self.device_values(st, d).layers.unwrap_or_default().len() < max
                    })
                });
                if !room {
                    return true;
                }
                self.dialog = Some(Dialog::ProfilePick(PickFor::Layer));
                self.dialog_scroll = 0;
                self.open_page(true);
            }
            Action::ProfilesRetry => {
                let picker = matches!(self.dialog, Some(Dialog::ProfilePick(_)));
                let page = self.page(picker);
                if page.reading.is_none() {
                    let after = page.after;
                    self.read_page(picker, Step::Again, after);
                }
            }
            Action::ProfilePage(forward) => {
                // The chooser pages while it is open; otherwise the adapter's Profiles view.
                let picker = matches!(self.dialog, Some(Dialog::ProfilePick(_)));
                let page = self.page(picker);
                if page.reading.is_some() {
                    return true;
                }
                let read = if *forward {
                    (page.next != 0).then_some((Step::Forward, page.next))
                } else {
                    page.back.last().map(|after| (Step::Back, *after))
                };
                let Some((step, after)) = read else {
                    return true;
                };
                if picker {
                    self.dialog_scroll = 0;
                } else {
                    self.detail_scroll = 0;
                }
                self.read_page(picker, step, after);
            }
            Action::ProfileNew | Action::ProfileCopy(_) => {
                let Some(st) = st.filter(|_| idle) else {
                    return true;
                };
                let (source, name) = match action {
                    Action::ProfileCopy(id) => {
                        let name = profiles::name_of(&st, *id);
                        (Some(*id), profiles::copy_name(&name))
                    }
                    _ => (None, String::new()),
                };
                self.dialog = Some(Dialog::ProfileName(source));
                self.form.limit = profiles::NAME_BYTES;
                self.form.set_value(&name);
                self.form_focused = true;
                self.form_err.clear();
                self.profile_err.clear();
            }
            Action::ProfileDelete(id) => {
                let Some(st) = st.filter(|_| idle) else {
                    return true;
                };
                match profiles::in_use(&st, *id) {
                    // The reason starts with a name, which keeps its own case.
                    Some(reason) => self.profile_err = format!("{reason}."),
                    None => {
                        self.profile_err.clear();
                        self.dialog = Some(Dialog::ProfileDelete(*id));
                    }
                }
            }
            Action::SaveProfileName => {
                let Some(Dialog::ProfileName(source)) = self.dialog.clone() else {
                    return true;
                };
                if !idle {
                    return true;
                }
                let name = self.form.value();
                if !profiles::valid_name(&name) {
                    self.form_err = "Enter a profile name of up to 64 bytes.".into();
                    return true;
                }
                self.form_err.clear();
                self.form_focused = false;
                self.execute(match source {
                    Some(id) => Command::ProfileCopy(Target::Id(id), name),
                    None => Command::ProfileCreate(name),
                });
            }
            _ => return false,
        }
        true
    }

    /// Answers a profile confirmation; false when the dialog isn't one.
    pub(super) fn profiles_confirm(&mut self, dialog: &Dialog) -> bool {
        match dialog {
            Dialog::ProfileDelete(id) => self.execute(Command::ProfileDelete(Target::Id(*id))),
            _ => return false,
        }
        true
    }
}

/// Whether a device has layers to show: the adapter supports profiles and reports the device's
/// layer list.
pub(super) fn layered(st: &State, d: &p::Device) -> bool {
    profiles::available(&st.status) && d.profiles.is_some()
}

/// The interfaces a configuration enables that conflict with `i`.
pub(super) fn conflicting(
    configured: &[p::ConfigurationInterfaceSupport],
    i: ConfigurationInterface,
) -> Vec<ConfigurationInterface> {
    let Some(this) = configured.iter().find(|s| s.interface == i as i32) else {
        return Vec::new();
    };
    // Either side listing the other is a conflict.
    configured
        .iter()
        .filter(|s| s.enabled && s.interface != i as i32)
        .filter(|s| this.conflicts.contains(&s.interface) || s.conflicts.contains(&(i as i32)))
        .filter_map(|s| ConfigurationInterface::try_from(s.interface).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_is_shown_in_whole_percent() {
        let mut status = p::Status::default();
        assert_eq!(memory_percent(&status), None);
        status.profile_support = Some(p::ProfileSupport {
            memory_budget: 8192,
            memory_used: 1024,
            ..Default::default()
        });
        assert_eq!(memory_percent(&status), Some(13));
        status.profile_support.as_mut().unwrap().memory_used = 8192;
        assert_eq!(memory_percent(&status), Some(100));
        status.profile_support.as_mut().unwrap().memory_budget = 0;
        assert_eq!(memory_percent(&status), None);
    }
}
