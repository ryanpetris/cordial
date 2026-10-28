//! A saved device's page: its battery, connection switches, settings and details, with Profiles
//! and Diagnostics opening from it. Switches, settings and Profiles layers apply each change as it
//! is made; Forget asks first. Refresh, Connect and Disconnect are in the page's menu. Warnings,
//! identifiers and connection errors belong to Diagnostics.
use super::{
    profiles::Layers,
    settings::Settings,
    shell::Ctx,
    widgets::{Btn, Item, List, Look, Menu, Tab as TabView, Toggle, when},
    window::{Head, Tabs, View},
};
use crate::{
    commands, model,
    ui::{
        app::{
            Action, App, Kind, Live, Spot, Tab, Toggle as Switch,
            device::{DETAILS, facts},
            words,
            world::{DeviceView, has_settings},
        },
        text,
    },
};
use adw::prelude::*;
use cordial_protocol::{DeviceState, InactiveReason};
use std::rc::Rc;

const SERIAL: &str = "Serial Number";

/// The Connection group's switches.
pub const SWITCHES: [(&str, Switch); 4] = [
    ("Use This Device", Switch::Enabled),
    ("Automatic Connections", Switch::Trusted),
    ("Logitech Features", Switch::Hidpp),
    ("Block Connections", Switch::Blocked),
];

pub struct DevicePage {
    adapter: String,
    id: u32,
    tabs: Tabs,
    details: Details,
    settings: Settings,
    layers: Layers,
    diagnostics: Diagnostics,
}

impl DevicePage {
    pub fn new(ctx: &Rc<Ctx>, adapter: &str, id: u32) -> Self {
        let mut tabs = Tabs::new(ctx);
        let tab = TabView::new();
        let details = Details::new(ctx, &tab);
        let tail = vec![
            details.info.group.clone(),
            details.pages.group.clone(),
            details.end.clone(),
        ];
        let settings = Settings::new(ctx, &tab, tail);
        let layers = Layers::new(ctx);
        let diagnostics = Diagnostics::new(ctx);
        tabs.add(Tab::Details, &tab.widget);
        tabs.add(Tab::Profiles, &layers.tab.widget);
        tabs.add(Tab::Diagnostics, &diagnostics.tab.widget);
        tabs.pack_end(Tab::Details, &details.menu.button);
        tabs.pack_end(Tab::Diagnostics, &diagnostics.menu.button);
        Self {
            adapter: adapter.to_owned(),
            id,
            tabs,
            details,
            settings,
            layers,
            diagnostics,
        }
    }
}

impl View for DevicePage {
    fn tabs(&self) -> &Tabs {
        &self.tabs
    }

    fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>) -> Option<Head> {
        let d = app.device(&self.adapter, self.id)?.clone();
        let mut offered = vec![Tab::Details];
        let settings =
            has_settings(app.state_of(&self.adapter), &d.d) || !app.gamepad_choices(&d).is_empty();
        let profiles = app
            .adapter(&self.adapter)
            .is_some_and(|a| a.profile_support())
            && d.d.profiles.is_some();
        if profiles {
            offered.push(Tab::Profiles);
        }
        offered.push(Tab::Diagnostics);
        self.tabs.show(&offered, app.tab());
        let link = link(app, &d);
        self.details.update(app, &d, profiles, settings, &link);
        self.settings.update(ctx, app, &d, settings);
        if profiles {
            self.layers.update(app, &d);
        }
        self.diagnostics.update(app, &d, &link);
        Some(Head {
            title: d.name.clone(),
            subtitle: d.status().to_owned(),
        })
    }
}

/// A menu in a page's header.
fn page_menu(ctx: &Rc<Ctx>) -> Menu {
    let menu = Menu::new(ctx);
    menu.button.set_icon_name("view-more-symbolic");
    menu.button.set_tooltip_text(Some("Device Menu"));
    menu.button
        .update_property(&[gtk::accessible::Property::Label("Device Menu")]);
    menu
}

