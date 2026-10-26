use cordial::{
    sim::{Desktop, Dongle},
    ui::app::{Action, App, Dialog, Live, Msg, add::Phase},
};
use cordial_protocol as p;
use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

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

fn setup(world: &Desktop) -> (App<Live>, Receiver<Msg>) {
    let (tx, rx) = mpsc::sync_channel(64);
    let mut app = App::new(world.live(Arc::new(move |msg| {
        let _ = tx.send(msg);
    })));
    until(&mut app, &rx, |app| app.ready_adapters().len() == 2);
    (app, rx)
}

fn scanning(app: &App<Live>, adapter: &str) -> bool {
    app.add()
        .is_some_and(|add| !add.starting && add.scan.is_some())
        && app
            .state_of(adapter)
            .is_some_and(|state| state.scanning.is_some() && !state.candidates.is_empty())
}

fn pairing_commands(dongle: &Dongle) -> Vec<&'static str> {
    dongle
        .sent()
        .into_iter()
        .filter(|command| {
            matches!(
                *command,
                "start_scan"
                    | "stop_scan"
                    | "start_pairing"
                    | "cancel_pairing"
                    | "accept_prompt"
                    | "reject_prompt"
            )
        })
        .collect()
}

fn finish(app: &mut App<Live>, rx: &Receiver<Msg>, world: &Desktop) {
    app.quit();
    until(app, rx, App::done);
    assert!(
        world
            .adapters()
            .iter()
            .all(|adapter| adapter.handles() == 0)
    );
}

#[test]
fn contextual_add_scans_only_the_chosen_second_adapter() {
    let world = Desktop::new(2);
    let ports = world.ports();
    let first = &ports[0].serial;
    let second = &ports[1].serial;
    let (mut app, rx) = setup(&world);
    assert!(app.can_add_to(first));
    assert!(app.can_add_to(second));

    app.act(Action::AddDeviceTo(second.clone()));
    until(&mut app, &rx, |app| scanning(app, second));
    assert_eq!(app.dialog(), Some(&Dialog::AddDevice));
    assert_eq!(app.add().unwrap().adapter.as_deref(), Some(second.as_str()));
    assert!(app.state_of(first).unwrap().scanning.is_none());
    assert!(app.state_of(first).unwrap().candidates.is_empty());
    assert!(pairing_commands(&world.adapters()[0]).is_empty());
    assert_eq!(pairing_commands(&world.adapters()[1]), ["start_scan"]);

    app.act(Action::Cancel);
    until(&mut app, &rx, |app| {
        app.state_of(second).unwrap().scanning.is_none()
    });
    finish(&mut app, &rx, &world);
    assert!(pairing_commands(&world.adapters()[0]).is_empty());
    assert_eq!(
        pairing_commands(&world.adapters()[1]),
        ["start_scan", "stop_scan"]
    );
}

#[test]
fn unknown_and_disconnected_targets_do_not_fall_back_to_a_ready_adapter() {
    let world = Desktop::new(2);
    let ports = world.ports();
    let first = &ports[0].serial;
    let second = &ports[1].serial;
    let (mut app, rx) = setup(&world);

    assert!(!app.can_add_to("missing"));
    app.act(Action::AddDeviceTo("missing".into()));
    assert!(app.dialog().is_none());
    assert!(app.add().is_none());

    app.act(Action::DisconnectAdapter(second.clone()));
    until(&mut app, &rx, |app| {
        app.adapter(second)
            .is_some_and(|adapter| !adapter.connected())
    });
    assert!(app.can_add_to(first));
    assert!(!app.can_add_to(second));
    app.act(Action::AddDeviceTo(second.clone()));
    assert!(app.dialog().is_none());
    assert!(app.add().is_none());

    finish(&mut app, &rx, &world);
    for adapter in world.adapters() {
        assert!(pairing_commands(adapter).is_empty());
    }
}

