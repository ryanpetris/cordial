//! The tray icon, a StatusNotifierItem.
//!
//! [`model`] decides what the tray shows. A worker thread owns the item, since registering it
//! waits on the session bus, and replaces what it shows with the latest model it was sent. Menu
//! clicks reach the app through [`Emit`].
use super::{Emit, ServiceEvent, alerts, prefs::Preferences};
use crate::ui::app::{Action, AdapterView, DeviceView, Page, Storage, words::Level, world::Conn};
use cordial_protocol::DeviceState;
use gtk::{gdk, glib, prelude::*};
use ksni::{
    MenuItem,
    blocking::TrayMethods,
    menu::{StandardItem, SubMenu},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, mpsc},
    thread,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    Idle,
    Connected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    None,
    Low,
    Attention,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub page: Page,
    pub label: String,
    pub connected: bool,
    /// The device's adapter is connected and ready, so it answers Disconnect.
    pub ready: bool,
    /// Connect is offered only where it can work.
    pub can_connect: bool,
}

/// An adapter, shown as a submenu of its actions, followed by its devices.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub adapter: String,
    pub name: String,
    pub devices: Vec<Device>,
    /// The adapter is connected, so it offers Disconnect in place of Connect.
    pub connected: bool,
    /// Connect or Disconnect can start now.
    pub can_toggle: bool,
    /// The adapter is connected and ready, so it can add a device.
    pub can_add: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub visible: bool,
    pub base: Base,
    pub badge: Badge,
    pub tooltip: Vec<String>,
    pub groups: Vec<Group>,
    /// What the menu says in place of devices when it has none.
    pub empty: Option<&'static str>,
    pub attention: Vec<String>,
}

fn low(d: &DeviceView, threshold: i64) -> bool {
    d.battery
        .as_ref()
        .and_then(|b| alerts::level(b, threshold))
        .is_some_and(|l| l != Level::Ok)
}

pub fn model(adapters: &[AdapterView], devices: &[DeviceView], p: &Preferences) -> Model {
    let threshold = p.low_battery_percent;
    // Every adapter gets its submenu, for Connect, but only these keep the tray shown and list
    // devices. An adapter that is reconnecting keeps its devices listed, as last known.
    let all = adapters;
    let adapters: Vec<&AdapterView> = all
        .iter()
        .filter(|a| {
            a.connected()
                || (a.conn == Conn::Connecting
                    && a.status.is_some()
                    && a.storage == Storage::Normal)
        })
        .collect();
    let ready: HashSet<&str> = adapters
        .iter()
        .filter(|a| a.connected() && a.ready)
        .map(|a| a.id.as_str())
        .collect();
    let devices: Vec<&DeviceView> = devices
        .iter()
        .filter(|d| adapters.iter().any(|a| a.id == d.adapter))
        .collect();
    let tray = |d: &DeviceView| {
        let ready = ready.contains(d.adapter.as_str());
        let mut label = d.name.clone();
        if let Some(battery) = d.battery.as_ref().and_then(|b| b.text()) {
            label.push_str(" · ");
            label.push_str(&battery);
            if low(d, threshold) {
                label.push_str(" (low)");
            }
        }
        Device {
            page: d.page(),
            label,
            connected: d.connected(),
            ready,
            // A record that is only last known leaves Connect to the adapter.
            can_connect: ready
                && (d.read_error.is_some()
                    || (d.d.state() == DeviceState::Disconnected
                        && crate::model::inactive(&d.d).is_none())),
        }
    };
    let connected: Vec<&DeviceView> = devices.iter().copied().filter(|d| d.connected()).collect();
    let lows: Vec<&DeviceView> = connected
        .iter()
        .copied()
        .filter(|d| low(d, threshold))
        .collect();
    let attention: Vec<String> = adapters
        .iter()
        .flat_map(|a| a.attention.iter().map(|r| format!("{}: {r}", a.name)))
        .collect();
    // A migration, even a failed one, holds every other adapter closed until it ends.
    let migrating = all.iter().any(|a| {
        matches!(
            a.storage,
            Storage::Migrating { .. } | Storage::Failed { .. }
        )
    });
    let groups = all
        .iter()
        .map(|a| Group {
            adapter: a.id.clone(),
            name: a.name.clone(),
            devices: devices
                .iter()
                .filter(|d| d.adapter == a.id)
                .map(|d| tray(d))
                .collect(),
            connected: a.connected(),
            can_toggle: a.connected() || (a.conn == Conn::Disconnected && !migrating),
            can_add: ready.contains(a.id.as_str()),
        })
        .collect();
    let mut tooltip = vec![match connected.len() {
        0 => "No devices connected".to_owned(),
        1 => "1 device connected".to_owned(),
        n => format!("{n} devices connected"),
    }];
    for d in &lows {
        let place = match adapters.iter().find(|a| a.id == d.adapter) {
            Some(a) if adapters.len() > 1 => format!(" ({})", a.name),
            _ => String::new(),
        };
        let battery = d
            .battery
            .as_ref()
            .and_then(|b| b.text())
            .unwrap_or_default();
        tooltip.push(format!("{}{place}: battery {battery}", d.name));
    }
    tooltip.extend(attention.iter().cloned());
    Model {
        visible: !adapters.is_empty() || p.always_show_tray,
        base: if connected.is_empty() {
            Base::Idle
        } else {
            Base::Connected
        },
        badge: if !attention.is_empty() {
            Badge::Attention
        } else if !lows.is_empty() {
            Badge::Low
        } else {
            Badge::None
        },
        tooltip,
        groups,
        empty: match (devices.is_empty(), adapters.is_empty()) {
            (false, _) => None,
            (true, false) => Some("No Saved Devices"),
            (true, true) => Some("No Adapter Connected"),
        },
        attention,
    }
}

