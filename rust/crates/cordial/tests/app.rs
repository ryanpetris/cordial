mod common;

use common::{Dongle, Plan, candidate, device, dpi, enable_profiles};
use cordial::{
    controller::{Event, Outcome as Done},
    error::Error,
    migration::Engine,
    settings::{Field, Leaf, Value},
    ui::app::{
        Action, App, Dialog, Live, Msg, Page, Spot, Tab, Toggle,
        profiles::{DeletionView, Mark, Stage},
    },
};
use cordial_client::{migration::Outcome, serial::PortInfo};
use cordial_protocol::{self as p, DeviceState, Transport};
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
fn forms_settings_profiles_and_pairing_work_without_terminal_state() {
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

const ADAPTER: &str = "ADAPTER01";

/// Work (1) in Office Mouse's layers between Games (2) and Keys (3), alone in Travel Pad's, and
/// on VIA, which is on, and Vial, which is off. Desk Keyboard uses only Games, and nothing uses
/// Spare (4).
fn profiles_in_use() -> Dongle {
    Dongle::with(|s| {
        enable_profiles(&mut s.status);
        s.reconnect_interfaces = true;
        let work = s.add_profile("Work", Vec::new());
        let games = s.add_profile("Games", Vec::new());
        let keys = s.add_profile("Keys", Vec::new());
        s.add_profile("Spare", Vec::new());
        let with = |id, name, layers: Vec<u32>| p::Device {
            profiles: Some(p::ProfileLayers { profiles: layers }),
            ..device(id, name, Transport::Ble)
        };
        s.devices = vec![
            with(1, "Office Mouse", vec![games, work, keys]),
            with(2, "Travel Pad", vec![work]),
            with(3, "Desk Keyboard", vec![games]),
        ];
        let [via, vial] = &mut s.status.configuration_interfaces[..] else {
            panic!()
        };
        (via.enabled, via.active, via.profile) = (true, true, work);
        vial.profile = work;
    })
}

/// Opens Delete for Work once the view names everything its confirmation lists.
fn confirm_delete(dongle: &Dongle) -> (App<Live>, Receiver<Msg>) {
    let (mut app, rx) = setup(
        dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    app.show(Page::Adapter(ADAPTER.into()));
    app.set_tab(Tab::Profiles);
    until(&mut app, &rx, |a| {
        a.deletion_steps(ADAPTER, 1).last().map(String::as_str) == Some("Delete “Work”")
    });
    app.act(Action::ProfileDelete(1));
    assert_eq!(
        app.dialog(),
        Some(&Dialog::ProfileDelete(ADAPTER.into(), 1))
    );
    (app, rx)
}

/// Answers Delete with the changes the confirmation for Work shows now.
fn confirm_deletion(app: &mut App<Live>) {
    let plan = app.deletion(ADAPTER, 1).unwrap();
    app.act(Action::ProfileDeleteConfirm(plan));
}

/// The changes the adapter received since `from`, in order.
fn changes(dongle: &Dongle, from: usize) -> Vec<p::request::Command> {
    use p::request::Command as C;
    dongle.0.lock().unwrap().log[from..]
        .iter()
        .filter(|c| matches!(c, C::SetDevice(_) | C::SetAdapter(_) | C::DeleteProfile(_)))
        .cloned()
        .collect()
}

fn detach(device: u32, profiles: Vec<u32>) -> p::request::Command {
    p::request::Command::SetDevice(p::SetDevice {
        device,
        profiles: Some(p::ProfileLayers { profiles }),
        ..Default::default()
    })
}

fn unassign(interfaces: &[p::ConfigurationInterface]) -> p::request::Command {
    p::request::Command::SetAdapter(p::SetAdapter {
        configuration_interfaces: interfaces
            .iter()
            .map(|i| p::ConfigurationInterfaceUpdate {
                interface: *i as i32,
                enabled: Some(false),
                profile: Some(0),
            })
            .collect(),
        ..Default::default()
    })
}

fn delete(profile: u32) -> p::request::Command {
    p::request::Command::DeleteProfile(p::DeleteProfile { profile })
}

fn layers_of(dongle: &Dongle, id: u32) -> Vec<u32> {
    let sim = dongle.0.lock().unwrap();
    let d = sim.devices.iter().find(|d| d.id == id).unwrap();
    d.profiles.clone().unwrap_or_default().profiles
}

fn has_profile(dongle: &Dongle, id: u32) -> bool {
    dongle.0.lock().unwrap().profiles.iter().any(|p| p.id == id)
}

#[test]
fn deleting_a_profile_in_use_makes_the_listed_changes_in_order() {
    use p::ConfigurationInterface::{Via, Vial};
    let dongle = profiles_in_use();
    let (mut app, rx) = confirm_delete(&dongle);
    assert_eq!(
        app.deletion_steps(ADAPTER, 1),
        [
            "Remove it from Office Mouse, keeping the other profiles in order",
            "Remove it from Travel Pad",
            "Turn off VIA and clear its profile",
            "Clear Vial's profile",
            "Delete “Work”",
        ]
    );
    // A profile nothing uses lists no changes.
    assert!(app.deletion_steps(ADAPTER, 4).is_empty());
    let from = dongle.0.lock().unwrap().log.len();
    confirm_deletion(&mut app);
    assert!(app.dialog_busy());
    until(&mut app, &rx, |a| a.dialog().is_none());
    assert_eq!(
        changes(&dongle, from),
        [
            detach(1, vec![2, 3]),
            detach(2, Vec::new()),
            unassign(&[Via, Vial]),
            delete(1),
        ]
    );
    assert_eq!(layers_of(&dongle, 3), [2]);
    assert!(!has_profile(&dongle, 1) && has_profile(&dongle, 2) && has_profile(&dongle, 3));
}

#[test]
fn a_failed_change_stops_the_deletion_and_keeps_what_was_done() {
    use p::ConfigurationInterface::{Via, Vial};
    let dongle = profiles_in_use();
    dongle.0.lock().unwrap().refuse.insert(
        "set_adapter",
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        },
    );
    let (mut app, rx) = confirm_delete(&dongle);
    let from = dongle.0.lock().unwrap().log.len();
    confirm_deletion(&mut app);
    until(&mut app, &rx, |a| !a.dialog_busy());
    assert_eq!(
        app.dialog(),
        Some(&Dialog::ProfileDelete(ADAPTER.into(), 1))
    );
    let note = app.note(&Spot::Dialog).unwrap();
    assert!(
        note.starts_with("The adapter couldn't finish deleting “Work”. ")
            && note.ends_with(" Try again."),
        "{note}"
    );
    assert_eq!(
        changes(&dongle, from),
        [
            detach(1, vec![2, 3]),
            detach(2, Vec::new()),
            unassign(&[Via, Vial])
        ]
    );
    assert!(has_profile(&dongle, 1));
    // The confirmation now lists only what is left, which a second try does.
    until(&mut app, &rx, |a| a.deletion_steps(ADAPTER, 1).len() == 3);
    assert_eq!(
        app.deletion_steps(ADAPTER, 1),
        [
            "Turn off VIA and clear its profile",
            "Clear Vial's profile",
            "Delete “Work”",
        ]
    );
    dongle.0.lock().unwrap().refuse.clear();
    let from = dongle.0.lock().unwrap().log.len();
    confirm_deletion(&mut app);
    until(&mut app, &rx, |a| a.dialog().is_none());
    assert_eq!(changes(&dongle, from), [unassign(&[Via, Vial]), delete(1)]);
    assert_eq!(layers_of(&dongle, 1), [2, 3]);
}

/// Applies messages, each through `alter`, until `done`, and returns each new way Work's
/// deletion showed, starting with how it shows now.
fn deletion_views(
    app: &mut App<Live>,
    rx: &Receiver<Msg>,
    mut alter: impl FnMut(Msg) -> Msg,
    done: impl Fn(&App<Live>) -> bool,
) -> Vec<DeletionView> {
    let mut views = vec![app.deletion_view(ADAPTER, 1)];
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(app) {
        assert!(
            Instant::now() < deadline,
            "application did not reach the expected state"
        );
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(msg) => app.update(alter(msg)),
            Err(mpsc::RecvTimeoutError::Timeout) => app.tick(),
            Err(e) => panic!("backend channel closed: {e}"),
        }
        let view = app.deletion_view(ADAPTER, 1);
        if views.last() != Some(&view) {
            views.push(view);
        }
    }
    views
}

