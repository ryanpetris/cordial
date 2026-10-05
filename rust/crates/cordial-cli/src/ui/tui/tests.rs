//! Model tests against a stand-in controller: what the TUI draws, and which
//! commands clicks and keys send.
use super::*;
use crate::profiles::InterfaceUpdate;
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
        serial: format!("S{name}"),
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
                candidate: 2,
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
        screen.contains("Choose Adapter") && screen.contains("no answer"),
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
        ("menu", |a| a.click(Action::Menu(Menu::Scan))),
        ("help", |a| a.press(KeyCode::Char('?'))),
        ("confirm", |a| {
            a.m.selected = Some(Item::Device(1));
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
    let row = app.hit(&Action::Select(Item::Device(1)));
    app.mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y);
    assert!(app.m.selected.is_none(), "press alone activated");
    app.mouse(MouseEventKind::Up(MouseButton::Left), row.x, row.y + 2);
    assert!(app.m.selected.is_none(), "release elsewhere activated");
    app.mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y);
    app.mouse(MouseEventKind::Up(MouseButton::Left), row.x, row.y);
    assert_eq!(app.m.selected, Some(Item::Device(1)));
    let screen = app.screen();
    assert!(
        screen.contains("[Disconnect]") && screen.contains("[Forget Device]"),
        "{screen}"
    );
    // Enter never disconnects the selected device.
    app.press(KeyCode::Enter);
    assert!(!app.ran("Disconnect"));
    app.click(Action::Disconnect);
    assert!(app.ran("Disconnect(Id(1))"));
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
        screen.contains("4 8 2   9 1 6") && screen.contains("[Codes Match]"),
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
    for text in ["Bluetooth LE and Classic", "Bluetooth LE Only"] {
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
    app.click(Action::Refresh);
    assert!(app.ran("Devices"));
    app.click(Action::Bootloader);
    app.click(Action::Confirm);
    assert!(app.ran("Bootloader"));
    app.click(Action::Select(Item::Candidate(2)));
    app.click(Action::Hide);
    assert!(app.calls().contains(&Call::Hide(2)));
    app.click(Action::Select(Item::Device(1)));
    // Device choices stage until Save, which sends them in one request.
    app.click(Action::Hidpp(true));
    assert!(!app.ran("DeviceSave"));
    app.click(Action::DeviceSave);
    assert!(app.ran("DeviceSave(1, DeviceUpdate { enabled: None, trusted: None,"));
    assert!(app.ran("blocked: None, hidpp: Some(true), layers: None })"));
    app.done(
        "DeviceSave",
        Err(Error::code(ErrorCode::Busy, Some("device set"))),
    );
    assert!(app.screen().contains("✕ Couldn't Save"));
    app.click(Action::DeviceDiscard);
    assert!(!app.reachable(&Action::DeviceSave));
    app.click(Action::Remove);
    assert!(app.screen().contains("Forget \u{201c}Keyboard\u{201d}?"));
    app.click(Action::Confirm);
    assert!(app.ran("Unpair(Id(1))"));
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
    app.m.selected = Some(Item::Candidate(2));
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
    app.click(Action::Select(Item::Device(1)));
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
    app.click(Action::Select(Item::Candidate(1)));
    app.click(Action::Pair);
    assert!(app.ran("Pair(Id(1))"));
    app.click(Action::Select(Item::Device(1)));
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
            candidate: 1,
            step: Some(pairing::Step::Connecting(p::PairingConnecting {})),
        })
    });
    app.click(Action::Select(Item::Candidate(1)));
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
    app.click(Action::Select(Item::Candidate(1)));
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
    app.click(Action::Select(Item::Device(2)));
    let screen = app.screen();
    assert!(screen.contains("○ Disabled"), "{screen}");
    assert!(!app.reachable(&Action::Connect), "{screen}");
    app.click(Action::Enable);
    assert!(!app.ran("DeviceSave"));
    app.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(app.ran("DeviceSave(2, DeviceUpdate { enabled: Some(true)"));
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
    app.click(Action::Select(Item::Device(1)));
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
    app.click(Action::Select(Item::Device(1)));
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
        let d = st.devices.iter_mut().find(|d| d.id == 1).unwrap();
        d.state = p::DeviceState::Disconnected as i32;
        d.error = Some(p::ErrorCode::ConnectionFailed as i32);
    });
    app.click(Action::Select(Item::Device(1)));
    app.click(Action::Diagnostics);
    let screen = app.screen();
    assert!(screen.contains("Last Error"), "{screen}");
    // The reason is shown in words, never as a raw code.
    assert!(!screen.contains("connection_failed"), "{screen}");
    // Refreshing needs a connection, so the dialog doesn't offer it.
    assert!(
        !app.m.hits.iter().any(|h| h.action == Action::RefreshInfo),
        "{screen}"
    );
}

