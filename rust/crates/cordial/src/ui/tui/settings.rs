//! A device's Settings tab: its gamepad output, its settings grouped by category, each with the
//! control its type calls for and a marker for its saved state, and the readings that belong beside
//! them. Every control stages a change; Save sends every staged setting of the device in one
//! request, then a staged gamepad output in another.
use super::Kind;
use super::{
    Action, Model, Spot, Tab,
    fleet::Fleet,
    layout::{self, Choice, Layout, Tone, accent, dim, err, ok, span, styled, warn},
    render::{PageView, button_if},
    words,
    world::DeviceView,
};
use crate::ui::app;
use crate::{
    model,
    settings::{self, Setting, Type, Value},
    ui::{catalog, text},
};
use cordial_protocol::SettingState;
use ratatui::{style::Style, text::Line};

use crate::ui::app::settings::*;

impl<F: Fleet> Model<F> {
    /// Steps the highlighted setting's value, or moves its choice, by `delta`; false when the
    /// highlight isn't on a setting.
    pub(super) fn adjust_focused(&mut self, delta: i64) -> bool {
        let Some(focus) = self.focus.clone() else {
            return false;
        };
        let super::Page::Device(adapter, id) = self.shown() else {
            return false;
        };
        let key = match &focus {
            Action::SettingStep(k, _) | Action::SettingEdit(k) => *k,
            Action::SettingChoice(k, _) | Action::SettingSelect(k) | Action::SettingBool(k, _) => {
                *k
            }
            _ => return false,
        };
        let Some(d) = self.device(&adapter, id).cloned() else {
            return false;
        };
        let Some(s) = self.setting(&d, &key) else {
            return false;
        };
        if self.form_busy(&d) {
            return true;
        }
        let choices = settings::choices(&s);
        let shown = self.shown_value(&d, &s);
        if settings::kind(&s) == Type::Bool {
            self.edit(&d, &s, Value::Bool(delta > 0));
        } else if !choices.is_empty() {
            let i = shown
                .as_ref()
                .and_then(|v| choices.iter().position(|c| c == v));
            let n = choices.len() as i64;
            let next = match i {
                Some(i) => (i as i64 + delta).clamp(0, n - 1),
                None if delta > 0 => 0,
                None => n - 1,
            };
            self.edit(&d, &s, choices[next as usize]);
            self.focus = match &focus {
                Action::SettingChoice(..) => {
                    Some(Action::SettingChoice(key, choices[next as usize]))
                }
                other => Some(other.clone()),
            };
        } else if settings::kind(&s) == Type::Integer {
            let v = step_value(bounds(&s), shown.as_ref(), delta);
            self.edit(&d, &s, v);
        }
        true
    }
    /// The control of one setting.
    fn control(&self, d: &DeviceView, s: &Setting, busy: bool, w: usize) -> Layout {
        let mut c = Layout::new(w);
        let key = s.field;
        let leaf = key.leaf;
        let fresh = crate::ui::app::App::<F>::settings_current(d) && s.value.is_some();
        let stale = if fresh { Style::new() } else { dim() };
        let shown = self.shown_value(d, s);
        let choices = settings::choices(s);
        let kind = settings::kind(s);
        if kind == Type::Choice && choices.is_empty() {
            c.label(&unit_text(leaf, s.value.as_ref()), stale);
            return c;
        }
        if kind == Type::Bool {
            let on = match &shown {
                Some(Value::Bool(b)) => Some(*b),
                _ => None,
            };
            let next = on.is_none_or(|on| !on);
            return layout::switch(on, Action::SettingBool(key, next), !busy);
        }
        if is_free_spin(s) {
            let on = match &shown {
                Some(Value::Integer(n)) => Some(*n != FREE_SPIN_OFF),
                _ => None,
            };
            let next = on.is_none_or(|on| !on);
            return layout::switch(on, Action::FreeSpin(key, next), !busy);
        }
        if !choices.is_empty() {
            let labels: Vec<String> = choices.iter().map(|v| unit_text(leaf, Some(v))).collect();
            let short = choices.len() <= 3
                && labels.iter().map(|l| l.chars().count()).sum::<usize>() <= 24
                && shown.as_ref().is_some_and(|v| choices.contains(v));
            if short {
                for (v, label) in choices.iter().zip(labels) {
                    let chosen = shown.as_ref() == Some(v);
                    let text = if chosen {
                        format!("[● {label}]")
                    } else {
                        format!("[○ {label}]")
                    };
                    let look = match () {
                        _ if busy => dim(),
                        _ if chosen => Tone::Chosen.style(),
                        _ => Tone::Normal.style(),
                    };
                    let x = layout::line_width(c.lines.last().unwrap_or(&Line::default()));
                    let x = if x > 0 { x + 1 } else { 0 };
                    if c.lines.is_empty() {
                        c.row();
                    }
                    let y = c.lines.len() - 1;
                    if x > 0 {
                        c.lines[y].spans.push(span(" ", Style::new()));
                    }
                    let tw = text::width(&text);
                    c.lines[y].spans.push(span(text, look));
                    if !busy {
                        c.hits.push(layout::Hit {
                            x,
                            y,
                            w: tw,
                            action: Action::SettingChoice(key, *v),
                        });
                    }
                }
                c.width = layout::line_width(&c.lines[0]);
                return c;
            }
            let text = format!("{} ▾", unit_text(leaf, shown.as_ref()));
            if busy {
                c.disabled(&text);
            } else {
                c.button(&text, Action::SettingSelect(key), Tone::Normal);
            }
            return c;
        }
        self.stepper(&mut c, d, s, busy);
        c
    }
    /// An integer's stepper: down, the value (typed when clicked), up, and the unit.
    fn stepper(&self, c: &mut Layout, d: &DeviceView, s: &Setting, busy: bool) {
        let key = s.field;
        let shown = self.shown_value(d, s);
        button_if(c, "-", Action::SettingStep(key, -1), Tone::Normal, !busy);
        if self.editing == Some(key) {
            let mut form = self.form.clone();
            let line = super::keys::edit_line(&mut form, true, 8);
            let y = c.lines.len() - 1;
            c.lines[y].spans.push(span(" ", Style::new()));
            c.lines[y].spans.extend(line.spans);
        } else {
            let text = match self.draft_of(d, &key) {
                Some(Draft::Typed(t)) => t.clone(),
                _ => value_text(key.leaf, shown.as_ref()),
            };
            button_if(c, &text, Action::SettingEdit(key), Tone::Normal, !busy);
        }
        button_if(c, "+", Action::SettingStep(key, 1), Tone::Normal, !busy);
        let unit = display_unit(key.leaf, shown.as_ref());
        if !unit.is_empty() {
            c.label(unit, dim());
        }
        c.width = c.lines.iter().map(layout::line_width).max().unwrap_or(0);
    }
    /// The Settings tab.
    pub(super) fn device_settings(&mut self, d: &DeviceView, v: &mut PageView) {
        let settings = self.settings_of(d);
        let readings = readings(&d.d);
        let figures = words::wheel_figures(d.d.information.as_ref());
        let busy = self.form_busy(d);
        let key = (d.adapter.clone(), d.d.id);
        let submission = self.submissions.get(&key).cloned();
        let mut categories: Vec<&'static str> = Vec::new();
        for leaf in settings
            .iter()
            .map(|s| s.field.leaf)
            .chain(readings.iter().map(|(f, _)| f.leaf))
        {
            let c = catalog::category(leaf);
            if !categories.contains(&c) {
                categories.push(c);
            }
        }
        if !figures.is_empty() && !categories.contains(&"Wheel") {
            categories.push("Wheel");
        }
        let w = v.body.width;
        let gamepad = self.gamepad_choices(d);
        let gamepad_change = self.device_changes(d).gamepad;
        let gamepad_saving = self.device_saving(d, Tab::Settings);
        let b = &mut v.body;
        if !gamepad.is_empty() {
            let shown = gamepad_change.or_else(|| model::gamepad_mode(&d.d));
            let options = gamepad
                .into_iter()
                .map(|mode| Choice {
                    label: text::output_mode_name(mode).into(),
                    action: Action::GamepadOutput(mode),
                    chosen: Some(mode) == shown,
                })
                .collect();
            b.section("Gamepad");
            let mut group = Layout::new(w.saturating_sub(4));
            group.choice_if("", 0, options, !busy);
            let mark = match () {
                _ if gamepad_saving => Some(format!("{} Sending", super::spinner())),
                _ if gamepad_change.is_some() => Some("✎ Changed".to_owned()),
                _ => None,
            };
            if let Some(mark) = mark {
                let mut m = Layout::new(group.width);
                m.lines.push(Line::from(span(format!(" {mark}"), accent())));
                group.append(m);
            }
            b.labelled(Line::from(span("Gamepad Output", Style::new())), group);
        }
        for category in categories {
            b.section(category);
            for s in settings
                .iter()
                .filter(|s| catalog::category(s.field.leaf) == category)
            {
                let label = if is_free_spin(s) {
                    "Automatic Free Spin".to_owned()
                } else {
                    catalog::label(&s.field)
                };
                let label = Line::from(span(label, Style::new()));
                let mut group = self.control(d, s, busy, w.saturating_sub(4));
                if group.lines.is_empty() {
                    group.row();
                }
                let marker = self.marker(d, s, busy);
                let saving = submission
                    .as_ref()
                    .is_some_and(|sub| sub.running && sub.fields.contains(&s.field));
                let (mark_text, mark_look) = if saving {
                    (format!("{} Sending", super::spinner()), accent())
                } else {
                    (
                        marker.text.to_owned(),
                        match marker.tone {
                            app::Tone::Accent => accent(),
                            app::Tone::Dim => dim(),
                            app::Tone::Warning => warn(),
                            app::Tone::Success => ok(),
                        },
                    )
                };
                group.width = w.saturating_sub(4);
                let mut mark = Layout::new(group.width);
                mark.lines
                    .push(Line::from(span(format!(" {mark_text}"), mark_look)));
                if !saving {
                    mark.hits.push(layout::Hit {
                        x: 1,
                        y: 0,
                        w: text::width(&mark_text),
                        action: Action::Marker(s.field),
                    });
                }
                group.append(mark);
                b.labelled(label, group);
                if is_free_spin(s)
                    && matches!(self.shown_value(d, s), Some(Value::Integer(n)) if n != FREE_SPIN_OFF)
                {
                    let mut c = Layout::new(w);
                    self.stepper(&mut c, d, s, busy);
                    let label = format!("  {}", catalog::label(&s.field));
                    b.labelled(styled(label, Style::new()), c);
                }
                let mut notes: Vec<(String, Style)> = Vec::new();
                if let Some(sub) = &submission
                    && !sub.running
                    && sub.fields.contains(&s.field)
                    && let Some(e) = &sub.error
                {
                    let text = if sub.unknown {
                        e.clone()
                    } else {
                        app::device::save_failed("this setting", e)
                    };
                    notes.push((text, err()));
                }
                if change_of(s, self.draft_of(d, &s.field)).is_err() {
                    notes.push((range_text(s), err()));
                }
                let fresh = crate::ui::app::App::<F>::settings_current(d) && s.value.is_some();
                if s.saved.is_some() && s.status == Some(Ok(SettingState::ChangedOnDevice)) && fresh
                {
                    notes.push((
                        format!("Device: {}", unit_text(s.field.leaf, s.value.as_ref())),
                        dim(),
                    ));
                }
                if let Some(Err(e)) = &s.status
                    && !(model::unsupported(e) && words::plain(e))
                {
                    notes.push((
                        words::reason(&words::error_text(e, words::Scope::Apply)).to_owned(),
                        dim(),
                    ));
                }
                if !notes.is_empty() {
                    let mut line = Line::from(span("  ", Style::new()));
                    for (i, (text, look)) in notes.into_iter().enumerate() {
                        if i > 0 {
                            line.spans.push(span(" · ", dim()));
                        }
                        line.spans.push(span(text, look));
                    }
                    b.line(line);
                }
            }
            let look = if d.connected() { Style::new() } else { dim() };
            for (field, value) in readings
                .iter()
                .filter(|(f, _)| catalog::category(f.leaf) == category)
            {
                let text = unit_text(field.leaf, Some(value));
                b.labelled(styled(catalog::label(field), Style::new()), {
                    let mut r = Layout::new(w);
                    r.label(&text, look);
                    r.width = text::width(&text);
                    r
                });
            }
            if category == "Wheel" {
                for (label, value) in &figures {
                    b.fact(label, value, look);
                }
            }
        }

        // The bar.
        let bar = &mut v.bar;
        let saving = submission.as_ref().is_some_and(|s| s.running) || gamepad_saving;
        let starting = d.connected() && model::hidpp_up(&d.d) == model::Up::Starting;
        if saving || self.refreshing(d) || starting {
            bar.label(&format!("{}", super::spinner()), accent());
        }
        let note = self
            .notes
            .get(&Spot::SettingsNote(key.0.clone(), key.1))
            .cloned()
            .or_else(|| {
                let s = submission.as_ref().filter(|s| !s.running && !s.unknown)?;
                Some(app::device::save_failed(
                    &app::device::settings_count(s.fields.len()),
                    s.error.as_deref()?,
                ))
            })
            .or_else(|| match model::hidpp_up(&d.d) {
                model::Up::Error(e) => Some(words::error_text(&e, words::Scope::Apply)),
                _ => None,
            });
        if let Some(note) = note {
            bar.para(&note, err());
        }
        if self.settings_errors.contains_key(&key) {
            bar.para(words::SETTINGS_READ_FAILED, dim());
            let reloading = self.running(
                &d.adapter,
                |k| matches!(k, Kind::SettingsList(x) if *x == d.d.id),
            );
            button_if(
                bar,
                "Retry",
                Action::SettingsReload,
                Tone::Normal,
                !reloading,
            );
        }
        let mut right = Layout::new(bar.width);
        button_if(
            &mut right,
            "Refresh",
            Action::SettingsRefresh,
            Tone::Normal,
            self.can_refresh(d),
        );
        let failed: Vec<&Setting> = settings
            .iter()
            .filter(|s| {
                matches!(&s.status, Some(Err(e)) if !model::unsupported(e)) && s.saved.is_some()
            })
            .collect();
        if !failed.is_empty() {
            let any_free = failed.iter().any(|s| self.draft_of(d, &s.field).is_none());
            button_if(
                &mut right,
                "Retry",
                Action::SettingsReapply,
                Tone::Normal,
                !busy && any_free,
            );
        }
        let dirty = self.settings_dirty(d) || gamepad_change.is_some();
        let (changes, invalid) = self.setting_changes(d);
        button_if(
            &mut right,
            "Discard",
            Action::SettingsDiscard,
            Tone::Normal,
            !self.submitting(d) && !gamepad_saving && dirty,
        );
        button_if(
            &mut right,
            "Save",
            Action::SettingsSave,
            Tone::Primary,
            !busy
                && (!changes.is_empty() || gamepad_change.is_some())
                && !invalid
                && !(gamepad_change.is_some() && self.device_locked(d)),
        );
        bar.align_right(right);
    }
}
