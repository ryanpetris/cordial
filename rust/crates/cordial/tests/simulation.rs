use cordial::{
    controller::{Command, Event, Phase, SettingInput, Target},
    settings::{self, Field, Leaf, Value},
    sim::{self, Desktop},
    ui::app::{Fleet, Live, Msg, Slot},
};
use cordial_protocol::{self as p, DeviceState};
use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

fn messages() -> (cordial::ui::app::Sink, Receiver<Msg>) {
    let (tx, rx) = mpsc::channel();
    (
        Arc::new(move |msg| {
            let _ = tx.send(msg);
        }),
        rx,
    )
}

fn until(rx: &Receiver<Msg>, wanted: impl Fn(&Msg) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let msg = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("expected simulator event");
        if wanted(&msg) {
            return;
        }
    }
}

fn ready(rx: &Receiver<Msg>, slot: Slot) {
    until(
        rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(**e, Event::Connection { phase: Phase::Ready, .. })),
    );
}

fn run(live: &Live, rx: &Receiver<Msg>, slot: Slot, command: Command) {
    let ticket = live.run(slot, command);
    until(rx, |m| match m {
        Msg::Fleet(s, e) if *s == slot => match &**e {
            Event::Done {
                ticket: t, result, ..
            } if *t == ticket => {
                assert!(result.is_ok(), "simulated command failed: {result:?}");
                true
            }
            _ => false,
        },
        _ => false,
    });
}

#[test]
fn desktop_factory_routes_independent_adapters_and_closes_their_sessions() {
    let world = Desktop::new(2);
    let ports = world.ports();
    assert_ne!(ports[0].port, ports[1].port);
    assert_ne!(ports[0].serial, ports[1].serial);
    let (sink, rx) = messages();
    let live = world.live(sink);
    let first = live.open(&ports[0].port);
    ready(&rx, first);
    let second = live.open(&ports[1].port);
    ready(&rx, second);
    assert_eq!(live.state(first).unwrap().status.id, ports[0].serial);
    assert_eq!(live.state(second).unwrap().status.id, ports[1].serial);
    assert_ne!(
        live.state(first).unwrap().devices[0].name,
        live.state(second).unwrap().devices[0].name
    );
    run(&live, &rx, second, Command::Name(Some("Studio".into())));
    assert_eq!(
        world.adapters()[0].0.lock().unwrap().status.name,
        "Example Adapter 1"
    );
    assert_eq!(world.adapters()[1].0.lock().unwrap().status.name, "Studio");
    assert!(world.adapters()[0].set_battery(1, Some(9), Some(false)));
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == first && matches!(&**e, Event::Notice { notice: cordial::controller::Notice::Event { event: p::Event { kind: Some(p::event::Kind::Device(d)) }, .. }, .. } if d.id == 1 && d.information.as_ref().and_then(|i| i.battery.as_ref()).and_then(|b| b.level) == Some(9))),
    );
    assert_eq!(
        live.state(second).unwrap().devices[0]
            .information
            .as_ref()
            .unwrap()
            .battery
            .as_ref()
            .unwrap()
            .level,
        Some(72)
    );
    assert!(world.set_present(0, false));
    assert_eq!(world.ports().len(), 1);
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == first && matches!(**e, Event::Connection { phase: Phase::Lost(_), .. })),
    );
    live.close(first);
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == first && matches!(**e, Event::Closed)),
    );
    assert_eq!(world.adapters()[0].handles(), 0);
    assert!(world.set_present(0, true));
    let reopened = live.open_adapter(&ports[0].port, &ports[0].serial);
    ready(&rx, reopened);
    assert_eq!(world.adapters()[0].sessions(), 2);
    assert_eq!(
        live.state(reopened).unwrap().devices[0]
            .information
            .as_ref()
            .unwrap()
            .battery
            .as_ref()
            .unwrap()
            .level,
        Some(9)
    );
    for slot in [reopened, second] {
        live.close(slot);
        until(
            &rx,
            |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(**e, Event::Closed)),
        );
    }
    assert!(world.adapters().iter().all(|d| d.handles() == 0));
    // The frontend's convenience factory also uses simulated connections for every port.
    let (sink, rx) = messages();
    let live = sim::desktop(sink, 2);
    live.list_ports();
    until(
        &rx,
        |m| matches!(m, Msg::Ports(Ok(ports)) if ports.len() == 2),
    );
    let slot = live.open(&ports[1].port);
    ready(&rx, slot);
    live.close(slot);
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(**e, Event::Closed)),
    );
}

