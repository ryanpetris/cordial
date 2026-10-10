//! An adapter's page: Details, Settings, Profiles and Diagnostics. Settings and Profiles stage
//! their changes until Save sends them together in one request; renaming, connecting and
//! disconnecting act at once.
use super::{
    Action, Dialog, Job, Kind, Model, Spot, Tab,
    fleet::Fleet,
    layout::{self, Choice, Layout, Tone, bold, dim, err, info_style, styled, warn},
    render::{DialogView, PageView, busy, button_if, pill, staged_label},
    words,
    world::{AdapterView, Conn},
};
use crate::{
    commands,
    controller::{AdapterUpdate, Command, Outcome},
    error::Error,
    model,
    profiles::{self, InterfaceUpdate},
};
use cordial_protocol::{self as p, ConfigurationInterface, Platform};
use ratatui::text::Line;

pub const PLATFORMS: [Platform; 3] = [Platform::Linux, Platform::Windows, Platform::Mac];

/// The platform of the computer the TUI runs on.
pub fn host_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Mac
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

/// The staged values in `draft` that differ from what the adapter reports.
pub fn adapter_pending(draft: &AdapterUpdate, status: &p::Status) -> AdapterUpdate {
    let interfaces = if profiles::available(status) {
        draft
            .interfaces
            .iter()
            .filter_map(|u| {
                let saved = profiles::interface(status, u.interface)?;
                let update = InterfaceUpdate {
                    interface: u.interface,
                    enabled: u.enabled.filter(|on| *on != saved.enabled),
                    profile: u.profile.filter(|id| *id != saved.profile),
                };
                (update.enabled.is_some() || update.profile.is_some()).then_some(update)
            })
            .collect()
    } else {
        Vec::new()
    };
    AdapterUpdate {
        platform: draft.platform.filter(|p| *p != status.platform()),
        transports: draft
            .transports
            .iter()
            .copied()
            .filter(|(t, on)| model::transport_enabled(status, *t).is_some_and(|e| e != *on))
            .collect(),
        interfaces,
    }
}

impl<F: Fleet> Model<F> {
    /// The adapter's staged changes that differ from its status.
    pub(super) fn adapter_changes(&self, a: &AdapterView) -> AdapterUpdate {
        match (self.adapter_drafts.get(&a.id), a.view()) {
            (Some(draft), Some(status)) => adapter_pending(draft, status),
            _ => AdapterUpdate::default(),
        }
    }

    pub(super) fn adapter_saving(&self, id: &str) -> bool {
        self.running(id, |k| matches!(k, Kind::AdapterSave(_)))
    }

    /// Staged values lock while the adapter isn't connected and ready, or while saving.
    pub(super) fn adapter_locked(&self, a: &AdapterView) -> bool {
        a.live().is_none() || self.adapter_saving(&a.id)
    }

    /// Stages a change to the adapter's settings and clears the last save's failure.
    pub(super) fn stage_adapter(&mut self, id: &str, change: impl FnOnce(&mut AdapterUpdate)) {
        let Some(status) = self.adapter(id).and_then(|a| a.view().cloned()) else {
            return;
        };
        let mut draft = self.adapter_drafts.remove(id).unwrap_or_default();
        change(&mut draft);
        let draft = adapter_pending(&draft, &status);
        if !draft.is_empty() {
            self.adapter_drafts.insert(id.to_owned(), draft);
        }
        self.notes.remove(&Spot::AdapterSave(id.to_owned()));
    }

    pub(super) fn stage_interface(
        &mut self,
        id: &str,
        i: ConfigurationInterface,
        enabled: Option<bool>,
        profile: Option<u32>,
    ) {
        self.stage_adapter(id, |d| {
            match d.interfaces.iter_mut().find(|u| u.interface == i) {
                Some(u) => {
                    u.enabled = enabled.or(u.enabled);
                    u.profile = profile.or(u.profile);
                }
                None => d.interfaces.push(InterfaceUpdate {
                    interface: i,
                    enabled,
                    profile,
                }),
            }
        });
    }

