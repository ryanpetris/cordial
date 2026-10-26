//! Immediate editing uses real framed simulator connections without starting GTK.
use cordial::{
    settings::{Field, Leaf, Value},
    sim::Desktop,
    ui::app::{Action, App, Live, Msg, Page, Spot, Tab, Toggle},
};
use cordial_protocol::{self as p, request::Command};
use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

struct Harness {
    app: App<Live>,
    world: Desktop,
    rx: Receiver<Msg>,
    adapter: String,
}

impl Harness {
    fn new() -> Self {
        let world = Desktop::new(1);
        let adapter = world.ports()[0].serial.clone();
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(world.live(Arc::new(move |msg| {
            let _ = tx.send(msg);
        })));
        app.enable_immediate();
        let mut h = Self {
            app,
            world,
            rx,
            adapter,
        };
        h.until(|a| {
            a.devices().len() == 4
                && a.devices()
                    .iter()
                    .filter(|d| d.d.id <= 2)
                    .all(|d| a.settings_of(d).len() > 2)
        });
        h.app.act(Action::Show(Page::Device(h.adapter.clone(), 1)));
        h.app.act(Action::Tab(Tab::Settings));
        h
    }

    fn until(&mut self, done: impl Fn(&App<Live>) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(&self.app) {
            assert!(
                Instant::now() < deadline,
                "immediate application did not settle"
            );
            self.app.apply_pending();
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(msg) => self.app.update(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => self.app.tick(),
                Err(error) => panic!("simulator stopped: {error}"),
            }
        }
    }

    fn delay(&self, command: &'static str) {
        self.world.adapters()[0]
            .0
            .lock()
            .unwrap()
            .slow
            .insert(command, Duration::from_millis(100));
    }

    fn dpi(&mut self, value: i64) {
        self.app.act(Action::SettingChoice(
            Field::hidpp(Leaf::Dpi, Some(0)),
            Value::Integer(value),
        ));
    }

    fn refuse(&self, command: &'static str, code: p::ErrorCode, outcome: p::ErrorOutcome) {
        self.world.adapters()[0].0.lock().unwrap().refuse.insert(
            command,
            p::Error {
                code: code as i32,
                outcome: outcome as i32,
                ..Default::default()
            },
        );
    }

    fn shown_dpi(&self) -> Option<Value> {
        let d = self.app.device(&self.adapter, 1).unwrap();
        let s = self
            .app
            .setting(d, &Field::hidpp(Leaf::Dpi, Some(0)))
            .unwrap();
        self.app.shown_value(d, &s)
    }

    fn settings_writes(&self) -> Vec<p::SetSettings> {
        self.world.adapters()[0]
            .0
            .lock()
            .unwrap()
            .log
            .iter()
            .filter_map(|c| match c {
                Command::SetSettings(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    fn close(mut self) {
        self.app.quit();
        self.until(App::done);
        assert_eq!(self.world.adapters()[0].handles(), 0);
    }
}

#[test]
fn newer_values_stay_visible_and_coalesce_while_the_first_write_runs() {
    let mut h = Harness::new();
    h.delay("set_settings");
    h.dpi(1200);
    h.app.apply_pending();
    let d = h.app.device(&h.adapter, 1).unwrap();
    assert!(h.app.submitting(d));
    assert!(h.app.settings_editable(d));
    for value in [1400, 1800, 1000] {
        h.dpi(value);
        assert_eq!(h.shown_dpi(), Some(Value::Integer(value)));
        h.app.apply_pending();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.app.writes_pending() {
        assert!(Instant::now() < deadline);
        if let Ok(msg) = h.rx.recv_timeout(Duration::from_millis(10)) {
            h.app.update(msg);
        }
        assert_eq!(
            h.shown_dpi(),
            Some(Value::Integer(1000)),
            "older confirmation replaced a newer edit"
        );
        h.app.apply_pending();
    }
    assert_eq!(
        h.settings_writes().len(),
        2,
        "intermediate edits must coalesce"
    );
    assert!(h.app.write_error().is_none());
    h.close();
}

#[test]
fn settings_failure_drops_sent_values_without_retrying_them_on_an_unrelated_edit() {
    let mut h = Harness::new();
    h.world.adapters()[0].0.lock().unwrap().refuse.insert(
        "set_settings",
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        },
    );
    h.dpi(1200);
    h.app.apply_pending();
    h.until(|a| a.write_error().is_some());
    assert!(!h.app.writes_pending());
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1000)));
    let failure = h.app.write_error().unwrap().to_owned();
    h.world.adapters()[0]
        .0
        .lock()
        .unwrap()
        .refuse
        .remove("set_settings");
    h.app.act(Action::SettingBool(
        Field::hidpp(Leaf::WheelInvert, None),
        true,
    ));
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1000)));
    let writes = h.settings_writes();
    assert_eq!(writes.len(), 2);
    assert_eq!(
        writes[1].changes.len(),
        1,
        "failed DPI must not join a later save"
    );
    assert_eq!(
        h.app.write_error(),
        Some(failure.as_str()),
        "an unrelated success must not resolve the failed DPI"
    );
    assert_eq!(
        h.app.note(&Spot::SettingsNote(h.adapter.clone(), 1)),
        Some(failure.as_str())
    );
    h.close();
}

