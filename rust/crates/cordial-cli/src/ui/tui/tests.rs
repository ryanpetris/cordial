//! Model tests against stand-in sessions: what the TUI draws, and which commands clicks and keys
//! send.
use super::*;
use crate::{
    controller::Target,
    settings::tests::{ranged, sensor, wheel},
    ui::command::tests as fixture,
};
use cordial_protocol::{ConfigurationInterface, DeviceState, InactiveReason, IntegrationKind};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
};
use std::{cell::RefCell, rc::Rc};

/// An error with only a code.
fn err(code: p::ErrorCode) -> p::Error {
    p::Error {
        code: code as i32,
        ..Default::default()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Open(String),
    OpenAdapter(String, String),
    Close(Slot),
    Run(Slot, String),
    List,
    Migrate(String, String),
    CancelMigration,
}

#[derive(Default)]
struct Inner {
    states: HashMap<Slot, State>,
    calls: Vec<Call>,
    next: u64,
}

#[derive(Clone, Default)]
pub struct FakeFleet(Rc<RefCell<Inner>>);

impl Fleet for FakeFleet {
    fn open(&self, port: &str) -> Slot {
        let mut i = self.0.borrow_mut();
        i.calls.push(Call::Open(port.into()));
        i.next += 1;
        i.next
    }
    fn open_adapter(&self, port: &str, id: &str) -> Slot {
        let mut i = self.0.borrow_mut();
        i.calls.push(Call::OpenAdapter(port.into(), id.into()));
        i.next += 1;
        i.next
    }
    fn close(&self, slot: Slot) {
        self.0.borrow_mut().calls.push(Call::Close(slot));
    }
    fn run(&self, slot: Slot, command: Command) -> Ticket {
        let mut i = self.0.borrow_mut();
        i.calls.push(Call::Run(slot, format!("{command:?}")));
        i.next += 1;
        i.next
    }
    fn state(&self, slot: Slot) -> Option<State> {
        self.0.borrow().states.get(&slot).cloned()
    }
    fn list_ports(&self) {
        self.0.borrow_mut().calls.push(Call::List);
    }
    fn migrate(&self, id: &str, port: &str) {
        self.0
            .borrow_mut()
            .calls
            .push(Call::Migrate(id.into(), port.into()));
    }
    fn cancel_migration(&self) {
        self.0.borrow_mut().calls.push(Call::CancelMigration);
    }
}

struct App {
    m: Model<FakeFleet>,
    fleet: FakeFleet,
}

fn port(name: &str, id: &str) -> PortInfo {
    PortInfo {
        port: name.into(),
        serial: id.into(),
    }
}

fn battery(d: &mut p::Device, percent: u32) {
    d.information
        .get_or_insert_default()
        .battery
        .get_or_insert_default()
        .level = Some(percent);
}

/// A mouse with Logitech Features up and three settings: a saved sensitivity, a stepped wheel
/// and its direction.
fn mouse_settings() -> Vec<p::SettingsObject> {
    let mut dpi = ranged(Some(1000), 200, 4000, 50);
    dpi.saved = Some(1000);
    dpi.status = Some(p::SettingStatus {
        result: Some(p::setting_status::Result::State(
            p::SettingState::Applied as i32,
        )),
    });
    vec![
        sensor(0, dpi),
        wheel(Some(p::WheelMode::Stepped as i32), Some(false)),
    ]
}

fn first_adapter() -> State {
    let mut st = fixture::state();
    st.status.id = "00000000000FA001".into();
    st.status.name = "Pico 2 W".into();
    fixture::with_profiles(&mut st.status);
    st.candidates.clear();
    let mut keys_dev = fixture::device(1, "Example Keys Wireless");
    battery(&mut keys_dev, 72);
    let mut mouse = fixture::device(2, "Example Mouse");
    mouse.kinds = vec![p::Kind::Mouse as i32];
    mouse.integrations = vec![p::Integration {
        kind: IntegrationKind::Hidpp as i32,
        enabled: true,
        status: Some(p::integration::Status::State(
            p::IntegrationState::Active as i32,
        )),
        ..Default::default()
    }];
    mouse.profiles = Some(p::ProfileLayers::default());
    battery(&mut mouse, 12);
    let travel = p::Device {
        transport: p::Transport::Classic as i32,
        state: DeviceState::Disconnected as i32,
        inactive: Some(InactiveReason::TransportDisabled as i32),
        ..fixture::device(3, "Travel Keyboard")
    };
    st.devices = vec![keys_dev, mouse, travel];
    st.settings.insert(2, mouse_settings());
    st
}

fn second_adapter() -> State {
    let mut st = fixture::state();
    st.status.id = "00000000000FA002".into();
    st.status.name = "ESP32-S3".into();
    st.candidates.clear();
    let mut kb = fixture::device(1, "Example Compact Keyboard");
    battery(&mut kb, 54);
    st.devices = vec![kb];
    st
}

impl App {
    fn new(width: usize, height: usize) -> Self {
        let fleet = FakeFleet::default();
        let mut m = Model::new(fleet.clone());
        m.width = width;
        m.height = height;
        Self { m, fleet }
    }

    /// A TUI with both fixture adapters attached and ready.
    fn two(width: usize, height: usize) -> Self {
        let mut app = Self::new(width, height);
        app.attach(vec![
            ("/dev/ttyACM0", first_adapter()),
            ("/dev/ttyACM1", second_adapter()),
        ]);
        app
    }

    fn attach(&mut self, adapters: Vec<(&str, State)>) {
        let ports = adapters
            .iter()
            .map(|(p, st)| port(p, &st.status.id))
            .collect();
        self.m.update(Msg::Ports(Ok(ports)));
        for (p, st) in adapters {
            let slot = self
                .m
                .probes
                .iter()
                .find(|(_, port)| port.as_str() == p)
                .map(|(s, _)| *s)
                .unwrap();
            self.fleet.0.borrow_mut().states.insert(slot, st);
            self.phase(slot, Phase::Opened);
            self.phase(slot, Phase::Ready);
        }
        self.fleet.0.borrow_mut().calls.clear();
    }

    fn slot(&self, id: &str) -> Slot {
        self.m.session_of(id).unwrap().slot
    }

    fn phase(&mut self, slot: Slot, phase: Phase) {
        self.m.update(Msg::Fleet(
            slot,
            Box::new(Event::Connection {
                session: 1,
                port: String::new(),
                phase,
            }),
        ));
    }

    fn edit(&mut self, slot: Slot, f: impl FnOnce(&mut State)) {
        f(self.fleet.0.borrow_mut().states.get_mut(&slot).unwrap());
    }

    fn calls(&self) -> Vec<Call> {
        self.fleet.0.borrow().calls.clone()
    }

    fn ran(&self, text: &str) -> bool {
        self.calls()
            .iter()
            .any(|c| matches!(c, Call::Run(_, r) if r.contains(text)))
    }

    fn render(&mut self) -> Vec<String> {
        let mut buf = Buffer::empty(Rect::new(0, 0, self.m.width as u16, self.m.height as u16));
        self.m.render(&mut buf);
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn screen(&mut self) -> String {
        self.render().join("\n")
    }

    fn shows(&mut self, text: &str) -> bool {
        self.render().iter().any(|l| l.contains(text))
    }

    fn key(&mut self, code: KeyCode) {
        self.m
            .update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn ctrl(&mut self, c: char) {
        self.m.update(Msg::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )));
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    /// Clicks the first control drawn with `label`.
    fn click(&mut self, label: &str) {
        self.render();
        let hit = self
            .find(label)
            .unwrap_or_else(|| panic!("no control {label}:\n{}", self.screen()));
        self.press(hit.x, hit.y);
    }

    fn find(&mut self, label: &str) -> Option<Hit> {
        let lines = self.render();
        self.m
            .hits
            .iter()
            .find(|h| {
                lines
                    .get(h.y)
                    .map(|l| {
                        let cells: Vec<&str> = l
                            .char_indices()
                            .map(|(i, c)| &l[i..i + c.len_utf8()])
                            .collect();
                        let end = (h.x + h.w).min(cells.len());
                        cells[h.x.min(end)..end].concat()
                    })
                    .is_some_and(|t| t.contains(label))
            })
            .cloned()
    }

    fn press(&mut self, x: usize, y: usize) {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            self.m.update(Msg::Mouse(MouseEvent {
                kind,
                column: x as u16,
                row: y as u16,
                modifiers: KeyModifiers::NONE,
            }));
        }
    }

    /// Finishes the last command run with a result.
    fn finish(&mut self, result: Result<Outcome, Error>) {
        let (slot, ticket) = {
            let jobs = &self.m.jobs;
            let (k, _) = jobs.iter().max_by_key(|(k, _)| k.1).unwrap();
            *k
        };
        self.m.update(Msg::Fleet(
            slot,
            Box::new(Event::Done {
                session: 1,
                ticket,
                result,
            }),
        ));
    }
}

/// Prints the main screens for a visual check:
/// `cargo test -p cordial-cli dump_screens -- --ignored --nocapture`.
#[test]
#[ignore]
fn dump_screens() {
    let (w, h) = std::env::var("DUMP_SIZE")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((110, 32));
    let mut app = App::two(w, h);
    println!("{}\n", app.screen());
    app.click("Example Mouse");
    println!("{}\n", app.screen());
    app.click("Settings");
    println!("{}\n", app.screen());
    app.click("Profiles");
    println!("{}\n", app.screen());
    app.click("Diagnostics");
    println!("{}\n", app.screen());
    app.click("Travel Keyboard");
    println!("{}\n", app.screen());
    app.click("Pico 2 W");
    println!("{}\n", app.screen());
    app.click("Settings");
    println!("{}\n", app.screen());
    app.click("Profiles");
    println!("{}\n", app.screen());
    app.ctrl('n');
    let slot = app.slot("00000000000FA001");
    app.edit(slot, |st| {
        st.scanning = Some(vec![p::Transport::Ble]);
        st.candidates = vec![fixture::candidate(9, "Example Pebble")];
    });
    println!("{}\n", app.screen());
}

#[test]
fn every_adapter_opens_and_lists_its_devices() {
    let mut app = App::two(110, 32);
    let screen = app.screen();
    for name in [
        "Example Keys Wireless",
        "Example Mouse",
        "Example Compact Keyboard",
        "Pico 2 W",
        "ESP32-S3",
        "Needs Attention",
    ] {
        assert!(screen.contains(name), "{name}:\n{screen}");
    }
    assert!(app.shows("Example Mouse has a low battery."));
}

#[test]
fn a_port_that_failed_is_tried_again_after_a_wait() {
    let mut app = App::new(100, 30);
    app.m
        .update(Msg::Ports(Ok(vec![port("/dev/ttyACM0", "A")])));
    let slot = *app.m.probes.keys().next().unwrap();
    assert_eq!(
        app.calls(),
        vec![Call::List, Call::Open("/dev/ttyACM0".into())]
    );
    app.phase(
        slot,
        Phase::Failed {
            error: Error::new("busy"),
            open: false,
        },
    );
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    app.fleet.0.borrow_mut().calls.clear();
    let opened = |app: &App| app.calls().iter().any(|c| matches!(c, Call::Open(_)));
    app.m
        .update(Msg::Ports(Ok(vec![port("/dev/ttyACM0", "A")])));
    assert!(!opened(&app), "it waits before trying again");
    app.m.failed.get_mut("/dev/ttyACM0").unwrap().at = Instant::now();
    app.m
        .update(Msg::Ports(Ok(vec![port("/dev/ttyACM0", "A")])));
    assert!(opened(&app));
    // A port that goes away and comes back is tried at once.
    let slot = *app.m.probes.keys().next().unwrap();
    app.phase(
        slot,
        Phase::Failed {
            error: Error::new("busy"),
            open: false,
        },
    );
    assert_eq!(app.m.failed["/dev/ttyACM0"].count, 2);
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    app.fleet.0.borrow_mut().calls.clear();
    app.m.update(Msg::Ports(Ok(vec![])));
    app.m
        .update(Msg::Ports(Ok(vec![port("/dev/ttyACM0", "A")])));
    assert!(opened(&app));
}

#[test]
fn a_lost_adapter_keeps_its_page_while_it_may_return() {
    let mut app = App::two(110, 32);
    app.click("Pico 2 W");
    app.click("Settings");
    let slot = app.slot("00000000000FA001");
    app.phase(slot, Phase::Lost(Error::new("gone")));
    assert!(app.shows("Connecting…"));
    assert_eq!(app.m.shown(), Page::Adapter("00000000000FA001".into()));
    assert!(!app.shows("Example Mouse"), "its devices leave the list");
    // It returns on another port and the page and tab stay.
    app.m.update(Msg::Ports(Ok(vec![
        port("/dev/ttyACM2", "00000000000FA001"),
        port("/dev/ttyACM1", "00000000000FA002"),
    ])));
    let back = *app
        .m
        .probes
        .iter()
        .find(|(_, p)| p.as_str() == "/dev/ttyACM2")
        .unwrap()
        .0;
    app.fleet
        .0
        .borrow_mut()
        .states
        .insert(back, first_adapter());
    app.phase(back, Phase::Opened);
    app.phase(back, Phase::Ready);
    assert_eq!(app.m.tab, Tab::Settings);
    assert!(app.shows("Example Mouse"));
}

#[test]
fn disconnect_keeps_the_adapter_until_connect() {
    let mut app = App::two(110, 32);
    app.click("ESP32-S3");
    app.click("Disconnect");
    let slot = 2;
    assert!(app.calls().contains(&Call::Close(slot)));
    assert!(app.shows("Disconnected"));
    app.fleet.0.borrow_mut().calls.clear();
    app.m.update(Msg::Ports(Ok(vec![
        port("/dev/ttyACM0", "00000000000FA001"),
        port("/dev/ttyACM1", "00000000000FA002"),
    ])));
    assert!(
        !app.calls().iter().any(|c| matches!(c, Call::Open(_))),
        "not reopened"
    );
    // Connect waits for the closing session to release the port.
    app.click("[Connect]");
    let connect = Call::OpenAdapter("/dev/ttyACM1".into(), "00000000000FA002".into());
    assert!(!app.calls().contains(&connect));
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    assert!(app.calls().contains(&connect));
}

#[test]
fn a_disconnected_adapter_without_a_serial_number_is_known_by_its_path() {
    let mut app = App::new(110, 32);
    let mut st = second_adapter();
    st.status.id = "B".into();
    app.attach(vec![("/dev/pts/9", st)]);
    app.m.disconnect_adapter("B");
    app.fleet.0.borrow_mut().calls.clear();
    app.m.update(Msg::Ports(Ok(vec![port("/dev/pts/9", "")])));
    assert!(
        !app.calls().iter().any(|c| matches!(c, Call::Open(_))),
        "not reopened"
    );
    assert!(app.shows("Disconnected"));
}

#[test]
fn help_and_quit_can_be_clicked() {
    for width in [60, 110] {
        let mut app = App::two(width, 32);
        app.click("[? Help]");
        assert_eq!(app.m.dialog, Some(Dialog::Help));
        app.click("[Close]");
        assert_eq!(app.m.dialog, None);
        app.click("[q Quit]");
        assert!(app.m.quitting());
    }
}

#[test]
fn a_menu_fits_a_terminal_that_shrank() {
    let mut app = App::two(110, 32);
    app.m.menu = Some(Menu {
        x: 100,
        y: 30,
        items: (0..20).map(|i| (format!("Item {i}"), None)).collect(),
    });
    app.m.width = 60;
    app.m.height = 16;
    app.render();
}

#[test]
fn device_details_stage_until_save() {
    let mut app = App::two(110, 32);
    app.click("Example Keys Wireless");
    app.click("[✓] On");
    assert!(app.shows("✎ Changed"));
    assert!(!app.ran("DeviceSave"));
    app.ctrl('s');
    assert!(app.ran("DeviceSave(1, DeviceUpdate { enabled: Some(false)"));
    app.finish(Ok(Outcome::Devices));
    assert!(!app.shows("✎ Changed"));
}

/// The first adapter with a gamepad whose saved output is `saved`, active in its first slot.
fn gamepad_adapter(saved: p::OutputMode) -> State {
    let mut st = first_adapter();
    let mut pad = fixture::device(4, "Example Gamepad");
    pad.kinds = vec![p::Kind::Gamepad as i32];
    pad.roles = vec![p::Role::Gamepad as i32];
    pad.output_preferences = vec![p::OutputPreference {
        role: p::Role::Gamepad as i32,
        mode: saved as i32,
    }];
    pad.outputs = vec![p::OutputStatus {
        role: p::Role::Gamepad as i32,
        mode: saved as i32,
        status: Some(p::output_status::Status::State(
            p::OutputState::Active as i32,
        )),
        slot: Some(0),
    }];
    st.devices.push(pad);
    st
}

/// Advertises HID and XInput gamepad output.
fn offer_gamepads(st: &mut State) {
    st.status.output_support = vec![p::OutputSupport {
        role: p::Role::Gamepad as i32,
        modes: [p::OutputMode::Hid, p::OutputMode::Xinput]
            .into_iter()
            .map(|mode| p::OutputModeSupport {
                mode: mode as i32,
                resources: vec![p::ResourceCapacity {
                    r#type: p::ResourceType::Slots as i32,
                    count: Some(1),
                    limit: Some(4),
                }],
            })
            .collect(),
    }];
}

impl App {
    /// A save sent at once, without a confirmation.
    fn saved_without_asking(&mut self) {
        let screen = self.screen();
        assert_eq!(self.m.dialog, None, "{screen}");
    }
}

#[test]
fn gamepad_output_is_staged_on_settings_until_save() {
    let mut st = gamepad_adapter(p::OutputMode::Hid);
    offer_gamepads(&mut st);
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Example Keys Wireless");
    assert!(!app.shows("Settings"));
    app.click("Example Gamepad");
    app.click("Settings");
    assert!(
        app.shows("[○ Disabled] [● HID] [○ XInput]"),
        "{}",
        app.screen()
    );
    app.click("[○ XInput]");
    assert!(app.shows("✎ Changed"));
    assert!(!app.ran("DeviceSave"));
    app.ctrl('s');
    app.saved_without_asking();
    assert!(app.ran("DeviceSave(4, DeviceUpdate { enabled: None, trusted: None, blocked: None, hidpp: None, gamepad: Some(Xinput), layers: None })"));
    app.finish(Ok(Outcome::Devices));
    assert!(!app.shows("✎ Changed"));
    app.click("Diagnostics");
    let screen = app.screen();
    assert!(
        screen.contains("Gamepad Output") && screen.contains("Active"),
        "{screen}"
    );
    assert!(screen.contains("Slot                    1"), "{screen}");
}

#[test]
fn a_saved_gamepad_output_the_adapter_lacks_can_only_be_disabled() {
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", gamepad_adapter(p::OutputMode::Hid))]);
    app.click("Example Gamepad");
    app.click("Settings");
    assert!(app.shows("[○ Disabled] [● HID]"), "{}", app.screen());
    assert!(!app.shows("XInput"));
    app.click("[○ Disabled]");
    assert!(app.shows("[● Disabled] [○ HID]"));
    // The adapter has no HID pool to change.
    app.ctrl('s');
    assert_eq!(app.m.dialog, None);
    assert!(app.ran("gamepad: Some(Disabled)"));
    // Without a saved choice there is nothing to change.
    let mut app = App::new(110, 40);
    app.attach(vec![(
        "/dev/ttyACM0",
        gamepad_adapter(p::OutputMode::Disabled),
    )]);
    app.click("Example Gamepad");
    assert!(!app.shows("Settings"), "{}", app.screen());
}

#[test]
fn settings_save_before_a_gamepad_output_change() {
    let mut st = first_adapter();
    offer_gamepads(&mut st);
    st.devices[1].output_preferences = vec![p::OutputPreference {
        role: p::Role::Gamepad as i32,
        mode: p::OutputMode::Hid as i32,
    }];
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Example Mouse");
    app.click("Settings");
    app.click("[○ Free Spin]");
    app.click("[○ Disabled]");
    assert!(app.shows("[● Disabled]"));
    app.ctrl('s');
    app.saved_without_asking();
    assert!(app.ran("SettingsSave { device: 2"));
    assert!(!app.ran("DeviceSave"));
    // The staged output can't change while the settings save.
    assert!(app.find("[○ XInput]").is_none());
    app.m.action(Action::GamepadOutput(p::OutputMode::Xinput));
    assert!(app.shows("[● Disabled]"));
    app.finish(Ok(Outcome::Devices));
    assert!(app.ran("gamepad: Some(Disabled)"));
}

#[test]
fn a_gamepad_output_change_saves_without_asking() {
    let mut st = first_adapter();
    offer_gamepads(&mut st);
    st.devices[1].output_preferences = vec![p::OutputPreference {
        role: p::Role::Gamepad as i32,
        mode: p::OutputMode::Hid as i32,
    }];
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Example Mouse");
    app.click("Settings");
    app.click("[○ Free Spin]");
    app.click("[○ Disabled]");
    // Save sends the settings, then Gamepad Output, whatever happens to the USB interfaces.
    app.click("[Save]");
    app.saved_without_asking();
    assert!(app.ran("SettingsSave { device: 2"));
    assert!(!app.ran("DeviceSave"));
    app.finish(Ok(Outcome::Devices));
    assert!(app.ran("gamepad: Some(Disabled)"));
    assert_eq!(app.m.dialog, None);
    // A gamepad alone also saves at once.
    let mut st = gamepad_adapter(p::OutputMode::Hid);
    offer_gamepads(&mut st);
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Example Gamepad");
    app.click("Settings");
    app.click("[○ Disabled]");
    app.render();
    app.ctrl('s');
    app.saved_without_asking();
    assert!(app.ran("gamepad: Some(Disabled)"));
}

#[test]
fn a_failed_settings_save_keeps_the_gamepad_output_change_staged() {
    for (error, text) in [
        (
            Error::new("the connection to the adapter closed"),
            words::CLOSED.trim_end_matches('.'),
        ),
        (Error::new("busy"), "Busy"),
    ] {
        let mut st = first_adapter();
        offer_gamepads(&mut st);
        st.devices[1].output_preferences = vec![p::OutputPreference {
            role: p::Role::Gamepad as i32,
            mode: p::OutputMode::Hid as i32,
        }];
        let mut app = App::new(110, 40);
        app.attach(vec![("/dev/ttyACM0", st)]);
        app.click("Example Mouse");
        app.click("Settings");
        app.click("[○ Free Spin]");
        app.click("[○ Disabled]");
        app.ctrl('s');
        app.saved_without_asking();
        app.finish(Err(error));
        assert!(!app.ran("DeviceSave"));
        let screen = app.screen();
        assert!(screen.contains(text), "{screen}");
        assert!(
            screen.contains("[● Disabled]") && screen.contains("[● Free Spin]"),
            "{screen}"
        );
    }
}

#[test]
fn details_save_waits_for_a_settings_save_that_saves_the_gamepad_output() {
    let mut st = first_adapter();
    offer_gamepads(&mut st);
    st.devices[1].output_preferences = vec![p::OutputPreference {
        role: p::Role::Gamepad as i32,
        mode: p::OutputMode::Hid as i32,
    }];
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Example Mouse");
    app.m.action(Action::DeviceToggle(Toggle::Trusted, false));
    app.click("Settings");
    app.click("[○ Free Spin]");
    app.click("[○ Disabled]");
    app.ctrl('s');
    app.saved_without_asking();
    assert!(app.ran("SettingsSave { device: 2"));
    // Saving Details now would leave the gamepad output unsent once the settings save.
    app.m.action(Action::DetailsSave);
    assert!(!app.ran("DeviceSave"));
    app.finish(Ok(Outcome::Devices));
    assert!(app.ran("DeviceSave(2, DeviceUpdate { enabled: None, trusted: None, blocked: None, hidpp: None, gamepad: Some(Disabled), layers: None })"));
    // The other way round, Settings waits for the Details save.
    app.finish(Ok(Outcome::Devices));
    app.click("Example Mouse");
    app.m.action(Action::DetailsSave);
    assert!(app.ran("trusted: Some(false)"));
    app.click("Settings");
    app.click("[○ XInput]");
    let runs = app.calls().len();
    app.ctrl('s');
    assert_eq!(app.calls().len(), runs);
    assert!(app.find("[Save]").is_none(), "{}", app.screen());
}

#[test]
fn an_unknown_saved_gamepad_output_can_be_disabled() {
    for offered in [true, false] {
        let mut st = first_adapter();
        if offered {
            offer_gamepads(&mut st);
        }
        st.devices[1].output_preferences = vec![p::OutputPreference {
            role: p::Role::Gamepad as i32,
            mode: 7,
        }];
        let mut app = App::new(110, 40);
        app.attach(vec![("/dev/ttyACM0", st)]);
        app.click("Example Mouse");
        // An unrelated save leaves the unknown choice alone.
        app.m.action(Action::DeviceToggle(Toggle::Trusted, false));
        app.m.action(Action::DetailsSave);
        assert!(app.ran("trusted: Some(false), blocked: None, hidpp: None, gamepad: None"));
        app.finish(Ok(Outcome::Devices));
        app.click("Settings");
        let choices = if offered {
            "[○ Disabled] [○ HID] [○ XInput]"
        } else {
            "[○ Disabled]"
        };
        assert!(app.shows(choices), "{}", app.screen());
        assert!(offered || !app.shows("HID"));
        app.click("[○ Disabled]");
        assert!(app.shows("✎ Changed"));
        app.ctrl('s');
        assert!(app.ran("gamepad: Some(Disabled)"), "{:?}", app.calls());
    }
}

/// Saves Gamepad Output XInput for the gamepad from its Settings tab.
fn save_xinput(app: &mut App) {
    app.click("Example Gamepad");
    app.click("Settings");
    app.click("[○ XInput]");
    app.render();
    app.ctrl('s');
    app.saved_without_asking();
    assert!(app.ran("gamepad: Some(Xinput)"));
}

impl App {
    /// The first adapter's session ends and its port goes away.
    fn lose_first(&mut self) {
        let slot = self.slot("00000000000FA001");
        self.phase(slot, Phase::Lost(Error::new("gone")));
        self.m.update(Msg::Ports(Ok(Vec::new())));
    }

    /// The first adapter returns on another port with `st`.
    fn return_first(&mut self, st: State) -> Slot {
        self.m
            .update(Msg::Ports(Ok(vec![port("/dev/ttyACM2", &st.status.id)])));
        let back = *self
            .m
            .probes
            .iter()
            .find(|(_, p)| p.as_str() == "/dev/ttyACM2")
            .unwrap()
            .0;
        self.fleet.0.borrow_mut().states.insert(back, st);
        self.phase(back, Phase::Opened);
        self.phase(back, Phase::Ready);
        back
    }

    /// Finishes the device save of the gamepad with a result.
    fn finish_gamepad_save(&mut self, result: Result<Outcome, Error>) {
        let (slot, ticket) = *self
            .m
            .jobs
            .iter()
            .find(|(_, j)| matches!(j.kind, Kind::DeviceSave { device: 4, .. }))
            .unwrap()
            .0;
        self.m.update(Msg::Fleet(
            slot,
            Box::new(Event::Done {
                session: 1,
                ticket,
                result,
            }),
        ));
    }
}

#[test]
fn a_gamepad_output_save_keeps_its_page_across_the_usb_reconnect() {
    let page = Page::Device("00000000000FA001".into(), 4);
    let mut st = gamepad_adapter(p::OutputMode::Hid);
    offer_gamepads(&mut st);
    let mut saved = st.clone();
    saved.devices[3].output_preferences[0].mode = p::OutputMode::Xinput as i32;

    // The adapter answers, then USB reconnects.
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st.clone())]);
    save_xinput(&mut app);
    let slot = app.slot("00000000000FA001");
    app.edit(slot, |s| *s = saved.clone());
    app.finish_gamepad_save(Ok(Outcome::Devices));
    app.lose_first();
    assert_eq!(app.m.shown(), page);
    assert_eq!(app.m.tab, Tab::Settings);
    assert!(app.shows("[○ HID] [● XInput]"), "{}", app.screen());
    assert!(!app.shows("✎ Changed"));
    assert!(app.find("[○ Disabled]").is_none(), "locked while offline");
    app.return_first(saved.clone());
    assert_eq!((app.m.shown(), app.m.tab), (page.clone(), Tab::Settings));
    assert!(app.shows("[● XInput]") && !app.shows("✎ Changed"));

    // USB drops before the answer, which arrives before or after the session ends.
    for lost_first in [true, false] {
        for after in [st.clone(), saved.clone()] {
            let mut app = App::new(110, 40);
            app.attach(vec![("/dev/ttyACM0", st.clone())]);
            save_xinput(&mut app);
            let closed = || Err(Error::new("the connection to the adapter closed"));
            if lost_first {
                app.lose_first();
                app.finish_gamepad_save(closed());
            } else {
                app.finish_gamepad_save(closed());
                app.lose_first();
            }
            assert_eq!((app.m.shown(), app.m.tab), (page.clone(), Tab::Settings));
            assert!(app.shows(words::CLOSED) && app.shows("✎ Changed"));
            let stored =
                after.devices[3].output_preferences[0].mode == p::OutputMode::Xinput as i32;
            app.return_first(after);
            assert_eq!((app.m.shown(), app.m.tab), (page.clone(), Tab::Settings));
            assert!(app.shows("[● XInput]"));
            // The note stays until the device reports the change saved.
            assert_eq!(app.shows(words::CLOSED), !stored, "{}", app.screen());
            assert_eq!(app.shows("✎ Changed"), !stored);
        }
    }
}

