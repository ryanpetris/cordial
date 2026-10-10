//! The adapters and devices as the TUI shows them, built from every session's view the way the
//! desktop application builds its state: connected adapters, adapters held while they may
//! return, and adapters the user disconnected while they stay plugged in; and every saved device
//! of every connected adapter in one list.
use super::{
    Model, Page, Tab,
    fleet::Fleet,
    words::{self, Battery, DisplayKind},
};
use crate::{model, profiles, view::State};
use cordial_protocol::{self as p, keys};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Conn {
    Connected,
    Connecting,
    Disconnected,
}

#[derive(Clone, Debug)]
pub struct AdapterView {
    pub id: String,
    pub name: String,
    pub conn: Conn,
    /// Ready, with its saved devices loaded.
    pub ready: bool,
    /// The adapter's status: current while connected, last known while held. None while
    /// disconnected.
    pub status: Option<p::Status>,
    pub attention: Vec<String>,
    /// Opening finished without the adapter becoming usable.
    pub failed: bool,
    pub connect_error: Option<String>,
}

impl AdapterView {
    pub fn connected(&self) -> bool {
        self.conn == Conn::Connected
    }

    /// The adapter's status when it is ready: shown, and locked unless connected.
    pub fn view(&self) -> Option<&p::Status> {
        self.status.as_ref().filter(|s| s.ready)
    }

    /// The status of a connected adapter that is ready.
    pub fn live(&self) -> Option<&p::Status> {
        self.view().filter(|_| self.connected())
    }

    /// The adapter's status in a few words, and whether it needs attention.
    pub fn status_text(&self) -> (&'static str, bool) {
        match self.conn {
            Conn::Disconnected => ("Disconnected", false),
            Conn::Connecting => ("Connecting…", false),
            Conn::Connected if self.failed => ("Needs Attention", true),
            Conn::Connected if !self.ready => ("Starting…", false),
            Conn::Connected if !self.attention.is_empty() => ("Needs Attention", true),
            Conn::Connected => ("Ready", false),
        }
    }

    pub fn profile_support(&self) -> bool {
        self.status.as_ref().is_some_and(profiles::available)
    }
}

#[derive(Clone, Debug)]
pub struct DeviceView {
    pub adapter: String,
    pub d: p::Device,
    pub name: String,
    pub kind: DisplayKind,
    pub battery: Option<Battery>,
}

impl DeviceView {
    pub fn page(&self) -> Page {
        Page::Device(self.adapter.clone(), self.d.id)
    }

    pub fn connected(&self) -> bool {
        model::connected(&self.d)
    }

    pub fn low(&self) -> bool {
        self.battery.as_ref().is_some_and(Battery::low)
    }
}

/// Name order the way a person reads a list: without regard to case, then by key.
fn order(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b))
}

fn adapter_name(status: &p::Status, id: &str) -> String {
    let name = words::clean(&status.name);
    if name.is_empty() { id.to_owned() } else { name }
}

impl<F: Fleet> Model<F> {
    /// Takes every session's current view and rebuilds the adapters and devices shown. The page
    /// returns to the Overview once its target, after having been shown, goes away.
    pub(super) fn refresh(&mut self) {
        let now = Instant::now();
        self.held.retain(|_, h| h.until > now);
        self.states.clear();
        for s in &mut self.sessions {
            if let Some(st) = self.fleet.state(s.slot) {
                s.listed |= st.loaded;
                self.states.insert(s.slot, st);
            }
        }
        let mut adapters = Vec::new();
        for s in &self.sessions {
            let Some(st) = self.states.get(&s.slot) else {
                continue;
            };
            let mut attention = Vec::new();
            if st.status.ready && model::storage_full(&st.status) {
                attention.push(words::STORAGE_FULL_ATTENTION.to_owned());
            }
            if let Some(why) = &s.unready {
                attention.push(why.clone());
            }
            adapters.push(AdapterView {
                id: s.id.clone(),
                name: adapter_name(&st.status, &s.id),
                conn: Conn::Connected,
                ready: st.ready(),
                status: Some(st.status.clone()),
                attention,
                failed: s.unready.is_some(),
                connect_error: None,
            });
        }
        for (id, h) in &self.held {
            if self.sessions.iter().any(|s| &s.id == id) {
                continue;
            }
            adapters.push(AdapterView {
                id: id.clone(),
                name: adapter_name(&h.status, id),
                conn: Conn::Connecting,
                ready: false,
                status: Some(h.status.clone()),
                attention: Vec::new(),
                failed: false,
                connect_error: None,
            });
        }
        for (id, o) in &self.off {
            if o.port.is_none() {
                continue;
            }
            adapters.push(AdapterView {
                id: id.clone(),
                name: adapter_name(&o.status, id),
                conn: if o.connecting.is_some() || o.waiting {
                    Conn::Connecting
                } else {
                    Conn::Disconnected
                },
                ready: false,
                status: None,
                attention: Vec::new(),
                failed: false,
                connect_error: o.error.clone(),
            });
        }
        adapters.sort_by(|a, b| order(&a.name, &b.name).then_with(|| a.id.cmp(&b.id)));
        self.adapters = adapters;

        let mut devices = Vec::new();
        for s in &self.sessions {
            // Devices stay listed while the adapter isn't ready, once listed in this session.
            let Some(st) = self.states.get(&s.slot).filter(|_| s.listed) else {
                continue;
            };
            for d in &st.devices {
                devices.push(DeviceView {
                    adapter: s.id.clone(),
                    name: words::device_name(d),
                    kind: words::display_kind(&d.kinds, &d.roles),
                    battery: Battery::of(d),
                    d: d.clone(),
                });
            }
        }
        devices.sort_by(|a, b| {
            order(&a.name, &b.name)
                .then_with(|| a.adapter.cmp(&b.adapter))
                .then(a.d.id.cmp(&b.d.id))
        });
        // A device's connection errors belong to its state at the time.
        for d in &devices {
            let key = (d.adapter.clone(), d.d.id);
            if self.device_states.insert(key.clone(), d.d.state) != Some(d.d.state) {
                self.notes.remove(&super::Spot::DeviceBar(key.0, key.1));
            }
        }
        self.devices = devices;

        if self.exists(&self.page) {
            self.page_shown = true;
        } else if self.page_shown {
            self.page = Page::Overview;
            self.tab = Tab::Details;
            self.main_scroll = 0;
            self.notes.clear();
            self.editing = None;
        }
    }

