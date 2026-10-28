//! The Overview: the adapters, what needs attention, and the connected devices.
use super::{
    shell::Ctx,
    widgets::{self, ADAPTER_ICON, Item, List, Look, Tab as TabView},
    window::{Head, Tabs, View},
};
use crate::ui::app::{Action, App, Live, Page, Tab};
use adw::prelude::*;
use std::rc::Rc;

pub struct Overview {
    tabs: Tabs,
    tab: TabView,
    empty: adw::StatusPage,
    attention: List,
    connected: List,
    adapters: List,
}

impl Overview {
    pub fn new(ctx: &Rc<Ctx>) -> Self {
        let tab = TabView::new();
        let attention = List::new(ctx, "Needs Attention");
        let connected = List::new(ctx, "Connected");
        let adapters = List::new(ctx, "");
        for list in [&adapters, &attention, &connected] {
            tab.add(&list.group);
        }
        let empty = adw::StatusPage::builder()
            .icon_name(ADAPTER_ICON)
            .title("No Adapter Found")
            .vexpand(true)
            .build();
        let mut tabs = Tabs::new(ctx);
        tabs.add(Tab::Details, &tab.widget);
        Self {
            tabs,
            tab,
            empty,
            attention,
            connected,
            adapters,
        }
    }
}

impl View for Overview {
    fn tabs(&self) -> &Tabs {
        &self.tabs
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) -> Option<Head> {
        self.tabs.show(&[Tab::Details], Tab::Details);
        let head = Head {
            title: "Overview".into(),
            subtitle: String::new(),
        };
        if app.adapters().is_empty() {
            self.empty.set_visible(app.listed);
            self.tab.show(Some(&self.empty.clone().upcast()));
            return Some(head);
        }
        self.tab.show(None);
        let adapters = app.adapters().iter().filter(|a| a.connected()).count();

        let alerts: Vec<Item> = app
            .alerts()
            .into_iter()
            .map(|a| {
                // A device's unloaded profiles are explained on its Diagnostics tab.
                let profiles = match &a.page {
                    Page::Device(adapter, id) => {
                        app.device(adapter, *id).is_some_and(|d| {
                            d.connected() && crate::model::profile_error(&d.d).is_some()
                        }) && !a.detail.starts_with("Battery")
                    }
                    _ => false,
                };
                let item = Item::link(a.name, "", Action::Show(a.page))
                    .look(Look::Error)
                    .icon("dialog-warning-symbolic");
                if profiles {
                    item.value("Profiles Not Loaded").tab(Tab::Diagnostics)
                } else {
                    item.value(a.detail)
                }
            })
            .collect();
        self.attention.set(&alerts);

        let several = adapters > 1;
        let devices: Vec<Item> = app
            .devices()
            .iter()
            .filter(|d| d.connected())
            .map(|d| {
                let adapter = if several {
                    app.adapter(&d.adapter)
                        .map(|a| a.name.clone())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let mut item = Item::link(d.name.clone(), adapter, Action::Show(d.page()))
                    .icon(widgets::device_icon(d.kind));
                if let Some(b) = &d.battery
                    && let Some(text) = b.text()
                {
                    item = item.value(text).look(match () {
                        _ if b.low() => Look::Error,
                        _ if b.stale() => Look::Dim,
                        _ => Look::Plain,
                    });
                }
                item
            })
            .collect();
        self.connected.set(&devices);

        let adapters: Vec<Item> = app
            .adapters()
            .iter()
            .map(|a| {
                let (status, attention) = a.status_text();
                let counts = if a.connected() && app.devices_listed(&a.id) {
                    let paired = app.devices().iter().filter(|d| d.adapter == a.id).count();
                    let on = app
                        .devices()
                        .iter()
                        .filter(|d| d.adapter == a.id && d.connected())
                        .count();
                    format!("{paired} paired · {on} connected")
                } else {
                    String::new()
                };
                Item::link(
                    a.name.clone(),
                    counts,
                    Action::Show(Page::Adapter(a.id.clone())),
                )
                .value(status)
                .look(if attention { Look::Warning } else { Look::Dim })
                .icon(ADAPTER_ICON)
            })
            .collect();
        self.adapters.set(&adapters);
        Some(head)
    }
}