#[test]
fn a_late_answer_from_the_ended_session_yields_to_the_returned_record() {
    let mut st = gamepad_adapter(p::OutputMode::Hid);
    offer_gamepads(&mut st);
    let mut saved = st.clone();
    saved.devices[3].output_preferences[0].mode = p::OutputMode::Xinput as i32;
    // The adapter returns and lists the device before the ended session answers.
    for stored in [true, false] {
        for failed in [true, false] {
            let mut app = App::new(110, 40);
            app.attach(vec![("/dev/ttyACM0", st.clone())]);
            save_xinput(&mut app);
            app.lose_first();
            app.return_first(if stored { saved.clone() } else { st.clone() });
            // The readback alone settles a stored change.
            let key = ("00000000000FA001".to_owned(), 4);
            assert_eq!(app.m.device_drafts.contains_key(&key), !stored);
            app.finish_gamepad_save(if failed {
                Err(Error::new("the connection to the adapter closed"))
            } else {
                Ok(Outcome::Devices)
            });
            let case = format!("stored {stored}, failed {failed}\n{}", app.screen());
            // Only a failure the record doesn't contradict shows, and its change stays staged.
            assert_eq!(app.shows(words::CLOSED), failed && !stored, "{case}");
            assert_eq!(app.shows("✎ Changed"), failed && !stored, "{case}");
            let mode = if stored || failed {
                "[● XInput]"
            } else {
                "[● HID]"
            };
            assert!(app.shows(mode), "{case}");
        }
    }
}

