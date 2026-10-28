//! An adapter's page: its details and settings, with Profiles and Diagnostics opening from it.
//! Settings and Profiles changes apply as they are made. While the adapter isn't ready, the page
//! shows its state instead, with the migration's Migrate and Cancel. Rename, Connect, and
//! Disconnect are in the page's menu, and Restart and the development tools in the Diagnostics
//! menu.
use super::{
    device::subpages,
    profiles::AdapterProfiles,
    search::transport_group,
    shell::Ctx,
    widgets::{
        self, ADAPTER_ICON, Btn, Item, List, Look, Menu, Status, Tab as TabView, Toggle, spin,
        spinner, when,
    },
    window::{Head, Tabs, View},
};
use crate::{
    model,
    ui::{
        app::{
            Action, App, Kind, Live, Spot, Tab,
            adapter::{PLATFORMS, host_platform, restartable},
            words,
            world::{AdapterView, Conn, Storage},
        },
        text,
    },
};
use adw::prelude::*;
use cordial_protocol::{ErrorCode, ResourceCapacity, ResourceType, Status as Report, Transport};
use gtk::glib;
use std::rc::Rc;

pub struct AdapterPage {
    id: String,
    tabs: Tabs,
    banners: gtk::Box,
    shown_banners: Vec<String>,
    details: Details,
    profiles: AdapterProfiles,
    diagnostics: Diagnostics,
}

impl AdapterPage {
    pub fn new(ctx: &Rc<Ctx>, id: &str) -> Self {
        let mut tabs = Tabs::new(ctx);
        let details = Details::new(ctx);
        let profiles = AdapterProfiles::new(ctx);
        let diagnostics = Diagnostics::new(ctx);
        tabs.add(Tab::Details, &details.tab.widget);
        tabs.add(Tab::Profiles, &profiles.tab.widget);
        tabs.add(Tab::Diagnostics, &diagnostics.tab.widget);
        tabs.pack_end(Tab::Details, &details.menu.button);
        tabs.pack_end(Tab::Diagnostics, &diagnostics.menu.button);
        let banners = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        tabs.add_top_bar(&banners);
        Self {
            id: id.to_owned(),
            tabs,
            banners,
            shown_banners: Vec::new(),
            details,
            profiles,
            diagnostics,
        }
    }
}

impl View for AdapterPage {
    fn tabs(&self) -> &Tabs {
        &self.tabs
    }

    fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>) -> Option<Head> {
        let a = app.adapter(&self.id)?.clone();
        let mut offered = vec![Tab::Details];
        if a.profile_support() {
            offered.push(Tab::Profiles);
        }
        offered.push(Tab::Diagnostics);
        self.tabs.show(&offered, app.tab());
        if a.attention != self.shown_banners {
            while let Some(child) = self.banners.first_child() {
                self.banners.remove(&child);
            }
            for text in &a.attention {
                self.banners.append(
                    &adw::Banner::builder()
                        .title(text)
                        .use_markup(false)
                        .revealed(true)
                        .build(),
                );
            }
            self.shown_banners = a.attention.clone();
        }
        self.details.update(ctx, app, &a);
        if a.profile_support() {
            self.profiles.update(ctx, app, &a);
        }
        self.diagnostics.update(app, &a);
        Some(Head {
            title: a.name.clone(),
            subtitle: a.status_text().0.to_owned(),
        })
    }
}

/// The adapter's migration or other state while its status can't be shown.
fn show_state(status: &Status, a: &AdapterView) {
    let progress = match a.storage {
        Storage::Migrating { progress, .. } => progress,
        _ => None,
    };
    status.show(a.status_text().0, progress);
}

/// A menu in a page's header.
fn page_menu(ctx: &Rc<Ctx>) -> Menu {
    let menu = Menu::new(ctx);
    menu.button.set_icon_name("view-more-symbolic");
    menu.button.set_tooltip_text(Some("Adapter Menu"));
    menu.button
        .update_property(&[gtk::accessible::Property::Label("Adapter Menu")]);
    menu
}

/// Cancel and Migrate, offered while the adapter's saved data needs migrating.
struct Migration {
    widget: gtk::Box,
    cancel: Btn,
    migrate: Btn,
}