fn marks(views: &[DeletionView]) -> Vec<(Stage, Vec<Mark>, bool)> {
    views
        .iter()
        .map(|v| (v.stage, v.marks.clone(), v.waiting))
        .collect()
}

const WORK_STEPS: [&str; 5] = [
    "Remove it from Office Mouse, keeping the other profiles in order",
    "Remove it from Travel Pad",
    "Turn off VIA and clear its profile",
    "Clear Vial's profile",
    "Delete “Work”",
];

#[test]
fn a_deletion_marks_each_change_once_the_adapter_confirms_it() {
    use Mark::{Active as A, Done as D, Pending as P};
    let dongle = profiles_in_use();
    let (mut app, rx) = confirm_delete(&dongle);
    app.keep_deleted();
    let asked = app.deletion_view(ADAPTER, 1);
    assert_eq!((asked.stage, asked.name.as_str()), (Stage::Ask, "Work"));
    assert_eq!(asked.steps, WORK_STEPS);
    assert_eq!(asked.marks, [P; 5]);
    confirm_deletion(&mut app);
    let views = deletion_views(&mut app, &rx, |m| m, |a| !a.dialog_busy());
    // VIA and Vial change in one request. Turning VIA off restarts the adapter's USB
    // configuration, and the deletion waits for it before it is sent.
    assert_eq!(
        marks(&views),
        [
            (Stage::Running, vec![A, P, P, P, P], false),
            (Stage::Running, vec![D, A, P, P, P], false),
            (Stage::Running, vec![D, D, A, A, P], false),
            (Stage::Running, vec![D, D, D, D, A], true),
            (Stage::Running, vec![D, D, D, D, A], false),
            (Stage::Deleted, vec![D; 5], false),
        ]
    );
    // The changes stay as confirmed while the adapter's view of them goes away.
    assert!(
        views
            .iter()
            .all(|v| v.steps == WORK_STEPS && v.name == "Work")
    );
    assert!(!has_profile(&dongle, 1));
    assert_eq!(
        app.dialog(),
        Some(&Dialog::ProfileDelete(ADAPTER.into(), 1)),
        "a finished deletion stays until it is closed"
    );
    app.act(Action::Cancel);
    assert!(app.dialog().is_none());
}

