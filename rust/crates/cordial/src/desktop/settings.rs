//! A device's settings on its page: why they couldn't be read or saved, its gamepad output, then
//! its settings grouped by category, each with a menu of its saved state before the control its
//! type calls for, beside the readings of the same category. A change applies as it is made; a
//! number applies once it is committed.
use super::{
    shell::Ctx,
    widgets::{
        Btn, Item, List, Look, Menu, Tab as TabView, set_label, set_look, set_subtitle, when,
    },
};
use crate::{
    model,
    settings::{self, Field, Setting, Type, Value},
    ui::{
        app::{
            Action, App, Kind, Live, Spot, Tone,
            settings::{
                FREE_SPIN_OFF, change_of, display_unit, is_free_spin, range_text, readings,
                value_text,
            },
            words,
            world::DeviceView,
        },
        catalog, text,
    },
};
use adw::prelude::*;
use cordial_protocol::{OutputMode, SettingState};
use gtk::glib;
use std::{cell::Cell, rc::Rc};

/// The control a setting gets, which decides its row.
#[derive(Clone, Debug, PartialEq)]
enum Control {
    ReadOnly,
    Switch,
    FreeSpin(Option<(i64, i64, i64)>),
    Choice(Vec<Value>),
    Number(Option<(i64, i64, i64)>),
}

fn control(s: &Setting) -> Control {
    let choices = settings::choices(s);
    match settings::kind(s) {
        Type::Choice if choices.is_empty() => Control::ReadOnly,
        Type::Bool => Control::Switch,
        _ if is_free_spin(s) => Control::FreeSpin(settings::range(s)),
        _ if !choices.is_empty() => Control::Choice(choices),
        _ => Control::Number(settings::range(s)),
    }
}

/// The switch that turns automatic free spin on, before its threshold's row.
const FREE_SPIN: &str = "Automatic Free Spin";

/// The titles of the rows of the device's settings and readings, in order. A free spin
/// threshold has none while free spin is off.
pub fn titles(app: &App<Live>, d: &DeviceView) -> Vec<String> {
    let settings = app.settings_of(d);
    let figures = words::wheel_figures(d.d.information.as_ref());
    let mut out = Vec::new();
    for part in parts(app, d, &settings, &readings(&d.d), &figures) {
        match part {
            Part::Gamepad(_) => out.push("Gamepad Output".to_owned()),
            Part::Category(_) => {}
            Part::Setting(field, Control::FreeSpin(_)) => {
                out.push(FREE_SPIN.to_owned());
                let shown = settings
                    .iter()
                    .find(|s| s.field == field)
                    .and_then(|s| app.shown_value(d, s));
                if shown != Some(Value::Integer(FREE_SPIN_OFF)) {
                    out.push(catalog::label(&field));
                }
            }
            Part::Setting(field, _) | Part::Reading(field) => out.push(catalog::label(&field)),
            Part::Figure(name) => out.push(name.to_owned()),
        }
    }
    out
}