#[test]
fn legacy_add_prefers_the_first_ready_adapter_with_room() {
    for (full, chosen) in [(0, 0), (1, 1), (2, 0)] {
        let world = Desktop::new(2);
        for adapter in world.adapters().iter().take(full) {
            adapter.0.lock().unwrap().status.errors.push(p::Error {
                code: p::ErrorCode::StorageFull as i32,
                ..Default::default()
            });
        }
        let ports = world.ports();
        let target = &ports[chosen].serial;
        let (mut app, rx) = setup(&world);

        app.act(Action::AddDevice);
        until(&mut app, &rx, |app| scanning(app, target));
        assert_eq!(app.add().unwrap().adapter.as_deref(), Some(target.as_str()));
        assert_eq!(pairing_commands(&world.adapters()[chosen]), ["start_scan"]);
        assert!(pairing_commands(&world.adapters()[1 - chosen]).is_empty());

        app.act(Action::Cancel);
        until(&mut app, &rx, |app| {
            app.state_of(target).unwrap().scanning.is_none()
        });
        finish(&mut app, &rx, &world);
    }
}

#[test]
fn repeated_add_requests_preserve_the_original_scan() {
    let world = Desktop::new(2);
    let ports = world.ports();
    let first = &ports[0].serial;
    let second = &ports[1].serial;
    let (mut app, rx) = setup(&world);
    app.act(Action::AddDeviceTo(second.clone()));
    until(&mut app, &rx, |app| scanning(app, second));
    app.act(Action::ShowUnnamed(true));

    for action in [
        Action::AddDeviceTo(second.clone()),
        Action::AddDeviceTo(first.clone()),
        Action::AddDevice,
    ] {
        app.act(action);
        let add = app.add().unwrap();
        assert_eq!(app.dialog(), Some(&Dialog::AddDevice));
        assert_eq!(add.adapter.as_deref(), Some(second.as_str()));
        assert!(add.show_unnamed);
        assert!(scanning(&app, second));
    }

    app.act(Action::Cancel);
    until(&mut app, &rx, |app| {
        app.state_of(second).unwrap().scanning.is_none()
    });
    finish(&mut app, &rx, &world);
    assert!(pairing_commands(&world.adapters()[0]).is_empty());
    assert_eq!(
        pairing_commands(&world.adapters()[1]),
        ["start_scan", "stop_scan"]
    );
}

#[test]
fn repeated_add_requests_preserve_the_prompt_and_running_pairing() {
    let world = Desktop::new(2);
    let ports = world.ports();
    let first = &ports[0].serial;
    let second = &ports[1].serial;
    let (mut app, rx) = setup(&world);
    app.act(Action::AddDeviceTo(second.clone()));
    until(&mut app, &rx, |app| scanning(app, second));
    app.act(Action::Pair(1));
    until(&mut app, &rx, App::pairing_code_input);
    let prompt = app.pairing_prompt();
    app.set_form("123456");

    for action in [
        Action::AddDeviceTo(second.clone()),
        Action::AddDeviceTo(first.clone()),
        Action::AddDevice,
    ] {
        app.act(action);
        assert_eq!(app.dialog(), Some(&Dialog::AddDevice));
        assert_eq!(app.pairing_prompt(), prompt);
        assert_eq!(app.form(), "123456");
        let pairing = app.add().unwrap().pairing.as_ref().unwrap();
        assert_eq!(&pairing.adapter, second);
        assert_eq!(pairing.candidate, 1);
        assert_eq!(pairing.phase, Phase::Pairing);
    }

    app.act(Action::Accept);
    until(&mut app, &rx, |app| {
        app.add()
            .and_then(|add| add.pairing.as_ref())
            .is_some_and(|pairing| pairing.phase == Phase::Connected)
    });
    assert!(
        world.adapters()[1]
            .0
            .lock()
            .unwrap()
            .devices
            .iter()
            .any(|d| d.id == 11)
    );
    app.act(Action::Done);
    finish(&mut app, &rx, &world);
    assert!(pairing_commands(&world.adapters()[0]).is_empty());
    let mut commands = pairing_commands(&world.adapters()[1]);
    commands.sort_unstable();
    assert_eq!(
        commands,
        ["accept_prompt", "start_pairing", "start_scan", "stop_scan"]
    );
}

#[test]
fn add_requests_preserve_another_open_dialog() {
    let world = Desktop::new(2);
    let target = world.ports()[1].serial.clone();
    let (mut app, rx) = setup(&world);
    app.act(Action::Help);
    for action in [Action::AddDeviceTo(target), Action::AddDevice] {
        app.act(action);
        assert_eq!(app.dialog(), Some(&Dialog::Help));
        assert!(app.add().is_none());
    }
    finish(&mut app, &rx, &world);
    for adapter in world.adapters() {
        assert!(pairing_commands(adapter).is_empty());
    }
}
