//! Small widgets shared by the pages: buttons that remember their action, menus and groups of
//! read-only rows. Setters change a widget only when its value differs, so redraws
//! keep focus, selection and scroll positions.
use super::shell::Ctx;
use crate::ui::app::{Action, Tone};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::RefCell, rc::Rc};

/// How a piece of text is emphasized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    #[default]
    Plain,
    Dim,
    Error,
    Warning,
    Success,
    Accent,
}

impl From<Tone> for Look {
    fn from(tone: Tone) -> Self {
        match tone {
            Tone::Accent => Look::Accent,
            Tone::Dim => Look::Dim,
            Tone::Warning => Look::Warning,
            Tone::Success => Look::Success,
        }
    }
}

const LOOKS: [&str; 5] = ["dim-label", "error", "warning", "success", "accent"];

/// The bands that join a setting's row to the row of its dependent number, as style classes and
/// colors in the order a page gives them out. Teal and pink are GNOME's teal and pink accent
/// colors, which have no named color.
pub const BANDS: [(&str, &str); 6] = [
    ("band-blue", "@blue_3"),
    ("band-purple", "@purple_3"),
    ("band-teal", "#2190a4"),
    ("band-orange", "@orange_3"),
    ("band-green", "@green_4"),
    ("band-pink", "#d56199"),
];

/// The bands' style rules, with separators of the `separator` color. A band is 3 pixels of the
/// row's background at the list's left edge, clipped by the list's rounded corners. The row's
/// separator is a border image whose first 3 pixels are the band's, so the band runs unbroken
/// through its rows and into the next band.
fn bands(separator: &str) -> String {
    BANDS
        .iter()
        .map(|(class, color)| {
            format!(
                "row.{class} {{
                    background-image: linear-gradient(to right, {color} 3px, transparent 3px);
                    border-image: linear-gradient(to right, {color} 3px, {separator} 3px)
                        0 0 1 0 / 0 0 1 0;
                }}"
            )
        })
        .collect()
}

/// Adds the app's style rules to the display. Separators of the high-contrast style are in the
/// text color at half strength, as the style's own.
pub fn add_style() {
    // A dimmed fact already fades its name with the row, so the name skips the property style's
    // own dimming and stays as readable as other facts' names.
    //
    // Pairing shows the device's icon on an accent-tinted disc, with a round badge at its corner
    // filled with the step's result color. The badge's ring in the dialog's color separates it
    // from the disc. Named colors follow the light, dark and high-contrast styles.
    const CSS: &str = "
        row.property.dim-label > box.header > box.title > .title { opacity: 1; }
        .pairing-icon { border-radius: 9999px; background-color: alpha(@accent_color, 0.15); }
        .pairing-badge {
            border-radius: 9999px;
            background-color: @dialog_bg_color;
            box-shadow: 0 0 0 3px @dialog_bg_color;
        }
        .pairing-badge.success { background-color: @success_bg_color; color: @success_fg_color; }
        .pairing-badge.error { background-color: @error_bg_color; color: @error_fg_color; }
    ";
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!("{CSS}{}", bands("@card_shade_color")));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let high_contrast = gtk::CssProvider::new();
    high_contrast.load_from_string(&bands("alpha(currentColor, 0.5)"));
    let follow = move |style: &adw::StyleManager| {
        if style.is_high_contrast() {
            gtk::style_context_add_provider_for_display(
                &display,
                &high_contrast,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        } else {
            gtk::style_context_remove_provider_for_display(&display, &high_contrast);
        }
    };
    let style = adw::StyleManager::default();
    follow(&style);
    style.connect_high_contrast_notify(follow);
}

pub fn set_look(w: &impl IsA<gtk::Widget>, look: Look) {
    let class = match look {
        Look::Plain => None,
        Look::Dim => Some("dim-label"),
        Look::Error => Some("error"),
        Look::Warning => Some("warning"),
        Look::Success => Some("success"),
        Look::Accent => Some("accent"),
    };
    for c in LOOKS {
        if Some(c) != class && w.has_css_class(c) {
            w.remove_css_class(c);
        }
    }
    if let Some(c) = class
        && !w.has_css_class(c)
    {
        w.add_css_class(c);
    }
}

pub fn set_label(l: &gtk::Label, text: &str) {
    if l.label() != text {
        l.set_label(text);
    }
}