#[test]
fn a_retried_deletion_keeps_what_was_done_and_makes_only_the_rest() {
    use Mark::{Active as A, Done as D, Failed as F, Pending as P};
    use p::ConfigurationInterface::{Via, Vial};
    let dongle = profiles_in_use();
    dongle.0.lock().unwrap().refuse.insert(
        "set_adapter",
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        },
    );
    let (mut app, rx) = confirm_delete(&dongle);
    app.keep_deleted();
    confirm_deletion(&mut app);
    let views = deletion_views(&mut app, &rx, |m| m, |a| !a.dialog_busy());
    assert_eq!(
        marks(&views),
        [
            (Stage::Running, vec![A, P, P, P, P], false),
            (Stage::Running, vec![D, A, P, P, P], false),
            (Stage::Running, vec![D, D, A, A, P], false),
            (Stage::Ask, vec![D, D, F, F, P], false),
        ],
        "a refused change is never marked done"
    );
    let failed = views.last().unwrap();
    assert_eq!(failed.steps, WORK_STEPS);
    assert!(!failed.uncertain);

    dongle.0.lock().unwrap().refuse.clear();
    let from = dongle.0.lock().unwrap().log.len();
    confirm_deletion(&mut app);
    let views = deletion_views(&mut app, &rx, |m| m, |a| !a.dialog_busy());
    assert_eq!(
        marks(&views),
        [
            (Stage::Running, vec![D, D, A, A, P], false),
            (Stage::Running, vec![D, D, D, D, A], true),
            (Stage::Running, vec![D, D, D, D, A], false),
            (Stage::Deleted, vec![D; 5], false),
        ]
    );
    assert!(views.iter().all(|v| v.steps == WORK_STEPS));
    assert_eq!(changes(&dongle, from), [unassign(&[Via, Vial]), delete(1)]);
}

