mod common;

use common::{Dongle, Plan, device, dpi};
use cordial_cli::{
    controller::{
        AdapterUpdate, Command, Controller, Event, Outcome, Phase, Pick, RunOptions, SettingInput,
        State, Target, Toggle, Wait,
    },
    error::Error,
    profiles::{self, InterfaceUpdate},
};
use cordial_protocol::{
    self as p, ConfigurationInterface, DeviceState, ErrorCode, SettingState, Transport,
    value::Value,
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
fn opening_loads_status_devices_and_warnings_then_events_keep_the_view() {
    let dongle = Dongle::with(|sim| {
        sim.devices[0].state = DeviceState::Connected as i32;
        sim.warnings.insert(
            1,
            vec![p::DeviceWarning {
                code: p::WarningCode::IndicatorWriteFailed as i32,
                service: 0,
                report_type: p::ReportType::Output as i32,
                report_id: Some(1),
                ..Default::default()
            }],
        );
    });
    let (c, rx) = ready(&dongle);
    let st = c.state().unwrap();
    assert_eq!(st.status.name, "Desk");
    assert_eq!(st.devices.len(), 2);
    assert_eq!(st.warnings_of(1).len(), 1);
    assert!(st.ready());
    // Only the connected device has warnings to read.
    assert_eq!(
        dongle.sent(),
        ["get_status", "list_devices", "list_warnings"]
    );
    // A device event replaces the record; a removal drops it with its lists.
    let mut connected = device(1, "Renamed Mouse", Transport::Ble);
    connected.state = DeviceState::Connected as i32;
    dongle.event(p::event::Kind::Device(connected));
    eventually(&c, &rx, |st| st.device(1).unwrap().name == "Renamed Mouse");
    dongle.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 1 }));
    eventually(&c, &rx, |st| {
        st.device(1).is_none() && st.warnings_of(1).is_empty()
    });
}

#[test]
fn opening_reads_every_page_of_devices() {
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
    assert!(st.unreadable.contains(&4));
    let pages = dongle
        .sent()
        .iter()
        .filter(|s| **s == "list_devices")
        .count();
    assert_eq!(pages, 3);
    let sim = dongle.0.lock().unwrap();
    let afters: Vec<u32> = sim
        .log
        .iter()
        .filter_map(|c| match c {
            p::request::Command::ListDevices(l) => Some(l.after),
            _ => None,
        })
        .collect();
    assert_eq!(afters, [0, 2, 4]);
    drop(sim);
    // A refresh replaces the listing, page by page.
    dongle.0.lock().unwrap().devices.retain(|d| d.id != 5);
    run(&c, &rx, Command::Devices).unwrap();
    let ids: Vec<u32> = c.state().unwrap().devices.iter().map(|d| d.id).collect();
    assert_eq!(ids, [1, 2, 3, 6]);
}

#[test]
fn an_unready_adapter_waits_then_loads_when_it_reports_ready() {
    let dongle = Dongle::with(|sim| sim.status = common::status(false));
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
            truncated: false
        }
    ));
}

#[test]
fn saving_settings_sends_one_write_and_events_report_the_apply() {
    let dongle = Dongle::with(|sim| {
        sim.settings.insert(1, vec![dpi(800, None)]);
    });
    let (c, rx) = ready(&dongle);
    run(&c, &rx, Command::Settings(id(1))).unwrap();
    let error = run(
        &c,
        &rx,
        Command::SettingSet(
            id(1),
            "pointer.sensor.0.dpi".into(),
            SettingInput::Text("1234".into()),
        ),
    )
    .unwrap_err();
    assert_eq!(
        error.message,
        "pointer.sensor.0.dpi takes an integer from 400 through 4000 in steps of 50"
    );
    assert!(!dongle.sent().contains(&"set_settings"));
    let outcome = run(
        &c,
        &rx,
        Command::SettingsSave {
            device: 1,
            set: vec![("pointer.sensor.0.dpi".into(), Value::Integer(1600))],
            forget: vec![],
        },
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Saved { set, .. } if set == ["pointer.sensor.0.dpi"]));
    let applied = Some(p::setting::Status::State(SettingState::Applied as i32));
    eventually(&c, &rx, |st| st.settings_of(1)[0].status == applied);
    let forgot = run(
        &c,
        &rx,
        Command::SettingForget(id(1), "pointer.sensor.0.dpi".into()),
    )
    .unwrap();
    assert!(matches!(forgot, Outcome::Saved { forget, .. } if forget.len() == 1));
    // A forget is a change in the same SetSettings command.
    {
        let sim = dongle.0.lock().unwrap();
        let Some(p::request::Command::SetSettings(last)) = sim.log.last() else {
            panic!("{:?}", sim.log.last());
        };
        assert!(matches!(
            last.changes[..],
            [p::SettingChange {
                change: Some(p::setting_change::Change::Forget(_)),
                ..
            }]
        ));
    }
    let unknown = run(&c, &rx, Command::SettingGet(id(1), "wheel.mode".into())).unwrap_err();
    assert_eq!(unknown.code_of(), Some(ErrorCode::NotFound));
}

