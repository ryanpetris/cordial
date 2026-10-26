//! The sidebar: the Overview and Settings, then each adapter followed by its saved devices, each
//! on one line with its state shown by its icon. Separators set the adapters apart. An adapter's
//! row has a context menu to connect, disconnect or rename it.
//!
//! Search opens from the header's start, or with Ctrl+F or typing while the sidebar has focus,
//! and its results take the list's place while it has text. A result opens its page and shows
//! its row there.
use super::{
    search::{self, Hit},
    shell::{Ctx, Ev},
    widgets::{self, ADAPTER_ICON, Look, Menu, set_label, set_look},
};
use crate::ui::app::{
    Action, App, Live, Page,
    words::Battery,
    world::{Conn, DeviceView},
};
use adw::prelude::*;
use gtk::glib;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Key {
    Overview,
    Settings,
    Page(Page),
    Add(String),
    Connect(String),
    Refresh,
}

/// How a device or adapter row's icon shows its state.
#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    /// Connected, ready, or a row without a state.
    On,
    /// Between states, such as connecting, starting or migrating.
    Busy,
    /// Disconnected or inactive.
    Off,
    /// Disabled or blocked, so it doesn't reconnect.
    Unavailable,
    /// A read failure or adapter condition needs attention.
    Attention,
}

/// The state for a device or adapter status word.
fn state(status: &str, attention: bool) -> State {
    match status {
        _ if attention => State::Attention,
        "Couldn't Read" => State::Attention,
        "Disconnected" | "Inactive" => State::Off,
        "Disabled" | "Blocked" => State::Unavailable,
        "Connecting…" | "Disconnecting…" | "Starting…" | "Migrating…" | "Waiting for Adapter" => {
            State::Busy
        }
        _ => State::On,
    }
}

/// What activating a row does.
#[derive(Clone)]
enum Go {
    Act(Action),
    Settings,
}

/// What one row shows.
struct Spec {
    key: Key,
    icon: &'static str,
    title: String,
    /// The row's description for assistive technologies, such as its status or adapter name.
    status: String,
    state: State,
    /// A battery glyph and the reading it stands for.
    battery: Option<(Glyph, String)>,
    look: Look,
    /// The row follows a separator.
    group: bool,
    go: Option<Go>,
    menu: Vec<(String, Option<Action>)>,
}

struct Row {
    row: gtk::ListBoxRow,
    icon: gtk::Image,
    title: gtk::Label,
    spinner: gtk::Spinner,
    badge: gtk::Image,
    battery: gtk::DrawingArea,
    glyph: Rc<Cell<Glyph>>,
    menu: Option<(Menu, gtk::PopoverMenu)>,
}

impl Drop for Row {
    fn drop(&mut self) {
        if let Some((_, popover)) = &self.menu {
            popover.unparent();
        }
    }
}

/// The search's results, from the model as of the last redraw.
struct Found {
    /// The list, the results, or the page saying there are none.
    stack: gtk::Stack,
    results: gtk::ListBox,
    /// Everything search can find.
    index: RefCell<Vec<Hit>>,
    /// The results shown, in row order.
    hits: Rc<RefCell<Vec<Hit>>>,
}

impl Found {
    /// Shows the results for `query` in the list's place, or the list without any.
    fn show(&self, query: &str) {
        let hits = search::find(&self.index.borrow(), query);
        let shown = if query.trim().is_empty() {
            "list"
        } else if hits.is_empty() {
            "none"
        } else {
            "results"
        };
        self.stack.set_visible_child_name(shown);
        if hits != *self.hits.borrow() {
            self.results.remove_all();
            for h in &hits {
                self.results.append(&result_row(h));
            }
            *self.hits.borrow_mut() = hits;
        }
    }
}

pub struct Sidebar {
    pub page: adw::NavigationPage,
    pub search: gtk::SearchBar,
    pub entry: gtk::SearchEntry,
    list: gtk::ListBox,
    found: Rc<Found>,
    rows: HashMap<Key, Row>,
    order: Vec<Key>,
    /// What each row does, in row order.
    actions: Rc<RefCell<Vec<Option<Go>>>>,
    /// Which rows follow a separator, in row order.
    groups: Rc<RefCell<Vec<bool>>>,
    ctx: Rc<Ctx>,
}

