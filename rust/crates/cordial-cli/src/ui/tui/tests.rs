//! Model tests against stand-in sessions: what the TUI draws, and which commands clicks and keys
//! send.
use super::*;
use crate::{
    controller::Target,
    model,
    ui::{
        catalog::tests::{boolean, choice, integer},
        command::tests as fixture,
    },
};
use cordial_protocol::{DeviceState, InactiveReason, IntegrationKind, keys};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Open(String),
    Close(Slot),
    Run(Slot, String),
    List,
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

fn battery(d: &mut p::Device, percent: i64) {
    d.info.push(p::Info {
        key: keys::BATTERY_LEVEL.into(),
        value: Some(model::wire_value(Value::Integer(percent))),
    });
}

/// A mouse with Logitech Features up and three settings.
fn mouse_settings() -> Vec<p::Setting> {
    let mut dpi = integer("pointer.sensor.0.dpi", 200, 4000, 50);
    if let Some(p::setting::Type::Integer(i)) = &mut dpi.r#type {
        i.value = Some(1000);
        i.saved = Some(1000);
    }
    dpi.status = Some(p::setting::Status::State(p::SettingState::Applied as i32));
    let mut mode = choice(keys::WHEEL_MODE, &["freespin", "ratchet"]);
    if let Some(p::setting::Type::Enum(e)) = &mut mode.r#type {
        e.value = Some("ratchet".into());
    }
    let mut invert = boolean(keys::WHEEL_INVERT);
    if let Some(p::setting::Type::Bool(b)) = &mut invert.r#type {
        b.value = Some(false);
    }
    vec![dpi, mode, invert]
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
    st.status.name = "XIAO ESP32-S3".into();
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
        "XIAO ESP32-S3",
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
    app.click("XIAO ESP32-S3");
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
    assert!(!app.calls().contains(&Call::Open("/dev/ttyACM1".into())));
    app.m.update(Msg::Fleet(slot, Box::new(Event::Closed)));
    assert!(app.calls().contains(&Call::Open("/dev/ttyACM1".into())));
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

#[test]
fn settings_save_sends_every_change_in_one_request() {
    let mut app = App::two(110, 32);
    app.click("Example Mouse");
    app.click("Settings");
    app.click("[○ Freespin]");
    app.click("[ ] Off");
    app.click("[Save]");
    assert!(app.ran("SettingsSave { device: 2"));
    assert!(app.ran("wheel.mode"));
    assert!(app.ran("wheel.invert"));
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
fn a_reconnecting_save_asks_first() {
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
    assert!(app.shows("USB Reconnect Required"));
    app.click("[Save]");
    assert!(app.ran("AdapterSave"));
}

#[test]
fn add_device_scans_pairs_and_stops_on_close() {
    let mut app = App::two(110, 32);
    app.ctrl('n');
    assert!(app.ran("Scan { transports: [], seconds: 30 }"));
    assert_eq!(app.m.dialog, Some(Dialog::AddDevice));
    app.finish(Ok(Outcome::ScanStarted(vec![p::Transport::Ble])));
    assert_eq!(app.m.dialog, Some(Dialog::AddDevice), "{:?}", app.m.add);
    let slot = app.slot("00000000000FA001");
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
    st.status.transports[1].max_enabled = Some(1);
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
    assert!(app.calls().contains(&Call::Open("/dev/ttyACM5".into())));
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