#[test]
fn a_failed_request_preserves_the_newer_desired_value_for_one_followup() {
    let mut h = Harness::new();
    h.delay("set_settings");
    h.world.adapters()[0].0.lock().unwrap().refuse.insert(
        "set_settings",
        p::Error {
            code: p::ErrorCode::Busy as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        },
    );
    h.dpi(1200);
    h.app.apply_pending();
    h.dpi(1800);
    h.until(|a| a.write_error().is_some());
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1800)));
    assert!(h.app.writes_pending());
    h.world.adapters()[0]
        .0
        .lock()
        .unwrap()
        .refuse
        .remove("set_settings");
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1800)));
    assert_eq!(h.settings_writes().len(), 2);
    assert!(h.app.write_error().is_none());
    h.close();
}

#[test]
fn unknown_outcome_keeps_the_choice_but_does_not_join_an_unrelated_save() {
    let mut h = Harness::new();
    h.world.adapters()[0].0.lock().unwrap().refuse.insert(
        "set_settings",
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            outcome: p::ErrorOutcome::Unknown as i32,
            ..Default::default()
        },
    );
    h.dpi(1200);
    h.app.apply_pending();
    h.until(|a| a.write_error().is_some());
    assert!(!h.app.writes_pending());
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1200)));
    assert!(h.app.submission(&h.adapter, 1).unwrap().unknown);
    h.world.adapters()[0]
        .0
        .lock()
        .unwrap()
        .refuse
        .remove("set_settings");
    h.app.act(Action::SettingBool(
        Field::hidpp(Leaf::WheelInvert, None),
        true,
    ));
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    assert_eq!(h.settings_writes()[1].changes.len(), 1);
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1200)));
    assert!(
        h.app
            .note(&Spot::SettingsNote(h.adapter.clone(), 1))
            .unwrap()
            .contains("couldn't confirm")
    );
    h.app.act(Action::SettingsRefresh);
    h.until(|a| !a.refreshing(a.devices().iter().find(|d| d.d.id == 1).unwrap()));
    assert_eq!(
        h.shown_dpi(),
        Some(Value::Integer(1000)),
        "fresh readback settles the uncertain choice"
    );
    h.close();
}

#[test]
fn fresh_readback_clears_unknown_submission_without_another_write() {
    let mut h = Harness::new();
    h.world.adapters()[0].0.lock().unwrap().refuse.insert(
        "set_settings",
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            outcome: p::ErrorOutcome::Unknown as i32,
            ..Default::default()
        },
    );
    h.dpi(1200);
    h.app.apply_pending();
    h.until(|a| a.write_error().is_some());
    let failure = h.app.write_error().unwrap().to_owned();
    let submission = h.app.submission(&h.adapter, 1).unwrap();
    assert!(submission.unknown);
    assert_eq!(submission.error.as_deref(), Some(failure.as_str()));
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1200)));
    h.app.act(Action::SettingsRefresh);
    assert!(h.app.refreshing(h.app.device(&h.adapter, 1).unwrap()));
    h.until(|a| !a.refreshing(a.devices().iter().find(|d| d.d.id == 1).unwrap()));
    let submission = h.app.submission(&h.adapter, 1).unwrap();
    assert!(!submission.unknown);
    assert!(submission.error.is_none());
    assert!(
        h.app
            .note(&Spot::SettingsNote(h.adapter.clone(), 1))
            .is_none()
    );
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1000)));
    assert!(!h.app.writes_pending());
    assert_eq!(h.settings_writes().len(), 1);
    assert!(h.app.write_error().is_none());
    h.close();
}

