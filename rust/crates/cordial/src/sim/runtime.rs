//! Seeded adapters and their discovery and migration backends for interface smoke tests.
use super::*;
use crate::ui::app::{Live, Sink};
use cordial_client::{migration, serial::PortInfo};
use p::migration::{
    MigrationCancelled, MigrationEvent, MigrationPhase, MigrationProgress, MigrationSuccess,
    migration_event,
};
use std::fs::OpenOptions;

/// Independent simulated adapters. Retain this handle to control an interface test's devices.
#[derive(Clone)]
pub struct Desktop {
    adapters: Vec<Dongle>,
    present: Arc<Mutex<Vec<bool>>>,
}

/// Builds the shared application backend without opening hardware or starting another process.
pub fn desktop(sink: Sink, count: usize) -> Live {
    let desktop = Desktop::new(count);
    let delay = std::env::var("CORDIAL_SIM_WRITE_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(2000);
    for adapter in desktop.adapters() {
        let mut sim = adapter.0.lock().unwrap();
        for command in ["set_adapter", "set_device", "set_settings"] {
            sim.slow.insert(command, Duration::from_millis(delay));
        }
        if std::env::var("CORDIAL_SIM_FAIL_SETTINGS_ONCE").as_deref() == Ok("1") {
            sim.fail_late
                .insert("set_settings", ErrorCode::StorageFailed);
        }
    }
    let reconnect_ms = std::env::var("CORDIAL_SIM_PAIRING_RECONNECT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|&value| value > 0);
    if let Some(reconnect_ms) = reconnect_ms
        && let Some(adapter) = desktop.adapters().first()
    {
        let (notify, paired) = mpsc::channel();
        adapter.0.lock().unwrap().pairing_reconnect = Some(notify);
        let adapter = Arc::downgrade(&adapter.0);
        let present = desktop.present.clone();
        thread::spawn(move || {
            while paired.recv().is_ok() {
                let Some(adapter) = adapter.upgrade() else {
                    return;
                };
                {
                    let mut present = present.lock().unwrap();
                    let mut sim = adapter.lock().unwrap();
                    reconnect_trace(&sim, "done");
                    present[0] = false;
                    sim.end_session(p::SessionEndingReason::DeviceConnection as i32);
                    reconnect_trace(&sim, "closed");
                }
                thread::sleep(Duration::from_millis(reconnect_ms.clamp(100, 10000)));
                let mut present = present.lock().unwrap();
                present[0] = true;
                reconnect_trace(&adapter.lock().unwrap(), "present");
            }
        });
    }
    desktop.live(sink)
}

fn reconnect_trace(sim: &Sim, stage: &str) {
    if let Ok(path) = std::env::var("CORDIAL_SIM_PAIRING_RECONNECT_TRACE")
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(
            file,
            "{stage} {} {} {}",
            sim.status.id, sim.session, sim.paired.id
        );
    }
}

impl Desktop {
    /// Seeds `count` adapters with devices, settings, profiles, files and pairing candidates.
    pub fn new(count: usize) -> Self {
        Self {
            adapters: (0..count)
                .map(|index| Dongle::with(|s| seed(s, index)))
                .collect(),
            present: Arc::new(Mutex::new(vec![true; count])),
        }
    }

    /// Adapter handles in discovery order, including adapters temporarily absent from discovery.
    pub fn adapters(&self) -> &[Dongle] {
        &self.adapters
    }

    /// Lists the simulated ports currently attached. No serial discovery is performed.
    pub fn ports(&self) -> Vec<PortInfo> {
        let present = self.present.lock().unwrap();
        self.adapters
            .iter()
            .enumerate()
            .filter(|(i, _)| present[*i])
            .map(|(i, d)| PortInfo {
                port: format!("simulated:{}", i + 1),
                serial: d.0.lock().unwrap().status.id.clone(),
            })
            .collect()
    }

    /// Attaches or unplugs an adapter. Call the application's normal discovery tick afterward.
    pub fn set_present(&self, index: usize, present: bool) -> bool {
        let Some(adapter) = self.adapters.get(index) else {
            return false;
        };
        let mut attached = self.present.lock().unwrap();
        attached[index] = present;
        if !present {
            adapter.unplug();
        }
        true
    }

    /// Creates controllers using framed connections and the application's migration worker.
    pub fn live(&self, sink: Sink) -> Live {
        let connections = self.clone();
        let ports = self.clone();
        let migration = self.clone();
        Live::new(sink, None).with_backend(
            Arc::new(move |port, handler| connections.open(port, handler)),
            move || Ok(ports.ports()),
            Arc::new(move |id, port, cancelled, report| {
                migration.migrate(id, port, cancelled, report)
            }),
        )
    }

    fn open(&self, port: &str, handler: Handler) -> io::Result<Connection> {
        let index = port
            .strip_prefix("simulated:")
            .and_then(|s| s.parse::<usize>().ok())
            .and_then(|n| n.checked_sub(1))
            .filter(|&i| i < self.adapters.len())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        // Keep discovery and opening consistent when a test unplugs the adapter.
        let present = self.present.lock().unwrap();
        if !present[index] {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let connection = self.adapters[index].connect(handler)?;
        let sim = self.adapters[index].0.lock().unwrap();
        if sim.pairing_reconnect.is_some() {
            reconnect_trace(&sim, "opened");
        }
        Ok(connection)
    }

    // The simulated engine exercises worker progress and ownership without changing disk data.
    fn migrate(
        &self,
        id: &str,
        _port: Option<&str>,
        cancelled: &std::sync::atomic::AtomicBool,
        report: &mut dyn FnMut(MigrationEvent),
    ) -> migration::Result<migration::Outcome> {
        let failure = |reason: &str| migration::Error {
            reason: reason.into(),
            outcome_unknown: false,
        };
        let adapter = self
            .adapters
            .iter()
            .find(|d| d.0.lock().unwrap().status.id == id)
            .ok_or_else(|| failure("the simulated adapter was not found"))?;
        if adapter.handles() != 0 {
            return Err(failure("the simulated adapter is still in use"));
        }
        let status = adapter.0.lock().unwrap().status.clone();
        let target = migration::check_target(&status)?;
        for phase in [
            MigrationPhase::Preflight,
            MigrationPhase::Download,
            MigrationPhase::Convert,
            MigrationPhase::Upload,
            MigrationPhase::Verify,
            MigrationPhase::Confirm,
        ] {
            if cancelled.load(Ordering::Acquire) {
                report(MigrationEvent {
                    event: Some(migration_event::Event::Cancelled(MigrationCancelled {})),
                });
                return Ok(migration::Outcome::Cancelled);
            }
            report(MigrationEvent {
                event: Some(migration_event::Event::Progress(MigrationProgress {
                    phase: phase as i32,
                    source_format: status
                        .information
                        .as_ref()
                        .and_then(|i| i.storage_format)
                        .unwrap_or_default(),
                    target_format: target,
                    checkpoint_format: target,
                    ..Default::default()
                })),
            });
        }
        {
            let mut sim = adapter.0.lock().unwrap();
            sim.status
                .errors
                .retain(|e| e.code() != ErrorCode::StorageMigrationRequired);
            sim.status.information.as_mut().unwrap().storage_format = Some(target);
            sim.status.ready = true;
        }
        report(MigrationEvent {
            event: Some(migration_event::Event::Success(MigrationSuccess {
                adapter_id: id.into(),
                target_format: target,
            })),
        });
        Ok(migration::Outcome::Success)
    }
}

fn integer(value: i64, saved: Option<i64>, min: i64, max: i64, step: u64) -> p::IntegerSetting {
    p::IntegerSetting {
        value: Some(value),
        saved,
        status: saved.and_then(|_| applied()),
        limits: Some(p::integer_setting::Limits::Range(p::IntegerRange {
            min,
            max,
            step,
        })),
    }
}

fn boolean(value: bool, saved: Option<bool>) -> p::BoolSetting {
    p::BoolSetting {
        value: Some(value),
        saved,
        status: saved.and_then(|_| applied()),
    }
}

fn object(group: settings_object::Group) -> p::SettingsObject {
    p::SettingsObject {
        integration: IntegrationKind::Hidpp as i32,
        group: Some(group),
    }
}

fn connected(id: u32, name: &str, kind: p::Kind, role: Role, level: u32) -> p::Device {
    let mut d = device(id, name, Transport::Ble);
    d.kinds = vec![kind as i32];
    d.roles = vec![role as i32];
    d.state = DeviceState::Connected as i32;
    d.information = Some(p::DeviceInformation {
        identity: Some(p::DeviceIdentity {
            manufacturer: Some("Example Co".into()),
            model: Some(name.into()),
            ..Default::default()
        }),
        battery: Some(p::BatteryInformation {
            level: Some(level),
            charging: Some(false),
        }),
        ..Default::default()
    });
    d.integrations[0].status = Some(p::integration::Status::State(
        p::IntegrationState::Active as i32,
    ));
    d
}

fn seed(sim: &mut Sim, index: usize) {
    sim.status.id = format!("SIMULATED{:07}", index + 1);
    sim.status.name = format!("Example Adapter {}", index + 1);
    enable_gamepads(&mut sim.status);
    enable_profiles(&mut sim.status);
    if index > 0 {
        sim.status
            .transports
            .retain(|t| t.transport == Transport::Ble as i32);
    }
    for transport in &mut sim.status.transports {
        transport.resources = vec![resource(ResourceType::Devices, Some(4), Some(7))];
    }
    let mut mouse = connected(
        1,
        if index == 0 {
            "Example Mouse"
        } else {
            "Example Trackball"
        },
        p::Kind::Mouse,
        Role::Mouse,
        72,
    );
    mouse.profiles = Some(p::ProfileLayers { profiles: vec![2] });
    mouse.information.as_mut().unwrap().wheel = Some(p::WheelInformation {
        resolution_multiplier: Some(8),
        ratchets_per_rotation: Some(24),
        diameter: Some(50),
        ..Default::default()
    });
    let mut keys = connected(
        2,
        "Example Keys Wireless",
        p::Kind::Keyboard,
        Role::Keyboard,
        54,
    );
    keys.roles.push(Role::ConsumerControl as i32);
    keys.profiles = Some(p::ProfileLayers { profiles: vec![1] });
    keys.information.as_mut().unwrap().backlight = Some(p::BacklightInformation {
        current_brightness: Some(3),
        ..Default::default()
    });
    let mut travel = device(
        3,
        "Travel Keyboard",
        if index == 0 {
            Transport::Classic
        } else {
            Transport::Ble
        },
    );
    travel.kinds = vec![p::Kind::Keyboard as i32];
    travel.roles = vec![Role::Keyboard as i32];
    travel.profiles = Some(p::ProfileLayers::default());
    let mut pad = connected(4, "Example Controller", p::Kind::Gamepad, Role::Gamepad, 64);
    pad.integrations.clear();
    pad.output_preferences = vec![p::OutputPreference {
        role: Role::Gamepad as i32,
        mode: p::OutputMode::Xinput as i32,
    }];
    pad.outputs = vec![p::OutputStatus {
        role: Role::Gamepad as i32,
        mode: p::OutputMode::Xinput as i32,
        status: Some(p::output_status::Status::State(
            p::OutputState::Active as i32,
        )),
        slot: Some(0),
    }];
    pad.profiles = Some(p::ProfileLayers::default());
    sim.devices = vec![mouse, keys, travel, pad];
    sim.settings.insert(
        1,
        vec![
            dpi(0, 1000, Some(1000)),
            object(settings_object::Group::Wheel(p::WheelSettings {
                mode: Some(p::WheelModeSetting {
                    value: Some(p::WheelMode::Stepped as i32),
                    choices: vec![p::WheelMode::FreeSpin as i32, p::WheelMode::Stepped as i32],
                    ..Default::default()
                }),
                invert: Some(boolean(false, None)),
                free_spin_threshold: Some(integer(30, None, 1, 255, 1)),
            })),
            object(settings_object::Group::Thumbwheel(p::ThumbwheelSettings {
                invert: Some(boolean(false, None)),
            })),
        ],
    );
    sim.settings.insert(
        2,
        vec![
            object(settings_object::Group::Keyboard(p::KeyboardSettings {
                platform: Some(p::KeyboardPlatformSetting {
                    value: Some(p::KeyboardPlatform::Linux as i32),
                    choices: vec![
                        p::KeyboardPlatform::Windows as i32,
                        p::KeyboardPlatform::Linux as i32,
                        p::KeyboardPlatform::Mac as i32,
                    ],
                    ..Default::default()
                }),
                function_row: Some(p::FunctionRowModeSetting {
                    value: Some(p::FunctionRowMode::SpecialActions as i32),
                    choices: vec![
                        p::FunctionRowMode::FunctionKeys as i32,
                        p::FunctionRowMode::SpecialActions as i32,
                    ],
                    ..Default::default()
                }),
            })),
            object(settings_object::Group::Power(p::PowerSettings {
                auto_off: Some(integer(1800, None, 0, 15300, 60)),
            })),
            object(settings_object::Group::Backlight(p::BacklightSettings {
                enabled: Some(boolean(true, Some(true))),
                mode: Some(p::BacklightModeSetting {
                    value: Some(p::BacklightMode::Automatic as i32),
                    choices: vec![
                        p::BacklightMode::Automatic as i32,
                        p::BacklightMode::PermanentManual as i32,
                    ],
                    ..Default::default()
                }),
                brightness: Some(integer(3, None, 0, 7, 1)),
                delay: Some(p::BacklightDelaySettings {
                    hands_away: Some(p::IntegerSetting {
                        value: Some(60),
                        saved: Some(30),
                        status: Some(p::SettingStatus {
                            result: Some(p::setting_status::Result::State(
                                SettingState::ChangedOnDevice as i32,
                            )),
                        }),
                        ..integer(60, Some(30), 5, 7200, 5)
                    }),
                    hands_nearby: Some(integer(60, None, 5, 7200, 5)),
                    external_power: Some(integer(300, None, 5, 7200, 5)),
                }),
                startup_effect: Some(boolean(true, None)),
                dial_effect: Some(boolean(false, None)),
                power_save: Some(boolean(true, None)),
                effect: Some(p::BacklightEffectSetting {
                    value: Some(p::BacklightEffect::Static as i32),
                    choices: vec![
                        p::BacklightEffect::Static as i32,
                        p::BacklightEffect::Breathing as i32,
                    ],
                    ..Default::default()
                }),
            })),
        ],
    );
    // A discrete integer list exercises the same control used by devices with fixed DPI steps.
    sim.settings
        .get_mut(&1)
        .unwrap()
        .push(object(settings_object::Group::PointerSensor(
            p::PointerSensorSettings {
                index: 1,
                dpi: Some(p::IntegerSetting {
                    value: Some(800),
                    limits: Some(p::integer_setting::Limits::Choices(p::IntegerChoices {
                        values: vec![400, 800, 1600],
                    })),
                    ..Default::default()
                }),
            },
        )));
    for n in 0..12 {
        let (name, collection, input, effect) = match n {
            0 => (
                "Typing".to_string(),
                KEYBOARD,
                (7, 0x39),
                profile_rule::Effect::Remap(p::profile_rule::Remap {
                    outputs: vec![p::profile_rule::Output {
                        usage: Some(usage(7, 0x29)),
                        collection: Some(usage(KEYBOARD.0, KEYBOARD.1)),
                    }],
                }),
            ),
            1 => (
                "Natural Scrolling".to_string(),
                MOUSE,
                (1, 0x38),
                profile_rule::Effect::Scale(p::profile_rule::Scale {
                    numerator: -1,
                    denominator: 1,
                }),
            ),
            _ => (
                format!("Example Profile {}", n + 1),
                GAMEPAD,
                (1, 0x30),
                profile_rule::Effect::Axis(p::profile_rule::Axis {
                    destination: Some(usage(1, 0x31)),
                    invert: false,
                }),
            ),
        };
        sim.add_profile(
            &name,
            vec![p::ProfileRule {
                input: Some(usage(input.0, input.1)),
                collection: Some(usage(collection.0, collection.1)),
                effect: Some(effect),
            }],
        );
    }
    sim.status.configuration_interfaces[0].profile = 1;
    sim.status.configuration_interfaces[1].profile = 2;
    sim.files.insert(
        "/".into(),
        vec![p::FileEntry {
            name: "examples".into(),
            directory: true,
            size: 0,
        }],
    );
    sim.files.insert(
        "/examples".into(),
        vec![p::FileEntry {
            name: "sample.txt".into(),
            directory: false,
            size: 15,
        }],
    );
    sim.data
        .insert("/examples/sample.txt".into(), b"Example device\n".to_vec());
    sim.scan_ends = false;
    sim.candidates = vec![
        candidate(1, "Example Keys Mini"),
        candidate(2, "Example Pebble"),
        candidate(3, "Example Display Keyboard"),
        candidate(4, "Example Numpad"),
        candidate(5, ""),
    ];
    sim.candidates[1].kinds = vec![p::Kind::Mouse as i32];
    sim.candidates[4].kinds.clear();
    for (candidate, plan) in [
        (1, Plan::EnterCode("123456".into())),
        (2, Plan::Confirm("123456".into())),
        (3, Plan::ShowCode("123456".into())),
        (4, Plan::Done),
        (5, Plan::Done),
    ] {
        let c = sim.candidates.iter().find(|c| c.id == candidate).unwrap();
        let mut d = connected(
            10 + candidate,
            &c.name,
            if candidate == 2 {
                p::Kind::Mouse
            } else {
                p::Kind::Keyboard
            },
            if candidate == 2 {
                Role::Mouse
            } else {
                Role::Keyboard
            },
            80,
        );
        d.profiles = Some(p::ProfileLayers::default());
        sim.pairings.insert(candidate, (plan, d));
    }
}
