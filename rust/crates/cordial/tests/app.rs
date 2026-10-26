mod common;

use common::{Dongle, Plan, candidate, device, dpi, enable_profiles};
use cordial::{
    migration::Engine,
    settings::{Field, Leaf, Value},
    ui::app::{Action, App, Dialog, Live, Msg, Page, Spot, Tab, Toggle, files::Transfer},
};
use cordial_client::{migration::Outcome, serial::PortInfo};
use cordial_protocol::{self as p, DeviceState, Transport};
use prost::Message as _;
use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

fn setup(dongle: &Dongle, engine: Engine) -> (App<Live>, Receiver<Msg>) {
    let (tx, rx) = mpsc::sync_channel(64);
    let fleet = Live::new(
        Arc::new(move |msg| {
            let _ = tx.send(msg);
        }),
        None,
    )
    .with_backend(
        Arc::new(dongle.connector()),
        || {
            Ok(vec![PortInfo {
                port: "sim".into(),
                serial: String::new(),
            }])
        },
        engine,
    );
    (App::new(fleet), rx)
}

fn until(app: &mut App<Live>, rx: &Receiver<Msg>, done: impl Fn(&App<Live>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(app) {
        assert!(
            Instant::now() < deadline,
            "application did not reach the expected state"
        );
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(msg) => app.update(msg),
            Err(mpsc::RecvTimeoutError::Timeout) => app.tick(),
            Err(e) => panic!("backend channel closed: {e}"),
        }
    }
}

fn ready(app: &mut App<Live>, rx: &Receiver<Msg>) {
    until(app, rx, |app| {
        app.adapter("ADAPTER01").is_some_and(|a| a.ready)
    });
}