impl Migration {
    fn new(ctx: &Rc<Ctx>) -> Self {
        let widget = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let cancel = Btn::new(ctx, "Cancel");
        let migrate = Btn::suggested(ctx, "Migrate");
        widget.append(&cancel.widget);
        widget.append(&migrate.widget);
        Self {
            widget,
            cancel,
            migrate,
        }
    }

    /// Shows the buttons the migration offers, returning whether there are any.
    fn update(&self, a: &AdapterView) -> bool {
        self.migrate.offer(
            matches!(a.storage, Storage::Required | Storage::Failed { .. }),
            Some(Action::Migrate(a.id.clone())),
        );
        match &a.storage {
            Storage::Migrating { cancelling, .. } => {
                self.cancel.set(when(!cancelling, Action::CancelMigration));
            }
            Storage::Failed { .. } => self.cancel.set(Some(Action::CancelMigration)),
            _ => self.cancel.hide(),
        }
        self.migrate.widget.is_visible() || self.cancel.widget.is_visible()
    }
}

/// Why the adapter can't be used: its saved data, or failing that why its connection or last
/// settings change failed.
fn problem(app: &App<Live>, a: &AdapterView) -> Option<String> {
    if let Storage::Failed { text, .. } | Storage::Blocked(text) | Storage::Unsupported(text) =
        &a.storage
    {
        return Some(text.clone());
    }
    app.note(&Spot::AdapterSave(a.id.clone()))
        .map(str::to_owned)
        .or_else(|| a.connect_error.clone().filter(|_| !a.connected()))
}

struct Details {
    tab: TabView,
    status: Status,
    /// The migration's buttons, and a spinner while connecting.
    migration: Migration,
    spinner: gtk::Spinner,
    firmware: List,
    devices: List,
    usb: List,
    info: List,
    pages: List,
    /// The links while the status page replaces the groups.
    status_pages: List,
    settings: Settings,
    menu: Menu,
}

impl Details {
    fn new(ctx: &Rc<Ctx>) -> Self {
        let tab = TabView::new();
        let status = Status::new(ADAPTER_ICON);
        let migration = Migration::new(ctx);
        let spinner = spinner();
        for b in [&migration.cancel.widget, &migration.migrate.widget] {
            b.add_css_class("pill");
        }
        migration.widget.prepend(&spinner);
        status.body.append(&migration.widget);
        let firmware = List::new(ctx, "Information");
        status.body.append(&firmware.group);
        let status_pages = List::new(ctx, "");
        status.body.append(&status_pages.group);
        let devices = List::new(ctx, "Devices");
        let usb = List::new(ctx, "USB");
        let info = List::new(ctx, "Information");
        let pages = List::new(ctx, "");
        let tail = [&devices, &usb, &info, &pages].map(|l| l.group.clone());
        let settings = Settings::new(ctx, &tab, tail.to_vec());
        for g in &tail {
            tab.add(g);
        }
        Self {
            devices,
            usb,
            info,
            pages,
            status_pages,
            settings,
            menu: page_menu(ctx),
            migration,
            spinner,
            status,
            firmware,
            tab,
        }
    }

    fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>, a: &AdapterView) {
        let rename = match a.storage {
            Storage::Migrating { .. } | Storage::Failed { .. } => None,
            _ => a.live().map(|_| Action::RenameAdapter(a.id.clone())),
        };
        let connection = if a.conn == Conn::Connected {
            ("Disconnect", Action::DisconnectAdapter(a.id.clone()))
        } else {
            ("Connect", Action::ConnectAdapter(a.id.clone()))
        };
        self.menu.set(&[
            ("Rename…".into(), rename),
            (
                connection.0.into(),
                when(a.conn != Conn::Connecting, connection.1),
            ),
        ]);
        let pages = subpages(a.profile_support());
        let Some(live) = a.live() else {
            self.settings.host.set_revealed(false);
            self.status_pages.set(&pages);
            show_state(&self.status, a);
            let problem = problem(app, a)
                .map(|text| glib::markup_escape_text(&text).to_string())
                .unwrap_or_default();
            if self.status.widget.description().unwrap_or_default() != problem {
                self.status.widget.set_description(Some(&problem));
            }
            let migrating = self.migration.update(a);
            let connecting =
                a.conn == Conn::Connecting && !matches!(a.storage, Storage::Failed { .. });
            spin(&self.spinner, connecting);
            self.migration.widget.set_visible(migrating || connecting);
            self.firmware.set(&information(a));
            self.tab.show(Some(&self.status.widget.clone().upcast()));
            return;
        };
        self.tab.show(None);
        self.settings.update(ctx, app, a, live);
        let save: Vec<Item> = app
            .note(&Spot::AdapterSave(a.id.clone()))
            .map(|note| Item::text(note).look(Look::Error))
            .into_iter()
            .collect();
        self.settings.error.set(&save);
        self.pages.set(&pages);
        let devices: Vec<Item> = app
            .devices()
            .iter()
            .filter(|d| d.adapter == a.id)
            .map(|d| {
                let mut item = Item::link(d.name.clone(), d.status(), Action::Show(d.page()))
                    .icon(widgets::device_icon(d.kind));
                if let Some(p) = d.battery.as_ref().and_then(|b| b.percent) {
                    item = item.value(format!("{p}%")).look(match () {
                        _ if d.low() => Look::Error,
                        _ if d.battery.as_ref().is_some_and(|b| b.stale()) => Look::Dim,
                        _ => Look::Plain,
                    });
                }
                item
            })
            .collect();
        self.devices.set(&devices);
        self.usb.set(&usb(live));
        self.info.set(&information(a));
    }
}