#[test]
fn a_new_user_after_a_failure_is_listed_and_needs_another_answer() {
    use Mark::{Done as D, Pending as P};
    use p::ConfigurationInterface::{Via, Vial};
    let dongle = profiles_in_use();
    dongle.0.lock().unwrap().refuse.insert(
        "set_adapter",
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        },
    );
    let (mut app, rx) = confirm_delete(&dongle);
    app.keep_deleted();
    confirm_deletion(&mut app);
    until(&mut app, &rx, |a| !a.dialog_busy());
    let stale = app.deletion(ADAPTER, 1).unwrap();
    {
        let mut sim = dongle.0.lock().unwrap();
        sim.refuse.clear();
        sim.devices[2].profiles = Some(p::ProfileLayers {
            profiles: vec![2, 1],
        });
        let d = sim.devices[2].clone();
        sim.event(p::event::Kind::Device(d));
    }
    until(&mut app, &rx, |a| {
        a.deletion(ADAPTER, 1).as_ref() != Some(&stale)
    });
    let view = app.deletion_view(ADAPTER, 1);
    let mut steps = WORK_STEPS[..2].to_vec();
    steps.extend([
        "Remove it from Desk Keyboard, keeping the other profiles in order",
        "Turn off VIA and clear its profile",
        "Clear Vial's profile",
        "Delete “Work”",
    ]);
    assert_eq!(view.steps, steps);
    assert_eq!(view.marks, [D, D, P, P, P, P]);

    let from = dongle.0.lock().unwrap().log.len();
    app.act(Action::ProfileDeleteConfirm(stale));
    assert!(
        !app.dialog_busy(),
        "the old answer doesn't cover Desk Keyboard"
    );
    assert!(changes(&dongle, from).is_empty());
    confirm_deletion(&mut app);
    let views = deletion_views(&mut app, &rx, |m| m, |a| !a.dialog_busy());
    assert_eq!(views.last().unwrap().stage, Stage::Deleted);
    assert!(views.iter().all(|v| v.steps == steps));
    assert_eq!(
        changes(&dongle, from),
        [detach(3, vec![2]), unassign(&[Via, Vial]), delete(1)]
    );
}

#[test]
fn an_unconfirmed_deletion_is_a_warning_and_is_not_tried_again() {
    use Mark::{Done as D, Unconfirmed as U};
    let dongle = profiles_in_use();
    let (mut app, rx) = confirm_delete(&dongle);
    app.keep_deleted();
    confirm_deletion(&mut app);
    // The adapter's answer to the deletion never arrives.
    let lost = |msg: Msg| match msg {
        Msg::Fleet(slot, event) => match *event {
            Event::Done {
                session,
                ticket,
                result: Ok(Done::ProfileDeleted(_)),
            } => Msg::Fleet(
                slot,
                Box::new(Event::Done {
                    session,
                    ticket,
                    result: Err(Error {
                        command: Some("profile delete"),
                        ..Error::new(
                            "the profile deletion is incomplete: the adapter didn't respond in time",
                        )
                    }),
                }),
            ),
            event => Msg::Fleet(slot, Box::new(event)),
        },
        msg => msg,
    };
    let views = deletion_views(&mut app, &rx, lost, |a| !a.dialog_busy());
    let last = views.last().unwrap();
    assert_eq!(
        (last.stage, last.marks.as_slice()),
        (Stage::Ask, &[D, D, D, D, U][..])
    );
    assert!(last.uncertain);
    assert_eq!(
        app.note(&Spot::Dialog),
        Some(
            "The adapter couldn't confirm whether the profile was deleted. Earlier changes were \
             saved. Check the current settings before trying again."
        )
    );
    let sent = changes(&dongle, 0);
    for _ in 0..5 {
        app.tick();
    }
    assert_eq!(
        changes(&dongle, 0),
        sent,
        "no write is retried while background readbacks finish"
    );
}