impl Sidebar {
    pub fn new(ctx: &Rc<Ctx>, split: &adw::NavigationSplitView) -> Self {
        let header = adw::HeaderBar::new();
        let menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .menu_model(&main_menu())
            .primary(true)
            .build();
        menu.update_property(&[gtk::accessible::Property::Label("Main Menu")]);
        header.pack_end(&menu);
        let toggle = gtk::ToggleButton::builder()
            .icon_name("edit-find-symbolic")
            .tooltip_text("Search")
            .build();
        toggle.update_property(&[gtk::accessible::Property::Label("Search")]);
        header.pack_start(&toggle);
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Search")
            .hexpand(true)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label("Search")]);
        let search = gtk::SearchBar::builder().child(&entry).build();
        search.connect_entry(&entry);
        toggle
            .bind_property("active", &search, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .build();
        let actions: Rc<RefCell<Vec<Option<Go>>>> = Rc::default();
        list.connect_row_activated(glib::clone!(
            #[weak]
            ctx,
            #[weak]
            split,
            #[strong]
            actions,
            move |_, row| {
                let go = usize::try_from(row.index())
                    .ok()
                    .and_then(|i| actions.borrow().get(i).cloned().flatten());
                match go {
                    Some(Go::Settings) => {
                        split.set_show_content(true);
                        ctx.send(Ev::Preferences);
                    }
                    Some(Go::Act(action)) => {
                        if matches!(action, Action::Open(_)) {
                            split.set_show_content(true);
                        }
                        ctx.act(action);
                    }
                    None => {}
                }
            }
        ));
        let groups: Rc<RefCell<Vec<bool>>> = Rc::default();
        list.set_header_func({
            let groups = groups.clone();
            move |row, _| {
                let group = usize::try_from(row.index())
                    .ok()
                    .and_then(|i| groups.borrow().get(i).copied())
                    .unwrap_or(false);
                if group != row.header().is_some() {
                    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
                    row.set_header(group.then_some(&separator));
                }
            }
        });
        let results = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .build();
        results.update_property(&[gtk::accessible::Property::Label("Search Results")]);
        let hits: Rc<RefCell<Vec<Hit>>> = Rc::default();
        results.connect_row_activated(glib::clone!(
            #[weak]
            ctx,
            #[weak]
            search,
            #[strong]
            hits,
            move |_, row| {
                let hit = usize::try_from(row.index())
                    .ok()
                    .and_then(|i| hits.borrow().get(i).cloned());
                if let Some(hit) = hit {
                    search.set_search_mode(false);
                    ctx.send(Ev::Find(hit.target));
                }
            }
        ));
        let scrolled = |child: &gtk::ListBox| {
            gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(child)
                .build()
        };
        let stack = gtk::Stack::new();
        stack.add_named(&scrolled(&list), Some("list"));
        stack.add_named(&scrolled(&results), Some("results"));
        stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("edit-find-symbolic")
                .title("No Results Found")
                .description("Try a different search")
                .css_classes(["compact"])
                .build(),
            Some("none"),
        );
        stack.set_visible_child_name("list");
        let found = Rc::new(Found {
            stack: stack.clone(),
            results,
            index: RefCell::default(),
            hits,
        });
        entry.connect_search_changed(glib::clone!(
            #[strong]
            found,
            move |entry| found.show(&entry.text())
        ));
        // Enter and Down catch the results up with text typed since the entry last reported it.
        entry.connect_activate(glib::clone!(
            #[strong]
            found,
            move |entry| {
                found.show(&entry.text());
                if let Some(row) = found.results.row_at_index(0) {
                    row.activate();
                }
            }
        ));
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            entry,
            #[strong]
            found,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key != gtk::gdk::Key::Down {
                    return glib::Propagation::Proceed;
                }
                found.show(&entry.text());
                match found.results.row_at_index(0) {
                    Some(row) => {
                        row.grab_focus();
                        glib::Propagation::Stop
                    }
                    None => glib::Propagation::Proceed,
                }
            }
        ));
        entry.add_controller(keys);
        entry.connect_stop_search(glib::clone!(
            #[weak]
            list,
            move |_| {
                if let Some(row) = list.selected_row() {
                    row.grab_focus();
                }
            }
        ));
        search.connect_search_mode_enabled_notify(glib::clone!(
            #[weak]
            entry,
            #[weak]
            stack,
            move |search| {
                if !search.is_search_mode() {
                    entry.set_text("");
                    stack.set_visible_child_name("list");
                }
            }
        ));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&search);
        toolbar.set_content(Some(&stack));
        search.set_key_capture_widget(Some(&toolbar));
        Self {
            page: adw::NavigationPage::new(&toolbar, "Cordial"),
            search,
            entry,
            list,
            found,
            rows: HashMap::new(),
            order: Vec::new(),
            actions,
            groups,
            ctx: ctx.clone(),
        }
    }

    /// Shows the model, with the Settings row selected while `settings` is on screen.
    pub fn update(&mut self, app: &App<Live>, settings: bool) {
        *self.found.index.borrow_mut() = search::index(app);
        self.found.show(&self.entry.text());
        let specs = specs(app);
        let order: Vec<Key> = specs.iter().map(|s| s.key.clone()).collect();
        let groups: Vec<bool> = specs.iter().map(|s| s.group).collect();
        if groups != *self.groups.borrow() {
            *self.groups.borrow_mut() = groups;
            self.list.invalidate_headers();
        }
        if order != self.order {
            self.list.remove_all();
            self.rows.retain(|k, _| order.contains(k));
            for spec in &specs {
                if !self.rows.contains_key(&spec.key) {
                    let row = self.new_row(spec);
                    self.rows.insert(spec.key.clone(), row);
                }
                self.list.append(&self.rows[&spec.key].row);
            }
            self.order = order;
        }
        for spec in &specs {
            if let Some(row) = self.rows.get_mut(&spec.key) {
                show(row, spec);
            }
        }
        *self.actions.borrow_mut() = specs.iter().map(|s| s.go.clone()).collect();
        let shown = match app.shown() {
            _ if settings => Key::Settings,
            Page::Overview => Key::Overview,
            page => Key::Page(page),
        };
        match self.rows.get(&shown) {
            Some(row) if !row.row.is_selected() => self.list.select_row(Some(&row.row)),
            Some(_) => {}
            None => self.list.unselect_all(),
        }
    }

    fn new_row(&self, spec: &Spec) -> Row {
        let icon = gtk::Image::builder()
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let spinner = widgets::spinner();
        spinner.set_accessible_role(gtk::AccessibleRole::Presentation);
        let badge = gtk::Image::builder()
            .accessible_role(gtk::AccessibleRole::Presentation)
            .visible(false)
            .build();
        let glyph: Rc<Cell<Glyph>> = Rc::default();
        let battery = battery_glyph(&glyph);
        // The insets of GNOME Settings' sidebar rows.
        let content = gtk::Box::builder()
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(6)
            .margin_end(6)
            .build();
        content.append(&icon);
        content.append(&title);
        for indicator in [
            spinner.upcast_ref::<gtk::Widget>(),
            badge.upcast_ref(),
            battery.upcast_ref(),
        ] {
            let slot = gtk::CenterBox::builder()
                .width_request(22)
                .center_widget(indicator)
                .build();
            indicator
                .bind_property("visible", &slot, "visible")
                .sync_create()
                .build();
            content.append(&slot);
        }
        let row = gtk::ListBoxRow::builder().child(&content).build();
        let menu = matches!(&spec.key, Key::Page(Page::Adapter(_))).then(|| {
            let menu = Menu::new(&self.ctx);
            menu.attach(&row);
            let popover = gtk::PopoverMenu::from_model(Some(menu.model()));
            popover.set_parent(&row);
            popover.set_has_arrow(false);
            let click = gtk::GestureClick::builder().button(3).build();
            click.connect_pressed(glib::clone!(
                #[weak]
                popover,
                move |_, _, x, y| {
                    popover
                        .set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                    popover.popup();
                }
            ));
            row.add_controller(click);
            let press = gtk::GestureLongPress::new();
            press.connect_pressed(glib::clone!(
                #[weak]
                popover,
                move |_, _, _| popover.popup()
            ));
            row.add_controller(press);
            (menu, popover)
        });
        Row {
            row,
            icon,
            title,
            spinner,
            badge,
            battery,
            glyph,
            menu,
        }
    }
}