/// The USB group's rows.
pub fn usb(live: &Report) -> Vec<Item> {
    let mut usb: Vec<Item> = capacity(
        "Interfaces",
        model::resource(&live.resources, ResourceType::Interfaces),
    )
    .into_iter()
    .collect();
    for (code, label) in text::usb_capacity(live) {
        let look = if code == ErrorCode::UsbRequiredCapacityExceeded {
            Look::Warning
        } else {
            Look::Dim
        };
        usb.push(Item::text(label).look(look));
    }
    usb
}

/// The Information group's facts: the firmware, board, and storage while the adapter is ready,
/// or the firmware it reported on connecting while it isn't.
pub fn information(a: &AdapterView) -> Vec<Item> {
    let Some(live) = a.live() else {
        return a
            .hello
            .as_ref()
            .and_then(|h| words::reported(h.hello.firmware_version.as_deref()))
            .map(|f| Item::fact("Firmware", f))
            .into_iter()
            .collect();
    };
    let mut info: Vec<Item> = [
        ("Firmware", App::<Live>::firmware(live)),
        ("Board", App::<Live>::board(live)),
    ]
    .into_iter()
    .filter_map(|(label, value)| Some(Item::fact(label, value?)))
    .collect();
    if let Some(text) = text::storage_text(live) {
        let r = model::resource(&live.resources, ResourceType::Storage);
        info.push(
            Item::fact("Storage", text).meter(r.and_then(|r| r.count), r.and_then(|r| r.limit)),
        );
    }
    info
}

/// A resource's usage as a fact, with a bar when its count and limit are known; None when
/// nothing is known of it.
fn capacity(label: &str, r: Option<&ResourceCapacity>) -> Option<Item> {
    let r = r?;
    Some(Item::fact(label, text::capacity_text(r)?).meter(r.count, r.limit))
}

/// The adapter's settings among the details' groups: why the last change couldn't be saved, a
/// platform row, a banner while the platform isn't this computer's, and a group for each
/// supported transport. A transport's group has its
/// switch and, while the adapter has it enabled, its device and connection capacity and whether
/// it can pair.
struct Settings {
    error: List,
    host: adw::Banner,
    platform: adw::ComboRow,
    page: adw::PreferencesPage,
    /// The page's groups that follow the transports.
    tail: Vec<adw::PreferencesGroup>,
    transports: Vec<(Transport, Toggle, List)>,
}