#[test]
fn detach_preserves_unknown_error_and_reconnect_does_not_replay_accepted_edits() {
    let mut h = Harness::new();
    h.delay("set_settings");
    h.dpi(1200);
    h.app.apply_pending();
    h.dpi(1800);
    assert!(h.world.set_present(0, false));
    h.app.update(Msg::Ports(Ok(h.world.ports())));
    h.until(|a| a.adapters().iter().all(|a| !a.connected()));
    let error = h.app.write_error().unwrap().to_owned();
    assert!(error.contains("couldn't confirm whether the change was saved"));
    assert!(!h.app.writes_pending());
    let submission = h.app.submission(&h.adapter, 1).unwrap();
    assert!(submission.unknown);
    assert_eq!(submission.error.as_deref(), Some(error.as_str()));
    let adapter = h.world.adapters()[0].clone();
    h.until(|_| adapter.handles() == 0);
    let before = h.settings_writes().len();
    assert!(h.world.set_present(0, true));
    h.app.act(Action::RefreshAdapters);
    let id = h.adapter.clone();
    h.until(|a| {
        a.devices()
            .iter()
            .find(|d| d.d.id == 1)
            .is_some_and(|d| d.available && !a.settings_of(d).is_empty())
            && a.submission(&id, 1).is_some_and(|s| !s.unknown)
    });
    h.app.apply_pending();
    let submission = h.app.submission(&h.adapter, 1).unwrap();
    assert!(!submission.unknown);
    assert!(submission.error.is_none());
    assert!(
        h.app
            .note(&Spot::SettingsNote(h.adapter.clone(), 1))
            .is_none()
    );
    assert!(!h.app.writes_pending());
    assert_eq!(h.settings_writes().len(), before);
    assert!(h.app.write_error().is_none());
    h.close();
}

#[test]
fn adapter_and_device_edits_serialize_and_return_to_the_original_value() {
    let mut h = Harness::new();
    h.delay("set_adapter");
    h.delay("set_device");
    h.app.act(Action::Show(Page::Adapter(h.adapter.clone())));
    let original = h
        .app
        .adapter(&h.adapter)
        .unwrap()
        .status
        .as_ref()
        .unwrap()
        .platform();
    h.app.act(Action::Platform(p::Platform::Windows));
    h.app.apply_pending();
    assert!(h.app.adapter_editable(h.app.adapter(&h.adapter).unwrap()));
    h.app.act(Action::Platform(original));
    h.app.act(Action::Show(Page::Device(h.adapter.clone(), 1)));
    h.app.act(Action::DeviceToggle(Toggle::Trusted, false));
    h.app.apply_pending();
    assert!(h.app.device_editable(h.app.device(&h.adapter, 1).unwrap()));
    h.app.act(Action::DeviceToggle(Toggle::Trusted, true));
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    let sim = h.world.adapters()[0].0.lock().unwrap();
    assert_eq!(sim.status.platform(), original);
    assert!(sim.devices[0].trusted);
    drop(sim);
    h.close();
}