#[test]
fn reconnect_keeps_all_devices_locked_until_the_replacement_listing() {
    let dongle = Dongle::with(|s| {
        s.devices = (1..=2)
            .map(|id| {
                let mut d = device(id, &format!("Mouse {id}"), Transport::Ble);
                d.state = DeviceState::Connected as i32;
                d.information.get_or_insert_default().battery = Some(p::BatteryInformation {
                    level: Some(5),
                    ..Default::default()
                });
                d
            })
            .collect();
        s.settings.insert(1, vec![dpi(0, 1000, Some(1000))]);
    });
    let (mut app, rx) = setup(
        &dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    let adapter = "ADAPTER01";
    let page = Page::Device(adapter.into(), 1);
    app.show(page.clone());
    app.act(Action::DeviceToggle(Toggle::Trusted, false));
    app.set_tab(Tab::Settings);
    until(&mut app, &rx, |a| {
        !a.settings_of(a.device(adapter, 1).unwrap()).is_empty()
    });
    let field = Field::hidpp(Leaf::Dpi, Some(0));
    app.act(Action::SettingChoice(field, Value::Integer(1200)));
    assert!(app.settings_dirty(app.device(adapter, 1).unwrap()));
    assert_eq!(app.alerts().len(), 2);

    dongle.0.lock().unwrap().status.ready = false;
    dongle.end_session(p::SessionEndingReason::ConfigurationChange as i32);
    until(&mut app, &rx, |a| {
        a.adapter(adapter).is_some_and(|a| !a.connected())
    });
    let assert_retained = |app: &App<Live>| {
        assert_eq!(app.devices().len(), 2);
        assert_eq!(app.shown(), page);
        for d in app.devices() {
            assert!(!d.available);
            assert!(!d.connected());
            assert_eq!(d.status(), "Disconnected");
            assert_eq!(d.d.state(), DeviceState::Connected);
            assert!(d.read_error.is_none());
            assert!(app.device_locked(d));
            assert!(app.form_busy(d));
            assert!(!app.can_refresh(d));
            assert!(!d.battery.as_ref().unwrap().fresh);
            assert_eq!(d.battery.as_ref().unwrap().percent, Some(5));
        }
        assert!(app.alerts().is_empty());
        let d = app.device(adapter, 1).unwrap();
        assert_eq!(app.device_values(d).trusted, Some(false));
        assert!(app.draft_of(d, &field).is_some());
        assert!(!app.settings_savable(d));
    };
    assert_retained(&app);
    until(&mut app, &rx, |a| {
        a.adapter(adapter)
            .is_some_and(|a| a.connected() && !a.ready)
    });
    assert_retained(&app);
    for action in [
        Action::SettingsSave,
        Action::SettingsRefresh,
        Action::DetailsSave,
        Action::Connect,
        Action::Disconnect,
        Action::Forget,
    ] {
        app.act(action);
    }
    assert!(app.dialog().is_none());

    let status = {
        let mut sim = dongle.0.lock().unwrap();
        sim.devices.pop();
        sim.devices[0].name = "Returned Mouse".into();
        sim.status.ready = true;
        sim.status.clone()
    };
    dongle.event(p::event::Kind::Adapter(status));
    ready(&mut app, &rx);
    until(&mut app, &rx, |a| {
        !a.settings_of(a.device(adapter, 1).unwrap()).is_empty()
    });
    assert_eq!(app.devices().len(), 1);
    assert!(app.device(adapter, 2).is_none());
    let d = app.device(adapter, 1).unwrap();
    assert_eq!(d.name, "Returned Mouse");
    assert!(d.available && d.connected());
    assert!(!app.device_locked(d));
    assert!(d.battery.as_ref().unwrap().fresh);
    assert_eq!(app.device_values(d).trusted, Some(false));
    let setting = app.setting(d, &field).unwrap();
    assert_eq!(app.shown_value(d, &setting), Some(Value::Integer(1200)));
    assert!(app.settings_savable(d));
    assert!(!dongle.sent().iter().any(|command| matches!(
        *command,
        "set_device"
            | "set_settings"
            | "refresh_device"
            | "connect_device"
            | "disconnect_device"
            | "unpair_device"
    )));
    app.quit();
    until(&mut app, &rx, App::done);
}

#[test]
fn an_ending_session_rejects_queued_edits_without_sending_or_replaying_them() {
    let dongle = Dongle::default();
    let (mut app, rx) = setup(
        &dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    app.enable_immediate();
    let adapter = "ADAPTER01";
    app.show(Page::Device(adapter.into(), 1));
    let trusted = app.device(adapter, 1).unwrap().d.trusted;
    app.act(Action::DeviceToggle(Toggle::Trusted, !trusted));
    assert!(app.writes_pending());
    assert_eq!(
        app.device_values(app.device(adapter, 1).unwrap()).trusted,
        Some(!trusted)
    );

    dongle.event(p::event::Kind::SessionEnding(p::SessionEnding {
        reason: p::SessionEndingReason::ConfigurationChange as i32,
    }));
    until(&mut app, &rx, |a| {
        a.adapter(adapter).is_some_and(|a| !a.connected())
    });
    app.apply_pending();
    assert!(!app.writes_pending());
    assert_eq!(
        app.write_error(),
        Some("This adapter is no longer available.")
    );
    assert_eq!(
        app.note(&Spot::DetailsSave(adapter.into(), 1)),
        Some("This adapter is no longer available.")
    );
    assert_eq!(
        app.device_values(app.device(adapter, 1).unwrap()).trusted,
        Some(trusted)
    );
    assert!(!dongle.sent().contains(&"set_device"));

    dongle.unplug();
    ready(&mut app, &rx);
    app.apply_pending();
    assert!(!app.writes_pending());
    assert!(!dongle.sent().contains(&"set_device"));
    assert_eq!(app.device(adapter, 1).unwrap().d.trusted, trusted);
    app.quit();
    until(&mut app, &rx, App::done);
}

#[test]
fn forms_settings_profiles_pairing_and_files_work_without_terminal_state() {
    let dongle = Dongle::with(|s| {
        enable_profiles(&mut s.status);
        let mut d = device(1, "Example Mouse", Transport::Ble);
        d.state = DeviceState::Connected as i32;
        d.integrations[0].status = Some(p::integration::Status::State(
            p::IntegrationState::Active as i32,
        ));
        s.devices = vec![d];
        s.settings.insert(1, vec![dpi(0, 1000, Some(1000))]);
        s.candidates = vec![candidate(9, "Example Keyboard")];
        s.plan = Plan::EnterCode("123456".into());
        s.files.insert(
            "/".into(),
            vec![p::FileEntry {
                name: "sample.txt".into(),
                size: 4,
                ..Default::default()
            }],
        );
        s.data.insert("/sample.txt".into(), b"data".to_vec());
    });
    let (mut app, rx) = setup(
        &dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    let adapter = "ADAPTER01";

    app.act(Action::RenameAdapter(adapter.into()));
    app.set_form("Studio");
    assert_eq!(app.form(), "Studio");
    assert!(app.form_valid());
    app.act(Action::Submit);
    assert!(app.dialog_busy());
    until(&mut app, &rx, |a| a.dialog().is_none());
    assert!(!app.dialog_busy());
    assert_eq!(app.adapter(adapter).unwrap().name, "Studio");

    app.show(Page::Device(adapter.into(), 1));
    app.act(Action::DeviceToggle(Toggle::Trusted, false));
    assert_eq!(
        app.device_values(app.device(adapter, 1).unwrap()).trusted,
        Some(false)
    );
    app.act(Action::DetailsSave);
    until(&mut app, &rx, |a| {
        !a.device_saving(a.device(adapter, 1).unwrap(), Tab::Details)
    });
    assert!(!app.device(adapter, 1).unwrap().d.trusted);
    assert!(app.note(&Spot::DetailsSave(adapter.into(), 1)).is_none());

    app.set_tab(Tab::Settings);
    until(&mut app, &rx, |a| {
        !a.settings_of(a.device(adapter, 1).unwrap()).is_empty()
    });
    let field = Field::hidpp(Leaf::Dpi, Some(0));
    app.act(Action::SettingChoice(field, Value::Integer(1200)));
    let d = app.device(adapter, 1).unwrap();
    let setting = app.setting(d, &field).unwrap();
    assert_eq!(app.shown_value(d, &setting), Some(Value::Integer(1200)));
    assert!(app.settings_savable(d));
    app.act(Action::SettingsSave);
    until(&mut app, &rx, |a| {
        !a.submitting(a.device(adapter, 1).unwrap())
    });
    let d = app.device(adapter, 1).unwrap();
    assert_eq!(
        app.setting(d, &field).unwrap().saved,
        Some(Value::Integer(1200))
    );
    assert!(!app.settings_dirty(d));

    app.show(Page::Adapter(adapter.into()));
    app.set_tab(Tab::Profiles);
    app.act(Action::ProfileNew);
    app.set_form("Work");
    assert!(app.form_valid());
    app.act(Action::Submit);
    until(&mut app, &rx, |a| a.dialog().is_none());
    until(&mut app, &rx, |a| {
        a.shown_page(adapter, false)
            .is_some_and(|p| p.shown().0.iter().any(|p| p.name == "Work"))
    });

    app.act(Action::Files);
    until(&mut app, &rx, |a| !a.files().entries.is_empty());
    app.act(Action::FilesEntry("sample.txt".into()));
    let directory = std::env::temp_dir().join(format!("cordial-app-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("download.txt");
    app.download_to(destination.clone(), false);
    until(&mut app, &rx, |a| {
        a.files()
            .download
            .as_ref()
            .is_some_and(|d| matches!(d.state, Transfer::Saved(..)))
    });
    assert_eq!(std::fs::read(destination).unwrap(), b"data");
    std::fs::remove_dir_all(directory).unwrap();
    app.act(Action::Cancel);

    app.act(Action::AddDevice);
    until(&mut app, &rx, |a| {
        a.state_of(adapter)
            .is_some_and(|s| !s.candidates.is_empty())
    });
    app.act(Action::Pair(9));
    until(&mut app, &rx, App::pairing_code_input);
    app.set_form("bad");
    assert!(!app.form_valid());
    app.set_form("123456");
    assert!(app.form_valid());
    app.act(Action::Accept);
    until(&mut app, &rx, |a| {
        a.add()
            .and_then(|a| a.pairing.as_ref())
            .is_some_and(|p| p.device.is_some())
    });
    app.hide();
    assert!(app.dialog().is_none());

    app.quit();
    until(&mut app, &rx, App::done);
    assert_eq!(dongle.handles(), 0);
}

#[test]
fn converted_download_reconfirms_its_final_destination_before_replacing() {
    let sequence = p::storage::Sequence {
        format: p::storage::CURRENT_FORMAT,
        next_device: 3,
        next_profile: 2,
    };
    let data = sequence.encode_to_vec();
    let dongle = Dongle::with(|s| {
        s.files.insert(
            "/".into(),
            vec![p::FileEntry {
                name: "sequence.pb".into(),
                size: data.len() as u64,
                ..Default::default()
            }],
        );
        s.data.insert("/sequence.pb".into(), data);
    });
    let (mut app, rx) = setup(
        &dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    app.show(Page::Adapter("ADAPTER01".into()));
    app.act(Action::Files);
    until(&mut app, &rx, |a| !a.files().entries.is_empty());
    app.act(Action::FilesEntry("sequence.pb".into()));

    let directory =
        std::env::temp_dir().join(format!("cordial-app-overwrite-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let chosen = directory.join("backup.pb");
    let converted = directory.join("backup.json");
    std::fs::write(&chosen, b"chosen").unwrap();
    std::fs::write(&converted, b"converted").unwrap();

    app.download_to(chosen.clone(), true);
    assert!(matches!(
        app.dialog(),
        Some(Dialog::Replace(adapter, target))
            if adapter == "ADAPTER01"
                && target.path == "/sequence.pb"
                && target.local == chosen
    ));
    assert!(app.files().download.is_none());
    assert_eq!(std::fs::read(&chosen).unwrap(), b"chosen");
    assert_eq!(std::fs::read(&converted).unwrap(), b"converted");

    app.act(Action::Confirm);
    until(&mut app, &rx, |a| {
        a.files()
            .download
            .as_ref()
            .is_some_and(|d| matches!(d.state, Transfer::Saved(..)))
    });
    assert_eq!(std::fs::read(&chosen).unwrap(), b"chosen");
    assert_eq!(
        serde_json::from_slice::<p::storage::Sequence>(&std::fs::read(&converted).unwrap())
            .unwrap(),
        sequence
    );
    std::fs::remove_dir_all(directory).unwrap();

    app.quit();
    until(&mut app, &rx, App::done);
    assert_eq!(dongle.handles(), 0);
}

#[test]
fn injected_migration_releases_the_controller_and_reopens_in_process() {
    let dongle = Dongle::with(|s| {
        s.status.ready = false;
        s.status.errors = vec![p::Error {
            code: p::ErrorCode::StorageMigrationRequired as i32,
            reason: p::ErrorReason::Storage as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }];
    });
    let migrating = dongle.clone();
    let engine: Engine = Arc::new(move |id, port, _, _| {
        assert_eq!(id, "ADAPTER01");
        assert_eq!(port, Some("sim"));
        assert_eq!(migrating.handles(), 0);
        let mut s = migrating.0.lock().unwrap();
        s.status.ready = true;
        s.status.errors.clear();
        Ok(Outcome::Success)
    });
    let (mut app, rx) = setup(&dongle, engine);
    ready(&mut app, &rx);
    assert!(!app.migrating());
    assert!(dongle.sessions() >= 2);
    assert_ne!(app.dialog(), Some(&Dialog::AddDevice));
    app.quit();
    until(&mut app, &rx, App::done);
    assert_eq!(dongle.handles(), 0);
}

#[test]
fn unplug_without_an_event_removes_the_adapter_and_its_devices_immediately() {
    let dongle = Dongle::with(|sim| {
        sim.status.information.as_mut().unwrap().firmware_version = Some("0.1.0".into());
    });
    let (mut app, rx) = setup(
        &dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    app.show(Page::Device("ADAPTER01".into(), 1));
    assert!(!app.devices().is_empty());
    dongle.unplug();
    loop {
        let msg = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let lost = matches!(&msg, Msg::Fleet(_, event) if matches!(&**event, cordial::controller::Event::Connection { phase: cordial::controller::Phase::Lost(_), .. }));
        app.update(msg);
        if lost {
            break;
        }
    }
    assert!(app.adapters().is_empty());
    assert!(app.devices().is_empty());
    assert_eq!(app.shown(), Page::Overview);
    app.quit();
    until(&mut app, &rx, App::done);
}

#[test]
fn an_idle_unknown_ending_reason_retains_the_page_until_configuration_returns() {
    let dongle = Dongle::default();
    let (mut app, rx) = setup(
        &dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    let page = Page::Device("ADAPTER01".into(), 1);
    app.show(page.clone());
    dongle.end_session(12345);
    until(&mut app, &rx, |a| {
        a.adapter("ADAPTER01").is_some_and(|a| !a.connected())
    });
    assert_eq!(app.shown(), page);
    assert!(app.devices().iter().all(|d| !d.available));
    app.act(Action::RefreshAdapters);
    ready(&mut app, &rx);
    assert_eq!(app.shown(), page);
    assert!(app.devices().iter().all(|d| d.available));
    app.quit();
    until(&mut app, &rx, App::done);
}