/// The icon variant for the panel: light icons on a dark panel. GNOME's top bar is dark in both
/// styles.
pub fn variant() -> &'static str {
    let gnome = std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.to_ascii_uppercase().contains("GNOME"));
    if gnome || adw::StyleManager::default().is_dark() {
        "light"
    } else {
        "dark"
    }
}

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        [$((
            $name,
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../packaging/icons/", $name, ".png")).as_slice(),
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../packaging/icons/", $name, "@2x.png")).as_slice(),
        )),*]
    };
}

/// Each tray image at one and two times its size.
static ICONS: [(&str, &[u8], &[u8]); 12] = icons![
    "tray-idle-none-dark",
    "tray-idle-none-light",
    "tray-idle-low-dark",
    "tray-idle-low-light",
    "tray-idle-attention-dark",
    "tray-idle-attention-light",
    "tray-connected-none-dark",
    "tray-connected-none-light",
    "tray-connected-low-dark",
    "tray-connected-low-light",
    "tray-connected-attention-dark",
    "tray-connected-attention-light",
];

fn name(base: Base, badge: Badge, variant: &str) -> String {
    let base = match base {
        Base::Idle => "idle",
        Base::Connected => "connected",
    };
    let badge = match badge {
        Badge::None => "none",
        Badge::Low => "low",
        Badge::Attention => "attention",
    };
    format!("tray-{base}-{badge}-{variant}")
}

/// A PNG as ARGB32 in network byte order.
fn pixmap(png: &'static [u8]) -> Option<ksni::Icon> {
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_static(png)).ok()?;
    let mut download = gdk::TextureDownloader::new(&texture);
    download.set_format(gdk::MemoryFormat::A8r8g8b8);
    let (bytes, stride) = download.download_bytes();
    let (width, height) = (texture.width(), texture.height());
    let row = width as usize * 4;
    let data = bytes
        .chunks(stride)
        .take(height as usize)
        .flat_map(|r| &r[..row])
        .copied()
        .collect();
    Some(ksni::Icon {
        width,
        height,
        data,
    })
}

#[derive(Clone)]
struct Shown {
    model: Model,
    icon: Arc<[ksni::Icon]>,
}

struct Item {
    shown: Shown,
    emit: Emit,
}

/// Menu labels mark mnemonics with underscores.
fn mnemonic(s: &str) -> String {
    s.replace('_', "__")
}

fn item(label: &str, enabled: bool, events: Vec<ServiceEvent>) -> MenuItem<Item> {
    StandardItem {
        label: mnemonic(label),
        enabled,
        activate: Box::new(move |t: &mut Item| {
            for e in &events {
                (t.emit)(e.clone());
            }
        }),
        ..Default::default()
    }
    .into()
}

fn text(label: &str) -> MenuItem<Item> {
    item(label, false, Vec::new())
}

