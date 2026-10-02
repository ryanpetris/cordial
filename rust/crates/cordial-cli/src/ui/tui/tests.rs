//! Model tests against a stand-in controller: what the TUI draws, and which
//! commands clicks and keys send.
use super::*;
use crate::ui::{
    catalog::tests::integer,
    command::tests as fixture,
    fake::{Call, Fake},
};
use crate::{controller::Notice, view::Pending};
use cordial_protocol::{DeviceState, ErrorCode, SettingState, keys, pairing, setting};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyModifiers},
    layout::Rect,
};

struct App {
    m: Model<Fake>,
    fake: Fake,
}

fn port(name: &str) -> PortInfo {
    PortInfo {
        port: name.into(),
        id: format!("S{name}"),
    }
}

impl App {
    fn new(width: usize, height: usize, explicit: Option<&str>) -> Self {
        let fake = Fake::default();
        let mut m = Model::new(fake.clone(), explicit.map(str::to_owned));
        m.width = width;
        m.height = height;
        Self { m, fake }
    }
    /// A TUI connected to the fixture adapter and showing its devices.
    fn connected(width: usize, height: usize) -> Self {
        let mut app = Self::new(width, height, None);
        app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
        let session = app.fake.next.get();
        let mut st = fixture::state();
        st.session = session;
        *app.fake.state.borrow_mut() = Some(st);
        app.phase(session, Phase::Opened);
        app.phase(session, Phase::Ready);
        app.fake.calls.borrow_mut().clear();
        app
    }
    fn phase(&mut self, session: SessionId, phase: Phase) {
        self.m.update(Msg::Controller(Box::new(Event::Connection {
            session,
            port: "/dev/ttyACM0".into(),
            phase,
        })));
    }
    fn edit(&mut self, f: impl FnOnce(&mut State)) {
        f(self.fake.state.borrow_mut().as_mut().unwrap());
    }
    fn calls(&self) -> Vec<Call> {
        self.fake.calls.borrow().clone()
    }
    fn ran(&self, text: &str) -> bool {
        self.calls()
            .iter()
            .any(|c| matches!(c, Call::Run(r) if r.contains(text)))
    }
    fn render(&mut self) -> Vec<String> {
        let mut buf = Buffer::empty(Rect::new(0, 0, self.m.width as u16, self.m.height as u16));
        self.m.render(&mut buf);
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }
    /// Renders and returns the cells of `text`'s first on-screen occurrence.
    fn cells(&mut self, text: &str) -> Vec<ratatui::buffer::Cell> {
        let mut buf = Buffer::empty(Rect::new(0, 0, self.m.width as u16, self.m.height as u16));
        self.m.render(&mut buf);
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(i) = row.find(text) {
                let x = row[..i].chars().count() as u16;
                return (x..x + text.chars().count() as u16)
                    .map(|x| buf[(x, y)].clone())
                    .collect();
            }
        }
        panic!("{text} not shown:\n{}", self.screen());
    }
    fn screen(&mut self) -> String {
        self.render().join("\n")
    }
    fn hit(&mut self, action: &Action) -> Hit {
        self.render();
        match self.m.hits.iter().find(|h| h.action == *action) {
            Some(h) => h.clone(),
            None => panic!("{action:?} unreachable by mouse:\n{}", self.screen()),
        }
    }
    fn reachable(&mut self, action: &Action) -> bool {
        self.render();
        self.m.hits.iter().any(|h| h.action == *action)
    }
    fn mouse(&mut self, kind: MouseEventKind, x: usize, y: usize) {
        self.m.update(Msg::Mouse(MouseEvent {
            kind,
            column: x as u16,
            row: y as u16,
            modifiers: KeyModifiers::NONE,
        }));
    }
    fn click(&mut self, action: Action) {
        let h = self.hit(&action);
        self.mouse(MouseEventKind::Down(MouseButton::Left), h.x, h.y);
        self.mouse(MouseEventKind::Up(MouseButton::Left), h.x, h.y);
    }
    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.render();
        self.m.update(Msg::Key(KeyEvent::new(code, modifiers)));
    }
    fn press(&mut self, code: KeyCode) {
        self.key(code, KeyModifiers::NONE);
    }
    fn done(&mut self, ticket_of: &str, result: Result<Outcome, Error>) {
        let ticket = *self
            .m
            .jobs
            .iter()
            .find(|(_, j)| format!("{:?}", j.command).contains(ticket_of))
            .unwrap()
            .0;
        let session = self.m.session.unwrap();
        self.m.update(Msg::Controller(Box::new(Event::Done {
            session,
            ticket,
            result,
        })));
    }
    fn notice(&mut self, notice: Notice) {
        let session = self.m.session.unwrap();
        self.m
            .update(Msg::Controller(Box::new(Event::Notice { session, notice })));
    }
    fn pairing(&mut self, step: pairing::Step) {
        self.edit(|st| {
            st.pairing = Some(p::Pairing {
                candidate: "c_2".into(),
                step: Some(step),
            })
        });
        self.m.tick();
    }
}