#[test]
fn a_failed_interface_switch_does_not_join_a_newer_profile_choice() {
    let mut h = Harness::new();
    h.delay("set_adapter");
    h.world.adapters()[0].0.lock().unwrap().refuse.insert(
        "set_adapter",
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        },
    );
    h.app.act(Action::Show(Page::Adapter(h.adapter.clone())));
    let interface = h
        .app
        .adapter(&h.adapter)
        .unwrap()
        .status
        .as_ref()
        .unwrap()
        .configuration_interfaces[0]
        .interface();
    assert!(
        !h.app
            .staged_interface(h.app.adapter(&h.adapter).unwrap(), interface)
            .unwrap()
            .enabled
    );
    h.app.act(Action::InterfaceEnabled(interface, true));
    h.app.apply_pending();
    let adapter = h.world.adapters()[0].clone();
    h.until(|_| {
        adapter
            .0
            .lock()
            .unwrap()
            .log
            .iter()
            .any(|command| matches!(command, Command::SetAdapter(_)))
    });
    assert!(h.app.adapter_saving(&h.adapter));
    h.app.act(Action::InterfacePick(interface));
    h.app.act(Action::Pick(2));
    h.app.act(Action::Submit);
    let changes = h.app.adapter_changes(h.app.adapter(&h.adapter).unwrap());
    assert_eq!(changes.interfaces.len(), 1);
    assert_eq!(changes.interfaces[0].enabled, Some(true));
    assert_eq!(changes.interfaces[0].profile, Some(2));
    h.until(|a| a.write_error().is_some());
    let changes = h.app.adapter_changes(h.app.adapter(&h.adapter).unwrap());
    assert_eq!(changes.interfaces.len(), 1);
    assert_eq!(changes.interfaces[0].enabled, None);
    assert_eq!(changes.interfaces[0].profile, Some(2));
    h.world.adapters()[0]
        .0
        .lock()
        .unwrap()
        .refuse
        .remove("set_adapter");
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    let sim = h.world.adapters()[0].0.lock().unwrap();
    assert!(!sim.status.configuration_interfaces[0].enabled);
    assert_eq!(sim.status.configuration_interfaces[0].profile, 2);
    let writes: Vec<_> = sim
        .log
        .iter()
        .filter_map(|command| match command {
            Command::SetAdapter(update) => Some(update),
            _ => None,
        })
        .collect();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].configuration_interfaces.len(), 1);
    assert_eq!(writes[0].configuration_interfaces[0].enabled, Some(true));
    assert_eq!(writes[0].configuration_interfaces[0].profile, None);
    assert_eq!(writes[1].configuration_interfaces.len(), 1);
    assert_eq!(writes[1].configuration_interfaces[0].enabled, None);
    assert_eq!(writes[1].configuration_interfaces[0].profile, Some(2));
    drop(sim);
    h.close();
}

#[test]
fn explicit_retry_success_resolves_only_its_failed_fields() {
    let mut h = Harness::new();
    h.refuse(
        "set_settings",
        p::ErrorCode::StorageFailed,
        p::ErrorOutcome::NoEffect,
    );
    h.dpi(1200);
    h.app.apply_pending();
    h.until(|a| a.write_error().is_some());
    let dpi_failure = h.app.write_error().unwrap().to_owned();

    h.refuse(
        "set_settings",
        p::ErrorCode::Busy,
        p::ErrorOutcome::NoEffect,
    );
    h.app.act(Action::SettingBool(
        Field::hidpp(Leaf::WheelInvert, None),
        true,
    ));
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    let wheel_failure = h.app.write_error().unwrap().to_owned();
    assert_ne!(dpi_failure, wheel_failure);
    h.world.adapters()[0]
        .0
        .lock()
        .unwrap()
        .refuse
        .remove("set_settings");

    h.app.act(Action::SettingBool(
        Field::hidpp(Leaf::WheelInvert, None),
        true,
    ));
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    assert_eq!(h.app.write_error(), Some(dpi_failure.as_str()));
    assert_eq!(
        h.app.note(&Spot::SettingsNote(h.adapter.clone(), 1)),
        Some(dpi_failure.as_str())
    );
    assert_eq!(h.settings_writes().len(), 3);
    assert_eq!(h.settings_writes()[2].changes.len(), 1);

    h.dpi(1200);
    h.app.apply_pending();
    assert_eq!(h.app.write_error(), Some(dpi_failure.as_str()));
    h.until(|a| !a.writes_pending());
    assert!(h.app.write_error().is_none());
    assert!(
        h.app
            .note(&Spot::SettingsNote(h.adapter.clone(), 1))
            .is_none()
    );
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1200)));
    assert_eq!(h.settings_writes().len(), 4);
    h.close();
}

