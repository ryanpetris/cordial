//! Model tests against a stand-in controller: what the TUI draws, and which
//! commands clicks and keys send.
use super::*;
use crate::ui::fake::{Call, Fake};
use crate::{
    controller::{DeviceSettings, Pending},
    ui::{catalog::tests::setting, command::tests as fixture},
};
use cordial_protocol::{
    errors::ErrorCode,
    identifiers::{ConnectionState, DeviceId, PairingState, Transport},
    info::{InfoField, InfoKey},
    messages::{BuildProfile, Capabilities, Capability, Prompt},
    settings::{SettingKey, SettingValue},
};
use cordial_protocol::{
    identifiers::SettingsState,
    settings::{SettingState, SettingType},
};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyModifiers},
    layout::Rect,
};

struct App {
    m: Model<Fake>,
    fake: Fake,
}

#[test]
fn hidpp_detection_translation_and_settings_have_separate_statuses() {
    use cordial_protocol::{hidpp::ProtocolState, identifiers::NormalizationState};
    let mut app = App::connected(120, 70);
    app.edit(|st| {
        let d = &mut st.devices[0];
        d.hidpp_protocol = ProtocolState::Detected { major: 4, minor: 2 };
        d.normalization_state = NormalizationState::Unsupported;
        d.normalization_error = Some(ErrorCode::HidppControlsUnavailable);
        d.settings_state = SettingsState::Ready;
        d.settings_error = None;
    });
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    assert!(screen.contains("HID++ 4.2"), "{screen}");
    assert!(screen.contains("Special-Key Translation"), "{screen}");
    assert!(screen.contains("Unavailable"), "{screen}");
    assert!(screen.contains("Settings Ready"), "{screen}");
    app.edit(|st| {
        let d = &mut st.devices[0];
        d.hidpp_enabled = false;
        d.normalization_state = NormalizationState::Off;
        d.normalization_error = None;
        d.hidpp_protocol = ProtocolState::Error {
            code: ErrorCode::HidppTimeout,
        };
    });
    app.render();
    assert!(
        app.screen().contains("HID++ Failed: no response"),
        "{}",
        app.screen()
    );
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
    fn done(&mut self, ticket_of: &str, result: Result<Outcome, Failure>) {
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
}

fn auth(method: PromptMethod, value: Option<&str>) -> Auth {
    Auth {
        request: RequestId::try_from(7).unwrap(),
        prompt: Prompt {
            candidate_id: CandidateId("c_2".into()),
            prompt_id: "p_1".into(),
            method,
            expires_in_ms: 30000,
            value: value.map(str::to_owned),
        },
        display: method.display(),
        expires: Instant::now() + Duration::from_secs(30),
        expires_in_ms: 30000,
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
            error: Error::new("no handshake"),
            open: false,
        },
    );
    let screen = app.screen();
    assert!(
        screen.contains("Choose an Adapter") && screen.contains("no handshake"),
        "{screen}"
    );
    app.click(Action::RefreshPorts);
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
    assert_eq!(
        app.calls()
            .iter()
            .filter(|c| matches!(c, Call::Open(_)))
            .count(),
        1
    );
    app.click(Action::Port("/dev/ttyACM0".into()));
    assert_eq!(
        app.calls()
            .iter()
            .filter(|c| matches!(c, Call::Open(_)))
            .count(),
        2
    );

    let mut two = App::new(100, 30, None);
    two.m.update(Msg::Ports(Ok(vec![
        port("/dev/ttyACM0"),
        port("/dev/ttyACM1"),
    ])));
    assert!(!two.calls().iter().any(|c| matches!(c, Call::Open(_))));
    assert!(two.screen().contains("/dev/ttyACM1"));

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
        ("adapter settings", |a| {
            a.click(Action::Menu(Menu::Adapter));
            a.click(Action::AdapterSettings);
        }),
        ("pairing code", |a| {
            a.edit(|st| st.auth = Some(auth(PromptMethod::EnterPasskey, None)));
            a.m.tick();
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
            !app.ran("Remove") && !app.ran("PairReply"),
            "{name}: quitting sent a change"
        );
        app.m.update(Msg::Controller(Box::new(Event::Closed)));
        assert!(app.m.done, "{name}");
    }
    let mut chooser = App::new(100, 30, None);
    chooser.m.update(Msg::Ports(Ok(vec![])));
    chooser.m.update(Msg::Interrupt);
    assert_eq!(chooser.calls().last(), Some(&Call::Close));
}

#[test]
fn a_click_needs_press_and_release_on_the_same_control() {
    let mut app = App::connected(100, 30);
    let row = app.hit(&Action::Device("d_2".into()));
    app.mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y);
    assert!(app.m.selected.is_empty(), "press alone activated");
    app.mouse(MouseEventKind::Up(MouseButton::Left), row.x, row.y + 2);
    assert!(app.m.selected.is_empty(), "release elsewhere activated");
    app.mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y);
    app.mouse(MouseEventKind::Up(MouseButton::Left), row.x, row.y);
    assert_eq!(app.m.selected, "d_2");
    let screen = app.screen();
    assert!(
        screen.contains("[Disconnect]") && screen.contains("[Remove]"),
        "{screen}"
    );
    // Enter never disconnects the selected device.
    app.press(KeyCode::Enter);
    assert!(!app.ran("Disconnect"));
    app.click(Action::Disconnect);
    assert!(app.ran("Disconnect(\"d_2\")"));
}

#[test]
fn pointer_and_tab_share_the_highlight() {
    let mut app = App::connected(100, 30);
    let help = app.hit(&Action::Help);
    app.mouse(MouseEventKind::Moved, help.x, help.y);
    app.render();
    assert_eq!(app.m.focus, Some(Action::Help));
    // Moving off controls clears it; hovering never activates.
    app.mouse(MouseEventKind::Moved, 0, app.m.height - 1);
    app.render();
    assert_eq!(app.m.focus, None);
    assert!(app.m.dialog.is_none());
    // Tab continues from the hovered control.
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
    app.edit(|st| st.auth = Some(auth(PromptMethod::EnterPasskey, None)));
    app.m.tick();
    let screen = app.screen();
    assert!(
        screen.contains("Pair With Other one") && screen.contains("6-digit"),
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
    assert!(app.ran("value: Some(\"042731\")"), "{:?}", app.calls());
    // Cancel rejects the prompt.
    app.click(Action::CancelDialog);
    assert!(app.ran("action: Reject"));
}

#[test]
fn comparison_and_display_prompts() {
    let mut app = App::connected(100, 30);
    app.edit(|st| st.auth = Some(auth(PromptMethod::ConfirmPasskey, Some("482916"))));
    app.m.tick();
    let screen = app.screen();
    assert!(
        screen.contains("4 8 2   9 1 6") && screen.contains("[Codes match]"),
        "{screen}"
    );
    assert!(!app.m.form_focused);
    app.press(KeyCode::Char('y'));
    assert!(app.ran("action: Accept, value: None"));
    app.edit(|st| st.auth = Some(auth(PromptMethod::DisplayPasskey, Some("123456"))));
    app.m.tick();
    // The dialog is drawn undimmed over the dimmed screen behind it.
    let dim = |c: &ratatui::buffer::Cell| c.modifier.contains(ratatui::style::Modifier::DIM);
    assert!(
        !app.cells("1 2 3   4 5 6").iter().any(dim),
        "{}",
        app.screen()
    );
    assert!(app.cells("Cordial").iter().all(dim));
    app.click(Action::CancelDialog);
    assert!(app.ran("Cancel(RequestId(7))"), "{:?}", app.calls());
}

#[test]
fn every_main_action_is_clickable() {
    let mut app = App::connected(100, 48);
    app.click(Action::Menu(Menu::Scan));
    // Menu entries never inherit styling, such as SAVED's dimming, from beneath.
    for text in ["Bluetooth LE and Classic", "Bluetooth LE only"] {
        let cells = app.cells(text);
        assert!(
            cells.iter().all(|c| c.modifier.is_empty()),
            "{text}: {cells:?}"
        );
    }
    app.click(Action::Scan(ScanTransport::Ble));
    assert!(app.ran("Scan(Ble)"));
    app.edit(|st| {
        st.pending.push(Pending {
            id: RequestId::try_from(3).unwrap(),
            device_id: None,
            command: "discovery.scan",
            target: None,
        });
    });
    assert!(app.screen().contains("Scanning BLE"));
    app.click(Action::ScanOff);
    assert!(app.ran("ScanOff"));
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::Monitor(false));
    assert!(app.ran("Monitor(false)"));
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
    app.click(Action::Hidpp(false));
    assert!(app.ran("Hidpp(\"d_1\", false)"));
    app.click(Action::Remove);
    assert!(app.screen().contains("Remove Test keyboard?"));
    app.click(Action::Confirm);
    assert!(app.ran("Remove(\"d_1\")"));
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
    app.edit(|st| st.candidates[1].name = Some("Test\u{9b}2J\u{202e}键\u{e0001}board".into()));
    app.m.selected = "c_2".into();
    let screen = app.screen();
    assert!(!screen.contains(['\u{9b}', '\u{202e}', '\u{e0001}']));
    assert!(screen.contains("\\u009b"), "{screen}");
}

#[test]
fn notification_loss_is_one_truthful_line() {
    use crate::{client::Envelope, controller::Notice};
    let mut app = App::connected(100, 30);
    let session = app.m.session.unwrap();
    let before = app.m.logs.len();
    let event = |name: &str, data| Notice::Message {
        envelope: Envelope {
            message: cordial_protocol::messages::Message::event(name.into(), None, data),
            raw: String::new(),
            command: None,
            internal: false,
            sequence: 0,
        },
        background: false,
    };
    // The session reports local loss as Skipped, then its marker envelope.
    for notice in [
        Notice::Skipped(4),
        event("local.events_lost", serde_json::json!({"dropped": 4})),
    ] {
        app.m
            .update(Msg::Controller(Box::new(Event::Notice { session, notice })));
    }
    let lines: Vec<&str> = app.m.logs[before..]
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(lines, ["Missed 4 updates; refreshing the device list"]);
    app.m.update(Msg::Controller(Box::new(Event::Notice {
        session,
        notice: event(
            "events.lost",
            serde_json::json!({"revision": 5, "dropped": 2}),
        ),
    })));
    assert_eq!(
        app.m.logs.last().unwrap().text,
        "Missed some updates; refreshing the device list"
    );
    assert!(!app.screen().contains("retained"));
}

#[test]
fn lost_adapter_keeps_the_last_state() {
    let mut app = App::connected(100, 30);
    app.edit(|st| st.available = false);
    let session = app.m.session.unwrap();
    app.phase(session, Phase::Lost(Error::new("unplugged")));
    let screen = app.screen();
    assert!(
        screen.contains("Lost the adapter connection") && screen.contains("Test keyboard"),
        "{screen}"
    );
    app.click(Action::Reopen);
    assert_eq!(app.calls().last(), Some(&Call::Open("/dev/ttyACM0".into())));
}

/// A writable setting with a reading, as the adapter reports it.
fn writable(key: SettingKey, observed: SettingValue) -> cordial_protocol::settings::Setting {
    let mut s = setting(key);
    s.writable = true;
    s.observed = observed;
    s.fresh = true;
    s
}