/// Sets a row's title as plain text, since names and messages may contain markup characters.
pub fn set_title(row: &impl IsA<adw::PreferencesRow>, text: &str) {
    if row.uses_markup() {
        row.set_use_markup(false);
    }
    if row.title() != text {
        row.set_title(text);
    }
}

pub fn set_subtitle(row: &impl IsA<adw::ActionRow>, text: &str) {
    let row = row.upcast_ref::<adw::ActionRow>();
    if row.uses_markup() {
        row.set_use_markup(false);
    }
    if row.subtitle().as_deref().unwrap_or("") != text {
        row.set_subtitle(text);
    }
}

pub fn spin(spinner: &gtk::Spinner, on: bool) {
    spinner.set_visible(on);
    if spinner.is_spinning() != on {
        spinner.set_spinning(on);
    }
}

pub fn spinner() -> gtk::Spinner {
    gtk::Spinner::builder().visible(false).build()
}

/// A wrapping label for sentences.
pub fn para() -> gtk::Label {
    gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .build()
}

/// Shows a label with this text, or hides it when there is none.
pub fn note(l: &gtk::Label, text: Option<&str>, look: Look) {
    l.set_visible(text.is_some_and(|t| !t.is_empty()));
    set_label(l, text.unwrap_or(""));
    set_look(l, look);
}

/// A button whose action is set at each redraw; it is unavailable without one.
#[derive(Clone)]
pub struct Btn {
    pub widget: gtk::Button,
    action: Rc<RefCell<Option<Action>>>,
}

impl Btn {
    pub fn new(ctx: &Rc<Ctx>, label: &str) -> Self {
        Self::wrap(ctx, gtk::Button::with_label(label))
    }

    pub fn suggested(ctx: &Rc<Ctx>, label: &str) -> Self {
        let b = Self::new(ctx, label);
        b.widget.add_css_class("suggested-action");
        b
    }

    pub fn destructive(ctx: &Rc<Ctx>, label: &str) -> Self {
        let b = Self::new(ctx, label);
        b.widget.add_css_class("destructive-action");
        b
    }

    /// An icon button, named by its tooltip.
    pub fn icon(ctx: &Rc<Ctx>, icon: &str, label: &str) -> Self {
        let b = Self::bare_icon(ctx, icon, label);
        b.widget.set_tooltip_text(Some(label));
        b
    }

    /// An icon button named for assistive technologies only, for an icon its place explains.
    pub fn bare_icon(ctx: &Rc<Ctx>, icon: &str, label: &str) -> Self {
        let button = gtk::Button::builder()
            .icon_name(icon)
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("flat");
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        Self::wrap(ctx, button)
    }

    fn wrap(ctx: &Rc<Ctx>, widget: gtk::Button) -> Self {
        let action: Rc<RefCell<Option<Action>>> = Rc::default();
        widget.connect_clicked(glib::clone!(
            #[weak]
            ctx,
            #[strong]
            action,
            move |_| {
                if let Some(a) = action.borrow().clone() {
                    ctx.act(a);
                }
            }
        ));
        Self { widget, action }
    }

    /// Shows the button, available when it has an action.
    pub fn set(&self, action: Option<Action>) {
        self.offer(true, action);
    }

    pub fn offer(&self, shown: bool, action: Option<Action>) {
        self.widget.set_visible(shown);
        self.widget.set_sensitive(action.is_some());
        *self.action.borrow_mut() = action;
    }

    pub fn hide(&self) {
        self.offer(false, None);
    }
}

/// An action from `enabled`.
pub fn when(enabled: bool, action: Action) -> Option<Action> {
    enabled.then_some(action)
}

/// A menu button whose items are set at each redraw; an item without an action is unavailable.
pub struct Menu {
    pub button: gtk::MenuButton,
    model: gio::Menu,
    group: gio::SimpleActionGroup,
    items: Rc<RefCell<Vec<Option<Action>>>>,
    labels: Vec<String>,
    ctx: Rc<Ctx>,
}

impl Menu {
    pub fn new(ctx: &Rc<Ctx>) -> Self {
        let model = gio::Menu::new();
        let group = gio::SimpleActionGroup::new();
        let button = gtk::MenuButton::builder()
            .menu_model(&model)
            .valign(gtk::Align::Center)
            .build();
        button.insert_action_group("item", Some(&group));
        Self {
            button,
            model,
            group,
            items: Rc::default(),
            labels: Vec::new(),
            ctx: ctx.clone(),
        }
    }

    /// Attaches the items to another widget, as a context menu does.
    pub fn attach(&self, widget: &impl IsA<gtk::Widget>) {
        widget.insert_action_group("item", Some(&self.group));
    }