#[test]
fn fresh_readback_resolves_uncertainty_without_clearing_another_spots_failure() {
    let mut h = Harness::new();
    h.app.act(Action::Tab(Tab::Details));
    h.refuse("set_device", p::ErrorCode::Busy, p::ErrorOutcome::NoEffect);
    h.app.act(Action::DeviceToggle(Toggle::Trusted, false));
    h.app.apply_pending();
    h.until(|a| a.write_error().is_some());
    let details_failure = h.app.write_error().unwrap().to_owned();

    h.app.act(Action::Tab(Tab::Settings));
    h.refuse(
        "set_settings",
        p::ErrorCode::StorageFailed,
        p::ErrorOutcome::Unknown,
    );
    h.dpi(1200);
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    assert_ne!(h.app.write_error(), Some(details_failure.as_str()));
    let unknown_failure = h.app.write_error().unwrap().to_owned();
    h.app.act(Action::Show(Page::Adapter(h.adapter.clone())));
    h.app.act(Action::Platform(p::Platform::Windows));
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    assert_eq!(h.app.write_error(), Some(unknown_failure.as_str()));
    assert_eq!(
        h.app.note(&Spot::DetailsSave(h.adapter.clone(), 1)),
        Some(details_failure.as_str())
    );
    h.app.act(Action::Show(Page::Device(h.adapter.clone(), 1)));
    h.app.act(Action::Tab(Tab::Settings));
    h.app.act(Action::SettingsRefresh);
    h.until(|a| !a.refreshing(a.devices().iter().find(|d| d.d.id == 1).unwrap()));
    assert_eq!(h.app.write_error(), Some(details_failure.as_str()));
    assert_eq!(
        h.app.note(&Spot::DetailsSave(h.adapter.clone(), 1)),
        Some(details_failure.as_str())
    );
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1000)));
    assert_eq!(h.settings_writes().len(), 1);

    h.world.adapters()[0]
        .0
        .lock()
        .unwrap()
        .refuse
        .remove("set_device");
    h.app.act(Action::Tab(Tab::Details));
    h.app.act(Action::DeviceToggle(Toggle::Trusted, false));
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    assert!(h.app.write_error().is_none());
    assert!(
        h.app
            .note(&Spot::DetailsSave(h.adapter.clone(), 1))
            .is_none()
    );
    h.close();
}

#[test]
fn a_stale_gamepad_record_cannot_discard_the_newer_return_to_the_saved_value() {
    let mut h = Harness::new();
    h.app.act(Action::Show(Page::Device(h.adapter.clone(), 4)));
    h.app.act(Action::Tab(Tab::Settings));
    let original = h
        .app
        .device_values(h.app.device(&h.adapter, 4).unwrap())
        .gamepad
        .unwrap();
    assert_ne!(original, p::OutputMode::Hid);

    let adapter = h.world.adapters()[0].clone();
    // Holding the simulator stops the first write from taking effect until the stale event
    // has reached the application.
    let mut sim = adapter.0.lock().unwrap();
    h.app.act(Action::GamepadOutput(p::OutputMode::Hid));
    h.app.apply_pending();
    h.app.act(Action::GamepadOutput(original));
    let mut stale = sim.devices.iter().find(|d| d.id == 4).unwrap().clone();
    stale.name = "Controller Update".into();
    sim.event(p::event::Kind::Device(stale));
    h.until(|a| {
        a.devices()
            .iter()
            .any(|d| d.d.id == 4 && d.name == "Controller Update")
    });
    let d = h.app.device(&h.adapter, 4).unwrap();
    assert!(h.app.device_saving(d, Tab::Settings));
    assert_eq!(h.app.device_changes(d).gamepad, Some(original));
    drop(sim);

    h.until(|a| !a.writes_pending());
    let sim = adapter.0.lock().unwrap();
    let modes: Vec<_> = sim
        .log
        .iter()
        .filter_map(|c| match c {
            Command::SetDevice(update) if update.device == 4 => {
                Some(update.output_preferences[0].mode())
            }
            _ => None,
        })
        .collect();
    assert_eq!(modes, [p::OutputMode::Hid, original]);
    assert_eq!(
        h.app
            .device_values(h.app.device(&h.adapter, 4).unwrap())
            .gamepad,
        Some(original)
    );
    assert!(h.app.write_error().is_none());
    drop(sim);
    h.close();
}

