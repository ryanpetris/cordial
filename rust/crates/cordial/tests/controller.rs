mod common;

use common::{Dongle, Plan, device, dpi, read_error, refusal};
use cordial::{
    controller::{
        AdapterUpdate, Command, Controller, Event, Outcome, Phase, Pick, RunOptions, SettingInput,
        State, Target, Toggle, Wait,
    },
    error::Error,
    profiles::{self, InterfaceUpdate},
    settings::{Field, Leaf, Value},
};
use cordial_protocol::{
    self as p, ConfigurationInterface, DeviceState, ErrorCode, SettingState, Transport,
};
use std::{
    sync::mpsc::{self, Receiver},
    time::Duration,
};

fn setup(dongle: &Dongle) -> (Controller, Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let controller = Controller::with_connector(
        move |e| {
            let _ = tx.send(e);
        },
        dongle.connector(),
    );
    (controller, rx)
}

fn until(events: &Receiver<Event>, mut wanted: impl FnMut(&Event) -> bool) -> Event {
    loop {
        let e = events
            .recv_timeout(Duration::from_secs(5))
            .expect("expected event");
        if wanted(&e) {
            return e;
        }
    }
}

/// Waits until the view satisfies `wanted`, checking before each event.
fn eventually(c: &Controller, events: &Receiver<Event>, wanted: impl Fn(&State) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !c.state().is_some_and(|st| wanted(&st)) {
        assert!(std::time::Instant::now() < deadline, "view never matched");
        let _ = events.recv_timeout(Duration::from_millis(20));
    }
}

fn ready(dongle: &Dongle) -> (Controller, Receiver<Event>) {
    let (c, rx) = setup(dongle);
    c.open("sim".into());
    until(&rx, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    (c, rx)
}

fn id(n: u32) -> Target {
    Target::Id(n)
}

fn name(n: &str) -> Target {
    Target::Name(n.into())
}

fn run(c: &Controller, rx: &Receiver<Event>, command: Command) -> Result<Outcome, Error> {
    run_with(c, rx, command, RunOptions::default())
}

fn run_with(
    c: &Controller,
    rx: &Receiver<Event>,
    command: Command,
    options: RunOptions,
) -> Result<Outcome, Error> {
    let ticket = c.run(command, options);
    match until(
        rx,
        |e| matches!(e, Event::Done { ticket: t, .. } if *t == ticket),
    ) {
        Event::Done { result, .. } => result,
        _ => unreachable!(),
    }
}

#[test]
fn opening_loads_status_and_devices_then_events_keep_the_view() {
    let dongle = Dongle::with(|sim| {
        sim.devices[0].state = DeviceState::Connected as i32;
        sim.devices[0].errors = vec![p::Error {
            code: ErrorCode::ProfileLoadFailed as i32,
            ..Default::default()
        }];
        sim.settings.insert(1, vec![dpi(0, 800, None)]);
    });
    let (c, rx) = ready(&dongle);
    let st = c.state().unwrap();
    assert_eq!(st.status.name, "Desk");
    assert_eq!(st.devices.len(), 2);
    assert_eq!(
        cordial::model::profile_error(st.device(1).unwrap()).map(p::Error::code),
        Some(ErrorCode::ProfileLoadFailed)
    );
    assert!(st.ready());
    assert_eq!(dongle.sent(), ["get_status", "list_devices"]);
    run(&c, &rx, Command::Settings(id(1))).unwrap();
    // A device event replaces the record; a removal drops it with its lists.
    let mut connected = device(1, "Renamed Mouse", Transport::Ble);
    connected.state = DeviceState::Connected as i32;
    dongle.event(p::event::Kind::Device(connected));
    eventually(&c, &rx, |st| st.device(1).unwrap().name == "Renamed Mouse");
    dongle.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 1 }));
    eventually(&c, &rx, |st| {
        st.device(1).is_none() && st.settings_of(1).is_empty() && !st.settings.contains_key(&1)
    });
}

#[test]
fn opening_lists_every_device_in_one_response() {
    let dongle = Dongle::with(|sim| {
        for n in 3..=6 {
            sim.devices.push(device(n, "Pad", Transport::Ble));
        }
        sim.unreadable.push(4);
        sim.devices.retain(|d| d.id != 4);
    });
    let (c, rx) = ready(&dongle);
    let st = c.state().unwrap();
    let ids: Vec<u32> = st.devices.iter().map(|d| d.id).collect();
    assert_eq!(ids, [1, 2, 3, 5, 6]);
    assert_eq!(st.unreadable.get(&4), read_error(4).error.as_ref());
    let lists = || {
        dongle
            .sent()
            .iter()
            .filter(|s| **s == "list_devices")
            .count()
    };
    assert_eq!(lists(), 1);
    // A refresh replaces the listing.
    dongle.0.lock().unwrap().devices.retain(|d| d.id != 5);
    run(&c, &rx, Command::Devices).unwrap();
    let ids: Vec<u32> = c.state().unwrap().devices.iter().map(|d| d.id).collect();
    assert_eq!(ids, [1, 2, 3, 6]);
    assert_eq!(lists(), 2);
    // A listing that ends in an error fails and changes nothing.
    {
        let mut sim = dongle.0.lock().unwrap();
        sim.devices.retain(|d| d.id != 6);
        sim.fail_late
            .insert("list_devices", ErrorCode::StorageFailed);
    }
    let error = run(&c, &rx, Command::Devices).unwrap_err();
    assert_eq!(error.code_of(), Some(ErrorCode::StorageFailed));
    let ids: Vec<u32> = c.state().unwrap().devices.iter().map(|d| d.id).collect();
    assert_eq!(ids, [1, 2, 3, 6]);
}

#[test]
fn an_unready_adapter_waits_then_loads_when_it_reports_ready() {
    let dongle = Dongle::with(|sim| {
        sim.status = common::status(false);
    });
    let (c, rx) = setup(&dongle);
    c.open("sim".into());
    until(&rx, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Waiting,
                ..
            }
        )
    });
    // Direct commands work meanwhile; others are refused locally.
    assert!(matches!(
        run(&c, &rx, Command::Status),
        Ok(Outcome::Status(_))
    ));
    let error = run(&c, &rx, Command::Devices).unwrap_err();
    assert!(error.message.contains("still starting"), "{error:?}");
    assert!(!dongle.sent().contains(&"list_devices"));
    dongle.0.lock().unwrap().status = common::status(true);
    dongle.event(p::event::Kind::Adapter(common::status(true)));
    until(&rx, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    assert_eq!(c.state().unwrap().devices.len(), 2);
}