fn wire_failure(code: ErrorCode) -> Failure {
    let mut error = crate::client::Error::new("refused");
    error.wire = Some(Box::new(cordial_protocol::messages::WireError {
        code,
        details: None,
    }));
    error.into()
}

impl App {
    /// Opens the settings page of d_1 with these settings.
    fn settings(&mut self, settings: Vec<cordial_protocol::settings::Setting>) {
        self.edit(|st| {
            st.settings.insert(
                DeviceId("d_1".into()),
                DeviceSettings {
                    loaded: true,
                    current: true,
                    settings,
                    ..Default::default()
                },
            );
        });
        self.click(Action::Device("d_1".into()));
        self.click(Action::DeviceSettings);
    }
    /// Changes one of d_1's cached settings.
    fn row(&mut self, key: SettingKey, f: impl FnOnce(&mut cordial_protocol::settings::Setting)) {
        self.edit(|st| {
            let c = st.settings.get_mut(&DeviceId("d_1".into())).unwrap();
            f(c.settings.iter_mut().find(|s| s.key == key).unwrap());
        });
    }
    /// The setting commands sent so far.
    fn sets(&self) -> Vec<String> {
        self.calls()
            .iter()
            .filter_map(|c| match c {
                Call::Run(r) if r.contains("SettingSet") || r.contains("SettingForget") => {
                    Some(r.clone())
                }
                _ => None,
            })
            .collect()
    }
    /// Marks a sent value stored and applied, with the device job ended.
    fn applied(&mut self, key: SettingKey, value: SettingValue) {
        self.row(key, |s| {
            s.managed = true;
            s.desired = value.clone();
            s.observed = value;
            s.state = SettingState::Applied;
        });
        self.edit(|st| st.devices[0].settings_state = SettingsState::Ready);
        self.m.tick();
    }
}

#[test]
fn settings_stage_changes_until_save() {
    let mut app = App::connected(120, 36);
    let mut delay = writable(SettingKey::BacklightDelayPowered, SettingValue::Integer(60));
    delay.min = Some(5);
    delay.max = Some(300);
    delay.step = Some(5);
    app.settings(vec![setting(SettingKey::WheelInfo), delay]);
    assert!(app.screen().contains("Settings · Test keyboard"));
    app.click(Action::Setting(SettingKey::BacklightDelayPowered));
    let screen = app.screen();
    assert!(screen.contains("○ Not Saved"), "{screen}");
    assert!(screen.contains("Range"), "{screen}");
    assert!(screen.contains("5-300, Steps of 5"), "{screen}");
    // The footer is always drawn; with nothing staged Save and Discard are off.
    assert!(
        screen.contains("[Save]") && screen.contains("[Discard]"),
        "{screen}"
    );
    assert!(!app.m.hits.iter().any(|h| h.action == Action::SaveAll));
    app.click(Action::Step(SettingKey::BacklightDelayPowered, 5));
    let screen = app.screen();
    assert!(screen.contains("✎ Changed"), "{screen}");
    assert!(screen.contains("65 s"), "{screen}");
    assert!(app.sets().is_empty(), "stepping sent a setting");
    // Backspace undoes the selected change; Right steps it again.
    app.press(KeyCode::Backspace);
    assert!(app.m.page.drafts["d_1"].is_empty());
    app.press(KeyCode::Right);
    assert_eq!(
        app.m.page.drafts["d_1"][&SettingKey::BacklightDelayPowered],
        settings::Change::Set {
            value: SettingValue::Integer(65),
            policy: false
        }
    );
    // Stepping back to the reading of an unsaved value is no change.
    app.press(KeyCode::Left);
    assert!(app.m.page.drafts["d_1"].is_empty());
    // Save Current Value is a change even though it matches the reading.
    app.click(Action::Keep(SettingKey::BacklightDelayPowered));
    assert!(app.screen().contains("✎ Changed"));
    // Esc leaves the page and keeps what is staged.
    app.press(KeyCode::Esc);
    assert!(app.m.page.device.is_empty());
    assert_eq!(app.m.page.drafts["d_1"].len(), 1);
    app.click(Action::DeviceSettings);
    app.click(Action::Discard);
    assert!(app.m.page.drafts["d_1"].is_empty());
    // Offline, values can't change and nothing can be staged but forgetting.
    app.edit(|st| st.devices[0].state = ConnectionState::Disconnected);
    app.click(Action::Setting(SettingKey::BacklightDelayPowered));
    app.render();
    assert!(
        !app.m
            .hits
            .iter()
            .any(|h| matches!(h.action, Action::Step(..)))
    );
    assert!(app.screen().contains("[−5]"), "controls stay in place");
    app.m
        .action(Action::Step(SettingKey::BacklightDelayPowered, 5));
    assert!(app.m.page.drafts["d_1"].is_empty());
    app.click(Action::SettingsBack);
    assert!(app.m.page.device.is_empty());
}

#[test]
fn save_sends_one_change_at_a_time_after_the_device_applies_it() {
    let mut app = App::connected(120, 36);
    let text = |t: &str| SettingValue::Text(t.into());
    let mut mode = writable(SettingKey::BacklightMode, text("automatic"));
    mode.kind = SettingType::Enum;
    mode.choices = vec![text("automatic"), text("permanent_manual")];
    let mut level = writable(SettingKey::BacklightLevel, SettingValue::Integer(3));
    level.min = Some(0);
    level.max = Some(7);
    level.step = Some(1);
    let mut delay = writable(SettingKey::BacklightDelayPowered, SettingValue::Integer(60));
    delay.min = Some(5);
    delay.max = Some(300);
    delay.step = Some(5);
    app.settings(vec![delay, level, mode]);
    app.m.action(Action::Draft(
        SettingKey::BacklightDelayPowered,
        SettingValue::Integer(90),
    ));
    app.click(Action::Setting(SettingKey::BacklightLevel));
    app.m.action(Action::Step(SettingKey::BacklightLevel, 1));
    app.click(Action::Setting(SettingKey::BacklightMode));
    app.m.action(Action::Draft(
        SettingKey::BacklightMode,
        text("permanent_manual"),
    ));
    app.press(KeyCode::Char('s'));
    // Display order: the mode first, and nothing else until it applies.
    assert_eq!(app.sets().len(), 1, "{:?}", app.sets());
    assert!(app.sets()[0].contains("BacklightMode"));
    assert!(app.screen().contains("◌ Sending"));
    // Editing and Refresh wait while the Save runs.
    app.m.action(Action::Discard);
    assert_eq!(app.m.page.drafts["d_1"].len(), 3);
    app.press(KeyCode::Char('r'));
    assert!(!app.ran("SettingsRefresh"));
    app.edit(|st| st.devices[0].settings_state = SettingsState::Applying);
    app.row(SettingKey::BacklightMode, |s| {
        s.managed = true;
        s.desired = text("permanent_manual");
        s.state = SettingState::Applying;
    });
    app.done("SettingSet", Ok(Outcome::ReplySent));
    // Stored, but the device job still runs: the level waits.
    assert_eq!(app.sets().len(), 1);
    // The row alone reads Applied while the job still runs.
    app.row(SettingKey::BacklightMode, |s| {
        s.state = SettingState::Applied
    });
    app.m.tick();
    assert_eq!(app.sets().len(), 1);
    app.applied(SettingKey::BacklightMode, text("permanent_manual"));
    assert_eq!(app.sets().len(), 2);
    assert!(app.sets()[1].contains("BacklightLevel, Value(Integer(4))"));
    assert!(!app.m.page.drafts["d_1"].contains_key(&SettingKey::BacklightMode));
    // A busy refusal is sent again once the device's settings work is idle.
    app.edit(|st| st.devices[0].settings_state = SettingsState::Discovering);
    app.done("BacklightLevel", Err(wire_failure(ErrorCode::Busy)));
    assert_eq!(app.sets().len(), 2);
    app.edit(|st| st.devices[0].settings_state = SettingsState::Ready);
    app.m.tick();
    // Resends are spaced, so an adapter that stays busy isn't flooded.
    assert_eq!(app.sets().len(), 2);
    std::thread::sleep(Duration::from_millis(1100));
    app.m.tick();
    assert_eq!(app.sets().len(), 3);
    app.done("BacklightLevel", Ok(Outcome::ReplySent));
    app.applied(SettingKey::BacklightLevel, SettingValue::Integer(4));
    assert!(app.sets()[3].contains("BacklightDelayPowered, Value(Integer(90))"));
    // Another refusal fails only that change, which keeps its draft.
    app.done(
        "BacklightDelayPowered",
        Err(wire_failure(ErrorCode::InvalidArgs)),
    );
    let screen = app.screen();
    assert!(screen.contains("✕ Couldn't Save 1"), "{screen}");
    assert!(!screen.contains("Sending"), "{screen}");
    assert_eq!(
        app.m.page.drafts["d_1"].keys().collect::<Vec<_>>(),
        [&SettingKey::BacklightDelayPowered]
    );
    assert!(!app.m.page.saves["d_1"].running);
}

#[test]
fn a_level_waits_for_its_mode_and_retry_resends_only_what_did_not_apply() {
    let mut app = App::connected(120, 36);
    let text = |t: &str| SettingValue::Text(t.into());
    let mut mode = writable(SettingKey::BacklightMode, text("automatic"));
    mode.kind = SettingType::Enum;
    mode.choices = vec![text("automatic"), text("permanent_manual")];
    let mut level = writable(SettingKey::BacklightLevel, SettingValue::Integer(3));
    level.min = Some(0);
    level.max = Some(7);
    level.step = Some(1);
    let mut invert = writable(SettingKey::WheelInvert, SettingValue::Bool(false));
    invert.managed = true;
    invert.desired = SettingValue::Bool(false);
    invert.state = SettingState::Applied;
    app.settings(vec![mode, level, invert]);
    app.m.action(Action::Draft(
        SettingKey::BacklightMode,
        text("permanent_manual"),
    ));
    app.m.action(Action::Step(SettingKey::BacklightLevel, 1));
    app.m.action(Action::Draft(
        SettingKey::WheelInvert,
        SettingValue::Bool(true),
    ));
    app.click(Action::SaveAll);
    app.row(SettingKey::BacklightMode, |s| {
        s.managed = true;
        s.desired = text("permanent_manual");
        s.state = SettingState::Error;
        s.error = Some(ErrorCode::HidppTimeout);
    });
    app.done("SettingSet", Ok(Outcome::ReplySent));
    app.m.tick();
    // The mode was stored but didn't apply, so the level stays unsent.
    let save = &app.m.page.saves["d_1"];
    assert_eq!(save.items[0].status, settings::Status::NotApplied);
    assert_eq!(save.items[1].status, settings::Status::NotSent);
    assert!(app.sets()[1].contains("WheelInvert"), "{:?}", app.sets());
    assert!(app.m.page.drafts["d_1"].contains_key(&SettingKey::BacklightLevel));
    app.done("WheelInvert", Ok(Outcome::ReplySent));
    app.applied(SettingKey::WheelInvert, SettingValue::Bool(true));
    let screen = app.screen();
    assert!(screen.contains("Didn't Apply 1"), "{screen}");
    assert!(screen.contains("Not Sent 1"), "{screen}");
    assert_eq!(
        app.m.page.saves["d_1"].items[1].error.as_deref(),
        Some("Backlight Mode isn't permanent manual")
    );
    // A draft of the mode itself keeps it out of Retry, and unsent.
    app.m
        .action(Action::Draft(SettingKey::BacklightMode, text("automatic")));
    app.render();
    assert!(!app.m.hits.iter().any(|h| h.action == Action::RetrySave));
    app.m.action(Action::RetrySave);
    assert_eq!(app.sets().len(), 2);
    // Retry resends only the mode, while it is still the saved value; the
    // unsent level stays staged.
    app.m.action(Action::Undo(SettingKey::BacklightMode));
    app.click(Action::RetrySave);
    assert_eq!(app.sets().len(), 3);
    assert!(app.sets()[2].contains("BacklightMode, Value(Text(\"permanent_manual\"))"));
    assert!(app.m.page.drafts["d_1"].contains_key(&SettingKey::BacklightLevel));
    app.done("BacklightMode", Ok(Outcome::ReplySent));
    app.row(SettingKey::BacklightMode, |s| {
        s.state = SettingState::Unsupported;
        s.error = None;
    });
    app.edit(|st| st.devices[0].settings_state = SettingsState::Ready);
    app.m.tick();
    // A value the device can't take now is not offered again.
    let screen = app.screen();
    assert!(screen.contains("Didn't Apply 1"), "{screen}");
    assert!(
        !app.m.hits.iter().any(|h| h.action == Action::RetrySave),
        "{screen}"
    );
    // Once a reconnect or Refresh shows the saved value applied, the failure
    // and Retry go away.
    app.row(SettingKey::BacklightMode, |s| {
        s.state = SettingState::Applied;
        s.observed = text("permanent_manual");
    });
    let screen = app.screen();
    assert!(!screen.contains("Didn't Apply"), "{screen}");
    assert!(!screen.contains("[Retry]"), "{screen}");
}