/// A result's row, laid out as GNOME Settings lays out its search results: the icon beside the
/// title, and where the result is below the title, both wrapping.
fn result_row(h: &Hit) -> gtk::ListBoxRow {
    let icon = gtk::Image::builder()
        .icon_name(h.icon)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    let label = |text: &str, chars| {
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .hexpand(true)
            .max_width_chars(chars)
            .wrap(true)
            .build()
    };
    let content = gtk::Grid::builder()
        .hexpand(true)
        .column_spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(6)
        .margin_end(6)
        .build();
    content.attach(&icon, 0, 0, 1, 1);
    content.attach(&label(&h.title, 40), 1, 0, 1, 1);
    if !h.context.is_empty() {
        let context = label(&h.context, 25);
        context.add_css_class("dim-label");
        content.attach(&context, 1, 1, 1, 1);
    }
    let row = gtk::ListBoxRow::builder().child(&content).build();
    row.update_property(&[
        gtk::accessible::Property::Label(&h.title),
        gtk::accessible::Property::Description(&h.context),
    ]);
    row
}

fn show(row: &mut Row, spec: &Spec) {
    row.icon.set_icon_name(Some(spec.icon));
    set_label(&row.title, &spec.title);
    let (look, badge) = match spec.state {
        State::On => (Look::Plain, None),
        State::Busy => (Look::Dim, None),
        State::Off => (Look::Dim, Some("network-offline-symbolic")),
        State::Unavailable => (Look::Dim, Some("action-unavailable-symbolic")),
        State::Attention => (Look::Warning, Some("dialog-warning-symbolic")),
    };
    set_look(&row.icon, look);
    widgets::spin(&row.spinner, spec.state == State::Busy);
    row.badge.set_visible(badge.is_some());
    if let Some(name) = badge {
        row.badge.set_icon_name(Some(name));
        set_look(&row.badge, look);
    }
    row.battery.set_visible(spec.battery.is_some());
    if let Some((glyph, label)) = &spec.battery {
        if row.glyph.replace(*glyph) != *glyph {
            row.battery.queue_draw();
        }
        row.battery
            .update_property(&[gtk::accessible::Property::Label(label)]);
        set_look(&row.battery, spec.look);
    }
    if let Some((menu, _)) = &mut row.menu {
        menu.set(&spec.menu);
    }
    row.row.update_property(&[
        gtk::accessible::Property::Label(&spec.title),
        gtk::accessible::Property::Description(&spec.status),
    ]);
}