#[test]
fn enabling_past_the_transport_limit_is_refused_before_sending() {
    let dongle = Dongle::with(|sim| {
        // Classic allows one enabled device; 3 already holds it.
        sim.devices
            .push(device(3, "Classic Pad", Transport::Classic));
    });
    let (c, rx) = ready(&dongle);
    let error = run(&c, &rx, Command::Set(id(2), Toggle::Enabled, true)).unwrap_err();
    assert_eq!(error.code_of(), Some(ErrorCode::NoCapacity));
    assert!(!dongle.sent().contains(&"set_device"));
    // Disabling the other one makes room.
    run(&c, &rx, Command::Set(id(3), Toggle::Enabled, false)).unwrap();
    let outcome = run(&c, &rx, Command::Set(id(2), Toggle::Enabled, true)).unwrap();
    assert!(matches!(outcome, Outcome::Device { device, .. } if device.enabled));
    // The integration preference is a partial update naming only HID++.
    run(&c, &rx, Command::Set(id(1), Toggle::Hidpp, false)).unwrap();
    let sim = dongle.0.lock().unwrap();
    let Some(p::request::Command::SetDevice(last)) = sim.log.last() else {
        panic!("{:?}", sim.log.last());
    };
    assert_eq!(last.enabled, None);
    assert_eq!(last.profiles, None);
    assert_eq!(
        last.integrations,
        [p::IntegrationUpdate {
            kind: p::IntegrationKind::Hidpp as i32,
            enabled: Some(false)
        }]
    );
}

#[test]
fn connecting_a_disabled_or_blocked_device_is_refused_locally() {
    let dongle = Dongle::default();
    let (c, rx) = ready(&dongle);
    let error = run(&c, &rx, Command::Connect(name("Old Keyboard"))).unwrap_err();
    assert_eq!(error.code_of(), Some(ErrorCode::Disabled));
    let outcome = run(&c, &rx, Command::Connect(id(1))).unwrap();
    assert!(matches!(outcome, Outcome::Device { .. }));
    let st = c.state().unwrap();
    assert!(st.pending.is_empty());
    assert!(dongle.sent().contains(&"connect_device"));
}

#[test]
fn pairing_follows_its_prompt_and_reports_the_saved_device() {
    let dongle = Dongle::with(|sim| sim.plan = Plan::EnterCode("123456".into()));
    let (c, rx) = ready(&dongle);
    let scan = run(
        &c,
        &rx,
        Command::Scan {
            transports: vec![],
            seconds: 0,
        },
    )
    .unwrap();
    assert!(matches!(&scan, Outcome::ScanStarted(t) if t.len() == 2));
    eventually(&c, &rx, |st| st.candidate(1).is_some());
    // A candidate ID is never a device ID: device 1 is a different thing.
    let ticket = c.run(Command::Pair(name("New Keyboard")), RunOptions::default());
    eventually(&c, &rx, |st| {
        st.pairing
            .as_ref()
            .is_some_and(|p| matches!(p.step, Some(p::pairing::Step::EnterCode(_))))
    });
    // A second pairing is refused while this one runs.
    let busy = run(&c, &rx, Command::Pair(id(1))).unwrap_err();
    assert_eq!(busy.code_of(), Some(ErrorCode::Busy));
    let short = run(&c, &rx, Command::Accept(Some("12".into()))).unwrap_err();
    assert_eq!(short.message, "passkey must contain exactly six digits");
    // The pairing and the answer finish in either order; collect both results.
    let accept = c.run(
        Command::Accept(Some("123456".into())),
        RunOptions::default(),
    );
    let (mut paired, mut accepted) = (None, None);
    while paired.is_none() || accepted.is_none() {
        if let Event::Done {
            ticket: t, result, ..
        } = until(&rx, |e| matches!(e, Event::Done { .. }))
        {
            if t == ticket {
                paired = Some(result);
            } else if t == accept {
                accepted = Some(result);
            }
        }
    }
    accepted.unwrap().unwrap();
    match paired.unwrap().unwrap() {
        Outcome::Paired { subject, device } => {
            assert_eq!(subject.id, 9);
            assert_eq!(device.unwrap().name, "New Keyboard");
        }
        other => panic!("{other:?}"),
    }
    assert!(c.state().unwrap().device(9).is_some());
    // No prompt is open now.
    let none = run(&c, &rx, Command::Reject).unwrap_err();
    assert_eq!(none.code_of(), Some(ErrorCode::NoPrompt));
}

#[test]
fn a_failed_pairing_ends_the_pair_command_with_its_code() {
    let dongle = Dongle::with(|sim| sim.plan = Plan::Confirm("654321".into()));
    let (c, rx) = ready(&dongle);
    run(
        &c,
        &rx,
        Command::Scan {
            transports: vec![Transport::Ble],
            seconds: 5,
        },
    )
    .unwrap();
    eventually(&c, &rx, |st| st.candidate(1).is_some());
    let ticket = c.run(Command::Pair(id(1)), RunOptions::default());
    eventually(&c, &rx, |st| {
        st.pairing
            .as_ref()
            .is_some_and(|p| matches!(p.step, Some(p::pairing::Step::ConfirmCode(_))))
    });
    // The pairing and the rejection finish in either order; collect both results.
    let reject = c.run(Command::Reject, RunOptions::default());
    let (mut paired, mut rejected) = (None, None);
    while paired.is_none() || rejected.is_none() {
        if let Event::Done {
            ticket: t, result, ..
        } = until(&rx, |e| matches!(e, Event::Done { .. }))
        {
            if t == ticket {
                paired = Some(result);
            } else if t == reject {
                rejected = Some(result);
            }
        }
    }
    rejected.unwrap().unwrap();
    assert_eq!(
        paired.unwrap().unwrap_err().code_of(),
        Some(ErrorCode::Rejected)
    );
}

#[test]
fn a_one_shot_scan_waits_for_its_end() {
    let dongle = Dongle::default();
    let (c, rx) = ready(&dongle);
    let outcome = run_with(
        &c,
        &rx,
        Command::Scan {
            transports: vec![],
            seconds: 3,
        },
        RunOptions {
            one_shot: true,
            wait: Wait::timeout(Duration::from_secs(5)),
        },
    )
    .unwrap();
    assert!(matches!(
        outcome,
        Outcome::ScanFinished {
            count: 1,
            error: None
        }
    ));
}

