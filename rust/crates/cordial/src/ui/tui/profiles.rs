//! Profiles: an adapter's profile memory, its configuration interfaces, each with a switch and a
//! profile staged until Save, and the listed profiles a page at a time, each labelled with its
//! roles, with creating, copying and deleting them at once. The picker of a profile for an
//! interface or a device's layers lists and pages on its own. Profile rules are never read or
//! changed here.
use super::{
    Action, Model, PickFor, Spot,
    fleet::Fleet,
    layout::{self, Layout, Tone, bold, dim, err, styled},
    render::{DialogView, button_if, staged_label},
    words,
    world::AdapterView,
};
use crate::profiles;
use cordial_protocol::ConfigurationInterface;
use ratatui::{style::Style, text::Line};

impl<F: Fleet> Model<F> {
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
