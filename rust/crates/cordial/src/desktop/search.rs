//! What the sidebar's search finds: pages, adapters, devices, the rows on their pages that change
//! a setting or show a fact, and profiles, read from the model so pages that aren't open are
//! found too.
use super::{
    adapter,
    device::{self, SWITCHES},
    services::SETTINGS,
    settings,
    widgets::{self, ADAPTER_ICON, Item},
};
use crate::{
    model, profiles,
    ui::app::{App, Live, Page, Tab, words},
};
use cordial_protocol::{ConfigurationInterface, Transport};

/// Where a result goes: a page and tab, or Settings without a page, and the row to show there.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub page: Option<Page>,
    pub tab: Tab,
    pub row: Option<Row>,
}

/// A row to show: the first one with a title, the one with a widget name where titles can
/// repeat, the first row of the group with a title, or the row with a title in the group with
/// a widget name, for groups that repeat their rows' titles.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Title(String),
    Name(String),
    Group(String),
    In(String, String),
}

/// The widget name of the adapter's library row for a profile.
pub fn profile_row(id: u32) -> String {
    format!("profile-{id}")
}

/// The widget name of the profile row of an adapter's configuration interface.
pub fn interface_row(i: ConfigurationInterface) -> String {
    format!("interface-{}", i as i32)
}

/// The widget name of an adapter's group for a transport.
pub fn transport_group(t: Transport) -> String {
    format!("transport-{}", t as i32)
}

/// The widget name of a device's layer row, by its place among the layers.
pub fn layer_row(index: usize) -> String {
    format!("layer-{index}")
}

/// A search result: what it is called, where it is, and where it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub icon: &'static str,
    pub title: String,
    /// Where the result is, such as the device a setting belongs to.
    pub context: String,
    pub target: Target,
}

const PROFILES_ICON: &str = "view-list-symbolic";
const DIAGNOSTICS_ICON: &str = "applications-engineering-symbolic";
const SETTINGS_ICON: &str = "preferences-system-symbolic";

/// Fact labels that only make sense under their group's title, so the group is found instead.
const GROUPED: [&str; 2] = ["Status", "Slot"];

/// The titles of the facts among `items`, leaving out rows of text such as messages, each once.
fn facts(items: impl IntoIterator<Item = Item>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        if !i.value.is_empty() && !GROUPED.contains(&i.title.as_str()) && !out.contains(&i.title) {
            out.push(i.title);
        }
    }
    out
}