#[test]
fn an_ordinary_gamepad_record_does_not_resolve_an_unknown_write() {
    let mut h = Harness::new();
    h.app.act(Action::Show(Page::Device(h.adapter.clone(), 4)));
    h.app.act(Action::Tab(Tab::Settings));
    h.refuse(
        "set_device",
        p::ErrorCode::StorageFailed,
        p::ErrorOutcome::Unknown,
    );
    h.app.act(Action::GamepadOutput(p::OutputMode::Hid));
    h.app.apply_pending();
    h.until(|a| a.write_error().is_some());
    let failure = h.app.write_error().unwrap().to_owned();
    let adapter = h.world.adapters()[0].clone();
    let mut reported = adapter
        .0
        .lock()
        .unwrap()
        .devices
        .iter()
        .find(|d| d.id == 4)
        .unwrap()
        .clone();
    reported.name = "Controller Update".into();
    reported.output_preferences[0].mode = p::OutputMode::Hid as i32;
    adapter.event(p::event::Kind::Device(reported));
    h.until(|a| {
        a.devices()
            .iter()
            .any(|d| d.d.id == 4 && d.name == "Controller Update")
    });
    assert_eq!(
        h.app
            .device_changes(h.app.device(&h.adapter, 4).unwrap())
            .gamepad,
        Some(p::OutputMode::Hid)
    );
    assert_eq!(h.app.write_error(), Some(failure.as_str()));
    assert!(!h.app.writes_pending());

    assert!(h.world.set_present(0, false));
    h.app.update(Msg::Ports(Ok(h.world.ports())));
    h.until(|_| adapter.handles() == 0);
    assert!(h.world.set_present(0, true));
    h.app.act(Action::RefreshAdapters);
    h.until(|a| {
        a.devices().iter().any(|d| d.d.id == 4 && d.available) && a.write_error().is_none()
    });
    assert_eq!(
        h.app
            .device_changes(h.app.device(&h.adapter, 4).unwrap())
            .gamepad,
        None
    );
    assert_eq!(
        h.app
            .device_values(h.app.device(&h.adapter, 4).unwrap())
            .gamepad,
        Some(p::OutputMode::Xinput)
    );
    assert!(!h.app.writes_pending());
    assert_eq!(
        adapter
            .0
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|c| matches!(c, Command::SetDevice(_)))
            .count(),
        1
    );
    h.close();
}

#[test]
fn hiding_keeps_writes_running_and_quit_waits_for_the_followup() {
    let mut h = Harness::new();
    h.delay("set_settings");
    h.dpi(1200);
    h.app.apply_pending();
    h.dpi(1800);
    h.app.hide();
    assert!(h.app.writes_pending());
    assert!(!h.app.quitting());
    h.until(|a| !a.writes_pending());
    assert_eq!(h.settings_writes().len(), 2);
    assert_eq!(h.shown_dpi(), Some(Value::Integer(1800)));
    assert!(h.app.write_error().is_none());
    h.close();
}