#[test]
fn startup_opens_only_a_single_adapter_once() {
    let mut app = App::new(100, 30, None);
    assert!(app.screen().contains("Looking for adapters"));
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
    assert_eq!(app.calls(), [Call::List, Call::Open("/dev/ttyACM0".into())]);
    assert!(app.screen().contains("Connecting to /dev/ttyACM0"));
    // A failure returns to the chooser, and later lists never retry.
    let session = app.fake.next.get();
    app.phase(
        session,
        Phase::Failed {
            error: Error::new("no answer"),
            open: false,
        },
    );
    let screen = app.screen();
    assert!(
        screen.contains("Choose an Adapter") && screen.contains("no answer"),
        "{screen}"
    );
    assert!(screen.contains("S/dev/ttyACM0"), "{screen}");
    app.click(Action::RefreshPorts);
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
    let opens = |app: &App| {
        app.calls()
            .iter()
            .filter(|c| matches!(c, Call::Open(_)))
            .count()
    };
    assert_eq!(opens(&app), 1);
    app.click(Action::Port("/dev/ttyACM0".into()));
    assert_eq!(opens(&app), 2);

    let mut explicit = App::new(100, 30, Some("/dev/ttyUSB9"));
    explicit
        .m
        .update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
    assert_eq!(
        explicit.calls(),
        [Call::List, Call::Open("/dev/ttyUSB9".into())]
    );
}

type Setup = (&'static str, fn(&mut App));

#[test]
fn ctrl_c_quits_from_every_screen() {
    let setups: Vec<Setup> = vec![
        ("devices", |_| {}),
        ("menu", |a| a.click(Action::Menu(Menu::Adapter))),
        ("help", |a| a.press(KeyCode::Char('?'))),
        ("confirm", |a| {
            a.m.selected = "d_1".into();
            a.click(Action::Remove);
        }),
        ("pairing code", |a| {
            a.pairing(pairing::Step::EnterCode(p::EnterCode {
                kind: CodeKind::Passkey as i32,
            }));
            assert!(a.m.form_focused);
        }),
        ("too small", |a| a.m.width = 20),
    ];
    for (name, setup) in setups {
        let mut app = App::connected(100, 30);
        setup(&mut app);
        app.render();
        app.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.m.quitting, "{name}");
        assert_eq!(app.calls().last(), Some(&Call::Close), "{name}");
        assert!(
            !app.ran("Unpair") && !app.ran("Reject"),
            "{name}: quitting sent a change"
        );
        app.m.update(Msg::Controller(Box::new(Event::Closed)));
        assert!(app.m.done, "{name}");
    }
}

#[test]
fn a_click_needs_press_and_release_on_the_same_control() {
    let mut app = App::connected(100, 30);
    let row = app.hit(&Action::Device("d_1".into()));
    app.mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y);
    assert!(app.m.selected.is_empty(), "press alone activated");
    app.mouse(MouseEventKind::Up(MouseButton::Left), row.x, row.y + 2);
    assert!(app.m.selected.is_empty(), "release elsewhere activated");
    app.mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y);
    app.mouse(MouseEventKind::Up(MouseButton::Left), row.x, row.y);
    assert_eq!(app.m.selected, "d_1");
    let screen = app.screen();
    assert!(
        screen.contains("[Disconnect]") && screen.contains("[Remove]"),
        "{screen}"
    );
    // Enter never disconnects the selected device.
    app.press(KeyCode::Enter);
    assert!(!app.ran("Disconnect"));
    app.click(Action::Disconnect);
    assert!(app.ran("Disconnect(\"d_1\")"));
}