    pub fn model(&self) -> &gio::Menu {
        &self.model
    }

    pub fn set(&mut self, items: &[(String, Option<Action>)]) {
        let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();
        if labels != self.labels {
            self.model.remove_all();
            for name in self.group.list_actions() {
                self.group.remove_action(&name);
            }
            for (i, label) in labels.iter().enumerate() {
                let name = i.to_string();
                let action = gio::SimpleAction::new(&name, None);
                let items = self.items.clone();
                let ctx = Rc::downgrade(&self.ctx);
                action.connect_activate(move |_, _| {
                    let chosen = items.borrow().get(i).cloned().flatten();
                    if let (Some(ctx), Some(a)) = (ctx.upgrade(), chosen) {
                        ctx.act(a);
                    }
                });
                self.group.add_action(&action);
                self.model
                    .append(Some(label), Some(&format!("item.{name}")));
            }
            self.labels = labels;
        }
        for (i, (_, action)) in items.iter().enumerate() {
            if let Some(a) = self.group.lookup_action(&i.to_string())
                && let Ok(a) = a.downcast::<gio::SimpleAction>()
            {
                a.set_enabled(action.is_some());
            }
        }
        *self.items.borrow_mut() = items.iter().map(|(_, a)| a.clone()).collect();
    }
}

/// One read-only row: a fact with its value under its name, a count above its bar, a sentence, or a
/// link to a page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub title: String,
    pub subtitle: String,
    pub value: String,
    pub look: Look,
    pub link: Option<Action>,
    /// A tab the link shows on its page.
    pub tab: Option<crate::ui::app::Tab>,
    pub icon: Option<&'static str>,
    /// A count and its limit, drawn as a bar under the fact.
    pub meter: Option<(u32, u32)>,
}

impl Item {
    pub fn fact(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            title: label.into(),
            value: value.into(),
            ..Self::default()
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self {
            title: text.into(),
            ..Self::default()
        }
    }

    pub fn link(title: impl Into<String>, subtitle: impl Into<String>, action: Action) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            link: Some(action),
            ..Self::default()
        }
    }

    pub fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn tab(mut self, tab: crate::ui::app::Tab) -> Self {
        self.tab = Some(tab);
        self
    }

    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Draws the count against its limit when both are known and the limit isn't zero.
    pub fn meter(mut self, count: Option<u32>, limit: Option<u32>) -> Self {
        self.meter = count.zip(limit).filter(|(_, limit)| *limit > 0);
        self
    }
}

struct ListRow {
    row: adw::ActionRow,
    icon: gtk::Image,
    value: gtk::Label,
    chevron: gtk::Image,
    link: Rc<RefCell<Vec<Action>>>,
}

/// A count against its limit: the name and the count on one line, above a bar across the row.
struct MeterRow {
    row: adw::PreferencesRow,
    title: gtk::Label,
    value: gtk::Label,
    bar: gtk::LevelBar,
}

impl MeterRow {
    fn new() -> Self {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        let value = gtk::Label::builder().xalign(1.0).build();
        value.add_css_class("numeric");
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.append(&title);
        line.append(&value);
        // Without offsets the bar keeps one fill color, since a count near its limit is not in
        // itself a problem.
        let bar = gtk::LevelBar::new();
        for offset in ["low", "high", "full"] {
            bar.remove_offset_value(Some(offset));
        }
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&line);
        content.append(&bar);
        let row = adw::PreferencesRow::builder()
            .activatable(false)
            .child(&content)
            .build();
        row.upcast_ref::<gtk::Widget>()
            .update_relation(&[gtk::accessible::Relation::LabelledBy(&[title.upcast_ref()])]);
        Self {
            row,
            title,
            value,
            bar,
        }
    }

    fn show(&self, item: &Item) {
        let Some((count, limit)) = item.meter else {
            return;
        };
        set_title(&self.row, &item.title);
        set_label(&self.title, &item.title);
        set_label(&self.value, &item.value);
        set_look(&self.value, item.look);
        // A reading that is only last known fades with its value.
        set_look(
            &self.bar,
            if item.look == Look::Dim {
                Look::Dim
            } else {
                Look::Plain
            },
        );
        // A count above its limit fills the bar, while the text keeps the count.
        let (count, limit) = (f64::from(count.min(limit)), f64::from(limit));
        if self.bar.max_value() != limit {
            self.bar.set_max_value(limit);
        }
        if self.bar.value() != count {
            self.bar.set_value(count);
        }
        // The bar reads as the fact's words, such as "4 of 7", rather than a bare number.
        self.bar.update_property(&[
            gtk::accessible::Property::Label(&item.title),
            gtk::accessible::Property::ValueText(&item.value),
        ]);
    }
}

