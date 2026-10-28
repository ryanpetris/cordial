//! An adapter's page: Details, Settings, Profiles, and Diagnostics. Settings and Profiles stage
//! their changes until Save sends them together in one request; renaming, connecting, and
//! disconnecting act at once.
use super::Kind;
use super::{
    Action, Model, Spot, Tab,
    fleet::Fleet,
    layout::{self, Choice, Layout, Tone, bold, dim, err, info_style, styled, warn},
    render::{DialogView, PageView, busy, button_if, pill, staged_label},
    words,
    world::{AdapterView, Conn, Storage},
};
use crate::{commands, model, ui::text};
use cordial_protocol::{ErrorCode, ResourceType};
use ratatui::text::Line;

use crate::ui::app::adapter::*;

impl<F: Fleet> Model<F> {
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
    pub(super) fn bootloader_dialog(&mut self, w: usize) -> DialogView {
        let mut body = Layout::new(w);
        body.para(
            "Restart the adapter into USB programming mode?",
            layout::bold(),
        );
        body.row();
        body.para(
            "Keyboard and mouse input stops until the adapter restarts. Saved bonds are kept and no firmware is installed.",
            layout::plain(),
        );
        let mut buttons = Layout::new(w);
        let mut right = Layout::new(w);
        right.button("Cancel", Action::Cancel, Tone::Normal);
        right.button("Enter Bootloader", Action::Confirm, Tone::Danger);
        buttons.align_right(right);
        DialogView {
            title: "Enter Bootloader".into(),
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
                self.adapter_diagnostics(&a, &mut v.body);
                self.adapter_bar(&a, &mut v.bar);
            }
        }
        v
    }
    /// The bar on Details and Diagnostics: Rename, and Connect or Disconnect. During a migration,
    /// Migrate and Cancel replace Rename.
    fn adapter_bar(&self, a: &AdapterView, bar: &mut Layout) {
        if !a.connected()
            && let Some(e) = &a.connect_error
        {
            bar.para(e, err());
        }
        if let Storage::Failed { text, .. } | Storage::Blocked(text) | Storage::Unsupported(text) =
            &a.storage
        {
            bar.para(text, err());
        }
        if a.conn == Conn::Connecting && !matches!(a.storage, Storage::Failed { .. }) {
            bar.line(busy(""));
        }
        let mut right = Layout::new(bar.width);
        let development = a.status.as_ref().is_some_and(model::development)
            && a.connected()
            && a.storage == Storage::Normal;
        if self.tab == Tab::Diagnostics && restartable(a) {
            button_if(
                &mut right,
                "Restart",
                Action::Restart,
                Tone::Normal,
                !self.running(&a.id, |k| matches!(k, Kind::Restart)),
            );
        }
        if development && self.tab == Tab::Diagnostics {
            right.button("Enter Bootloader…", Action::Bootloader, Tone::Danger);
        }
        if matches!(a.storage, Storage::Required | Storage::Failed { .. }) {
            right.button("Migrate", Action::Migrate(a.id.clone()), Tone::Primary);
        }
        match &a.storage {
            Storage::Migrating { cancelling, .. } => button_if(
                &mut right,
                "Cancel",
                Action::CancelMigration,
                Tone::Normal,
                !cancelling,
            ),
            Storage::Failed { .. } => right.button("Cancel", Action::CancelMigration, Tone::Normal),
            _ => button_if(
                &mut right,
                "Rename",
                Action::RenameAdapter(a.id.clone()),
                Tone::Normal,
                a.live().is_some(),
            ),
        }
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
        if let Storage::Migrating {
            progress: Some((done, total)),
            ..
        } = a.storage
        {
            b.section("Migration");
            let percent = done.saturating_mul(100) / total.max(1);
            b.fact(
                "Progress",
                &format!("{}%", percent.min(100)),
                layout::plain(),
            );
        }
    }
    /// Diagnostics: the adapter's errors, why its migration failed, the saved devices it couldn't
    /// read, and its ID. A refused adapter has no confirmed ID, only the serial number it reported.
    fn adapter_diagnostics(&self, a: &AdapterView, b: &mut Layout) {
        if let Storage::Failed { reason, .. } = &a.storage {
            b.section("Migration");
            b.para(&text::display(reason), layout::plain());
        }
        if let Some(st) = a
            .status
            .as_ref()
            .filter(|s| a.connected() && !s.errors.is_empty())
        {
            b.section("Errors");
            for e in &st.errors {
                b.para(&words::error_text(e, words::Scope::Save), layout::plain());
            }
        }
        if !a.unreadable.is_empty() {
            b.section("Saved Devices");
            for (id, e) in &a.unreadable {
                b.line(Line::from(vec![
                    layout::span(format!("Device {id}"), dim()),
                    layout::span(format!("  {}", words::couldnt_read(Some(e))), dim()),
                ]));
            }
            let mut row = Layout::new(b.width);
            button_if(
                &mut row,
                "Retry",
                Action::DevicesRetry,
                Tone::Normal,
                a.connected() && !self.running(&a.id, |k| matches!(k, Kind::Quiet)),
            );
            b.align_right(row);
        }
        if let Some(hello) = &a.hello {
            if let Some(serial) = words::reported(hello.hello.serial_number.as_deref()) {
                b.section("Identifiers");
                b.fact("Serial Number", &serial, layout::plain());
            }
            return;
        }
        b.section("Identifiers");
        b.fact("Adapter ID", &a.id, layout::plain());
        if let Some(format) = a.live().and_then(text::storage_format) {
            b.fact("Storage Format", &format.to_string(), layout::plain());
        }
    }
    fn adapter_details(&self, a: &AdapterView, b: &mut Layout) {
        let Some(live) = a.live() else {
            Self::empty_state(a, b);
            let firmware = a
                .hello
                .as_ref()
                .and_then(|h| words::reported(h.hello.firmware_version.as_deref()));
            if let Some(firmware) = firmware {
                b.section("Information");
                b.fact("Firmware", &firmware, layout::plain());
            }
            return;
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
                let second = styled(format!("  {}", d.status()), dim());
                b.control2(first, second, Action::Show(d.page()));
            }
        }
        let limited: Vec<_> = model::enabled_transports(live)
            .into_iter()
            .filter_map(|t| Some((t, text::capacity_text(model::enabled_devices(live, t)?)?)))
            .collect();
        if !limited.is_empty() {
            b.section("Active Devices");
            for (t, used) in limited {
                b.fact(words::transport_name(t), &used, layout::plain());
            }
        }
        let interfaces = model::resource(&live.resources, ResourceType::Interfaces)
            .and_then(text::capacity_text);
        let usb = text::usb_capacity(live);
        if interfaces.is_some() || !usb.is_empty() {
            b.section("USB");
            if let Some(count) = interfaces {
                b.fact("Interfaces", &count, layout::plain());
            }
            for (code, label) in usb {
                let look = if code == ErrorCode::UsbRequiredCapacityExceeded {
                    warn()
                } else {
                    dim()
                };
                b.fact("", label, look);
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
        let facts: Vec<_> = [
            ("Firmware", crate::ui::app::App::<F>::firmware(live)),
            ("Board", crate::ui::app::App::<F>::board(live)),
            ("Storage", text::storage_text(live)),
        ]
        .into_iter()
        .filter_map(|(label, value)| Some((label, value?)))
        .collect();
        if !facts.is_empty() {
            b.section("Information");
            for (label, value) in facts {
                b.fact(label, &value, layout::plain());
            }
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
}