#[test]
fn pointer_and_tab_share_the_highlight() {
    let mut app = App::connected(100, 30);
    let help = app.hit(&Action::Help);
    app.mouse(MouseEventKind::Moved, help.x, help.y);
    app.render();
    assert_eq!(app.m.focus, Some(Action::Help));
    app.mouse(MouseEventKind::Moved, 0, app.m.height - 1);
    app.render();
    assert_eq!(app.m.focus, None);
    assert!(app.m.dialog.is_none());
    app.mouse(MouseEventKind::Moved, help.x, help.y);
    app.render();
    app.press(KeyCode::Tab);
    assert_eq!(app.m.focus, Some(Action::Quit));
    app.press(KeyCode::Enter);
    assert!(app.m.quitting);
}

#[test]
fn pairing_code_survives_resize_and_is_submitted() {
    let mut app = App::connected(100, 30);
    app.pairing(pairing::Step::EnterCode(p::EnterCode {
        kind: CodeKind::Passkey as i32,
    }));
    let screen = app.screen();
    assert!(
        screen.contains("Pair With Keyboard") && screen.contains("six-digit"),
        "{screen}"
    );
    for c in "0427".chars() {
        app.press(KeyCode::Char(c));
    }
    app.m.update(Msg::Resize(60, 20));
    app.render();
    assert_eq!(app.m.form.value(), "0427");
    app.click(Action::Accept);
    assert_eq!(app.m.form_err, "passkey must contain exactly six digits");
    app.press(KeyCode::Char('3'));
    app.press(KeyCode::Char('1'));
    app.press(KeyCode::Enter);
    assert!(app.ran("Accept(Some(\"042731\"))"), "{:?}", app.calls());
    // Cancel rejects the prompt.
    app.click(Action::CancelDialog);
    assert!(app.ran("Reject"));
}

#[test]
fn comparison_and_display_prompts() {
    let mut app = App::connected(100, 30);
    app.pairing(pairing::Step::ConfirmCode(p::ConfirmCode {
        passkey: "482916".into(),
    }));
    let screen = app.screen();
    assert!(
        screen.contains("4 8 2   9 1 6") && screen.contains("[Codes match]"),
        "{screen}"
    );
    assert!(!app.m.form_focused);
    app.press(KeyCode::Char('y'));
    assert!(app.ran("Accept(None)"));
    app.pairing(pairing::Step::ShowCode(p::ShowCode {
        kind: CodeKind::Passkey as i32,
        value: "123456".into(),
    }));
    // The dialog is drawn undimmed over the dimmed screen behind it.
    let dim = |c: &ratatui::buffer::Cell| c.modifier.contains(ratatui::style::Modifier::DIM);
    assert!(
        !app.cells("1 2 3   4 5 6").iter().any(dim),
        "{}",
        app.screen()
    );
    assert!(app.cells("Cordial").iter().all(dim));
    app.click(Action::CancelDialog);
    assert!(app.ran("CancelPairing"), "{:?}", app.calls());
}

#[test]
fn every_main_action_is_clickable() {
    let mut app = App::connected(100, 48);
    app.click(Action::Menu(Menu::Scan));
    for text in ["Bluetooth LE and Classic", "Bluetooth LE only"] {
        let cells = app.cells(text);
        assert!(
            cells.iter().all(|c| c.modifier.is_empty()),
            "{text}: {cells:?}"
        );
    }
    app.click(Action::Scan(Some(Transport::Ble)));
    assert!(app.ran("Scan { transports: [Ble], seconds: 0 }"));
    app.edit(|st| st.scanning = Some(vec![Transport::Ble]));
    assert!(app.screen().contains("Scanning BLE"));
    app.click(Action::ScanOff);
    assert!(app.ran("ScanStop"));
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::Refresh);
    assert!(app.ran("Devices"));
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::Bootloader);
    app.click(Action::Confirm);
    assert!(app.ran("Bootloader"));
    app.click(Action::Device("c_2".into()));
    app.click(Action::Hide);
    assert!(app.calls().contains(&Call::Hide("c_2".into())));
    app.click(Action::Device("d_1".into()));
    app.click(Action::Hidpp(true));
    assert!(app.ran("Set(\"d_1\", Hidpp, true)"));
    app.click(Action::Remove);
    assert!(app.screen().contains("Remove Keyboard?"));
    app.click(Action::Confirm);
    assert!(app.ran("Unpair(\"d_1\")"));
    app.click(Action::Help);
    app.click(Action::CancelDialog);
    assert!(app.m.dialog.is_none());
    app.click(Action::Quit);
    assert!(app.m.quitting);
}