#[test]
fn forgetting_the_mode_holds_a_level_only_on_the_reported_mode() {
    let mut app = App::connected(120, 36);
    let text = |t: &str| SettingValue::Text(t.into());
    let mut mode = writable(SettingKey::BacklightMode, text("permanent_manual"));
    mode.kind = SettingType::Enum;
    mode.choices = vec![text("automatic"), text("permanent_manual")];
    mode.managed = true;
    mode.desired = text("permanent_manual");
    mode.state = SettingState::Applied;
    let mut level = writable(SettingKey::BacklightLevel, SettingValue::Integer(3));
    level.min = Some(0);
    level.max = Some(7);
    level.step = Some(1);
    app.settings(vec![mode, level]);
    app.m.action(Action::Forget(SettingKey::BacklightMode));
    app.m.action(Action::Step(SettingKey::BacklightLevel, 1));
    app.click(Action::SaveAll);
    assert!(app.sets()[0].contains("SettingForget(\"d_1\", BacklightMode)"));
    app.row(SettingKey::BacklightMode, |s| {
        s.managed = false;
        s.desired = SettingValue::Null;
        s.state = SettingState::Unmanaged;
    });
    // The forget is stored and the device still reports permanent manual.
    app.done("SettingForget", Ok(Outcome::ReplySent));
    assert_eq!(app.sets().len(), 2, "{:?}", app.sets());
    assert!(app.sets()[1].contains("BacklightLevel, Value(Integer(4))"));
}

#[test]
fn with_logitech_features_off_save_only_stores_and_forget_works_offline() {
    let mut app = App::connected(120, 36);
    app.edit(|st| st.devices[0].hidpp_enabled = false);
    let mut invert = writable(SettingKey::WheelInvert, SettingValue::Bool(false));
    invert.managed = true;
    invert.desired = SettingValue::Bool(false);
    invert.state = SettingState::Pending;
    let thumb = writable(SettingKey::ThumbwheelInvert, SettingValue::Bool(false));
    app.settings(vec![invert, thumb]);
    app.m.action(Action::Draft(
        SettingKey::ThumbwheelInvert,
        SettingValue::Bool(true),
    ));
    app.click(Action::SaveAll);
    // The row stays Pending while nothing is applied; the Save doesn't wait.
    app.row(SettingKey::ThumbwheelInvert, |s| {
        s.managed = true;
        s.desired = SettingValue::Bool(true);
        s.state = SettingState::Pending;
    });
    app.done("SettingSet", Ok(Outcome::ReplySent));
    assert_eq!(
        app.m.page.saves["d_1"].items[0].status,
        settings::Status::Saved
    );
    assert!(!app.m.page.saves["d_1"].running);
    assert!(app.m.page.drafts["d_1"].is_empty());
    // Disconnected: forgetting a saved value can be staged and saved.
    app.edit(|st| st.devices[0].state = ConnectionState::Disconnected);
    app.click(Action::Setting(SettingKey::WheelInvert));
    app.click(Action::Forget(SettingKey::WheelInvert));
    app.click(Action::SaveAll);
    assert!(app.sets()[1].contains("SettingForget(\"d_1\", WheelInvert)"));
    app.done("SettingForget", Ok(Outcome::ReplySent));
    assert_eq!(
        app.m.page.saves["d_1"].items[0].status,
        settings::Status::Saved
    );
}

#[test]
fn a_disconnect_while_applying_stops_the_save_and_keeps_unsent_changes() {
    let mut app = App::connected(120, 36);
    let a = writable(SettingKey::WheelInvert, SettingValue::Bool(false));
    let b = writable(SettingKey::ThumbwheelInvert, SettingValue::Bool(false));
    app.settings(vec![a, b]);
    app.m.action(Action::Draft(
        SettingKey::WheelInvert,
        SettingValue::Bool(true),
    ));
    app.m.action(Action::Draft(
        SettingKey::ThumbwheelInvert,
        SettingValue::Bool(true),
    ));
    app.click(Action::SaveAll);
    app.done("SettingSet", Ok(Outcome::ReplySent));
    // Leaving the page doesn't stop following the Save.
    app.click(Action::SettingsBack);
    app.edit(|st| st.devices[0].state = ConnectionState::Disconnected);
    app.m.tick();
    let save = &app.m.page.saves["d_1"];
    assert!(!save.running);
    assert_eq!(save.items[0].status, settings::Status::NotApplied);
    assert_eq!(save.items[1].status, settings::Status::NotSent);
    assert_eq!(app.sets().len(), 1);
    assert!(!app.m.page.drafts["d_1"].contains_key(&SettingKey::WheelInvert));
    assert!(app.m.page.drafts["d_1"].contains_key(&SettingKey::ThumbwheelInvert));
}

#[test]
fn a_level_alone_waits_for_the_reported_mode_when_the_device_has_one() {
    let text = |t: &str| SettingValue::Text(t.into());
    let level = || {
        let mut level = writable(SettingKey::BacklightLevel, SettingValue::Integer(3));
        level.min = Some(0);
        level.max = Some(7);
        level.step = Some(1);
        level
    };
    let mut mode = writable(SettingKey::BacklightMode, text("automatic"));
    mode.kind = SettingType::Enum;
    mode.choices = vec![text("automatic"), text("permanent_manual")];
    // The device reports another mode, so a level saved alone stays unsent.
    let mut app = App::connected(120, 36);
    app.settings(vec![mode, level()]);
    app.m.action(Action::Step(SettingKey::BacklightLevel, 1));
    app.click(Action::SaveAll);
    assert!(app.sets().is_empty(), "{:?}", app.sets());
    let item = &app.m.page.saves["d_1"].items[0];
    assert_eq!(item.status, settings::Status::NotSent);
    assert_eq!(
        item.error.as_deref(),
        Some("Backlight Mode isn't permanent manual")
    );
    assert!(app.m.page.drafts["d_1"].contains_key(&SettingKey::BacklightLevel));
    // With Logitech Features off, a level is only stored.
    app.edit(|st| st.devices[0].hidpp_enabled = false);
    app.click(Action::SaveAll);
    assert_eq!(app.sets().len(), 1, "{:?}", app.sets());
    // A device without a Backlight Mode setting has nothing to wait for.
    let mut app = App::connected(120, 36);
    app.settings(vec![level()]);
    app.m.action(Action::Step(SettingKey::BacklightLevel, 1));
    app.click(Action::SaveAll);
    assert_eq!(app.sets().len(), 1, "{:?}", app.sets());
}

#[test]
fn saving_another_setting_keeps_a_value_that_did_not_apply_for_retry() {
    let mut app = App::connected(120, 36);
    let a = writable(SettingKey::WheelInvert, SettingValue::Bool(false));
    let b = writable(SettingKey::ThumbwheelInvert, SettingValue::Bool(false));
    app.settings(vec![a, b]);
    app.m.action(Action::Draft(
        SettingKey::WheelInvert,
        SettingValue::Bool(true),
    ));
    app.click(Action::SaveAll);
    app.row(SettingKey::WheelInvert, |s| {
        s.managed = true;
        s.desired = SettingValue::Bool(true);
        s.state = SettingState::Error;
        s.error = Some(ErrorCode::HidppTimeout);
    });
    app.done("SettingSet", Ok(Outcome::ReplySent));
    app.m.tick();
    assert!(app.screen().contains("Didn't Apply 1"));
    // Saving B sends only B, and A's failure stays with its Retry.
    app.m.action(Action::Draft(
        SettingKey::ThumbwheelInvert,
        SettingValue::Bool(true),
    ));
    app.click(Action::SaveAll);
    assert_eq!(app.sets().len(), 2);
    assert!(
        app.sets()[1].contains("ThumbwheelInvert"),
        "{:?}",
        app.sets()
    );
    app.done("ThumbwheelInvert", Ok(Outcome::ReplySent));
    app.applied(SettingKey::ThumbwheelInvert, SettingValue::Bool(true));
    assert_eq!(app.sets().len(), 2, "{:?}", app.sets());
    assert!(!app.m.page.saves["d_1"].running);
    let screen = app.screen();
    assert!(screen.contains("Didn't Apply 1"), "{screen}");
    app.click(Action::RetrySave);
    assert_eq!(app.sets().len(), 3);
    assert!(
        app.sets()[2].contains("WheelInvert, Value(Bool(true))"),
        "{:?}",
        app.sets()
    );
}

#[test]
fn offline_save_forgets_and_keeps_value_drafts_staged() {
    let mut app = App::connected(120, 36);
    let mut a = writable(SettingKey::WheelInvert, SettingValue::Bool(false));
    a.managed = true;
    a.desired = SettingValue::Bool(false);
    a.state = SettingState::Applied;
    let b = writable(SettingKey::ThumbwheelInvert, SettingValue::Bool(false));
    app.settings(vec![a, b]);
    app.m.action(Action::Draft(
        SettingKey::ThumbwheelInvert,
        SettingValue::Bool(true),
    ));
    app.edit(|st| st.devices[0].state = ConnectionState::Disconnected);
    app.click(Action::Setting(SettingKey::WheelInvert));
    app.click(Action::Forget(SettingKey::WheelInvert));
    app.click(Action::SaveAll);
    assert_eq!(app.sets().len(), 1, "{:?}", app.sets());
    assert!(app.sets()[0].contains("SettingForget(\"d_1\", WheelInvert)"));
    app.done("SettingForget", Ok(Outcome::ReplySent));
    assert!(!app.m.page.saves["d_1"].running);
    assert_eq!(
        app.m.page.drafts["d_1"].keys().collect::<Vec<_>>(),
        [&SettingKey::ThumbwheelInvert]
    );
}