#[test]
fn saving_settings_sends_one_write_and_events_report_the_apply() {
    let dongle = Dongle::with(|sim| {
        // Two sensors, one per Message of the listing's frame.
        sim.settings
            .insert(1, vec![dpi(0, 800, None), dpi(1, 1600, Some(1600))]);
        sim.batch_size = 1;
    });
    let (c, rx) = ready(&dongle);
    run(&c, &rx, Command::Settings(id(1))).unwrap();
    assert_eq!(
        dongle
            .sent()
            .iter()
            .filter(|s| **s == "list_settings")
            .count(),
        1
    );
    let first = Field::hidpp(Leaf::Dpi, Some(0));
    assert_eq!(c.state().unwrap().settings_of(1).len(), 2);
    let error = run(
        &c,
        &rx,
        Command::SettingSet(id(1), first, SettingInput::Text("1234".into())),
    )
    .unwrap_err();
    assert_eq!(
        error.message,
        "pointer_sensor.0.dpi takes an integer from 400 through 4000 in steps of 50"
    );
    assert!(!dongle.sent().contains(&"set_settings"));
    let outcome = run(
        &c,
        &rx,
        Command::SettingsSave {
            device: 1,
            set: vec![(first, Value::Integer(1600))],
            forget: vec![],
        },
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Saved { set, .. } if set == [first]));
    let applied = Some(Ok(SettingState::Applied));
    eventually(&c, &rx, |st| st.settings_of(1)[0].status == applied);
    let second = Field::hidpp(Leaf::Dpi, Some(1));
    let forgot = run(&c, &rx, Command::SettingForget(id(1), second)).unwrap();
    assert!(matches!(forgot, Outcome::Saved { forget, .. } if forget == [second]));
    // A forget is a change in the same SetSettings command, for the second sensor only.
    {
        let sim = dongle.0.lock().unwrap();
        let Some(p::request::Command::SetSettings(last)) = sim.log.last() else {
            panic!("{:?}", sim.log.last());
        };
        let [
            p::SettingsUpdate {
                group: Some(p::settings_update::Group::PointerSensor(sensor)),
                ..
            },
        ] = &last.changes[..]
        else {
            panic!("{last:?}");
        };
        assert_eq!(sensor.index, 1);
        assert!(matches!(
            sensor.dpi.as_ref().and_then(|d| d.change),
            Some(p::integer_setting_update::Change::Forget(_))
        ));
    }
    eventually(&c, &rx, |st| {
        let all = st.settings_of(1);
        all[1].saved.is_none() && all[0].saved == Some(Value::Integer(1600))
    });
    let mode = Field::hidpp(Leaf::WheelMode, None);
    let unknown = run(&c, &rx, Command::SettingGet(id(1), mode)).unwrap_err();
    assert_eq!(unknown.code_of(), Some(ErrorCode::NotFound));
}

#[test]
fn a_dongle_refusal_is_reported_without_a_refresh() {
    let dongle = Dongle::with(|sim| {
        sim.refuse
            .insert("disconnect_device", refusal(ErrorCode::Busy));
    });
    let (c, rx) = ready(&dongle);
    let before = dongle.sent().len();
    let error = run(&c, &rx, Command::Disconnect(id(1))).unwrap_err();
    assert_eq!(error.code_of(), Some(ErrorCode::Busy));
    assert_eq!(error.command, Some("device disconnect"));
    assert_eq!(dongle.sent().len(), before + 1);
}

#[test]
fn losing_the_stream_reports_lost_and_closing_reports_closed() {
    let dongle = Dongle::default();
    let (c, rx) = ready(&dongle);
    dongle.unplug();
    until(&rx, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Lost(_),
                ..
            }
        )
    });
    assert!(!c.state().unwrap().available);
    let error = run(&c, &rx, Command::Devices).unwrap_err();
    assert!(error.message.contains("unavailable"), "{error:?}");
    c.close();
    until(&rx, |e| matches!(e, Event::Closed));
    assert!(c.state().is_none());
}

#[test]
fn device_commands_take_only_saved_devices() {
    let dongle = Dongle::default();
    let (c, rx) = ready(&dongle);
    run(
        &c,
        &rx,
        Command::Scan {
            transports: vec![],
            seconds: 0,
        },
    )
    .unwrap();
    eventually(&c, &rx, |st| st.candidate(1).is_some());
    // A candidate's name isn't a device name.
    let error = run(&c, &rx, Command::Unpair(name("New Keyboard"))).unwrap_err();
    assert!(error.message.contains("not found"), "{error:?}");
    assert!(!dongle.sent().contains(&"unpair_device"));
    let error = run(&c, &rx, Command::Get(id(7))).unwrap_err();
    assert_eq!(error.message, "device 7 not found; use device list");
    c.hide_candidate(1);
    assert!(c.state().unwrap().candidate(1).is_none());
    run(&c, &rx, Command::Unpair(id(1))).unwrap();
    eventually(&c, &rx, |st| st.device(1).is_none());
}

fn with_profiles() -> Dongle {
    Dongle::with(|sim| {
        common::enable_profiles(&mut sim.status);
        sim.batch_size = 2;
        for name in ["Work", "Games", "Mouse Fix", "Work"] {
            sim.add_profile(name, Vec::new());
        }
    })
}

#[test]
fn profile_commands_need_profile_support() {
    let dongle = Dongle::default();
    let (c, rx) = ready(&dongle);
    for command in [
        Command::Profiles,
        Command::ProfileCreate("Work".into()),
        Command::Layers(id(1), Vec::new()),
        Command::Interface {
            interface: ConfigurationInterface::Via,
            enabled: Some(false),
            profile: None,
        },
    ] {
        let error = run(&c, &rx, command).unwrap_err();
        assert_eq!(error.message, "this adapter doesn't support profiles");
    }
    assert!(!dongle.sent().iter().any(|s| s.contains("profile")));
}