impl Harness {
    fn adapter_writes(&self) -> Vec<p::SetAdapter> {
        self.world.adapters()[0]
            .0
            .lock()
            .unwrap()
            .log
            .iter()
            .filter_map(|command| match command {
                Command::SetAdapter(update) => Some(update.clone()),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn direct_interface_choices_apply_the_latest_profile_and_refresh_sends_nothing() {
    let mut h = Harness::new();
    h.delay("set_adapter");
    h.app.show(Page::Adapter(h.adapter.clone()));
    let interface = p::ConfigurationInterface::Via;
    for _ in 0..3 {
        h.app.tick();
        let adapter = h.app.adapter(&h.adapter).unwrap();
        assert_eq!(
            h.app.staged_interface(adapter, interface).unwrap().profile,
            1
        );
        h.app.apply_pending();
    }
    assert!(h.adapter_writes().is_empty());
    h.app.act(Action::InterfaceProfile(interface, 2));
    h.app.apply_pending();
    for profile in [3, 4, 5] {
        h.app.act(Action::InterfaceProfile(interface, profile));
        let adapter = h.app.adapter(&h.adapter).unwrap();
        assert_eq!(
            h.app.staged_interface(adapter, interface).unwrap().profile,
            profile
        );
        h.app.apply_pending();
    }
    h.until(|app| !app.writes_pending());
    let writes = h.adapter_writes();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].configuration_interfaces[0].profile, Some(2));
    assert_eq!(writes[1].configuration_interfaces[0].profile, Some(5));
    assert_eq!(
        h.world.adapters()[0]
            .0
            .lock()
            .unwrap()
            .status
            .configuration_interfaces[0]
            .profile,
        5
    );
    for _ in 0..3 {
        h.app.tick();
        h.app.apply_pending();
    }
    assert_eq!(h.adapter_writes().len(), 2);
    assert!(h.app.write_error().is_none());
    h.close();
}

#[test]
fn direct_interface_choice_allows_profile_zero_only_while_off() {
    let mut h = Harness::new();
    h.app.show(Page::Adapter(h.adapter.clone()));
    let interface = p::ConfigurationInterface::Via;
    h.app.act(Action::InterfaceProfile(interface, 0));
    h.app.apply_pending();
    h.until(|app| !app.writes_pending());
    assert_eq!(
        h.adapter_writes()[0].configuration_interfaces[0].profile,
        Some(0)
    );
    h.app.act(Action::InterfaceProfile(interface, 2));
    h.app.act(Action::InterfaceEnabled(interface, true));
    h.app.apply_pending();
    h.until(|app| !app.writes_pending());
    let before = h.adapter_writes().len();
    h.app.act(Action::InterfaceProfile(interface, 0));
    h.app.apply_pending();
    assert!(!h.app.writes_pending());
    assert_eq!(h.adapter_writes().len(), before);
    let adapter = h.app.adapter(&h.adapter).unwrap();
    let shown = h.app.staged_interface(adapter, interface).unwrap();
    assert!(shown.enabled);
    assert_eq!(shown.profile, 2);
    assert!(h.app.write_error().is_none());
    h.close();
}

#[test]
fn a_dropped_layer_saves_the_final_order_once_and_a_stale_move_does_nothing() {
    let mut h = Harness::new();
    h.app.act(Action::Tab(Tab::Profiles));
    let adapter = h.world.adapters()[0].clone();
    let mut reported = adapter.0.lock().unwrap().devices[0].clone();
    assert_eq!(reported.id, 1);
    reported.profiles = Some(p::ProfileLayers {
        profiles: vec![1, 2, 3],
    });
    adapter.event(p::event::Kind::Device(reported));
    let id = h.adapter.clone();
    let layers = |a: &App<Live>| a.device_values(a.device(&id, 1).unwrap()).layers;
    h.until(|a| layers(a) == Some(vec![1, 2, 3]));
    let moved = |device, from, to| Action::LayerMove {
        adapter: id.clone(),
        device,
        layers: vec![1, 2, 3],
        from,
        to,
    };
    h.app.act(moved(1, 0, 2));
    assert_eq!(layers(&h.app), Some(vec![2, 3, 1]));
    h.app.act(moved(1, 1, 0));
    h.app.act(moved(2, 0, 1));
    h.app.apply_pending();
    h.until(|a| !a.writes_pending());
    let sim = adapter.0.lock().unwrap();
    let saved: Vec<_> = sim
        .log
        .iter()
        .filter_map(|c| match c {
            Command::SetDevice(update) => update.profiles.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        saved,
        [p::ProfileLayers {
            profiles: vec![2, 3, 1]
        }]
    );
    drop(sim);
    assert_eq!(layers(&h.app), Some(vec![2, 3, 1]));
    assert!(h.app.write_error().is_none());
    h.close();
}