#[test]
fn an_apply_timeout_reads_the_settings_again_and_keeps_unsent_drafts() {
    let mut app = App::connected(120, 36);
    let a = writable(SettingKey::WheelInvert, SettingValue::Bool(false));
    let b = writable(SettingKey::ThumbwheelInvert, SettingValue::Bool(false));
    app.settings(vec![a, b]);
    app.m.action(Action::Draft(
        SettingKey::WheelInvert,
        SettingValue::Bool(true),
    ));
    app.m.action(Action::Draft(
        SettingKey::ThumbwheelInvert,
        SettingValue::Bool(true),
    ));
    app.click(Action::SaveAll);
    app.done("SettingSet", Ok(Outcome::ReplySent));
    app.m.tick();
    let loads = |app: &App| {
        app.calls()
            .iter()
            .filter(|c| matches!(c, Call::Run(r) if r.contains("Settings(\"d_1\")")))
            .count()
    };
    let before = loads(&app);
    // No outcome arrives until the bounded wait ends.
    app.m.page.age_apply("d_1", Duration::from_secs(90));
    app.m.tick();
    assert!(app.m.page.saves["d_1"].running);
    app.m.page.age_apply("d_1", Duration::from_secs(16));
    app.m.tick();
    let save = &app.m.page.saves["d_1"];
    assert!(!save.running);
    assert_eq!(save.items[0].status, settings::Status::NotApplied);
    assert_eq!(save.items[0].error.as_deref(), Some("Timed out"));
    assert_eq!(save.items[1].status, settings::Status::NotSent);
    assert_eq!(loads(&app), before + 1, "{:?}", app.calls());
    assert!(app.m.page.drafts["d_1"].contains_key(&SettingKey::ThumbwheelInvert));
}

#[test]
fn logitech_features_stay_dim_options_while_settings_are_busy() {
    let mut app = App::connected(100, 60);
    app.click(Action::Device("d_1".into()));
    // The On and Off options, on their own line, with the cells after its label.
    let options = |app: &mut App| {
        let mut buf = Buffer::empty(Rect::new(0, 0, app.m.width as u16, app.m.height as u16));
        app.m.render(&mut buf);
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(i) = row.find("Logitech Features") {
                let at = row[i..].find("[● On] [○ Off]").expect(&row) + i;
                let x = row[..at].chars().count() as u16;
                return (x..x + 14).map(|x| buf[(x, y)].clone()).collect::<Vec<_>>();
            }
        }
        panic!("no Logitech Features:\n{}", app.screen());
    };
    let hidpp = |app: &App| {
        app.m
            .hits
            .iter()
            .any(|h| matches!(h.action, Action::Hidpp(_)))
    };
    let idle = options(&mut app);
    assert!(hidpp(&app));
    assert!(!idle[0].modifier.contains(ratatui::style::Modifier::DIM));
    // While the device's settings are applied, both options keep their place
    // and width, dim and without targets, and send nothing.
    app.edit(|st| st.devices[0].settings_state = SettingsState::Applying);
    let busy = options(&mut app);
    assert_eq!(busy.len(), idle.len());
    assert!(
        busy.iter()
            .filter(|c| c.symbol() != " ")
            .all(|c| c.modifier.contains(ratatui::style::Modifier::DIM))
    );
    assert!(!hidpp(&app), "{}", app.screen());
    for on in [true, false] {
        app.m.action(Action::Hidpp(on));
    }
    assert!(!app.ran("Hidpp("), "{:?}", app.calls());
}