/// One part of the settings, in order; their groups are rebuilt when the parts change.
#[derive(Clone, Debug, PartialEq)]
enum Part {
    Gamepad(Vec<OutputMode>),
    Category(&'static str),
    Setting(Field, Control),
    Reading(Field),
    Figure(&'static str),
}

/// What an integer row's handlers share between redraws.
#[derive(Default)]
struct Spin {
    /// The model's value at the row's last redraw, `Some(None)` while it is unknown, or the value
    /// the row last committed.
    shown: Cell<Option<Option<i64>>>,
    /// The row's value as last shown or committed. GTK puts back text it rejects by notifying
    /// the unchanged value, which commits nothing.
    value: Cell<i64>,
    /// Set by the input handler for text the row accepts, so committing the value the row holds
    /// already still applies it while the model's value is unknown or out of range.
    typed: Cell<bool>,
}

impl Spin {
    /// The number the row shows while it holds `v`: the model's value until the row's value
    /// moves away from it, so a model value outside the row's range or steps shows as it is.
    fn showing(&self, v: i64) -> Option<i64> {
        match self.shown.get() {
            Some(n) if v == self.value.get() => n,
            _ => Some(v),
        }
    }
}

enum Widget {
    ReadOnly(gtk::Label),
    Switch(adw::SwitchRow),
    FreeSpin(adw::SwitchRow, adw::SpinRow, Rc<Spin>),
    Choice(adw::ComboRow, Vec<Value>),
    Number(adw::SpinRow, Rc<Spin>),
}

struct Row {
    field: Field,
    row: adw::ActionRow,
    widget: Widget,
    marker: Menu,
}

struct Gamepad {
    row: adw::ComboRow,
    modes: Vec<OutputMode>,
}

/// The settings' groups, placed before the page's other groups.
pub struct Settings {
    page: adw::PreferencesPage,
    /// The page's groups that follow the settings.
    tail: Vec<adw::PreferencesGroup>,
    parts: Vec<Part>,
    groups: Vec<adw::PreferencesGroup>,
    gamepad: Option<Gamepad>,
    rows: Vec<Row>,
    readings: Vec<(Field, gtk::Label)>,
    figures: Vec<(&'static str, gtk::Label)>,
    /// Why the settings couldn't be read, saved or applied, with Retry for a failed read.
    error: List,
    reload: Btn,
    /// Retry in each category's header, offered while one of its saved values failed to apply.
    reapply: Vec<(&'static str, Btn)>,
}

impl Settings {
    pub fn new(ctx: &Rc<Ctx>, tab: &TabView, tail: Vec<adw::PreferencesGroup>) -> Self {
        let error = List::new(ctx, "");
        let reload = Btn::new(ctx, "Retry");
        reload.widget.set_valign(gtk::Align::Center);
        error.group.set_header_suffix(Some(&reload.widget));
        tab.add(&error.group);
        for g in &tail {
            tab.page.remove(g);
            tab.page.add(g);
        }
        Self {
            error,
            reload,
            reapply: Vec::new(),
            page: tab.page.clone(),
            tail,
            parts: Vec::new(),
            groups: Vec::new(),
            gamepad: None,
            rows: Vec::new(),
            readings: Vec::new(),
            figures: Vec::new(),
        }
    }

    /// Shows the device's settings, or removes them while it has none to offer.
    pub fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>, d: &DeviceView, offered: bool) {
        if !offered {
            if !self.parts.is_empty() {
                self.build(ctx, &[]);
                self.parts.clear();
            }
            self.error.set(&[]);
            self.reload.hide();
            return;
        }
        let settings = app.settings_of(d);
        let readings = readings(&d.d);
        let figures = words::wheel_figures(d.d.information.as_ref());
        let parts = parts(app, d, &settings, &readings, &figures);
        if parts != self.parts {
            self.build(ctx, &parts);
            self.parts = parts;
        }
        let editable = app.settings_editable(d);
        let submission = app.submission(&d.adapter, d.d.id).cloned();

        let gamepad_change = app.device_changes(d).gamepad;
        if let Some(g) = &self.gamepad {
            let shown = gamepad_change.or_else(|| model::gamepad_mode(&d.d));
            let i = g
                .modes
                .iter()
                .position(|m| Some(*m) == shown)
                .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32);
            if g.row.selected() != i {
                g.row.set_selected(i);
            }
            g.row.set_sensitive(editable);
        }

        let fresh_device = App::<Live>::settings_current(d);
        for r in &mut self.rows {
            let Some(s) = settings.iter().find(|s| s.field == r.field) else {
                continue;
            };
            let shown = app.shown_value(d, s);
            show_control(&r.widget, s, shown.as_ref(), fresh_device);
            r.row.set_sensitive(editable);
            if let Widget::FreeSpin(_, threshold, _) = &r.widget {
                threshold.set_sensitive(editable);
            }
            let marker = app.marker(d, s, !editable);
            show_marker(&r.marker.button, &r.row.title(), marker.text, marker.tone);
            let items: Vec<(String, Option<Action>)> = marker
                .items
                .into_iter()
                .map(|(label, action)| (label.to_owned(), action))
                .collect();
            r.marker.set(&items);
            set_subtitle(&r.row, &notes(app, d, s, submission.as_ref()));
        }
        let look = if d.connected() {
            Look::Plain
        } else {
            Look::Dim
        };
        for (field, label) in &self.readings {
            if let Some((_, v)) = readings.iter().find(|(f, _)| f == field) {
                let mut text = value_text(field.leaf, Some(v));
                let unit = display_unit(field.leaf, Some(v));
                if !unit.is_empty() {
                    text = format!("{text} {unit}");
                }
                set_label(label, &text);
                set_look(label, if look == Look::Plain { Look::Dim } else { look });
            }
        }
        for (name, label) in &self.figures {
            if let Some((_, v)) = figures.iter().find(|(n, _)| n == name) {
                set_label(label, v);
            }
        }
        self.update_buttons(app, d, &settings, editable);
        self.update_error(app, d);
    }

    /// Shows why the settings couldn't be read, saved or applied.
    fn update_error(&mut self, app: &App<Live>, d: &DeviceView) {
        let key = (d.adapter.clone(), d.d.id);
        let submission = app.submission(&key.0, key.1);
        let note = app
            .note(&Spot::SettingsNote(key.0.clone(), key.1))
            .map(str::to_owned)
            .or_else(|| {
                submission
                    .filter(|s| !s.running && s.error.is_some() && !s.unknown)
                    .map(|s| format!("Couldn't Save {}", s.fields.len()))
            })
            .or_else(|| match model::hidpp_up(&d.d) {
                model::Up::Error(e) => Some(words::error_text(&e, words::Scope::Apply)),
                _ => None,
            });
        let note = match note {
            Some(note) => Some(Item::text(note).look(Look::Error)),
            None if app.settings_error(&key.0, key.1).is_some() => {
                Some(Item::text(words::SETTINGS_READ_FAILED).look(Look::Dim))
            }
            None => None,
        };
        self.error.set(&note.into_iter().collect::<Vec<_>>());
    }

    fn update_buttons(
        &self,
        app: &App<Live>,
        d: &DeviceView,
        settings: &[Setting],
        editable: bool,
    ) {
        let read_failed = app.settings_error(&d.adapter, d.d.id).is_some();
        let reloading = app.running(
            &d.adapter,
            |k| matches!(k, Kind::SettingsList(x) if *x == d.d.id),
        );
        self.reload
            .offer(read_failed, when(!reloading, Action::SettingsReload));
        let failed: Vec<&Setting> = settings
            .iter()
            .filter(|s| {
                matches!(&s.status, Some(Err(e)) if !model::unsupported(e)) && s.saved.is_some()
            })
            .collect();
        let any_free = failed.iter().any(|s| app.draft_of(d, &s.field).is_none());
        for (category, retry) in &self.reapply {
            retry.offer(
                failed
                    .iter()
                    .any(|s| catalog::category(s.field.leaf) == *category),
                when(editable && any_free, Action::SettingsReapply),
            );
        }
    }

    fn build(&mut self, ctx: &Rc<Ctx>, parts: &[Part]) {
        for g in self.groups.drain(..) {
            self.page.remove(&g);
        }
        self.gamepad = None;
        self.reapply.clear();
        self.rows.clear();
        self.readings.clear();
        self.figures.clear();
        let mut group: Option<adw::PreferencesGroup> = None;
        let add =
            |page: &adw::PreferencesPage, groups: &mut Vec<adw::PreferencesGroup>, title: &str| {
                let g = adw::PreferencesGroup::builder().title(title).build();
                page.add(&g);
                groups.push(g.clone());
                g
            };
        for part in parts {
            match part {
                Part::Gamepad(modes) => {
                    let g = add(&self.page, &mut self.groups, "Gamepad");
                    let gamepad = gamepad_row(ctx, modes);
                    g.add(&gamepad.row);
                    self.gamepad = Some(gamepad);
                }
                Part::Category(name) => {
                    let g = add(&self.page, &mut self.groups, name);
                    let retry = Btn::new(ctx, "Retry");
                    retry.widget.set_valign(gtk::Align::Center);
                    retry.hide();
                    g.set_header_suffix(Some(&retry.widget));
                    self.reapply.push((name, retry));
                    group = Some(g);
                }
                Part::Setting(field, control) => {
                    if let Some(g) = &group {
                        let row = setting_row(ctx, *field, control, g);
                        self.rows.push(row);
                    }
                }
                Part::Reading(field) => {
                    if let Some(g) = &group {
                        let label = value_row(g, &catalog::label(field));
                        self.readings.push((*field, label));
                    }
                }
                Part::Figure(name) => {
                    if let Some(g) = &group {
                        let label = value_row(g, name);
                        self.figures.push((name, label));
                    }
                }
            }
        }
        // A page only appends groups, so the groups after the settings move back behind them.
        if !self.groups.is_empty() {
            for g in &self.tail {
                self.page.remove(g);
                self.page.add(g);
            }
        }
    }
}

fn parts(
    app: &App<Live>,
    d: &DeviceView,
    settings: &[Setting],
    readings: &[(Field, Value)],
    figures: &[(&'static str, String)],
) -> Vec<Part> {
    let mut out = Vec::new();
    let gamepad = app.gamepad_choices(d);
    if !gamepad.is_empty() {
        out.push(Part::Gamepad(gamepad));
    }
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
    for category in categories {
        out.push(Part::Category(category));
        for s in settings
            .iter()
            .filter(|s| catalog::category(s.field.leaf) == category)
        {
            out.push(Part::Setting(s.field, control(s)));
        }
        for (field, _) in readings
            .iter()
            .filter(|(f, _)| catalog::category(f.leaf) == category)
        {
            out.push(Part::Reading(*field));
        }
        if category == "Wheel" {
            out.extend(figures.iter().map(|(label, _)| Part::Figure(label)));
        }
    }
    out
}

/// The notes below a setting: why it couldn't be saved, the range a typed number must be in, the
/// device's own value, and why its saved value couldn't be applied.
fn notes(
    app: &App<Live>,
    d: &DeviceView,
    s: &Setting,
    submission: Option<&crate::ui::app::Submission>,
) -> String {
    let mut notes = Vec::new();
    if let Some(sub) = submission
        && !sub.running
        && sub.fields.contains(&s.field)
        && let Some(e) = &sub.error
    {
        notes.push(if sub.unknown {
            words::reason(e).to_owned()
        } else {
            format!("Couldn't Save: {}", words::reason(e))
        });
    }
    if change_of(s, app.draft_of(d, &s.field)).is_err() {
        notes.push(range_text(s));
    }
    let fresh = App::<Live>::settings_current(d) && s.value.is_some();
    if s.saved.is_some() && s.status == Some(Ok(SettingState::ChangedOnDevice)) && fresh {
        notes.push(format!(
            "Device: {}",
            value_text(s.field.leaf, s.value.as_ref())
        ));
    }
    if let Some(Err(e)) = &s.status
        && !(model::unsupported(e) && words::plain(e))
    {
        notes.push(words::reason(&words::error_text(e, words::Scope::Apply)).to_owned());
    }
    notes.join(" · ")
}

fn value_row(group: &adw::PreferencesGroup, title: &str) -> gtk::Label {
    let row = adw::ActionRow::builder().title(title).build();
    let label = gtk::Label::builder().selectable(true).build();
    row.add_suffix(&label);
    group.add(&row);
    label
}

fn gamepad_row(ctx: &Rc<Ctx>, modes: &[OutputMode]) -> Gamepad {
    let names: Vec<&str> = modes.iter().map(|m| text::output_mode_name(*m)).collect();
    let row = adw::ComboRow::builder()
        .title("Gamepad Output")
        .model(&gtk::StringList::new(&names))
        .build();
    let modes = modes.to_vec();
    let choices = modes.clone();
    row.connect_selected_notify(glib::clone!(
        #[weak]
        ctx,
        move |r| {
            if let Some(m) = choices.get(r.selected() as usize) {
                ctx.act(Action::GamepadOutput(*m));
            }
        }
    ));
    Gamepad { row, modes }
}

/// The text an integer row shows for a value.
fn number_text(field: Field, v: Option<i64>) -> String {
    let v = v.map(Value::Integer);
    let mut text = value_text(field.leaf, v.as_ref());
    let unit = display_unit(field.leaf, v.as_ref());
    if !unit.is_empty() {
        text = format!("{text} {unit}");
    }
    text
}

/// The number typed into an integer row, read up to its unit; "Never" reads as 0.
fn parse_number(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("never") {
        return Some(0);
    }
    let number: String = text
        .chars()
        .enumerate()
        .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && *c == '-'))
        .map(|(_, c)| c)
        .collect();
    number.parse().ok()
}

/// A spin row for an integer, showing its unit, "Never" or "Unknown" in place of the number.
/// Stepping, Enter and leaving the row commit it, passing a new value to `apply`. Text that
/// isn't a number in range puts back the row's value.
fn number_row(
    field: Field,
    title: &str,
    range: Option<(i64, i64, i64)>,
    free_spin: bool,
    apply: impl Fn(i64) + 'static,
) -> (adw::SpinRow, Rc<Spin>) {
    let (mut min, mut max, step) = range.unwrap_or((i64::from(i32::MIN), i64::from(i32::MAX), 1));
    if free_spin {
        min = min.max(1);
        max = max.min(FREE_SPIN_OFF - 1);
    }
    let row = adw::SpinRow::builder()
        .title(title)
        .adjustment(&gtk::Adjustment::new(
            min as f64,
            min as f64,
            max as f64,
            step.max(1) as f64,
            (step.max(1) * 10) as f64,
            0.0,
        ))
        .snap_to_ticks(true)
        .numeric(false)
        .update_policy(gtk::SpinButtonUpdatePolicy::IfValid)
        // AT-SPI leaves out the presentation role libadwaita 1.6 and later give a spin row,
        // together with its spin button, so the row takes the list item role other rows have
        // and its title names the spin button.
        .accessible_role(gtk::AccessibleRole::ListItem)
        .build();
    row.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(title)]);
    let state = Rc::new(Spin::default());
    let commit = Rc::new(glib::clone!(
        #[strong]
        state,
        move |v: i64| {
            state.value.set(v);
            state.shown.set(Some(Some(v)));
            apply(v);
        }
    ));
    row.connect_input(glib::clone!(
        #[strong]
        state,
        move |r| {
            // The text the row shows already commits nothing, so leaving a row that holds an
            // off-step model value doesn't apply the step GTK snaps it to.
            let text = r.text();
            let shows = text == number_text(field, state.showing(r.value() as i64));
            let n = parse_number(&text).filter(|_| !shows);
            let a = r.adjustment();
            state
                .typed
                .set(n.is_some_and(|n| (a.lower()..=a.upper()).contains(&(n as f64))));
            // libadwaita 1.5.0 and 1.5.1 lose the value a spin row's input handler returns, so
            // the row leaves the plain number for GTK to read.
            let Some(n) = n else {
                return Some(Err(()));
            };
            let plain = n.to_string();
            if text != plain {
                r.set_text(&plain);
            }
            None
        }
    ));
    row.connect_output(glib::clone!(
        #[strong]
        state,
        #[strong]
        commit,
        move |r| {
            let mut v = r.value() as i64;
            if state.typed.take() && state.shown.get() != Some(Some(v)) {
                commit(v);
            } else if let Some(Some(n)) = state.shown.get()
                && v == state.value.get()
            {
                // A model value that arrived while text was typed shows once the row puts the
                // text back, so the row's value moves to it too and steps from there.
                let at = within(&r.adjustment(), n);
                if at as i64 != v {
                    v = at as i64;
                    state.value.set(v);
                    r.set_value(at);
                }
            }
            let text = number_text(field, state.showing(v));
            if r.text() != text {
                r.set_text(&text);
            }
            true
        }
    ));
    row.connect_value_notify(glib::clone!(
        #[strong]
        state,
        move |r| {
            let v = r.value() as i64;
            if v != state.value.get() {
                commit(v);
            }
        }
    ));
    (row, state)
}

