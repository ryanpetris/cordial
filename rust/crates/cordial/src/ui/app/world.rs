//! The adapters and devices built from every session's view: connected adapters, adapters held while they may
//! return, adapters the user disconnected while they stay plugged in, and ports whose adapter
//! refused the connection; and every saved device of connected and held adapters in one list.
use super::{
    App, Page, Tab,
    fleet::Fleet,
    words::{self, Battery, DisplayKind},
};
use crate::{model, profiles, view::State};
use cordial_client::compatibility::NativeHello;
use cordial_protocol::{self as p, ErrorCode};
use std::{sync::Arc, time::Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Conn {
    Connected,
    Connecting,
    Disconnected,
}

/// Whether the application may use the adapter's saved data, and its migration.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Storage {
    #[default]
    Normal,
    /// The saved data must be migrated; Migrate starts it.
    Required,
    Migrating {
        waiting: bool,
        cancelling: bool,
        /// Bytes done and in all, of the current step.
        progress: Option<(u64, u64)>,
    },
    /// The migration failed: what to tell the user, and the worker's reason. Nothing opens
    /// until Migrate tries again or Cancel ends it.
    Failed { text: String, reason: String },
    /// The adapter waits for a migration, but another error, shown here, stops Cordial from
    /// running one. Restart still works.
    Blocked(String),
    /// This version of Cordial can't use the adapter, and why.
    Unsupported(String),
}

#[derive(Clone, Debug)]
pub struct AdapterView {
    /// The adapter's ID, or the port of a refused adapter, which has no confirmed ID.
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
    /// Saved devices the adapter's last listing couldn't read and that this session has no record
    /// of, with why, in ID order.
    pub unreadable: Vec<(u32, p::Error)>,
    pub storage: Storage,
    /// What a refused adapter reported about itself.
    pub hello: Option<Arc<NativeHello>>,
}

impl AdapterView {
    pub fn connected(&self) -> bool {
        self.conn == Conn::Connected
    }

    /// The adapter's status when it is ready and usable: shown, and locked unless connected.
    pub fn view(&self) -> Option<&p::Status> {
        self.status
            .as_ref()
            .filter(|s| s.ready && self.storage == Storage::Normal)
    }

    /// The status of a connected adapter that is ready.
    pub fn live(&self) -> Option<&p::Status> {
        self.view().filter(|_| self.connected())
    }

    /// The adapter's status in a few words, and whether it needs attention.
    pub fn status_text(&self) -> (&'static str, bool) {
        match &self.storage {
            Storage::Normal => {}
            Storage::Migrating { waiting: true, .. } => return ("Waiting for Adapter", false),
            Storage::Migrating { .. } => return ("Migrating…", false),
            Storage::Unsupported(_) => return ("Unsupported", true),
            Storage::Required | Storage::Blocked(_) | Storage::Failed { .. } => {
                return ("Needs Attention", true);
            }
        }
        match self.conn {
            Conn::Disconnected if self.hello.is_some() => ("Needs Attention", true),
            Conn::Disconnected => ("Disconnected", false),
            Conn::Connecting => ("Connecting…", false),
            Conn::Connected if self.failed => ("Needs Attention", true),
            Conn::Connected if !self.ready && !self.faults().is_empty() => {
                ("Needs Attention", true)
            }
            Conn::Connected if !self.ready => ("Starting…", false),
            Conn::Connected if !self.attention.is_empty() => ("Needs Attention", true),
            Conn::Connected => ("Ready", false),
        }
    }

    pub fn profile_support(&self) -> bool {
        self.status.as_ref().is_some_and(profiles::available)
    }