#[test]
fn layouts_stack_and_shrink() {
    let mut app = App::connected(89, 30);
    let screen = app.render();
    let devices = screen.iter().position(|l| l.contains("Devices")).unwrap();
    let details = screen.iter().position(|l| l.contains("Details")).unwrap();
    assert!(details > devices, "narrow panes stack");
    let mut wide = App::connected(120, 30);
    let screen = wide.render();
    assert!(
        screen
            .iter()
            .any(|l| l.contains("Devices") && l.contains("Details"))
    );
    let mut small = App::connected(39, 9);
    let screen = small.screen();
    assert!(screen.starts_with("[Quit]") && screen.contains("Resize to at least"));
    small.click(Action::Quit);
    assert!(small.m.quitting);
}

#[test]
fn hostile_names_are_escaped_on_screen() {
    let mut app = App::connected(100, 40);
    app.edit(|st| st.candidates[1].name = "Test\u{9b}2J\u{202e}键\u{e0001}board".into());
    app.m.selected = "c_2".into();
    let screen = app.screen();
    assert!(!screen.contains(['\u{9b}', '\u{202e}', '\u{e0001}']));
    assert!(screen.contains("\\u009b"), "{screen}");
}

#[test]
fn lost_adapter_keeps_the_last_state() {
    let mut app = App::connected(100, 30);
    app.edit(|st| st.available = false);
    let session = app.m.session.unwrap();
    app.phase(session, Phase::Lost(Error::new("unplugged")));
    let screen = app.screen();
    assert!(
        screen.contains("Lost the adapter connection") && screen.contains("Keyboard"),
        "{screen}"
    );
    app.click(Action::Reopen);
    assert_eq!(app.calls().last(), Some(&Call::Open("/dev/ttyACM0".into())));
}

#[test]
fn hidpp_status_is_one_line_and_translation_is_never_reported() {
    let mut app = App::connected(120, 60);
    app.edit(|st| {
        st.devices[0].integrations = vec![p::Integration {
            kind: p::IntegrationKind::Hidpp as i32,
            enabled: true,
            detected: Some(p::IntegrationDetection {
                version: Some(p::Version { major: 4, minor: 2 }),
            }),
            status: Some(p::integration::Status::State(
                p::IntegrationState::Active as i32,
            )),
        }];
    });
    app.click(Action::Device("d_1".into()));
    // The details pane keeps the switch; the status lines are in Diagnostics.
    let screen = app.screen();
    assert!(!screen.contains("HID++ 4.2"), "{screen}");
    app.click(Action::Diagnostics);
    let screen = app.screen();
    assert!(screen.contains("HID++ 4.2"), "{screen}");
    assert!(screen.contains("Logitech Features Active"), "{screen}");
    assert!(!screen.contains("Special-Key"), "{screen}");
    app.edit(|st| {
        st.devices[0].integrations[0].status = Some(p::integration::Status::Error(
            ErrorCode::TransportError as i32,
        ));
    });
    let screen = app.screen();
    assert!(
        screen.contains("Logitech Features Failed:") && screen.contains("couldn't send"),
        "{screen}"
    );
    app.click(Action::CancelDialog);
}

#[test]
fn nearby_candidates_pair_and_saved_rows_never_pair() {
    let mut app = App::connected(100, 40);
    app.click(Action::Device("c_1".into()));
    app.click(Action::Pair);
    assert!(app.ran("Pair(\"c_1\")"));
    app.click(Action::Device("d_1".into()));
    assert!(!app.reachable(&Action::Pair));
    app.press(KeyCode::Char('p'));
    assert_eq!(
        app.calls()
            .iter()
            .filter(|c| matches!(c, Call::Run(r) if r.starts_with("Pair")))
            .count(),
        1
    );
    // A running pairing shows progress and offers Cancel instead of Pair.
    app.edit(|st| {
        st.pairing = Some(p::Pairing {
            candidate: "c_1".into(),
            step: Some(pairing::Step::Connecting(p::PairingConnecting {})),
        })
    });
    app.click(Action::Device("c_1".into()));
    let screen = app.screen();
    assert!(screen.contains("Pairing…"), "{screen}");
    app.click(Action::CancelPairing);
    assert!(app.ran("CancelPairing"));
}