/// What a battery glyph shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Glyph {
    /// The charge from 0 to 100, when known.
    percent: Option<i64>,
    charging: bool,
}

/// A battery's glyph and its reading in words; None when nothing is known.
fn battery(b: &Battery) -> Option<(Glyph, String)> {
    let text = b.text()?;
    let glyph = Glyph {
        percent: b.percent.map(|p| p.clamp(0, 100)),
        charging: b.charging == Some(true),
    };
    Some((glyph, format!("Battery {}", text.replace(" · ", ", "))))
}

/// The bolt's yellow, which stays the same in every style.
const BOLT: (f64, f64, f64) = (246.0 / 255.0, 211.0 / 255.0, 45.0 / 255.0);

/// A battery outline filled to the charge, with a bolt while charging. The outline and charge
/// take the widget's color, so the row's look recolors them.
fn battery_glyph(glyph: &Rc<Cell<Glyph>>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(22)
        .content_height(12)
        .valign(gtk::Align::Center)
        .accessible_role(gtk::AccessibleRole::Img)
        .visible(false)
        .build();
    let glyph = glyph.clone();
    area.set_draw_func(move |area, cr, _, _| {
        let _ = draw_battery(cr, &area.color(), glyph.get());
    });
    area
}

fn draw_battery(
    cr: &gtk::cairo::Context,
    color: &gtk::gdk::RGBA,
    glyph: Glyph,
) -> Result<(), gtk::cairo::Error> {
    cr.set_source_color(color);
    rounded(cr, 0.75, 0.75, 17.5, 10.5, 2.5);
    cr.set_line_width(1.5);
    cr.stroke()?;
    rounded(cr, 19.3, 3.5, 2.0, 5.0, 1.0);
    if let Some(p) = glyph.percent {
        rounded(cr, 3.0, 3.0, (13.0 * p as f64 / 100.0).max(1.5), 6.0, 1.0);
    }
    cr.fill()?;
    if glyph.charging {
        cr.move_to(10.5, 1.8);
        for (x, y) in [
            (7.0, 6.6),
            (10.0, 6.6),
            (8.5, 10.2),
            (12.4, 5.2),
            (9.4, 5.2),
        ] {
            cr.line_to(x, y);
        }
        cr.close_path();
        cr.set_source_rgb(BOLT.0, BOLT.1, BOLT.2);
        cr.fill_preserve()?;
        // A clear edge sets the bolt apart from the outline and charge beneath it.
        cr.set_operator(gtk::cairo::Operator::Clear);
        cr.set_line_width(0.6);
        cr.stroke()?;
    }
    Ok(())
}