#[test]
fn a_held_device_page_closes_when_the_device_does_not_return() {
    let mut st = gamepad_adapter(p::OutputMode::Hid);
    offer_gamepads(&mut st);
    // The hold expires.
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st.clone())]);
    save_xinput(&mut app);
    app.finish_gamepad_save(Err(Error::new("the connection to the adapter closed")));
    app.lose_first();
    app.m.held.get_mut("00000000000FA001").unwrap().until = Instant::now();
    app.m.tick();
    assert_eq!(app.m.shown(), Page::Overview);
    assert!(app.m.notes.is_empty());
    // The adapter returns without the device.
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st.clone())]);
    save_xinput(&mut app);
    app.lose_first();
    let mut gone = st.clone();
    gone.devices.pop();
    app.return_first(gone);
    assert_eq!(app.m.shown(), Page::Overview);
}

#[test]
fn a_settings_save_answered_after_a_reconnect_sends_nothing_to_the_new_session() {
    let mut st = gamepad_adapter(p::OutputMode::Hid);
    offer_gamepads(&mut st);
    st.devices[1].output_preferences = st.devices[3].output_preferences.clone();
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st.clone())]);
    app.click("Example Mouse");
    app.click("Settings");
    app.click("[○ Free Spin]");
    app.click("[○ Disabled]");
    app.ctrl('s');
    app.saved_without_asking();
    let (old, ticket) = *app
        .m
        .jobs
        .iter()
        .find(|(_, j)| matches!(j.kind, Kind::SettingsSave { .. }))
        .unwrap()
        .0;
    app.lose_first();
    let back = app.return_first(st);
    app.fleet.0.borrow_mut().calls.clear();
    app.m.update(Msg::Fleet(
        old,
        Box::new(Event::Done {
            session: 1,
            ticket,
            result: Ok(Outcome::Devices),
        }),
    ));
    assert!(!app.ran("DeviceSave"), "{:?}", app.calls());
    assert_ne!(old, back);
    assert!(app.shows("[● Disabled]") && app.shows("✎ Changed"));
}