#[test]
fn full_storage_explains_why_pairing_is_unavailable() {
    let mut app = App::connected(100, 40);
    app.edit(|st| {
        st.status.info.push(p::Info {
            key: keys::STORAGE_FULL.into(),
            value: Some(crate::model::wire_value(Value::Bool(true))),
        })
    });
    app.click(Action::Device("c_1".into()));
    let screen = app.screen();
    assert!(screen.contains("Storage Full"), "{screen}");
    assert!(!app.reachable(&Action::Pair));
    app.press(KeyCode::Char('p'));
    assert!(!app.ran("Pair"));
}

#[test]
fn saved_devices_enable_and_disable() {
    let mut app = App::connected(100, 40);
    app.edit(|st| {
        st.devices[1].enabled = false;
        st.devices[1].inactive = Some(p::InactiveReason::Disabled as i32);
    });
    app.click(Action::Device("d_2".into()));
    let screen = app.screen();
    assert!(screen.contains("○ Disabled"), "{screen}");
    assert!(!app.reachable(&Action::Connect), "{screen}");
    app.click(Action::Enable);
    assert!(app.ran("Set(\"d_2\", Enabled, true)"));
    app.press(KeyCode::Char('c'));
    assert!(!app.ran("Connect"));
}

#[test]
fn link_security_follows_the_connection() {
    let mut app = App::connected(120, 60);
    app.edit(|st| {
        st.devices[0].security = Some(p::Security {
            encrypted: Some(true),
            authenticated: Some(false),
            secure_connections: None,
            key_size: Some(16),
        })
    });
    app.click(Action::Device("d_1".into()));
    assert!(!app.screen().contains("128 bits"));
    app.click(Action::Diagnostics);
    let screen = app.screen();
    assert!(screen.contains("Encrypted, Unauthenticated"), "{screen}");
    assert!(screen.contains("Encryption Key: 128 bits"), "{screen}");
    app.edit(|st| st.devices[0].state = DeviceState::Disconnected as i32);
    assert!(!app.screen().contains("128 bits"));
}

#[test]
fn battery_is_listed_and_details_show_device_info() {
    let mut app = App::connected(120, 60);
    app.edit(|st| {
        st.devices[0].info = vec![
            p::Info {
                key: keys::BATTERY_LEVEL.into(),
                value: Some(crate::model::wire_value(Value::Integer(80))),
            },
            p::Info {
                key: keys::BATTERY_CHARGING.into(),
                value: Some(crate::model::wire_value(Value::Bool(true))),
            },
            p::Info {
                key: keys::DEVICE_MODEL.into(),
                value: Some(crate::model::wire_value(Value::Text("MX Keys".into()))),
            },
            p::Info {
                key: "future.key".into(),
                value: Some(crate::model::wire_value(Value::Text("hidden".into()))),
            },
        ]
    });
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    assert!(screen.contains("80%↑"), "{screen}");
    assert!(
        screen.contains("Model") && screen.contains("MX Keys"),
        "{screen}"
    );
    assert!(!screen.contains("hidden"), "{screen}");
}

#[test]
fn diagnostics_show_the_last_connection_error() {
    let mut app = App::connected(120, 60);
    app.edit(|st| {
        let d = st.devices.iter_mut().find(|d| d.id == "d_1").unwrap();
        d.state = p::DeviceState::Disconnected as i32;
        d.error = Some(p::ErrorCode::ConnectionFailed as i32);
    });
    app.click(Action::Device("d_1".into()));
    app.click(Action::Diagnostics);
    let screen = app.screen();
    assert!(screen.contains("Last Error"), "{screen}");
    // The reason is shown in words, never as a raw code.
    assert!(!screen.contains("connection_failed"), "{screen}");
    // Refreshing needs a connection, so the dialog doesn't offer it.
    assert!(!screen.contains("Refresh Info"), "{screen}");
}

#[test]
fn a_device_without_warnings_has_no_warnings_section_in_diagnostics() {
    let mut app = App::connected(120, 60);
    app.click(Action::Device("d_1".into()));
    app.click(Action::Diagnostics);
    let screen = app.screen();
    assert!(!screen.contains("Device Warnings"), "{screen}");
    assert!(screen.contains("Identifiers"), "{screen}");
    assert!(screen.contains("d_1"), "{screen}");
}