fn submenu(label: &str, submenu: Vec<MenuItem<Item>>) -> MenuItem<Item> {
    SubMenu {
        label: mnemonic(label),
        submenu,
        ..Default::default()
    }
    .into()
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "cordial".into()
    }

    fn title(&self) -> String {
        "Cordial".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.shown.icon.to_vec()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Cordial".into(),
            // The description may hold markup; device and adapter names must not.
            description: glib::markup_escape_text(&self.shown.model.tooltip.join("\n")).into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.emit)(ServiceEvent::Present(None));
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        use ServiceEvent::*;
        let m = &self.shown.model;
        let mut items: Vec<MenuItem<Self>> = m.attention.iter().map(|r| text(r)).collect();
        if !items.is_empty() {
            items.push(MenuItem::Separator);
        }
        if let Some(empty) = m.empty {
            items.push(text(empty));
        }
        for (i, g) in m.groups.iter().enumerate() {
            if i > 0 {
                items.push(MenuItem::Separator);
            }
            let id = &g.adapter;
            let page = Page::Adapter(id.clone());
            let toggle = if g.connected {
                ("Disconnect", Action::DisconnectAdapter(id.clone()))
            } else {
                ("Connect", Action::ConnectAdapter(id.clone()))
            };
            items.push(submenu(
                &g.name,
                vec![
                    item(toggle.0, g.can_toggle, vec![Act(page.clone(), toggle.1)]),
                    item("Open", true, vec![Present(Some(page))]),
                    item(
                        "Add Device",
                        g.can_add,
                        vec![
                            Present(None),
                            Act(Page::Overview, Action::AddDeviceTo(id.clone())),
                        ],
                    ),
                ],
            ));
            for d in &g.devices {
                let page = d.page.clone();
                let first = if d.connected {
                    item(
                        "Disconnect",
                        d.ready,
                        vec![Act(page.clone(), Action::Disconnect)],
                    )
                } else {
                    item(
                        "Connect",
                        d.can_connect,
                        vec![Act(page.clone(), Action::Connect)],
                    )
                };
                let open = item("Open", true, vec![Present(Some(page))]);
                items.push(submenu(&d.label, vec![first, open]));
            }
        }
        items.extend([
            MenuItem::Separator,
            item("Open Cordial", true, vec![Present(None)]),
            MenuItem::Separator,
            item("Settings", true, vec![Preferences]),
            item("Quit", true, vec![Quit]),
        ]);
        items
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        // The panel may come back, as when GNOME Shell restarts.
        true
    }
}

/// Shows the latest model on the worker's item: registering it on the first, updating it after,
/// and removing it when hidden. The item ends when the channel closes.
fn work(rx: mpsc::Receiver<Option<Shown>>, emit: Emit) {
    let mut handle: Option<ksni::blocking::Handle<Item>> = None;
    let mut unavailable = false;
    while let Ok(mut next) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            next = newer;
        }
        let Some(shown) = next else {
            if let Some(h) = handle.take() {
                h.shutdown().wait();
            }
            continue;
        };
        if let Some(h) = &handle
            && h.update(|item| item.shown = shown.clone()).is_some()
        {
            continue;
        }
        if unavailable {
            continue;
        }
        let item = Item {
            shown,
            emit: emit.clone(),
        };
        // Without a panel that shows tray items yet, the item waits for one.
        match item.assume_sni_available(true).spawn() {
            Ok(h) => handle = Some(h),
            Err(e) => {
                crate::desktop::diagnostic(format_args!("showing the tray icon failed: {e}"));
                unavailable = true;
            }
        }
    }
    if let Some(h) = handle {
        h.shutdown().wait();
    }
}

/// The tray worker, fed from the main thread.
pub struct Tray {
    tx: Option<mpsc::Sender<Option<Shown>>>,
    /// What was last sent: the model and icon variant, or None when hidden.
    last: Option<Option<(Model, &'static str)>>,
    icons: HashMap<String, Arc<[ksni::Icon]>>,
}

impl Tray {
    pub fn new(emit: Emit) -> Self {
        let (tx, rx) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("tray".into())
            .spawn(move || work(rx, emit));
        if let Err(e) = &spawned {
            crate::desktop::diagnostic(format_args!("starting the tray failed: {e}"));
        }
        Self {
            tx: spawned.ok().map(|_| tx),
            last: None,
            icons: HashMap::new(),
        }
    }