    /// The adapter's radio, storage and other faults, which a starting adapter doesn't report:
    /// every error other than those shown in their own places.
    pub fn faults(&self) -> Vec<&p::Error> {
        use ErrorCode::*;
        let shown = [
            StorageFull,
            ProfileMemoryLow,
            UsbRequiredCapacityExceeded,
            UsbReservedCapacityExceeded,
        ];
        self.status
            .iter()
            .flat_map(|s| &s.errors)
            .filter(|e| !shown.iter().any(|c| *c as i32 == e.code))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct DeviceView {
    pub adapter: String,
    /// The current adapter connection has listed this device and remains open.
    pub available: bool,
    pub d: p::Device,
    pub name: String,
    pub kind: DisplayKind,
    pub battery: Option<Battery>,
    /// Why the adapter's last listing couldn't read the device's record; `d` is then the record
    /// read before.
    pub read_error: Option<p::Error>,
}

impl DeviceView {
    pub fn page(&self) -> Page {
        Page::Device(self.adapter.clone(), self.d.id)
    }

    /// Whether the device is connected as far as the adapter last confirmed; a record the latest
    /// listing couldn't read confirms nothing.
    pub fn connected(&self) -> bool {
        self.available && self.read_error.is_none() && model::connected(&self.d)
    }

    /// The device's state in a few words, as lists show it.
    pub fn status(&self) -> &'static str {
        if !self.available {
            "Disconnected"
        } else if self.read_error.is_some() {
            "Couldn't Read"
        } else {
            words::device_status(&self.d)
        }
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

impl<F: Fleet> App<F> {
    /// Takes every session's current view and rebuilds the adapters and devices shown. The page
    /// returns to the Overview once its target, after having been shown, goes away.
    pub(crate) fn refresh(&mut self) {
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
            // Missing required USB interfaces need attention; missing reserved ones don't.
            if model::has_error(&st.status, ErrorCode::UsbRequiredCapacityExceeded) {
                attention.push(words::code_text(ErrorCode::UsbRequiredCapacityExceeded));
            }
            if let Some(why) = &s.unready {
                attention.push(why.clone());
            }
            adapters.push(AdapterView {
                id: s.id.clone(),
                name: adapter_name(&st.status, &s.id),
                conn: if st.session_ending.is_some() {
                    Conn::Connecting
                } else {
                    Conn::Connected
                },
                ready: st.ready(),
                status: Some(st.status.clone()),
                attention,
                failed: s.unready.is_some(),
                connect_error: None,
                unreadable: if s.listed {
                    st.unreadable
                        .iter()
                        .filter(|(id, _)| !st.devices.iter().any(|d| d.id == **id))
                        .map(|(id, e)| (*id, e.clone()))
                        .collect()
                } else {
                    Vec::new()
                },
                storage: s.storage.clone(),
                hello: None,
            });
        }
        if let Some(m) = &self.migration {
            adapters.push(AdapterView {
                id: m.id.clone(),
                name: adapter_name(&m.status, &m.id),
                conn: Conn::Connecting,
                ready: false,
                status: Some(m.status.clone()),
                attention: Vec::new(),
                failed: false,
                connect_error: None,
                unreadable: Vec::new(),
                storage: m.storage(),
                hello: None,
            });
        }
        let migrating = self.migration.as_ref().map(|m| m.id.as_str());
        for (id, h) in &self.held {
            if self.sessions.iter().any(|s| &s.id == id) || migrating == Some(id) {
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
                unreadable: Vec::new(),
                storage: Storage::Normal,
                hello: None,
            });
        }
        for (id, o) in &self.off {
            if o.port.is_none() || migrating == Some(id) {
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
                unreadable: Vec::new(),
                storage: Storage::Normal,
                hello: None,
            });
        }
        for (port, r) in &self.refused {
            let connecting = r.retry || self.probes.values().any(|p| p == port);
            adapters.push(AdapterView {
                id: port.clone(),
                name: words::reported(r.hello.hello.name.as_deref())
                    .unwrap_or_else(|| "Adapter".into()),
                conn: if connecting {
                    Conn::Connecting
                } else {
                    Conn::Disconnected
                },
                ready: false,
                status: None,
                attention: Vec::new(),
                failed: false,
                connect_error: (!connecting).then(|| r.why.clone()),
                unreadable: Vec::new(),
                storage: Storage::Normal,
                hello: Some(r.hello.clone()),
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
                let read_error = st.unreadable.get(&d.id).cloned();
                devices.push(DeviceView {
                    adapter: s.id.clone(),
                    available: st.available && st.session_ending.is_none(),
                    name: words::device_name(d),
                    kind: words::display_kind(&d.kinds, &d.roles),
                    battery: Battery::of(d, st.ready() && read_error.is_none()),
                    read_error,
                    d: d.clone(),
                });
            }
        }
        // Keep the last listing until recovery ends or the replacement session lists devices.
        for previous in &self.devices {
            let relisting = match self.session_of(&previous.adapter) {
                Some(s) => !s.listed && s.retained_until.is_some_and(|until| until > now),
                None => self.held.contains_key(&previous.adapter),
            };
            if relisting {
                let mut d = previous.clone();
                d.available = false;
                if let Some(battery) = &mut d.battery {
                    battery.fresh = false;
                }
                devices.push(d);
            }
        }
        devices.sort_by(|a, b| {
            order(&a.name, &b.name)
                .then_with(|| a.adapter.cmp(&b.adapter))
                .then(a.d.id.cmp(&b.d.id))
        });
        for d in &devices {
            let key = (d.adapter.clone(), d.d.id);
            // A device's connection errors belong to its state at the time.
            if self.device_states.insert(key.clone(), d.d.state) != Some(d.d.state) {
                self.notes
                    .remove(&super::Spot::DeviceBar(key.0.clone(), key.1));
            }
            // A Gamepad Output change whose outcome was unknown no longer is once the device
            // reports it saved.
            if self.immediate.is_none()
                && d.available
                && let Some(draft) = self.device_drafts.get_mut(&key)
                && draft.gamepad.is_some()
                && draft.gamepad == model::gamepad_mode(&d.d)
            {
                draft.gamepad = None;
                if draft.is_empty() {
                    self.device_drafts.remove(&key);
                }
                self.notes.remove(&super::Spot::SettingsNote(key.0, key.1));
            }
        }
        self.devices = devices;

        if self.exists(&self.page) {
            self.page_shown = true;
        } else if self.page_shown {
            self.page = Page::Overview;
            self.tab = Tab::Details;
            self.notes.clear();
            self.editing = None;
        }
    }

    pub(crate) fn exists(&self, page: &Page) -> bool {
        match page {
            Page::Overview => true,
            Page::Device(a, id) => self.device(a, *id).is_some(),
            Page::Adapter(a) => self.adapter(a).is_some(),
        }
    }

    /// The page shown: the selection, or the Overview while its target isn't there.
    pub fn shown(&self) -> Page {
        if self.exists(&self.page) {
            self.page.clone()
        } else {
            Page::Overview
        }
    }

    pub fn adapter(&self, id: &str) -> Option<&AdapterView> {
        self.adapters.iter().find(|a| a.id == id)
    }

    pub fn device(&self, adapter: &str, id: u32) -> Option<&DeviceView> {
        self.devices
            .iter()
            .find(|d| d.adapter == adapter && d.d.id == id)
    }

    /// Adapters that can add a device: connected and ready.
    pub fn ready_adapters(&self) -> Vec<&AdapterView> {
        self.adapters
            .iter()
            .filter(|a| a.connected() && a.ready)
            .collect()
    }

    /// A profile's name as an adapter knows it, else "Profile {id}".
    pub fn profile_text(&self, adapter: &str, id: u32) -> String {
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
    pub fn profile_roles(&self, adapter: &str, id: u32) -> Vec<p::Role> {
        self.state_of(adapter)
            .and_then(|st| st.profiles.get(&id))
            .or_else(|| self.held.get(adapter).and_then(|h| h.profiles.get(&id)))
            .map(profiles::roles)
            .unwrap_or_default()
    }

    /// The adapter's firmware version, cleaned.
    pub fn firmware(status: &p::Status) -> Option<String> {
        let info = status.information.as_ref()?;
        info.firmware_version.as_deref().map(words::clean)
    }

    pub fn board(status: &p::Status) -> Option<String> {
        let info = status.information.as_ref()?;
        info.board_name.as_deref().map(words::clean)
    }
}

/// Something that needs the user's attention in the overview.
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

impl<F: Fleet> App<F> {
    /// What needs attention now: low batteries, adapters almost out of profile memory or
    /// needing attention, and connected devices whose profiles aren't loaded.
    pub fn alerts(&self) -> Vec<Alert> {
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
                let detail = match words::memory_percent(status) {
                    Some(percent) => format!("Profile Memory {percent}%"),
                    None => "Profile Memory".into(),
                };
                out.push(Alert {
                    name: a.name.clone(),
                    title: format!("{} is almost out of profile memory.", a.name),
                    detail,
                    page: page.clone(),
                });
            }
            for text in &a.attention {
                let detail = if text == words::STORAGE_FULL_ATTENTION {
                    words::STORAGE_FULL.to_owned()
                } else if *text == words::code_text(ErrorCode::UsbRequiredCapacityExceeded) {
                    words::REQUIRED_CAPACITY.to_owned()
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
        st.is_some_and(|st| !crate::ui::catalog::presented(&st.settings_of(d.id)).is_empty());
    listed
        || !super::settings::readings(d).is_empty()
        || !words::wheel_figures(d.information.as_ref()).is_empty()
}
