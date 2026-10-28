//! A device's settings on its page: why they couldn't be read or saved, its gamepad output, then
//! its settings grouped by category, each with the control its type calls for followed by a menu
//! of its saved state, beside the readings of the same category. A change applies as it is made;
//! a number applies once it is committed.
//!
//! A number one of whose values turns it off is a switch row followed by the number's row while
//! it is on. The two rows share a band of color on the list's edge, the next of six in the order
//! the page builds them, so turning one on or off recolors no other.
use super::{
    shell::Ctx,
    widgets::{
        BANDS, Btn, Item, List, Look, Menu, Tab as TabView, set_label, set_look, set_subtitle, when,
    },
};
use crate::{
    model,
    settings::{self, Field, Leaf, Setting, Type, Value},
    ui::{
        app::{
            Action, App, Kind, Live, Spot, Tone,
            device::{save_failed, settings_count},
            settings::{FREE_SPIN_OFF, change_of, is_free_spin, range_text, readings, unit_text},
            words,
            world::DeviceView,
        },
        catalog, text,
    },
};
use adw::prelude::*;
use cordial_protocol::{OutputMode, SettingState};
use gtk::{gio, glib};
use std::{cell::Cell, rc::Rc};

/// The control a setting gets, which decides its row.
#[derive(Clone, Debug, PartialEq)]
enum Control {
    ReadOnly,
    Switch,
    Toggled(Toggle, Option<(i64, i64, i64)>),
    Choice(Vec<Value>),
    Number(Option<(i64, i64, i64)>),
}

fn control(s: &Setting) -> Control {
    let choices = settings::choices(s);
    match settings::kind(s) {
        Type::Choice if choices.is_empty() => Control::ReadOnly,
        Type::Bool => Control::Switch,
        _ if is_free_spin(s) => Control::Toggled(Toggle::FreeSpin, settings::range(s)),
        _ if !choices.is_empty() => Control::Choice(choices),
        _ if s.field.leaf == Leaf::AutoOff && settings::accepts(s, &Value::Integer(0)) => {
            Control::Toggled(Toggle::AutoOff, settings::range(s))
        }
        _ => Control::Number(settings::range(s)),
    }
}

/// The range of an integer without limits.
const WHOLE: (i64, i64, i64) = (i32::MIN as i64, i32::MAX as i64, 1);

/// The switch that turns automatic free spin on, in the row above its threshold's row.
const FREE_SPIN: &str = "Automatic Free Spin";

/// The row of automatic power-off's timeout, below the row of its switch.
const POWER_OFF_TIMEOUT: &str = "Power-Off Timeout";

/// A number one of whose values turns it off, shown as a switch in a row followed by the number's
/// row while it is on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Toggle {
    FreeSpin,
    AutoOff,
}

impl Toggle {
    /// The value that turns it off.
    fn off(self) -> i64 {
        match self {
            Self::FreeSpin => FREE_SPIN_OFF,
            Self::AutoOff => 0,
        }
    }

    /// The titles of the switch and of the number.
    fn titles(self, field: &Field) -> (String, String) {
        match self {
            Self::FreeSpin => (FREE_SPIN.to_owned(), catalog::label(field)),
            Self::AutoOff => (catalog::label(field), POWER_OFF_TIMEOUT.to_owned()),
        }
    }

    /// The number's range while it is on. Zero is on the steps of automatic power-off's range,
    /// so its shortest timeout is one step.
    fn range(self, (min, max, step): (i64, i64, i64)) -> (i64, i64, i64) {
        match self {
            Self::FreeSpin => (min.max(1), max.min(FREE_SPIN_OFF - 1), step),
            Self::AutoOff => (min.max(step), max, step),
        }
    }
}

/// Whether a switch shows on, or None while its value is unknown. A switch of a number is on at
/// any value but `off`.
fn switched(off: Option<i64>, shown: Option<&Value>) -> Option<bool> {
    match (off, shown?) {
        (None, Value::Bool(on)) => Some(*on),
        (Some(off), Value::Integer(n)) => Some(*n != off),
        _ => None,
    }
}

