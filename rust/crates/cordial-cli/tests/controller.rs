mod common;

use common::{Dongle, Plan, device, dpi};
use cordial_cli::{
    controller::{
        Command, Controller, Event, Outcome, Phase, RunOptions, SettingInput, State, Toggle, Wait,
    },
    error::Error,
};
use cordial_protocol::{self as p, DeviceState, ErrorCode, SettingState, Transport, value::Value};
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
        sim.warnings.insert(
            "d_1".into(),
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
    assert_eq!(st.warnings_of("d_1").len(), 1);
    assert!(st.ready());
    assert_eq!(
        dongle.sent(),
        [
            "get_status",
            "list_devices",
            "list_warnings",
            "list_warnings"
        ]
    );
    // A device event replaces the record; a removal drops it with its lists.
    let mut connected = device("d_1", "Renamed Mouse", Transport::Ble);
    connected.state = DeviceState::Connected as i32;
    dongle.event(p::event::Kind::Device(connected));
    eventually(&c, &rx, |st| {
        st.device("d_1").unwrap().name == "Renamed Mouse"
    });
    dongle.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
        id: "d_1".into(),
    }));
    eventually(&c, &rx, |st| {
        st.device("d_1").is_none() && st.warnings_of("d_1").is_empty()
    });
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
        // Classic allows one enabled device; d_3 already holds it.
        sim.devices
            .push(device("d_3", "Classic Pad", Transport::Classic));
    });
    let (c, rx) = ready(&dongle);
    let error = run(&c, &rx, Command::Set("d_2".into(), Toggle::Enabled, true)).unwrap_err();
    assert_eq!(error.code_of(), Some(ErrorCode::NoCapacity));
    assert!(!dongle.sent().contains(&"set_device"));
    // Disabling the other one makes room.
    run(&c, &rx, Command::Set("d_3".into(), Toggle::Enabled, false)).unwrap();
    let outcome = run(&c, &rx, Command::Set("d_2".into(), Toggle::Enabled, true)).unwrap();
    assert!(matches!(outcome, Outcome::Device { device, .. } if device.enabled));
    // The integration preference is a partial update naming only HID++.
    run(&c, &rx, Command::Set("d_1".into(), Toggle::Hidpp, false)).unwrap();
    let sim = dongle.0.lock().unwrap();
    let Some(p::request::Command::SetDevice(last)) = sim.log.last() else {
        panic!("{:?}", sim.log.last());
    };
    assert_eq!(last.enabled, None);
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
    let error = run(&c, &rx, Command::Connect("Old Keyboard".into())).unwrap_err();
    assert_eq!(error.code_of(), Some(ErrorCode::Disabled));
    let outcome = run(&c, &rx, Command::Connect("d_1".into())).unwrap();
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
    eventually(&c, &rx, |st| st.candidate("c_1").is_some());
    let ticket = c.run(Command::Pair("New Keyboard".into()), RunOptions::default());
    eventually(&c, &rx, |st| {
        st.pairing
            .as_ref()
            .is_some_and(|p| matches!(p.step, Some(p::pairing::Step::EnterCode(_))))
    });
    // A second pairing is refused while this one runs.
    let busy = run(&c, &rx, Command::Pair("c_1".into())).unwrap_err();
    assert_eq!(busy.code_of(), Some(ErrorCode::Busy));
    let short = run(&c, &rx, Command::Accept(Some("12".into()))).unwrap_err();
    assert_eq!(short.message, "passkey must contain exactly six digits");
    run(&c, &rx, Command::Accept(Some("123456".into()))).unwrap();
    let Event::Done { result, .. } = until(
        &rx,
        |e| matches!(e, Event::Done { ticket: t, .. } if *t == ticket),
    ) else {
        unreachable!()
    };
    match result.unwrap() {
        Outcome::Paired { subject, device } => {
            assert_eq!(subject.id, "d_9");
            assert_eq!(device.unwrap().name, "New Keyboard");
        }
        other => panic!("{other:?}"),
    }
    assert!(c.state().unwrap().device("d_9").is_some());
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
    eventually(&c, &rx, |st| st.candidate("c_1").is_some());
    let ticket = c.run(Command::Pair("c_1".into()), RunOptions::default());
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
        sim.settings.insert("d_1".into(), vec![dpi(800, None)]);
    });
    let (c, rx) = ready(&dongle);
    run(&c, &rx, Command::Settings("d_1".into())).unwrap();
    let error = run(
        &c,
        &rx,
        Command::SettingSet(
            "d_1".into(),
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
            device: "d_1".into(),
            set: vec![("pointer.sensor.0.dpi".into(), Value::Integer(1600))],
            forget: vec![],
        },
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Saved { set, .. } if set == ["pointer.sensor.0.dpi"]));
    let applied = Some(p::setting::Status::State(SettingState::Applied as i32));
    eventually(&c, &rx, |st| st.settings_of("d_1")[0].status == applied);
    let forgot = run(
        &c,
        &rx,
        Command::SettingForget("d_1".into(), "pointer.sensor.0.dpi".into()),
    )
    .unwrap();
    assert!(matches!(forgot, Outcome::Saved { forget, .. } if forget.len() == 1));
    let unknown = run(
        &c,
        &rx,
        Command::SettingGet("d_1".into(), "wheel.mode".into()),
    )
    .unwrap_err();
    assert_eq!(unknown.code_of(), Some(ErrorCode::NotFound));
}

#[test]
fn a_dongle_refusal_is_reported_without_a_refresh() {
    let dongle = Dongle::with(|sim| {
        sim.refuse.insert("disconnect_device", ErrorCode::Busy);
    });
    let (c, rx) = ready(&dongle);
    let before = dongle.sent().len();
    let error = run(&c, &rx, Command::Disconnect("d_1".into())).unwrap_err();
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
fn unpairing_a_candidate_hides_it_locally() {
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
    eventually(&c, &rx, |st| st.candidate("c_1").is_some());
    let outcome = run(&c, &rx, Command::Unpair("c_1".into())).unwrap();
    assert!(matches!(outcome, Outcome::Hidden(_)));
    assert!(c.state().unwrap().candidate("c_1").is_none());
    assert!(!dongle.sent().contains(&"unpair_device"));
    run(&c, &rx, Command::Unpair("d_1".into())).unwrap();
    eventually(&c, &rx, |st| st.device("d_1").is_none());
}