/// Disconnect while the device is connected or connecting, otherwise Connect.
fn link(app: &App<Live>, d: &DeviceView) -> (String, Option<Action>) {
    let ready = app.adapter_ready(d);
    let busy = app.device_busy(d);
    let switching = app.running(
        &d.adapter,
        |k| matches!(k, Kind::Device(device, Spot::DeviceBar(..)) if *device == d.d.id),
    );
    let connecting = d.d.state() == DeviceState::Connecting || (switching && !d.connected());
    if d.connected() || connecting {
        (
            "Disconnect".into(),
            when(!busy && ready, Action::Disconnect),
        )
    } else {
        // A record that is only last known leaves Connect to the adapter.
        let can_connect = d.read_error.is_some()
            || (model::inactive(&d.d).is_none() && d.d.state() == DeviceState::Disconnected);
        (
            "Connect".into(),
            when(ready && !busy && can_connect, Action::Connect),
        )
    }
}

struct Details {
    /// Why the last Connect or Disconnect failed, and Battery.
    top: List,
    /// The switches, and why the last change to them couldn't be saved.
    connection: List,
    toggles: Vec<(Switch, Toggle)>,
    info: List,
    pages: List,
    /// The group holding Forget.
    end: adw::PreferencesGroup,
    forget: Btn,
    /// Refresh for the settings, then Connect or Disconnect.
    menu: Menu,
}

impl Details {
    fn new(ctx: &Rc<Ctx>, tab: &TabView) -> Self {
        let top = List::new(ctx, "");
        tab.add(&top.group);
        let mut connection = List::new(ctx, "Connection");
        let mut toggles = Vec::new();
        for (label, switch) in SWITCHES {
            let ctx = Rc::downgrade(ctx);
            let toggle = Toggle::new(label, move |on| {
                if let Some(ctx) = ctx.upgrade() {
                    ctx.act(Action::DeviceToggle(switch, on));
                }
            });
            connection.lead(&toggle.row);
            toggles.push((switch, toggle));
        }
        tab.add(&connection.group);
        let info = List::new(ctx, "Information");
        tab.add(&info.group);
        let pages = List::new(ctx, "");
        tab.add(&pages.group);
        let forget = Btn::destructive(ctx, "Forget Device…");
        forget.widget.set_halign(gtk::Align::Center);
        let end = adw::PreferencesGroup::new();
        end.add(&forget.widget);
        tab.add(&end);
        Self {
            top,
            connection,
            forget,
            menu: page_menu(ctx),
            toggles,
            info,
            pages,
            end,
        }
    }

    fn update(
        &mut self,
        app: &App<Live>,
        d: &DeviceView,
        profiles: bool,
        settings: bool,
        link: &(String, Option<Action>),
    ) {
        // A change being applied shows until its result.
        let values = app.device_values(d);
        let editable = app.device_editable(d);
        let enabled = values.enabled.unwrap_or(d.d.enabled);
        let blocked = values.blocked.unwrap_or(d.d.blocked);
        // Turning the device on or unblocking it isn't offered while the adapter would refuse
        // it for want of a place for its transport.
        let refused = |enabled: bool, blocked: bool| {
            app.state_of(&d.adapter)
                .is_some_and(|st| commands::capacity_refused(st, &d.d, enabled, blocked))
        };
        let no_enable = !enabled && refused(true, blocked);
        let no_unblock = blocked && refused(enabled, false);
        for (switch, toggle) in &self.toggles {
            let (on, available) = match switch {
                Switch::Enabled => (enabled, !no_enable),
                Switch::Trusted => (values.trusted.unwrap_or(d.d.trusted), true),
                Switch::Hidpp => (values.hidpp.unwrap_or(false), true),
                Switch::Blocked => (blocked, !no_unblock),
            };
            toggle.show(on, editable && available);
        }
        let (a, id) = (d.adapter.clone(), d.d.id);
        let error = |spot: Spot| {
            app.note(&spot)
                .map(|note| Item::text(note).look(Look::Error))
        };
        self.connection.set(
            &error(Spot::DetailsSave(a.clone(), id))
                .into_iter()
                .collect::<Vec<_>>(),
        );

        let top: Vec<Item> = error(Spot::DeviceBar(a.clone(), id))
            .into_iter()
            .chain(battery(d))
            .collect();
        self.top.set(&top);
        self.info.set(&information(app, d));
        self.pages.set(&subpages(profiles));

        self.forget.set(when(app.adapter_ready(d), Action::Forget));
        let mut items = Vec::new();
        if settings {
            items.push((
                "Refresh".to_owned(),
                when(app.can_refresh(d), Action::SettingsRefresh),
            ));
        }
        items.push(link.clone());
        self.menu.set(&items);
    }
}