/// Everything search can find, in sidebar order.
pub fn index(app: &App<Live>) -> Vec<Hit> {
    let hit = |icon, title: &str, context: &str, page: Option<Page>, tab, row: bool| Hit {
        icon,
        title: title.to_owned(),
        context: context.to_owned(),
        target: Target {
            page,
            tab,
            row: row.then(|| Row::Title(title.to_owned())),
        },
    };
    // A group, shown by its first row.
    let group = |icon, title: &str, context: &str, page: Option<Page>, tab| Hit {
        icon,
        title: title.to_owned(),
        context: context.to_owned(),
        target: Target {
            page,
            tab,
            row: Some(Row::Group(title.to_owned())),
        },
    };
    // A row on the Profiles tab, found by its widget name since profiles can share a name.
    let profile = |title: String, context: &str, page: Option<Page>, row: String| Hit {
        icon: PROFILES_ICON,
        title,
        context: context.to_owned(),
        target: Target {
            page,
            tab: Tab::Profiles,
            row: Some(Row::Name(row)),
        },
    };
    let mut out = vec![
        hit(
            "go-home-symbolic",
            "Overview",
            "",
            Some(Page::Overview),
            Tab::Details,
            false,
        ),
        hit(SETTINGS_ICON, "Settings", "", None, Tab::Details, false),
    ];
    out.extend(
        SETTINGS
            .iter()
            .map(|t| hit(SETTINGS_ICON, t, "Settings", None, Tab::Details, true)),
    );
    let adapter_name = |id: &str| app.adapter(id).map_or("", |a| a.name.as_str());
    let names: Vec<(&str, &str)> = app
        .devices()
        .iter()
        .map(|d| (d.name.as_str(), adapter_name(&d.adapter)))
        .collect();
    let devices = |id: Option<&str>| {
        app.devices()
            .iter()
            .filter(move |d| match id {
                Some(id) => d.adapter == id,
                None => app.adapter(&d.adapter).is_none(),
            })
            .flat_map(|d| {
                let page = Some(d.page());
                let icon = widgets::device_icon(d.kind);
                let context = context(&names, (d.name.as_str(), adapter_name(&d.adapter)));
                let mut out = vec![hit(
                    icon,
                    &d.name,
                    adapter_name(&d.adapter),
                    page.clone(),
                    Tab::Details,
                    false,
                )];
                let shown = facts(device::battery(d))
                    .into_iter()
                    .chain(SWITCHES.iter().map(|(label, _)| (*label).to_owned()))
                    .chain(settings::titles(app, d))
                    .chain(facts(device::information(app, d)));
                for title in shown {
                    out.push(hit(
                        icon,
                        &title,
                        &context,
                        page.clone(),
                        Tab::Details,
                        true,
                    ));
                }
                let profiles = app.adapter(&d.adapter).is_some_and(|a| a.profile_support())
                    && d.d.profiles.is_some();
                if profiles {
                    out.push(hit(
                        PROFILES_ICON,
                        Tab::Profiles.label(),
                        &context,
                        page.clone(),
                        Tab::Profiles,
                        false,
                    ));
                    let layers = app.device_values(d).layers.unwrap_or_default();
                    for (i, id) in layers.into_iter().enumerate() {
                        out.push(profile(
                            app.profile_text(&d.adapter, id),
                            &context,
                            page.clone(),
                            layer_row(i),
                        ));
                    }
                }
                out.push(hit(
                    DIAGNOSTICS_ICON,
                    Tab::Diagnostics.label(),
                    &context,
                    page.clone(),
                    Tab::Diagnostics,
                    false,
                ));
                // The Profiles group holds only why a connected device's profiles didn't load.
                let profiles_failed = model::profile_error(&d.d).is_some() && d.connected();
                let diagnostics = [
                    ("", device::last_error(d).into_iter().collect()),
                    (
                        "Profiles",
                        profiles_failed
                            .then(|| Item::fact("Status", "Not Loaded"))
                            .into_iter()
                            .collect(),
                    ),
                    ("Logitech Features", device::hidpp(d)),
                    ("Gamepad Output", device::gamepad(d)),
                    ("Security", device::security(d)),
                    ("Identifiers", device::identifiers(d)),
                ];
                for (title, items) in diagnostics {
                    if !title.is_empty() && !items.is_empty() {
                        out.push(group(
                            DIAGNOSTICS_ICON,
                            title,
                            &context,
                            page.clone(),
                            Tab::Diagnostics,
                        ));
                    }
                    for fact in facts(items) {
                        out.push(hit(
                            DIAGNOSTICS_ICON,
                            &fact,
                            &context,
                            page.clone(),
                            Tab::Diagnostics,
                            true,
                        ));
                    }
                }
                out
            })
            .collect::<Vec<_>>()
    };
    for a in app.adapters() {
        let page = Some(Page::Adapter(a.id.clone()));
        out.push(hit(
            ADAPTER_ICON,
            &a.name,
            "",
            page.clone(),
            Tab::Details,
            false,
        ));
        // The settings and the groups after them show only while the adapter is ready.
        let mut shown = Vec::new();
        if let Some(live) = a.live() {
            out.push(hit(
                ADAPTER_ICON,
                "Platform",
                &a.name,
                page.clone(),
                Tab::Details,
                true,
            ));
            for t in model::transports(live) {
                let name = words::transport_name(t);
                out.push(hit(
                    ADAPTER_ICON,
                    name,
                    &a.name,
                    page.clone(),
                    Tab::Details,
                    true,
                ));
                // Every transport's group has the same facts, so each is found in its group.
                let context = format!("{} · {name}", a.name);
                for fact in facts(adapter::transport_facts(live, t)) {
                    out.push(Hit {
                        icon: ADAPTER_ICON,
                        context: context.clone(),
                        target: Target {
                            page: page.clone(),
                            tab: Tab::Details,
                            row: Some(Row::In(transport_group(t), fact.clone())),
                        },
                        title: fact,
                    });
                }
            }
            let usb = adapter::usb(live);
            if !usb.is_empty() {
                out.push(group(
                    ADAPTER_ICON,
                    "USB",
                    &a.name,
                    page.clone(),
                    Tab::Details,
                ));
            }
            shown.extend(facts(usb));
        }
        shown.extend(facts(adapter::information(a)));
        for title in shown {
            out.push(hit(
                ADAPTER_ICON,
                &title,
                &a.name,
                page.clone(),
                Tab::Details,
                true,
            ));
        }
        if a.profile_support() {
            out.push(hit(
                PROFILES_ICON,
                Tab::Profiles.label(),
                &a.name,
                page.clone(),
                Tab::Profiles,
                false,
            ));
            // The Profiles tab shows the configuration interfaces and the listed profiles while
            // the adapter is ready. An interface goes to its profile row, which can be changed
            // while the interface's switch can't.
            for s in a.view().map(profiles::interfaces).unwrap_or_default() {
                if let Ok(i) = ConfigurationInterface::try_from(s.interface) {
                    out.push(profile(
                        profiles::interface_label(i).to_owned(),
                        &a.name,
                        page.clone(),
                        interface_row(i),
                    ));
                }
            }
            let list = a.view().and(app.shown_page(&a.id, false));
            for p in list.iter().flat_map(|l| l.all().0) {
                out.push(profile(
                    app.profile_text(&a.id, p.id),
                    &a.name,
                    page.clone(),
                    profile_row(p.id),
                ));
            }
        }
        out.push(hit(
            DIAGNOSTICS_ICON,
            Tab::Diagnostics.label(),
            &a.name,
            page.clone(),
            Tab::Diagnostics,
            false,
        ));
        let ids = adapter::identifiers(a);
        if !ids.is_empty() {
            out.push(group(
                DIAGNOSTICS_ICON,
                "Identifiers",
                &a.name,
                page.clone(),
                Tab::Diagnostics,
            ));
        }
        for fact in facts(ids) {
            out.push(hit(
                DIAGNOSTICS_ICON,
                &fact,
                &a.name,
                page.clone(),
                Tab::Diagnostics,
                true,
            ));
        }
        out.extend(devices(Some(a.id.as_str())));
    }
    out.extend(devices(None));
    out
}