#[test]
fn profiles_are_listed_in_one_response_and_found_by_unique_name() {
    let dongle = with_profiles();
    let (c, rx) = ready(&dongle);
    let Outcome::Profiles(list) = run(&c, &rx, Command::Profiles).unwrap() else {
        panic!()
    };
    assert_eq!(list.entries.len(), 4);
    let lists = dongle
        .sent()
        .iter()
        .filter(|s| **s == "list_profiles")
        .count();
    assert_eq!(lists, 1);
    let Outcome::Profile(found) = run(&c, &rx, Command::ProfileShow(name("Mouse Fix"))).unwrap()
    else {
        panic!()
    };
    assert_eq!(found.id, 3);
    let error = run(&c, &rx, Command::ProfileShow(name("Work"))).unwrap_err();
    assert!(error.message.contains("ambiguous"), "{error:?}");
    let error = run(&c, &rx, Command::ProfileShow(name("Nothing"))).unwrap_err();
    assert_eq!(
        error.message,
        "profile \"Nothing\" not found; use profile list"
    );
    let error = run(&c, &rx, Command::ProfileShow(id(40))).unwrap_err();
    assert!(error.message.contains("not found"), "{error:?}");
    let Outcome::Profile(created) = run(&c, &rx, Command::ProfileCreate("Keys".into())).unwrap()
    else {
        panic!()
    };
    assert_eq!(created.id, 5);
    eventually(&c, &rx, |st| st.profile(5).is_some());
    let Outcome::Profile(copy) = run(
        &c,
        &rx,
        Command::ProfileCopy(name("Keys"), "Keys Copy".into()),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(copy.id, 6);
    assert!(run(&c, &rx, Command::ProfileCreate(String::new())).is_err());
}

#[test]
fn layers_are_saved_in_order_and_profiles_in_use_stay() {
    let dongle = with_profiles();
    let (c, rx) = ready(&dongle);
    let outcome = run(
        &c,
        &rx,
        Command::Layers(id(1), vec![name("Mouse Fix"), id(2)]),
    )
    .unwrap();
    let Outcome::Device { device, .. } = outcome else {
        panic!()
    };
    assert_eq!(profiles::layers(&device), [3, 2]);
    // More layers than the adapter allows are refused before sending.
    let before = dongle.sent().len();
    let error = run(
        &c,
        &rx,
        Command::Layers(id(1), vec![id(1), id(2), id(3), id(2)]),
    )
    .unwrap_err();
    assert_eq!(error.message, "a device can use at most 3 profiles");
    assert_eq!(dongle.sent().len(), before);
    // A profile in a device's layers isn't deleted.
    eventually(&c, &rx, |st| {
        profiles::layers(st.device(1).unwrap()) == [3, 2]
    });
    let error = run(&c, &rx, Command::ProfileDelete(id(2))).unwrap_err();
    assert_eq!(
        error.message,
        "Office Mouse is using this profile. Remove it from Office Mouse's profiles first"
    );
    run(&c, &rx, Command::Layers(name("Office Mouse"), Vec::new())).unwrap();
    eventually(&c, &rx, |st| {
        profiles::layers(st.device(1).unwrap()).is_empty()
    });
    let Outcome::ProfileDeleted(gone) = run(&c, &rx, Command::ProfileDelete(id(2))).unwrap() else {
        panic!()
    };
    assert_eq!(gone.id, 2);
    eventually(&c, &rx, |st| st.profile(2).is_none());
    // The adapter's own refusal is reported as is.
    dongle
        .0
        .lock()
        .unwrap()
        .refuse
        .insert("delete_profile", refusal(ErrorCode::InUse));
    let error = run(&c, &rx, Command::ProfileDelete(id(3))).unwrap_err();
    assert_eq!(error.code_of(), Some(ErrorCode::InUse));
}

#[test]
fn interfaces_need_a_profile_and_respect_conflicts() {
    let dongle = with_profiles();
    let (c, rx) = ready(&dongle);
    let via = |enabled, profile| Command::Interface {
        interface: ConfigurationInterface::Via,
        enabled,
        profile,
    };
    let error = run(&c, &rx, via(Some(true), None)).unwrap_err();
    assert_eq!(
        error.message,
        "choose a profile for VIA before turning it on"
    );
    assert!(!dongle.sent().contains(&"set_adapter"));
    let Outcome::Interface(_, status) =
        run(&c, &rx, via(Some(true), Some(Pick::Profile(name("Games"))))).unwrap()
    else {
        panic!()
    };
    let saved = profiles::interface(&status, ConfigurationInterface::Via).unwrap();
    assert!(saved.enabled && saved.profile == 2);
    eventually(&c, &rx, |st| {
        profiles::interface(&st.status, ConfigurationInterface::Via).is_some_and(|s| s.enabled)
    });
    // Vial conflicts with VIA.
    let vial = Command::Interface {
        interface: ConfigurationInterface::Vial,
        enabled: Some(true),
        profile: Some(Pick::Profile(id(1))),
    };
    let error = run(&c, &rx, vial).unwrap_err();
    assert_eq!(
        error.message,
        "VIA and Vial can't both be on. Turn one off first"
    );
    // An enabled interface keeps its profile; a selected profile isn't deleted.
    assert!(run(&c, &rx, via(None, Some(Pick::Clear))).is_err());
    let error = run(&c, &rx, Command::ProfileDelete(id(2))).unwrap_err();
    assert!(
        error.message.starts_with("VIA is using this profile"),
        "{error:?}"
    );
    // Several changes go out in one request, applied in order.
    let update = AdapterUpdate {
        interfaces: vec![
            InterfaceUpdate {
                interface: ConfigurationInterface::Via,
                enabled: Some(false),
                profile: Some(0),
            },
            InterfaceUpdate {
                interface: ConfigurationInterface::Vial,
                enabled: Some(true),
                profile: Some(1),
            },
        ],
        ..Default::default()
    };
    run(&c, &rx, Command::AdapterSave(update)).unwrap();
    let sim = dongle.0.lock().unwrap();
    let Some(p::request::Command::SetAdapter(last)) = sim.log.last() else {
        panic!()
    };
    assert_eq!(last.configuration_interfaces.len(), 2);
    let vial = profiles::interface(&sim.status, ConfigurationInterface::Vial).unwrap();
    assert!(vial.enabled && vial.profile == 1);
}

#[test]
fn adapter_names_are_trimmed_before_sending() {
    let dongle = Dongle::default();
    let (c, rx) = ready(&dongle);
    let Outcome::Name { name, reset } =
        run(&c, &rx, Command::Name(Some("  Desk\u{a0}".into()))).unwrap()
    else {
        panic!()
    };
    assert_eq!((name.as_str(), reset), ("Desk", false));
    assert_eq!(c.state().unwrap().status.name, "Desk");
    let Some(p::request::Command::SetAdapter(update)) =
        dongle.0.lock().unwrap().log.last().cloned()
    else {
        panic!()
    };
    assert_eq!(update.name.as_deref(), Some("Desk"));
    // A name that is empty once trimmed is refused before sending.
    let sent = dongle.sent().len();
    let error = run(&c, &rx, Command::Name(Some(" \u{a0} ".into()))).unwrap_err();
    assert_eq!(error.message, "invalid adapter name");
    assert_eq!(dongle.sent().len(), sent);
    // Padding beyond 64 bytes is allowed, as the adapter limits the trimmed name.
    let long = format!("  {}  ", "x".repeat(64));
    run(&c, &rx, Command::Name(Some(long))).unwrap();
    assert_eq!(c.state().unwrap().status.name, "x".repeat(64));
}

#[test]
fn rules_are_checked_saved_and_forgotten() {
    let dongle = with_profiles();
    let (c, rx) = ready(&dongle);
    let at = |input: &str| profiles::parse_input(input).unwrap();
    let remap = |input, outputs: &str| {
        profiles::save_rule(profiles::rule(
            at(input),
            p::profile_rule::Effect::Remap(p::profile_rule::Remap {
                outputs: profiles::parse_outputs(outputs).unwrap(),
            }),
        ))
    };
    let axis = |input, destination, invert| {
        profiles::save_rule(profiles::rule(
            at(input),
            p::profile_rule::Effect::Axis(p::profile_rule::Axis {
                destination: Some(profiles::parse_usage(destination).unwrap()),
                invert,
            }),
        ))
    };
    let words =
        |rules: &[p::ProfileRule]| rules.iter().map(profiles::rule_words).collect::<Vec<_>>();
    // An input the adapter can't remap is refused before sending.
    let error = run(
        &c,
        &rx,
        Command::RuleChange(id(1), remap("01:30@01:02", "07:04")),
    )
    .unwrap_err();
    assert_eq!(error.message, "this adapter can't remap 01:30@01:02");
    assert!(!dongle.sent().contains(&"set_profile_rules"));
    let Outcome::Rules { profile, rules } = run(
        &c,
        &rx,
        Command::RuleChange(name("Mouse Fix"), remap("09:04@01:02", "09:01")),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(profile.id, 3);
    // The profile holds the rule sent in its saved form, with the output's collection resolved.
    assert_eq!(words(&rules), ["09:04@01:02 remap 09:01@01:02"]);
    // The profile's roles follow its rules.
    eventually(&c, &rx, |st| {
        st.profile(3)
            .is_some_and(|p| p.roles == [p::Role::Mouse as i32])
    });
    let Outcome::Rules { rules, .. } = run(&c, &rx, Command::Rules(id(3))).unwrap() else {
        panic!()
    };
    // A listing reports the output's collection the adapter resolved.
    assert_eq!(words(&rules), ["09:04@01:02 remap 09:01@01:02"]);
    // The same button in the gamepad collection is a separate rule, and an axis rule is listed
    // in order of input, then collection, across pages.
    for change in [
        remap("09:04@01:05", "07:04"),
        axis("01:30@01:05", "01:31", true),
    ] {
        run(&c, &rx, Command::RuleChange(id(3), change)).unwrap();
    }
    let Outcome::Rules { rules, .. } = run(&c, &rx, Command::Rules(id(3))).unwrap() else {
        panic!()
    };
    assert_eq!(
        words(&rules),
        [
            "01:30@01:05 axis 01:31 invert",
            "09:04@01:02 remap 09:01@01:02",
            "09:04@01:05 remap 07:04@01:06",
        ]
    );
    eventually(&c, &rx, |st| {
        st.profile(3).is_some_and(|p| {
            [p::Role::Mouse, p::Role::Gamepad]
                .iter()
                .all(|r| p.roles.contains(&(*r as i32)))
        })
    });
    // An axis to itself without inversion forgets the axis's rule.
    let Outcome::Rules { rules, .. } = run(
        &c,
        &rx,
        Command::RuleChange(id(3), axis("01:30@01:05", "01:30", false)),
    )
    .unwrap() else {
        panic!()
    };
    assert!(rules.is_empty());
    // Forgetting one collection's rule keeps the other's.
    let forget = profiles::forget_rule(at("09:04@01:05"));
    let Outcome::Rules { rules, .. } = run(&c, &rx, Command::RuleChange(id(3), forget)).unwrap()
    else {
        panic!()
    };
    assert!(rules.is_empty());
    let Outcome::Rules { rules, .. } = run(&c, &rx, Command::Rules(id(3))).unwrap() else {
        panic!()
    };
    assert_eq!(words(&rules), ["09:04@01:02 remap 09:01@01:02"]);
    let forget = profiles::forget_rule(at("09:04@01:02"));
    run(&c, &rx, Command::RuleChange(id(3), forget)).unwrap();
    // Outputs are shown sorted and once, as the adapter saves them.
    let Outcome::Rules { rules, .. } = run(
        &c,
        &rx,
        Command::RuleChange(id(3), remap("07:39@01:06", "07:e0,07:04,07:e0@01:06")),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(words(&rules), ["07:39@01:06 remap 07:04@01:06,07:e0@01:06"]);
    let Outcome::Rules { rules: listed, .. } = run(&c, &rx, Command::Rules(id(3))).unwrap() else {
        panic!()
    };
    assert_eq!(listed, rules);
    // A rule that changes nothing forgets the input's rule.
    let Outcome::Rules { rules, .. } = run(
        &c,
        &rx,
        Command::RuleChange(id(3), remap("07:39@01:06", "07:39")),
    )
    .unwrap() else {
        panic!()
    };
    assert!(rules.is_empty());
    let Outcome::Rules { rules, .. } = run(&c, &rx, Command::Rules(id(3))).unwrap() else {
        panic!()
    };
    assert!(rules.is_empty());
}

#[test]
fn opening_reads_the_names_of_profiles_in_use() {
    let dongle = Dongle::with(|sim| {
        common::enable_profiles(&mut sim.status);
        sim.add_profile("Work", Vec::new());
        sim.add_profile("Games", Vec::new());
        sim.add_profile("Unused", Vec::new());
        sim.devices[0].profiles = Some(p::ProfileLayers { profiles: vec![2] });
        sim.status.configuration_interfaces[0].profile = 1;
    });
    let (c, _rx) = ready(&dongle);
    let st = c.state().unwrap();
    assert_eq!(st.profile(1).unwrap().name, "Work");
    assert_eq!(st.profile(2).unwrap().name, "Games");
    assert!(st.profile(3).is_none());
    let reads = dongle
        .sent()
        .iter()
        .filter(|s| **s == "get_profile")
        .count();
    assert_eq!(reads, 2);
}

#[test]
fn a_missing_device_and_a_missing_profile_are_told_apart() {
    let dongle = with_profiles();
    let (c, rx) = ready(&dongle);
    dongle
        .0
        .lock()
        .unwrap()
        .refuse
        .insert("set_device", refusal(ErrorCode::NotFound));
    let error = run(&c, &rx, Command::Set(id(1), Toggle::Trusted, false)).unwrap_err();
    assert_eq!(
        cordial::ui::text::error_line(&error),
        "not_found: the adapter has no saved device with that ID"
    );
    let error = run(&c, &rx, Command::Layers(id(1), vec![id(1)])).unwrap_err();
    assert_eq!(
        cordial::ui::text::error_line(&error),
        "not_found: the adapter has no profile with that ID"
    );
}

#[test]
fn a_download_replaces_only_the_file_it_was_allowed_to() {
    let dir = std::env::temp_dir().join(format!("cordial-replace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dongle = Dongle::with(|sim| {
        sim.data.insert("/devices/1/device.pb".into(), vec![0xff]);
    });
    let (c, rx) = ready(&dongle);
    let get = || Command::FileGet {
        path: "/devices/1/device.pb".into(),
        local: dir.join("device.json"),
        overwrite: true,
        raw: false,
    };
    // Replacing device.json was confirmed, but the file doesn't decode, so its bytes go to
    // device.pb, which exists and is kept.
    std::fs::write(dir.join("device.pb"), b"old").unwrap();
    assert_eq!(
        run(&c, &rx, get()).unwrap_err().message,
        format!(
            "the file couldn't be converted, and saving its bytes to {} failed: the local file \
             already exists",
            dir.join("device.pb").display()
        )
    );
    assert_eq!(std::fs::read(dir.join("device.pb")).unwrap(), b"old");
    std::fs::remove_file(dir.join("device.pb")).unwrap();
    let Ok(Outcome::FileSaved {
        local, unconverted, ..
    }) = run(&c, &rx, get())
    else {
        panic!("saved");
    };
    assert_eq!(local, dir.join("device.pb"));
    assert!(unconverted.is_some());
    assert_eq!(std::fs::read(dir.join("device.pb")).unwrap(), [0xff]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn gamepad_output_is_saved_only_where_the_adapter_offers_it() {
    let dongle = Dongle::default();
    let (c, rx) = ready(&dongle);
    let error = run(&c, &rx, Command::GamepadOutput(id(1), p::OutputMode::Hid)).unwrap_err();
    assert_eq!(
        error.message,
        "this adapter doesn't offer HID gamepad output"
    );
    assert!(!dongle.sent().contains(&"set_device"));
    // Disabled is always accepted.
    run(
        &c,
        &rx,
        Command::GamepadOutput(id(1), p::OutputMode::Disabled),
    )
    .unwrap();

    let dongle = Dongle::with(|sim| common::enable_gamepads(&mut sim.status));
    let (c, rx) = ready(&dongle);
    let outcome = run(
        &c,
        &rx,
        Command::GamepadOutput(id(1), p::OutputMode::Xinput),
    )
    .unwrap();
    let Outcome::Device { device, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        device.output_preferences,
        [p::OutputPreference {
            role: p::Role::Gamepad as i32,
            mode: p::OutputMode::Xinput as i32,
        }]
    );
    {
        let sim = dongle.0.lock().unwrap();
        let Some(p::request::Command::SetDevice(last)) = sim.log.last() else {
            panic!("{:?}", sim.log.last());
        };
        // Only the gamepad choice is sent.
        assert_eq!(
            *last,
            p::SetDevice {
                device: 1,
                output_preferences: device.output_preferences.clone(),
                ..Default::default()
            }
        );
    }
    let outcome = run(
        &c,
        &rx,
        Command::GamepadOutput(id(1), p::OutputMode::Disabled),
    )
    .unwrap();
    assert!(
        matches!(outcome, Outcome::Device { device, .. } if device.output_preferences.is_empty())
    );
}

/// Sets the adapter's committed and target storage formats.
fn formats(sim: &mut common::Sim, current: Option<u32>, target: u32) {
    let information = sim.status.information.as_mut().unwrap();
    information.storage_format = current;
    information.target_storage_format = Some(target);
}

fn storage_error(code: ErrorCode, outcome: p::ErrorOutcome) -> p::Error {
    p::Error {
        code: code as i32,
        reason: p::ErrorReason::Storage as i32,
        outcome: outcome as i32,
        ..Default::default()
    }
}

/// The marker the adapter reports from boot until restart while it waits for a migration.
fn required() -> p::Error {
    storage_error(
        ErrorCode::StorageMigrationRequired,
        p::ErrorOutcome::NoEffect,
    )
}

fn unusable() -> [Command; 4] {
    [
        Command::Files("/".into()),
        Command::Bootloader,
        Command::Devices,
        Command::FileGet {
            path: "/format.pb".into(),
            local: "unused".into(),
            overwrite: false,
            raw: true,
        },
    ]
}

fn first_phase(rx: &mpsc::Receiver<Event>) -> Phase {
    match until(rx, |e| matches!(e, Event::Connection { .. })) {
        Event::Connection { phase, .. } => phase,
        _ => unreachable!(),
    }
}

#[test]
fn an_adapter_that_needs_migration_takes_only_status_until_closed() {
    // The marker decides, whatever committed format the adapter reports.
    for current in [Some(1), Some(2), None] {
        let dongle = Dongle::with(|sim| {
            sim.status.ready = false;
            formats(sim, current, 2);
            sim.status.errors = vec![required()];
        });
        let (c, rx) = setup(&dongle);
        c.open("sim".into());
        let phase = first_phase(&rx);
        assert!(matches!(phase, Phase::Migrate), "{current:?}: {phase:?}");
        for command in unusable() {
            let e = run(&c, &rx, command).unwrap_err();
            assert!(e.message.contains("must be migrated"), "{e:?}");
        }
        run(&c, &rx, Command::Status).unwrap();
        assert_eq!(dongle.sent(), ["get_status", "get_status"]);
        c.close();
        until(&rx, |e| matches!(e, Event::Closed));
        assert_eq!(dongle.handles(), 0);
    }
}

#[test]
fn other_errors_with_the_marker_take_only_status_and_restart() {
    let blocked = || storage_error(ErrorCode::StorageFailed, p::ErrorOutcome::Unspecified);
    let radio = p::Error {
        reason: p::ErrorReason::Radio as i32,
        ..storage_error(ErrorCode::NotReady, p::ErrorOutcome::NoEffect)
    };
    let mut unknown = required();
    unknown.outcome = p::ErrorOutcome::Unknown as i32;
    let mut related = required();
    related.related = vec![blocked()];
    for (current, errors) in [
        (None, vec![required(), blocked()]),
        (Some(1), vec![blocked(), required()]),
        (Some(2), vec![required(), radio]),
        (Some(1), vec![unknown]),
        (Some(3), vec![related]),
    ] {
        let dongle = Dongle::with(|sim| {
            sim.status.ready = false;
            formats(sim, current, 2);
            sim.status.errors = errors.clone();
        });
        let (c, rx) = setup(&dongle);
        c.open("sim".into());
        let Phase::Blocked(error) = first_phase(&rx) else {
            panic!("expected Blocked for {current:?} {errors:?}");
        };
        let wanted = errors.iter().find(|e| **e != required());
        assert_eq!(error.dongle.as_ref(), wanted, "{error:?}");
        for command in unusable() {
            let e = run(&c, &rx, command).unwrap_err();
            assert_eq!(e.message, error.message, "{e:?}");
        }
        run(&c, &rx, Command::Status).unwrap();
        assert!(matches!(
            run(&c, &rx, Command::Restart).unwrap(),
            Outcome::Restart
        ));
        assert_eq!(dongle.sent(), ["get_status", "get_status", "restart"]);
        // The adapter resets after its reply.
        eventually(&c, &rx, |st| !st.available);
    }
}

#[test]
fn without_the_marker_an_unready_adapter_waits_as_usual() {
    for (current, code) in [
        (Some(2), ErrorCode::NotReady),
        (Some(1), ErrorCode::NotReady),
        (None, ErrorCode::StorageFailed),
    ] {
        let dongle = Dongle::with(|sim| {
            sim.status = common::status(false);
            formats(sim, current, 2);
            sim.status.errors = vec![storage_error(code, p::ErrorOutcome::NoEffect)];
            sim.files.insert("/".into(), Vec::new());
        });
        let (c, rx) = setup(&dongle);
        c.open("sim".into());
        let phase = first_phase(&rx);
        assert!(matches!(phase, Phase::Opened), "{current:?}: {phase:?}");
        until(&rx, |e| {
            matches!(
                e,
                Event::Connection {
                    phase: Phase::Waiting,
                    ..
                }
            )
        });
        let e = run(&c, &rx, Command::Devices).unwrap_err();
        assert!(e.message.contains("still starting"), "{e:?}");
        run(&c, &rx, Command::Files("/".into())).unwrap();
        // Restart doesn't wait for readiness.
        run(&c, &rx, Command::Restart).unwrap();
        assert_eq!(dongle.sent(), ["get_status", "list_files", "restart"]);
    }
}

#[test]
fn file_access_works_with_matching_formats_or_readiness() {
    let unready = |sim: &mut common::Sim| {
        sim.status = common::status(false);
        sim.status.errors = vec![p::Error {
            code: ErrorCode::NotReady as i32,
            reason: p::ErrorReason::Radio as i32,
            ..Default::default()
        }];
    };
    let ready = |sim: &mut common::Sim| sim.status = common::status(true);
    for setup_status in [unready as fn(&mut common::Sim), ready] {
        let dongle = Dongle::with(|sim| {
            setup_status(sim);
            sim.files.insert("/".into(), Vec::new());
        });
        let (c, rx) = setup(&dongle);
        c.open("sim".into());
        until(&rx, |e| {
            matches!(
                e,
                Event::Connection {
                    phase: Phase::Waiting | Phase::Ready,
                    ..
                }
            )
        });
        run(&c, &rx, Command::Files("/".into())).unwrap();
        assert!(dongle.sent().contains(&"list_files"));
    }
}

#[test]
fn an_unsupported_or_unlabelled_adapter_takes_only_status() {
    for (current, target, wanted) in [
        (Some(2), 3, "Update Cordial"),
        (Some(1), 1, "Update Cordial"),
        (None, 3, "Update Cordial"),
        (Some(2), 0, "firmware"),
    ] {
        let dongle = Dongle::with(|sim| {
            formats(sim, current, target);
            sim.status.errors = vec![required()];
        });
        let (c, rx) = setup(&dongle);
        c.open("sim".into());
        match first_phase(&rx) {
            Phase::Unsupported(e) => assert!(e.message.contains(wanted), "{e:?}"),
            phase => panic!("expected Unsupported, got {phase:?}"),
        }
        for command in [Command::Files("/".into()), Command::Restart] {
            let e = run(&c, &rx, command).unwrap_err();
            assert!(e.message.contains(wanted), "{e:?}");
        }
        assert_eq!(dongle.sent(), ["get_status"]);
    }
}

#[test]
fn an_adapter_without_a_target_takes_only_status_despite_present_information() {
    for errors in [vec![], vec![required()]] {
        let dongle = Dongle::with(|sim| {
            sim.status.ready = true;
            formats(sim, Some(2), 2);
            sim.status
                .information
                .as_mut()
                .unwrap()
                .target_storage_format = None;
            sim.status.errors = errors;
        });
        let (c, rx) = setup(&dongle);
        c.open("sim".into());
        assert!(matches!(first_phase(&rx), Phase::Unsupported(_)));
        for command in unusable().into_iter().chain([Command::Restart]) {
            assert!(
                run(&c, &rx, command)
                    .unwrap_err()
                    .message
                    .contains("firmware")
            );
        }
        run(&c, &rx, Command::Status).unwrap();
        assert_eq!(dongle.sent(), ["get_status", "get_status"]);
        c.close();
        until(&rx, |e| matches!(e, Event::Closed));
        assert_eq!(dongle.handles(), 0);
    }
}

#[test]
fn another_adapter_on_the_expected_port_takes_only_status() {
    let dongle = Dongle::default();
    let (c, rx) = setup(&dongle);
    c.open_adapter("sim".into(), "ADAPTER02".into());
    match until(&rx, |e| matches!(e, Event::Connection { .. })) {
        Event::Connection {
            phase: Phase::Other,
            ..
        } => {}
        e => panic!("expected Other, got {e:?}"),
    }
    assert_eq!(dongle.handles(), 0);
    assert!(run(&c, &rx, Command::Devices).is_err());
    assert_eq!(dongle.sent(), ["get_status"]);

    // The expected adapter opens as usual.
    let dongle = Dongle::default();
    let (c, rx) = setup(&dongle);
    c.open_adapter("sim".into(), "adapter01".into());
    until(&rx, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    assert!(dongle.sent().contains(&"list_devices"));
}

#[test]
fn closed_means_the_port_is_released_even_while_opening() {
    let dongle = Dongle::with(|sim| {
        sim.slow.insert("get_status", Duration::from_millis(200));
    });
    let (c, rx) = setup(&dongle);
    c.open("sim".into());
    // Closed while the opening status is still unanswered.
    std::thread::sleep(Duration::from_millis(50));
    c.close();
    until(&rx, |e| matches!(e, Event::Closed));
    assert_eq!(dongle.handles(), 0);
}

#[test]
fn incompatible_opening_retains_metadata_without_requesting_status_or_cached_state() {
    let dongle = Dongle::with(|sim| {
        sim.hello.protocol_version = Some(p::CURRENT_PROTOCOL_VERSION + 1);
        sim.hello.name = Some("Desk".into());
    });
    let (controller, events) = setup(&dongle);
    controller.open("sim".into());
    let Event::Connection {
        phase: Phase::Failed { error, open },
        ..
    } = until(&events, |event| {
        matches!(
            event,
            Event::Connection {
                phase: Phase::Failed { .. },
                ..
            }
        )
    })
    else {
        unreachable!()
    };
    assert!(!open);
    assert_eq!(
        error.native_hello.unwrap().hello.name.as_deref(),
        Some("Desk")
    );
    assert!(dongle.sent().is_empty());
    assert!(controller.state().is_none());
    assert!(events.try_iter().all(|event| !matches!(
        event,
        Event::Connection {
            phase: Phase::Lost(_),
            ..
        }
    )));
}

#[test]
fn closing_a_negotiating_controller_releases_its_port_without_waiting_for_the_hello_deadline() {
    let dongle = Dongle::with(|sim| sim.answer_hello = false);
    let (controller, events) = setup(&dongle);
    controller.open("sim".into());
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while dongle.0.lock().unwrap().hellos == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let started = std::time::Instant::now();
    controller.close();
    until(&events, |event| matches!(event, Event::Closed));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(dongle.sent().is_empty());
    assert!(controller.state().is_none());
}

#[test]
fn closing_a_replacement_waits_for_the_previous_sessions_reader() {
    use std::{
        io::{self, Read, Write},
        sync::Mutex,
    };

    struct Reader {
        responses: Receiver<Vec<u8>>,
        entered: mpsc::Sender<()>,
        release: Receiver<()>,
        reads: usize,
    }
    impl Read for Reader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.reads == 3 {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
                return Ok(0);
            }
            let bytes = self.responses.recv().unwrap();
            out[..bytes.len()].copy_from_slice(&bytes);
            self.reads += 1;
            Ok(bytes.len())
        }
    }
    struct Writer {
        responses: mpsc::Sender<Vec<u8>>,
        closing: mpsc::Sender<()>,
        writes: usize,
    }
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut response = Vec::new();
            if self.writes == 0 {
                p::frame::encode(
                    &p::AdapterHello {
                        protocol_version: Some(p::CURRENT_PROTOCOL_VERSION),
                        ..Default::default()
                    },
                    &mut response,
                );
            } else {
                p::frame::encode(
                    &p::Message {
                        kind: Some(p::message::Kind::Response(p::Response {
                            result: Some(if self.writes == 1 {
                                p::response::Result::Status(common::status(true))
                            } else {
                                p::response::Result::Devices(p::DeviceList::default())
                            }),
                        })),
                    },
                    &mut response,
                );
            }
            self.writes += 1;
            self.responses.send(response).unwrap();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Drop for Writer {
        fn drop(&mut self) {
            self.closing.send(()).unwrap();
        }
    }

    let (responses, input) = mpsc::channel();
    let (entered, reading) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let (closing, shutdown) = mpsc::channel();
    let reader = Mutex::new(Some(Reader {
        responses: input,
        entered,
        release: held,
        reads: 0,
    }));
    let (send, events) = mpsc::channel();
    let controller = Controller::with_connector(
        move |event| {
            let _ = send.send(event);
        },
        move |_, handler| {
            Ok(cordial_client::Connection::with_handler(
                reader.lock().unwrap().take().unwrap(),
                Writer {
                    responses: responses.clone(),
                    closing: closing.clone(),
                    writes: 0,
                },
                handler,
            ))
        },
    );
    controller.open("sim".into());
    until(&events, |event| {
        matches!(
            event,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    reading.recv_timeout(Duration::from_secs(1)).unwrap();
    controller.open("sim".into());
    shutdown.recv_timeout(Duration::from_secs(1)).unwrap();
    controller.close();
    assert!(events.recv_timeout(Duration::from_millis(100)).is_err());
    release.send(()).unwrap();
    until(&events, |event| matches!(event, Event::Closed));
}

#[test]
fn a_framed_eof_cannot_publish_loss_before_the_opening_phase() {
    use std::sync::Mutex;

    let dongle = Dongle::default();
    let connect = dongle.connector();
    let (send, events) = mpsc::channel();
    let (opening, opened) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let held = Mutex::new(held);
    let (closing, closed) = mpsc::channel();
    let controller = Controller::with_connector(
        move |event| {
            if matches!(
                event,
                Event::Connection {
                    phase: Phase::Opened,
                    ..
                }
            ) {
                opening.send(()).unwrap();
                held.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            let _ = send.send(event);
        },
        move |port, mut handler| {
            let closing = closing.clone();
            connect(
                port,
                Box::new(move |received| {
                    if matches!(received, cordial_client::Received::Closed(_)) {
                        closing.send(()).unwrap();
                    }
                    handler(received);
                }),
            )
        },
    );
    for announced in [true, false] {
        let session = controller.open("sim".into());
        opened.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(controller.state().unwrap().status.id, "ADAPTER01");
        assert!(controller.state().unwrap().session_ending.is_none());
        if announced {
            dongle.end_session(p::SessionEndingReason::ConfigurationChange as i32);
            until(&events, |event| {
                matches!(
                    event,
                    Event::Notice {
                        notice: cordial::controller::Notice::Event {
                            event: p::Event {
                                kind: Some(p::event::Kind::SessionEnding(_))
                            },
                            ..
                        },
                        ..
                    }
                )
            });
        } else {
            dongle.unplug();
        }
        closed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(events.recv_timeout(Duration::from_millis(100)).is_err());
        release.send(()).unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Connection { session: s, phase: Phase::Opened, .. } if s == session
        ));
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Connection { session: s, phase: Phase::Lost(_), .. } if s == session
        ));
        let state = controller.state().unwrap();
        assert_eq!(state.session_ending.is_some(), announced);
        assert!(!state.available);
        controller.close();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Closed
        ));
        assert_eq!(dongle.handles(), 0);
        assert!(events.try_iter().next().is_none());
    }
}

#[test]
fn intentional_ending_is_session_scoped_and_duplicates_keep_the_first_deadline() {
    let dongle = Dongle::default();
    let (controller, events) = ready(&dongle);
    assert!(controller.state().unwrap().session_ending.is_none());
    let event = |reason| p::event::Kind::SessionEnding(p::SessionEnding { reason });
    dongle.event(event(9876));
    until(&events, |e| {
        matches!(
            e,
            Event::Notice {
                notice: cordial::controller::Notice::Event {
                    event: p::Event {
                        kind: Some(p::event::Kind::SessionEnding(_))
                    },
                    ..
                },
                ..
            }
        )
    });
    let first = controller.state().unwrap().session_ending.unwrap();
    assert!(!controller.state().unwrap().ready());
    dongle.event(event(p::SessionEndingReason::ConfigurationChange as i32));
    until(&events, |e| {
        matches!(
            e,
            Event::Notice {
                notice: cordial::controller::Notice::Event {
                    event: p::Event {
                        kind: Some(p::event::Kind::SessionEnding(_))
                    },
                    ..
                },
                ..
            }
        )
    });
    assert_eq!(controller.state().unwrap().session_ending, Some(first));
    dongle.unplug();
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Lost(_),
                ..
            }
        )
    });
    controller.open("sim".into());
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        )
    });
    assert!(controller.state().unwrap().session_ending.is_none());
    dongle.end_session(p::SessionEndingReason::DeviceConnection as i32);
    until(&events, |e| {
        matches!(
            e,
            Event::Connection {
                phase: Phase::Lost(_),
                ..
            }
        )
    });
    assert!(controller.state().unwrap().session_ending.unwrap() >= first);
    controller.close();
    until(&events, |e| matches!(e, Event::Closed));
}

#[test]
fn an_open_adapter_without_an_id_still_reports_its_loss() {
    let dongle = Dongle::with(|sim| sim.status.id.clear());
    let (controller, events) = ready(&dongle);
    dongle.unplug();
    until(&events, |event| {
        matches!(
            event,
            Event::Connection {
                phase: Phase::Lost(_),
                ..
            }
        )
    });
    let state = controller.state().unwrap();
    assert!(!state.available);
    assert!(state.session_ending.is_none());
    controller.close();
    until(&events, |event| matches!(event, Event::Closed));
}