/// Adds a rectangle with corners rounded as an SVG `rect` rounds them.
fn rounded(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, -FRAC_PI_2);
    cr.close_path();
}

fn specs(app: &App<Live>) -> Vec<Spec> {
    let spec = |key: Key, icon, title: &str, go: Option<Go>| Spec {
        key,
        icon,
        title: title.to_owned(),
        status: String::new(),
        state: State::On,
        battery: None,
        look: Look::Plain,
        group: false,
        go,
        menu: Vec::new(),
    };
    let act = |action| Some(Go::Act(action));
    let device = |d: &DeviceView| {
        let mut s = spec(
            Key::Page(d.page()),
            widgets::device_icon(d.kind),
            &d.name,
            act(Action::Open(d.page())),
        );
        s.status = d.status().to_owned();
        s.state = state(&s.status, false);
        if let Some(b) = &d.battery {
            s.battery = battery(b);
            s.look = match () {
                _ if b.low() => Look::Error,
                _ if b.stale() => Look::Dim,
                _ => Look::Plain,
            };
        }
        s
    };
    let mut out = vec![
        spec(
            Key::Overview,
            "go-home-symbolic",
            "Overview",
            act(Action::Open(Page::Overview)),
        ),
        spec(
            Key::Settings,
            "preferences-system-symbolic",
            "Settings",
            Some(Go::Settings),
        ),
    ];
    let ready = app.ready_adapters();
    for a in app.adapters() {
        let page = Page::Adapter(a.id.clone());
        let mut s = spec(
            Key::Page(page.clone()),
            ADAPTER_ICON,
            &a.name,
            act(Action::Open(page)),
        );
        let (status, attention) = a.status_text();
        s.status = status.to_owned();
        s.state = state(status, attention);
        s.group = true;
        let toggle = if a.connected() {
            (
                "Disconnect".to_owned(),
                Some(Action::DisconnectAdapter(a.id.clone())),
            )
        } else {
            let can = a.conn == Conn::Disconnected;
            (
                "Connect".to_owned(),
                can.then(|| Action::ConnectAdapter(a.id.clone())),
            )
        };
        s.menu = vec![
            toggle,
            (
                "Rename".to_owned(),
                a.live().map(|_| Action::RenameAdapter(a.id.clone())),
            ),
        ];
        out.push(s);
        let before = out.len();
        out.extend(
            app.devices()
                .iter()
                .filter(|d| d.adapter == a.id)
                .map(device),
        );
        // A device is added from its adapter's page, or from here while the adapter has none.
        // The row's description names the adapter.
        if out.len() == before && app.can_add_to(&a.id) {
            let mut add = spec(
                Key::Add(a.id.clone()),
                "list-add-symbolic",
                "Add Device",
                act(Action::AddDeviceTo(a.id.clone())),
            );
            add.status = a.name.clone();
            out.push(add);
        }
        // Without a ready adapter, one that can connect offers it here too.
        if ready.is_empty() && a.conn == Conn::Disconnected && a.hello.is_none() {
            out.push(spec(
                Key::Connect(a.id.clone()),
                "network-wired-symbolic",
                &format!("Connect {}", a.name),
                act(Action::ConnectAdapter(a.id.clone())),
            ));
        }
    }
    // Devices whose adapter isn't listed follow the adapters.
    let mut stray = app
        .devices()
        .iter()
        .filter(|d| app.adapter(&d.adapter).is_none())
        .map(device)
        .peekable();
    if let Some(first) = stray.peek_mut() {
        first.group = true;
    }
    out.extend(stray);
    if app.adapters().is_empty() {
        let mut s = spec(
            Key::Refresh,
            "view-refresh-symbolic",
            "Refresh Adapters",
            act(Action::RefreshAdapters),
        );
        s.group = true;
        out.push(s);
    }
    out
}

fn main_menu() -> gtk::gio::Menu {
    let adapters = gtk::gio::Menu::new();
    adapters.append(Some("Refresh Adapters"), Some("app.refresh"));
    let app = gtk::gio::Menu::new();
    app.append(Some("About Cordial"), Some("app.about"));
    app.append(Some("Quit"), Some("app.quit"));
    let menu = gtk::gio::Menu::new();
    menu.append_section(None, &adapters);
    menu.append_section(None, &app);
    menu
}