enum Row {
    Plain(ListRow),
    Meter(MeterRow),
}

impl Row {
    fn widget(&self) -> &gtk::Widget {
        match self {
            Row::Plain(r) => r.row.upcast_ref(),
            Row::Meter(r) => r.row.upcast_ref(),
        }
    }
}

/// A group of read-only rows, hidden while empty. Rows are reused in place.
pub struct List {
    pub group: adw::PreferencesGroup,
    rows: Vec<Row>,
    /// Keeps the group shown while empty, for rows added to it before the list's.
    shown: bool,
    ctx: Rc<Ctx>,
}

impl List {
    pub fn new(ctx: &Rc<Ctx>, title: &str) -> Self {
        let group = adw::PreferencesGroup::builder().title(title).build();
        group.set_visible(false);
        Self {
            group,
            rows: Vec::new(),
            shown: false,
            ctx: ctx.clone(),
        }
    }

    /// Puts a row of its own first in the group, which then stays shown.
    pub fn lead(&mut self, row: &impl IsA<gtk::Widget>) {
        self.group.add(row);
        self.shown = true;
        self.group.set_visible(true);
    }

    pub fn set(&mut self, items: &[Item]) {
        // A group only appends, so a row that changes kind goes with the rows after it.
        let keep = self
            .rows
            .iter()
            .zip(items)
            .take_while(|(r, item)| matches!(r, Row::Meter(_)) == item.meter.is_some())
            .count();
        for r in self.rows.drain(keep..) {
            self.group.remove(r.widget());
        }
        for item in &items[keep..] {
            let row = match item.meter {
                Some(_) => Row::Meter(MeterRow::new()),
                None => Row::Plain(self.new_row()),
            };
            self.group.add(row.widget());
            self.rows.push(row);
        }
        for (r, item) in self.rows.iter().zip(items) {
            match r {
                Row::Plain(r) => show_item(r, item),
                Row::Meter(r) => r.show(item),
            }
        }
        self.group.set_visible(self.shown || !items.is_empty());
    }

    fn new_row(&self) -> ListRow {
        let row = adw::ActionRow::builder().title_lines(0).build();
        let icon = gtk::Image::builder()
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let value = gtk::Label::builder()
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(1.0)
            .justify(gtk::Justification::Right)
            .max_width_chars(36)
            .build();
        let chevron = gtk::Image::builder()
            .icon_name("go-next-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let link: Rc<RefCell<Vec<Action>>> = Rc::default();
        row.connect_activated(glib::clone!(
            #[weak(rename_to = ctx)]
            self.ctx,
            #[strong]
            link,
            move |_| {
                for a in link.borrow().iter() {
                    ctx.act(a.clone());
                }
            }
        ));
        ListRow {
            row,
            icon,
            value,
            chevron,
            link,
        }
    }
}

/// Puts a row's icon, value and chevron in it only while they are used. A row keeps its prefix
/// and suffix boxes, with their margins and spacing, while anything is in them, even hidden
/// widgets, so an unused one would indent the row's text.
fn attach(r: &ListRow, icon: bool, value: bool, chevron: bool) {
    let row = &r.row;
    if r.icon.parent().is_some() != icon {
        if icon {
            row.add_prefix(&r.icon);
        } else {
            row.remove(&r.icon);
        }
    }
    // Suffixes go back in order, so the value stays before the chevron.
    let suffixes = [
        (r.value.upcast_ref::<gtk::Widget>(), value),
        (r.chevron.upcast_ref::<gtk::Widget>(), chevron),
    ];
    if suffixes.iter().any(|(w, on)| w.parent().is_some() != *on) {
        for (w, _) in suffixes {
            if w.parent().is_some() {
                row.remove(w);
            }
        }
        for (w, on) in suffixes {
            if on {
                row.add_suffix(w);
            }
        }
    }
}

fn show_item(r: &ListRow, item: &Item) {
    // A fact is a property row: a small name above its value, as in GNOME Settings.
    let fact = item.link.is_none() && !item.value.is_empty();
    if fact {
        r.row.add_css_class("property");
    } else {
        r.row.remove_css_class("property");
    }
    if r.row.is_subtitle_selectable() != fact {
        r.row.set_subtitle_selectable(fact);
    }
    set_title(&r.row, &item.title);
    set_subtitle(&r.row, if fact { &item.value } else { &item.subtitle });
    let value = if fact { "" } else { item.value.as_str() };
    set_label(&r.value, value);
    attach(
        r,
        item.icon.is_some(),
        !value.is_empty(),
        item.link.is_some(),
    );
    // Short values stay on one line; long ones wrap rather than widen the row.
    r.value.set_wrap(value.chars().count() > 24);
    // A link's value carries its emphasis; a fact, sentence or link without one is emphasized
    // whole.
    if value.is_empty() {
        set_look(&r.row, item.look);
        set_look(&r.value, Look::Plain);
    } else {
        set_look(&r.row, Look::Plain);
        set_look(
            &r.value,
            if item.look == Look::Plain {
                Look::Dim
            } else {
                item.look
            },
        );
    }
    if let Some(name) = item.icon {
        r.icon.set_icon_name(Some(name));
    }
    r.row.set_activatable(item.link.is_some());
    *r.link.borrow_mut() = item
        .link
        .iter()
        .cloned()
        .chain(item.tab.map(Action::Tab))
        .collect();
}

/// A page's content: scrolling groups, or another widget in their place.
pub struct Tab {
    pub widget: adw::ToolbarView,
    pub page: adw::PreferencesPage,
}

impl Tab {
    pub fn new() -> Self {
        let page = adw::PreferencesPage::new();
        let widget = adw::ToolbarView::builder().content(&page).build();
        Self { widget, page }
    }