#[test]
fn warnings_show_in_diagnostics_and_are_not_logged() {
    let mut app = App::connected(120, 60);
    let warning = p::DeviceWarning {
        code: p::WarningCode::IndicatorWriteFailed as i32,
        service: 1,
        report_type: p::ReportType::Output as i32,
        report_id: Some(3),
        ..Default::default()
    };
    app.edit(|st| {
        st.warnings.insert("d_1".into(), vec![]);
    });
    app.m.remember();
    app.edit(|st| {
        st.warnings.insert("d_1".into(), vec![warning]);
    });
    app.notice(Notice::Event {
        event: p::Event {
            kind: Some(p::event::Kind::Warnings(p::DeviceWarnings {
                device: "d_1".into(),
                warnings: vec![warning],
            })),
        },
        first: false,
        changed: vec![],
    });
    assert!(
        app.m
            .logs
            .iter()
            .all(|l| !l.text.contains("indicator lights"))
    );
    app.click(Action::Device("d_1".into()));
    assert!(!app.screen().contains("Service 1, Output Report 3"));
    app.click(Action::Diagnostics);
    let screen = app.screen();
    assert!(screen.contains("Service 1, Output Report 3"), "{screen}");
}

#[test]
fn settings_stage_changes_until_save_and_send_them_together() {
    let mut app = App::connected(120, 50);
    let mut level = integer(keys::BACKLIGHT_LEVEL, 0, 10, 1);
    level.r#type = Some(setting::Type::Integer(p::IntegerSetting {
        value: Some(3),
        saved: None,
        limits: Some(p::integer_setting::Limits::Range(p::IntegerRange {
            min: 0,
            max: 10,
            step: 1,
        })),
    }));
    let mut dpi = integer("pointer.sensor.0.dpi", 400, 4000, 50);
    dpi.r#type = Some(setting::Type::Integer(p::IntegerSetting {
        value: Some(800),
        saved: Some(800),
        limits: Some(p::integer_setting::Limits::Range(p::IntegerRange {
            min: 400,
            max: 4000,
            step: 50,
        })),
    }));
    dpi.status = Some(setting::Status::State(SettingState::Applied as i32));
    app.edit(|st| {
        st.devices[0].integrations = vec![p::Integration {
            kind: p::IntegrationKind::Hidpp as i32,
            enabled: true,
            detected: None,
            status: Some(p::integration::Status::State(
                p::IntegrationState::Active as i32,
            )),
        }];
        st.settings
            .insert("d_1".into(), vec![level.clone(), dpi.clone()]);
    });
    app.click(Action::Device("d_1".into()));
    app.click(Action::DeviceSettings);
    assert!(app.ran("Settings(\"d_1\")"));
    app.done(
        "Settings",
        Ok(Outcome::Settings(crate::controller::Subject {
            id: "d_1".into(),
            name: "Keyboard".into(),
        })),
    );
    let screen = app.screen();
    assert!(screen.contains("Manual Backlight Level"), "{screen}");
    assert!(screen.contains("Pointer Speed"), "{screen}");
    app.click(Action::Setting(keys::BACKLIGHT_LEVEL.into()));
    app.click(Action::Step(keys::BACKLIGHT_LEVEL.into(), 1));
    assert!(app.screen().contains("✎ Changed"));
    app.click(Action::Setting("pointer.sensor.0.dpi".into()));
    app.click(Action::Forget("pointer.sensor.0.dpi".into()));
    // Nothing is sent until Save, which sends every staged change at once.
    assert!(!app.ran("SettingsSave"));
    app.click(Action::SaveAll);
    assert!(
        app.ran(
            "SettingsSave { device: \"d_1\", set: [(\"backlight.level\", Integer(4))], forget: [\"pointer.sensor.0.dpi\"] }"
        ),
        "{:?}",
        app.calls()
    );
    assert!(app.screen().contains("Saving Settings…"));
    app.done(
        "SettingsSave",
        Err(Error::code(ErrorCode::StorageFailed, Some("setting set"))),
    );
    let screen = app.screen();
    assert!(screen.contains("Couldn't Save"), "{screen}");
    assert!(
        app.m.page.drafts["d_1"].len() == 2,
        "a failed save keeps the drafts"
    );
    app.click(Action::SaveAll);
    app.done(
        "SettingsSave",
        Ok(Outcome::Saved {
            subject: crate::controller::Subject {
                id: "d_1".into(),
                name: "Keyboard".into(),
            },
            set: vec![keys::BACKLIGHT_LEVEL.into()],
            forget: vec!["pointer.sensor.0.dpi".into()],
            settings: vec![],
        }),
    );
    assert!(app.m.page.drafts["d_1"].is_empty());
    app.press(KeyCode::Esc);
    assert!(app.m.page.device.is_empty());
}