/// A device's context: its name, with its adapter's when another device has the same name.
fn context(names: &[(&str, &str)], (name, adapter): (&str, &str)) -> String {
    let shared = names.iter().filter(|(n, _)| *n == name).count() > 1;
    if shared && !adapter.is_empty() {
        format!("{name} · {adapter}")
    } else {
        name.to_owned()
    }
}

/// The results for `query`: every hit whose title or context holds each of its words, ignoring
/// case. Hits whose title holds them all come first.
pub fn find(index: &[Hit], query: &str) -> Vec<Hit> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let (mut titled, mut placed) = (Vec::new(), Vec::new());
    for h in index {
        let title = h.title.to_lowercase();
        let context = h.context.to_lowercase();
        if terms.iter().all(|t| title.contains(t.as_str())) {
            titled.push(h.clone());
        } else if terms
            .iter()
            .all(|t| title.contains(t.as_str()) || context.contains(t.as_str()))
        {
            placed.push(h.clone());
        }
    }
    titled.extend(placed);
    titled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str, context: &str) -> Hit {
        Hit {
            icon: "",
            title: title.into(),
            context: context.into(),
            target: Target {
                page: None,
                tab: Tab::Details,
                row: None,
            },
        }
    }

    fn titles(hits: &[Hit]) -> Vec<(&str, &str)> {
        hits.iter()
            .map(|h| (h.title.as_str(), h.context.as_str()))
            .collect()
    }

    #[test]
    fn finds_every_word_in_the_title_or_context() {
        let index = [
            hit("Automatic Connections", "MX Keys"),
            hit("Pointer Speed", "MX Master"),
            hit("MX Master", "Cordial"),
            hit("Connections", "Settings"),
        ];
        assert_eq!(
            titles(&find(&index, "  mx  ")),
            [
                ("MX Master", "Cordial"),
                ("Automatic Connections", "MX Keys"),
                ("Pointer Speed", "MX Master")
            ],
            "title matches come first, then sidebar order"
        );
        assert_eq!(
            titles(&find(&index, "CONNECTIONS keys")),
            [("Automatic Connections", "MX Keys")]
        );
        assert_eq!(
            titles(&find(&index, "connections")),
            [
                ("Automatic Connections", "MX Keys"),
                ("Connections", "Settings")
            ]
        );
        assert!(find(&index, "").is_empty());
        assert!(find(&index, "   ").is_empty());
        assert!(find(&index, "speed keys").is_empty());
    }

    /// Runs `check` with the adapter's ID once the simulated adapter is ready and its profiles
    /// and its mouse's settings are read.
    fn when_ready(world: &crate::sim::Desktop, check: impl FnOnce(&App<Live>, &str)) {
        use std::{sync::Arc, time::Duration};
        let id = world.ports()[0].serial.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(world.live(Arc::new(move |msg| {
            let _ = tx.send(msg);
        })));
        let next = |app: &mut App<Live>| {
            let msg = rx.recv_timeout(Duration::from_secs(5));
            app.update(msg.expect("the simulated adapter answers"));
        };
        let ready = |app: &App<Live>| {
            app.adapter(&id).and_then(|a| a.view()).is_some()
                && app.shown_page(&id, false).is_some_and(|p| p.listed)
                && app
                    .device(&id, 1)
                    .is_some_and(|d| !app.settings_of(d).is_empty())
        };
        while !ready(&app) {
            next(&mut app);
        }
        check(&app, &id);
        app.quit();
        while !app.done() {
            next(&mut app);
        }
    }

    #[test]
    fn finds_an_adapters_configuration_interfaces() {
        when_ready(&crate::sim::Desktop::new(1), |app, id| {
            let name = &app.adapter(id).unwrap().name;
            let hits = find(&index(app), &format!("vial {name}"));
            assert_eq!(
                hits.first().map(|h| (h.title.as_str(), &h.target)),
                Some((
                    "Vial",
                    &Target {
                        page: Some(Page::Adapter(id.to_owned())),
                        tab: Tab::Profiles,
                        row: Some(Row::Name(interface_row(ConfigurationInterface::Vial))),
                    }
                ))
            );
            assert!(find(&index(app), "via").iter().any(|h| h.title == "VIA"));
        });
    }

    #[test]
    fn finds_profiles_and_the_layers_that_use_them() {
        when_ready(&crate::sim::Desktop::new(1), |app, id| {
            let name = &app.adapter(id).unwrap().name;
            let keys = app.device(id, 2).unwrap();
            let found: Vec<_> = find(&index(app), "typing")
                .into_iter()
                .map(|h| (h.title, h.context, h.target))
                .collect();
            assert_eq!(
                found,
                [
                    (
                        "Typing".to_owned(),
                        name.clone(),
                        Target {
                            page: Some(Page::Adapter(id.to_owned())),
                            tab: Tab::Profiles,
                            row: Some(Row::Name(profile_row(1))),
                        }
                    ),
                    (
                        "Typing".to_owned(),
                        keys.name.clone(),
                        Target {
                            page: Some(keys.page()),
                            tab: Tab::Profiles,
                            row: Some(Row::Name(layer_row(0))),
                        }
                    ),
                ],
                "the library row, then the keyboard's layer"
            );
            assert_eq!(
                find(&index(app), "example profile 11")
                    .iter()
                    .map(|h| &h.target.row)
                    .collect::<Vec<_>>(),
                [&Some(Row::Name(profile_row(11)))],
                "a profile past the first ten is found"
            );
        });
    }

    #[test]
    fn finds_settings_the_device_cant_apply_now() {
        use cordial_protocol::{self as p, ErrorCode, settings_object::Group};
        let world = crate::sim::Desktop::new(1);
        {
            let mut sim = world.adapters()[0].0.lock().unwrap();
            for object in sim.settings.get_mut(&1).expect("the mouse has settings") {
                match &mut object.group {
                    Some(Group::PointerSensor(sensor)) => {
                        sensor.dpi.as_mut().unwrap().status = Some(p::SettingStatus {
                            result: Some(p::setting_status::Result::Error(crate::sim::refusal(
                                ErrorCode::Unsupported,
                            ))),
                        });
                    }
                    Some(Group::Wheel(wheel)) => {
                        wheel.free_spin_threshold.as_mut().unwrap().value =
                            Some(crate::ui::app::settings::FREE_SPIN_OFF);
                    }
                    _ => {}
                }
            }
        }
        when_ready(&world, |app, _| {
            let found = |query| find(&index(app), query);
            assert!(
                found("pointer sensitivity example mouse")
                    .iter()
                    .any(|h| h.title == "Pointer Sensitivity"),
                "a setting the device can't apply now still has its control"
            );
            assert!(
                found("free spin")
                    .iter()
                    .any(|h| h.title == "Automatic Free Spin"),
                "free spin's switch shows while it is off"
            );
            assert!(
                found("free spin threshold").is_empty(),
                "the threshold's row is hidden while free spin is off"
            );
        });
    }

    #[test]
    fn finds_facts_and_groups_on_details_and_diagnostics() {
        when_ready(&crate::sim::Desktop::new(1), |app, id| {
            let name = &app.adapter(id).unwrap().name;
            let mouse = app.device(id, 1).unwrap();
            let target = |query: &str, title: &str, context: &str| {
                find(&index(app), query)
                    .into_iter()
                    .find(|h| h.title == title && h.context == context)
                    .map(|h| h.target)
            };
            let at = |page: Page, tab, row: Row| {
                Some(Target {
                    page: Some(page),
                    tab,
                    row: Some(row),
                })
            };
            let adapter = Page::Adapter(id.to_owned());
            assert_eq!(
                target("battery", "Battery", &mouse.name),
                at(mouse.page(), Tab::Details, Row::Title("Battery".into()))
            );
            assert_eq!(
                target("firmware", "Firmware", name),
                at(adapter.clone(), Tab::Details, Row::Title("Firmware".into()))
            );
            assert_eq!(
                target("adapter id", "Adapter ID", name),
                at(adapter, Tab::Diagnostics, Row::Title("Adapter ID".into())),
                "a ready adapter is identified by its ID, as its handshake's serial number shows \
                 only while the adapter is refused"
            );
            assert_eq!(
                target("identifiers", "Identifiers", &mouse.name),
                at(
                    mouse.page(),
                    Tab::Diagnostics,
                    Row::Group("Identifiers".into())
                )
            );
            assert!(
                !find(&index(app), "status")
                    .iter()
                    .any(|h| h.title == "Status"),
                "a fact named only by its group is found through the group"
            );
            assert!(find(&index(app), "72").is_empty(), "values aren't searched");
        });
    }

    #[test]
    fn finds_readings_beside_settings() {
        use crate::{settings::Leaf, ui::app::settings::readings, ui::catalog};
        when_ready(&crate::sim::Desktop::new(1), |app, id| {
            let found = |device: u32, title: &str| {
                let d = app.device(id, device).unwrap();
                find(&index(app), title).iter().any(|h| {
                    h.title == title && h.context == d.name && h.target.tab == Tab::Details
                })
            };
            assert!(found(1, "Ratchets per Rotation"), "a wheel figure is found");
            let (brightness, _) = readings(&app.device(id, 2).unwrap().d)
                .into_iter()
                .find(|(f, _)| f.leaf == Leaf::CurrentBrightness)
                .expect("the keyboard reports its brightness");
            assert!(found(2, &catalog::label(&brightness)), "a reading is found");
        });
    }

    #[test]
    fn finds_each_transports_facts_in_its_group() {
        when_ready(&crate::sim::Desktop::new(1), |app, id| {
            let name = &app.adapter(id).unwrap().name;
            let found: Vec<_> = find(&index(app), "active devices")
                .into_iter()
                .filter(|h| h.target.page == Some(Page::Adapter(id.to_owned())))
                .map(|h| (h.context, h.target.row))
                .collect();
            let row = |t| Some(Row::In(transport_group(t), "Active Devices".into()));
            assert_eq!(
                found,
                [
                    (
                        format!("{name} · Bluetooth Classic"),
                        row(Transport::Classic)
                    ),
                    (format!("{name} · Bluetooth LE"), row(Transport::Ble)),
                ]
            );
        });
    }

    #[test]
    fn devices_with_the_same_name_name_their_adapter() {
        let names = [("MX Keys", "Desk"), ("MX Keys", "Laptop"), ("M720", "Desk")];
        assert_eq!(context(&names, ("MX Keys", "Laptop")), "MX Keys · Laptop");
        assert_eq!(context(&names, ("M720", "Desk")), "M720");
        assert_eq!(context(&names, ("MX Keys", "")), "MX Keys");
    }
}