/// Shows the model's value in an integer row. Text typed into the focused row stays until the
/// row commits it, so a failed write or a readback doesn't replace a number being typed.
fn show_number(row: &adw::SpinRow, state: &Spin, field: Field, v: Option<&Value>) {
    let n = match v {
        Some(Value::Integer(n)) => Some(*n),
        _ => None,
    };
    if state.shown.get() == Some(n) {
        return;
    }
    let typing = row
        .root()
        .and_then(|r| r.focus())
        .is_some_and(|f| f.is_ancestor(row))
        && row.text() != number_text(field, state.showing(row.value() as i64));
    state.shown.set(Some(n));
    if typing {
        return;
    }
    let at = n.map_or(row.value(), |n| within(&row.adjustment(), n));
    state.value.set(at as i64);
    if row.value() != at {
        row.set_value(at);
    }
    let text = number_text(field, n);
    if row.text() != text {
        row.set_text(&text);
    }
}

/// The nearest value to `n` an integer row can hold.
fn within(a: &gtk::Adjustment, n: i64) -> f64 {
    (n as f64).min(a.upper()).max(a.lower())
}

/// Applies a number an integer row commits.
fn apply_number(ctx: &Rc<Ctx>, field: Field) -> impl Fn(i64) + 'static {
    glib::clone!(
        #[weak]
        ctx,
        move |n: i64| ctx.act(Action::SettingChoice(field, Value::Integer(n)))
    )
}