/// The device's battery as a fact, when it reported a charge or whether it is charging.
pub fn battery(d: &DeviceView) -> Option<Item> {
    let b = d.battery.as_ref()?;
    // A known charge also fills a bar; charging alone has nothing to fill it.
    let percent = b.percent.and_then(|p| u32::try_from(p).ok());
    Some(
        Item::fact("Battery", b.text()?)
            .look(match () {
                _ if b.low() => Look::Error,
                _ if b.stale() => Look::Dim,
                _ => Look::Plain,
            })
            .meter(percent, percent.map(|_| 100)),
    )
}

/// What a fact the device reported looks like: dimmed while it is only last known.
fn reported_look(d: &DeviceView) -> Look {
    if d.connected() {
        Look::Plain
    } else {
        Look::Dim
    }
}

/// The Information group's facts.
pub fn information(app: &App<Live>, d: &DeviceView) -> Vec<Item> {
    let mut info = Vec::new();
    if !model::known_kinds(&d.d.kinds).is_empty() {
        info.push(Item::fact("Device Type", words::kind_text(d.kind)));
    }
    // The serial number identifies the device, so it shows with the identifiers.
    let about: Vec<&str> = DETAILS.into_iter().filter(|l| *l != SERIAL).collect();
    for (label, value) in facts(&d.d, &about) {
        info.push(Item::fact(label, text::display(&value)).look(reported_look(d)));
    }
    let adapter = app
        .adapter(&d.adapter)
        .map(|a| a.name.clone())
        .unwrap_or_default();
    info.push(Item::fact("Adapter", adapter));
    if let Some(t) = model::transport(d.d.transport) {
        info.push(Item::fact("Bluetooth", words::transport_name(t)));
    }
    let roles: Vec<&str> = model::roles(&d.d)
        .into_iter()
        .filter_map(words::role_text)
        .collect();
    if !roles.is_empty() {
        info.push(Item::fact("Input", roles.join(", ")));
    }
    info
}

/// Why the device's connection last failed, as a fact.
pub fn last_error(d: &DeviceView) -> Option<Item> {
    model::last_error(&d.d)
        .map(|e| Item::fact("Last Error", words::error_text(e, words::Scope::Other)))
}

/// The Logitech Features group's facts.
pub fn hidpp(d: &DeviceView) -> Vec<Item> {
    let mut hidpp = Vec::new();
    if let Some(i) = model::hidpp(&d.d) {
        if let Some(v) = text::hidpp_protocol_text(&d.d) {
            hidpp.push(Item::fact("HID++ Protocol", v));
        }
        hidpp.push(Item::fact("Status", words::integration_text(i)));
    }
    hidpp
}

/// The Gamepad Output group's facts, while the device has a gamepad output saved.
pub fn gamepad(d: &DeviceView) -> Vec<Item> {
    let mut gamepad = Vec::new();
    if let Some(o) = model::gamepad_output(&d.d).filter(|_| model::gamepad_saved(&d.d)) {
        gamepad.push(Item::fact("Status", text::output_state_words(o)));
        if let Some(slot) = o.slot {
            gamepad.push(Item::fact("Slot", text::slot_number(slot)));
        }
    }
    gamepad
}

/// The Security group's facts, which only a connected device's link has.
pub fn security(d: &DeviceView) -> Vec<Item> {
    d.d.security
        .as_ref()
        .filter(|_| d.connected())
        .map(words::security_facts)
        .unwrap_or_default()
        .into_iter()
        .map(|(label, value)| Item::fact(label, value))
        .collect()
}

/// The Identifiers group's facts.
pub fn identifiers(d: &DeviceView) -> Vec<Item> {
    let mut ids: Vec<Item> = facts(&d.d, &[SERIAL])
        .into_iter()
        .chain(facts(&d.d, &text::IDENTIFIERS))
        .map(|(label, value)| Item::fact(label, text::display(&value)).look(reported_look(d)))
        .collect();
    ids.push(Item::fact("Device ID", d.d.id.to_string()));
    ids
}

/// Links to the tabs that open over the page.
pub fn subpages(profiles: bool) -> Vec<Item> {
    let link =
        |tab: Tab, icon: &'static str| Item::link(tab.label(), "", Action::Tab(tab)).icon(icon);
    let mut out = Vec::new();
    if profiles {
        out.push(link(Tab::Profiles, "view-list-symbolic"));
    }
    out.push(link(Tab::Diagnostics, "applications-engineering-symbolic"));
    out
}