impl Settings {
    fn new(ctx: &Rc<Ctx>, tab: &TabView, tail: Vec<adw::PreferencesGroup>) -> Self {
        let names: Vec<&str> = PLATFORMS.iter().map(|p| text::platform_name(*p)).collect();
        let platform = adw::ComboRow::builder()
            .title("Platform")
            .model(&gtk::StringList::new(&names))
            .build();
        platform.connect_selected_notify(glib::clone!(
            #[weak]
            ctx,
            move |row| {
                if let Some(p) = PLATFORMS.get(row.selected() as usize) {
                    ctx.act(Action::Platform(*p));
                }
            }
        ));
        let error = List::new(ctx, "");
        tab.add(&error.group);
        let platform_group = adw::PreferencesGroup::new();
        platform_group.add(&platform);
        tab.add(&platform_group);
        let host_platform = text::platform_name(host_platform());
        let host = adw::Banner::builder()
            .title(format!("This computer runs {host_platform}."))
            .button_label(format!("Switch to {host_platform}"))
            .build();
        let sync_accessibility = |host: &adw::Banner| {
            host.upcast_ref::<gtk::Widget>()
                .update_state(&[gtk::accessible::State::Hidden(!host.is_revealed())]);
        };
        sync_accessibility(&host);
        host.connect_revealed_notify(sync_accessibility);
        // GTK resets the hidden state after emitting map, so restore it before painting.
        host.connect_map(move |host| {
            host.add_tick_callback(move |host, _| {
                sync_accessibility(host);
                glib::ControlFlow::Break
            });
        });
        host.connect_button_clicked(glib::clone!(
            #[weak]
            ctx,
            move |_| ctx.act(Action::SwitchPlatform)
        ));
        tab.widget.add_top_bar(&host);
        Self {
            error,
            host,
            platform,
            page: tab.page.clone(),
            tail,
            transports: Vec::new(),
        }
    }

    fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>, a: &AdapterView, view: &Report) {
        let editable = app.adapter_editable(a);
        // A change being applied shows until its result.
        let draft = app.adapter_drafts.get(&a.id);
        let shown = draft
            .and_then(|d| d.platform)
            .unwrap_or_else(|| view.platform());
        if let Some(i) = PLATFORMS.iter().position(|p| *p == shown)
            && self.platform.selected() != i as u32
        {
            self.platform.set_selected(i as u32);
        }
        self.platform.set_sensitive(editable);
        self.host.set_revealed(shown != host_platform());
        self.host.set_sensitive(editable);

        let transports = model::transports(view);
        if transports != self.transports.iter().map(|(t, ..)| *t).collect::<Vec<_>>() {
            for (_, _, list) in self.transports.drain(..) {
                self.page.remove(&list.group);
            }
            for t in &transports {
                let t = *t;
                let weak = Rc::downgrade(ctx);
                let toggle = Toggle::new(words::transport_name(t), move |on| {
                    if let Some(ctx) = weak.upgrade() {
                        ctx.act(Action::Transport(t, on));
                    }
                });
                let mut list = List::new(ctx, "");
                list.group.set_widget_name(&transport_group(t));
                list.lead(&toggle.row);
                self.page.add(&list.group);
                self.transports.push((t, toggle, list));
            }
            // A page only appends groups, so the groups after the transports move back behind
            // them.
            for g in &self.tail {
                self.page.remove(g);
                self.page.add(g);
            }
        }
        for (t, toggle, list) in &mut self.transports {
            let t = *t;
            let saved = model::transport_enabled(view, t).unwrap_or(true);
            let on = draft
                .and_then(|d| d.transports.iter().find(|(x, _)| *x == t))
                .map_or(saved, |(_, on)| *on);
            toggle.show(on, editable);
            list.set(&transport_facts(view, t));
        }
    }
}

/// A transport group's facts while the adapter has the transport enabled: its device and
/// connection capacity and whether it can pair.
pub fn transport_facts(view: &Report, t: Transport) -> Vec<Item> {
    let mut items = Vec::new();
    if model::transport_enabled(view, t).unwrap_or(true) {
        let resources = view
            .transports
            .iter()
            .find(|s| s.transport == t as i32)
            .map_or(&[][..], |s| s.resources.as_slice());
        items.extend(capacity(
            "Active Devices",
            model::resource(resources, ResourceType::Devices),
        ));
        items.extend(capacity(
            "Connections",
            model::resource(resources, ResourceType::Connections),
        ));
        // Pairing is limited by the adapter's storage, not by the transport.
        if model::storage_full(view) {
            items.push(Item::fact("New Pairings", words::STORAGE_FULL).look(Look::Warning));
        }
    }
    items
}

struct Diagnostics {
    tab: TabView,
    migration: List,
    errors: List,
    saved: List,
    retry: Btn,
    ids: List,
    buttons: Migration,
    /// Restart, and, on development firmware, Enter Bootloader.
    menu: Menu,
}