fn setting_row(
    ctx: &Rc<Ctx>,
    field: Field,
    control: &Control,
    group: &adw::PreferencesGroup,
) -> Row {
    let title = catalog::label(&field);
    let (row, widget): (adw::ActionRow, Widget) = match control {
        Control::ReadOnly => {
            let row = adw::ActionRow::builder().title(&title).build();
            let label = gtk::Label::builder().selectable(true).build();
            row.add_suffix(&label);
            (row, Widget::ReadOnly(label))
        }
        Control::Switch => {
            let row = adw::SwitchRow::builder().title(&title).build();
            row.connect_active_notify(glib::clone!(
                #[weak]
                ctx,
                move |r| ctx.act(Action::SettingBool(field, r.is_active()))
            ));
            (row.clone().upcast(), Widget::Switch(row))
        }
        Control::FreeSpin(range) => {
            let row = adw::SwitchRow::builder().title(FREE_SPIN).build();
            row.connect_active_notify(glib::clone!(
                #[weak]
                ctx,
                move |r| ctx.act(Action::FreeSpin(field, r.is_active()))
            ));
            let (threshold, state) =
                number_row(field, &title, *range, true, apply_number(ctx, field));
            group.add(&row);
            group.add(&threshold);
            let widget = Widget::FreeSpin(row.clone(), threshold, state);
            return finish(ctx, field, row.upcast(), widget);
        }
        Control::Choice(choices) => {
            let names: Vec<String> = choices
                .iter()
                .map(|v| value_text(field.leaf, Some(v)))
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let row = adw::ComboRow::builder()
                .title(&title)
                .model(&gtk::StringList::new(&names))
                .build();
            let values = choices.clone();
            row.connect_selected_notify(glib::clone!(
                #[weak]
                ctx,
                move |r| {
                    if let Some(v) = values.get(r.selected() as usize) {
                        ctx.act(Action::SettingChoice(field, *v));
                    }
                }
            ));
            (row.clone().upcast(), Widget::Choice(row, choices.clone()))
        }
        Control::Number(range) => {
            let (row, state) = number_row(field, &title, *range, false, apply_number(ctx, field));
            (row.clone().upcast(), Widget::Number(row, state))
        }
    };
    group.add(&row);
    finish(ctx, field, row, widget)
}