    /// Saves the adapter's staged changes, asking first when the save reconnects USB.
    pub(super) fn save_adapter(&mut self, id: &str, confirmed: bool) {
        let Some(a) = self.adapter(id).cloned() else {
            return;
        };
        let changes = self.adapter_changes(&a);
        if changes.is_empty() || self.adapter_locked(&a) {
            return;
        }
        let Some(status) = a.live() else { return };
        if let Some(e) = profiles::interface_refusal(status, &changes.interfaces) {
            self.notes
                .insert(Spot::AdapterSave(id.to_owned()), words::failure(&e));
            return;
        }
        if !confirmed && profiles::reconnects(status, &changes.interfaces) {
            self.dialog = Some(Dialog::SaveAdapter(id.to_owned()));
            return;
        }
        self.notes.remove(&Spot::AdapterSave(id.to_owned()));
        self.run(
            id,
            Kind::AdapterSave(changes.clone()),
            Command::AdapterSave(changes),
        );
    }

    pub(super) fn adapter_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let id = job.adapter.clone();
        match (&job.kind, result) {
            (Kind::AdapterSave(sent), Ok(_)) => {
                if let Some(draft) = self.adapter_drafts.get_mut(&id) {
                    if draft.platform == sent.platform {
                        draft.platform = None;
                    }
                    draft.transports.retain(|t| !sent.transports.contains(t));
                    draft.interfaces.retain(|u| !sent.interfaces.contains(u));
                    if draft.is_empty() {
                        self.adapter_drafts.remove(&id);
                    }
                }
            }
            (Kind::AdapterSave(_), Err(e)) => {
                self.notes.insert(Spot::AdapterSave(id), words::failure(e));
            }
            (Kind::Rename, Ok(_)) => {
                if matches!(&self.dialog, Some(Dialog::Rename(a)) if *a == id) {
                    self.dialog = None;
                    self.form_focused = false;
                }
            }
            (Kind::Rename, Err(e)) => {
                if matches!(&self.dialog, Some(Dialog::Rename(a)) if *a == id) {
                    self.notes.insert(Spot::Dialog, words::failure(e));
                }
            }
            _ => {}
        }
    }

    /// Handles an adapter page action; false when `action` isn't one.
    pub(super) fn adapter_action(&mut self, id: &str, action: &Action) -> bool {
        let Some(a) = self.adapter(id).cloned() else {
            return false;
        };
        let locked = self.adapter_locked(&a);
        match action {
            Action::Platform(p) if !locked => {
                let p = *p;
                self.stage_adapter(id, |d| d.platform = Some(p));
            }
            Action::SwitchPlatform if !locked => {
                let p = host_platform();
                self.stage_adapter(id, |d| d.platform = Some(p));
            }
            Action::Transport(t, on) if !locked => {
                let (t, on) = (*t, *on);
                self.stage_adapter(id, |d| {
                    d.transports.retain(|(x, _)| *x != t);
                    d.transports.push((t, on));
                });
            }
            Action::InterfaceEnabled(i, on) if !locked => {
                self.stage_interface(id, *i, Some(*on), None);
            }
            Action::InterfacePick(i) if !locked => {
                let staged = self.staged_interface(&a, *i);
                self.dialog = Some(Dialog::Pick {
                    adapter: id.to_owned(),
                    purpose: super::PickFor::Interface(*i),
                    chosen: staged.map_or(0, |s| s.profile),
                });
                self.dialog_scroll = 0;
                self.notes.remove(&Spot::Dialog);
                self.open_picker(id);
            }
            Action::AdapterSave => self.save_adapter(id, false),
            Action::AdapterDiscard if !self.adapter_saving(id) => {
                self.adapter_drafts.remove(id);
                self.notes.remove(&Spot::AdapterSave(id.to_owned()));
            }
            Action::Files if a.status.as_ref().is_some_and(model::development) => {
                self.open_files(id)
            }
            Action::Bootloader if a.status.as_ref().is_some_and(model::development) => {
                self.dialog = Some(Dialog::Bootloader(id.to_owned()));
            }
            _ => return self.profiles_action(id, action),
        }
        true
    }

    pub(super) fn open_rename(&mut self, id: &str) {
        let Some(a) = self.adapter(id) else { return };
        let name = a.live().map(|s| words::clean(&s.name)).unwrap_or_default();
        self.form.limit = 64;
        self.form.set_value(&name);
        self.form_focused = true;
        self.notes.remove(&Spot::Dialog);
        self.dialog = Some(Dialog::Rename(id.to_owned()));
    }

    pub(super) fn renaming(&self, id: &str) -> bool {
        self.running(id, |k| matches!(k, Kind::Rename))
    }

    /// Sends the Rename dialog's name, or the default name with `reset`.
    pub(super) fn submit_rename(&mut self, id: &str, reset: bool) {
        if self.renaming(id) || self.adapter(id).and_then(AdapterView::live).is_none() {
            return;
        }
        let name = if reset {
            None
        } else {
            match commands::adapter_name(&self.form.value()) {
                Some(name) => Some(name.to_owned()),
                None => {
                    self.notes
                        .insert(Spot::Dialog, words::ADAPTER_NAME_INVALID.into());
                    return;
                }
            }
        };
        self.notes.remove(&Spot::Dialog);
        self.run(id, Kind::Rename, Command::Name(name));
    }

    pub(super) fn rename_dialog(&mut self, id: &str, w: usize) -> DialogView {
        let mut body = Layout::new(w);
        let line = self.field_line(w);
        body.control(line, Action::Field);
        if let Some(e) = self.notes.get(&Spot::Dialog) {
            body.para(e, err());
        }
        let busy_now = self.renaming(id);
        let available = self.adapter(id).and_then(AdapterView::live).is_some();
        let valid = commands::adapter_name(&self.form.value()).is_some();
        let mut buttons = Layout::new(w);
        button_if(
            &mut buttons,
            "Reset to Default",
            Action::ResetName,
            Tone::Normal,
            !busy_now && available,
        );
        let mut right = Layout::new(w);
        button_if(
            &mut right,
            "Cancel",
            Action::Cancel,
            Tone::Normal,
            !busy_now,
        );
        button_if(
            &mut right,
            "Rename",
            Action::Submit,
            Tone::Primary,
            !busy_now && available && valid,
        );
        buttons.align_right(right);
        DialogView {
            title: "Rename Adapter".into(),
            body,
            buttons,
        }
    }

    pub(super) fn reconnect_dialog(&mut self, id: &str, w: usize) -> DialogView {
        let mut body = Layout::new(w);
        body.para(words::RECONNECT_TEXT, layout::plain());
        let can_save = self
            .adapter(id)
            .cloned()
            .is_some_and(|a| !self.adapter_locked(&a) && !self.adapter_changes(&a).is_empty());
        let mut buttons = Layout::new(w);
        let mut right = Layout::new(w);
        right.button("Cancel", Action::Cancel, Tone::Normal);
        button_if(&mut right, "Save", Action::Confirm, Tone::Primary, can_save);
        buttons.align_right(right);
        DialogView {
            title: words::RECONNECT_TITLE.into(),
            body,
            buttons,
        }
    }

    /// The adapter's page.
    pub(super) fn adapter_page(&mut self, id: &str, w: usize) -> PageView {
        let Some(a) = self.adapter(id).cloned() else {
            return PageView::new(Line::default(), w);
        };
        let (status, attention) = a.status_text();
        let tone = match () {
            _ if attention => warn(),
            _ if a.connected() && a.ready => layout::ok(),
            _ => dim(),
        };
        let name_look = if a.connected() { bold() } else { dim() };
        let mut title = Line::from(layout::span(a.name.clone(), name_look));
        title
            .spans
            .push(layout::span("  ", ratatui::style::Style::new()));
        title.spans.extend(pill(status, tone).spans);
        let mut v = PageView::new(title, w);
        v.tabs = vec![Tab::Details, Tab::Settings];
        if a.profile_support() {
            v.tabs.push(Tab::Profiles);
        }
        v.tabs.push(Tab::Diagnostics);
        v.tab = if v.tabs.contains(&self.tab) {
            self.tab
        } else {
            Tab::Details
        };
        for text in &a.attention {
            v.banners.push((text.clone(), warn()));
        }
        match v.tab {
            Tab::Details => {
                self.adapter_details(&a, &mut v.body);
                self.adapter_bar(&a, &mut v.bar);
            }
            Tab::Settings => {
                self.adapter_settings(&a, &mut v.body);
                self.adapter_save_bar(&a, &mut v.bar);
            }
            Tab::Profiles => {
                self.adapter_profiles(&a, &mut v.body);
                self.adapter_save_bar(&a, &mut v.bar);
            }
            Tab::Diagnostics => {
                v.body.section("Identifiers");
                v.body.fact("Adapter ID", &a.id, layout::plain());
                self.adapter_bar(&a, &mut v.bar);
            }
        }
        v
    }

    /// The bar on Details and Diagnostics: Rename, and Connect or Disconnect.
    fn adapter_bar(&self, a: &AdapterView, bar: &mut Layout) {
        if !a.connected()
            && let Some(e) = &a.connect_error
        {
            bar.para(e, err());
        }
        if a.conn == Conn::Connecting {
            bar.line(busy(""));
        }
        let mut right = Layout::new(bar.width);
        let development = a.status.as_ref().is_some_and(model::development) && a.connected();
        if development && self.tab == Tab::Diagnostics {
            right.button("Files…", Action::Files, Tone::Normal);
            right.button("Enter Bootloader…", Action::Bootloader, Tone::Danger);
        }
        button_if(
            &mut right,
            "Rename",
            Action::RenameAdapter(a.id.clone()),
            Tone::Normal,
            a.live().is_some(),
        );
        match a.conn {
            Conn::Connected => right.button(
                "Disconnect",
                Action::DisconnectAdapter(a.id.clone()),
                Tone::Normal,
            ),
            Conn::Disconnected => right.button(
                "Connect",
                Action::ConnectAdapter(a.id.clone()),
                Tone::Primary,
            ),
            Conn::Connecting => {}
        }
        bar.align_right(right);
    }

    /// The bar on Settings and Profiles: Discard and Save for the staged changes.
    fn adapter_save_bar(&self, a: &AdapterView, bar: &mut Layout) {
        let saving = self.adapter_saving(&a.id);
        if saving {
            bar.line(busy(""));
        }
        if let Some(e) = self.notes.get(&Spot::AdapterSave(a.id.clone())) {
            bar.para(e, err());
        }
        let dirty = !self.adapter_changes(a).is_empty();
        let mut right = Layout::new(bar.width);
        button_if(
            &mut right,
            "Discard",
            Action::AdapterDiscard,
            Tone::Normal,
            !saving && dirty,
        );
        button_if(
            &mut right,
            "Save",
            Action::AdapterSave,
            Tone::Primary,
            !self.adapter_locked(a) && dirty,
        );
        bar.align_right(right);
    }

    fn empty_state(a: &AdapterView, b: &mut Layout) {
        b.row();
        b.line(styled(a.status_text().0, bold()));
    }

    fn adapter_details(&self, a: &AdapterView, b: &mut Layout) {
        let Some(live) = a.live() else {
            return Self::empty_state(a, b);
        };
        let devices: Vec<_> = self.devices.iter().filter(|d| d.adapter == a.id).collect();
        if !devices.is_empty() {
            b.section("Devices");
            for d in devices {
                let right = if let Some(p) = d.battery.as_ref().and_then(|b| b.percent) {
                    let look = match () {
                        _ if d.low() => err(),
                        _ if d.battery.as_ref().is_some_and(|b| b.stale()) => dim(),
                        _ => ratatui::style::Style::new(),
                    };
                    styled(format!("{p}% ›"), look)
                } else {
                    styled("›", dim())
                };
                let dot = if d.connected() {
                    layout::span("● ", layout::ok())
                } else {
                    layout::span("○ ", dim())
                };
                let first = layout::spread(
                    Line::from(vec![dot, layout::span(d.name.clone(), bold())]),
                    right,
                    b.width,
                );
                let second = styled(format!("  {}", words::device_status(&d.d)), dim());
                b.control2(first, second, Action::Show(d.page()));
            }
        }
        let limited: Vec<_> = live
            .transports
            .iter()
            .filter(|t| t.enabled() && t.max_enabled.is_some())
            .collect();
        if !limited.is_empty() {
            b.section("Active Devices");
            for t in limited {
                let Some(transport) = model::transport(t.transport) else {
                    continue;
                };
                let max = t.max_enabled.unwrap_or(0);
                let used = self
                    .devices
                    .iter()
                    .filter(|d| {
                        d.adapter == a.id
                            && d.d.transport == t.transport
                            && model::inactive(&d.d).is_none()
                    })
                    .count();
                b.fact(
                    words::transport_name(transport),
                    &format!("{used} of {max}"),
                    layout::plain(),
                );
            }
        }
        let enabled = model::enabled_transports(live);
        if !enabled.is_empty() {
            b.section("New Pairings");
            for t in enabled {
                let full = model::storage_full(live);
                let (value, look) = if full {
                    (words::STORAGE_FULL, warn())
                } else {
                    ("Available", layout::plain())
                };
                b.fact(words::transport_name(t), value, look);
            }
        }
        b.section("Information");
        let firmware = Self::info_text(live, cordial_protocol::keys::FIRMWARE_VERSION)
            .unwrap_or_else(|| "Unknown".into());
        b.fact("Firmware", &firmware, layout::plain());
        if let Some(board) = Self::board(live) {
            b.fact("Board", &board, layout::plain());
        }
    }

    fn adapter_settings(&self, a: &AdapterView, b: &mut Layout) {
        let Some(view) = a.view() else {
            return Self::empty_state(a, b);
        };
        let locked = self.adapter_locked(a);
        let changes = self.adapter_changes(a);
        let draft = self.adapter_drafts.get(&a.id);
        let shown = draft
            .and_then(|d| d.platform)
            .unwrap_or_else(|| view.platform());
        b.row();
        let options = PLATFORMS
            .iter()
            .map(|p| Choice {
                label: crate::ui::text::platform_name(*p).into(),
                action: Action::Platform(*p),
                chosen: *p == shown,
            })
            .collect();
        let label = staged_label("Platform", changes.platform.is_some());
        b.line(label);
        let kw = 2;
        b.choice_if("", kw, options, !locked);
        let host = host_platform();
        if shown != host {
            b.row();
            b.para(
                &format!(
                    "This computer runs {}.",
                    crate::ui::text::platform_name(host)
                ),
                info_style(),
            );
            let mut row = Layout::new(b.width);
            button_if(
                &mut row,
                &format!("Switch to {}", crate::ui::text::platform_name(host)),
                Action::SwitchPlatform,
                Tone::Normal,
                !locked,
            );
            b.add(row);
        }
        let transports = model::transports(view);
        if !transports.is_empty() {
            b.section("Bluetooth");
            for t in transports {
                let saved = model::transport_enabled(view, t).unwrap_or(true);
                let on = draft
                    .and_then(|d| d.transports.iter().find(|(x, _)| *x == t))
                    .map_or(saved, |(_, on)| *on);
                let staged = changes.transports.iter().any(|(x, _)| *x == t);
                b.labelled(
                    staged_label(words::transport_name(t), staged),
                    layout::switch(Some(on), Action::Transport(t, !on), !locked),
                );
            }
        }
    }

    /// An interface's preferences as staged.
    pub(super) fn staged_interface(
        &self,
        a: &AdapterView,
        i: ConfigurationInterface,
    ) -> Option<p::ConfigurationInterfaceSupport> {
        let view = a.view()?;
        let draft = self
            .adapter_drafts
            .get(&a.id)
            .map(|d| d.interfaces.clone())
            .unwrap_or_default();
        profiles::configured(view, &draft)
            .into_iter()
            .find(|s| s.interface == i as i32)
    }

    /// Answers the reconnect confirmation by saving; false when the
    /// dialog isn't the confirmation.
    pub(super) fn adapter_confirm(&mut self, dialog: &Dialog) -> bool {
        let Dialog::SaveAdapter(id) = dialog else {
            return false;
        };
        let id = id.clone();
        self.dialog = None;
        self.save_adapter(&id, true);
        true
    }
}