#[test]
fn seeded_settings_save_bool_integer_and_choice_values_over_the_protocol() {
    let world = Desktop::new(1);
    let (sink, rx) = messages();
    let live = world.live(sink);
    let slot = live.open(&world.ports()[0].port);
    ready(&rx, slot);
    run(&live, &rx, slot, Command::Settings(Target::Id(2)));
    let fields = [
        (Leaf::BacklightEnabled, Value::Bool(false)),
        (Leaf::Brightness, Value::Integer(5)),
        (
            Leaf::FunctionRow,
            Value::Choice(p::FunctionRowMode::FunctionKeys as i32),
        ),
    ];
    for (leaf, value) in fields {
        run(
            &live,
            &rx,
            slot,
            Command::SettingSet(
                Target::Id(2),
                Field::hidpp(leaf, None),
                SettingInput::Value(value),
            ),
        );
    }
    // A fresh listing verifies persistence independently of the controller's optimistic save.
    run(&live, &rx, slot, Command::Settings(Target::Id(2)));
    let state = live.state(slot).unwrap();
    for (leaf, value) in fields {
        let setting = state
            .settings_of(2)
            .into_iter()
            .find(|s| s.field.leaf == leaf)
            .unwrap();
        assert_eq!(setting.value, Some(value));
        assert_eq!(setting.saved, Some(value));
        assert_eq!(setting.status, Some(Ok(p::SettingState::Applied)));
    }
    assert_eq!(state.device(3).unwrap().state(), DeviceState::Disconnected);
    assert!(world.adapters()[0].0.lock().unwrap().profiles.len() > 10);
    assert!(
        settings::settings(
            world.adapters()[0]
                .0
                .lock()
                .unwrap()
                .settings
                .get(&1)
                .unwrap()
        )
        .iter()
        .any(|s| matches!(
            s.limits,
            settings::Limits::Integer(Some(p::integer_setting::Limits::Choices(_)))
        ))
    );
    live.close(slot);
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(**e, Event::Closed)),
    );
}

#[test]
fn display_pairing_waits_for_the_device_and_migration_reopens_ready() {
    let world = Desktop::new(1);
    let port = world.ports().remove(0);
    let (sink, rx) = messages();
    let live = world.live(sink);
    let slot = live.open(&port.port);
    ready(&rx, slot);
    run(
        &live,
        &rx,
        slot,
        Command::Scan {
            transports: Vec::new(),
            seconds: 30,
        },
    );
    let pairing = live.run(slot, Command::Pair(Target::Id(3)));
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(&**e, Event::Notice { notice: cordial::controller::Notice::Event { event: p::Event { kind: Some(p::event::Kind::Pairing(pairing)) }, .. }, .. } if matches!(&pairing.step, Some(p::pairing::Step::ShowCode(code)) if code.value == "123456"))),
    );
    assert!(world.adapters()[0].complete_pairing());
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(&**e, Event::Notice { notice: cordial::controller::Notice::Event { event: p::Event { kind: Some(p::event::Kind::Pairing(pairing)) }, .. }, .. } if matches!(pairing.step, Some(p::pairing::Step::Done(_))))),
    );
    assert!(live.state(slot).unwrap().device(13).is_some());
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(&**e, Event::Done { ticket, result, .. } if *ticket == pairing && result.is_ok())),
    );
    assert!(!world.adapters()[0].complete_pairing());
    live.close(slot);
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(**e, Event::Closed)),
    );
    world.adapters()[0].require_migration();
    let slot = live.open(&port.port);
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(**e, Event::Connection { phase: Phase::Migrate, .. })),
    );
    assert!(world.adapters()[0].sent().ends_with(&["get_status"]));
    live.close(slot);
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(**e, Event::Closed)),
    );
    live.migrate(&port.serial, &port.port);
    until(&rx, |m| {
        matches!(
            m,
            Msg::Migrated(Ok(cordial_client::migration::Outcome::Success))
        )
    });
    assert_eq!(world.adapters()[0].handles(), 0);
    let slot = live.open_adapter(&port.port, &port.serial);
    ready(&rx, slot);
    assert!(live.state(slot).unwrap().device(13).is_some());
    live.close(slot);
    until(
        &rx,
        |m| matches!(m, Msg::Fleet(s, e) if *s == slot && matches!(**e, Event::Closed)),
    );
}
