//! Which notifications to show for each new view of the adapters and devices.
//!
//! A device is notified once on entering low battery and once on entering critical; the alert
//! resets after a reading above the threshold or while charging. Profile memory and unloaded
//! profiles are notified once until the condition clears. Adapters and devices that go away, as
//! while an adapter reconnects, keep their alerts.
use super::prefs::Preferences;
use crate::model;
use crate::ui::app::{
    AdapterView, DeviceView, Page,
    words::{self, Battery, CRITICAL_PERCENT, Level},
};
use cordial_protocol::DeviceState;
use std::collections::{HashMap, HashSet};

/// A notification to show. A newer one with the same ID replaces it.
#[derive(Debug, PartialEq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: Option<String>,
    /// What activating it opens; None opens the window as it was.
    pub page: Option<Page>,
}

/// The battery's level against `threshold`; None when the readings don't decide it.
pub fn level(b: &Battery, threshold: i64) -> Option<Level> {
    if !b.fresh {
        return None;
    }
    if b.charging == Some(true) {
        return Some(Level::Ok);
    }
    Some(match b.percent? {
        p if p <= CRITICAL_PERCENT.min(threshold) => Level::Critical,
        p if p <= threshold => Level::Low,
        _ => Level::Ok,
    })
}

type Key = (String, u32);

fn key(d: &DeviceView) -> Key {
    (d.adapter.clone(), d.d.id)
}

#[derive(Default)]
pub struct Alerts {
    levels: HashMap<Key, Level>,
    states: HashMap<Key, DeviceState>,
    memory: HashSet<String>,
    profiles: HashSet<Key>,
}