#[test]
fn settings_save_sends_every_change_in_one_request() {
    let mut app = App::two(110, 32);
    app.click("Example Mouse");
    app.click("Settings");
    app.click("[○ Free Spin]");
    app.click("[ ] Off");
    app.click("[Save]");
    assert!(app.ran("SettingsSave { device: 2"));
    assert!(app.ran("WheelMode"));
    assert!(app.ran("WheelInvert"));
}

#[test]
fn typed_numbers_are_checked_before_saving() {
    let mut app = App::two(110, 32);
    app.click("Example Mouse");
    app.click("Settings");
    app.click("1000");
    for _ in 0..4 {
        app.key(KeyCode::Backspace);
    }
    app.type_text("1025");
    app.key(KeyCode::Tab);
    assert!(app.shows("200-4000, Steps of 50"));
    assert!(
        app.find("[Save]").is_none(),
        "Save waits for a valid number"
    );
    app.ctrl('s');
    assert!(!app.ran("SettingsSave"));
}

#[test]
fn an_interface_save_is_sent_without_asking() {
    let mut app = App::two(110, 32);
    app.click("Pico 2 W");
    app.click("Profiles");
    app.m
        .action(Action::InterfaceEnabled(ConfigurationInterface::Via, true));
    // VIA has no profile yet, so the save is refused before sending.
    app.m.action(Action::AdapterSave);
    assert!(app.shows("Choose a profile for VIA before turning it on."));
    app.m.stage_interface(
        "00000000000FA001",
        ConfigurationInterface::Via,
        None,
        Some(3),
    );
    app.m.action(Action::AdapterSave);
    app.saved_without_asking();
    assert!(app.ran("AdapterSave"));
}

/// Profiles P01 to P`count`, as one listing.
fn profile_list(count: u32) -> p::ProfileList {
    p::ProfileList {
        entries: (1..=count)
            .map(|id| p::ProfileListEntry {
                entry: Some(p::profile_list_entry::Entry::Profile(p::Profile {
                    id,
                    name: format!("P{id:02}"),
                    roles: Vec::new(),
                })),
            })
            .collect(),
    }
}

impl App {
    /// Delivers a response notice, as the session reports a command's response when it arrives.
    fn respond(&mut self, slot: Slot, result: p::response::Result) {
        self.m.update(Msg::Fleet(
            slot,
            Box::new(Event::Notice {
                session: 1,
                notice: crate::controller::Notice::Response(p::Response {
                    result: Some(result),
                }),
            }),
        ));
    }

    /// Answers the running profile listing for the list, or the picker: the response notice,
    /// then the command's result.
    fn list_profiles(&mut self, picker: bool, list: p::ProfileList) {
        let slot = self.slot("00000000000FA001");
        self.respond(slot, p::response::Result::Profiles(list.clone()));
        self.finish_profiles(picker, Ok(Outcome::Profiles(list)));
    }

    /// Finishes the running profile listing for the list, or the picker.
    fn finish_profiles(&mut self, picker: bool, result: Result<Outcome, Error>) {
        let (slot, ticket) = *self
            .m
            .jobs
            .iter()
            .find(|(_, j)| matches!(j.kind, Kind::Page { picker: p } if p == picker))
            .unwrap()
            .0;
        self.m.update(Msg::Fleet(
            slot,
            Box::new(Event::Done {
                session: 1,
                ticket,
                result,
            }),
        ));
    }

    fn profile_event(&mut self, slot: Slot, kind: p::event::Kind) {
        self.m.update(Msg::Fleet(
            slot,
            Box::new(Event::Notice {
                session: 1,
                notice: crate::controller::Notice::Event {
                    event: p::Event { kind: Some(kind) },
                    first: false,
                    changed: Vec::new(),
                },
            }),
        ));
    }
}

#[test]
fn profiles_show_one_listing_a_page_at_a_time() {
    let mut app = App::two(110, 60);
    app.click("Pico 2 W");
    app.click("Profiles");
    app.list_profiles(false, profile_list(25));
    assert!(app.shows("P10") && !app.shows("P11") && app.find("Previous").is_none());
    app.click("Next");
    assert!(app.shows("P11") && app.shows("P20") && !app.shows("P10"));
    app.click("Next");
    assert!(app.shows("P25") && app.find("Next").is_none());
    // Moving between pages lists nothing again.
    assert!(!app.ran("Profiles"));
    // Events change the listing, and an emptied last page shows the one before it.
    let slot = app.slot("00000000000FA001");
    for id in 21..=25 {
        app.profile_event(
            slot,
            p::event::Kind::ProfileRemoved(p::ProfileRemoved { id }),
        );
    }
    assert!(app.shows("P20") && app.find("Next").is_none());
    app.profile_event(
        slot,
        p::event::Kind::Profile(p::Profile {
            id: 30,
            name: "Q30".into(),
            ..Default::default()
        }),
    );
    assert!(app.find("Next").is_some());
    // A failed listing keeps what was listed.
    app.m.action(Action::ProfilesRetry);
    app.finish_profiles(false, Err(Error::new("the adapter is busy")));
    assert!(app.shows("The adapter couldn't read its profiles.") && app.shows("P20"));
}

/// Renames P02, removes P03 and creates P04, as profile events.
fn change_profiles(app: &mut App) {
    let slot = app.slot("00000000000FA001");
    for (id, name) in [(2, "Renamed"), (4, "P04")] {
        app.profile_event(
            slot,
            p::event::Kind::Profile(p::Profile {
                id,
                name: name.into(),
                ..Default::default()
            }),
        );
    }
    app.profile_event(
        slot,
        p::event::Kind::ProfileRemoved(p::ProfileRemoved { id: 3 }),
    );
}

fn changed_profiles_shown(app: &mut App) -> bool {
    app.shows("P01")
        && app.shows("Renamed")
        && !app.shows("P02")
        && !app.shows("P03")
        && app.shows("P04")
}

#[test]
fn profile_events_after_a_listing_apply_before_its_command_finishes() {
    let mut app = App::two(110, 60);
    app.click("Pico 2 W");
    app.click("Profiles");
    let slot = app.slot("00000000000FA001");
    // The events that follow the first listing's response arrive before its command finishes.
    app.respond(slot, p::response::Result::Profiles(profile_list(3)));
    change_profiles(&mut app);
    app.finish_profiles(false, Ok(Outcome::Profiles(profile_list(3))));
    assert!(changed_profiles_shown(&mut app));
    // A refresh is overtaken by events the same way.
    app.m.action(Action::ProfilesRetry);
    app.respond(slot, p::response::Result::Profiles(profile_list(3)));
    change_profiles(&mut app);
    app.finish_profiles(false, Ok(Outcome::Profiles(profile_list(3))));
    assert!(changed_profiles_shown(&mut app));
    // A refresh that fails keeps the listing and the events applied to it.
    app.m.action(Action::ProfilesRetry);
    app.respond(slot, p::response::Result::Error(p::Error::default()));
    app.profile_event(
        slot,
        p::event::Kind::Profile(p::Profile {
            id: 5,
            name: "P05".into(),
            ..Default::default()
        }),
    );
    app.finish_profiles(false, Err(Error::new("the adapter is busy")));
    assert!(changed_profiles_shown(&mut app) && app.shows("P05"));
    assert!(app.shows("The adapter couldn't read its profiles."));
}

#[test]
fn the_picker_follows_events_after_its_listing_and_keeps_its_own_page() {
    let mut app = App::two(110, 60);
    app.click("Pico 2 W");
    app.click("Profiles");
    app.list_profiles(false, profile_list(25));
    app.click("Next");
    assert!(app.shows("P11"));
    app.m
        .action(Action::InterfacePick(ConfigurationInterface::Via));
    let slot = app.slot("00000000000FA001");
    app.respond(slot, p::response::Result::Profiles(profile_list(3)));
    change_profiles(&mut app);
    app.finish_profiles(true, Ok(Outcome::Profiles(profile_list(3))));
    assert!(changed_profiles_shown(&mut app));
    // The Profiles tab stays on its page, which the removal moved along by one.
    app.key(KeyCode::Esc);
    assert!(app.shows("P12") && app.shows("P21") && !app.shows("P11") && !app.shows("P01"));
}