    fn exists(&self, page: &Page) -> bool {
        match page {
            Page::Overview => true,
            Page::Device(a, id) => self.device(a, *id).is_some(),
            Page::Adapter(a) => self.adapter(a).is_some(),
        }
    }

    /// The page shown: the selection, or the Overview while its target isn't there.
    pub(super) fn shown(&self) -> Page {
        if self.exists(&self.page) {
            self.page.clone()
        } else {
            Page::Overview
        }
    }

    pub(super) fn adapter(&self, id: &str) -> Option<&AdapterView> {
        self.adapters.iter().find(|a| a.id == id)
    }

    pub(super) fn device(&self, adapter: &str, id: u32) -> Option<&DeviceView> {
        self.devices
            .iter()
            .find(|d| d.adapter == adapter && d.d.id == id)
    }

    /// Adapters that can add a device: connected and ready.
    pub(super) fn ready_adapters(&self) -> Vec<&AdapterView> {
        self.adapters
            .iter()
            .filter(|a| a.connected() && a.ready)
            .collect()
    }

    /// A profile's name as an adapter knows it, else "Profile {id}".
    pub(super) fn profile_text(&self, adapter: &str, id: u32) -> String {
        let known = self
            .state_of(adapter)
            .and_then(|st| st.profiles.get(&id))
            .or_else(|| self.held.get(adapter).and_then(|h| h.profiles.get(&id)));
        match known {
            Some(p) => {
                let name = words::clean(&p.name);
                if name.is_empty() {
                    format!("Profile {id}")
                } else {
                    name
                }
            }
            None => format!("Profile {id}"),
        }
    }

    /// The roles of a profile as an adapter knows it.
    pub(super) fn profile_roles(&self, adapter: &str, id: u32) -> Vec<p::Role> {
        self.state_of(adapter)
            .and_then(|st| st.profiles.get(&id))
            .map(profiles::roles)
            .unwrap_or_default()
    }

    /// A text information entry an adapter reports, cleaned.
    pub(super) fn info_text(status: &p::Status, key: &str) -> Option<String> {
        model::info_text(&status.info, key).map(words::clean)
    }

    pub(super) fn board(status: &p::Status) -> Option<String> {
        Self::info_text(status, keys::BOARD_NAME)
    }
}

/// Something that needs the user's attention while the TUI is open.
#[derive(Clone, Debug)]
pub struct Alert {
    /// The device's or adapter's name.
    pub name: String,
    /// The alert as a sentence.
    pub title: String,
    /// A few words for the Overview's row.
    pub detail: String,
    pub page: Page,
}

impl<F: Fleet> Model<F> {
    /// What needs attention now: low batteries, adapters almost out of profile memory or
    /// needing attention, and connected devices whose profiles aren't loaded.
    pub(super) fn alerts(&self) -> Vec<Alert> {
        let mut out = Vec::new();
        for d in &self.devices {
            let Some(b) = &d.battery else { continue };
            let (Some(level), Some(p)) = (b.level(), b.percent) else {
                continue;
            };
            let title = match level {
                words::Level::Critical => format!("{}'s battery is critically low.", d.name),
                words::Level::Low => format!("{} has a low battery.", d.name),
                words::Level::Ok => continue,
            };
            out.push(Alert {
                name: d.name.clone(),
                title,
                detail: format!("Battery {p}%"),
                page: d.page(),
            });
        }
        for a in self.adapters.iter().filter(|a| a.connected()) {
            let page = Page::Adapter(a.id.clone());
            if let Some(status) = &a.status
                && words::memory_alert(status)
            {
                let percent = words::memory_percent(status).unwrap_or(0);
                out.push(Alert {
                    name: a.name.clone(),
                    title: format!("{} is almost out of profile memory.", a.name),
                    detail: format!("Profile Memory {percent}%"),
                    page: page.clone(),
                });
            }
            for text in &a.attention {
                let detail = if text == words::STORAGE_FULL_ATTENTION {
                    words::STORAGE_FULL.to_owned()
                } else {
                    text.clone()
                };
                out.push(Alert {
                    name: a.name.clone(),
                    title: text.clone(),
                    detail,
                    page: page.clone(),
                });
            }
        }
        for d in self.devices.iter().filter(|d| d.connected()) {
            if let Some(code) = model::profile_error(&d.d) {
                out.push(Alert {
                    name: d.name.clone(),
                    title: format!("{} connected without its profiles.", d.name),
                    detail: words::profile_error_text(code),
                    page: d.page(),
                });
            }
        }
        out
    }
}

/// Whether the device's settings tab has anything to show.
pub fn has_settings(st: Option<&State>, d: &p::Device) -> bool {
    let listed =
        st.is_some_and(|st| !crate::ui::catalog::presented(st.settings_of(d.id)).is_empty());
    listed || !super::settings::readings(d).is_empty() || !words::wheel_figures(&d.info).is_empty()
}