impl Alerts {
    pub fn update(
        &mut self,
        adapters: &[AdapterView],
        devices: &[DeviceView],
        p: &Preferences,
    ) -> Vec<Note> {
        let mut notes = Vec::new();
        if p.notify_low_battery {
            for d in devices {
                let Some(b) = &d.battery else { continue };
                let Some(level) = level(b, p.low_battery_percent) else {
                    continue;
                };
                let old = self.levels.get(&key(d)).copied().unwrap_or(Level::Ok);
                if level > old {
                    let title = if level == Level::Critical {
                        format!("{}'s battery is critically low.", d.name)
                    } else {
                        format!("{} has a low battery.", d.name)
                    };
                    let percent = b.percent.expect("a low level has a percent");
                    notes.push(Note {
                        id: format!("battery:{}:{}", d.adapter, d.d.id),
                        title,
                        body: Some(format!("The battery is at {percent}%. Charge it soon.")),
                        page: Some(d.page()),
                    });
                }
                // Recovering from critical to low keeps the low alert without a repeat.
                if level == Level::Ok || level > old {
                    self.levels.insert(key(d), level);
                }
            }
        }

        let states: HashMap<Key, DeviceState> =
            devices.iter().map(|d| (key(d), d.d.state())).collect();
        if p.notify_connections {
            for d in devices {
                let Some(old) = self.states.get(&key(d)) else {
                    continue;
                };
                let (new, connected) = (d.d.state(), DeviceState::Connected);
                if *old != new && (new == connected || *old == connected) {
                    notes.push(Note {
                        id: format!("connection:{}:{}", d.adapter, d.d.id),
                        title: d.name.clone(),
                        body: Some(
                            if new == connected {
                                "Connected"
                            } else {
                                "Disconnected"
                            }
                            .into(),
                        ),
                        page: None,
                    });
                }
            }
        }
        self.states = states;

        for a in adapters.iter().filter(|a| a.connected()) {
            let Some(status) = &a.status else { continue };
            if !words::memory_alert(status) {
                self.memory.remove(&a.id);
            } else if self.memory.insert(a.id.clone()) {
                notes.push(Note {
                    id: format!("memory:{}", a.id),
                    title: format!("{} is almost out of profile memory.", a.name),
                    body: None,
                    page: Some(Page::Adapter(a.id.clone())),
                });
            }
        }
        // A record the adapter couldn't read says nothing current of the device's profiles.
        for d in devices.iter().filter(|d| d.read_error.is_none()) {
            match model::profile_error(&d.d) {
                None => {
                    self.profiles.remove(&key(d));
                }
                Some(e) if self.profiles.insert(key(d)) => notes.push(Note {
                    id: format!("profiles:{}:{}", d.adapter, d.d.id),
                    title: format!("{} connected without its profiles.", d.name),
                    body: Some(words::profile_error_text(e)),
                    page: Some(d.page()),
                }),
                Some(_) => {}
            }
        }
        notes
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::ui::app::{Storage, world::Conn};
    use cordial_protocol::{self as p, ErrorCode};

    pub fn adapter(id: &str) -> AdapterView {
        AdapterView {
            id: id.into(),
            name: format!("Adapter {id}"),
            conn: Conn::Connected,
            ready: true,
            status: Some(p::Status {
                ready: true,
                ..Default::default()
            }),
            attention: Vec::new(),
            failed: false,
            connect_error: None,
            unreadable: Vec::new(),
            storage: Storage::Normal,
            hello: None,
        }
    }

    pub fn device(adapter: &str, id: u32, state: DeviceState) -> DeviceView {
        DeviceView {
            adapter: adapter.into(),
            available: true,
            d: p::Device {
                id,
                state: state as i32,
                ..Default::default()
            },
            name: id.to_string(),
            kind: words::DisplayKind::Keyboard,
            battery: None,
            read_error: None,
        }
    }

    pub fn battery(percent: Option<i64>, charging: Option<bool>) -> Option<Battery> {
        Some(Battery {
            percent,
            charging,
            fresh: true,
        })
    }

    fn error(code: ErrorCode) -> p::Error {
        p::Error {
            code: code as i32,
            ..Default::default()
        }
    }

    #[test]
    fn levels_treat_unknown_as_undecided_and_charging_as_not_low() {
        let b = |percent, charging| battery(percent, charging).unwrap();
        assert_eq!(level(&b(None, Some(false)), 20), None);
        assert_eq!(level(&b(Some(3), Some(true)), 20), Some(Level::Ok));
        assert_eq!(level(&b(Some(20), None), 20), Some(Level::Low));
        assert_eq!(level(&b(Some(25), None), 30), Some(Level::Low));
        assert_eq!(level(&b(Some(5), Some(false)), 20), Some(Level::Critical));
        assert_eq!(level(&b(Some(5), Some(false)), 3), Some(Level::Ok));
        assert_eq!(level(&b(Some(4), Some(false)), 3), Some(Level::Ok));
        assert_eq!(level(&b(Some(3), Some(false)), 3), Some(Level::Critical));
        assert_eq!(level(&b(Some(100), Some(false)), 100), Some(Level::Low));
        let stale = Battery {
            fresh: false,
            ..b(Some(3), None)
        };
        assert_eq!(level(&stale, 20), None);
    }

    #[test]
    fn alerts_on_entering_low_and_critical_once() {
        let mut alerts = Alerts::default();
        let p = Preferences::default();
        let mut levels = |percent: Option<i64>, charging: Option<bool>| {
            let mut d = device("A", 1, DeviceState::Connected);
            d.battery = if percent.is_none() && charging.is_none() {
                None
            } else {
                battery(percent, charging)
            };
            alerts
                .update(&[], &[d], &p)
                .into_iter()
                .map(|n| n.title)
                .collect::<Vec<_>>()
        };
        assert!(levels(Some(50), Some(false)).is_empty());
        assert_eq!(levels(Some(20), Some(false)), ["1 has a low battery."]);
        assert!(levels(Some(19), Some(false)).is_empty());
        assert!(levels(None, None).is_empty());
        assert!(levels(Some(18), Some(false)).is_empty());
        assert_eq!(
            levels(Some(5), Some(false)),
            ["1's battery is critically low."]
        );
        assert!(levels(Some(10), Some(false)).is_empty());
        assert!(levels(Some(4), Some(false)).is_empty());
        assert!(levels(Some(21), Some(false)).is_empty());
        assert_eq!(levels(Some(15), Some(false)), ["1 has a low battery."]);
        assert!(levels(Some(15), Some(true)).is_empty());
        assert_eq!(levels(Some(15), Some(false)), ["1 has a low battery."]);
    }

    #[test]
    fn a_battery_alert_opens_the_device_and_survives_absence() {
        let mut alerts = Alerts::default();
        let p = Preferences::default();
        let mut d = device("A", 7, DeviceState::Connected);
        d.battery = battery(Some(12), Some(false));
        let notes = alerts.update(&[adapter("A")], std::slice::from_ref(&d), &p);
        assert_eq!(
            notes,
            [Note {
                id: "battery:A:7".into(),
                title: "7 has a low battery.".into(),
                body: Some("The battery is at 12%. Charge it soon.".into()),
                page: Some(Page::Device("A".into(), 7)),
            }]
        );
        assert!(alerts.update(&[], &[], &p).is_empty());
        assert!(alerts.update(&[adapter("A")], &[d], &p).is_empty());
    }

    #[test]
    fn battery_alerts_follow_the_preferences() {
        let mut alerts = Alerts::default();
        let mut d = device("A", 1, DeviceState::Connected);
        d.battery = battery(Some(25), Some(false));
        let off = Preferences {
            notify_low_battery: false,
            low_battery_percent: 30,
            ..Preferences::default()
        };
        assert!(
            alerts
                .update(&[], std::slice::from_ref(&d), &off)
                .is_empty()
        );
        let on = Preferences {
            notify_low_battery: true,
            ..off
        };
        assert_eq!(alerts.update(&[], &[d], &on).len(), 1);
    }

    #[test]
    fn connection_changes_notify_only_when_enabled() {
        let p = Preferences {
            notify_connections: true,
            ..Preferences::default()
        };
        let mut alerts = Alerts::default();
        let at = |s| [device("A", 1, s)];
        assert!(
            alerts
                .update(&[], &at(DeviceState::Disconnected), &p)
                .is_empty()
        );
        let notes = alerts.update(&[], &at(DeviceState::Connecting), &p);
        assert!(notes.is_empty());
        let notes = alerts.update(&[], &at(DeviceState::Connected), &p);
        assert_eq!(notes[0].title, "1");
        assert_eq!(notes[0].body.as_deref(), Some("Connected"));
        assert_eq!(notes[0].page, None);
        let notes = alerts.update(&[], &at(DeviceState::Disconnected), &p);
        assert_eq!(notes[0].body.as_deref(), Some("Disconnected"));
        // A device that returns after going away has no earlier state to compare.
        assert!(alerts.update(&[], &[], &p).is_empty());
        assert!(
            alerts
                .update(&[], &at(DeviceState::Connected), &p)
                .is_empty()
        );

        let mut quiet = Alerts::default();
        let off = Preferences::default();
        quiet.update(&[], &at(DeviceState::Disconnected), &off);
        assert!(
            quiet
                .update(&[], &at(DeviceState::Connected), &off)
                .is_empty()
        );
    }

    #[test]
    fn profile_memory_alerts_once_until_it_clears() {
        let p = Preferences::default();
        let mut alerts = Alerts::default();
        let mut full = adapter("A");
        full.status.as_mut().unwrap().errors = vec![error(ErrorCode::ProfileMemoryLow)];
        let notes = alerts.update(std::slice::from_ref(&full), &[], &p);
        assert_eq!(notes[0].title, "Adapter A is almost out of profile memory.");
        assert_eq!(notes[0].body, None);
        assert_eq!(notes[0].page, Some(Page::Adapter("A".into())));
        assert!(
            alerts
                .update(std::slice::from_ref(&full), &[], &p)
                .is_empty()
        );
        let mut away = full.clone();
        away.conn = Conn::Connecting;
        assert!(alerts.update(&[away], &[], &p).is_empty());
        assert!(
            alerts
                .update(std::slice::from_ref(&full), &[], &p)
                .is_empty()
        );
        alerts.update(&[adapter("A")], &[], &p);
        assert_eq!(alerts.update(&[full], &[], &p).len(), 1);
    }

    #[test]
    fn unloaded_profiles_alert_once_until_they_load() {
        let p = Preferences::default();
        let mut alerts = Alerts::default();
        let mut d = device("A", 2, DeviceState::Connected);
        d.d.errors = vec![error(ErrorCode::ProfileMemoryExceeded)];
        let notes = alerts.update(&[], std::slice::from_ref(&d), &p);
        assert_eq!(notes[0].title, "2 connected without its profiles.");
        assert_eq!(notes[0].page, Some(Page::Device("A".into(), 2)));
        assert!(notes[0].body.is_some());
        assert!(alerts.update(&[], std::slice::from_ref(&d), &p).is_empty());
        let mut unread = d.clone();
        unread.d.errors.clear();
        unread.read_error = Some(error(ErrorCode::NotReady));
        assert!(alerts.update(&[], &[unread], &p).is_empty());
        assert!(alerts.update(&[], std::slice::from_ref(&d), &p).is_empty());
        alerts.update(&[], &[device("A", 2, DeviceState::Connected)], &p);
        assert_eq!(alerts.update(&[], &[d], &p).len(), 1);
    }
}