#[test]
fn add_device_scans_pairs_and_stops_on_close() {
    let mut app = App::two(110, 32);
    app.ctrl('n');
    assert!(app.ran("Scan { transports: [], seconds: 30 }"));
    assert_eq!(app.m.dialog, Some(Dialog::AddDevice));
    app.finish(Ok(Outcome::ScanStarted(vec![p::Transport::Ble])));
    assert_eq!(app.m.dialog, Some(Dialog::AddDevice), "{:?}", app.m.add);
    let slot = app.slot(app.m.add.as_ref().unwrap().adapter.as_deref().unwrap());
    app.edit(slot, |st| {
        st.scanning = Some(vec![p::Transport::Ble]);
        st.candidates = vec![fixture::candidate(9, "Example Pebble")];
    });
    assert!(app.shows("Example Pebble"));
    app.click("[Pair]");
    assert!(app.ran("ScanStop"));
    assert!(app.ran(&format!("{:?}", Command::Pair(Target::Id(9)))));
    assert!(app.shows("Pairing with Example Pebble…"));
    app.key(KeyCode::Esc);
    assert!(app.ran("CancelPairing"));
    assert_eq!(app.m.dialog, None);
}

#[test]
fn quitting_closes_every_session() {
    let mut app = App::two(110, 32);
    app.key(KeyCode::Char('q'));
    let closes = app
        .calls()
        .iter()
        .filter(|c| matches!(c, Call::Close(_)))
        .count();
    assert_eq!(closes, 2);
    assert!(!app.m.done);
    for slot in [1, 2] {
        app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    }
    assert!(app.m.done);
}

#[test]
fn the_enabled_limit_is_predicted_as_the_adapter_applies_it() {
    let mut st = first_adapter();
    // Bluetooth LE allows one device in use, and the keyboard holds it.
    st.status.transports[1].resources[0].limit = Some(1);
    st.devices.truncate(1);
    let off = p::Device {
        id: 9,
        enabled: false,
        inactive: Some(InactiveReason::Disabled as i32),
        state: DeviceState::Disconnected as i32,
        ..fixture::device(9, "Spare")
    };
    let blocked = p::Device {
        id: 10,
        blocked: true,
        inactive: Some(InactiveReason::Blocked as i32),
        state: DeviceState::Disconnected as i32,
        ..fixture::device(10, "Blocked")
    };
    st.devices.push(off.clone());
    st.devices.push(blocked.clone());
    use crate::commands::capacity_refused;
    assert!(
        capacity_refused(&st, &off, true, false),
        "turning it on needs a place"
    );
    assert!(
        !capacity_refused(&st, &off, true, true),
        "a blocked device needs none"
    );
    assert!(
        capacity_refused(&st, &blocked, true, false),
        "unblocking needs a place"
    );
    assert!(
        !capacity_refused(&st, &st.devices[0].clone(), true, false),
        "one in use keeps its place"
    );
    // A disabled transport takes no place.
    st.status.transports[1].enabled = Some(false);
    assert!(!capacity_refused(&st, &off, true, false));
}

#[test]
fn connect_refuses_another_adapter_on_the_port() {
    let mut app = App::two(110, 32);
    app.m.disconnect_adapter("00000000000FA002");
    closed(&mut app, 2);
    app.fleet.0.borrow_mut().calls.clear();
    app.m.connect_adapter("00000000000FA002");
    assert_eq!(
        app.calls(),
        [Call::OpenAdapter(
            "/dev/ttyACM1".into(),
            "00000000000FA002".into()
        )]
    );
    let slot = app.m.off["00000000000FA002"].connecting.unwrap();
    app.phase(slot, Phase::Other);
    assert_eq!(app.calls().last(), Some(&Call::Close(slot)));
    assert!(app.m.session_of("00000000000FA002").is_none());
    assert!(
        app.m.off["00000000000FA002"]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Another adapter answered on its port."))
    );
}

#[test]
fn a_connect_waiting_for_its_port_follows_the_adapter_to_another_port() {
    let mut app = App::two(110, 32);
    app.m.disconnect_adapter("00000000000FA002");
    app.m.connect_adapter("00000000000FA002");
    assert!(app.m.off["00000000000FA002"].waiting);
    app.fleet.0.borrow_mut().calls.clear();
    // Plugged in again on another port before the old session closed.
    app.m.update(Msg::Ports(Ok(vec![
        port("/dev/ttyACM0", "00000000000FA001"),
        port("/dev/ttyACM5", "00000000000FA002"),
    ])));
    assert!(app.calls().contains(&Call::OpenAdapter(
        "/dev/ttyACM5".into(),
        "00000000000FA002".into()
    )));
}

#[test]
fn a_connect_whose_port_went_away_fails_instead_of_hanging() {
    let mut app = App::two(110, 32);
    app.m.disconnect_adapter("00000000000FA002");
    app.m.update(Msg::Fleet(2, Box::new(Event::Closed)));
    app.m.connect_adapter("00000000000FA002");
    let slot = app.m.off["00000000000FA002"].connecting.unwrap();
    app.m.update(Msg::Ports(Ok(vec![port(
        "/dev/ttyACM0",
        "00000000000FA001",
    )])));
    app.phase(slot, Phase::Lost(Error::new("gone")));
    assert_eq!(app.m.off["00000000000FA002"].connecting, None);
    assert!(app.calls().contains(&Call::Close(slot)));
}