#[test]
fn a_device_without_warnings_has_no_warnings_section_in_diagnostics() {
    let mut app = App::connected(120, 60);
    app.click(Action::Select(Item::Device(1)));
    app.click(Action::Diagnostics);
    let screen = app.screen();
    assert!(!screen.contains("Device Warnings"), "{screen}");
    assert!(screen.contains("Identifiers"), "{screen}");
    assert!(screen.contains(" 1"), "{screen}");
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
        st.warnings.insert(1, vec![]);
    });
    app.m.remember();
    app.edit(|st| {
        st.warnings.insert(1, vec![warning]);
    });
    app.notice(Notice::Event {
        event: p::Event {
            kind: Some(p::event::Kind::WarningsChanged(p::WarningsChanged {
                device: 1,
                added: vec![warning],
                removed: Vec::new(),
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
    app.click(Action::Select(Item::Device(1)));
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
        st.settings.insert(1, vec![level.clone(), dpi.clone()]);
    });
    app.click(Action::Select(Item::Device(1)));
    app.click(Action::DeviceSettings);
    assert!(app.ran("Settings(Id(1))"));
    app.done(
        "Settings",
        Ok(Outcome::Settings(crate::controller::Subject {
            id: 1,
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
    // Nothing is sent until Save, which sends every staged change at once. Plain s never
    // saves; Ctrl+S presses the page's Save.
    assert!(!app.ran("SettingsSave"));
    app.press(KeyCode::Char('s'));
    assert!(!app.ran("SettingsSave"));
    app.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(
        app.ran(
            "SettingsSave { device: 1, set: [(\"backlight.level\", Integer(4))], forget: [\"pointer.sensor.0.dpi\"] }"
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
        app.m.page.drafts[&1].len() == 2,
        "a failed save keeps the drafts"
    );
    app.click(Action::SaveAll);
    app.done(
        "SettingsSave",
        Ok(Outcome::Saved {
            subject: crate::controller::Subject {
                id: 1,
                name: "Keyboard".into(),
            },
            set: vec![keys::BACKLIGHT_LEVEL.into()],
            forget: vec!["pointer.sensor.0.dpi".into()],
            settings: vec![],
        }),
    );
    assert!(app.m.page.drafts[&1].is_empty());
    app.press(KeyCode::Esc);
    assert_eq!(app.m.page.device, 0);
}

#[test]
fn rename_dialog_validates_and_saves() {
    let mut app = App::connected(100, 40);
    app.click(Action::Select(Item::Adapter));
    app.click(Action::Rename);
    assert_eq!(app.m.form.value(), "Desk");
    app.m.form.set_value("");
    app.click(Action::SaveName);
    assert_eq!(app.m.form_err, "Enter an adapter name of up to 64 bytes.");
    let screen = app.screen();
    assert!(
        screen.contains("✕ Enter an adapter name of up to 64 bytes."),
        "{screen}"
    );
    // The refusal belongs to the dialog: it doesn't stay on the adapter's page.
    app.press(KeyCode::Esc);
    assert_eq!(app.m.dialog, None);
    let screen = app.screen();
    assert!(!screen.contains("adapter name"), "{screen}");
    app.click(Action::Rename);
    app.m.form.set_value("Den");
    app.press(KeyCode::Enter);
    assert!(app.ran("Name(Some(\"Den\"))"));
    app.done(
        "Name",
        Err(Error::code(ErrorCode::BadArgs, Some("adapter name"))),
    );
    // Failures read like the profile name dialog's.
    let screen = app.screen();
    assert!(
        screen.contains("✕ The adapter rejected the command's arguments"),
        "{screen}"
    );
}

#[test]
fn each_supported_transport_is_switched_in_adapter_settings() {
    let mut app = App::connected(100, 44);
    app.click(Action::Select(Item::Adapter));
    let screen = app.screen();
    assert!(screen.contains("Bluetooth Classic  [● On]"), "{screen}");
    assert!(screen.contains("Bluetooth LE       [● On]"), "{screen}");
    // Choosing stages; nothing is sent until Save, and choosing the saved value again drops
    // the change.
    app.click(Action::Transport(Transport::Ble, false));
    assert!(!app.ran("AdapterSave"));
    assert!(app.screen().contains("✎ Changed"));
    app.click(Action::Transport(Transport::Ble, true));
    assert!(!app.screen().contains("✎ Changed"));
    app.click(Action::Transport(Transport::Ble, false));
    app.click(Action::AdapterSave);
    assert!(app.ran("AdapterSave(AdapterUpdate { platform: None, transports: [(Ble, false)]"));
    app.done(
        "AdapterSave",
        Err(Error::code(
            ErrorCode::StorageFailed,
            Some("adapter settings"),
        )),
    );
    // A failed save keeps the staged change and says why.
    assert_eq!(
        app.m.adapter_error(&app.m.state().unwrap()),
        Some("The adapter couldn't read or write its saved data")
    );
    let screen = app.screen();
    assert!(
        screen.contains("✕ The adapter couldn't read or write"),
        "{screen}"
    );
    assert!(screen.contains("✎ Changed"), "{screen}");
    assert!(
        app.m
            .logs
            .iter()
            .any(|l| l.text.starts_with("Couldn't save the adapter settings: "))
    );
    app.click(Action::AdapterSave);
    app.done(
        "AdapterSave",
        Ok(Outcome::AdapterSaved(p::Status::default())),
    );
    assert!(app.m.adapter_err.is_none());
    assert!(app.m.adapter_drafts.is_empty());
    assert!(
        app.m
            .logs
            .iter()
            .any(|l| l.text == "Saved the adapter settings")
    );

    // A transport without its enabled field is enabled.
    app.edit(|st| st.status.transports[0].enabled = None);
    assert!(app.reachable(&Action::Transport(Transport::Classic, false)));
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
    app.click(Action::Select(Item::Device(2)));
    let screen = app.screen();
    assert!(
        screen.contains("Bluetooth Classic is disabled. Enable it in the"),
        "{screen}"
    );
    assert!(!app.reachable(&Action::Connect), "{screen}");
    app.press(KeyCode::Char('c'));
    assert!(!app.ran("Connect"));

    app.click(Action::Select(Item::Candidate(1)));
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
            id: 3,
            transport: Transport::Ble as i32,
            name: String::new(),
            kinds: Vec::new(),
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
            target: 2,
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

fn profile(id: u32, name: &str, roles: &[p::Role]) -> p::Profile {
    p::Profile {
        id,
        name: name.into(),
        roles: roles.iter().map(|r| *r as i32).collect(),
    }
}

/// The fixture adapter with profiles Alpha, Beta and Gamma, two layers at most, and VIA and
/// Vial, which conflict, both off without a profile.
fn with_profiles(st: &mut State) {
    fixture::with_profiles(&mut st.status);
    st.status.profile_support.as_mut().unwrap().max_layers = 2;
    for p in [
        profile(1, "Alpha", &[p::Role::Keyboard]),
        profile(2, "Beta", &[p::Role::Keyboard, p::Role::ConsumerControl]),
        profile(3, "Gamma", &[p::Role::Mouse]),
    ] {
        st.profiles.insert(p.id, p);
    }
}

/// Changes one of the fixture adapter's configuration interfaces.
fn interface(
    app: &mut App,
    i: ConfigurationInterface,
    f: impl FnOnce(&mut p::ConfigurationInterfaceSupport),
) {
    app.edit(|st| {
        f(st.status
            .configuration_interfaces
            .iter_mut()
            .find(|s| s.interface == i as i32)
            .unwrap())
    });
}

impl App {
    /// Answers the page read a profile dialog started with these known profiles; `next` is the
    /// last ID when more pages follow, or 0 when the page ends the listing.
    fn page(&mut self, ids: &[u32], next: u32) {
        let after = self
            .m
            .jobs
            .values()
            .find_map(|j| match &j.command {
                Command::Profiles { after } => Some(*after),
                _ => None,
            })
            .unwrap();
        let entries = {
            let st = self.fake.state.borrow();
            let st = st.as_ref().unwrap();
            ids.iter()
                .map(|id| p::ProfileListEntry {
                    entry: Some(p::profile_list_entry::Entry::Profile(
                        st.profiles[id].clone(),
                    )),
                })
                .collect()
        };
        self.done(
            "Profiles {",
            Ok(Outcome::Profiles {
                after,
                list: p::ProfileList {
                    entries,
                    end: next == 0,
                },
            }),
        );
    }
    fn ctrl_s(&mut self) {
        self.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    }
    fn page_reads(&self) -> usize {
        self.calls()
            .iter()
            .filter(|c| matches!(c, Call::Run(r) if r.starts_with("Profiles {")))
            .count()
    }
    fn open_profiles(&mut self) {
        self.click(Action::Select(Item::Adapter));
        self.click(Action::Profiles);
    }
}

#[test]
fn profiles_are_offered_only_by_adapters_with_them() {
    let mut app = App::connected(100, 44);
    app.click(Action::Select(Item::Adapter));
    assert!(!app.reachable(&Action::Profiles));
    app.click(Action::Select(Item::Device(1)));
    app.edit(|st| st.devices[0].profiles = Some(p::ProfileLayers::default()));
    assert!(!app.reachable(&Action::DeviceProfiles));
    assert!(!app.screen().contains("Profiles"));
    app.edit(with_profiles);
    // A device's Profiles view sits between its Settings and Diagnostics.
    let screen = app.screen();
    assert!(
        screen.contains("[Settings…] [Profiles…]") && screen.contains("[Diagnostics…]"),
        "{screen}"
    );
    // A device that reports no layer list has none.
    app.edit(|st| st.devices[0].profiles = None);
    assert!(!app.reachable(&Action::DeviceProfiles));
    app.click(Action::Select(Item::Adapter));
    assert!(app.reachable(&Action::Profiles));
}

#[test]
fn profiles_show_their_roles_and_are_created_copied_and_deleted() {
    let mut app = App::connected(100, 50);
    app.edit(with_profiles);
    app.open_profiles();
    assert!(app.ran("Profiles { after: 0 }"));
    assert!(app.screen().contains("Loading…"));
    app.page(&[1, 2], 0);
    let screen = app.screen();
    assert!(screen.contains("Alpha Keyboard"), "{screen}");
    assert!(screen.contains("Beta Keyboard, Media Keys"), "{screen}");
    assert!(screen.contains("Configuration Interfaces"), "{screen}");
    // Rules are never read.
    assert!(!app.ran("Rules"));

    // A profile in use isn't deleted.
    app.edit(|st| {
        st.devices[0].profiles = Some(p::ProfileLayers { profiles: vec![1] });
    });
    app.click(Action::ProfileDelete(1));
    assert_eq!(
        app.m.profile_err,
        "Keyboard is using this profile. Remove it from Keyboard's profiles first."
    );
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
    interface(&mut app, ConfigurationInterface::Vial, |s| s.profile = 2);
    app.click(Action::ProfileDelete(2));
    assert!(app.m.profile_err.starts_with("Vial is using this profile"));
    interface(&mut app, ConfigurationInterface::Vial, |s| s.profile = 0);
    app.click(Action::ProfileDelete(2));
    app.press(KeyCode::Char('y'));
    assert!(app.ran("ProfileDelete(Id(2))"));
    app.done(
        "ProfileDelete",
        Ok(Outcome::ProfileDeleted(p::Profile::default())),
    );

    // New starts empty; Copy from the source's name.
    app.click(Action::ProfileNew);
    assert_eq!(app.m.form.value(), "");
    app.press(KeyCode::Enter);
    assert!(!app.ran("ProfileCreate"));
    assert!(!app.m.form_err.is_empty());
    app.m.form.set_value("Delta");
    app.press(KeyCode::Enter);
    assert!(app.ran("ProfileCreate(\"Delta\")"));
    app.done(
        "ProfileCreate",
        Err(Error::code(ErrorCode::NoCapacity, Some("profile create"))),
    );
    assert_eq!(app.m.dialog, Some(Dialog::ProfileName(None)));
    assert!(!app.m.form_err.is_empty());
    app.click(Action::CancelDialog);
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
    app.click(Action::ProfileCopy(1));
    assert_eq!(app.m.form.value(), "Alpha Copy");
    app.click(Action::SaveProfileName);
    assert!(app.ran("ProfileCopy(Id(1), \"Alpha Copy\")"));
    app.done("ProfileCopy", Ok(Outcome::Profile(p::Profile::default())));
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
}

#[test]
fn profile_events_keep_the_page_current() {
    let mut app = App::connected(100, 50);
    app.edit(with_profiles);
    app.open_profiles();
    app.page(&[1, 2], 0);
    let event = |kind| Notice::Event {
        event: p::Event { kind: Some(kind) },
        first: false,
        changed: Vec::new(),
    };
    // A profile changed elsewhere replaces its entry; a new one in the page's range re-reads
    // the page; a removed one leaves it.
    let reads = app.page_reads();
    app.notice(event(p::event::Kind::Profile(profile(
        2,
        "Bravo",
        &[p::Role::Mouse],
    ))));
    assert_eq!(app.page_reads(), reads);
    assert!(app.screen().contains("Bravo Mouse"));
    app.notice(event(p::event::Kind::Profile(profile(9, "Copy", &[]))));
    assert_eq!(app.page_reads(), reads + 1);
    app.page(&[2, 3], 0);
    app.notice(event(p::event::Kind::ProfileRemoved(p::ProfileRemoved {
        id: 2,
    })));
    let screen = app.screen();
    assert!(
        screen.contains("Gamma") && !screen.contains("Beta"),
        "{screen}"
    );
    assert_eq!(app.page_reads(), reads + 1);
    // A removal that empties the page reads it again. A change while that read is under way
    // starts no other read; rule changes start none at all.
    app.notice(event(p::event::Kind::ProfileRemoved(p::ProfileRemoved {
        id: 3,
    })));
    assert!(app.m.profile_page.list.is_empty());
    assert_eq!(app.page_reads(), reads + 2);
    app.notice(event(p::event::Kind::Profile(profile(1, "Alpha", &[]))));
    app.notice(event(p::event::Kind::ProfileRulesChanged(
        p::ProfileRulesChanged {
            profile: 1,
            ..Default::default()
        },
    )));
    assert_eq!(app.page_reads(), reads + 2);
}

#[test]
fn a_last_page_left_empty_shows_the_page_before() {
    let mut app = App::connected(100, 50);
    app.edit(with_profiles);
    app.open_profiles();
    app.page(&[1], 1);
    app.click(Action::ProfilePage(true));
    app.page(&[2], 2);
    app.click(Action::ProfilePage(true));
    app.page(&[3], 0);
    assert_eq!(app.m.profile_page.back, [0, 1]);
    let event = |kind| Notice::Event {
        event: p::Event { kind: Some(kind) },
        first: false,
        changed: Vec::new(),
    };
    app.fake.calls.borrow_mut().clear();
    // Deleting the only profile of the last page reads it again, finds it empty and goes back.
    app.notice(event(p::event::Kind::ProfileRemoved(p::ProfileRemoved {
        id: 3,
    })));
    assert!(app.ran("Profiles { after: 2 }"));
    app.page(&[], 0);
    assert!(app.ran("Profiles { after: 1 }"));
    app.page(&[2], 0);
    assert_eq!(app.m.profile_page.after, 1);
    assert_eq!(app.m.profile_page.back, [0]);
    let screen = app.screen();
    assert!(screen.contains("Beta"), "{screen}");
    assert!(app.reachable(&Action::ProfilePage(false)));
    assert!(!app.reachable(&Action::ProfilePage(true)));
    // When the page before has emptied too, it goes back again.
    app.notice(event(p::event::Kind::ProfileRemoved(p::ProfileRemoved {
        id: 2,
    })));
    app.page(&[], 0);
    app.page(&[1], 0);
    assert!(app.m.profile_page.back.is_empty());
    assert!(app.screen().contains("Alpha"));
    assert!(!app.reachable(&Action::ProfilePage(false)));
}

#[test]
fn profile_pages_go_forward_and_back() {
    let mut app = App::connected(100, 50);
    app.edit(with_profiles);
    app.open_profiles();
    app.page(&[1], 1);
    assert!(app.reachable(&Action::ProfilePage(true)));
    assert!(!app.reachable(&Action::ProfilePage(false)));

    // A failed read keeps the shown page and its cursors.
    app.click(Action::ProfilePage(true));
    assert!(app.ran("Profiles { after: 1 }"));
    app.done(
        "Profiles {",
        Err(Error::code(ErrorCode::Timeout, Some("profile list"))),
    );
    assert_eq!(app.m.profile_page.after, 0);
    assert!(app.m.profile_page.back.is_empty());
    assert!(app.screen().contains("Alpha"));
    // Retry reads the shown page again.
    app.fake.calls.borrow_mut().clear();
    app.click(Action::ProfilesRetry);
    assert!(app.ran("Profiles { after: 0 }"));
    app.page(&[1], 1);
    assert!(!app.reachable(&Action::ProfilesRetry));

    app.click(Action::ProfilePage(true));
    app.page(&[2], 0);
    let screen = app.screen();
    assert!(
        screen.contains("Beta") && !screen.contains("Alpha"),
        "{screen}"
    );
    assert_eq!(app.m.profile_page.after, 1);
    assert_eq!(app.m.profile_page.back, [0]);
    assert!(!app.reachable(&Action::ProfilePage(true)));
    app.fake.calls.borrow_mut().clear();
    app.click(Action::ProfilePage(false));
    assert!(app.ran("Profiles { after: 0 }"));
    app.page(&[1], 1);
    assert!(app.m.profile_page.back.is_empty());
    assert!(!app.reachable(&Action::ProfilePage(false)));
}

#[test]
fn interfaces_stage_their_switch_and_profile_and_confirm_a_usb_reconnect() {
    let mut app = App::connected(130, 60);
    app.edit(with_profiles);
    app.open_profiles();
    app.page(&[1, 2, 3], 0);
    let via = ConfigurationInterface::Via;
    let vial = ConfigurationInterface::Vial;
    // An interface needs a profile; Save waits for one and says so.
    app.click(Action::InterfaceEnabled(via, true));
    let screen = app.screen();
    assert!(
        screen.contains("Choose a profile for VIA before turning it on."),
        "{screen}"
    );

    assert!(!app.reachable(&Action::AdapterSave));
    app.click(Action::InterfaceChoose(via));
    assert_eq!(
        app.m.dialog,
        Some(Dialog::ProfilePick(PickFor::Interface(via)))
    );
    app.page(&[1, 2], 0);
    // An enabled interface always has a profile, so None isn't offered.
    assert!(!app.reachable(&Action::PickProfile(0)));
    app.click(Action::PickProfile(2));
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
    let screen = app.screen();
    assert!(screen.contains("Profile Beta"), "{screen}");
    assert!(screen.contains("✎ Changed"), "{screen}");
    // Turning on a conflicting interface turns the other off.
    app.click(Action::InterfaceEnabled(vial, true));
    let draft = app.m.adapter_draft(&app.m.state().unwrap());
    assert_eq!(
        draft.interfaces,
        [
            InterfaceUpdate {
                interface: via,
                enabled: None,
                profile: Some(2),
            },
            InterfaceUpdate {
                interface: vial,
                enabled: Some(true),
                profile: None,
            }
        ]
    );
    app.click(Action::InterfaceEnabled(via, true));
    // Plain s never saves; Ctrl+S presses the Save shown, which asks first because USB
    // reconnects.
    app.press(KeyCode::Char('s'));
    assert!(!app.ran("AdapterSave"));
    app.ctrl_s();
    assert_eq!(app.m.dialog, Some(Dialog::SaveAdapter));
    assert!(app.screen().contains("USB Reconnect Required"));
    assert!(!app.ran("AdapterSave"));
    app.click(Action::Confirm);
    assert!(
        app.ran("interfaces: [InterfaceUpdate { interface: Via, enabled: Some(true), profile: Some(2) }]"),
        "{:?}",
        app.calls()
    );
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
    app.done(
        "AdapterSave",
        Ok(Outcome::AdapterSaved(p::Status::default())),
    );
    assert!(app.m.adapter_drafts.is_empty());

    // With VIA on, changing its profile reconnects USB too, so it asks first.
    interface(&mut app, via, |s| {
        s.enabled = true;
        s.profile = 2;
    });
    app.fake.calls.borrow_mut().clear();
    app.click(Action::InterfaceChoose(via));
    app.page(&[1, 2], 0);
    app.click(Action::PickProfile(1));
    app.click(Action::AdapterSave);
    assert_eq!(app.m.dialog, Some(Dialog::SaveAdapter));
    assert!(app.screen().contains("disconnect from this computer"));
    app.press(KeyCode::Char('n'));
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
    assert!(!app.ran("AdapterSave"));

    // A disabled interface's profile changes without asking, and may be cleared.
    app.click(Action::AdapterDiscard);
    app.click(Action::InterfaceChoose(vial));
    app.page(&[1, 2], 0);
    app.click(Action::PickProfile(0));
    app.click(Action::InterfaceChoose(vial));
    app.page(&[1, 2, 3], 0);
    app.click(Action::PickProfile(3));
    app.click(Action::AdapterSave);
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
    assert!(
        app.ran(
            "interfaces: [InterfaceUpdate { interface: Vial, enabled: None, profile: Some(3) }]"
        )
    );
}

/// Saves VIA on with profile Beta from the adapter's Profiles view, then drops the connection as
/// the adapter does when it reconnects USB.
fn save_and_lose_usb(app: &mut App) {
    let via = ConfigurationInterface::Via;
    app.edit(|st| {
        with_profiles(st);
        st.status.id = "0123456789ABCDEF".into();
    });
    app.open_profiles();
    app.page(&[1, 2, 3], 0);
    app.click(Action::InterfaceEnabled(via, true));
    app.click(Action::InterfaceChoose(via));
    app.page(&[1, 2], 0);
    app.click(Action::PickProfile(2));
    app.click(Action::AdapterSave);
    app.click(Action::Confirm);
    interface(app, via, |s| {
        s.enabled = true;
        s.profile = 2;
    });
    let saved = app.m.state().unwrap().status;
    app.done("AdapterSave", Ok(Outcome::AdapterSaved(saved)));
    app.edit(|st| st.available = false);
    app.fake.calls.borrow_mut().clear();
    let session = app.m.session.unwrap();
    app.phase(session, Phase::Lost(Error::new("unplugged")));
}

#[test]
fn a_save_that_reconnects_usb_reopens_the_adapter_when_it_returns() {
    let mut app = App::connected(100, 60);
    save_and_lose_usb(&mut app);
    // The expected loss isn't reported; the adapter is looked for instead.
    assert_eq!(app.calls(), [Call::List]);
    let screen = app.screen();
    assert!(
        screen.contains("Connecting") && !screen.contains("Lost the adapter connection"),
        "{screen}"
    );
    // A listing without it looks again shortly.
    app.m.update(Msg::Ports(Ok(Vec::new())));
    assert_eq!(app.calls(), [Call::List]);
    app.m.reconnect.as_mut().unwrap().hurry();
    app.m.tick();
    assert_eq!(app.calls(), [Call::List, Call::List]);
    // It returns on another port, its serial number now ending in a Vial suffix.
    app.m.update(Msg::Ports(Ok(vec![PortInfo {
        port: "/dev/ttyACM1".into(),
        serial: "0123456789abcdef-vial:f64c2b3c".into(),
    }])));
    assert_eq!(app.calls().last(), Some(&Call::Open("/dev/ttyACM1".into())));
    // The port may not open at first.
    let session = app.fake.next.get();
    app.phase(
        session,
        Phase::Failed {
            error: Error::new("busy"),
            open: false,
        },
    );
    assert_eq!(app.m.gate(), Some("connecting"));
    assert!(!app.m.chooser);
    app.m.reconnect.as_mut().unwrap().hurry();
    app.m.tick();
    assert_eq!(app.calls().last(), Some(&Call::List));
    app.m.update(Msg::Ports(Ok(vec![PortInfo {
        port: "/dev/ttyACM1".into(),
        serial: "0123456789ABCDEF-vial:f64c2b3c".into(),
    }])));
    let session = app.fake.next.get();
    app.edit(|st| {
        st.session = session;
        st.available = true;
    });
    app.phase(session, Phase::Opened);
    app.phase(session, Phase::Ready);
    // Where the save was made is shown again, with its page read from the reopened session.
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
    assert!(app.ran("Profiles { after: 0 }"), "{:?}", app.calls());
    assert_eq!(app.m.selected, Some(Item::Adapter));
    assert!(app.m.reconnect.is_none());
    let screen = app.screen();
    assert!(
        screen.contains("Connected to /dev/ttyACM1") && !screen.contains("Lost the adapter"),
        "{screen}"
    );
}

#[test]
fn an_adapter_that_doesnt_return_is_reported_lost() {
    let mut app = App::connected(100, 60);
    save_and_lose_usb(&mut app);
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM9")])));
    app.m.reconnect.as_mut().unwrap().expire();
    app.m.tick();
    assert!(app.m.reconnect.is_none());
    assert_eq!(
        (app.m.dialog.clone(), app.m.profiles_open),
        (None, Some(Item::Adapter))
    );
    let screen = app.screen();
    assert!(
        screen.contains("Lost the adapter connection: unplugged")
            && screen.contains("Adapter Lost"),
        "{screen}"
    );
}

#[test]
fn only_a_save_that_changes_the_interfaces_expects_a_usb_reconnect() {
    let mut app = App::connected(100, 60);
    app.edit(with_profiles);
    app.open_profiles();
    app.page(&[1, 2, 3], 0);
    let via = ConfigurationInterface::Via;
    app.click(Action::InterfaceEnabled(via, true));
    app.click(Action::InterfaceChoose(via));
    app.page(&[1, 2], 0);
    app.click(Action::PickProfile(2));
    app.click(Action::AdapterSave);
    app.click(Action::Confirm);
    // Refused while connected: nothing is stored, so no reconnect follows.
    app.done(
        "AdapterSave",
        Err(Error::code(ErrorCode::Busy, Some("adapter save"))),
    );
    assert!(app.m.reconnect.is_none());
    let session = app.m.session.unwrap();
    app.edit(|st| st.available = false);
    app.phase(session, Phase::Lost(Error::new("unplugged")));
    assert!(app.screen().contains("Lost the adapter connection"));
}

#[test]
fn a_device_orders_its_layers_in_its_profiles_view_and_saves_them_on_their_own() {
    let mut app = App::connected(120, 50);
    app.edit(with_profiles);
    app.edit(|st| st.devices[0].profiles = Some(p::ProfileLayers { profiles: vec![1] }));
    app.click(Action::Select(Item::Device(1)));
    // The layers are in the device's Profiles view, not its details.
    let screen = app.screen();
    assert!(!screen.contains("1. Alpha"), "{screen}");
    app.click(Action::DeviceProfiles);
    let screen = app.screen();
    assert!(
        screen.contains("Profiles · Keyboard") && screen.contains("1. Alpha"),
        "{screen}"
    );
    app.click(Action::LayerAdd);
    assert_eq!(app.m.dialog, Some(Dialog::ProfilePick(PickFor::Layer)));
    assert!(!app.reachable(&Action::PickProfile(0)));
    app.page(&[2, 3], 0);
    app.click(Action::PickProfile(3));
    assert_eq!(app.m.dialog, None);
    let screen = app.screen();
    assert!(
        screen.contains("1. Alpha") && screen.contains("2. Gamma Mouse"),
        "{screen}"
    );
    assert!(screen.contains("✎ Changed"), "{screen}");
    // Two layers fill the adapter's limit, so Add Profile is unavailable.
    assert!(!app.reachable(&Action::LayerAdd));
    app.click(Action::LayerUp(1));
    let screen = app.screen();
    assert!(
        screen.contains("1. Gamma") && screen.contains("2. Alpha"),
        "{screen}"
    );
    // A details change stays out of the layers' Save, and the layers out of the details'.
    app.click(Action::ProfilesBack);
    assert!(!app.screen().contains("1. Gamma"));
    app.click(Action::Hidpp(true));
    app.ctrl_s();
    assert!(
        app.ran("DeviceSave(1, DeviceUpdate { enabled: None, trusted: None, blocked: None, hidpp: Some(true), layers: None })"),
        "{:?}",
        app.calls()
    );
    let saved = |app: &mut App| {
        app.done(
            "DeviceSave",
            Ok(Outcome::Device {
                subject: crate::controller::Subject {
                    id: 1,
                    name: "Keyboard".into(),
                },
                device: p::Device::default(),
            }),
        )
    };
    saved(&mut app);
    app.click(Action::DeviceProfiles);
    app.fake.calls.borrow_mut().clear();
    app.ctrl_s();
    assert!(
        app.ran("DeviceSave(1, DeviceUpdate { enabled: None, trusted: None, blocked: None, hidpp: None, layers: Some([3, 1]) })"),
        "{:?}",
        app.calls()
    );
    // A failed layers save is shown in the Profiles view, not the details.
    app.done(
        "DeviceSave",
        Err(Error::code(ErrorCode::Busy, Some("device set"))),
    );
    assert!(app.screen().contains("✕ Couldn't Save"));
    app.click(Action::ProfilesBack);
    assert!(!app.screen().contains("✕ Couldn't Save"));
    // Discarding the layers keeps the view, and an empty list is saved like any other.
    app.click(Action::DeviceProfiles);
    app.click(Action::LayersDiscard);
    assert!(app.screen().contains("1. Alpha") && !app.screen().contains("2."));
    app.click(Action::LayerRemove(0));
    app.click(Action::LayersSave);
    assert!(app.ran("layers: Some([]) })"), "{:?}", app.calls());
}

#[test]
fn diagnostics_show_why_a_devices_profiles_are_not_loaded() {
    let mut app = App::connected(120, 60);
    app.edit(with_profiles);
    app.edit(|st| st.devices[0].profile_error = Some(ErrorCode::NoCapacity as i32));
    app.click(Action::Select(Item::Device(1)));
    assert!(!app.screen().contains("Not Loaded"));
    app.click(Action::Diagnostics);
    // The desktop's words: a Profiles section whose Status is Not Loaded, with the reason.
    let screen = app.screen();
    let status = screen
        .lines()
        .position(|l| l.contains("Status") && l.contains("Not Loaded"))
        .unwrap_or_else(|| panic!("{screen}"));
    let lines: Vec<&str> = screen.lines().collect();
    assert!(lines[status - 1].contains("Profiles"), "{screen}");
    assert!(
        lines[status + 1].contains("The adapter doesn't have room for"),
        "{screen}"
    );
    // A profile that couldn't be read is still Not Loaded, with its own reason.
    app.edit(|st| st.devices[0].profile_error = Some(ErrorCode::StorageFailed as i32));
    let screen = app.screen();
    assert!(
        screen.contains("Status") && screen.contains("Not Loaded"),
        "{screen}"
    );
    assert!(!screen.contains("Couldn't Read"), "{screen}");
    assert!(screen.contains("couldn't read one of"), "{screen}");
    assert!(!screen.contains("no_capacity"), "{screen}");
}

#[test]
fn the_adapters_pane_lists_the_adapter_and_its_actions() {
    let mut app = App::connected(120, 50);
    let screen = app.screen();
    assert!(!screen.contains("Adapter ▾"), "{screen}");
    assert!(
        screen.contains("Adapters") && screen.contains("● Ready"),
        "{screen}"
    );
    for action in [
        Action::AdapterDisconnect,
        Action::Adapters,
        Action::Refresh,
        Action::FilesOpen,
        Action::Bootloader,
    ] {
        assert!(app.reachable(&action), "{action:?}");
    }
    // Selecting the adapter shows its page in place of a device's details.
    app.click(Action::Select(Item::Adapter));
    let screen = app.screen();
    assert!(screen.contains("▌ Desk"), "{screen}");
    assert!(
        screen.contains("Platform") && screen.contains("Bluetooth LE"),
        "{screen}"
    );
    assert!(app.reachable(&Action::Rename));
    assert!(!app.reachable(&Action::Profiles));
    // Staged choices save with Ctrl+S from the page.
    app.click(Action::Platform(Platform::Mac));
    app.ctrl_s();
    assert!(app.ran("AdapterSave(AdapterUpdate { platform: Some(Mac)"));
    // Selecting a device shows its details again.
    app.click(Action::Select(Item::Device(1)));
    assert!(!app.screen().contains("Platform"));
}

#[test]
fn keys_select_the_adapter() {
    let mut app = App::connected(120, 50);
    app.press(KeyCode::Char('a'));
    assert_eq!(app.m.selected, Some(Item::Adapter));
    app.press(KeyCode::Up);
    assert_eq!(app.m.selected, Some(Item::Candidate(2)));
    app.press(KeyCode::Down);
    assert_eq!(app.m.selected, Some(Item::Adapter));
    // From a device's settings page, a goes back to the list with the adapter selected.
    app.click(Action::Select(Item::Device(1)));
    app.press(KeyCode::Char('o'));
    assert_eq!(app.m.page.device, 1);
    app.press(KeyCode::Char('a'));
    assert_eq!(app.m.page.device, 0);
    assert_eq!(app.m.selected, Some(Item::Adapter));
}

#[test]
fn disconnect_ends_the_session_and_shows_the_adapters() {
    let mut app = App::connected(120, 50);
    app.click(Action::AdapterDisconnect);
    assert_eq!(app.calls(), [Call::Close, Call::List]);
    assert!(app.m.session.is_none());
    let screen = app.screen();
    assert!(screen.contains("Choose Adapter"), "{screen}");
    // Another adapter opens as usual.
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
    app.click(Action::Port("/dev/ttyACM0".into()));
    assert_eq!(app.calls().last(), Some(&Call::Open("/dev/ttyACM0".into())));

    // Quitting after a disconnect needs no further close.
    let mut app = App::connected(120, 50);
    app.click(Action::AdapterDisconnect);
    app.press(KeyCode::Char('q'));
    assert!(app.m.done);
    assert_eq!(app.calls(), [Call::Close, Call::List]);
}

#[test]
fn the_adapter_profiles_view_shows_memory_and_unreadable_profiles_and_goes_back() {
    let mut app = App::connected(130, 50);
    app.edit(with_profiles);
    app.edit(|st| {
        let support = st.status.profile_support.as_mut().unwrap();
        support.memory_budget = 8192;
        support.memory_used = 7168;
    });
    app.click(Action::Select(Item::Adapter));
    app.click(Action::Profiles);
    app.done(
        "Profiles {",
        Ok(Outcome::Profiles {
            after: 0,
            list: p::ProfileList {
                entries: vec![
                    p::ProfileListEntry {
                        entry: Some(p::profile_list_entry::Entry::Profile(profile(
                            1,
                            "Alpha",
                            &[p::Role::Keyboard],
                        ))),
                    },
                    p::ProfileListEntry {
                        entry: Some(p::profile_list_entry::Entry::Unreadable(4)),
                    },
                ],
                end: true,
            },
        }),
    );
    let screen = app.screen();
    assert!(screen.contains("Profile Memory"), "{screen}");
    assert!(screen.contains("In Use    88%"), "{screen}");
    assert!(screen.contains("Alpha Keyboard"), "{screen}");
    assert!(screen.contains("Profile 4 Couldn't Read"), "{screen}");
    // Removing the unreadable profile empties nothing while Alpha stays.
    let reads = app.page_reads();
    app.notice(Notice::Event {
        event: p::Event {
            kind: Some(p::event::Kind::ProfileRemoved(p::ProfileRemoved { id: 4 })),
        },
        first: false,
        changed: Vec::new(),
    });
    assert!(!app.screen().contains("Couldn't Read"));
    assert_eq!(app.page_reads(), reads);
    // Back returns to the adapter's page, and so does Esc.
    app.click(Action::ProfilesBack);
    assert!(app.screen().contains("Platform"));
    app.click(Action::Profiles);
    app.page(&[1], 0);
    app.press(KeyCode::Esc);
    assert_eq!(app.m.profiles_open, None);
    assert!(app.screen().contains("Platform"));
}

#[test]
fn the_profile_chooser_pages_apart_from_the_profiles_view() {
    let mut app = App::connected(130, 50);
    app.edit(with_profiles);
    app.open_profiles();
    app.page(&[1], 1);
    app.click(Action::ProfilePage(true));
    app.page(&[2], 2);
    assert_eq!(app.m.profile_page.after, 1);
    let via = ConfigurationInterface::Via;
    app.click(Action::InterfaceChoose(via));
    assert!(app.ran("Profiles { after: 0 }"));
    app.page(&[1], 1);
    app.click(Action::ProfilePage(true));
    app.page(&[2], 2);
    assert_eq!(app.m.picker_page.after, 1);
    app.click(Action::PickProfile(2));
    // The Profiles view keeps its own page.
    assert_eq!(app.m.profile_page.after, 1);
    assert_eq!(app.m.profile_page.back, [0]);
    let screen = app.screen();
    assert!(screen.contains("Beta"), "{screen}");
}

#[test]
fn the_device_list_hides_an_empty_saved_section() {
    let mut app = App::connected(100, 30);
    let header = |screen: &str, name: &str| {
        screen
            .lines()
            .any(|l| l.split('│').any(|cell| cell.trim() == name))
    };
    let screen = app.screen();
    assert!(
        header(&screen, "Saved") && header(&screen, "Nearby"),
        "{screen}"
    );
    assert!(screen.contains("Saved · "), "{screen}");
    app.edit(|st| st.devices.clear());
    let screen = app.screen();
    assert!(
        !header(&screen, "Saved") && header(&screen, "Nearby"),
        "{screen}"
    );
    assert!(!screen.contains("none"), "{screen}");
}

impl App {
    /// Scrolls the list column with its border arrows until `action` is drawn. The arrows are
    /// controls, which Tab reaches like any other.
    fn scroll_to(&mut self, action: &Action) -> bool {
        self.m.device_scroll = 0;
        self.m.adapter_scroll = 0;
        for _ in 0..30 {
            if self.reachable(action) {
                return true;
            }
            let Some(arrow) = self
                .m
                .hits
                .iter()
                .map(|h| h.action.clone())
                .find(|a| matches!(a, Action::Scroll(Area::Adapters | Area::Devices, false)))
            else {
                return false;
            };
            self.click(arrow);
        }
        false
    }
}

#[test]
fn the_adapters_actions_stay_reachable_and_the_column_fits_at_every_size() {
    let actions = [
        Action::AdapterDisconnect,
        Action::Adapters,
        Action::Refresh,
        Action::FilesOpen,
        Action::Bootloader,
    ];
    let sizes = [
        (60, 24),
        (80, 20),
        (100, 14),
        (40, 17),
        (40, 12),
        (40, 10),
        (95, 12),
        (95, 11),
        (95, 10),
    ];
    for (w, h) in sizes {
        for selected in [None, Some(Item::Device(1)), Some(Item::Adapter)] {
            let case = format!("{w}x{h} {selected:?}");
            let mut app = App::connected(w, h);
            app.m.selected = selected;
            // The panes end on the line above the footer; none is pushed under it.
            let lines = app.render();
            assert!(lines[h - 2].ends_with('╯'), "{case}:\n{}", lines.join("\n"));
            assert!(lines[h - 1].contains("q quit") || lines[h - 1].contains("? hel"));
            assert!(app.m.hits.iter().all(|hit| hit.y < h - 1), "{case}");
            for action in &actions {
                assert!(
                    app.scroll_to(action),
                    "{case}: {action:?} unreachable:\n{}",
                    app.screen()
                );
            }
        }
    }
}

#[test]
fn each_result_shows_only_beside_the_action_that_produced_it() {
    let mut app = App::connected(130, 60);
    app.edit(with_profiles);
    app.edit(|st| st.devices[0].profiles = Some(p::ProfileLayers { profiles: vec![1] }));
    app.open_profiles();
    app.page(&[1, 2], 0);
    // A profile in use isn't deleted; the reason stays in the Profiles view.
    app.click(Action::ProfileDelete(1));
    assert!(app.screen().contains("✕ Keyboard is using this profile"));
    app.click(Action::Select(Item::Device(1)));
    app.click(Action::Select(Item::Adapter));
    let screen = app.screen();
    assert!(!screen.contains("is using this profile"), "{screen}");
    app.click(Action::Profiles);
    app.page(&[1, 2], 0);
    let screen = app.screen();
    assert!(!screen.contains("is using this profile"), "{screen}");
    // A failed delete shows there too, and closes with the view.
    app.click(Action::ProfileDelete(2));
    app.press(KeyCode::Char('y'));
    app.done(
        "ProfileDelete",
        Err(Error::code(ErrorCode::Busy, Some("profile delete"))),
    );
    assert!(app.screen().contains("✕ The adapter is busy"));
    app.click(Action::ProfilesBack);
    let screen = app.screen();
    assert!(!screen.contains("✕"), "{screen}");

    // An adapter's failed save shows only for that adapter, and not after reopening.
    app.click(Action::Platform(Platform::Mac));
    app.click(Action::AdapterSave);
    app.done(
        "AdapterSave",
        Err(Error::code(ErrorCode::Busy, Some("adapter settings"))),
    );
    assert!(app.screen().contains("✕ The adapter is busy"));
    let id = app.m.state().unwrap().status.id;
    app.edit(|st| st.status.id = "OTHER".into());
    let screen = app.screen();
    assert!(!screen.contains("✕"), "{screen}");
    app.edit(|st| st.status.id = id);
    assert!(app.screen().contains("✕ The adapter is busy"));
    app.click(Action::AdapterDisconnect);
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
    app.click(Action::Port("/dev/ttyACM0".into()));
    let session = app.fake.next.get();
    app.edit(|st| st.session = session);
    app.phase(session, Phase::Opened);
    app.phase(session, Phase::Ready);
    app.click(Action::Select(Item::Adapter));
    let screen = app.screen();
    assert!(!screen.contains("✕"), "{screen}");
}

#[test]
fn details_and_layers_keep_their_own_save_failures() {
    let mut app = App::connected(120, 50);
    app.edit(with_profiles);
    app.edit(|st| {
        st.devices[0].profiles = Some(p::ProfileLayers {
            profiles: vec![1, 2],
        })
    });
    app.click(Action::Select(Item::Device(1)));
    app.click(Action::DeviceProfiles);
    app.click(Action::LayerUp(1));
    app.click(Action::LayersSave);
    app.done(
        "DeviceSave",
        Err(Error::code(ErrorCode::Busy, Some("device set"))),
    );
    assert!(app.screen().contains("✕ Couldn't Save"));
    // Staging a Details switch leaves the layers' failure in place.
    app.click(Action::ProfilesBack);
    app.click(Action::Hidpp(true));
    app.click(Action::DeviceProfiles);
    assert!(app.screen().contains("✕ Couldn't Save"));
    // A Details failure isn't cleared by staging layers either.
    app.click(Action::ProfilesBack);
    app.click(Action::DeviceSave);
    app.done(
        "DeviceSave",
        Err(Error::code(ErrorCode::Busy, Some("device set"))),
    );
    assert!(app.screen().contains("✕ Couldn't Save"));
    app.click(Action::DeviceProfiles);
    app.click(Action::LayerUp(1));
    assert!(!app.screen().contains("✕ Couldn't Save"));
    app.click(Action::ProfilesBack);
    assert!(app.screen().contains("✕ Couldn't Save"));
    // Each clears with its own view's actions.
    app.click(Action::DeviceDiscard);
    assert!(!app.screen().contains("✕ Couldn't Save"));
}

#[test]
fn a_profiles_view_takes_only_its_own_keys() {
    let mut app = App::connected(120, 50);
    app.edit(with_profiles);
    app.edit(|st| {
        st.devices[1].profiles = Some(p::ProfileLayers { profiles: vec![1] });
    });
    // A disconnected device, whose details offer Connect on Enter.
    app.click(Action::Select(Item::Device(2)));
    assert!(app.screen().contains("⏎ connect"));
    app.click(Action::DeviceProfiles);
    app.fake.calls.borrow_mut().clear();
    for c in ['e', 't', 'b', 'c', 'd', 'x', 'o', 'i', 'h', 'p', 's', 'r'] {
        app.press(KeyCode::Char(c));
    }
    app.press(KeyCode::Enter);
    app.press(KeyCode::Delete);
    assert!(app.calls().is_empty(), "{:?}", app.calls());
    assert!(app.m.device_drafts.is_empty());
    assert_eq!(app.m.dialog, None);
    assert_eq!(app.m.profiles_open, Some(Item::Device(2)));
    let screen = app.screen();
    let hints = screen.lines().last().unwrap();
    assert!(
        hints.contains("esc back") && !hints.contains("connect"),
        "{hints}"
    );
    // Arrows move the highlight through the view's own controls.
    app.press(KeyCode::Down);
    assert!(
        matches!(
            app.m.focus,
            Some(Action::LayerRemove(0) | Action::LayerAdd | Action::ProfilesBack)
        ),
        "{:?}",
        app.m.focus
    );
    app.press(KeyCode::Esc);
    app.press(KeyCode::Esc);
    assert_eq!(app.m.profiles_open, None);

    // The adapter's Profiles view stays shown under the arrows.
    app.open_profiles();
    app.page(&[1, 2], 0);
    for key in [KeyCode::Up, KeyCode::Up, KeyCode::Down, KeyCode::Char('k')] {
        app.press(key);
        assert_eq!(app.m.profiles_open, Some(Item::Adapter));
        assert_eq!(app.m.selected, Some(Item::Adapter));
    }
    assert!(
        !matches!(app.m.focus, Some(Action::Select(_)) | None),
        "{:?}",
        app.m.focus
    );
    let screen = app.screen();
    let hints = screen.lines().last().unwrap();
    assert!(hints.contains("esc back"), "{hints}");
}

impl App {
    /// Saves VIA on with profile Beta from the adapter's Profiles view, leaving the save running.
    fn save_via(&mut self) {
        let via = ConfigurationInterface::Via;
        self.edit(|st| {
            with_profiles(st);
            st.status.id = "0123456789ABCDEF".into();
        });
        self.open_profiles();
        self.page(&[1, 2, 3], 0);
        self.click(Action::InterfaceEnabled(via, true));
        self.click(Action::InterfaceChoose(via));
        self.page(&[1, 2], 0);
        self.click(Action::PickProfile(2));
        self.click(Action::AdapterSave);
        self.click(Action::Confirm);
        assert!(self.ran("AdapterSave"));
    }
    fn lose_usb(&mut self) {
        self.edit(|st| st.available = false);
        let session = self.m.session.unwrap();
        self.phase(session, Phase::Lost(Error::new("unplugged")));
    }
    /// The adapter returns with VIA on when `stored`, else as it was.
    fn usb_returns(&mut self, stored: bool) {
        self.m.update(Msg::Ports(Ok(vec![PortInfo {
            port: "/dev/ttyACM1".into(),
            serial: "0123456789ABCDEF-via:f64c2b3c".into(),
        }])));
        assert_eq!(
            self.calls().last(),
            Some(&Call::Open("/dev/ttyACM1".into()))
        );
        let session = self.fake.next.get();
        self.edit(|st| {
            st.session = session;
            st.available = true;
        });
        if stored {
            interface(self, ConfigurationInterface::Via, |s| {
                s.enabled = true;
                s.profile = 2;
            });
        }
        self.phase(session, Phase::Opened);
        self.phase(session, Phase::Ready);
        assert!(self.m.reconnect.is_none() && self.m.unsettled.is_none());
    }
    fn logged(&self, text: &str) -> bool {
        self.m.logs.iter().any(|l| l.text.contains(text))
    }
}

#[test]
fn a_save_reply_cut_short_by_its_usb_reconnect_is_not_a_failure() {
    let closed = || Err(Error::new("the connection to the adapter closed"));
    // The loss arrives before the save's reply, and then the other way round.
    for lost_first in [true, false] {
        let mut app = App::connected(100, 60);
        app.save_via();
        if lost_first {
            app.lose_usb();
            app.done("AdapterSave", closed());
        } else {
            app.edit(|st| st.available = false);
            app.done("AdapterSave", closed());
            app.lose_usb();
        }
        assert!(app.m.reconnecting(), "{lost_first}");
        assert!(!app.logged("Couldn't save"), "{lost_first}");
        app.usb_returns(true);
        // The returned adapter shows what was sent, so the save is reported as saved.
        assert!(app.m.adapter_err.is_none(), "{lost_first}");
        assert!(app.m.adapter_drafts.is_empty(), "{lost_first}");
        assert!(!app.logged("Couldn't save"), "{lost_first}");
        assert!(app.logged("Saved the adapter settings"), "{lost_first}");
        let screen = app.screen();
        assert!(!screen.contains("✕"), "{lost_first}: {screen}");
    }
}

#[test]
fn a_cut_short_save_the_returned_adapter_doesnt_show_has_an_unknown_outcome() {
    let closed = || Err(Error::new("the connection to the adapter closed"));
    // Lost first; the reply first while the adapter still seems connected; the reply first
    // after the loss is known.
    for order in 0..3 {
        let mut app = App::connected(100, 60);
        app.save_via();
        match order {
            0 => {
                app.lose_usb();
                app.done("AdapterSave", closed());
            }
            1 => {
                app.done("AdapterSave", closed());
                app.lose_usb();
            }
            _ => {
                app.edit(|st| st.available = false);
                app.done("AdapterSave", closed());
                app.lose_usb();
            }
        }
        app.usb_returns(false);
        let shown = app
            .m
            .adapter_error(&app.m.state().unwrap())
            .unwrap_or_default()
            .to_owned();
        assert_eq!(shown, text::SAVE_UNCONFIRMED, "{order}");
        assert!(app.logged(text::SAVE_UNCONFIRMED_LOG), "{order}");
        assert!(!app.logged("Couldn't save"), "{order}");
        assert!(!app.logged("connection to the adapter closed"), "{order}");
        assert!(!app.m.adapter_drafts.is_empty(), "{order}");
        let screen = app.screen();
        assert!(
            screen.contains("✕ Cordial couldn't confirm"),
            "{order}: {screen}"
        );
    }
}

#[test]
fn a_cut_short_save_is_settled_when_the_returned_adapter_fails_to_start() {
    let mut app = App::connected(100, 60);
    app.save_via();
    app.lose_usb();
    app.done(
        "AdapterSave",
        Err(Error::new("the connection to the adapter closed")),
    );
    // Save stays unavailable while the outcome is unknown.
    assert!(app.m.adapter_saving());
    app.m.update(Msg::Ports(Ok(vec![PortInfo {
        port: "/dev/ttyACM1".into(),
        serial: "0123456789ABCDEF-via:f64c2b3c".into(),
    }])));
    let session = app.fake.next.get();
    app.edit(|st| st.session = session);
    app.phase(session, Phase::Opened);
    assert!(app.m.unsettled.is_some());
    app.phase(
        session,
        Phase::Failed {
            error: Error::new("storage failed"),
            open: true,
        },
    );
    assert!(app.m.unsettled.is_none());
    assert!(!app.m.adapter_saving());
    assert!(app.logged(text::SAVE_UNCONFIRMED_LOG));
    assert!(!app.logged("Couldn't save"));
}

#[test]
fn selecting_the_adapter_shows_its_row() {
    let mut app = App::connected(100, 60);
    app.m.adapter_scroll = 3;
    app.press(KeyCode::Char('a'));
    assert_eq!(app.m.selected, Some(Item::Adapter));
    assert_eq!(app.m.adapter_scroll, 0);
}

#[test]
fn choosing_another_adapter_drops_a_cut_short_save() {
    let mut app = App::connected(100, 60);
    app.save_via();
    app.lose_usb();
    app.done(
        "AdapterSave",
        Err(Error::new("the connection to the adapter closed")),
    );
    assert!(app.m.unsettled.is_some());
    // The user opens another adapter from the chooser.
    app.m.open("/dev/ttyACM9".into());
    assert!(app.m.unsettled.is_none());
    assert!(!app.logged(text::SAVE_UNCONFIRMED_LOG));
    assert!(!app.logged("Couldn't save"));
}

/// A cut-short VIA save whose adapter has returned and opened, before it is ready.
fn cut_short_and_reopened(app: &mut App) -> SessionId {
    app.save_via();
    app.lose_usb();
    app.done(
        "AdapterSave",
        Err(Error::new("the connection to the adapter closed")),
    );
    app.m.update(Msg::Ports(Ok(vec![PortInfo {
        port: "/dev/ttyACM1".into(),
        serial: "0123456789ABCDEF-via:f64c2b3c".into(),
    }])));
    let session = app.fake.next.get();
    app.edit(|st| {
        st.session = session;
        st.available = true;
    });
    app.phase(session, Phase::Opened);
    session
}

#[test]
fn an_adapter_that_isnt_ready_doesnt_confirm_a_cut_short_save() {
    let mut app = App::connected(100, 60);
    let session = cut_short_and_reopened(&mut app);
    // The status shows what was sent, but an adapter that isn't ready reports only defaults.
    interface(&mut app, ConfigurationInterface::Via, |s| {
        s.enabled = true;
        s.profile = 2;
    });
    app.edit(|st| st.status.ready = false);
    app.phase(
        session,
        Phase::Failed {
            error: Error::new("storage failed"),
            open: true,
        },
    );
    assert!(app.logged(text::SAVE_UNCONFIRMED_LOG));
    assert!(!app.logged("Saved the adapter settings"));
    assert!(!app.m.adapter_drafts.is_empty());
}

#[test]
fn a_cut_short_save_is_settled_when_the_adapter_never_finishes_starting() {
    let mut app = App::connected(100, 60);
    let session = cut_short_and_reopened(&mut app);
    app.edit(|st| st.status.ready = false);
    app.phase(session, Phase::Waiting);
    app.m.waiting_since = Some(Instant::now() - STARTUP_GRACE);
    app.m.tick();
    assert!(app.m.unsettled.is_none());
    assert!(!app.m.adapter_saving());
    assert!(app.logged(text::SAVE_UNCONFIRMED_LOG));
}

#[test]
fn reopening_the_same_adapter_keeps_a_cut_short_save_to_settle() {
    let mut app = App::connected(100, 60);
    app.save_via();
    app.lose_usb();
    app.done(
        "AdapterSave",
        Err(Error::new("the connection to the adapter closed")),
    );
    app.m.ports = vec![PortInfo {
        port: "/dev/ttyACM1".into(),
        serial: "0123456789ABCDEF-via:f64c2b3c".into(),
    }];
    app.m.open("/dev/ttyACM1".into());
    assert!(app.m.unsettled.is_some());
}

#[test]
fn moving_down_to_the_adapter_shows_its_row() {
    let mut app = App::connected(100, 60);
    app.m.adapter_scroll = 3;
    for _ in 0..20 {
        app.press(KeyCode::Down);
        if app.m.selected == Some(Item::Adapter) {
            break;
        }
    }
    assert_eq!(app.m.selected, Some(Item::Adapter));
    assert_eq!(app.m.adapter_scroll, 0);
}