#[test]
fn rename_dialog_validates_and_saves() {
    let mut app = App::connected(100, 40);
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::AdapterSettings);
    app.click(Action::Rename);
    assert_eq!(app.m.form.value(), "Desk");
    app.m.form.set_value("");
    app.click(Action::SaveName);
    assert_eq!(app.m.form_err, "Invalid adapter name");
    app.m.form.set_value("Den");
    app.press(KeyCode::Enter);
    assert!(app.ran("Name(Some(\"Den\"))"));
    app.done(
        "Name",
        Err(Error::code(ErrorCode::BadArgs, Some("adapter name"))),
    );
    assert_eq!(
        app.m.form_err,
        "the adapter rejected the command's arguments"
    );
}

#[test]
fn each_supported_transport_is_switched_in_adapter_settings() {
    let mut app = App::connected(100, 44);
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::AdapterSettings);
    let screen = app.screen();
    assert!(screen.contains("Bluetooth Classic  [● On]"), "{screen}");
    assert!(screen.contains("Bluetooth LE       [● On]"), "{screen}");
    // The current value sends nothing.
    app.click(Action::Transport(Transport::Ble, true));
    assert!(!app.ran("Transport"));
    app.click(Action::Transport(Transport::Ble, false));
    assert!(app.ran("Transport(Ble, false)"));
    app.done(
        "Transport",
        Err(Error::code(
            ErrorCode::StorageFailed,
            Some("adapter transport"),
        )),
    );
    assert_eq!(
        app.m.transport_err,
        Some((
            Transport::Ble,
            "the adapter couldn't read or write its saved data".into()
        ))
    );
    assert!(app.m.form_err.is_empty());
    let screen = app.screen();
    assert!(
        screen.contains("✕ the adapter couldn't read or write"),
        "{screen}"
    );
    assert!(
        app.m
            .logs
            .iter()
            .any(|l| l.text.starts_with("Couldn't disable Bluetooth LE: "))
    );
    app.click(Action::Transport(Transport::Ble, false));
    assert!(app.m.transport_err.is_none());
    app.done("Transport", Ok(Outcome::Transport(Transport::Ble, false)));
    assert!(app.m.logs.iter().any(|l| l.text == "Bluetooth LE disabled"));

    // Firmware that predates the setting offers no choice.
    app.edit(|st| st.status.transports[0].enabled = None);
    assert!(!app.reachable(&Action::Transport(Transport::Classic, false)));
    app.edit(|st| st.status.transports[0].enabled = Some(true));

    // A BLE-only adapter has no Classic choice.
    app.edit(|st| {
        st.status
            .transports
            .retain(|t| t.transport != Transport::Classic as i32);
    });
    let screen = app.screen();
    assert!(!screen.contains("Bluetooth Classic  ["), "{screen}");
    assert!(!app.reachable(&Action::Transport(Transport::Classic, true)));
    assert!(app.reachable(&Action::Transport(Transport::Ble, false)));
}

#[test]
fn devices_of_a_disabled_transport_explain_it_and_offer_no_connect_or_pair() {
    let mut app = App::connected(120, 40);
    app.edit(|st| {
        st.status.transports[0].enabled = Some(false);
        st.devices[1].transport = Transport::Classic as i32;
        st.devices[1].inactive = Some(p::InactiveReason::TransportDisabled as i32);
        st.candidates[0].transport = Transport::Classic as i32;
    });
    let screen = app.screen();
    assert!(screen.contains("[Scan BLE]"), "{screen}");
    app.click(Action::Device("d_2".into()));
    let screen = app.screen();
    assert!(
        screen.contains("Bluetooth Classic is disabled. Enable it in the"),
        "{screen}"
    );
    assert!(!app.reachable(&Action::Connect), "{screen}");
    app.press(KeyCode::Char('c'));
    assert!(!app.ran("Connect"));

    app.click(Action::Device("c_1".into()));
    let screen = app.screen();
    assert!(
        screen.contains("Bluetooth Classic is disabled. Enable it in the"),
        "{screen}"
    );
    assert!(!app.reachable(&Action::Pair), "{screen}");
    app.press(KeyCode::Char('p'));
    assert!(!app.ran("Pair"));

    // With every transport disabled, nothing is scanned.
    app.edit(|st| st.status.transports[1].enabled = Some(false));
    let screen = app.screen();
    assert!(!screen.contains("[Scan"), "{screen}");
}