#[test]
fn file_downloads_save_records_as_json() {
    let mut app = App::two(110, 40);
    app.m.open_files("00000000000FA001");
    app.finish(Ok(Outcome::Files {
        path: "/".into(),
        entries: vec![p::FileEntry {
            name: "sequence.pb".into(),
            directory: false,
            size: 4,
        }],
    }));
    app.click("sequence.pb");
    assert_eq!(app.m.files.dest.value(), "sequence.json");
    app.click("Download");
    assert!(
        app.ran(r#"FileGet { path: "/sequence.pb", local: "sequence.json", overwrite: false, raw: false }"#),
        "{:?}",
        app.calls()
    );
    // A file that doesn't decode is saved as it is, under its own name.
    app.finish(Ok(Outcome::FileSaved {
        path: "/sequence.pb".into(),
        local: "sequence.pb".into(),
        bytes: 4,
        json: false,
        unconverted: Some(crate::records::Unconverted::Undecodable),
    }));
    let screen = app.screen();
    assert!(
        screen.contains("Saved without converting to JSON: the file doesn't decode as its record."),
        "{screen}"
    );
    let to = screen.lines().find(|l| l.contains(" To ")).unwrap();
    assert!(to.contains("sequence.pb") && !to.contains("json"), "{to}");
}

#[test]
fn usb_capacity_errors_show_independently() {
    let mut st = first_adapter();
    st.status.resources = vec![p::ResourceCapacity {
        r#type: p::ResourceType::Interfaces as i32,
        count: Some(0),
        limit: Some(8),
    }];
    st.status.errors = vec![err(p::ErrorCode::UsbReservedCapacityExceeded)];
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    // Reserved capacity alone is shown on the adapter's page, without needing attention.
    assert!(!app.shows("Reserved Capacity Exceeded"));
    app.click("Pico 2 W");
    let screen = app.screen();
    assert!(
        screen.contains("0 of 8") && screen.contains("Reserved Capacity Exceeded"),
        "{screen}"
    );
    assert!(!screen.contains("Needs Attention") && !screen.contains("Required Capacity"));
    // Required capacity needs attention, and both show at once.
    let slot = app.slot("00000000000FA001");
    app.edit(slot, |st| {
        st.status.errors = vec![
            err(p::ErrorCode::UsbRequiredCapacityExceeded),
            err(p::ErrorCode::UsbReservedCapacityExceeded),
        ];
        st.status.resources[0].count = None;
    });
    app.m.tick();
    let screen = app.screen();
    for text in [
        "Needs Attention",
        "! The adapter doesn't have enough USB capacity for every required",
        "Required Capacity Exceeded",
        "Reserved Capacity Exceeded",
        "Up to 8",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    app.click("Overview");
    assert!(app.shows("Required Capacity Exceeded"), "{}", app.screen());
    // Required capacity alone.
    app.edit(slot, |st| {
        st.status.errors = vec![err(p::ErrorCode::UsbRequiredCapacityExceeded)];
    });
    app.click("Pico 2 W");
    let screen = app.screen();
    assert!(screen.contains("Required Capacity Exceeded"), "{screen}");
    assert!(!screen.contains("Reserved Capacity Exceeded"), "{screen}");
}

#[test]
fn a_device_record_the_adapter_couldnt_read_says_so_on_diagnostics() {
    let mut st = first_adapter();
    st.unreadable.insert(
        1,
        p::Error {
            code: p::ErrorCode::StorageFailed as i32,
            reason: p::ErrorReason::Storage as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            related: Vec::new(),
        },
    );
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    // The device stays listed with what was known of it.
    app.click("Example Keys Wireless");
    app.click("Diagnostics");
    let screen = app.screen();
    assert!(
        screen.contains("The adapter couldn't read this device's saved data. Try again."),
        "{screen}"
    );
    assert!(screen.contains("Retry"), "{screen}");
}

#[test]
fn a_settings_save_failure_says_couldnt_save_only_with_no_effect() {
    for (outcome, refused) in [
        (p::ErrorOutcome::Unspecified as i32, false),
        (97, false),
        (p::ErrorOutcome::NoEffect as i32, true),
    ] {
        let mut app = App::two(110, 32);
        app.click("Example Mouse");
        app.click("Settings");
        app.click("[○ Free Spin]");
        app.click("[Save]");
        let error = p::Error {
            outcome,
            ..err(p::ErrorCode::Timeout)
        };
        app.finish(Err(Error::dongle(error, Some("setting set"))));
        let screen = app.screen();
        assert_eq!(screen.contains("Couldn't Save"), refused, "{screen}");
        assert!(
            screen.contains("This device didn't respond in time"),
            "{screen}"
        );
    }
}

#[test]
fn a_settings_save_failure_after_sending_is_unconfirmed() {
    use cordial_client::Error as E;
    let io = || E::Io(std::sync::Arc::new(std::io::Error::other("unplugged")));
    for (error, refused) in [
        (Error::from_client(E::Timeout, "setting set"), false),
        (Error::from_client(io(), "setting set"), false),
        (Error::from_client(E::Protocol, "setting set"), false),
        (
            Error::from_client(E::UnexpectedResponse, "setting set"),
            false,
        ),
        (Error::from_client(E::Closed, "setting set"), false),
        (Error::from_client(E::TooLong, "setting set"), true),
        (
            Error::code(p::ErrorCode::BadArgs, Some("setting set")),
            true,
        ),
    ] {
        let mut app = App::two(110, 32);
        app.click("Example Mouse");
        app.click("Settings");
        app.click("[○ Free Spin]");
        app.click("[Save]");
        let message = error.message.clone();
        app.finish(Err(error));
        let screen = app.screen();
        assert_eq!(
            screen.contains("Couldn't Save"),
            refused,
            "{message}: {screen}"
        );
    }
}

#[test]
fn a_device_record_the_adapter_couldnt_read_is_only_last_known() {
    let mut st = first_adapter();
    let read = p::Error {
        code: p::ErrorCode::StorageFailed as i32,
        reason: p::ErrorReason::Storage as i32,
        outcome: p::ErrorOutcome::NoEffect as i32,
        related: Vec::new(),
    };
    st.unreadable.insert(2, read.clone());
    // A device this session never read stays on the adapter's Diagnostics with its error.
    st.unreadable.insert(9, read);
    st.status.errors = vec![p::Error {
        code: p::ErrorCode::StorageFailed as i32,
        reason: p::ErrorReason::Storage as i32,
        outcome: p::ErrorOutcome::Unknown as i32,
        related: Vec::new(),
    }];
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Example Mouse");
    let screen = app.screen();
    assert!(screen.contains("Couldn't Read"), "{screen}");
    assert!(!screen.contains("[Disconnect]"), "{screen}");
    app.click("Pico 2 W");
    app.click("Diagnostics");
    let screen = app.screen();
    assert!(screen.contains("Device 9  Couldn't Read"), "{screen}");
    assert!(
        screen.contains("The adapter couldn't read or write its saved data."),
        "{screen}"
    );
    app.click("Retry");
    assert!(
        app.calls()
            .iter()
            .any(|c| matches!(c, Call::Run(_, r) if r == "Devices")),
        "{:?}",
        app.calls()
    );
}

#[test]
fn a_fault_while_starting_needs_attention() {
    let mut st = first_adapter();
    st.status.ready = false;
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Pico 2 W");
    assert!(app.shows("Starting…"), "{}", app.screen());
    let slot = app.slot("00000000000FA001");
    app.edit(slot, |st| {
        st.status.errors = vec![p::Error {
            reason: p::ErrorReason::Radio as i32,
            ..err(p::ErrorCode::NotReady)
        }];
    });
    app.click("Pico 2 W");
    assert!(app.shows("Needs Attention"), "{}", app.screen());
}

#[test]
fn device_counts_wait_for_every_adapter_to_list_its_devices() {
    let unlisted = |mut st: State| {
        st.loaded = false;
        st.devices.clear();
        st
    };
    let counts = |app: &mut App| {
        app.render()
            .into_iter()
            .find(|l| l.contains(" Adapters"))
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    };

    let mut app = App::new(110, 32);
    app.attach(vec![("/dev/ttyACM0", unlisted(second_adapter()))]);
    assert!(
        counts(&mut app).ends_with("│ 1 Adapters"),
        "{}",
        app.screen()
    );
    assert!(!app.shows("paired"), "{}", app.screen());

    // One adapter listed and one not: no total yet, and only the listed adapter's counts.
    let mut app = App::new(110, 32);
    app.attach(vec![
        ("/dev/ttyACM0", first_adapter()),
        ("/dev/ttyACM1", unlisted(second_adapter())),
    ]);
    assert!(
        counts(&mut app).ends_with("│ 2 Adapters"),
        "{}",
        app.screen()
    );
    assert!(app.shows("3 paired · 2 connected"), "{}", app.screen());
    assert!(!app.shows("0 paired"), "{}", app.screen());
    assert!(app.shows("Example Keys Wireless"), "{}", app.screen());

    let slot = app.slot("00000000000FA002");
    app.edit(slot, |st| *st = second_adapter());
    app.m.tick();
    assert!(
        counts(&mut app).ends_with("│ 3 Connected 4 Paired 2 Adapters"),
        "{}",
        app.screen()
    );
    assert!(app.shows("1 paired · 1 connected"), "{}", app.screen());

    // A listed adapter with no devices has a known count of zero.
    let mut empty = second_adapter();
    empty.devices.clear();
    let mut app = App::new(110, 32);
    app.attach(vec![("/dev/ttyACM0", empty)]);
    assert!(
        counts(&mut app).ends_with("│ 0 Connected 0 Paired 1 Adapters"),
        "{}",
        app.screen()
    );
    assert!(app.shows("0 paired · 0 connected"), "{}", app.screen());
}

/// The page's fact row labelled `label`, its spacing collapsed, if any.
fn fact_line(screen: &str, label: &str) -> Option<String> {
    screen
        .lines()
        .filter_map(|l| l.split_once("│ ").map(|(_, page)| page))
        .find(|l| l.starts_with(&format!("{label} ")))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[test]
fn adapter_storage_shows_what_is_known() {
    let mut st = first_adapter();
    st.status.information = None;
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Pico 2 W");
    // Nothing known of the adapter itself leaves no Information section.
    let screen = app.screen();
    assert!(
        !screen.contains("Information") && !screen.contains("Firmware"),
        "{screen}"
    );
    app.click("Diagnostics");
    assert!(!app.shows("Storage Format"), "{}", app.screen());
    // A status event brings zero values, which are known.
    let slot = app.slot("00000000000FA001");
    let storage = |count, limit| p::ResourceCapacity {
        r#type: p::ResourceType::Storage as i32,
        count,
        limit,
    };
    app.edit(slot, |st| {
        st.status.information = Some(p::AdapterInformation {
            storage_format: Some(0),
            ..Default::default()
        });
        st.status.resources = vec![storage(Some(0), Some(0))];
    });
    app.m.tick();
    let screen = app.screen();
    assert_eq!(
        fact_line(&screen, "Storage Format").as_deref(),
        Some("Storage Format 0"),
        "{screen}"
    );
    app.click("Details");
    let screen = app.screen();
    assert!(
        screen.contains("Information") && !screen.contains("Firmware"),
        "{screen}"
    );
    assert_eq!(
        fact_line(&screen, "Storage").as_deref(),
        Some("Storage 0 B of 0 B"),
        "{screen}"
    );
    for (resource, shown) in [
        (storage(Some(12_288), None), Some("Storage 12.0 KiB")),
        (
            storage(None, Some(1_048_576)),
            Some("Storage Up to 1.0 MiB"),
        ),
        (
            storage(Some(512), Some(2048)),
            Some("Storage 512 B of 2.0 KiB"),
        ),
        (storage(None, None), None),
    ] {
        app.edit(slot, |st| st.status.resources = vec![resource]);
        app.m.tick();
        let screen = app.screen();
        assert_eq!(fact_line(&screen, "Storage").as_deref(), shown, "{screen}");
    }
    assert!(!app.shows("Information"), "{}", app.screen());
}

#[test]
fn usb_errors_show_without_an_unknown_interface_count() {
    let mut st = first_adapter();
    st.status.resources = vec![p::ResourceCapacity {
        r#type: p::ResourceType::Interfaces as i32,
        count: None,
        limit: None,
    }];
    st.status.errors = vec![err(p::ErrorCode::UsbReservedCapacityExceeded)];
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Pico 2 W");
    let screen = app.screen();
    assert!(screen.contains("Reserved Capacity Exceeded"), "{screen}");
    assert!(
        !screen.contains("Interfaces") && !screen.contains("Unknown"),
        "{screen}"
    );
}

#[test]
fn device_diagnostics_leave_out_unreported_facts() {
    let mut st = first_adapter();
    // The mouse has HID++ without a detected version.
    st.devices[1].security = Some(p::Security {
        encrypted: Some(false),
        ..Default::default()
    });
    let mut app = App::new(110, 40);
    app.attach(vec![("/dev/ttyACM0", st)]);
    app.click("Example Mouse");
    app.click("Diagnostics");
    let screen = app.screen();
    assert!(screen.contains("Logitech Features"), "{screen}");
    assert!(
        !screen.contains("HID++ Protocol") && !screen.contains("Unknown"),
        "{screen}"
    );
    assert_eq!(
        fact_line(&screen, "Encrypted").as_deref(),
        Some("Encrypted No")
    );
    assert!(!screen.contains("Not Reported"), "{screen}");
    // Unreported encryption leaves the other reported facts in place.
    let slot = app.slot("00000000000FA001");
    app.edit(slot, |st| {
        st.devices[1].security = Some(p::Security {
            authenticated: Some(false),
            ..Default::default()
        })
    });
    app.m.tick();
    let screen = app.screen();
    assert!(app.shows("Security"), "{screen}");
    assert_eq!(
        fact_line(&screen, "Authenticated Pairing").as_deref(),
        Some("Authenticated Pairing No")
    );
    assert!(
        fact_line(&screen, "Encrypted").is_none() && !screen.contains("Not Reported"),
        "{screen}"
    );
    // Security that reports nothing leaves no section.
    app.edit(slot, |st| {
        st.devices[1].security = Some(p::Security::default())
    });
    app.m.tick();
    assert!(!app.shows("Security"), "{}", app.screen());
}

const MIGRATING_ID: &str = "00000000000FA002";

fn needs_migration() -> State {
    let mut st = second_adapter();
    st.status.ready = false;
    st.loaded = false;
    st.devices.clear();
    st.status.errors = vec![p::Error {
        code: p::ErrorCode::StorageMigrationRequired as i32,
        reason: p::ErrorReason::Storage as i32,
        outcome: p::ErrorOutcome::NoEffect as i32,
        ..Default::default()
    }];
    st
}

fn closed(app: &mut App, slot: Slot) {
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
}

fn opens(app: &App) -> usize {
    app.calls()
        .iter()
        .filter(|c| matches!(c, Call::Open(_)))
        .count()
}

fn migration_failed() -> Msg {
    Msg::Migrated(Err(cordial_client::migration::Error {
        reason: "the adapter refused a write".into(),
        outcome_unknown: false,
    }))
}

/// A TUI with a ready adapter, a port still opening, and an adapter that needs migration, which
/// starts closing every session. Returns the three slots.
fn migrating_app() -> (App, [Slot; 3]) {
    let mut app = App::new(100, 30);
    let ports = vec![
        port("/dev/ttyACM0", "00000000000FA001"),
        port("/dev/ttyACM1", MIGRATING_ID),
        port("/dev/ttyACM2", "00000000000FA003"),
    ];
    app.m.update(Msg::Ports(Ok(ports)));
    let slot = |app: &App, name: &str| {
        *app.m
            .probes
            .iter()
            .find(|(_, p)| p.as_str() == name)
            .unwrap()
            .0
    };
    let slots = [
        slot(&app, "/dev/ttyACM0"),
        slot(&app, "/dev/ttyACM1"),
        slot(&app, "/dev/ttyACM2"),
    ];
    app.fleet
        .0
        .borrow_mut()
        .states
        .insert(slots[0], first_adapter());
    app.phase(slots[0], Phase::Opened);
    app.phase(slots[0], Phase::Ready);
    app.fleet
        .0
        .borrow_mut()
        .states
        .insert(slots[1], needs_migration());
    app.fleet.0.borrow_mut().calls.clear();
    app.phase(slots[1], Phase::Migrate);
    (app, slots)
}

#[test]
fn migration_closes_every_session_and_open_before_its_worker_starts() {
    let (mut app, [a, b, c]) = migrating_app();
    let mut calls = app.calls();
    calls.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(calls, [Call::Close(a), Call::Close(b), Call::Close(c)]);
    closed(&mut app, a);
    closed(&mut app, b);
    // The port still opening has not released it yet.
    assert!(!app.calls().iter().any(|c| matches!(c, Call::Migrate(..))));
    closed(&mut app, c);
    assert_eq!(
        app.calls().last(),
        Some(&Call::Migrate(MIGRATING_ID.into(), "/dev/ttyACM1".into()))
    );
    app.m.update(Msg::Migration(
        cordial_protocol::migration::MigrationEvent {
            event: Some(
                cordial_protocol::migration::migration_event::Event::Waiting(Default::default()),
            ),
        },
    ));
    assert!(app.shows("Waiting for Adapter"));
    // Discovery stops: nothing is listed or opened while it migrates.
    app.fleet.0.borrow_mut().calls.clear();
    app.m.list_due = Instant::now();
    app.m.tick();
    app.m.update(Msg::Ports(Ok(vec![port(
        "/dev/ttyACM0",
        "00000000000FA001",
    )])));
    assert!(app.calls().is_empty(), "{:?}", app.calls());
    app.m.update(Msg::Migrated(Ok(Migrated::Success)));
    app.m.tick();
    assert_eq!(app.calls(), [Call::List]);
}

#[test]
fn a_failed_migration_opens_nothing_until_cancelled_and_is_not_repeated() {
    let (mut app, slots) = migrating_app();
    for slot in slots {
        closed(&mut app, slot);
    }
    app.m.update(migration_failed());
    app.m.open(Page::Adapter(MIGRATING_ID.into()));
    assert!(app.shows("Needs Attention"));
    assert!(app.shows("Cordial couldn't migrate the adapter's saved data."));
    app.fleet.0.borrow_mut().calls.clear();
    app.m.list_due = Instant::now();
    app.m.tick();
    app.m
        .update(Msg::Ports(Ok(vec![port("/dev/ttyACM1", MIGRATING_ID)])));
    assert!(app.calls().is_empty(), "{:?}", app.calls());
    // Migrate tries again without opening anything.
    app.click("Migrate");
    assert_eq!(
        app.calls(),
        [Call::Migrate(MIGRATING_ID.into(), "/dev/ttyACM1".into())]
    );
    app.m.update(migration_failed());
    app.fleet.0.borrow_mut().calls.clear();
    app.click("Cancel");
    app.m.tick();
    assert_eq!(app.calls(), [Call::List]);
    app.m
        .update(Msg::Ports(Ok(vec![port("/dev/ttyACM1", MIGRATING_ID)])));
    assert_eq!(opens(&app), 1);
    let slot = *app.m.probes.keys().next().unwrap();
    app.fleet
        .0
        .borrow_mut()
        .states
        .insert(slot, needs_migration());
    app.fleet.0.borrow_mut().calls.clear();
    app.phase(slot, Phase::Migrate);
    // A cancelled adapter waits for Migrate.
    assert!(app.calls().is_empty(), "{:?}", app.calls());
    app.m.open(Page::Adapter(MIGRATING_ID.into()));
    assert!(app.shows("Needs Attention"));
    app.click("Migrate");
    assert_eq!(app.calls(), [Call::Close(slot)]);
}

#[test]
fn a_failure_reported_after_cancel_ends_the_migration() {
    let (mut app, slots) = migrating_app();
    for slot in slots {
        closed(&mut app, slot);
    }
    app.m.open(Page::Adapter(MIGRATING_ID.into()));
    app.fleet.0.borrow_mut().calls.clear();
    // The worker failed before it saw the cancellation; its result arrives after.
    app.click("Cancel");
    assert_eq!(app.calls(), [Call::CancelMigration]);
    app.m.list_due = Instant::now();
    app.m.tick();
    assert_eq!(app.calls(), [Call::CancelMigration]);
    app.m.update(migration_failed());
    assert!(app.m.migration.is_none());
    assert!(app.m.declined.contains(MIGRATING_ID));
    app.m.tick();
    app.m.update(Msg::Ports(Ok(vec![
        port("/dev/ttyACM0", "00000000000FA001"),
        port("/dev/ttyACM1", MIGRATING_ID),
    ])));
    assert_eq!(opens(&app), 2);
}

#[test]
fn quitting_cancels_the_migration_and_waits_for_its_worker() {
    let (mut app, slots) = migrating_app();
    for slot in slots {
        closed(&mut app, slot);
    }
    app.fleet.0.borrow_mut().calls.clear();
    app.m.update(Msg::Interrupt);
    assert_eq!(app.calls(), [Call::CancelMigration]);
    assert!(!app.m.done);
    app.m.tick();
    assert!(!app.m.done);
    app.m.update(Msg::Migrated(Ok(Migrated::Cancelled)));
    assert!(app.m.done);
}

#[test]
fn an_unsupported_adapter_says_why_and_offers_nothing() {
    let mut app = App::new(100, 30);
    app.m.update(Msg::Ports(Ok(vec![port(
        "/dev/ttyACM0",
        "00000000000FA001",
    )])));
    let slot = *app.m.probes.keys().next().unwrap();
    let mut st = first_adapter();
    st.loaded = false;
    st.status
        .information
        .as_mut()
        .unwrap()
        .target_storage_format = Some(0);
    app.fleet.0.borrow_mut().states.insert(slot, st);
    app.phase(
        slot,
        Phase::Unsupported(Error::new(crate::migration::FIRMWARE)),
    );
    app.m.open(Page::Adapter("00000000000FA001".into()));
    assert!(app.shows("Unsupported"));
    assert!(app.shows("Update the adapter's firmware."));
    assert!(app.find("Rename").is_none());
    app.click("Diagnostics");
    assert!(app.find("Restart").is_none());
    assert!(!app.calls().iter().any(|c| matches!(c, Call::Run(..))));
}

#[test]
fn a_blocked_adapter_shows_its_error_and_offers_restart() {
    let mut app = App::new(100, 30);
    app.m.update(Msg::Ports(Ok(vec![port(
        "/dev/ttyACM0",
        "00000000000FA001",
    )])));
    let slot = *app.m.probes.keys().next().unwrap();
    let mut st = first_adapter();
    st.loaded = false;
    st.status.ready = false;
    app.fleet.0.borrow_mut().states.insert(slot, st);
    let blocked = p::Error {
        code: p::ErrorCode::StorageFailed as i32,
        outcome: p::ErrorOutcome::NoEffect as i32,
        related: vec![p::Error {
            code: p::ErrorCode::StorageFull as i32,
            ..Default::default()
        }],
        ..Default::default()
    };
    app.phase(slot, Phase::Blocked(Error::dongle(blocked, None)));
    assert!(!app.m.migrating());
    app.m.open(Page::Adapter("00000000000FA001".into()));
    assert!(app.shows("Needs Attention"));
    assert!(app.shows("The adapter couldn't read or write its saved data. Also, the"));
    assert!(app.shows("adapter's storage is full."));
    let shown = app.render().join("\n");
    assert!(!shown.contains("storage_f"), "{shown}");
    app.click("Diagnostics");
    app.click("Restart");
    assert!(app.ran("Restart"));
    assert!(!app.calls().iter().any(|c| matches!(c, Call::Migrate(..))));
}

const NEWER: &str = "This adapter requires a newer version of Cordial. Update Cordial to connect.";

/// An adapter's refusal of a newer protocol, with what its hello reports.
fn newer(f: impl FnOnce(&mut p::AdapterHello)) -> Error {
    let mut hello = p::AdapterHello {
        protocol_version: Some(p::CURRENT_PROTOCOL_VERSION + 1),
        ..Default::default()
    };
    f(&mut hello);
    let hello = cordial_client::compatibility::NativeHello {
        hello,
        bytes: Vec::new(),
    };
    Error::from_client(cordial_client::Error::Incompatible(hello), "adapter status")
}

impl App {
    /// Lists `ports` and refuses the probe of `refused` with `error`.
    fn refuse(&mut self, ports: Vec<PortInfo>, refused: &str, error: Error) {
        self.m.update(Msg::Ports(Ok(ports)));
        let slot = self.probe(refused).unwrap();
        self.phase(slot, Phase::Failed { error, open: false });
        self.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
        self.fleet.0.borrow_mut().calls.clear();
    }

    fn probe(&self, port: &str) -> Option<Slot> {
        self.m
            .probes
            .iter()
            .find(|(_, p)| p.as_str() == port)
            .map(|(s, _)| *s)
    }

    fn opened(&self) -> bool {
        self.calls().iter().any(|c| matches!(c, Call::Open(_)))
    }
}

#[test]
fn a_refused_adapter_stays_listed_with_why_and_without_its_metadata() {
    let mut app = App::new(140, 32);
    app.refuse(
        vec![port("/dev/ttyACM0", "")],
        "/dev/ttyACM0",
        newer(|_| {}),
    );
    let screen = app.screen();
    assert!(screen.contains("Adapter"), "{screen}");
    assert!(screen.contains("Needs Attention"), "{screen}");
    app.m.open(Page::Adapter("/dev/ttyACM0".into()));
    assert!(app.shows(NEWER), "{}", app.screen());
    assert!(app.find("[Connect]").is_some());
    app.click("Diagnostics");
    assert!(!app.shows("/dev/ttyACM0"), "the port isn't its ID");
    assert!(!app.shows("Identifiers"));
}

#[test]
fn a_refused_adapter_shows_what_it_reported() {
    let mut app = App::new(140, 32);
    app.refuse(
        vec![port("/dev/ttyACM0", "")],
        "/dev/ttyACM0",
        newer(|h| {
            h.name = Some("Desk".into());
            h.firmware_version = Some("9.0.0".into());
            h.serial_number = Some("00000000000FA009".into());
        }),
    );
    app.click("Desk");
    assert!(app.shows("9.0.0"), "{}", app.screen());
    assert!(app.shows(NEWER));
    app.click("Diagnostics");
    assert!(app.shows("Serial Number"));
    assert!(app.shows("00000000000FA009"));
    assert!(!app.shows("Adapter ID"));
}

#[test]
fn a_refused_adapter_takes_no_configuration() {
    let mut app = App::new(140, 32);
    app.refuse(
        vec![port("/dev/ttyACM0", "")],
        "/dev/ttyACM0",
        newer(|_| {}),
    );
    app.m.open(Page::Adapter("/dev/ttyACM0".into()));
    for tab in ["Settings", "Diagnostics", "Details"] {
        app.click(tab);
        assert!(app.find("Rename").is_none(), "{tab}:\n{}", app.screen());
        assert!(!app.shows("Platform"), "{tab}");
        app.ctrl('s');
    }
    app.m.update(Msg::Key(KeyEvent::new(
        KeyCode::Char('n'),
        KeyModifiers::CONTROL,
    )));
    assert!(app.m.ready_adapters().is_empty());
    assert!(app.m.jobs.is_empty());
    assert!(
        !app.calls().iter().any(|c| matches!(c, Call::Run(..))),
        "{:?}",
        app.calls()
    );
}

#[test]
fn a_refused_port_is_tried_again_only_on_refresh_connect_or_its_return() {
    let ports = || vec![port("/dev/ttyACM0", "")];
    let mut app = App::new(140, 32);
    app.refuse(ports(), "/dev/ttyACM0", newer(|_| {}));
    assert!(!app.m.failed.contains_key("/dev/ttyACM0"));
    for _ in 0..3 {
        app.m.update(Msg::Ports(Ok(ports())));
    }
    assert!(!app.opened(), "no automatic retry: {:?}", app.calls());

    // Refresh.
    app.ctrl('r');
    assert!(app.calls().contains(&Call::List));
    app.m.update(Msg::Ports(Ok(ports())));
    assert!(app.opened());
    assert!(app.shows("Connecting…"));
    let slot = app.probe("/dev/ttyACM0").unwrap();
    app.phase(
        slot,
        Phase::Failed {
            error: newer(|_| {}),
            open: false,
        },
    );
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    app.fleet.0.borrow_mut().calls.clear();
    app.m.update(Msg::Ports(Ok(ports())));
    assert!(!app.opened());

    // Connect, from the adapter's page.
    app.m.open(Page::Adapter("/dev/ttyACM0".into()));
    app.click("[Connect]");
    app.m.update(Msg::Ports(Ok(ports())));
    assert!(app.opened());
    let slot = app.probe("/dev/ttyACM0").unwrap();
    app.phase(
        slot,
        Phase::Failed {
            error: newer(|_| {}),
            open: false,
        },
    );
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    assert!(app.shows(NEWER));
    app.fleet.0.borrow_mut().calls.clear();

    // Unplugged and plugged in again.
    app.m.update(Msg::Ports(Ok(Vec::new())));
    assert!(app.m.refused.is_empty());
    app.m.update(Msg::Ports(Ok(ports())));
    assert!(app.opened());
}

#[test]
fn a_refused_adapter_that_answers_is_known_by_its_status_id() {
    let ports = || vec![port("/dev/ttyACM0", "")];
    let mut app = App::new(140, 32);
    app.refuse(
        ports(),
        "/dev/ttyACM0",
        newer(|h| h.serial_number = Some("X".into())),
    );
    app.m.open(Page::Adapter("/dev/ttyACM0".into()));
    app.click("[Connect]");
    app.m.update(Msg::Ports(Ok(ports())));
    let slot = app.probe("/dev/ttyACM0").unwrap();
    app.fleet
        .0
        .borrow_mut()
        .states
        .insert(slot, second_adapter());
    app.phase(slot, Phase::Opened);
    app.phase(slot, Phase::Ready);
    assert!(app.m.refused.is_empty());
    assert_eq!(app.m.session_of("00000000000FA002").unwrap().slot, slot);
    assert_eq!(app.m.shown(), Page::Adapter("00000000000FA002".into()));
    assert!(app.shows("ESP32-S3"));
    assert!(!app.shows(NEWER));
}

#[test]
fn connecting_a_disconnected_adapter_that_refuses_shows_why() {
    let mut app = App::two(140, 32);
    app.click("ESP32-S3");
    app.click("Disconnect");
    app.m.update(Msg::Fleet(2, Box::new(Event::Closed)));
    app.fleet.0.borrow_mut().calls.clear();
    app.click("[Connect]");
    let slot = app.m.off["00000000000FA002"].connecting.unwrap();
    app.phase(
        slot,
        Phase::Failed {
            error: newer(|_| {}),
            open: false,
        },
    );
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    assert!(app.shows(NEWER), "{}", app.screen());
    assert!(app.shows("ESP32-S3"), "it keeps its name");
    assert!(
        app.m.refused.is_empty(),
        "it stays the adapter the user disconnected"
    );
    app.fleet.0.borrow_mut().calls.clear();
    app.m.update(Msg::Ports(Ok(vec![
        port("/dev/ttyACM0", "00000000000FA001"),
        port("/dev/ttyACM1", "00000000000FA002"),
    ])));
    assert!(!app.opened(), "not tried again by itself");
}

#[test]
fn a_held_adapter_whose_port_refuses_shows_the_refusal() {
    let mut app = App::two(140, 32);
    app.click("Pico 2 W");
    let slot = app.slot("00000000000FA001");
    app.phase(slot, Phase::Lost(Error::new("gone")));
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    app.refuse(
        vec![
            port("/dev/ttyACM2", "00000000000FA001"),
            port("/dev/ttyACM1", "00000000000FA002"),
        ],
        "/dev/ttyACM2",
        newer(|_| {}),
    );
    assert!(app.m.held.is_empty());
    assert_eq!(app.m.shown(), Page::Adapter("/dev/ttyACM2".into()));
    assert!(app.shows(NEWER), "{}", app.screen());
    assert!(
        !app.shows("Example Mouse"),
        "its saved devices aren't shown"
    );
}

#[test]
fn a_refused_port_is_not_tried_again_while_an_adapter_migrates() {
    let ports = || vec![port("/dev/ttyACM0", ""), port("/dev/ttyACM1", MIGRATING_ID)];
    let mut app = App::new(140, 32);
    app.refuse(ports(), "/dev/ttyACM0", newer(|_| {}));
    let slot = app.probe("/dev/ttyACM1").unwrap();
    app.fleet
        .0
        .borrow_mut()
        .states
        .insert(slot, needs_migration());
    app.phase(slot, Phase::Migrate);
    closed(&mut app, slot);
    assert_eq!(
        app.calls().last(),
        Some(&Call::Migrate(MIGRATING_ID.into(), "/dev/ttyACM1".into()))
    );
    app.fleet.0.borrow_mut().calls.clear();

    app.m.open(Page::Adapter("/dev/ttyACM0".into()));
    app.click("[Connect]");
    app.ctrl('r');
    app.m.list_due = Instant::now();
    app.m.tick();
    app.m.update(Msg::Ports(Ok(ports())));
    assert!(app.calls().is_empty(), "{:?}", app.calls());
    assert!(app.shows(NEWER), "{}", app.screen());

    // After the migration, the refused port still waits for Refresh or Connect.
    app.m.update(Msg::Migrated(Ok(Migrated::Success)));
    app.m.tick();
    app.m.update(Msg::Ports(Ok(ports())));
    assert_eq!(opens(&app), 1);
    assert!(app.probe("/dev/ttyACM0").is_none());
    app.m.open(Page::Adapter("/dev/ttyACM0".into()));
    app.click("[Connect]");
    app.m.update(Msg::Ports(Ok(ports())));
    assert!(app.probe("/dev/ttyACM0").is_some());
}

#[test]
fn a_refused_port_opening_when_a_migration_begins_is_closed_first() {
    let ports = || vec![port("/dev/ttyACM0", ""), port("/dev/ttyACM1", MIGRATING_ID)];
    let mut app = App::new(140, 32);
    app.refuse(ports(), "/dev/ttyACM0", newer(|_| {}));
    let migrating = app.probe("/dev/ttyACM1").unwrap();
    app.ctrl('r');
    app.m.update(Msg::Ports(Ok(ports())));
    let retry = app.probe("/dev/ttyACM0").unwrap();
    app.fleet
        .0
        .borrow_mut()
        .states
        .insert(migrating, needs_migration());
    app.fleet.0.borrow_mut().calls.clear();
    app.phase(migrating, Phase::Migrate);
    let mut calls = app.calls();
    calls.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(calls, [Call::Close(migrating), Call::Close(retry)]);
    closed(&mut app, migrating);
    assert!(!app.calls().iter().any(|c| matches!(c, Call::Migrate(..))));
    // The refusal arrives after its session was told to close; the port opens no further.
    app.phase(
        retry,
        Phase::Failed {
            error: newer(|_| {}),
            open: false,
        },
    );
    closed(&mut app, retry);
    assert_eq!(
        app.calls().last(),
        Some(&Call::Migrate(MIGRATING_ID.into(), "/dev/ttyACM1".into()))
    );
    assert_eq!(opens(&app), 0);
}