/// Puts the saved-state menu before a setting's title.
fn finish(ctx: &Rc<Ctx>, field: Field, row: adw::ActionRow, widget: Widget) -> Row {
    let marker = Menu::new(ctx);
    marker.button.add_css_class("flat");
    row.add_prefix(&marker.button);
    Row {
        field,
        row,
        widget,
        marker,
    }
}

/// Shows only the setting's state icon. Assistive technologies receive its name and state.
fn show_marker(button: &gtk::MenuButton, title: &str, text: &str, tone: Tone) {
    let (mark, state) = text.split_once(' ').unwrap_or((text, text));
    let label = button
        .child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
        .unwrap_or_else(|| {
            let label = gtk::Label::builder()
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build();
            button.set_child(Some(&label));
            label
        });
    set_label(&label, mark);
    button.set_always_show_arrow(false);
    button.update_property(&[gtk::accessible::Property::Label(&format!(
        "{title}: {state}"
    ))]);
    set_look(button, tone.into());
}

fn show_control(widget: &Widget, s: &Setting, shown: Option<&Value>, fresh_device: bool) {
    match widget {
        Widget::ReadOnly(label) => {
            let mut text = value_text(s.field.leaf, s.value.as_ref());
            let unit = display_unit(s.field.leaf, s.value.as_ref());
            if !unit.is_empty() {
                text = format!("{text} {unit}");
            }
            set_label(label, &text);
            let fresh = fresh_device && s.value.is_some();
            set_look(label, if fresh { Look::Plain } else { Look::Dim });
        }
        Widget::Switch(row) => {
            let on = matches!(shown, Some(Value::Bool(true)));
            if row.is_active() != on {
                row.set_active(on);
            }
        }
        Widget::FreeSpin(row, threshold, state) => {
            let on = !matches!(shown, Some(Value::Integer(FREE_SPIN_OFF)));
            if row.is_active() != on {
                row.set_active(on);
            }
            threshold.set_visible(on);
            if on {
                show_number(threshold, state, s.field, shown);
            }
        }
        Widget::Choice(row, choices) => {
            let i = shown
                .and_then(|v| choices.iter().position(|c| c == v))
                .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32);
            if row.selected() != i {
                row.set_selected(i);
            }
        }
        Widget::Number(row, state) => show_number(row, state, s.field, shown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Leaf;
    use std::cell::RefCell;

    #[test]
    fn reads_typed_numbers() {
        assert_eq!(parse_number("12"), Some(12));
        assert_eq!(parse_number(" 800 DPI "), Some(800));
        assert_eq!(parse_number("-3"), Some(-3));
        assert_eq!(parse_number("Never"), Some(0));
        assert_eq!(parse_number("Unknown"), None);
        assert_eq!(parse_number("-"), None);
        assert_eq!(parse_number(""), None);
    }

    /// The first widget of type `T` in `w`'s tree.
    fn find<T: IsA<gtk::Widget>>(w: &gtk::Widget) -> Option<T> {
        if let Ok(t) = w.clone().downcast::<T>() {
            return Some(t);
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(t) = find(&c) {
                return Some(t);
            }
            child = c.next_sibling();
        }
        None
    }

    /// A DPI row, the numbers it applies, and the text and spin button inside it.
    struct Rig {
        row: adw::SpinRow,
        state: Rc<Spin>,
        applied: Rc<RefCell<Vec<i64>>>,
        text: gtk::Text,
        button: gtk::SpinButton,
    }

    fn field() -> Field {
        Field::hidpp(Leaf::Dpi, Some(0))
    }

    impl Rig {
        fn new(range: (i64, i64, i64)) -> Self {
            let applied = Rc::new(RefCell::new(Vec::new()));
            let (row, state) = number_row(
                field(),
                "Pointer Sensitivity",
                Some(range),
                false,
                glib::clone!(
                    #[strong]
                    applied,
                    move |n| applied.borrow_mut().push(n)
                ),
            );
            let text = find(row.upcast_ref()).expect("a spin row has a text");
            let button = find(row.upcast_ref()).expect("a spin row has a spin button");
            Self {
                row,
                state,
                applied,
                text,
                button,
            }
        }

        /// Redraws the row with the model's value.
        fn show(&self, n: Option<i64>) {
            show_number(
                &self.row,
                &self.state,
                field(),
                n.map(Value::Integer).as_ref(),
            );
        }

        /// Puts the row in a window and focuses its text, as clicking into the row does.
        fn focus(&self) {
            let list = gtk::ListBox::new();
            list.append(&self.row);
            let window = gtk::Window::new();
            window.set_child(Some(&list));
            window.present();
            assert!(self.text.grab_focus(), "the row's text takes focus");
        }

        fn type_text(&self, typed: &str) {
            self.row.set_text(typed);
        }

        fn press_enter(&self) {
            self.text.emit_by_name::<()>("activate", &[]);
        }

        fn enter(&self, typed: &str) {
            self.type_text(typed);
            self.press_enter();
        }

        /// Commits the row as moving focus away from it does.
        fn leave(&self) {
            self.row.update();
        }

        /// Steps up as the Up key does.
        fn step_up(&self) {
            self.button
                .emit_by_name::<()>("change-value", &[&gtk::ScrollType::StepUp]);
        }

        fn applied(&self) -> Vec<i64> {
            self.applied.take()
        }

        fn shows(&self, n: Option<i64>) {
            assert_eq!(self.row.text(), number_text(field(), n));
        }
    }

    #[test]
    #[ignore = "requires a display"]
    fn integer_rows_commit_accepted_numbers() {
        adw::init().expect("GTK needs a display");
        let r = Rig::new((1, 1000, 1));
        assert_eq!(
            r.row.upcast_ref::<gtk::Widget>().accessible_role(),
            gtk::AccessibleRole::ListItem,
            "AT-SPI shows the row and its spin button"
        );

        r.show(None);
        r.shows(None);
        r.leave();
        assert!(
            r.applied().is_empty(),
            "leaving an untouched row applies nothing"
        );
        r.shows(None);
        r.enter("abc");
        assert!(
            r.applied().is_empty(),
            "text that isn't a number applies nothing"
        );
        r.shows(None);
        r.enter("5000");
        assert!(
            r.applied().is_empty(),
            "a number out of range applies nothing"
        );
        r.shows(None);
        r.enter("1");
        assert_eq!(
            r.applied(),
            [1],
            "the minimum commits while the value is unknown"
        );
        r.shows(Some(1));
        r.show(Some(1));
        assert!(r.applied().is_empty());

        for typed in ["8", "80", "800"] {
            r.type_text(typed);
            r.show(Some(1));
            assert_eq!(r.row.text(), typed, "a redraw keeps typed text");
        }
        assert!(
            r.applied().is_empty(),
            "typing applies nothing until it commits"
        );
        r.press_enter();
        assert_eq!(r.applied(), [800]);
        r.shows(Some(800));
        r.show(Some(800));

        r.enter("abc");
        r.type_text("0");
        r.leave();
        r.enter("1001");
        r.enter("800");
        assert!(
            r.applied().is_empty(),
            "rejected text and the same number keep the applied value"
        );
        r.shows(Some(800));
        assert_eq!(r.row.value(), 800.0);

        r.step_up();
        assert_eq!(r.applied(), [801], "stepping commits");
        r.shows(Some(801));
        r.show(Some(801));

        r.show(Some(42));
        assert!(
            r.applied().is_empty(),
            "showing the model's value applies nothing"
        );
        r.shows(Some(42));
        r.show(None);
        assert!(r.applied().is_empty());
        r.shows(None);
        r.enter("42");
        assert_eq!(
            r.applied(),
            [42],
            "the value the row holds commits while the value is unknown"
        );
        r.show(None);
        r.step_up();
        assert_eq!(r.applied(), [43], "stepping from an unknown value commits");
        r.shows(Some(43));

        let r = Rig::new((0, 1000, 5));
        r.show(Some(1500));
        r.shows(Some(1500));
        assert_eq!(r.row.value(), 1000.0);
        r.leave();
        r.enter("abc");
        r.enter("2000");
        assert!(
            r.applied().is_empty(),
            "a model value above the range stays without a commit"
        );
        r.shows(Some(1500));
        r.enter("1000");
        assert_eq!(r.applied(), [1000], "the bound the row holds commits");
        r.shows(Some(1000));

        r.show(Some(803));
        r.shows(Some(803));
        r.leave();
        r.enter("abc");
        assert!(
            r.applied().is_empty(),
            "leaving an off-step model value applies nothing"
        );
        r.shows(Some(803));
        r.enter("803");
        assert_eq!(
            r.applied(),
            [805],
            "a typed number snaps to the row's steps"
        );
        r.shows(Some(805));

        let r = Rig::new((1, 4000, 1));
        r.focus();
        r.show(Some(1000));
        r.enter("1200");
        assert_eq!(r.applied(), [1200]);
        r.show(Some(1200));
        r.type_text("18");
        r.show(Some(1000));
        assert_eq!(
            r.row.text(),
            "18",
            "a failed write keeps the text being typed"
        );
        r.type_text("1800");
        r.show(Some(1100));
        r.show(None);
        assert_eq!(
            r.row.text(),
            "1800",
            "a readback keeps the text being typed"
        );
        assert!(r.applied().is_empty());
        r.press_enter();
        assert_eq!(r.applied(), [1800], "Enter commits the typed number");
        r.shows(Some(1800));
        r.show(Some(1800));

        r.type_text("2000");
        r.show(Some(1000));
        r.leave();
        assert_eq!(r.applied(), [2000], "leaving commits the typed number");
        r.shows(Some(2000));
        r.show(Some(2000));

        r.show(Some(1500));
        r.shows(Some(1500));
        r.type_text("abc");
        r.show(Some(900));
        r.leave();
        assert!(r.applied().is_empty());
        r.shows(Some(900));
        assert_eq!(
            r.row.value(),
            900.0,
            "rejected text leaves the row at the model's value"
        );
        r.step_up();
        assert_eq!(
            r.applied(),
            [901],
            "stepping goes on from the model's value"
        );
        r.shows(Some(901));
    }
}