impl Diagnostics {
    fn new(ctx: &Rc<Ctx>) -> Self {
        let tab = TabView::new();
        let d = Self {
            migration: List::new(ctx, "Migration"),
            errors: List::new(ctx, "Errors"),
            saved: List::new(ctx, "Saved Devices"),
            retry: Btn::new(ctx, "Retry"),
            ids: List::new(ctx, "Identifiers"),
            buttons: Migration::new(ctx),
            menu: page_menu(ctx),
            tab,
        };
        d.migration.group.set_header_suffix(Some(&d.buttons.widget));
        d.retry.widget.set_valign(gtk::Align::Center);
        d.saved.group.set_header_suffix(Some(&d.retry.widget));
        for list in [&d.migration, &d.errors, &d.saved, &d.ids] {
            d.tab.add(&list.group);
        }
        d
    }

    fn update(&mut self, app: &App<Live>, a: &AdapterView) {
        let development = a.status.as_ref().is_some_and(model::development)
            && a.connected()
            && a.storage == Storage::Normal;
        let mut tools = Vec::new();
        if restartable(a) {
            tools.push((
                "Restart".to_owned(),
                when(
                    !app.running(&a.id, |k| matches!(k, Kind::Restart)),
                    Action::Restart,
                ),
            ));
        }
        if development {
            tools.push(("Enter Bootloader…".to_owned(), Some(Action::Bootloader)));
        }
        self.menu.set(&tools);
        self.menu.button.set_visible(!tools.is_empty());

        let migration: Vec<Item> = match &a.storage {
            Storage::Failed { text, reason } => vec![
                Item::text(text.clone()).look(Look::Error),
                Item::text(text::display(reason)),
            ],
            _ => Vec::new(),
        };
        self.migration.set(&migration);
        if self.buttons.update(a) {
            self.migration.group.set_visible(true);
        }
        let mut errors: Vec<Item> = match &a.storage {
            Storage::Blocked(text) | Storage::Unsupported(text) => {
                vec![Item::text(text.clone()).look(Look::Error)]
            }
            _ => Vec::new(),
        };
        errors.extend(
            a.status
                .iter()
                .filter(|_| a.connected())
                .flat_map(|s| &s.errors)
                .map(|e| Item::text(words::error_text(e, words::Scope::Save))),
        );
        self.errors.set(&errors);
        let saved: Vec<Item> = a
            .unreadable
            .iter()
            .map(|(id, e)| Item::fact(format!("Device {id}"), words::couldnt_read(Some(e))))
            .collect();
        self.saved.set(&saved);
        self.retry.set(when(
            a.connected() && !app.running(&a.id, |k| matches!(k, Kind::Quiet)),
            Action::DevicesRetry,
        ));
        self.ids.set(&identifiers(a));
    }
}

/// The Identifiers group's facts.
pub fn identifiers(a: &AdapterView) -> Vec<Item> {
    let mut ids = Vec::new();
    if let Some(hello) = &a.hello {
        if let Some(serial) = words::reported(hello.hello.serial_number.as_deref()) {
            ids.push(Item::fact("Serial Number", serial));
        }
    } else {
        ids.push(Item::fact("Adapter ID", a.id.clone()));
        if let Some(format) = a.live().and_then(text::storage_format) {
            ids.push(Item::fact("Storage Format", format.to_string()));
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::translate::{IntoGlib, ToGlibPtr, from_glib_full};

    #[test]
    #[ignore = "requires a display"]
    fn platform_banner_accessibility_follows_revelation() {
        adw::init().expect("GTK needs a display");
        let (ctx, _) = Ctx::test();
        let tab = TabView::new();
        let settings = Settings::new(&ctx, &tab, Vec::new());
        let window = adw::Window::builder().content(&tab.widget).build();
        for _ in 0..2 {
            window.present();
            assert!(settings.host.is_mapped());
            for revealed in [false, true, false] {
                settings.host.set_revealed(revealed);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let mismatch: Option<glib::GString> = unsafe {
                        from_glib_full(gtk::ffi::gtk_test_accessible_check_state(
                            settings
                                .host
                                .upcast_ref::<gtk::Widget>()
                                .upcast_ref::<gtk::Accessible>()
                                .to_glib_none()
                                .0,
                            gtk::AccessibleState::Hidden.into_glib(),
                            (!revealed).into_glib(),
                        ))
                    };
                    if mismatch.is_none() {
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline, "{mismatch:?}");
                    glib::MainContext::default().iteration(false);
                }
                assert!(settings.host.is_visible());
                assert_eq!(settings.host.is_revealed(), revealed);
            }
            window.set_visible(false);
        }
        window.close();
    }
}