#[test]
fn a_user_found_after_confirming_stops_the_deletion_and_is_left_alone() {
    let dongle = profiles_in_use();
    let (mut app, rx) = confirm_delete(&dongle);
    // Desk Keyboard starts using Work after the confirmation was shown.
    {
        let mut sim = dongle.0.lock().unwrap();
        sim.devices[2].profiles = Some(p::ProfileLayers {
            profiles: vec![2, 1],
        });
        let d = sim.devices[2].clone();
        sim.event(p::event::Kind::Device(d));
    }
    let from = dongle.0.lock().unwrap().log.len();
    confirm_deletion(&mut app);
    until(&mut app, &rx, |a| !a.dialog_busy());
    let note = app.note(&Spot::Dialog).unwrap();
    assert!(
        note.starts_with(
            "The adapter couldn't finish deleting “Work”. Desk Keyboard is using this profile."
        ),
        "{note}"
    );
    let sent = changes(&dongle, from);
    assert!(
        !sent
            .iter()
            .any(|c| matches!(c, p::request::Command::SetDevice(d) if d.device == 3)),
        "{sent:?}"
    );
    assert!(!sent.contains(&delete(1)), "{sent:?}");
    assert_eq!(layers_of(&dongle, 3), [2, 1]);
    assert!(has_profile(&dongle, 1));
    // Trying again lists Desk Keyboard for confirmation first.
    until(&mut app, &rx, |a| {
        a.deletion_steps(ADAPTER, 1)
            .iter()
            .any(|s| s.contains("Desk Keyboard"))
    });
}

#[test]
fn a_user_reported_after_the_confirmation_was_shown_needs_another_answer() {
    let dongle = profiles_in_use();
    let (mut app, rx) = confirm_delete(&dongle);
    let shown = app.deletion(ADAPTER, 1).unwrap();
    // Desk Keyboard starts using Work, and the view learns it before Delete is answered.
    {
        let mut sim = dongle.0.lock().unwrap();
        sim.devices[2].profiles = Some(p::ProfileLayers {
            profiles: vec![2, 1],
        });
        let d = sim.devices[2].clone();
        sim.event(p::event::Kind::Device(d));
    }
    until(&mut app, &rx, |a| {
        a.deletion_steps(ADAPTER, 1)
            .iter()
            .any(|s| s.contains("Desk Keyboard"))
    });
    let from = dongle.0.lock().unwrap().log.len();
    app.act(Action::ProfileDeleteConfirm(shown));
    assert!(!app.dialog_busy());
    assert_eq!(
        app.dialog(),
        Some(&Dialog::ProfileDelete(ADAPTER.into(), 1))
    );
    assert_eq!(
        app.note(&Spot::Dialog),
        Some("The changes needed to delete “Work” changed. Review them and try again.")
    );
    // Answering the changes shown now makes them, and only them.
    confirm_deletion(&mut app);
    until(&mut app, &rx, |a| a.dialog().is_none());
    use p::ConfigurationInterface::{Via, Vial};
    assert_eq!(
        changes(&dongle, from),
        [
            detach(1, vec![2, 3]),
            detach(2, Vec::new()),
            detach(3, vec![2]),
            unassign(&[Via, Vial]),
            delete(1),
        ]
    );
}

#[test]
fn the_adapters_own_refusal_stops_a_deletion_as_incomplete() {
    let dongle = profiles_in_use();
    dongle
        .0
        .lock()
        .unwrap()
        .refuse
        .insert("delete_profile", common::refusal(p::ErrorCode::InUse));
    let (mut app, rx) = confirm_delete(&dongle);
    confirm_deletion(&mut app);
    until(&mut app, &rx, |a| !a.dialog_busy());
    let note = app.note(&Spot::Dialog).unwrap();
    assert!(
        note.starts_with("The adapter couldn't finish deleting “Work”."),
        "{note}"
    );
    assert!(has_profile(&dongle, 1));
    assert!(layers_of(&dongle, 2).is_empty());
}