    pub fn add(&self, group: &adw::PreferencesGroup) {
        self.page.add(group);
    }

    /// Shows the groups, or another widget in their place.
    pub fn show(&self, other: Option<&gtk::Widget>) {
        let want: gtk::Widget = other.cloned().unwrap_or_else(|| self.page.clone().upcast());
        if self.widget.content().as_ref() != Some(&want) {
            self.widget.set_content(Some(&want));
        }
    }
}

/// A switch row that reports each change the user makes.
pub struct Toggle {
    pub row: adw::SwitchRow,
}

impl Toggle {
    pub fn new(title: &str, on_change: impl Fn(bool) + 'static) -> Self {
        let row = adw::SwitchRow::builder().title(title).build();
        row.connect_active_notify(move |r| on_change(r.is_active()));
        Self { row }
    }

    pub fn show(&self, on: bool, enabled: bool) {
        if self.row.is_active() != on {
            self.row.set_active(on);
        }
        self.row.set_sensitive(enabled);
    }
}

/// A status page shown in place of a page's groups, with an optional progress bar.
pub struct Status {
    pub widget: adw::StatusPage,
    pub progress: gtk::ProgressBar,
    /// Below the progress bar.
    pub body: gtk::Box,
}

impl Status {
    pub fn new(icon: &str) -> Self {
        let progress = gtk::ProgressBar::builder()
            .show_text(true)
            .halign(gtk::Align::Center)
            .width_request(240)
            .visible(false)
            .build();
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        body.append(&progress);
        let widget = adw::StatusPage::builder()
            .icon_name(icon)
            .child(&adw::Clamp::builder().child(&body).build())
            .vexpand(true)
            .build();
        Self {
            widget,
            progress,
            body,
        }
    }

    pub fn show(&self, title: &str, progress: Option<(u64, u64)>) {
        if self.widget.title() != title {
            self.widget.set_title(title);
        }
        match progress {
            Some((done, total)) => {
                let fraction = (done as f64 / total.max(1) as f64).clamp(0.0, 1.0);
                self.progress.set_fraction(fraction);
                let percent = (done.saturating_mul(100) / total.max(1)).min(100);
                self.progress.set_text(Some(&format!("{percent}%")));
                self.progress.set_visible(true);
            }
            None => self.progress.set_visible(false),
        }
    }
}

/// Icons for the kinds of device.
pub fn device_icon(kind: crate::ui::app::words::DisplayKind) -> &'static str {
    use crate::ui::app::words::DisplayKind::*;
    match kind {
        Keyboard | KeyboardMouse => "input-keyboard-symbolic",
        Mouse => "input-mouse-symbolic",
        Gamepad => "input-gaming-symbolic",
        Other => "bluetooth-symbolic",
    }
}

pub const ADAPTER_ICON: &str = "drive-removable-media-symbolic";