#[test]
fn files_list_and_download_with_replace_confirmation() {
    let mut app = App::connected(120, 40);
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::FilesOpen);
    assert!(app.ran("Files(\"/\")"));
    let ticket = app.m.files.listing.unwrap();
    let session = app.m.session.unwrap();
    app.m.update(Msg::Controller(Box::new(Event::Done {
        session,
        ticket,
        result: Ok(Outcome::Files {
            path: "/".into(),
            entries: vec![
                p::FileEntry {
                    name: "log.txt".into(),
                    directory: false,
                    size: 12,
                },
                p::FileEntry {
                    name: "data".into(),
                    directory: true,
                    size: 0,
                },
            ],
        }),
    })));
    let screen = app.screen();
    assert!(
        screen.contains("data/") && screen.contains("log.txt"),
        "{screen}"
    );
    app.click(Action::FilesEntry("log.txt".into()));
    let dir = std::env::temp_dir().join(format!("cordial-tui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let local = dir.join("log.txt");
    std::fs::write(&local, b"old").unwrap();
    app.m.files.dest.set_value(&local.display().to_string());
    app.click(Action::FilesDownload);
    assert!(matches!(app.m.dialog, Some(Dialog::Replace(_))));
    app.click(Action::Confirm);
    assert!(app.ran("overwrite: true"), "{:?}", app.calls());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unnamed_devices_of_unknown_kind_are_hidden_until_shown() {
    let mut app = App::connected(100, 40);
    app.edit(|st| {
        st.candidates.push(p::Candidate {
            id: "c_3".into(),
            transport: Transport::Ble as i32,
            name: String::new(),
            kind: p::Kind::Unknown as i32,
            rssi: None,
        })
    });
    let screen = app.screen();
    assert!(!screen.contains("  Unnamed Device "), "{screen}");
    assert!(screen.contains("1 unnamed device hidden"), "{screen}");
    app.click(Action::ShowUnnamed(true));
    assert!(app.screen().contains("  Unnamed Device "));
}

#[test]
fn pending_commands_show_progress_beside_their_device() {
    let mut app = App::connected(100, 40);
    app.edit(|st| {
        st.pending.push(Pending {
            command: "device connect",
            target: Some("d_2".into()),
        })
    });
    assert!(app.screen().contains("Connecting…"));
}

#[test]
fn an_adapter_that_keeps_starting_is_shown_not_ready_with_files() {
    let mut app = App::new(120, 40, Some("/dev/ttyACM0"));
    let session = app.fake.next.get();
    let mut st = fixture::state();
    st.session = session;
    st.status.ready = false;
    st.loaded = false;
    *app.fake.state.borrow_mut() = Some(st);
    app.phase(session, Phase::Opened);
    app.phase(session, Phase::Waiting);
    app.m.tick();
    assert!(app.screen().contains("Waiting for adapter"));
    app.m.waiting_since = Some(Instant::now() - STARTUP_GRACE);
    app.m.tick();
    let screen = app.screen();
    assert!(screen.contains("The adapter isn't ready"), "{screen}");
    app.click(Action::FilesOpen);
    assert!(app.ran("Files(\"/\")"));
    app.edit(|st| {
        st.status.ready = true;
        st.loaded = true;
    });
    app.phase(session, Phase::Ready);
    assert!(!app.screen().contains("The adapter isn't ready"));
}

#[test]
fn the_scan_bar_names_only_the_enabled_transports_being_scanned() {
    let mut app = App::connected(100, 40);
    app.edit(|st| st.scanning = Some(vec![Transport::Classic, Transport::Ble]));
    let screen = app.screen();
    assert!(screen.contains("Scanning BLE + Classic…"), "{screen}");
    // Disabling a transport drops it from the running scan.
    app.edit(|st| st.status.transports[0].enabled = Some(false));
    let screen = app.screen();
    assert!(screen.contains("Scanning BLE…"), "{screen}");
    // With every scanned transport disabled before the scan's end arrives, the bar keeps the
    // name the scan started with.
    app.edit(|st| st.status.transports[1].enabled = Some(false));
    let screen = app.screen();
    assert!(screen.contains("Scanning BLE + Classic…"), "{screen}");
}