#[test]
fn an_empty_picker_creates_a_profile_and_returns_to_choose_it() {
    let dongle = Dongle::with(|s| enable_profiles(&mut s.status));
    let (mut app, rx) = setup(
        &dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    app.show(Page::Device(ADAPTER.into(), 1));
    app.set_tab(Tab::Profiles);
    app.act(Action::LayerAdd);
    until(&mut app, &rx, |a| {
        a.shown_page(ADAPTER, true)
            .is_some_and(|p| p.listed && !p.reading && p.entries.is_empty())
    });
    app.act(Action::PickNewProfile);
    assert_eq!(
        app.dialog(),
        Some(&Dialog::ProfileName(ADAPTER.into(), None))
    );
    app.set_form("Work");
    app.act(Action::Submit);
    until(
        &mut app,
        &rx,
        |a| matches!(a.dialog(), Some(Dialog::Pick { chosen, .. }) if *chosen != 0),
    );
    let Some(Dialog::Pick {
        adapter, chosen, ..
    }) = app.dialog().cloned()
    else {
        panic!()
    };
    assert_eq!(adapter, ADAPTER);
    until(&mut app, &rx, |a| {
        a.shown_page(ADAPTER, true)
            .is_some_and(|p| p.shown().0.iter().any(|p| p.id == chosen))
    });
    app.act(Action::Submit);
    assert!(app.dialog().is_none());
    let d = app.device(ADAPTER, 1).unwrap();
    assert_eq!(app.device_values(d).layers, Some(vec![chosen]));
}

#[test]
fn a_failed_picker_listing_can_be_retried_in_the_picker() {
    let dongle = profiles_in_use();
    dongle
        .0
        .lock()
        .unwrap()
        .refuse
        .insert("list_profiles", common::refusal(p::ErrorCode::Busy));
    let (mut app, rx) = setup(
        &dongle,
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    ready(&mut app, &rx);
    app.show(Page::Device(ADAPTER.into(), 3));
    app.set_tab(Tab::Profiles);
    app.act(Action::LayerAdd);
    until(&mut app, &rx, |a| {
        a.shown_page(ADAPTER, true)
            .is_some_and(|p| !p.reading && p.error.is_some())
    });
    assert!(app.picker_retryable(ADAPTER));
    app.act(Action::Pick(4));

    dongle.0.lock().unwrap().refuse.clear();
    app.act(Action::PickRetry);
    let page = app.shown_page(ADAPTER, true).unwrap();
    assert!(page.reading && page.error.is_none());
    assert!(!app.picker_retryable(ADAPTER));
    until(&mut app, &rx, |a| {
        a.shown_page(ADAPTER, true)
            .is_some_and(|p| p.listed && !p.reading)
    });
    let page = app.shown_page(ADAPTER, true).unwrap();
    assert!(page.error.is_none());
    let names: Vec<&str> = page.all().0.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Work", "Games", "Keys", "Spare"]);
    assert!(
        matches!(app.dialog(), Some(Dialog::Pick { chosen: 4, .. })),
        "{:?}",
        app.dialog()
    );
    app.act(Action::Submit);
    assert!(app.dialog().is_none());
    let d = app.device(ADAPTER, 3).unwrap();
    assert_eq!(app.device_values(d).layers, Some(vec![2, 4]));
}

#[test]
fn a_deletion_success_stays_visible_when_loss_precedes_its_result() {
    deletion_success_with_loss(false);
}

#[test]
fn a_deletion_success_stays_visible_when_loss_follows_its_result() {
    deletion_success_with_loss(true);
}

fn deletion_success_with_loss(result_first: bool) {
    use cordial::controller::Phase;

    for keep_deleted in [true, false] {
        let dongle = profiles_in_use();
        let (mut app, rx) = confirm_delete(&dongle);
        if keep_deleted {
            app.keep_deleted();
        }
        confirm_deletion(&mut app);
        let (slot, session, result) = loop {
            let msg = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if let Msg::Fleet(slot, event) = &msg
                && let Event::Done {
                    session,
                    result: Ok(Done::ProfileDeleted(profile)),
                    ..
                } = event.as_ref()
                && profile.id == 1
            {
                break (*slot, *session, msg);
            }
            app.update(msg);
        };
        assert!(!has_profile(&dongle, 1));
        assert!(app.dialog_busy());
        let running = app.deletion_view(ADAPTER, 1);
        dongle.unplug();
        let lost = Msg::Fleet(
            slot,
            Box::new(Event::Connection {
                session,
                port: "sim".into(),
                phase: Phase::Lost(Error::new("the connection to the adapter closed")),
            }),
        );
        if result_first {
            app.update(result);
            app.update(lost);
        } else {
            app.update(lost);
            assert!(app.state_of(ADAPTER).is_none());
            assert_eq!(app.deletion_view(ADAPTER, 1), running);
            app.update(result);
        }
        assert!(app.state_of(ADAPTER).is_none());
        assert!(!app.dialog_busy());
        assert!(app.toast_text().is_none());
        if keep_deleted {
            assert_eq!(
                app.dialog(),
                Some(&Dialog::ProfileDelete(ADAPTER.into(), 1))
            );
            let view = app.deletion_view(ADAPTER, 1);
            assert_eq!(view.stage, Stage::Deleted);
            assert_eq!(view.name, "Work");
            assert_eq!(view.steps, WORK_STEPS);
            assert_eq!(view.marks, [Mark::Done; 5]);
            assert!(!view.waiting);
            assert!(!view.uncertain);
            app.act(Action::Cancel);
        }
        assert!(app.dialog().is_none());
    }
}

#[test]
fn a_deletion_failure_stays_visible_with_either_result_and_loss_order() {
    use cordial::controller::Phase;

    for (command, expected, completed) in [
        (
            "set_device",
            "whether the device's profiles were updated",
            false,
        ),
        (
            "set_adapter",
            "whether its configuration interfaces were updated",
            true,
        ),
        ("delete_profile", "whether the profile was deleted", true),
    ] {
        for (result_first, keep_deleted) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let dongle = profiles_in_use();
            dongle
                .0
                .lock()
                .unwrap()
                .refuse
                .insert(command, common::refusal(p::ErrorCode::StorageFailed));
            let (mut app, rx) = confirm_delete(&dongle);
            if keep_deleted {
                app.keep_deleted();
            }
            confirm_deletion(&mut app);
            // Hold the deletion's result at the app boundary. Its confirmed progress has
            // arrived, but the reader's loss notification can arrive on either side of it.
            let (slot, session, ticket, error) = loop {
                let msg = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                if let Msg::Fleet(slot, event) = &msg
                    && let Event::Done {
                        session,
                        ticket,
                        result: Err(error),
                    } = event.as_ref()
                    && matches!(
                        error.command,
                        Some("device set profiles" | "adapter interface" | "profile delete")
                    )
                {
                    break (
                        *slot,
                        *session,
                        *ticket,
                        Error {
                            command: error.command,
                            ..Error::new(if completed {
                                "the profile deletion is incomplete: the connection to the adapter closed"
                            } else {
                                "the connection to the adapter closed"
                            })
                        },
                    );
                }
                app.update(msg);
            };
            assert!(app.dialog_busy());
            let result = Msg::Fleet(
                slot,
                Box::new(Event::Done {
                    session,
                    ticket,
                    result: Err(error),
                }),
            );
            let lost = Msg::Fleet(
                slot,
                Box::new(Event::Connection {
                    session,
                    port: "sim".into(),
                    phase: Phase::Lost(Error::new("the connection to the adapter closed")),
                }),
            );
            let messages = if result_first {
                [result, lost]
            } else {
                [lost, result]
            };
            for message in messages {
                app.update(message);
            }
            assert!(app.dialog().is_none());
            assert!(!app.profiles_busy(ADAPTER));
            let toast = app
                .toast_text()
                .expect("the deletion's failure stays visible");
            assert!(toast.contains(expected), "{toast}");
            assert_eq!(
                toast.contains("Earlier changes were saved."),
                completed,
                "{toast}"
            );
            assert!(
                toast.ends_with("Check the current settings before trying again."),
                "{toast}"
            );
            let sent = changes(&dongle, 0);
            app.tick();
            assert_eq!(changes(&dongle, 0), sent);
        }
    }
}

#[test]
fn a_deletion_cut_off_by_the_adapter_leaving_reports_the_failing_step() {
    for (command, expected, completed) in [
        (
            "set_device",
            "whether the device's profiles were updated",
            false,
        ),
        (
            "set_adapter",
            "whether its configuration interfaces were updated",
            true,
        ),
        ("delete_profile", "whether the profile was deleted", true),
    ] {
        let dongle = profiles_in_use();
        dongle
            .0
            .lock()
            .unwrap()
            .slow
            .insert(command, Duration::from_millis(500));
        let (mut app, rx) = confirm_delete(&dongle);
        app.keep_deleted();
        confirm_deletion(&mut app);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dongle.sent().contains(&command) {
            assert!(
                Instant::now() < deadline,
                "the deletion never reached {command}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        dongle.unplug();
        until(&mut app, &rx, |a| a.toast_text().is_some());
        let toast = app.toast_text().unwrap();
        assert!(toast.contains(expected), "{toast}");
        assert_eq!(
            toast.contains("Earlier changes were saved."),
            completed,
            "{toast}"
        );
        assert!(
            toast.ends_with("Check the current settings before trying again."),
            "{toast}"
        );
        assert!(app.dialog().is_none());
        let deleted = command == "delete_profile";
        assert_eq!(dongle.sent().contains(&"delete_profile"), deleted);
        assert_eq!(has_profile(&dongle, 1), !deleted);
        if !deleted {
            assert!(
                toast.contains("Profile deletion did not proceed."),
                "{toast}"
            );
            assert!(!toast.contains("was deleted"), "{toast}");
        }
        if completed {
            assert_eq!(layers_of(&dongle, 1), [2, 3]);
            assert!(layers_of(&dongle, 2).is_empty());
        }
    }
}

#[test]
fn discovery_leaves_a_profile_deletions_reconnect_to_its_controller() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dongle = profiles_in_use();
    let returning = Arc::new(AtomicBool::new(false));
    let available = returning.clone();
    let adapter = dongle.clone();
    let (attempted, attempts) = mpsc::channel();
    let (tx, rx) = mpsc::sync_channel(64);
    let fleet = Live::new(
        Arc::new(move |msg| {
            let _ = tx.send(msg);
        }),
        None,
    )
    .with_backend(
        Arc::new(move |_, handler| {
            if adapter.sessions() > 0 && !available.load(Ordering::Acquire) {
                let _ = attempted.send(());
                return Err(std::io::ErrorKind::NotFound.into());
            }
            adapter.connect(handler)
        }),
        || {
            Ok(vec![PortInfo {
                port: "sim".into(),
                serial: ADAPTER.into(),
            }])
        },
        Arc::new(|_, _, _, _| panic!("unexpected migration")),
    );
    let mut app = App::new(fleet);
    ready(&mut app, &rx);
    app.show(Page::Adapter(ADAPTER.into()));
    app.set_tab(Tab::Profiles);
    until(&mut app, &rx, |a| a.deletion_steps(ADAPTER, 1).len() == 5);
    app.act(Action::ProfileDelete(1));
    confirm_deletion(&mut app);
    attempts.recv_timeout(Duration::from_secs(2)).unwrap();
    app.update(Msg::Ports(Ok(Vec::new())));
    assert!(app.dialog_busy());
    assert!(matches!(app.dialog(), Some(Dialog::ProfileDelete(_, 1))));
    // Discovery must not open either the original path or a changed path with this identity.
    app.update(Msg::Ports(Ok(vec![PortInfo {
        port: "replacement".into(),
        serial: ADAPTER.into(),
    }])));
    returning.store(true, Ordering::Release);
    until(&mut app, &rx, |a| a.dialog().is_none());
    assert_eq!(dongle.sessions(), 2);
    assert!(!has_profile(&dongle, 1));
    assert_eq!(layers_of(&dongle, 1), [2, 3]);
    assert_eq!(
        dongle
            .sent()
            .iter()
            .filter(|c| **c == "delete_profile")
            .count(),
        1
    );
    app.quit();
    until(&mut app, &rx, App::done);
    assert_eq!(dongle.handles(), 0);
}