    /// Shows `model` with icons for `variant`, unless it is already shown.
    pub fn show(&mut self, model: Model, variant: &'static str) {
        let next = model.visible.then_some((model, variant));
        if self.last.as_ref() == Some(&next) {
            return;
        }
        let shown = next.clone().map(|(model, variant)| {
            let name = name(model.base, model.badge, variant);
            let icon = self
                .icons
                .entry(name)
                .or_insert_with_key(|name| {
                    let (_, one, two) = ICONS
                        .iter()
                        .find(|(n, ..)| n == name)
                        .expect("every tray image is embedded");
                    [one, two]
                        .into_iter()
                        .filter_map(|png| pixmap(png))
                        .collect()
                })
                .clone();
            Shown { model, icon }
        });
        self.last = Some(next);
        if let Some(tx) = &self.tx {
            let _ = tx.send(shown);
        }
    }

    /// Ends the worker, which removes the item.
    pub fn stop(&mut self) {
        self.tx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::services::alerts::tests::{adapter, battery, device};
    use crate::ui::app::AdapterView;
    use cordial_protocol::{self as p, ErrorCode, InactiveReason};
    use ksni::Tray as _;
    use std::sync::Mutex;

    fn tray(adapters: &[AdapterView], devices: &[DeviceView], always: bool) -> Model {
        let p = Preferences {
            always_show_tray: always,
            ..Preferences::default()
        };
        model(adapters, devices, &p)
    }

    fn disconnected(id: &str) -> AdapterView {
        AdapterView {
            conn: Conn::Disconnected,
            status: None,
            ready: false,
            ..adapter(id)
        }
    }

    /// The tray's item, recording the events its menu emits.
    fn item(model: Model) -> (Item, Arc<Mutex<Vec<ServiceEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let item = Item {
            shown: Shown {
                model,
                icon: Arc::new([]),
            },
            emit: Arc::new(move |e| sink.lock().unwrap().push(e)),
        };
        (item, events)
    }

    /// Each entry's label and whether it is enabled, with a submenu's entries in brackets.
    fn labels(menu: &[MenuItem<Item>]) -> Vec<String> {
        menu.iter()
            .map(|i| match i {
                MenuItem::Standard(s) => format!("{} {}", s.label, s.enabled),
                MenuItem::SubMenu(s) => format!("{} [{}]", s.label, labels(&s.submenu).join(", ")),
                MenuItem::Separator => "-".into(),
                _ => unreachable!(),
            })
            .collect()
    }

    /// Activates the entry `entry` of the submenu at `at`, returning what it emitted.
    fn click(model: Model, at: usize, entry: usize) -> Vec<ServiceEvent> {
        let (mut item, events) = item(model);
        let menu = item.menu();
        let MenuItem::SubMenu(sub) = &menu[at] else {
            panic!("no submenu at {at}")
        };
        let MenuItem::Standard(s) = &sub.submenu[entry] else {
            panic!("no entry at {entry}")
        };
        assert!(s.enabled, "{} is disabled", s.label);
        (s.activate)(&mut item);
        events.lock().unwrap().clone()
    }

    #[test]
    fn is_hidden_without_a_connected_adapter_unless_always_shown() {
        assert!(!tray(&[], &[], false).visible);
        assert!(!tray(&[disconnected("A")], &[], false).visible);
        assert!(tray(&[], &[], true).visible);
    }

    #[test]
    fn fills_when_a_device_is_connected_and_badges_attention_before_low_battery() {
        let idle = tray(
            &[adapter("A")],
            &[device("A", 1, DeviceState::Disconnected)],
            false,
        );
        assert_eq!(
            (idle.visible, idle.base, idle.badge),
            (true, Base::Idle, Badge::None)
        );
        let mut low = device("A", 1, DeviceState::Connected);
        low.battery = battery(Some(20), Some(false));
        let mut needs = adapter("A");
        needs.attention = vec!["1 device needs pairing again".into()];
        let attention = tray(&[needs], std::slice::from_ref(&low), false);
        assert_eq!(
            (attention.base, attention.badge),
            (Base::Connected, Badge::Attention)
        );
        assert_eq!(
            attention.attention,
            ["Adapter A: 1 device needs pairing again"]
        );
        let model = tray(&[adapter("A")], &[low], false);
        assert_eq!((model.base, model.badge), (Base::Connected, Badge::Low));
        assert_eq!(model.groups[0].devices[0].label, "1 · 20% (low)");
        assert_eq!(model.tooltip, ["1 device connected", "1: battery 20%"]);
    }

    #[test]
    fn low_battery_follows_the_chosen_level() {
        let mut d = device("A", 1, DeviceState::Connected);
        d.battery = battery(Some(25), Some(false));
        let p = Preferences {
            low_battery_percent: 30,
            ..Preferences::default()
        };
        assert_eq!(
            model(&[adapter("A")], std::slice::from_ref(&d), &p).badge,
            Badge::Low
        );
        assert_eq!(tray(&[adapter("A")], &[d], false).badge, Badge::None);
    }

    #[test]
    fn labels_devices_by_name_and_battery_alone() {
        let mut charged = device("A", 1, DeviceState::Connected);
        charged.battery = battery(Some(80), Some(false));
        let mut unread = device("A", 2, DeviceState::Disconnected);
        unread.read_error = Some(p::Error {
            code: ErrorCode::NotFound as i32,
            ..Default::default()
        });
        let mut away = device("A", 3, DeviceState::Connected);
        away.available = false;
        let mut blocked = device("A", 4, DeviceState::Disconnected);
        blocked.d.inactive = Some(InactiveReason::Blocked as i32);
        let mut failed = device("A", 5, DeviceState::Disconnected);
        failed.d.errors = vec![p::Error {
            code: ErrorCode::NotFound as i32,
            ..Default::default()
        }];
        let model = tray(
            &[adapter("A")],
            &[
                charged,
                unread,
                away,
                blocked,
                failed,
                device("A", 6, DeviceState::Connecting),
                device("A", 7, DeviceState::Disconnecting),
            ],
            false,
        );
        let rows: Vec<_> = model.groups[0]
            .devices
            .iter()
            .map(|d| d.label.as_str())
            .collect();
        assert_eq!(rows, ["1 · 80%", "2", "3", "4", "5", "6", "7"]);
    }

    #[test]
    fn lists_one_adapter_as_a_submenu_above_its_devices() {
        let mut d = device("A", 1, DeviceState::Disconnected);
        d.battery = battery(Some(50), Some(false));
        let model = tray(&[adapter("A")], &[d], false);
        let (item, _) = item(model.clone());
        assert_eq!(
            labels(&item.menu()),
            [
                "Adapter A [Disconnect true, Open true, Add Device true]",
                "1 · 50% [Connect true, Open true]",
                "-",
                "Open Cordial true",
                "-",
                "Settings true",
                "Quit true",
            ]
        );
        let a = || Page::Adapter("A".into());
        assert_eq!(
            click(model.clone(), 0, 0),
            [ServiceEvent::Act(
                a(),
                Action::DisconnectAdapter("A".into())
            )]
        );
        assert_eq!(
            click(model.clone(), 0, 1),
            [ServiceEvent::Present(Some(a()))]
        );
        assert_eq!(
            click(model.clone(), 0, 2),
            [
                ServiceEvent::Present(None),
                ServiceEvent::Act(Page::Overview, Action::AddDeviceTo("A".into())),
            ]
        );
        assert_eq!(
            click(model, 1, 0),
            [ServiceEvent::Act(
                Page::Device("A".into(), 1),
                Action::Connect
            )]
        );
    }

    #[test]
    fn gives_each_adapter_its_own_actions() {
        let waiting = AdapterView {
            ready: false,
            ..adapter("C")
        };
        let model = tray(
            &[adapter("A"), disconnected("B"), waiting],
            &[
                device("A", 1, DeviceState::Connected),
                device("C", 1, DeviceState::Connected),
            ],
            false,
        );
        let (item, _) = item(model.clone());
        assert_eq!(
            labels(&item.menu()),
            [
                "Adapter A [Disconnect true, Open true, Add Device true]",
                "1 [Disconnect true, Open true]",
                "-",
                "Adapter B [Connect true, Open true, Add Device false]",
                "-",
                "Adapter C [Disconnect true, Open true, Add Device false]",
                "1 [Disconnect false, Open true]",
                "-",
                "Open Cordial true",
                "-",
                "Settings true",
                "Quit true",
            ]
        );
        let b = || Page::Adapter("B".into());
        assert_eq!(
            click(model.clone(), 3, 0),
            [ServiceEvent::Act(b(), Action::ConnectAdapter("B".into()))]
        );
        assert_eq!(
            click(model.clone(), 3, 1),
            [ServiceEvent::Present(Some(b()))]
        );
        assert_eq!(
            click(model, 5, 0),
            [ServiceEvent::Act(
                Page::Adapter("C".into()),
                Action::DisconnectAdapter("C".into())
            )]
        );
    }

    #[test]
    fn offers_connect_to_a_disconnected_adapter_while_always_shown() {
        let model = tray(&[disconnected("A")], &[], true);
        assert!(model.visible);
        assert_eq!(model.empty, Some("No Adapter Connected"));
        let (item, _) = item(model.clone());
        assert_eq!(
            labels(&item.menu())[..2],
            [
                "No Adapter Connected false",
                "Adapter A [Connect true, Open true, Add Device false]",
            ]
        );
        assert_eq!(
            click(model, 1, 0),
            [ServiceEvent::Act(
                Page::Adapter("A".into()),
                Action::ConnectAdapter("A".into())
            )]
        );
    }

    #[test]
    fn holds_connect_while_an_adapter_connects_or_migrates() {
        let toggles = |model: &Model| -> Vec<(bool, bool)> {
            model
                .groups
                .iter()
                .map(|g| (g.connected, g.can_toggle))
                .collect()
        };
        let opening = AdapterView {
            conn: Conn::Connecting,
            status: None,
            ..adapter("B")
        };
        let model = tray(&[adapter("A"), opening, disconnected("C")], &[], false);
        assert_eq!(
            toggles(&model),
            [(true, true), (false, false), (false, true)]
        );
        for storage in [
            Storage::Migrating {
                waiting: false,
                cancelling: false,
                progress: None,
            },
            Storage::Failed {
                text: String::new(),
                reason: String::new(),
            },
        ] {
            let migrating = AdapterView {
                conn: Conn::Connecting,
                ready: false,
                storage,
                ..adapter("A")
            };
            let model = tray(&[migrating, disconnected("B")], &[], true);
            assert_eq!(toggles(&model), [(false, false), (false, false)]);
        }
    }

    #[test]
    fn names_what_is_missing_when_there_are_no_devices_to_list() {
        let a = [adapter("A")];
        assert_eq!(
            tray(&a, &[device("A", 1, DeviceState::Disconnected)], false).empty,
            None
        );
        assert_eq!(tray(&a, &[], false).empty, Some("No Saved Devices"));
        assert_eq!(tray(&[], &[], false).empty, Some("No Adapter Connected"));
        let connecting = AdapterView {
            conn: Conn::Connecting,
            status: None,
            ..adapter("A")
        };
        assert_eq!(
            tray(
                &[connecting, disconnected("B")],
                &[device("A", 1, DeviceState::Disconnected)],
                false
            )
            .empty,
            Some("No Adapter Connected")
        );
    }

    #[test]
    fn keeps_a_reconnecting_adapter_listed() {
        let held = AdapterView {
            conn: Conn::Connecting,
            ready: false,
            ..adapter("A")
        };
        let model = tray(&[held], &[], false);
        assert!(model.visible);
        assert_eq!(model.empty, Some("No Saved Devices"));
        assert!(!model.groups[0].can_add);
    }

    #[test]
    fn offers_connect_only_through_a_ready_adapter() {
        let d = [device("A", 1, DeviceState::Disconnected)];
        let ready = tray(&[adapter("A")], &d, false);
        assert!(ready.groups[0].devices[0].can_connect);
        assert!(ready.groups[0].can_add);
        let waiting = AdapterView {
            ready: false,
            ..adapter("A")
        };
        let waiting = tray(&[waiting], &d, false);
        assert!(!waiting.groups[0].devices[0].can_connect);
        assert!(!waiting.groups[0].can_add);
        let connecting = tray(
            &[adapter("A")],
            &[device("A", 1, DeviceState::Connecting)],
            false,
        );
        assert!(!connecting.groups[0].devices[0].can_connect);
    }

    #[test]
    fn escapes_mnemonics() {
        assert_eq!(mnemonic("My_Keyboard"), "My__Keyboard");
    }

    #[test]
    fn embeds_every_tray_image() {
        for base in [Base::Idle, Base::Connected] {
            for badge in [Badge::None, Badge::Low, Badge::Attention] {
                for variant in ["light", "dark"] {
                    let name = name(base, badge, variant);
                    assert!(ICONS.iter().any(|(n, ..)| *n == name), "{name}");
                }
            }
        }
    }
}