#[test]
fn a_dongle_refusal_is_reported_without_a_refresh() {
    let dongle = Dongle::with(|sim| {
        sim.refuse.insert("disconnect_device", ErrorCode::Busy);
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
        sim.page_size = 2;
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
        Command::Profiles { after: 0 },
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
fn profiles_are_listed_by_page_and_found_by_unique_name() {
    let dongle = with_profiles();
    let (c, rx) = ready(&dongle);
    let Outcome::Profiles { list, .. } = run(&c, &rx, Command::Profiles { after: 0 }).unwrap()
    else {
        panic!()
    };
    assert_eq!(list.entries.len(), 2);
    assert!(!list.end);
    assert_eq!(cordial_client::paging::Page::next(&list), Some(2));
    // Without a cursor, every page is read.
    let Outcome::Profiles { list, .. } = run(&c, &rx, Command::AllProfiles).unwrap() else {
        panic!()
    };
    assert_eq!(list.entries.len(), 4);
    assert!(list.end);
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
        .insert("delete_profile", ErrorCode::InUse);
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
    let usage = common::usage;
    let remap = |input, outputs: &str| {
        profiles::save_rule(p::ProfileRule {
            input: Some(input),
            effect: Some(p::profile_rule::Effect::Remap(p::profile_rule::Remap {
                outputs: profiles::parse_outputs(outputs).unwrap(),
            })),
        })
    };
    // An input the adapter can't remap is refused before sending.
    let error = run(
        &c,
        &rx,
        Command::RuleChange(id(1), remap(usage(0x01, 0x30), "07:04")),
    )
    .unwrap_err();
    assert_eq!(error.message, "this adapter can't remap 01:30");
    assert!(!dongle.sent().contains(&"set_profile_rules"));
    let Outcome::Rules { profile, rules } = run(
        &c,
        &rx,
        Command::RuleChange(name("Mouse Fix"), remap(usage(0x09, 4), "09:01")),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(profile.id, 3);
    // The profile holds the rule sent in its saved form, with the output's collection resolved.
    assert_eq!(profiles::rule_words(&rules[0]), "09:04 remap 09:01@01:02");
    // The profile's roles follow its rules.
    eventually(&c, &rx, |st| {
        st.profile(3)
            .is_some_and(|p| p.roles == [p::Role::Mouse as i32])
    });
    let Outcome::Rules { rules, .. } = run(&c, &rx, Command::Rules(id(3))).unwrap() else {
        panic!()
    };
    // A listing reports the output's collection the adapter resolved.
    assert_eq!(profiles::rule_words(&rules[0]), "09:04 remap 09:01@01:02");
    let forget = profiles::forget_rule(usage(0x09, 4));
    let Outcome::Rules { rules, .. } = run(&c, &rx, Command::RuleChange(id(3), forget)).unwrap()
    else {
        panic!()
    };
    assert!(rules.is_empty());
    // Outputs are shown sorted and once, as the adapter saves them.
    let Outcome::Rules { rules, .. } = run(
        &c,
        &rx,
        Command::RuleChange(id(3), remap(usage(0x07, 0x39), "07:e0,07:04,07:e0@01:06")),
    )
    .unwrap() else {
        panic!()
    };
    let saved = "07:39 remap 07:04@01:06,07:e0@01:06";
    assert_eq!(profiles::rule_words(&rules[0]), saved);
    let Outcome::Rules { rules: listed, .. } = run(&c, &rx, Command::Rules(id(3))).unwrap() else {
        panic!()
    };
    assert_eq!(listed, rules);
    // A rule that changes nothing forgets the input's rule.
    let Outcome::Rules { rules, .. } = run(
        &c,
        &rx,
        Command::RuleChange(id(3), remap(usage(0x07, 0x39), "07:39")),
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
        .insert("set_device", ErrorCode::NotFound);
    let error = run(&c, &rx, Command::Set(id(1), Toggle::Trusted, false)).unwrap_err();
    assert_eq!(
        cordial_cli::ui::text::error_line(&error),
        "not_found: the adapter has no saved device with that ID"
    );
    let error = run(&c, &rx, Command::Layers(id(1), vec![id(1)])).unwrap_err();
    assert_eq!(
        cordial_cli::ui::text::error_line(&error),
        "not_found: the adapter has no profile with that ID"
    );
}