struct Diagnostics {
    tab: TabView,
    /// Why the last refresh failed.
    error: List,
    connection: List,
    profiles: List,
    saved: List,
    settings: List,
    hidpp: List,
    gamepad: List,
    security: List,
    ids: List,
    /// Refresh, then Connect or Disconnect.
    menu: Menu,
    /// Retry beside each read failure.
    retries: Vec<Btn>,
}

impl Diagnostics {
    fn new(ctx: &Rc<Ctx>) -> Self {
        let tab = TabView::new();
        let d = Self {
            error: List::new(ctx, ""),
            connection: List::new(ctx, "Connection"),
            profiles: List::new(ctx, "Profiles"),
            saved: List::new(ctx, "Saved Data"),
            settings: List::new(ctx, "Device Settings"),
            hidpp: List::new(ctx, "Logitech Features"),
            gamepad: List::new(ctx, "Gamepad Output"),
            security: List::new(ctx, "Security"),
            ids: List::new(ctx, "Identifiers"),
            menu: page_menu(ctx),
            retries: (0..3).map(|_| Btn::new(ctx, "Retry")).collect(),
            tab,
        };
        for (list, retry) in [&d.error, &d.saved, &d.settings]
            .into_iter()
            .zip(&d.retries)
        {
            retry.widget.set_valign(gtk::Align::Center);
            list.group.set_header_suffix(Some(&retry.widget));
        }
        for list in [
            &d.error,
            &d.connection,
            &d.profiles,
            &d.saved,
            &d.settings,
            &d.hidpp,
            &d.gamepad,
            &d.security,
            &d.ids,
        ] {
            d.tab.add(&list.group);
        }
        d
    }

    fn update(&mut self, app: &App<Live>, d: &DeviceView, link: &(String, Option<Action>)) {
        let (a, id) = (d.adapter.clone(), d.d.id);
        // A Connect or Disconnect from this tab's menu reports its failure here too.
        let mut connection: Vec<Item> = app
            .note(&Spot::DeviceBar(a.clone(), id))
            .map(|note| Item::text(note).look(Look::Error))
            .into_iter()
            .collect();
        if let Some(reason) = model::inactive(&d.d).filter(|_| d.read_error.is_none())
            && reason != InactiveReason::Disabled
        {
            connection.push(Item::text(words::inactive_text(&d.d)));
        }
        connection.extend(last_error(d));
        self.connection.set(&connection);
        let profiles: Vec<Item> = model::profile_error(&d.d)
            .filter(|_| d.connected())
            .map(|e| {
                vec![
                    Item::fact("Status", "Not Loaded").look(Look::Error),
                    Item::text(words::profile_error_text(e)).look(Look::Dim),
                ]
            })
            .unwrap_or_default();
        self.profiles.set(&profiles);
        let saved: Vec<Item> = d
            .read_error
            .iter()
            .map(|e| {
                Item::text(match words::error_text(e, words::Scope::Read) {
                    read if read == words::READ_FAILED => {
                        format!("{} Try again.", words::DEVICE_READ_FAILED)
                    }
                    read => format!("{} {read}", words::DEVICE_READ_FAILED),
                })
            })
            .collect();
        self.saved.set(&saved);
        let settings_error = app.settings_error(&a, id);
        let settings: Vec<Item> = settings_error
            .iter()
            .map(|e| {
                Item::text(if *e == words::READ_FAILED {
                    format!("{} Try again.", words::SETTINGS_READ_FAILED)
                } else {
                    format!("{} {e}", words::SETTINGS_READ_FAILED)
                })
            })
            .collect();
        self.settings.set(&settings);
        self.hidpp.set(&hidpp(d));
        self.gamepad.set(&gamepad(d));
        self.security.set(&security(d));
        self.ids.set(&identifiers(d));

        let error: Vec<Item> = app
            .note(&Spot::Diagnostics(a, id))
            .map(|note| Item::text(note).look(Look::Error))
            .into_iter()
            .collect();
        self.error.set(&error);
        let refresh = when(
            !app.device_busy(d) && app.adapter_ready(d),
            Action::DiagnosticsRefresh,
        );
        for b in &self.retries {
            b.set(refresh.clone());
        }
        self.menu
            .set(&[("Refresh".to_owned(), refresh), link.clone()]);
    }
}