/// The titles of the rows of the device's settings and readings, in order. A number with a switch
/// has none while it is off or unknown.
pub fn titles(app: &App<Live>, d: &DeviceView) -> Vec<String> {
    let settings = app.settings_of(d);
    let figures = words::wheel_figures(d.d.information.as_ref());
    let mut out = Vec::new();
    for part in parts(app, d, &settings, &readings(&d.d), &figures) {
        match part {
            Part::Gamepad(_) => out.push("Gamepad Output".to_owned()),
            Part::Category(_) => {}
            Part::Setting(field, Control::Toggled(toggle, _)) => {
                let (switch, number) = toggle.titles(&field);
                out.push(switch);
                let shown = settings
                    .iter()
                    .find(|s| s.field == field)
                    .and_then(|s| app.shown_value(d, s));
                if switched(Some(toggle.off()), shown.as_ref()) == Some(true) {
                    out.push(number);
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

/// An on/off control: a switch, with Off and On in its place while the value is unknown.
struct OnOff {
    switch: gtk::Switch,
    unknown: gtk::Box,
    /// The row the switch is the control of, which toggles it while it is shown.
    row: Option<adw::ActionRow>,
}

impl OnOff {
    /// The switch and its unknown value's choices, for the caller to place.
    fn new(title: &str, set: impl Fn(bool) + 'static) -> Self {
        let set = Rc::new(set);
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        switch.update_property(&[gtk::accessible::Property::Label(title)]);
        switch.connect_active_notify(glib::clone!(
            #[strong]
            set,
            move |s| set(s.is_active())
        ));
        let choices = gtk::Box::builder()
            .css_classes(["linked"])
            .valign(gtk::Align::Center)
            .build();
        for (label, on) in [("Off", false), ("On", true)] {
            let b = gtk::Button::with_label(label);
            b.update_property(&[gtk::accessible::Property::Label(&format!(
                "{title}: {label}"
            ))]);
            b.connect_clicked(glib::clone!(
                #[strong]
                set,
                move |_| set(on)
            ));
            choices.append(&b);
        }
        let unknown = gtk::Box::builder().spacing(12).visible(false).build();
        unknown.append(
            &gtk::Label::builder()
                .label("Unknown")
                .css_classes(["dim-label"])
                .build(),
        );
        unknown.append(&choices);
        Self {
            switch,
            unknown,
            row: None,
        }
    }

    /// The switch in a row of its own.
    fn row(title: &str, set: impl Fn(bool) + 'static) -> Self {
        let mut s = Self::new(title, set);
        let row = adw::ActionRow::builder().title(title).build();
        row.add_suffix(&s.unknown);
        row.add_suffix(&s.switch);
        row.set_activatable_widget(Some(&s.switch));
        s.row = Some(row);
        s
    }

    fn show(&self, on: Option<bool>) {
        if self.switch.is_visible() != on.is_some() {
            self.switch.set_visible(on.is_some());
            self.unknown.set_visible(on.is_none());
            // Activating the row toggles the switch only while it is shown.
            if let Some(row) = &self.row {
                row.set_activatable_widget(on.is_some().then_some(&self.switch));
            }
        }
        if let Some(on) = on
            && self.switch.is_active() != on
        {
            self.switch.set_active(on);
        }
    }
}

/// A row of choices whose menu button shows the value even while it is unknown or none of the
/// choices, which a combo row can't. The menu lists only the choices and marks the value's.
/// Picking the marked choice applies nothing, as in a combo row.
struct Pick {
    row: adw::ActionRow,
    button: gtk::MenuButton,
    /// The position of the marked choice, or -1 for none.
    chosen: gio::SimpleAction,
}

impl Pick {
    /// `set` gets the position of the choice the user picks.
    fn new(title: &str, names: &[&str], set: impl Fn(usize) + 'static) -> Self {
        let menu = gio::Menu::new();
        for (i, name) in names.iter().enumerate() {
            let target = (i as i32).to_variant();
            menu.append(
                Some(name),
                Some(&gio::Action::print_detailed_name(
                    "pick.choice",
                    Some(&target),
                )),
            );
        }
        let chosen = gio::SimpleAction::new_stateful(
            "choice",
            Some(glib::VariantTy::INT32),
            &(-1i32).to_variant(),
        );
        chosen.connect_activate(move |a, target| {
            if let Some(i) = target.and_then(|t| t.get::<i32>())
                && a.state().and_then(|s| s.get::<i32>()) != Some(i)
            {
                set(i as usize);
            }
        });
        let group = gio::SimpleActionGroup::new();
        group.add_action(&chosen);
        let button = gtk::MenuButton::builder()
            .menu_model(&menu)
            .always_show_arrow(true)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        button.insert_action_group("pick", Some(&group));
        button.update_property(&[gtk::accessible::Property::Label(title)]);
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable_widget(&button)
            .build();
        row.add_suffix(&button);
        Self {
            row,
            button,
            chosen,
        }
    }

    /// Marks the choice at `i`, or none, and shows the value as `text`.
    fn show(&self, i: Option<usize>, text: &str) {
        let i = i.map_or(-1, |i| i as i32).to_variant();
        if self.chosen.state().as_ref() != Some(&i) {
            self.chosen.set_state(&i);
        }
        if self.button.label().as_deref() != Some(text) {
            self.button.set_label(text);
            self.button
                .update_property(&[gtk::accessible::Property::Description(text)]);
        }
    }
}

enum Widget {
    ReadOnly(gtk::Label),
    Switch(OnOff),
    /// A number's switch in its row, the number's row, the value that turns it off, and the
    /// rows' band.
    Toggled(OnOff, adw::SpinRow, Rc<Spin>, i64, &'static str),
    Choice(Pick, Vec<Value>),
    Number(adw::SpinRow, Rc<Spin>),
}

struct Row {
    field: Field,
    row: adw::PreferencesRow,
    widget: Widget,
    marker: Menu,
}

struct Gamepad {
    pick: Pick,
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
            g.pick.show(
                g.modes.iter().position(|m| Some(*m) == shown),
                shown.map_or("Unknown", text::output_mode_name),
            );
            g.pick.row.set_sensitive(editable);
        }

        let fresh_device = App::<Live>::settings_current(d);
        for r in &mut self.rows {
            let Some(s) = settings.iter().find(|s| s.field == r.field) else {
                continue;
            };
            let shown = app.shown_value(d, s);
            show_control(&r.widget, s, shown.as_ref(), fresh_device);
            r.row.set_sensitive(editable);
            if let Widget::Toggled(_, number, ..) = &r.widget {
                number.set_sensitive(editable);
            }
            let marker = app.marker(d, s, !editable);
            show_marker(&r.marker.button, &r.row.title(), marker.text, marker.tone);
            let items: Vec<(String, Option<Action>)> = marker
                .items
                .into_iter()
                .map(|(label, action)| (label.to_owned(), action))
                .collect();
            r.marker.set(&items);
            let notes = notes(app, d, s, submission.as_ref());
            if let Some(row) = r.row.downcast_ref::<adw::ActionRow>() {
                set_subtitle(row, &notes);
            }
        }
        let look = if d.connected() {
            Look::Plain
        } else {
            Look::Dim
        };
        for (field, label) in &self.readings {
            if let Some((_, v)) = readings.iter().find(|(f, _)| f == field) {
                set_label(label, &unit_text(field.leaf, Some(v)));
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
                let s = submission.filter(|s| !s.running && !s.unknown)?;
                let what = settings_count(s.fields.len());
                Some(save_failed(&what, s.error.as_deref()?))
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
        // A category's readings follow its settings in a list of their own, as they have no
        // saved-state menu to line up with. Settings come before readings in each category.
        let mut settings_shown = false;
        let mut bands = 0;
        let add =
            |page: &adw::PreferencesPage, groups: &mut Vec<adw::PreferencesGroup>, title: &str| {
                let g = adw::PreferencesGroup::builder().title(title).build();
                page.add(&g);
                groups.push(g.clone());
                g
            };
        for part in parts {
            if settings_shown && matches!(part, Part::Reading(_) | Part::Figure(_)) {
                group = Some(add(&self.page, &mut self.groups, ""));
            }
            settings_shown = matches!(part, Part::Setting(..));
            match part {
                Part::Gamepad(modes) => {
                    let g = add(&self.page, &mut self.groups, "Gamepad");
                    let gamepad = gamepad_row(ctx, modes);
                    g.add(&gamepad.pick.row);
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
                        let row = setting_row(ctx, *field, control, g, &mut bands);
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
        let note = if sub.unknown {
            e.clone()
        } else {
            save_failed("this setting", e)
        };
        notes.push(note);
    }
    if change_of(s, app.draft_of(d, &s.field)).is_err() {
        notes.push(range_text(s));
    }
    let fresh = App::<Live>::settings_current(d) && s.value.is_some();
    if s.saved.is_some() && s.status == Some(Ok(SettingState::ChangedOnDevice)) && fresh {
        notes.push(format!(
            "Device: {}",
            unit_text(s.field.leaf, s.value.as_ref())
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
    let modes = modes.to_vec();
    let choices = modes.clone();
    let pick = Pick::new(
        "Gamepad Output",
        &names,
        glib::clone!(
            #[weak]
            ctx,
            move |i| ctx.act(Action::GamepadOutput(choices[i]))
        ),
    );
    Gamepad { pick, modes }
}

/// The text an integer row shows for a value: the number alone, or nothing while it is unknown.
fn number_text(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_default()
}

/// A spin row for an integer that takes only digits, with its unit beside it, and shows nothing
/// while the value is unknown. Stepping, Enter and leaving the row commit it, passing a new value
/// to `apply`. Text that isn't a number in range puts back the row's value.
fn number_row(
    field: Field,
    title: &str,
    (min, max, step): (i64, i64, i64),
    apply: impl Fn(i64) + 'static,
) -> (adw::SpinRow, Rc<Spin>) {
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
        .numeric(true)
        .digits(0)
        .update_policy(gtk::SpinButtonUpdatePolicy::IfValid)
        // AT-SPI leaves out the presentation role libadwaita 1.6 and later give a spin row,
        // together with its spin button, so the row takes the list item role other rows have
        // and its title names the spin button.
        .accessible_role(gtk::AccessibleRole::ListItem)
        .build();
    row.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(title)]);
    let unit = catalog::unit(field.leaf);
    if !unit.is_empty() {
        row.add_suffix(&gtk::Label::new(Some(unit)));
    }
    let state = Rc::new(Spin::default());
    state.value.set(min);
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
            // off-step model value doesn't apply the step GTK snaps it to, and leaving a row
            // of an unknown value doesn't apply the 0 GTK reads empty text as.
            let text = r.text();
            let shows = text == number_text(state.showing(r.value() as i64));
            let n = text.parse::<i64>().ok().filter(|_| !shows);
            let a = r.adjustment();
            state
                .typed
                .set(n.is_some_and(|n| (a.lower()..=a.upper()).contains(&(n as f64))));
            // GTK reads an accepted number itself, as libadwaita 1.5.0 and 1.5.1 lose the value
            // a spin row's input handler returns.
            n.is_none().then_some(Err(()))
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
            let text = number_text(state.showing(v));
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
fn show_number(row: &adw::SpinRow, state: &Spin, v: Option<&Value>) {
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
        && row.text() != number_text(state.showing(row.value() as i64));
    state.shown.set(Some(n));
    if typing {
        return;
    }
    let at = n.map_or(row.value(), |n| within(&row.adjustment(), n));
    state.value.set(at as i64);
    if row.value() != at {
        row.set_value(at);
    }
    let text = number_text(n);
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

/// A setting's row, or a switch's row and its number's row, in `group`. A number with a switch
/// takes the next of the page's bands, counted in `bands`.
fn setting_row(
    ctx: &Rc<Ctx>,
    field: Field,
    control: &Control,
    group: &adw::PreferencesGroup,
    bands: &mut usize,
) -> Row {
    let title = catalog::label(&field);
    let marker = marker(ctx);
    let (row, widget): (adw::ActionRow, Widget) = match control {
        Control::ReadOnly => {
            let row = adw::ActionRow::builder().title(&title).build();
            let label = gtk::Label::builder().selectable(true).build();
            row.add_suffix(&label);
            (row, Widget::ReadOnly(label))
        }
        Control::Switch => {
            let switch = OnOff::row(
                &title,
                glib::clone!(
                    #[weak]
                    ctx,
                    move |on| ctx.act(Action::SettingBool(field, on))
                ),
            );
            (switch.row.clone().unwrap(), Widget::Switch(switch))
        }
        Control::Toggled(toggle, range) => {
            let toggle = *toggle;
            let (band, _) = BANDS[*bands % BANDS.len()];
            *bands += 1;
            toggled_row(
                field,
                toggle,
                *range,
                band,
                glib::clone!(
                    #[weak]
                    ctx,
                    move |on, n| match toggle {
                        Toggle::FreeSpin => ctx.act(Action::FreeSpin(field, on)),
                        Toggle::AutoOff => ctx.act(Action::SettingChoice(
                            field,
                            Value::Integer(if on { n } else { toggle.off() }),
                        )),
                    }
                ),
                apply_number(ctx, field),
            )
        }
        Control::Choice(choices) => {
            let names: Vec<String> = choices
                .iter()
                .map(|v| unit_text(field.leaf, Some(v)))
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let values = choices.clone();
            let pick = Pick::new(
                &title,
                &names,
                glib::clone!(
                    #[weak]
                    ctx,
                    move |i| ctx.act(Action::SettingChoice(field, values[i]))
                ),
            );
            (
                pick.row.clone().upcast(),
                Widget::Choice(pick, choices.clone()),
            )
        }
        Control::Number(range) => {
            let (row, state) = number_row(
                field,
                &title,
                range.unwrap_or(WHOLE),
                apply_number(ctx, field),
            );
            (row.clone().upcast(), Widget::Number(row, state))
        }
    };
    row.add_suffix(&marker.button);
    group.add(&row);
    if let Widget::Toggled(_, number, ..) = &widget {
        // The switch and the number set one stored value, whose menu is on the switch's row. The
        // number's row keeps the menu's width empty, so its controls end where the switch ends.
        let gap = gtk::Box::default();
        number.add_suffix(&gap);
        let menu = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        menu.add_widget(&marker.button);
        menu.add_widget(&gap);
        group.add(number);
    }
    Row {
        field,
        row: row.upcast(),
        widget,
        marker,
    }
}

/// A number's switch in a row of its own, and the number's row, shown below it while it is on in
/// the `band` both rows then share. The switch and the number set one value. `set` gets whether
/// the switch turns on, and the number the number's row holds to turn on with, which stays while
/// the row is hidden.
fn toggled_row(
    field: Field,
    toggle: Toggle,
    range: Option<(i64, i64, i64)>,
    band: &'static str,
    set: impl Fn(bool, i64) + 'static,
    apply: impl Fn(i64) + 'static,
) -> (adw::ActionRow, Widget) {
    let (title, number_title) = toggle.titles(&field);
    let range = toggle.range(range.unwrap_or(WHOLE));
    let (number, state) = number_row(field, &number_title, range, apply);
    number.set_visible(false);
    number.add_css_class(band);
    let switch = OnOff::row(
        &title,
        glib::clone!(
            #[strong]
            state,
            move |on| set(on, state.value.get())
        ),
    );
    let row = switch.row.clone().unwrap();
    let widget = Widget::Toggled(switch, number, state, toggle.off(), band);
    (row, widget)
}

/// The menu of a setting's saved state, shown after its control.
fn marker(ctx: &Rc<Ctx>) -> Menu {
    let marker = Menu::new(ctx);
    marker.button.add_css_class("flat");
    let icon = gtk::Image::builder()
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    marker.button.set_child(Some(&icon));
    marker.button.set_always_show_arrow(true);
    marker
}

/// Shows the setting's saved state as an icon named by its tooltip; Not Saved has the open
/// circle the state's text starts with. Saved values, the usual state, stay as dim as unsaved
/// ones.
fn show_marker(button: &gtk::MenuButton, title: &str, text: &str, tone: Tone) {
    let state = text.split_once(' ').map_or(text, |(_, state)| state);
    let icon = match state {
        "Not Saved" => "radio-symbolic",
        "Saved" => "object-select-symbolic",
        "Changed" | "Changed on Device" => "document-edit-symbolic",
        _ => "dialog-warning-symbolic",
    };
    if let Some(image) = button.child().and_downcast::<gtk::Image>()
        && image.icon_name().as_deref() != Some(icon)
    {
        image.set_icon_name(Some(icon));
    }
    if button.tooltip_text().as_deref() != Some(state) {
        button.set_tooltip_text(Some(state));
    }
    button.update_property(&[gtk::accessible::Property::Label(&format!(
        "{title}: {state}"
    ))]);
    set_look(
        button,
        match tone {
            Tone::Success => Look::Dim,
            tone => tone.into(),
        },
    );
}

fn show_control(widget: &Widget, s: &Setting, shown: Option<&Value>, fresh_device: bool) {
    match widget {
        Widget::ReadOnly(label) => {
            set_label(label, &unit_text(s.field.leaf, s.value.as_ref()));
            let fresh = fresh_device && s.value.is_some();
            set_look(label, if fresh { Look::Plain } else { Look::Dim });
        }
        Widget::Switch(switch) => switch.show(switched(None, shown)),
        Widget::Toggled(switch, number, state, off, band) => {
            let on = switched(Some(*off), shown);
            switch.show(on);
            let on = on == Some(true);
            if number.is_visible() != on {
                number.set_visible(on);
                let row = switch.row.as_ref().unwrap();
                if on {
                    row.add_css_class(band);
                } else {
                    row.remove_css_class(band);
                }
            }
            if on {
                show_number(number, state, shown);
            }
        }
        Widget::Choice(pick, choices) => pick.show(
            shown.and_then(|v| choices.iter().position(|c| c == v)),
            &unit_text(s.field.leaf, shown),
        ),
        Widget::Number(row, state) => show_number(row, state, shown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::shell::Ev;
    use cordial_protocol::{IntegerRange, integer_setting};
    use std::cell::RefCell;

    #[test]
    fn unknown_values_switch_neither_way() {
        let int = |n| Some(Value::Integer(n));
        assert_eq!(switched(None, None), None);
        assert_eq!(switched(None, Some(&Value::Bool(false))), Some(false));
        assert_eq!(switched(None, Some(&Value::Bool(true))), Some(true));
        assert_eq!(switched(None, int(1).as_ref()), None);
        let free_spin = Some(FREE_SPIN_OFF);
        assert_eq!(
            switched(free_spin, None),
            None,
            "free spin may be on or off"
        );
        assert_eq!(
            switched(free_spin, int(FREE_SPIN_OFF).as_ref()),
            Some(false)
        );
        assert_eq!(switched(free_spin, int(1).as_ref()), Some(true));
        assert_eq!(switched(free_spin, Some(&Value::Bool(true))), None);
        assert_eq!(switched(Some(0), int(0).as_ref()), Some(false));
        assert_eq!(switched(Some(0), int(60).as_ref()), Some(true));
    }

    /// An integer setting with a range.
    fn ranged(field: Field, min: i64, max: i64, step: u64) -> Setting {
        Setting {
            field,
            value: None,
            saved: None,
            status: None,
            limits: settings::Limits::Integer(Some(integer_setting::Limits::Range(IntegerRange {
                min,
                max,
                step,
            }))),
        }
    }

    #[test]
    fn automatic_power_off_has_a_switch_when_it_can_be_never() {
        let field = Field::hidpp(Leaf::AutoOff, None);
        assert_eq!(
            control(&ranged(field, 0, 15300, 60)),
            Control::Toggled(Toggle::AutoOff, Some((0, 15300, 60)))
        );
        assert_eq!(
            Toggle::AutoOff.range((0, 15300, 60)),
            (60, 15300, 60),
            "the shortest timeout is one step"
        );
        assert_eq!(
            control(&ranged(field, 60, 15300, 60)),
            Control::Number(Some((60, 15300, 60))),
            "a range without zero is only a number"
        );
    }

    #[test]
    #[ignore = "requires a display"]
    fn unknown_switches_offer_off_and_on() {
        adw::init().expect("GTK needs a display");
        // A switch toggles at the end of its animation, which a widget off screen never runs.
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let applied = Rc::new(RefCell::new(Vec::new()));
        let s = OnOff::row(
            FREE_SPIN,
            glib::clone!(
                #[strong]
                applied,
                move |on| applied.borrow_mut().push(on)
            ),
        );
        s.show(None);
        assert!(!s.switch.is_visible() && s.unknown.is_visible());
        ActionRowExt::activate(s.row.as_ref().unwrap());
        assert!(
            applied.take().is_empty(),
            "activating the row of an unknown value applies nothing"
        );
        let choices: gtk::Box = find(s.unknown.last_child().unwrap().upcast_ref()).unwrap();
        let off: gtk::Button = find(choices.upcast_ref()).unwrap();
        let on = off.next_sibling().and_downcast::<gtk::Button>().unwrap();
        off.emit_clicked();
        assert_eq!(applied.take(), [false], "Off applies off directly");
        on.emit_clicked();
        assert_eq!(applied.take(), [true], "On applies on directly");

        s.show(Some(false));
        assert!(s.switch.is_visible() && !s.unknown.is_visible());
        assert!(
            applied.take().is_empty(),
            "showing a known value applies nothing"
        );
        ActionRowExt::activate(s.row.as_ref().unwrap());
        assert_eq!(applied.take(), [true], "the row toggles a known value");
    }

    #[test]
    #[ignore = "requires a display"]
    fn every_saved_state_has_an_icon() {
        adw::init().expect("GTK needs a display");
        let mut icons = Vec::new();
        for (text, tone) in [
            ("○ Not Saved", Tone::Dim),
            ("● Saved", Tone::Success),
            ("✎ Changed", Tone::Accent),
            ("▲ Failed", Tone::Warning),
        ] {
            let button = gtk::MenuButton::new();
            button.set_child(Some(&gtk::Image::new()));
            show_marker(&button, "Pointer Sensitivity", text, tone);
            let icon = button
                .child()
                .and_downcast::<gtk::Image>()
                .unwrap()
                .icon_name();
            let icon = icon.expect("each state shows an icon").to_string();
            assert!(!icons.contains(&icon), "{text} has an icon of its own");
            icons.push(icon);
        }
    }

    #[test]
    #[ignore = "requires a display"]
    fn free_spin_threshold_follows_its_switch_while_on() {
        adw::init().expect("GTK needs a display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let (ctx, requests) = Ctx::test();
        let field = Field::hidpp(Leaf::FreeSpinThreshold, None);
        let s = ranged(field, 1, 255, 1);
        let group = adw::PreferencesGroup::new();
        let r = setting_row(&ctx, field, &control(&s), &group, &mut 0);
        let Widget::Toggled(switch, threshold, ..) = &r.widget else {
            unreachable!()
        };
        let row = switch.row.as_ref().unwrap();
        assert_eq!(row.title(), FREE_SPIN);
        assert_eq!(
            threshold.parent(),
            row.parent(),
            "the threshold's row is a plain row of the same list"
        );
        assert_eq!(
            row.next_sibling().as_ref(),
            Some(threshold.upcast_ref()),
            "the threshold's row follows its switch's"
        );
        let show =
            |n: Option<i64>| show_control(&r.widget, &s, n.map(Value::Integer).as_ref(), true);
        let banded = |w: &gtk::Widget| w.has_css_class("band-blue");

        show(None);
        assert!(!threshold.is_visible() && !banded(row.upcast_ref()));
        assert!(
            switch.unknown.is_visible(),
            "an unknown value offers Off and On"
        );
        show(Some(30));
        assert!(
            threshold.is_visible(),
            "turning free spin on shows its threshold"
        );
        assert!(banded(row.upcast_ref()) && banded(threshold.upcast_ref()));
        assert_eq!(threshold.text(), "30");
        show(Some(31));
        assert_eq!(threshold.text(), "31");
        show(Some(FREE_SPIN_OFF));
        assert!(!threshold.is_visible() && !banded(row.upcast_ref()));
        assert!(!switch.switch.is_active());
        while requests.try_recv().is_ok() {}
        ActionRowExt::activate(row);
        assert!(
            matches!(
                requests.try_recv(),
                Ok(Ev::Act(Action::FreeSpin(f, true))) if f == field
            ),
            "activating the switch's row turns free spin on"
        );
        show(Some(30));
        assert!(threshold.is_visible() && switch.switch.is_active());
        assert_eq!(threshold.text(), "30", "the threshold stays while hidden");
    }

    #[test]
    #[ignore = "requires a display"]
    fn automatic_power_off_turns_on_with_its_last_timeout() {
        adw::init().expect("GTK needs a display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let (ctx, requests) = Ctx::test();
        let field = Field::hidpp(Leaf::AutoOff, None);
        let s = ranged(field, 0, 15300, 60);
        let group = adw::PreferencesGroup::new();
        let r = setting_row(&ctx, field, &control(&s), &group, &mut 0);
        let Widget::Toggled(switch, timeout, ..) = &r.widget else {
            unreachable!()
        };
        let applied = || {
            std::iter::from_fn(|| requests.try_recv().ok())
                .map(|ev| match ev {
                    Ev::Act(Action::SettingChoice(f, Value::Integer(n))) if f == field => n,
                    _ => panic!("only automatic power-off's value applies"),
                })
                .collect::<Vec<_>>()
        };
        // A redraw's own changes reach no handler, as `Ctx::send` drops them.
        let show = |n: Option<i64>| {
            show_control(&r.widget, &s, n.map(Value::Integer).as_ref(), true);
            applied();
        };
        assert_eq!(r.row.title(), "Automatic Power-Off");
        assert_eq!(timeout.title(), POWER_OFF_TIMEOUT);
        assert!(
            find_label(timeout.upcast_ref(), "s"),
            "the timeout's unit shows beside it"
        );

        show(Some(0));
        assert!(switch.switch.is_visible() && !switch.switch.is_active());
        assert!(!timeout.is_visible(), "Never hides the timeout");
        switch.switch.set_active(true);
        assert_eq!(
            applied(),
            [60],
            "turning it on applies the shortest timeout"
        );
        show(Some(300));
        assert!(timeout.is_visible() && switch.switch.is_active());
        assert_eq!(timeout.text(), "300");
        switch.switch.set_active(false);
        assert_eq!(applied(), [0], "turning it off applies Never");
        show(Some(0));
        assert!(!timeout.is_visible());
        switch.switch.set_active(true);
        assert_eq!(applied(), [300], "turning it on again keeps its timeout");

        show(None);
        assert!(!switch.switch.is_visible() && switch.unknown.is_visible());
        assert!(!timeout.is_visible(), "an unknown value hides the timeout");
        let off: gtk::Button = find(switch.unknown.upcast_ref()).unwrap();
        let on = off.next_sibling().and_downcast::<gtk::Button>().unwrap();
        on.emit_clicked();
        assert_eq!(applied(), [300]);
        off.emit_clicked();
        assert_eq!(applied(), [0]);
    }

    #[test]
    #[ignore = "requires a display"]
    fn switch_and_number_rows_line_up_in_banded_pairs() {
        adw::init().expect("GTK needs a display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let (ctx, _requests) = Ctx::test();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        page.add(&group);
        let mut bands = 0;
        let plain = Field::hidpp(Leaf::Dpi, Some(0));
        let dpi = ranged(plain, 1, 4000, 1);
        let first = setting_row(&ctx, plain, &control(&dpi), &group, &mut bands);
        // Seven pairs, one more than the colors, so the last takes the first color again.
        let pairs: Vec<(Setting, Row)> = (0..7)
            .map(|i| {
                let s = ranged(Field::hidpp(Leaf::AutoOff, Some(i)), 0, 15300, 60);
                let r = setting_row(&ctx, s.field, &control(&s), &group, &mut bands);
                show_control(&r.widget, &s, Some(&Value::Integer(300)), true);
                (s, r)
            })
            .collect();
        let window = gtk::Window::builder()
            .default_width(480)
            .default_height(1200)
            .child(&page)
            .build();
        window.present();
        let Widget::Toggled(_, last, ..) = &pairs[6].1.widget else {
            unreachable!()
        };
        pump(&|| last.is_mapped() && last.width() > 0);
        assert!(
            !first
                .row
                .css_classes()
                .iter()
                .any(|c| c.starts_with("band-")),
            "a row on its own has no band"
        );
        let band = |r: &Row| match &r.widget {
            Widget::Toggled(switch, number, ..) => {
                let classes = |w: &gtk::Widget| {
                    w.css_classes()
                        .into_iter()
                        .filter(|c| c.starts_with("band-"))
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                };
                let row = classes(switch.row.as_ref().unwrap().upcast_ref());
                let number = classes(number.upcast_ref());
                assert!(number.len() == 1, "the number's row has one band");
                (!row.is_empty()).then(|| {
                    assert_eq!(row, number, "both rows share the band");
                    number[0].clone()
                })
            }
            _ => unreachable!(),
        };
        let colors: Vec<_> = pairs.iter().map(|(_, r)| band(r)).collect();
        let mut expected: Vec<_> = BANDS.iter().map(|(b, _)| Some(b.to_string())).collect();
        expected.push(expected[0].clone());
        assert_eq!(colors, expected, "six colors, then the first again");

        show_control(
            &pairs[1].1.widget,
            &pairs[1].0,
            Some(&Value::Integer(0)),
            true,
        );
        assert_eq!(band(&pairs[1].1), None, "a pair that is off has no band");
        assert_eq!(
            band(&pairs[2].1),
            expected[2],
            "the others keep their colors"
        );
        show_control(&pairs[1].1.widget, &pairs[1].0, None, true);
        assert_eq!(band(&pairs[1].1), None, "an unknown value has no band");
        show_control(
            &pairs[1].1.widget,
            &pairs[1].0,
            Some(&Value::Integer(60)),
            true,
        );
        assert_eq!(band(&pairs[1].1), expected[1]);

        let (_, r) = &pairs[0];
        let Widget::Toggled(switch, timeout, ..) = &r.widget else {
            unreachable!()
        };
        let bounds = |w: &gtk::Widget| w.compute_bounds(&window).unwrap();
        let title = |w: &gtk::Widget, text: &str| {
            let mut found = None;
            let mut stack = vec![w.clone()];
            while let Some(w) = stack.pop() {
                if w.downcast_ref::<gtk::Label>()
                    .is_some_and(|l| l.label() == text)
                {
                    found = Some(w.clone());
                }
                let mut child = w.first_child();
                while let Some(c) = child {
                    stack.push(c.clone());
                    child = c.next_sibling();
                }
            }
            bounds(&found.expect("the row shows its title"))
        };
        let parent = switch.row.as_ref().unwrap();
        assert_eq!(
            title(parent.upcast_ref(), &parent.title()).x(),
            title(timeout.upcast_ref(), &timeout.title()).x(),
            "the titles line up"
        );
        let toggle = bounds(switch.switch.upcast_ref());
        let marker = bounds(r.marker.button.upcast_ref());
        assert!(
            marker.x() >= toggle.x() + toggle.width(),
            "the saved-state menu follows the switch"
        );
        let unit = title(timeout.upcast_ref(), "s");
        assert_eq!(
            unit.x() + unit.width(),
            toggle.x() + toggle.width(),
            "the timeout's controls end where the switch ends"
        );
        let menus = || {
            let mut n = 0;
            let mut stack = vec![timeout.clone().upcast::<gtk::Widget>()];
            while let Some(w) = stack.pop() {
                n += usize::from(w.is::<gtk::MenuButton>());
                let mut child = w.first_child();
                while let Some(c) = child {
                    stack.push(c.clone());
                    child = c.next_sibling();
                }
            }
            n
        };
        assert_eq!(menus(), 0, "the timeout has no saved-state menu of its own");
        let dpi_spin = bounds(
            find::<gtk::SpinButton>(first.row.upcast_ref())
                .unwrap()
                .upcast_ref(),
        );
        let dpi_marker = bounds(first.marker.button.upcast_ref());
        assert!(dpi_marker.x() >= dpi_spin.x() + dpi_spin.width());
        assert_eq!(
            dpi_marker.x() + dpi_marker.width(),
            marker.x() + marker.width(),
            "saved-state menus end at the row's end"
        );
        window.destroy();
    }

    #[test]
    #[ignore = "requires a display"]
    fn unknown_choices_show_unknown_without_listing_it() {
        adw::init().expect("GTK needs a display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let field = Field::hidpp(Leaf::Dpi, Some(1));
        let values: Vec<Value> = [400, 800, 1200].map(Value::Integer).into();
        let names: Vec<String> = values
            .iter()
            .map(|v| unit_text(field.leaf, Some(v)))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let applied = Rc::new(RefCell::new(Vec::new()));
        let pick = Pick::new(
            "Pointer Sensitivity",
            &names,
            glib::clone!(
                #[strong]
                applied,
                #[strong]
                values,
                move |i| applied.borrow_mut().push(values[i])
            ),
        );
        let list = gtk::ListBox::new();
        list.append(&pick.row);
        let window = gtk::Window::new();
        window.set_child(Some(&list));
        window.present();
        pump(&|| pick.row.is_mapped());
        let widget = Widget::Choice(pick, values.clone());
        let Widget::Choice(pick, _) = &widget else {
            unreachable!()
        };
        let s = Setting {
            field,
            value: None,
            saved: None,
            status: None,
            limits: settings::Limits::Integer(None),
        };
        let show = |v: Option<i64>| show_control(&widget, &s, v.map(Value::Integer).as_ref(), true);
        let shown = || pick.button.label().map(|l| l.to_string());
        let marked = || pick.chosen.state().and_then(|s| s.get::<i32>());
        // Picks a choice as its menu item does.
        let choose = |i: i32| {
            pick.button
                .activate_action("pick.choice", Some(&i.to_variant()))
                .unwrap();
        };
        let menu = pick.button.menu_model().unwrap();
        let listed: Vec<String> = (0..menu.n_items())
            .filter_map(|i| {
                menu.item_attribute_value(i, "label", Some(glib::VariantTy::STRING))?
                    .get()
            })
            .collect();
        assert_eq!(listed, names, "the menu lists only the choices");

        show(None);
        assert_eq!(shown().as_deref(), Some("Unknown"));
        assert_eq!(marked(), Some(-1), "an unknown value marks no choice");
        // Activating the row opens the menu, as clicking it or pressing Enter does.
        ActionRowExt::activate(&pick.row);
        pump(&|| pick.button.is_active());
        pick.button.popdown();
        choose(0);
        assert_eq!(
            applied.take(),
            [Value::Integer(400)],
            "the first choice applies while the value is unknown"
        );

        show(Some(800));
        assert_eq!(shown().as_deref(), Some("800 DPI"));
        assert_eq!(marked(), Some(1));
        choose(1);
        assert!(
            applied.take().is_empty(),
            "picking the marked choice applies nothing"
        );
        choose(0);
        assert_eq!(applied.take(), [Value::Integer(400)]);
        show(Some(1000));
        assert_eq!(
            shown().as_deref(),
            Some("1000 DPI"),
            "a value that is none of the choices shows as it is"
        );
        assert_eq!(marked(), Some(-1));
        choose(0);
        assert_eq!(
            applied.take(),
            [Value::Integer(400)],
            "the first choice applies while the value is none of them"
        );
        show(None);
        assert_eq!(
            shown().as_deref(),
            Some("Unknown"),
            "a value can become unknown"
        );
        show(Some(1200));
        assert_eq!(marked(), Some(2));
        assert!(applied.take().is_empty(), "redraws apply nothing");
        window.destroy();
    }

    /// Runs the main loop until `done`, failing after five seconds.
    fn pump(done: &dyn Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out");
            glib::MainContext::default().iteration(false);
        }
    }

    /// Whether `w`'s tree has a label reading `text`.
    fn find_label(w: &gtk::Widget, text: &str) -> bool {
        if w.downcast_ref::<gtk::Label>()
            .is_some_and(|l| l.label() == text)
        {
            return true;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if find_label(&c, text) {
                return true;
            }
            child = c.next_sibling();
        }
        false
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
                range,
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
            show_number(&self.row, &self.state, n.map(Value::Integer).as_ref());
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

        /// Replaces the row's text, as selecting it and typing over it does.
        fn type_text(&self, typed: &str) {
            self.row.set_text("");
            self.type_keys(typed);
        }

        /// Types at the cursor one key at a time.
        fn type_keys(&self, keys: &str) {
            for key in keys.chars() {
                self.text
                    .emit_by_name::<()>("insert-at-cursor", &[&key.to_string()]);
            }
        }

        /// Pastes `pasted` at the cursor.
        fn paste(&self, pasted: &str) {
            let clipboard = self.text.clipboard();
            clipboard.set_text(pasted);
            let before = self.row.text();
            self.text.emit_by_name::<()>("paste-clipboard", &[]);
            // The paste reads the clipboard asynchronously; a rejected paste changes nothing.
            let context = glib::MainContext::default();
            for _ in 0..100 {
                if self.row.text() != before {
                    break;
                }
                context.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
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
            assert_eq!(self.row.text(), number_text(n));
        }
    }

    #[test]
    #[ignore = "requires a display"]
    fn integer_rows_take_only_numbers() {
        adw::init().expect("GTK needs a display");
        let r = Rig::new((0, 4000, 1));
        r.focus();
        assert!(
            find_label(r.row.upcast_ref(), "DPI"),
            "the unit shows beside the number"
        );
        r.show(None);
        assert_eq!(r.row.text(), "", "an unknown value shows no number");
        r.leave();
        r.press_enter();
        assert!(
            r.applied().is_empty(),
            "an empty row applies nothing, though GTK reads it as 0"
        );
        assert_eq!(r.row.text(), "");

        r.type_keys("30o");
        assert_eq!(
            r.row.text(),
            "30",
            "a letter typed after a number is left out"
        );
        r.press_enter();
        assert_eq!(r.applied(), [30]);
        r.show(Some(30));

        r.text.set_position(-1);
        r.paste("0x");
        assert_eq!(
            r.row.text(),
            "30",
            "pasted text that isn't a number is left out"
        );
        r.paste("0");
        assert_eq!(r.row.text(), "300", "pasted digits go in");
        r.leave();
        assert_eq!(r.applied(), [300]);
        r.shows(Some(300));

        r.type_text("abc");
        assert_eq!(r.row.text(), "");
        r.leave();
        assert!(r.applied().is_empty(), "emptying the row applies nothing");
        r.shows(Some(300));
        r.type_text("");
        r.show(None);
        r.show(Some(300));
        r.leave();
        assert!(r.applied().is_empty());
        r.shows(Some(300));
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

        r.enter("");
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
        r.enter("");
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
        r.enter("");
        assert!(
            r.applied().is_empty(),
            "leaving an off-step model value applies nothing"
        );
        r.shows(Some(803));
        r.enter("803");
        assert!(
            r.applied().is_empty(),
            "entering the value shown applies nothing"
        );
        r.enter("807");
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
        r.type_text("");
        r.show(Some(900));
        r.leave();
        assert!(r.applied().is_empty());
        r.shows(Some(900));
        assert_eq!(
            r.row.value(),
            900.0,
            "emptied text leaves the row at the model's value"
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