#[test]
fn link_security_follows_the_connection() {
    use cordial_protocol::messages::ConnectionSecurity;
    let mut app = App::connected(200, 40);
    app.m.selected = "d_1".into();
    let screen = app.screen();
    for text in [
        "Encrypted, Unauthenticated",
        "● Encryption: Yes",
        "○ Authenticated Pairing (MITM Protection): No",
        "● Secure Connections: Yes",
        "· Encryption Key: 128 bits",
        "● Saved Bond: Yes",
        "Enc · unauth",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    // Unreported properties are marked, and an unencrypted link is flagged.
    app.edit(|st| {
        st.devices[0].security = Some(ConnectionSecurity {
            encrypted: Some(false),
            ..Default::default()
        });
        st.devices[1].security = None;
    });
    let screen = app.screen();
    for text in [
        "Security               Not Encrypted",
        "○ Encryption: No",
        "? Authenticated Pairing (MITM Protection): Not Reported",
        "? Encryption Key: Not Reported",
        "Unencrypted",
        "Unreported",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    app.m.selected = "d_2".into();
    let screen = app.screen();
    assert!(
        screen.contains("Security               Not Reported"),
        "{screen}"
    );
    // Every reported key length is a neutral number, never approved or
    // unknown; a missing one stays marked not reported.
    app.m.selected = "d_1".into();
    for (bytes, text) in [
        (7, "· Encryption Key: 56 bits"),
        (16, "· Encryption Key: 128 bits"),
    ] {
        app.edit(|st| {
            st.devices[0].security = Some(ConnectionSecurity {
                key_size: Some(bytes),
                ..fixture::device("d", "d").security.unwrap()
            });
        });
        let mut buf = Buffer::empty(Rect::new(0, 0, 170, 40));
        app.m.render(&mut buf);
        let (x, y) = (0..40u16)
            .find_map(|y| {
                let row: String = (0..170u16).map(|x| buf[(x, y)].symbol()).collect();
                row.find(text).map(|i| (row[..i].chars().count() as u16, y))
            })
            .unwrap_or_else(|| panic!("{text}:\n{}", app.screen()));
        for dx in 0..text.chars().count() as u16 {
            let cell = &buf[(x + dx, y)];
            assert_eq!(cell.fg, ratatui::style::Color::Reset, "{text} at {dx}");
            assert!(
                !cell.modifier.contains(ratatui::style::Modifier::DIM),
                "{text}"
            );
        }
    }
    // A disconnected device's leftover report is never shown.
    app.edit(|st| {
        for d in &mut st.devices {
            d.state = ConnectionState::Disconnected;
            d.security = fixture::device("d", "d").security;
        }
    });
    let screen = app.screen();
    assert!(
        !screen.contains("Security") && !screen.contains("Enc ·") && !screen.contains("Encryption"),
        "{screen}"
    );
    // Narrow lists drop the column; details still show the link.
    let mut narrow = App::connected(50, 40);
    narrow.m.selected = "d_1".into();
    let screen = narrow.screen();
    assert!(
        !screen.contains("Enc · unauth") && screen.contains("Encrypted,"),
        "{screen}"
    );
}

fn offer(app: &mut App, caps: &[Capability]) {
    app.edit(|st| st.capabilities = Capabilities(caps.to_vec()));
}

fn reachable(app: &mut App, action: &Action) -> bool {
    app.render();
    app.m.hits.iter().any(|h| h.action == *action)
}

#[test]
fn scan_controls_follow_independent_transports() {
    use Capability::{Ble, Classic};
    let cases: [(&[Capability], Option<&str>, &str); 4] = [
        (&[Classic, Ble], Some("Scan(Both)"), "[Scan ▾]"),
        (&[Ble], Some("Scan(Ble)"), "[Scan BLE]"),
        (&[Classic], Some("Scan(Classic)"), "[Scan Classic]"),
        (&[], None, ""),
    ];
    for (transports, sent, button) in cases {
        let mut app = App::connected(100, 30);
        offer(&mut app, transports);
        let screen = app.screen();
        if button.is_empty() {
            assert!(!screen.contains("[Scan"), "{screen}");
            assert!(!screen.contains("s scan"), "hints agree");
            assert!(!screen.contains("Use Scan"));
        } else {
            assert!(screen.contains(button), "{transports:?}\n{screen}");
            assert!(screen.contains("s scan"));
        }
        app.press(KeyCode::Char('s'));
        match sent {
            Some(sent) => assert!(app.ran(sent), "{transports:?}: {:?}", app.calls()),
            None => assert!(app.calls().is_empty(), "{:?}", app.calls()),
        }
        // Every scan the adapter doesn't offer is unreachable and inert.
        for t in [
            ScanTransport::Both,
            ScanTransport::Ble,
            ScanTransport::Classic,
        ] {
            let offered = match transports.len() {
                2 => true,
                1 => {
                    t != ScanTransport::Both
                        && transports[0]
                            == if t == ScanTransport::Ble {
                                Ble
                            } else {
                                Classic
                            }
                }
                _ => false,
            };
            app.fake.calls.borrow_mut().clear();
            app.m.action(Action::Scan(t));
            assert_eq!(app.calls().is_empty(), !offered, "{transports:?} {t:?}");
        }
    }
    let mut app = App::connected(100, 30);
    offer(&mut app, &[Ble]);
    app.click(Action::Scan(ScanTransport::Ble));
    assert!(!reachable(&mut app, &Action::Menu(Menu::Scan)));
    app.edit(|st| {
        st.pending.push(Pending {
            id: RequestId::try_from(3).unwrap(),
            device_id: None,
            command: "discovery.scan",
            target: None,
        })
    });
    assert!(app.screen().contains("Scanning BLE"));
    let mut both = App::connected(100, 30);
    both.click(Action::Menu(Menu::Scan));
    let screen = both.screen();
    assert!(screen.contains("Bluetooth LE and Classic") && screen.contains("Classic only"));
}

#[test]
fn no_transports_keeps_management_actions() {
    let mut app = App::connected(100, 34);
    offer(&mut app, &[]);
    app.click(Action::Device("c_2".into()));
    assert!(!reachable(&mut app, &Action::Pair));
    assert!(reachable(&mut app, &Action::Hide), "hiding is local");
    app.press(KeyCode::Char('p'));
    app.press(KeyCode::Enter);
    assert!(app.calls().is_empty(), "{:?}", app.calls());
    app.edit(|st| st.devices[0].state = ConnectionState::Disconnected);
    app.click(Action::Device("d_1".into()));
    assert!(!reachable(&mut app, &Action::Connect));
    assert!(!app.screen().contains("⏎ connect"));
    app.press(KeyCode::Char('c'));
    assert!(app.calls().is_empty());
    for action in [
        Action::Untrust,
        Action::Block,
        Action::Remove,
        Action::DeviceSettings,
        Action::Hidpp(false),
    ] {
        assert!(reachable(&mut app, &action), "{action:?}");
    }
    app.press(KeyCode::Char('t'));
    assert!(app.ran("Trusted(\"d_1\", false)"));
    app.click(Action::Help);
    let help = app.screen();
    assert!(
        !help.contains("Pair a nearby") && help.contains("Use a saved device for connections"),
        "{help}"
    );
}

#[test]
fn switching_adapters_drops_stale_options() {
    let mut app = App::connected(100, 30);
    app.click(Action::Menu(Menu::Scan));
    assert!(reachable(&mut app, &Action::Scan(ScanTransport::Classic)));
    app.m.action(Action::Adapters);
    app.m.update(Msg::Ports(Ok(vec![
        port("/dev/ttyACM0"),
        port("/dev/ttyACM1"),
    ])));
    app.click(Action::Port("/dev/ttyACM1".into()));
    assert_eq!(app.m.menu, None);
    let session = app.fake.next.get();
    let mut st = fixture::offering(&[Capability::Ble]);
    st.session = session;
    *app.fake.state.borrow_mut() = Some(st);
    app.phase(session, Phase::Opened);
    assert!(!app.screen().contains("[Scan"), "nothing while loading");
    app.phase(session, Phase::Ready);
    let screen = app.screen();
    assert!(
        screen.contains("[Scan BLE]") && !screen.contains("s stop scan"),
        "{screen}"
    );
    assert!(!reachable(&mut app, &Action::Menu(Menu::Scan)));
    assert!(!reachable(&mut app, &Action::Scan(ScanTransport::Classic)));
    app.fake.calls.borrow_mut().clear();
    app.m.action(Action::Scan(ScanTransport::Classic));
    assert!(app.calls().is_empty());
}

#[test]
fn nearby_candidates_always_pair_and_saved_rows_never_pair() {
    let mut app = App::connected(100, 34);
    app.edit(|st| st.devices[0].state = ConnectionState::Disconnected);
    // Saved: no pairing control, and no key or older frame's click pairs.
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    for absent in ["[Pair again]", "[Pair]", "⏎ pair"] {
        assert!(!screen.contains(absent), "{absent} shown:\n{screen}");
    }
    app.press(KeyCode::Char('p'));
    app.m.action(Action::Pair);
    app.press(KeyCode::Enter);
    assert!(
        app.ran("Connect(\"d_1\")") && !app.ran("Pair("),
        "{:?}",
        app.calls()
    );
    // A Nearby entry sharing a saved name is never associated with it.
    app.click(Action::Device("c_1".into()));
    let screen = app.screen();
    assert_eq!(app.m.selected, "c_1");
    for absent in [
        "Already paired",
        "Needs pairing",
        "[Pair again]",
        "Saved as",
    ] {
        assert!(!screen.contains(absent), "{absent} shown:\n{screen}");
    }
    assert!(
        screen.contains("[Pair]") && screen.contains("renews its bond"),
        "{screen}"
    );
    assert!(screen.contains("[Hide]"), "{screen}");
    app.press(KeyCode::Enter);
    assert!(app.ran("Pair(\"c_1\")"), "{:?}", app.calls());
    // Pairing shows on the Nearby row only; the saved row stays usable.
    app.edit(|st| {
        st.pending.push(Pending {
            id: RequestId::try_from(4).unwrap(),
            device_id: None,
            command: "pairing.start",
            target: Some("c_1".into()),
        })
    });
    let screen = app.screen();
    assert!(
        screen.contains("Pairing…") && screen.contains("[Cancel Pairing]"),
        "{screen}"
    );
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    assert!(
        !screen.contains("[Cancel pairing]") && screen.contains("[Remove]"),
        "{screen}"
    );
    app.click(Action::Device("c_1".into()));
    app.click(Action::Cancel(RequestId::try_from(4).unwrap()));
    assert!(app.ran("Cancel("));
}

#[test]
fn stale_pair_actions_send_nothing() {
    let mut app = App::connected(100, 34);
    // A candidate the scan no longer lists.
    app.click(Action::Device("c_2".into()));
    app.edit(|st| st.candidates.retain(|c| c.candidate_id.0 != "c_2"));
    app.m.action(Action::Pair);
    assert!(!app.ran("Pair("), "{:?}", app.calls());
    // Hide is local and never touches a saved device.
    app.click(Action::Device("c_1".into()));
    app.click(Action::Hide);
    assert!(
        app.calls()
            .iter()
            .any(|c| matches!(c, Call::Hide(id) if id == "c_1")),
        "{:?}",
        app.calls()
    );
    assert!(!app.ran("Remove("));
}

#[test]
fn unavailable_pairing_explains_its_reason_and_sends_nothing() {
    use cordial_protocol::errors::PairUnavailable;
    let mut app = App::connected(100, 34);
    app.edit(|st| {
        let p = &mut st.status.capacity.pairing[1];
        assert_eq!(p.transport, Transport::Ble);
        p.available = false;
        p.reason = Some(PairUnavailable::StorageFull);
        p.estimated_additional = 0;
    });
    app.click(Action::Device("c_1".into()));
    let screen = app.screen();
    assert!(!screen.contains("[Pair]"), "{screen}");
    assert!(screen.contains("Storage Full"), "{screen}");
    app.press(KeyCode::Enter);
    app.press(KeyCode::Char('p'));
    app.m.action(Action::Pair);
    assert!(!app.ran("Pair("), "{:?}", app.calls());
    // Scanning stays available while pairing is full.
    assert!(app.screen().contains("[Scan"), "{}", app.screen());
    // Another transport with room still pairs.
    app.edit(|st| st.candidates[0].transport = Transport::Classic);
    app.click(Action::Pair);
    assert!(app.ran("Pair(\"c_1\")"), "{:?}", app.calls());
}

#[test]
fn saved_devices_enable_and_disable_without_pairing() {
    use cordial_protocol::errors::DisabledReason;
    let mut app = App::connected(100, 34);
    app.edit(|st| {
        let d = &mut st.devices[0];
        d.state = ConnectionState::Disconnected;
        d.security = None;
        d.enabled = false;
        d.effective_enabled = false;
        d.enabled_reason = Some(DisabledReason::Disabled);
    });
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    assert!(
        screen.contains("○ Disabled") && screen.contains("Use This Device        [○ On] [● Off]"),
        "{screen}"
    );
    assert!(!screen.contains("[Connect]"), "{screen}");
    app.m.action(Action::Connect);
    assert!(!app.ran("Connect("), "{:?}", app.calls());
    assert!(app.screen().contains("can't connect now: it is disabled"));
    app.click(Action::Enable);
    assert!(app.ran("Enabled(\"d_1\", true)"), "{:?}", app.calls());
    // Preferred, but no enabled place is free: Use This Device stays On, and
    // Connect isn't offered.
    app.edit(|st| {
        let d = &mut st.devices[0];
        d.enabled = true;
        d.enabled_reason = Some(DisabledReason::Capacity);
    });
    let screen = app.screen();
    assert!(
        screen.contains("! Inactive")
            && screen.contains("Use This Device        [● On] [○ Off]")
            && !screen.contains("[Connect]"),
        "{screen}"
    );
    app.press(KeyCode::Char('e'));
    assert!(app.ran("Enabled(\"d_1\", false)"), "{:?}", app.calls());
}

#[test]
fn unsupported_transport_keeps_the_device_without_connect() {
    use cordial_protocol::errors::DisabledReason;
    let mut app = App::connected(100, 34);
    app.edit(|st| {
        let d = &mut st.devices[0];
        d.transport = Transport::Classic;
        d.state = ConnectionState::Disconnected;
        d.security = None;
        d.transport_supported = false;
        d.effective_enabled = false;
        d.enabled_reason = Some(DisabledReason::UnsupportedTransport);
    });
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    assert!(
        screen.contains("! Unsupported") && screen.contains("bond and settings are kept"),
        "{screen}"
    );
    assert!(!screen.contains("[Connect]"), "{screen}");
    for shown in ["Use This Device", "[Settings…]", "[Remove]"] {
        assert!(screen.contains(shown), "{shown} missing:\n{screen}");
    }
}

#[test]
fn needs_pairing_keeps_settings_and_pairs_only_from_nearby() {
    let mut app = App::connected(100, 48);
    app.edit(|st| {
        let d = &mut st.devices[0];
        d.pairing_state = PairingState::NeedsPairing;
        d.validation_error = Some(cordial_protocol::errors::ValidationError::BondMissing);
        d.effective_enabled = false;
        d.enabled_reason = Some(cordial_protocol::errors::DisabledReason::Invalid);
        d.state = ConnectionState::Disconnected;
        d.security = None;
    });
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    for shown in [
        "Needs Pairing",
        "Unavailable Until",
        "Not Until Paired",
        "then choose Pair on it",
        "saved bond is",
        "[Settings…]",
        "[Remove]",
    ] {
        assert!(screen.contains(shown), "{shown} missing:\n{screen}");
    }
    for absent in [
        "[Connect]",
        "[Pair again]",
        "[Pair]",
        "Reconnect              Automatic",
    ] {
        assert!(!screen.contains(absent), "{absent} shown:\n{screen}");
    }
    app.press(KeyCode::Enter);
    app.press(KeyCode::Char('p'));
    app.press(KeyCode::Char('c'));
    app.m.action(Action::Pair);
    app.m.action(Action::Connect);
    assert!(
        !app.ran("Pair(") && !app.ran("Connect("),
        "{:?}",
        app.calls()
    );
    assert!(
        app.screen()
            .contains("can't connect now: it needs pairing again")
    );
    // Its Nearby entry pairs it.
    app.click(Action::Device("c_1".into()));
    app.press(KeyCode::Enter);
    assert!(app.ran("Pair(\"c_1\")"), "{:?}", app.calls());
    app.click(Action::Device("d_1".into()));
    app.click(Action::Remove);
    assert!(app.screen().contains("Remove Test keyboard?"));
}

#[test]
fn paired_devices_offer_connect_but_no_pairing() {
    let mut app = App::connected(100, 34);
    app.edit(|st| {
        st.devices[0].state = ConnectionState::Disconnected;
        st.devices[0].security = None;
    });
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    assert!(screen.contains("[Connect]"), "{screen}");
    for absent in ["[Scan to pair again]", "[Pair again]", "Already paired"] {
        assert!(!screen.contains(absent), "{absent} shown:\n{screen}");
    }
    app.press(KeyCode::Enter);
    assert!(app.ran("Connect(\"d_1\")") && !app.ran("Scan(") && !app.ran("Pair("));
}

#[test]
fn development_functions_follow_capabilities_not_profile() {
    let mut app = App::connected(100, 48);
    app.click(Action::Menu(Menu::Adapter));
    assert!(reachable(&mut app, &Action::Bootloader));
    assert!(reachable(&mut app, &Action::FilesOpen));
    app.press(KeyCode::Esc);
    offer(&mut app, &[Capability::Ble]);
    app.click(Action::Menu(Menu::Adapter));
    assert!(!reachable(&mut app, &Action::Bootloader));
    assert!(!reachable(&mut app, &Action::FilesOpen));
    app.press(KeyCode::Esc);
    app.m.action(Action::Bootloader);
    app.m.action(Action::FilesOpen);
    assert!(app.m.dialog.is_none() && !app.m.files.open && app.calls().is_empty());
    // A production-profile build that advertises them still offers them.
    offer(
        &mut app,
        &[Capability::Debug, Capability::StorageManagement],
    );
    app.edit(|st| st.status.build_profile = BuildProfile::Production);
    app.click(Action::Menu(Menu::Adapter));
    assert!(reachable(&mut app, &Action::Bootloader) && reachable(&mut app, &Action::FilesOpen));
    // Management of saved devices needs no optional capability.
    app.press(KeyCode::Esc);
    app.click(Action::Device("d_1".into()));
    assert!(reachable(&mut app, &Action::Untrust) && reachable(&mut app, &Action::Remove));
}

fn entry(
    name: &str,
    kind: cordial_protocol::payloads::FileType,
    size: usize,
) -> cordial_protocol::payloads::FileEntry {
    cordial_protocol::payloads::FileEntry {
        name: name.into(),
        kind,
        size,
    }
}

impl App {
    fn notice(&mut self, notice: crate::controller::Notice) {
        let session = self.m.session.unwrap();
        self.m
            .update(Msg::Controller(Box::new(Event::Notice { session, notice })));
    }
    fn finish_ticket(&mut self, ticket: Ticket, result: Result<Outcome, Failure>) {
        let session = self.m.session.unwrap();
        self.m.update(Msg::Controller(Box::new(Event::Done {
            session,
            ticket,
            result,
        })));
    }
    /// Opens Files from the Adapter menu and delivers the root's rows.
    fn open_files(&mut self) -> Ticket {
        use cordial_protocol::payloads::FileType::{Directory, File};
        self.click(Action::Menu(Menu::Adapter));
        self.click(Action::FilesOpen);
        assert!(self.ran("StorageList(\"/\")"), "{:?}", self.calls());
        let ticket = self.m.files.listing.as_ref().unwrap().0;
        assert!(self.screen().contains("Reading…"));
        self.notice(crate::controller::Notice::StorageEntries {
            ticket,
            entries: vec![
                entry("device.json", File, 800),
                entry("bonds", Directory, 0),
            ],
        });
        ticket
    }
}

#[test]
fn files_list_incrementally_and_navigate_by_mouse_and_keyboard() {
    use cordial_protocol::payloads::FileType::File;
    let mut app = App::connected(100, 30);
    let ticket = app.open_files();
    let screen = app.screen();
    // Rows show while the listing is still running; directories come first.
    assert!(
        screen.contains("Files · /") && screen.contains("Reading…"),
        "{screen}"
    );
    let bonds = screen.find("bonds/").unwrap();
    assert!(bonds < screen.find("device.json").unwrap());
    assert!(screen.contains("800 B"));
    app.finish_ticket(
        ticket,
        Ok(Outcome::StorageListed {
            path: "/".into(),
            count: 2,
        }),
    );
    assert!(!app.screen().contains("Reading…"));
    // Clicking a directory opens it, without preloading anything else.
    app.fake.calls.borrow_mut().clear();
    app.click(Action::FilesEntry("bonds".into()));
    assert!(app.ran("StorageList(\"/bonds\")"), "{:?}", app.calls());
    assert!(app.screen().contains("Files · /bonds"));
    let ticket = app.m.files.listing.as_ref().unwrap().0;
    app.notice(crate::controller::Notice::StorageEntries {
        ticket,
        entries: vec![entry("0001.bin", File, 70)],
    });
    let mut error = Error::new("storage_failed");
    error.wire = Some(Box::new(ErrorCode::StorageFailed.into()));
    app.finish_ticket(ticket, Err(error.into()));
    let screen = app.screen();
    assert!(
        screen.contains("0001.bin") && screen.contains("Incomplete"),
        "{screen}"
    );
    // Backspace goes up; arrows select; Enter on a directory opens it.
    app.fake.calls.borrow_mut().clear();
    app.press(KeyCode::Backspace);
    assert!(app.ran("StorageList(\"/\")"));
    let ticket = app.m.files.listing.as_ref().unwrap().0;
    app.notice(crate::controller::Notice::StorageEntries {
        ticket,
        entries: vec![
            entry("device.json", File, 800),
            entry("bonds", cordial_protocol::payloads::FileType::Directory, 0),
        ],
    });
    app.press(KeyCode::Down);
    assert_eq!(app.m.files.selected.as_deref(), Some("bonds"));
    app.press(KeyCode::Down);
    assert_eq!(app.m.files.selected.as_deref(), Some("device.json"));
    assert_eq!(app.m.files.dest.value(), "device.json");
    app.press(KeyCode::Up);
    app.fake.calls.borrow_mut().clear();
    app.press(KeyCode::Enter);
    assert!(app.ran("StorageList(\"/bonds\")"));
    // A stale listing's rows are ignored.
    app.notice(crate::controller::Notice::StorageEntries {
        ticket,
        entries: vec![entry("stale", File, 1)],
    });
    assert!(!app.screen().contains("stale"));
    // Esc closes Files and the device list returns.
    app.press(KeyCode::Esc);
    assert!(!app.m.files.open && app.screen().contains("SAVED"));
}

#[test]
fn download_confirms_replacing_shows_progress_and_cancels() {
    let dir = std::env::temp_dir().join(format!("cordial-tui-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let existing = dir.join("device.json");
    std::fs::write(&existing, b"old").unwrap();
    let mut app = App::connected(100, 30);
    app.open_files();
    app.click(Action::FilesEntry("device.json".into()));
    // The destination is edited in its field.
    app.click(Action::FilesDest);
    assert!(app.m.files.editing);
    for _ in 0.."device.json".len() {
        app.press(KeyCode::Backspace);
    }
    app.m.update(Msg::Paste(existing.display().to_string()));
    app.fake.calls.borrow_mut().clear();
    app.press(KeyCode::Enter);
    // An existing file asks first and nothing is sent.
    assert!(
        matches!(app.m.dialog, Some(Dialog::Replace(_))),
        "{:?}",
        app.m.dialog
    );
    assert!(app.screen().contains("Replace File"));
    assert!(app.calls().is_empty());
    app.press(KeyCode::Char('n'));
    assert!(app.m.dialog.is_none() && app.calls().is_empty());
    app.click(Action::FilesDownload);
    app.click(Action::Confirm);
    assert!(app.ran("overwrite: true"), "{:?}", app.calls());
    let ticket = app.m.files.download.as_ref().unwrap().ticket;
    app.notice(crate::controller::Notice::StorageProgress { ticket, bytes: 400 });
    let screen = app.screen();
    assert!(
        screen.contains("Downloading…") && screen.contains("400 B of 800 B"),
        "{screen}"
    );
    // Heartbeats and control keep running: other commands still send.
    app.click(Action::FilesCancel);
    let cancel = app.m.files.download.as_ref().unwrap().cancel.clone();
    assert!(cancel.cancelled());
    let mut error = Error::new("operation cancelled");
    error.wire = None;
    app.finish_ticket(ticket, Err(error.into()));
    let screen = app.screen();
    assert!(
        screen.contains("Cancelled; no file was saved.") && screen.contains("[Retry]"),
        "{screen}"
    );
    assert_eq!(std::fs::read(&existing).unwrap(), b"old");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn changed_storage_offers_retry_and_success_is_reported() {
    let mut app = App::connected(100, 30);
    app.open_files();
    app.click(Action::FilesEntry("device.json".into()));
    let dest = std::env::temp_dir().join(format!("cordial-tui-new-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&dest);
    app.m.files.dest.set_value(&dest.display().to_string());
    app.click(Action::FilesDownload);
    assert!(app.ran("overwrite: false"));
    let ticket = app.m.files.download.as_ref().unwrap().ticket;
    let mut error = Error::new("storage_changed");
    error.wire = Some(Box::new(ErrorCode::StorageChanged.into()));
    app.finish_ticket(ticket, Err(error.into()));
    assert!(app.screen().contains("files changed during the download"));
    app.fake.calls.borrow_mut().clear();
    app.click(Action::FilesRetry);
    assert!(app.ran("StorageGet"), "{:?}", app.calls());
    let ticket = app.m.files.download.as_ref().unwrap().ticket;
    app.finish_ticket(
        ticket,
        Ok(Outcome::StorageSaved {
            path: "/device.json".into(),
            local: dest.clone(),
            bytes: 800,
        }),
    );
    let screen = app.screen();
    assert!(
        screen.contains("Saved") && screen.contains("800 bytes"),
        "{screen}"
    );
}

#[test]
fn losing_or_switching_the_adapter_cancels_and_clears_files() {
    let mut app = App::connected(100, 30);
    app.open_files();
    app.click(Action::FilesEntry("device.json".into()));
    app.m.files.dest.set_value("/nonexistent-dir-cordial/x");
    app.click(Action::FilesDownload);
    let cancel = app.m.files.download.as_ref().unwrap().cancel.clone();
    let listing = app.m.files.listing.as_ref().map(|l| l.1.clone());
    let session = app.m.session.unwrap();
    app.phase(session, Phase::Lost(Error::new("unplugged")));
    assert!(cancel.cancelled());
    assert!(listing.is_none_or(|l| l.cancelled()));
    assert!(!app.m.files.open && app.m.files.entries.is_empty());
    let mut app = App::connected(100, 30);
    app.open_files();
    app.m.action(Action::Adapters);
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM1")])));
    app.click(Action::Port("/dev/ttyACM1".into()));
    assert!(!app.m.files.open && app.m.files.path.is_empty());
}

#[test]
fn radio_failure_keeps_files_available() {
    let mut app = App::new(100, 30, None);
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
    let session = app.fake.next.get();
    let mut st = fixture::state();
    st.session = session;
    st.ready = false;
    st.devices.clear();
    st.status.radio_ready = false;
    *app.fake.state.borrow_mut() = Some(st);
    app.phase(session, Phase::Opened);
    let mut error = Error::new("radio_unavailable");
    error.wire = Some(Box::new(ErrorCode::RadioUnavailable.into()));
    app.phase(session, Phase::Failed { error, open: true });
    let screen = app.screen();
    assert!(
        screen.contains("isn't ready") && screen.contains("[Files]"),
        "{screen}"
    );
    app.fake.calls.borrow_mut().clear();
    app.click(Action::FilesOpen);
    assert!(app.ran("StorageList(\"/\")"), "{:?}", app.calls());
    // Without the capability, the banner offers no Files.
    let mut app = App::new(100, 30, None);
    app.m.update(Msg::Ports(Ok(vec![port("/dev/ttyACM0")])));
    let session = app.fake.next.get();
    let mut st = fixture::offering(&[Capability::Ble]);
    st.session = session;
    *app.fake.state.borrow_mut() = Some(st);
    app.phase(session, Phase::Opened);
    app.phase(
        session,
        Phase::Failed {
            error: Error::new("radio_unavailable"),
            open: true,
        },
    );
    assert!(!app.screen().contains("[Files]"));
}

#[test]
fn large_directories_keep_every_row() {
    use cordial_protocol::payloads::FileType::File;
    let mut app = App::connected(100, 30);
    let ticket = app.open_files();
    for batch in 0..200 {
        let entries = (0..32)
            .map(|i| entry(&format!("f{:05}", batch * 32 + i), File, 1))
            .collect();
        app.notice(crate::controller::Notice::StorageEntries { ticket, entries });
    }
    assert_eq!(app.m.files.entries.len(), 2 + 6400);
    app.press(KeyCode::End);
    assert_eq!(app.m.files.selected.as_deref(), Some("f06399"));
    assert!(app.screen().contains("f06399"));
}

#[test]
fn retry_uses_the_failed_transfer_not_the_current_selection() {
    use cordial_protocol::payloads::FileType::File;
    let dir = std::env::temp_dir().join(format!("cordial-tui-retry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a_local = dir.join("a.json");
    let mut app = App::connected(100, 30);
    app.open_files();
    app.click(Action::FilesEntry("device.json".into()));
    app.m.files.dest.set_value(&a_local.display().to_string());
    app.click(Action::FilesDownload);
    let ticket = app.m.files.download.as_ref().unwrap().ticket;
    let mut error = Error::new("storage_changed");
    error.wire = Some(Box::new(ErrorCode::StorageChanged.into()));
    app.finish_ticket(ticket, Err(error.into()));
    // Meanwhile another directory and file are chosen with another name.
    app.click(Action::FilesEntry("bonds".into()));
    let listing = app.m.files.listing.as_ref().unwrap().0;
    app.notice(crate::controller::Notice::StorageEntries {
        ticket: listing,
        entries: vec![entry("b.bin", File, 9)],
    });
    app.click(Action::FilesEntry("b.bin".into()));
    assert_eq!(
        app.m.files.dest.value(),
        dir.join("b.bin").display().to_string()
    );
    app.fake.calls.borrow_mut().clear();
    app.m.action(Action::FilesRetry);
    let a = format!(
        "StorageGet {{ path: \"/device.json\", local: {:?}, overwrite: false }}",
        a_local
    );
    assert_eq!(app.calls(), [Call::Run(a)]);
    // Retrying after the destination appeared asks about that file, and the
    // confirmation replaces it, still for the failed transfer.
    let ticket = app.m.files.download.as_ref().unwrap().ticket;
    app.finish_ticket(ticket, Err(Error::new("timeout").into()));
    std::fs::write(&a_local, b"appeared").unwrap();
    app.fake.calls.borrow_mut().clear();
    app.m.action(Action::FilesRetry);
    assert!(app.calls().is_empty());
    assert!(
        matches!(&app.m.dialog, Some(Dialog::Replace(t)) if t.path == "/device.json" && t.local == a_local)
    );
    app.m.action(Action::Confirm);
    let replace = format!(
        "StorageGet {{ path: \"/device.json\", local: {:?}, overwrite: true }}",
        a_local
    );
    assert_eq!(app.calls(), [Call::Run(replace)]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn selecting_another_file_keeps_the_chosen_directory() {
    use super::files::keep_directory;
    assert_eq!(keep_directory("", "b.bin"), "b.bin");
    assert_eq!(keep_directory("a.json", "b.bin"), "b.bin");
    assert_eq!(keep_directory("/tmp/out/a.json", "b.bin"), "/tmp/out/b.bin");
    assert_eq!(keep_directory("/tmp/out/", "b.bin"), "/tmp/out/b.bin");
    let mut app = App::connected(100, 30);
    app.open_files();
    app.click(Action::FilesEntry("device.json".into()));
    let dir = std::env::temp_dir().join(format!("cordial-keep-{}", std::process::id()));
    app.m
        .files
        .dest
        .set_value(&dir.join("custom.json").display().to_string());
    app.click(Action::FilesDownload);
    let ticket = app.m.files.download.as_ref().unwrap().ticket;
    app.finish_ticket(ticket, Err(Error::new("timeout").into()));
    app.press(KeyCode::Up);
    assert_eq!(app.m.files.selected.as_deref(), Some("bonds"));
    app.press(KeyCode::Down);
    assert_eq!(
        app.m.files.dest.value(),
        dir.join("device.json").display().to_string()
    );
    // The failed transfer still retries its own destination.
    app.fake.calls.borrow_mut().clear();
    app.m.action(Action::FilesRetry);
    assert!(
        app.ran(&format!("{:?}", dir.join("custom.json"))),
        "{:?}",
        app.calls()
    );
}

#[test]
fn unnamed_devices_of_unknown_kind_are_hidden_until_shown() {
    use crate::{client::Envelope, controller::Notice};
    use cordial_protocol::messages::DeviceKind;
    let mut app = App::connected(100, 34);
    let unnamed = |id: &str, kind| {
        let mut c = fixture::candidate(id, "");
        c.name = None;
        c.kind = kind;
        c
    };
    app.edit(|st| {
        st.candidates.push(unnamed("c_k", DeviceKind::Keyboard));
        st.candidates.push(unnamed("c_m", DeviceKind::Mouse));
        st.candidates
            .push(unnamed("c_km", DeviceKind::KeyboardMouse));
        st.candidates.push(unnamed("c_u", DeviceKind::Unknown));
        st.candidates.push(unnamed("c_v", DeviceKind::Unknown));
    });
    // Off in each session: unnamed devices of a known kind are listed by it.
    let screen = app.screen();
    for shown in [
        "Test keyboard",
        "Unnamed Keyboard ",
        "Unnamed Mouse",
        "Unnamed Keyboard/Mouse",
        "Show Unnamed Devices [○ On] [● Off]",
        "2 unnamed devices hidden",
    ] {
        assert!(screen.contains(shown), "{shown} missing:\n{screen}");
    }
    assert!(!screen.contains("Unnamed Device "), "{screen}");
    // Navigation skips hidden rows and actions refuse them.
    app.click(Action::Device("c_km".into()));
    app.press(KeyCode::Down);
    assert_eq!(app.m.selected, "c_km");
    app.m.selected = "c_u".into();
    app.m.action(Action::Pair);
    app.m.action(Action::Hide);
    assert!(
        !app.ran("Pair(") && !app.calls().iter().any(|c| matches!(c, Call::Hide(_))),
        "{:?}",
        app.calls()
    );
    // Hidden devices report nothing in the activity; typed ones use their kind.
    let session = app.m.session.unwrap();
    let before = app.m.logs.len();
    let found = |app: &mut App, c: &Candidate| {
        let notice = Notice::Message {
            envelope: Envelope {
                message: cordial_protocol::messages::Message::event(
                    "discovery.result".into(),
                    None,
                    serde_json::to_value(c).unwrap(),
                ),
                raw: String::new(),
                command: None,
                internal: false,
                sequence: 0,
            },
            background: false,
        };
        app.m
            .update(Msg::Controller(Box::new(Event::Notice { session, notice })));
    };
    found(&mut app, &unnamed("c_u", DeviceKind::Unknown));
    found(&mut app, &unnamed("c_m", DeviceKind::Mouse));
    let lines: Vec<&str> = app.m.logs[before..]
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(lines, ["Found Unnamed Mouse (BLE)"]);
    // A device being paired stays listed.
    app.edit(|st| {
        st.pending.push(Pending {
            id: RequestId::try_from(4).unwrap(),
            device_id: None,
            command: "pairing.start",
            target: Some("c_u".into()),
        })
    });
    assert!(app.screen().contains("1 unnamed device hidden"));
    app.edit(|st| st.pending.clear());
    // Showing lists them all; a later name or kind updates the row.
    app.click(Action::ShowUnnamed(true));
    let screen = app.screen();
    assert!(
        screen.contains("Show Unnamed Devices [● On] [○ Off]")
            && screen.contains("Unnamed Device ")
            && !screen.contains("hidden"),
        "{screen}"
    );
    app.m.selected = "c_u".into();
    app.m.action(Action::Pair);
    assert!(app.ran("Pair(\"c_u\")"), "{:?}", app.calls());
    app.click(Action::ShowUnnamed(false));
    app.edit(|st| {
        st.candidates[5].name = Some("Late name".into());
        st.candidates[6].kind = DeviceKind::Mouse;
    });
    let screen = app.screen();
    assert!(
        screen.contains("Late name") && !screen.contains("hidden"),
        "{screen}"
    );
    // Saved devices are never filtered, and a new session starts hidden.
    assert!(
        screen.contains("SAVED") && !app.fake.state.borrow().as_ref().unwrap().devices.is_empty()
    );
    let fresh = Model::new(Fake::default(), None);
    assert!(!fresh.show_unnamed);
}

#[test]
fn hiding_unnamed_devices_drops_their_selection_and_space_chooses() {
    use crate::{client::Envelope, controller::Notice};
    use cordial_protocol::messages::DeviceKind;
    let mut app = App::connected(100, 34);
    let mut c = fixture::candidate("c_u", "");
    c.name = None;
    c.kind = DeviceKind::Unknown;
    app.edit(|st| st.candidates.push(c.clone()));
    // Only hidden entries: the count replaces the empty-list hint.
    let only_hidden = |app: &mut App| {
        app.edit(|st| st.candidates.retain(|c| c.candidate_id.0 == "c_u"));
        let screen = app.screen();
        assert!(
            screen.contains("1 unnamed device hidden")
                && !screen.contains("Use Scan to find")
                && !screen.contains("No nearby devices"),
            "{screen}"
        );
    };
    only_hidden(&mut app);
    app.edit(|st| st.candidates.clear());
    assert!(app.screen().contains("Use Scan to find nearby devices"));
    app.edit(|st| st.candidates.push(c.clone()));
    // Tab reaches each option; Space and Enter choose it, keeping the highlight.
    while app.m.focus != Some(Action::ShowUnnamed(true)) {
        app.press(KeyCode::Tab);
    }
    app.press(KeyCode::Char(' '));
    assert!(app.m.show_unnamed);
    app.click(Action::Device("c_u".into()));
    let screen = app.screen();
    assert!(
        screen.contains("Unnamed Device") && screen.contains("[Pair]"),
        "{screen}"
    );
    // Turning it off deselects it: no details or actions remain, even by key.
    app.m.focus = Some(Action::ShowUnnamed(false));
    app.m.focus_ctx = app.m.focus_context();
    app.press(KeyCode::Char(' '));
    assert!(!app.m.show_unnamed && app.m.selected.is_empty());
    assert_eq!(app.m.focus, Some(Action::ShowUnnamed(false)));
    let screen = app.screen();
    assert!(
        !screen.contains("[Pair]") && screen.contains("Select a device"),
        "{screen}"
    );
    app.m.focus = Some(Action::ShowUnnamed(true));
    app.m.focus_ctx = app.m.focus_context();
    app.press(KeyCode::Enter);
    assert!(app.m.show_unnamed, "Enter chooses the highlighted option");
    app.click(Action::ShowUnnamed(false));
    app.press(KeyCode::Char('p'));
    assert!(!app.ran("Pair("), "{:?}", app.calls());
    // A selection hidden when its pairing ends is dropped at the next frame.
    app.click(Action::ShowUnnamed(true));
    app.click(Action::Device("c_u".into()));
    app.edit(|st| {
        st.pending.push(Pending {
            id: RequestId::try_from(4).unwrap(),
            device_id: None,
            command: "pairing.start",
            target: Some("c_u".into()),
        })
    });
    app.click(Action::ShowUnnamed(false));
    assert_eq!(
        app.m.selected, "c_u",
        "a device being paired stays selected"
    );
    app.edit(|st| st.pending.clear());
    app.render();
    assert!(app.m.selected.is_empty());
    // A name that follows a kind-only report is reported, and later labels
    // use the latest name or kind, even once the candidate is gone.
    let session = app.m.session.unwrap();
    let found = |app: &mut App, c: &Candidate| {
        let notice = Notice::Message {
            envelope: Envelope {
                message: cordial_protocol::messages::Message::event(
                    "discovery.result".into(),
                    None,
                    serde_json::to_value(c).unwrap(),
                ),
                raw: String::new(),
                command: None,
                internal: false,
                sequence: 0,
            },
            background: false,
        };
        app.m
            .update(Msg::Controller(Box::new(Event::Notice { session, notice })));
    };
    let before = app.m.logs.len();
    let mut late = c.clone();
    late.candidate_id = CandidateId("c_late".into());
    found(&mut app, &late);
    late.kind = DeviceKind::Keyboard;
    found(&mut app, &late);
    found(&mut app, &late);
    assert_eq!(app.m.label("c_late"), "Unnamed Keyboard");
    late.name = Some("Desk keyboard".into());
    found(&mut app, &late);
    found(&mut app, &late);
    let lines: Vec<&str> = app.m.logs[before..]
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(
        lines,
        ["Found Unnamed Keyboard (BLE)", "Found Desk keyboard (BLE)"]
    );
    assert_eq!(app.m.label("c_late"), "Desk keyboard");
}

fn info_field(key: InfoKey, instance: u8, value: SettingValue, fresh: bool) -> InfoField {
    InfoField {
        key,
        instance,
        value,
        available: true,
        fresh,
    }
}

#[test]
fn battery_is_listed_once_and_details_show_device_info() {
    let mut app = App::connected(120, 56);
    app.edit(|st| {
        st.info.insert(
            DeviceId("d_1".into()),
            crate::controller::DeviceInfoView {
                current: true,
                revision: 3,
                fields: vec![
                    info_field(InfoKey::Model, 0, SettingValue::Text("K1".into()), true),
                    info_field(
                        InfoKey::Firmware,
                        0,
                        SettingValue::Text("1.2".into()),
                        false,
                    ),
                    info_field(InfoKey::BatteryPercent, 0, SettingValue::Integer(80), true),
                    info_field(InfoKey::BatteryCharging, 0, SettingValue::Bool(true), true),
                    info_field(
                        InfoKey::Serial,
                        0,
                        SettingValue::Text("SN-0123456789ABCDEF".into()),
                        true,
                    ),
                    info_field(
                        InfoKey::VendorIdNamespace,
                        0,
                        SettingValue::Text("bluetooth".into()),
                        true,
                    ),
                    InfoField::unknown(InfoKey::ProductVersion, 0),
                ],
            },
        );
        let d = &mut st.devices[0];
        d.warnings = vec![cordial_protocol::errors::WarningCode::LedOutputUnavailable];
        d.last_error = Some(cordial_protocol::messages::WireError {
            code: ErrorCode::SettingsApplyFailed,
            details: None,
        });
    });
    app.click(Action::Device("d_1".into()));
    let screen = app.screen();
    assert!(screen.contains("80%↑"), "{screen}");
    assert!(screen.contains("Device Info"), "{screen}");
    assert!(screen.contains("Charging"), "{screen}");
    assert!(!screen.contains("Battery 2"), "{screen}");
    let line = |screen: &str, label: &str| {
        screen
            .lines()
            .find(|l| l.contains(label))
            .map(str::to_owned)
            .unwrap_or_default()
    };
    assert!(line(&screen, "Charging").contains("Yes"), "{screen}");
    // Every row of the card, Device Info included, shares one value column
    // past its longest label, with a gap; further lines of a field start
    // there too.
    let label_at = |screen: &str, label: &str| {
        let key = format!("│ {label}  ");
        let l = line(screen, &key);
        let k = l.find(&key).unwrap_or_else(|| panic!("{label}:\n{screen}")) + "│ ".len();
        (l.clone(), text::width(&l[..k]))
    };
    let value_at = |screen: &str, label: &str| {
        let (l, k) = label_at(screen, label);
        let rest = &l[l.find(&format!("│ {label}  ")).unwrap() + "│ ".len() + label.len()..];
        k + text::width(label) + rest.len() - rest.trim_start().len()
    };
    let at = |screen: &str, text: &str| {
        text::width(&line(screen, text)[..line(screen, text).find(text).unwrap()])
    };
    let (_, k) = label_at(&screen, "Vendor ID Namespace");
    let v = value_at(&screen, "Vendor ID Namespace");
    assert_eq!(v, k + 23, "{screen}");
    for label in [
        "Status",
        "Security",
        "Type",
        "Use This Device",
        "Automatic Connections",
        "Block Connections",
        "Reconnect",
        "Logitech Features",
        "Model",
        "Serial Number",
        "Warning",
        "ID",
    ] {
        assert_eq!(label_at(&screen, label).1, k, "{label}:\n{screen}");
        assert_eq!(value_at(&screen, label), v, "{label}:\n{screen}");
    }
    for more in [
        "● Encryption: Yes",
        "● Special-Key Translation",
        "● Settings Ready",
    ] {
        assert_eq!(at(&screen, more), v, "{more}:\n{screen}");
    }
    // Too narrow for a label column: each value sits under its label.
    let mut narrow = App::connected(MIN_WIDTH, 120);
    let (info, device) = {
        let st = app.fake.state.borrow();
        let st = st.as_ref().unwrap();
        (st.info.clone(), st.devices[0].clone())
    };
    narrow.edit(|n| {
        n.info = info;
        n.devices[0] = device;
    });
    narrow.click(Action::Device("d_1".into()));
    let small = narrow.screen();
    let rows: Vec<&str> = small.lines().collect();
    let stacked = |label: &str| {
        let i = rows
            .iter()
            .position(|l| l.split('│').any(|c| c.trim() == label))
            .unwrap_or_else(|| panic!("{label}:\n{small}"));
        let k = text::width(&rows[i][..rows[i].find(label).unwrap()]);
        (i, k)
    };
    let (_, k) = stacked("Status");
    for (label, value) in [
        ("Status", "● Connected"),
        ("Security", "Encrypted"),
        ("Logitech Features", "[● On]"),
        ("Vendor ID Namespace", "Bluetooth"),
        ("Serial Number", "SN-0123456789ABCDEF"),
        ("Warning", ""),
        ("ID", "d_1"),
    ] {
        let (i, at) = stacked(label);
        assert_eq!(at, k, "{label}:\n{small}");
        let next = rows[i + 1];
        let body = next.split('│').nth(1).unwrap_or(next);
        let x =
            text::width(&next[..next.find(body).unwrap()]) + body.len() - body.trim_start().len();
        assert_eq!(x, k + 2, "{label}:\n{small}");
        assert!(next.contains(value), "{label}:\n{small}");
    }
    for more in ["● Encryption: Yes", "● Special-Key Translation"] {
        assert_eq!(at(&small, more), k + 2, "{more}:\n{small}");
    }
    // An unknown charge is never 0%, and "not charging" differs from unknown.
    app.edit(|st| {
        let info = st.info.get_mut(&DeviceId("d_1".into())).unwrap();
        info.fields
            .retain(|f| !matches!(f.key, InfoKey::BatteryPercent | InfoKey::BatteryCharging));
        info.fields
            .push(InfoField::unknown(InfoKey::BatteryPercent, 0));
        info.fields.push(info_field(
            InfoKey::BatteryCharging,
            0,
            SettingValue::Bool(false),
            true,
        ));
    });
    app.render();
    let screen = app.screen();
    assert!(line(&screen, "Battery").contains("Unknown"), "{screen}");
    assert!(line(&screen, "Charging").contains("No"), "{screen}");
    assert!(!screen.contains("0%"), "{screen}");
    assert!(screen.contains("1.2 (last known)"), "{screen}");
    assert!(
        !screen.contains("Product Version"),
        "unreported fields are left out"
    );
    app.click(Action::RefreshInfo);
    assert!(app.ran("DeviceInfoRefresh(\"d_1\")"));
    // A disconnected device can't be asked; its last values stay shown.
    app.edit(|st| st.devices[0].state = ConnectionState::Disconnected);
    app.render();
    assert!(!app.m.hits.iter().any(|h| h.action == Action::RefreshInfo));
    assert!(app.screen().contains("Device Info"));
    // Without Device Info the card keeps one column for its own labels,
    // including a pending HID++ change under its value.
    app.edit(|st| {
        st.info.clear();
        st.pending.push(Pending {
            id: RequestId::try_from(3).unwrap(),
            device_id: None,
            command: "device.hidpp.set",
            target: Some("d_1".into()),
        });
    });
    app.render();
    let screen = app.screen();
    assert!(!screen.contains("Device Info"), "{screen}");
    let (_, k) = label_at(&screen, "Status");
    for label in ["Status", "Logitech Features", "Warning", "ID"] {
        assert_eq!(label_at(&screen, label).1, k, "{label}:\n{screen}");
        assert_eq!(value_at(&screen, label), k + 23, "{label}:\n{screen}");
    }
    assert_eq!(at(&screen, "Saving…") - 2, k + 23, "{screen}");
    // A cached connection error changes neither the status nor the card.
    assert!(
        !screen.contains("Last Error") && !screen.contains("Failed"),
        "{screen}"
    );
}

#[test]
fn rename_dialog_validates_saves_and_preserves_failed_drafts() {
    let mut app = App::connected(100, 40);
    app.edit(|s| s.devices.clear());
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::AdapterSettings);
    app.click(Action::Rename);
    assert_eq!(app.m.form.value(), "Test adapter");
    app.m.form.set_value(&"é".repeat(33));
    app.press(KeyCode::Enter);
    assert!(!app.ran("Name("));
    assert_eq!(app.m.form_err, "Invalid adapter name");
    app.m.form.set_value("  Desk  ");
    app.press(KeyCode::Enter);
    assert!(app.ran("Name(Some(\"Desk\"))"));
    assert!(app.screen().contains("Saving…"));
    app.done(
        "Name(",
        Err(crate::client::Error::new("storage full").into()),
    );
    assert_eq!(app.m.dialog, Some(Dialog::Rename));
    assert_eq!(app.m.form.value(), "  Desk  ");
    assert!(app.m.form_err.contains("storage full"));
    assert!(app.screen().contains("rename the adapter"));
    app.press(KeyCode::Enter);
    app.edit(|s| s.status.name = "Desk".into());
    app.done("Name(", Ok(Outcome::Name("Desk".into())));
    assert_eq!(app.m.dialog, None);
    assert!(!app.m.form_focused);
    assert!(app.screen().contains("Desk"));
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::AdapterSettings);
    app.click(Action::Rename);
    app.m.form.set_value("Cancelled");
    app.press(KeyCode::Esc);
    assert!(!app.ran("Cancelled"));
    assert!(!app.m.form_focused);
    app.click(Action::Menu(Menu::Adapter));
    app.click(Action::AdapterSettings);
    app.click(Action::Rename);
    app.click(Action::ResetName);
    assert!(app.ran("Name(None)"));
    app.edit(|s| s.status.name = "Test adapter".into());
    app.done("Name(", Ok(Outcome::Name("Test adapter".into())));
    assert_eq!(app.m.dialog, None);
    assert!(app.screen().contains("Test adapter"));
}
