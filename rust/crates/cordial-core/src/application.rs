//! Serial command handling and asynchronous Bluetooth operations.
//!
//! Every command validates, acts and responds at once. Work that outlives a command (scanning,
//! pairing, an unpair waiting for its link to close, settings jobs) lives here and reports its
//! progress through events. Each event carries the complete current state of one thing; changed
//! things are marked dirty and written one frame at a time when the serial output is free.
use alloc::{boxed::Box, collections::VecDeque, format, string::String, vec::Vec};

use cordial_protocol::{self as p, event::Kind as Ev, request::Command, response::Result as R};

use crate::{
    bluetooth::{Bluetooth, Event},
    control::Session,
    devices::{self, Peer},
    link::LinkId,
    manager::Manager,
    model::{
        errors::ErrorCode as Error,
        identifiers::{ConnectionState, Transport},
        link::{DeviceKind, PromptMethod},
    },
    settings::Change,
    storage::{Preferences, RecordStore},
    wire,
};

pub struct Build {
    /// Development firmware: the development commands are available.
    pub development: bool,
    pub version: &'static str,
    /// Hardware configuration the firmware was built for.
    pub board: &'static str,
    pub default_adapter_name: &'static str,
    pub adapter_id: String,
    /// Production composition supplies None and does not link the entry function.
    pub bootloader: Option<Bootloader>,
}
pub struct Bootloader {
    pub enter: fn() -> !,
}

struct Candidate {
    id: String,
    peer: Peer,
    address: Peer,
    kind: DeviceKind,
    name: Box<str>,
    rssi: Option<i32>,
    dirty: bool,
}

struct Scan {
    token: u64,
    classic: bool,
    ble: bool,
    deadline: u64,
}

struct Prompt {
    method: PromptMethod,
    deadline: u64,
    answered: bool,
}

struct Pair {
    candidate: String,
    link: Option<LinkId>,
    address: Peer,
    expected: Option<Peer>,
    /// A native bond to remove once the attempt's link has closed.
    cleanup: Option<Peer>,
    deadline: u64,
    name: Box<str>,
    /// Set once the attempt is ending, with the error it ends with.
    cancelling: Option<Error>,
    prompt: Option<Prompt>,
    /// The session that started the attempt has ended, so its remaining events go nowhere.
    quiet: bool,
}

/// Background settings work a device is waiting for.
#[derive(Clone, Copy, Default)]
struct Jobs {
    /// Re-read device information, before `read`.
    information: bool,
    /// Re-read settings.
    read: bool,
    apply: bool,
}

const DEVICE: u8 = 1;
const SETTINGS: u8 = 2;
const WARNINGS: u8 = 4;

const PAIR_TIMEOUT_MS: u64 = 120_000;
const PROMPT_TIMEOUT_MS: u64 = 30_000;
const CONNECT_TIMEOUT_MS: u64 = 30_000;
const PAGE_BUSY_MS: u64 = 1000;
const SCAN_DEFAULT_SECONDS: u32 = 10;
const SCAN_MAX_SECONDS: u32 = 60;

pub struct Application {
    pub manager: Manager,
    pub serial: Session,
    build: Build,
    candidates: Vec<Candidate>,
    candidate_seq: u64,
    scan: Option<Scan>,
    scan_seq: u64,
    radio_scan: Option<(u64, bool, bool)>,
    truncated: bool,
    pair: Option<Pair>,
    pairing: Option<p::Pairing>,
    pairing_dirty: bool,
    scan_done: Option<p::ScanDone>,
    /// Devices whose unpair waits for their link to close, by policy ID.
    unpairing: Vec<u64>,
    jobs: Vec<(u64, Jobs)>,
    dirty: Vec<u8>,
    removed: VecDeque<String>,
    adapter_dirty: bool,
    /// The readiness last reported, so any change sends an adapter event.
    reported_ready: bool,
    /// The device slot whose events go first next time, so devices take turns.
    next_slot: usize,
    /// Scan results go before device events next time, so neither starves the other.
    scan_turn: bool,
    reboot_at: Option<u64>,
}

fn failure(code: Error) -> p::Error {
    wire::error(code, None, false)
}

fn bad_args() -> p::Error {
    failure(Error::InvalidArgs)
}

fn capacity(reason: p::CapacityReason) -> p::Error {
    wire::error(Error::Capacity, Some(reason), false)
}

impl Application {
    pub fn new(build: Build) -> Self {
        Self {
            manager: Manager::default(),
            serial: Session::new(),
            build,
            candidates: Vec::new(),
            candidate_seq: 0,
            scan: None,
            scan_seq: 0,
            radio_scan: None,
            truncated: false,
            pair: None,
            pairing: None,
            pairing_dirty: false,
            scan_done: None,
            unpairing: Vec::new(),
            jobs: Vec::new(),
            dirty: Vec::new(),
            removed: VecDeque::new(),
            adapter_dirty: false,
            reported_ready: false,
            next_slot: 0,
            scan_turn: false,
            reboot_at: None,
        }
    }

    fn adapter_name(&self) -> &str {
        self.manager
            .preference
            .name
            .as_deref()
            .unwrap_or(self.build.default_adapter_name)
    }

    pub fn status<B: Bluetooth>(&self, radio: &B) -> p::Status {
        let supported = radio.capabilities();
        let enabled = self.manager.capabilities(radio);
        let mut info = Vec::new();
        let mut fact = |key: &str, value: p::value::Value| {
            info.push(p::Info {
                key: key.into(),
                value: Some(p::Value { value: Some(value) }),
            });
        };
        fact(
            p::keys::FIRMWARE_VERSION,
            p::value::Value::Text(self.build.version.into()),
        );
        fact(
            p::keys::BOARD_NAME,
            p::value::Value::Text(self.build.board.into()),
        );
        if self.development() {
            fact(p::keys::BUILD_DEVELOPMENT, p::value::Value::Bool(true));
        }
        if self.manager.storage_ready && self.manager.storage_full() {
            fact(p::keys::STORAGE_FULL, p::value::Value::Bool(true));
        }
        p::Status {
            id: self.build.adapter_id.clone(),
            name: self.adapter_name().into(),
            platform: wire::platform(self.manager.preference.host_platform) as i32,
            ready: self.manager.radio_ready && self.manager.storage_ready,
            transports: devices::Transports::ALL
                .into_iter()
                .filter(|t| supported.supports(*t))
                .map(|t| p::TransportSupport {
                    transport: wire::transport(t) as i32,
                    max_enabled: self
                        .manager
                        .storage_ready
                        .then(|| self.manager.max_enabled(t) as u32),
                    enabled: Some(enabled.supports(t)),
                })
                .collect(),
            info,
        }
    }

    fn development(&self) -> bool {
        cfg!(feature = "development") && self.build.development
    }

    fn mark(&mut self, slot: usize, bits: u8) {
        if self.dirty.len() <= slot {
            self.dirty.resize(slot + 1, 0);
        }
        self.dirty[slot] |= bits;
    }

    fn reply(&mut self, result: Option<R>) {
        self.serial.respond(p::Response { result });
    }

    fn fail(&mut self, error: p::Error) {
        self.serial.fail(error);
    }

    fn device_result(&mut self, slot: usize) {
        let device = wire::device(&self.manager, slot);
        self.reply(device.map(R::Device));
    }

    /// Saves first-connection setup progress for connected devices. A failed
    /// save leaves the remaining steps to the device's next connection.
    /// `write_uncertain` describes the last requested write, so these
    /// background saves leave it as they found it.
    async fn setup<S: RecordStore>(&mut self, store: &mut S) {
        if !self.manager.storage_ready {
            return;
        }
        while let Some((index, slot, policy, setup)) = self.manager.setup() {
            let uncertain = self.manager.write_uncertain;
            let saved = self.manager.policy(slot, policy, store).await;
            self.manager.write_uncertain = uncertain;
            if saved.is_ok() {
                self.manager.devices[slot].as_mut().unwrap().setup = setup;
                self.mark(slot, DEVICE);
            } else if let Some(c) = &mut self.manager.connections[index] {
                c.setup_failed = true;
            }
        }
    }

    fn find(&self, id: &str) -> Result<usize, p::Error> {
        self.manager
            .devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.policy.device_id().0 == id))
            .ok_or_else(|| failure(Error::NotFound))
    }

    /// Answers one request. The session reads the next request only after this response has been
    /// written.
    pub async fn dispatch<S: RecordStore, B: Bluetooth>(
        &mut self,
        request: p::Request,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let Some(command) = request.command else {
            return self.fail(failure(Error::UnknownCommand));
        };
        let development = self.development();
        match command {
            Command::GetStatus(_) => {
                if self.manager.storage_ready {
                    // A failed size read keeps the last estimate.
                    if let Ok(bytes) = store.available().await {
                        self.manager.available_bytes = bytes;
                    }
                }
                let status = self.status(radio);
                self.reply(Some(R::Status(status)));
            }
            Command::SetAdapter(args) => self.set_adapter(args, store, radio, now).await,
            Command::EnterBootloader(_) => self.bootloader(now),
            Command::StartScan(args) => self.start_scan(args, radio, now),
            Command::StopScan(_) => {
                self.stop_scan(radio);
                self.reply(None);
            }
            Command::StartPairing(args) => self.start_pairing(args, store, radio, now).await,
            Command::AcceptPrompt(args) => self.answer(true, Some(args.value), radio, now),
            Command::RejectPrompt(_) => self.answer(false, None, radio, now),
            Command::CancelPairing(_) => {
                self.stop_pairing(Error::Cancelled, radio);
                self.reply(None);
            }
            Command::ListDevices(_) => {
                let devices = (0..self.manager.devices.len())
                    .filter_map(|slot| wire::device(&self.manager, slot))
                    .collect();
                self.reply(Some(R::Devices(p::DeviceList { devices })));
            }
            Command::GetDevice(args) => match self.find(&args.device) {
                Ok(slot) => self.device_result(slot),
                Err(e) => self.fail(e),
            },
            Command::SetDevice(args) => self.set_device(args, store, radio).await,
            Command::ConnectDevice(args) => self.connect(&args.device, store, radio, now).await,
            Command::DisconnectDevice(args) => match self.find(&args.device) {
                Ok(slot) => {
                    self.manager.disconnect(slot, radio).ok();
                    self.mark(slot, DEVICE);
                    self.device_result(slot);
                }
                Err(e) => self.fail(e),
            },
            Command::UnpairDevice(args) => self.unpair(&args.device, store, radio).await,
            Command::RefreshDevice(args) => self.refresh(&args.device, radio, now),
            Command::ListWarnings(args) => match self.find(&args.device) {
                Ok(slot) => {
                    let warnings = wire::warnings(&self.manager, slot);
                    self.reply(warnings.map(R::Warnings));
                }
                Err(e) => self.fail(e),
            },
            Command::ListSettings(args) => match self.find(&args.device) {
                Ok(slot) => {
                    let settings = wire::settings(&self.manager, slot);
                    self.reply(settings.map(R::Settings));
                }
                Err(e) => self.fail(e),
            },
            Command::SetSettings(args) => {
                let changes = args
                    .changes
                    .iter()
                    .map(|c| (c.integration, c.key.as_str(), Some(c.value.as_ref())))
                    .collect();
                self.change_settings(&args.device, changes, store).await
            }
            Command::ForgetSettings(args) => {
                let changes = args
                    .settings
                    .iter()
                    .map(|s| (s.integration, s.key.as_str(), None))
                    .collect();
                self.change_settings(&args.device, changes, store).await
            }
            Command::ListFeatures(args) if development => match self.find(&args.device) {
                Ok(slot) => {
                    let features = self.manager.devices[slot]
                        .as_ref()
                        .unwrap()
                        .catalog
                        .features()
                        .iter()
                        .enumerate()
                        .map(|(index, f)| p::Feature {
                            integration: p::IntegrationKind::Hidpp as i32,
                            supported: f.supported(),
                            detail: Some(p::feature::Detail::Hidpp(p::HidppFeature {
                                index: index as u32,
                                id: f.id.0.into(),
                                version: f.version.0.into(),
                                flags: f.flags.0.into(),
                            })),
                        })
                        .collect();
                    self.reply(Some(R::Features(p::FeatureList { features })));
                }
                Err(e) => self.fail(e),
            },
            Command::ListFiles(args) if development => self.list_files(&args.path, store).await,
            Command::ReadFile(args) if development => self.read_file(&args.path, store).await,
            Command::ListFeatures(_) | Command::ListFiles(_) | Command::ReadFile(_) => {
                self.fail(failure(Error::UnknownCommand))
            }
        }
    }

    async fn set_adapter<S: RecordStore, B: Bluetooth>(
        &mut self,
        args: p::SetAdapter,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let name = match args.name.as_deref() {
            None => self.manager.preference.name.clone(),
            Some("") => None,
            Some(value) => match crate::model::adapter_name(value) {
                Some(name) => Some(name.into()),
                None => return self.fail(bad_args()),
            },
        };
        let platform = match args.platform.map(p::Platform::try_from) {
            None => self.manager.preference.host_platform,
            Some(Ok(platform)) => wire::host_platform(platform),
            Some(Err(_)) => return self.fail(bad_args()),
        };
        let mut transports = self.manager.preference.transports;
        for update in &args.transports {
            let transport = match p::Transport::try_from(update.transport) {
                Ok(p::Transport::Classic) => Transport::Classic,
                Ok(p::Transport::Ble) => Transport::Ble,
                Ok(p::Transport::Unspecified) => return self.fail(bad_args()),
                Err(_) => return self.fail(failure(Error::UnsupportedTransport)),
            };
            if !radio.capabilities().supports(transport) {
                return self.fail(failure(Error::UnsupportedTransport));
            }
            if let Some(enabled) = update.enabled {
                transports.set(transport, enabled);
            }
        }
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        let platform_changed = platform != self.manager.preference.host_platform;
        let changed: Vec<Transport> = devices::Transports::ALL
            .into_iter()
            .filter(|t| transports.contains(*t) != self.manager.preference.transports.contains(*t))
            .collect();
        if name != self.manager.preference.name || platform_changed || !changed.is_empty() {
            let preference = devices::AdapterPreference {
                name,
                host_platform: platform,
                transports,
            };
            if let Err(code) = self.manager.adapter(preference, store).await {
                let uncertain = self.manager.write_uncertain;
                return self.fail(wire::error(code, None, uncertain));
            }
            self.adapter_dirty = true;
            if platform_changed {
                for slot in 0..self.manager.devices.len() {
                    if self.manager.devices[slot].as_ref().is_some_and(|d| {
                        d.state == ConnectionState::Connected && d.policy.hidpp_enabled
                    }) {
                        self.mark(slot, DEVICE | SETTINGS);
                    }
                }
            }
            if !changed.is_empty()
                && let Err(code) = self.apply_transports(&changed, store, radio, now).await
            {
                return self.fail(failure(code));
            }
        }
        let status = self.status(radio);
        self.reply(Some(R::Status(status)));
    }

    /// Applies saved changes to the enabled transports, all supported by the
    /// radio. Disabling a transport closes its links, ends a pairing over it as
    /// an unsupported transport would, and drops it from a running scan, ending
    /// the scan when nothing is left. Saved devices of a disabled transport
    /// become inactive, and eligible again once it is enabled.
    async fn apply_transports<S: RecordStore, B: Bluetooth>(
        &mut self,
        changed: &[Transport],
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> Result<(), Error> {
        let enabled = self.manager.preference.transports;
        for &transport in changed.iter().filter(|t| !enabled.contains(**t)) {
            if self
                .pair
                .as_ref()
                .is_some_and(|p| p.address.transport == transport)
            {
                self.stop_pairing(Error::UnsupportedTransport, radio);
            }
            let links: Vec<LinkId> = self
                .manager
                .connections
                .iter()
                .flatten()
                .filter(|c| c.peer.transport == transport)
                .map(|c| c.id)
                .collect();
            for link in links {
                self.close(link, None, radio);
            }
            if let Some(scan) = &mut self.scan {
                match transport {
                    Transport::Classic => scan.classic = false,
                    Transport::Ble => scan.ble = false,
                }
                if !scan.classic && !scan.ble {
                    self.stop_scan(radio);
                }
            }
        }
        self.manager.refresh_enabled();
        for slot in 0..self.manager.devices.len() {
            if self.manager.devices[slot]
                .as_ref()
                .is_some_and(|d| changed.contains(&d.policy.peer.transport))
            {
                self.mark(slot, DEVICE);
            }
        }
        for &transport in changed {
            if let Err(error) = radio.set_transport(transport, enabled.contains(transport)) {
                self.event(Event::Failed(error), store, radio, now).await;
                return Ok(());
            }
        }
        // A pairing syncs once it ends. A radio failure here is retried at the next sync; a
        // storage read failure fails the command, with the saved change kept.
        if self.pair.is_some() {
            return Ok(());
        }
        match self.manager.sync_bonds(store, radio).await {
            Err(Error::StorageFailed) => Err(Error::StorageFailed),
            _ => Ok(()),
        }
    }

    fn bootloader(&mut self, now: u64) {
        if !self.development() || self.build.bootloader.is_none() {
            return self.fail(failure(Error::UnknownCommand));
        }
        if self.pair.is_some()
            || self
                .manager
                .connections
                .iter()
                .flatten()
                .any(|c| c.device.is_none())
        {
            return self.fail(failure(Error::Busy));
        }
        self.reply(None);
        self.serial.stop_commands();
        for source in 0..devices::ACTIVE_CONNECTIONS {
            self.manager.forward.remove(source);
        }
        self.reboot_at = Some(now.saturating_add(250));
    }

    fn start_scan<B: Bluetooth>(&mut self, args: p::StartScan, radio: &mut B, now: u64) {
        if args.transports.is_empty() {
            return self.fail(bad_args());
        }
        let seconds = match args.seconds {
            0 => SCAN_DEFAULT_SECONDS,
            s if s <= SCAN_MAX_SECONDS => s,
            _ => return self.fail(bad_args()),
        };
        // Requested transports that are unsupported or disabled are left out.
        let caps = self.manager.capabilities(radio);
        let (mut classic, mut ble) = (false, false);
        for transport in &args.transports {
            match p::Transport::try_from(*transport) {
                Ok(p::Transport::Classic) => classic |= caps.classic,
                Ok(p::Transport::Ble) => ble |= caps.ble,
                _ => {}
            }
        }
        if !classic && !ble {
            return self.fail(failure(Error::UnsupportedTransport));
        }
        if !self.manager.radio_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        let Some(token) = self.scan_seq.checked_add(1) else {
            return self.fail(failure(Error::InternalError));
        };
        if let Err(e) = radio.scan(token, classic, ble) {
            return self.fail(failure(e));
        }
        self.scan_seq = token;
        self.radio_scan = Some((token, classic, ble));
        // A new scan invalidates earlier candidates, except one a pairing already captured.
        let pairing = self.pair.as_ref().map(|p| p.candidate.clone());
        self.candidates.retain(|c| Some(&c.id) == pairing.as_ref());
        self.truncated = false;
        self.scan_done = None;
        self.scan = Some(Scan {
            token,
            classic,
            ble,
            deadline: now.saturating_add(u64::from(seconds) * 1000),
        });
        self.reply(None);
    }

    fn stop_scan<B: Bluetooth>(&mut self, radio: &mut B) {
        if let Some(scan) = self.scan.take() {
            let _ = radio.scan(scan.token, false, false);
            self.radio_scan = None;
            self.scan_done = Some(p::ScanDone {
                count: self.candidates.len() as u32,
                truncated: self.truncated,
            });
        }
    }

    async fn start_pairing<S: RecordStore, B: Bluetooth>(
        &mut self,
        args: p::StartPairing,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        if self.pair.is_some() {
            return self.fail(failure(Error::Busy));
        }
        let Some(c) = self.candidates.iter().find(|c| c.id == args.candidate) else {
            return self.fail(failure(Error::NotFound));
        };
        let (peer, address, name) = (c.peer, c.address, c.name.clone());
        if !self.manager.storage_ready || !self.manager.radio_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        if !self.manager.capabilities(radio).supports(peer.transport)
            || radio.bond_capacity(peer.transport) == 0
        {
            return self.fail(failure(Error::UnsupportedTransport));
        }
        let slot = self.manager.peer(peer);
        if let Some(slot) = slot
            && self.manager.devices[slot].as_ref().unwrap().policy.blocked
        {
            return self.fail(failure(Error::Blocked));
        }
        match store.available().await {
            Ok(bytes) => self.manager.available_bytes = bytes,
            Err(_) => return self.fail(failure(Error::StorageFailed)),
        }
        if self.manager.storage_full() {
            self.adapter_dirty = true;
            return self.fail(capacity(p::CapacityReason::Storage));
        }
        // The selected device's link and background setup can free a slot. Keep
        // unrelated established input links intact.
        let closing: Vec<LinkId> = self
            .manager
            .connections
            .iter()
            .flatten()
            .filter(|c| (slot.is_some() && c.device == slot) || c.runtime.is_none())
            .map(|c| c.id)
            .collect();
        if self.manager.connections.iter().all(Option::is_some) && closing.is_empty() {
            return self.fail(capacity(p::CapacityReason::Connections));
        }
        for link in closing {
            self.close(link, None, radio);
        }
        self.pair = Some(Pair {
            candidate: args.candidate.clone(),
            link: None,
            address,
            expected: slot.map(|_| peer),
            cleanup: None,
            deadline: now.saturating_add(PAIR_TIMEOUT_MS),
            name,
            cancelling: None,
            prompt: None,
            quiet: false,
        });
        self.set_pairing(
            &args.candidate,
            p::pairing::Step::Connecting(p::PairingConnecting {}),
        );
        self.reply(None);
    }

    /// Reports how a finished attempt ended, unless its session has ended.
    fn pairing_ended(&mut self, pair: &Pair, step: p::pairing::Step) {
        if !pair.quiet {
            self.set_pairing(&pair.candidate, step);
        }
    }

    fn set_pairing(&mut self, candidate: &str, step: p::pairing::Step) {
        if self.pair.as_ref().is_some_and(|p| p.quiet) {
            return;
        }
        self.pairing = Some(p::Pairing {
            candidate: candidate.into(),
            step: Some(step),
        });
        self.pairing_dirty = true;
    }

    fn answer<B: Bluetooth>(
        &mut self,
        accept: bool,
        value: Option<String>,
        radio: &mut B,
        now: u64,
    ) {
        let Some(pair) = &mut self.pair else {
            return self.fail(failure(Error::StalePrompt));
        };
        let (Some(link), None, Some(prompt)) = (pair.link, pair.cancelling, &mut pair.prompt)
        else {
            return self.fail(failure(Error::StalePrompt));
        };
        if prompt.answered || prompt.method.display() || now >= prompt.deadline {
            return self.fail(failure(Error::StalePrompt));
        }
        let value = value.filter(|v| !v.is_empty());
        let action = if accept {
            crate::model::link::PairAction::Accept
        } else {
            crate::model::link::PairAction::Reject
        };
        let entry = matches!(
            prompt.method,
            PromptMethod::EnterPasskey | PromptMethod::EnterPin
        );
        if !prompt.method.valid_reply(
            action,
            if entry && accept {
                value.as_deref()
            } else {
                None
            },
        ) {
            return self.fail(bad_args());
        }
        prompt.answered = true;
        let method = prompt.method;
        self.reply(None);
        let result = radio.pair_reply(link, method, accept, value.as_deref());
        if let Err(e) = result {
            self.stop_pairing(e, radio);
        } else if !accept {
            self.stop_pairing(Error::AuthenticationRejected, radio);
        }
    }

    /// Ends the pairing attempt that has not saved its bond, closing its link.
    fn stop_pairing<B: Bluetooth>(&mut self, reason: Error, radio: &mut B) {
        let Some(pair) = &mut self.pair else { return };
        if pair.cancelling.is_some() {
            return;
        }
        pair.cancelling = Some(reason);
        pair.prompt = None;
        if let Some(link) = pair.link {
            // A user cancellation is a request result, not a link/profile failure.
            self.close(link, (reason != Error::Cancelled).then_some(reason), radio);
        }
    }

    async fn set_device<S: RecordStore, B: Bluetooth>(
        &mut self,
        args: p::SetDevice,
        store: &mut S,
        radio: &mut B,
    ) {
        let slot = match self.find(&args.device) {
            Ok(slot) => slot,
            Err(e) => return self.fail(e),
        };
        let d = self.manager.devices[slot].as_ref().unwrap();
        let mut policy = d.policy.clone();
        let mut setup = d.setup;
        let mut seen = Vec::new();
        for update in &args.integrations {
            if seen.contains(&update.kind) {
                return self.fail(bad_args());
            }
            seen.push(update.kind);
            if update.kind != p::IntegrationKind::Hidpp as i32 {
                return self.fail(failure(Error::UnsupportedTransport));
            }
            if let Some(enabled) = update.enabled {
                // A user choice settles setup's HID++ detection.
                policy.hidpp_enabled = enabled;
                setup.hidpp = true;
                policy.setup_pending &= !setup.complete();
            }
        }
        if let Some(enabled) = args.enabled {
            policy.enabled = enabled;
        }
        if let Some(trusted) = args.trusted {
            policy.trusted = trusted;
        }
        if let Some(blocked) = args.blocked {
            policy.blocked = blocked;
        }
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        if self.pair.is_some() && policy != d.policy {
            // An unresolved pairing may still reveal this identity after native
            // bonding; its storage reservation must not be spent meanwhile.
            return self.fail(failure(Error::Busy));
        }
        if let Err(code) = self.manager.policy(slot, policy, store).await {
            if code == Error::NotFound {
                // The device's record was lost; it is deleted as at startup.
                let _ = self.manager.lose(slot, store, radio).await;
            }
            let error = if code == Error::Capacity {
                capacity(p::CapacityReason::Enabled)
            } else {
                wire::error(code, None, self.manager.write_uncertain)
            };
            return self.fail(error);
        }
        self.manager.devices[slot].as_mut().unwrap().setup = setup;
        let d = self.manager.devices[slot].as_ref().unwrap();
        if (d.policy.blocked || !d.policy.enabled)
            && let Some(link) = self.manager.link_for(slot)
        {
            self.close(link, None, radio);
        }
        // A radio failure here is retried at the next sync; a storage read failure fails the
        // command, with the saved change kept.
        let synced = self.manager.sync_bonds(store, radio).await;
        if self.manager.devices.get(slot).is_some_and(Option::is_some) {
            self.mark(slot, DEVICE | SETTINGS);
        }
        match synced {
            Err(Error::StorageFailed) => self.fail(failure(Error::StorageFailed)),
            _ if self.manager.devices.get(slot).is_some_and(Option::is_some) => {
                self.device_result(slot)
            }
            _ => self.fail(failure(Error::NotFound)),
        }
    }

    async fn connect<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: &str,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let slot = match self.find(id) {
            Ok(slot) => slot,
            Err(e) => return self.fail(e),
        };
        if !self.manager.storage_ready || !self.manager.radio_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        if self.pair.is_some() {
            return self.fail(failure(Error::Busy));
        }
        let d = self.manager.devices[slot].as_ref().unwrap();
        let layout = if d.state == ConnectionState::Disconnected {
            crate::layouts::load(store, d.policy.id, d.policy.peer.transport).await
        } else {
            None
        };
        match self.manager.connect(
            slot,
            true,
            now.saturating_add(CONNECT_TIMEOUT_MS),
            layout.as_ref(),
            radio,
        ) {
            Ok(_) => {
                self.mark(slot, DEVICE);
                self.device_result(slot);
            }
            Err(Error::Capacity) => {
                let effective = self.manager.devices[slot]
                    .as_ref()
                    .unwrap()
                    .effective_enabled;
                self.fail(capacity(if effective {
                    p::CapacityReason::Connections
                } else {
                    p::CapacityReason::Enabled
                }));
            }
            Err(e) => self.fail(failure(e)),
        }
    }

    async fn unpair<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: &str,
        store: &mut S,
        radio: &mut B,
    ) {
        let slot = match self.find(id) {
            Ok(slot) => slot,
            Err(e) => return self.fail(e),
        };
        if self.pair.is_some() {
            return self.fail(failure(Error::Busy));
        }
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        self.manager.disconnect(slot, radio).ok();
        if self.manager.link_for(slot).is_some() {
            let id = self.manager.devices[slot].as_ref().unwrap().policy.id;
            if !self.unpairing.contains(&id) {
                self.unpairing.push(id);
            }
            self.mark(slot, DEVICE);
            return self.reply(None);
        }
        match self.remove(slot, store, radio).await {
            Ok(()) => self.reply(None),
            Err(code) => {
                let uncertain = self.manager.write_uncertain;
                self.fail(wire::error(code, None, uncertain));
            }
        }
    }

    async fn remove<S: RecordStore, B: Bluetooth>(
        &mut self,
        slot: usize,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        let id = self.manager.devices[slot]
            .as_ref()
            .ok_or(Error::NotFound)?
            .policy
            .device_id();
        self.manager.unpair(slot, store, radio).await?;
        if let Some(bits) = self.dirty.get_mut(slot) {
            *bits = 0;
        }
        self.removed.push_back(id.0);
        self.adapter_dirty = true;
        Ok(())
    }

    fn refresh<B: Bluetooth>(&mut self, id: &str, radio: &mut B, now: u64) {
        let slot = match self.find(id) {
            Ok(slot) => slot,
            Err(e) => return self.fail(e),
        };
        let Some(link) = self.manager.link_for(slot).filter(|_| {
            self.manager.devices[slot].as_ref().unwrap().state == ConnectionState::Connected
        }) else {
            return self.fail(failure(Error::NotConnected));
        };
        radio.refresh_info(link).ok();
        let id = self.manager.devices[slot].as_ref().unwrap().policy.id;
        let job = self.job(id);
        job.information = true;
        job.read = true;
        self.start_jobs(now);
        self.reply(None);
    }

    fn job(&mut self, id: u64) -> &mut Jobs {
        let index = match self.jobs.iter().position(|(d, _)| *d == id) {
            Some(i) => i,
            None => {
                self.jobs.push((id, Jobs::default()));
                self.jobs.len() - 1
            }
        };
        &mut self.jobs[index].1
    }

    /// Starts waiting settings work on devices whose settings engine is idle. Work for a device that
    /// is not connected waits for its next connection, which applies saved settings anyway.
    fn start_jobs(&mut self, now: u64) {
        let mut i = 0;
        while i < self.jobs.len() {
            let id = self.jobs[i].0;
            let Some(slot) = self
                .manager
                .devices
                .iter()
                .position(|d| d.as_ref().is_some_and(|d| d.policy.id == id))
            else {
                self.jobs.swap_remove(i);
                continue;
            };
            let Some(link) = self.manager.link_for(slot) else {
                self.jobs.swap_remove(i);
                continue;
            };
            let c = self.manager.connections[link.slot as usize]
                .as_mut()
                .unwrap();
            let d = self.manager.devices[slot].as_mut().unwrap();
            let Some(runtime) = c.runtime.as_mut().filter(|_| !c.closing) else {
                i += 1;
                continue;
            };
            if runtime.busy() {
                i += 1;
                continue;
            }
            let jobs = &mut self.jobs[i].1;
            if jobs.information {
                if d.policy.hidpp_enabled {
                    runtime.start_information(&mut d.catalog, now).ok();
                }
                jobs.information = false;
            }
            if !runtime.busy() {
                if jobs.read {
                    runtime
                        .start_settings(&mut d.catalog, false, None, false, now)
                        .ok();
                    jobs.read = false;
                } else if jobs.apply {
                    if d.policy.hidpp_enabled {
                        runtime
                            .start_settings(&mut d.catalog, true, None, false, now)
                            .ok();
                    }
                    jobs.apply = false;
                }
            }
            if !jobs.information && !jobs.read && !jobs.apply {
                self.jobs.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    async fn change_settings<S: RecordStore>(
        &mut self,
        id: &str,
        changes: Vec<(i32, &str, Option<Option<&p::Value>>)>,
        store: &mut S,
    ) {
        let slot = match self.find(id) {
            Ok(slot) => slot,
            Err(e) => return self.fail(e),
        };
        if changes.is_empty() {
            return self.fail(bad_args());
        }
        let mut parsed = Vec::new();
        for (integration, key, value) in changes {
            if integration != p::IntegrationKind::Hidpp as i32 {
                return self.fail(failure(Error::NotFound));
            }
            let Some(key) = wire::parse_setting_key(key) else {
                return self.fail(failure(Error::NotFound));
            };
            if parsed.iter().any(|c: &Change| c.key == key) {
                return self.fail(bad_args());
            }
            let value = match value {
                None => None,
                Some(value) => match value.and_then(|v| wire::setting_value(key.kind(), v)) {
                    Some(value) => Some(value),
                    None => return self.fail(bad_args()),
                },
            };
            parsed.push(Change { key, value });
        }
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        let d = self.manager.devices[slot].as_mut().unwrap();
        let mut prefs = Preferences {
            store,
            device: d.policy.id,
        };
        if let Err(error) = d.catalog.change(&parsed, &mut prefs).await {
            if error == crate::settings::Error::StorageUnknown {
                self.manager.storage_ready = false;
                self.adapter_dirty = true;
            }
            return self.fail(wire::error(
                error.code(),
                None,
                error == crate::settings::Error::StorageUnknown,
            ));
        }
        let id = d.policy.id;
        self.mark(slot, SETTINGS);
        self.job(id).apply = true;
        let settings = wire::settings(&self.manager, slot);
        self.reply(settings.map(R::Settings));
    }

    async fn list_files<S: RecordStore>(&mut self, path: &str, store: &mut S) {
        if !crate::model::storage_path(path) {
            return self.fail(bad_args());
        }
        let mut entries = Vec::new();
        loop {
            match store.file_entry(path, entries.len()).await {
                Ok(Some(entry)) => entries.push(p::FileEntry {
                    name: entry.name,
                    directory: entry.kind == crate::storage::FileType::Directory,
                    size: entry.size as u64,
                }),
                Ok(None) => break,
                Err(crate::storage::Error::Missing) => return self.fail(failure(Error::NotFound)),
                Err(_) => return self.fail(failure(Error::StorageFailed)),
            }
        }
        self.reply(Some(R::Files(p::FileList { entries })));
    }

    async fn read_file<S: RecordStore>(&mut self, path: &str, store: &mut S) {
        if !crate::model::storage_path(path) {
            return self.fail(bad_args());
        }
        let mut data = Vec::new();
        let mut chunk = [0; 512];
        loop {
            match store.file_read(path, data.len() as u32, &mut chunk).await {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&chunk[..n]),
                Err(crate::storage::Error::Missing) => return self.fail(failure(Error::NotFound)),
                Err(_) => return self.fail(failure(Error::StorageFailed)),
            }
        }
        self.reply(Some(R::File(p::FileData { data })));
    }

    fn close<B: Bluetooth>(&mut self, link: LinkId, error: Option<Error>, radio: &mut B) {
        let slot = self.manager.connection(link).and_then(|c| c.device);
        self.manager.close(link, error, radio);
        if let Some(slot) = slot {
            self.mark(slot, DEVICE | SETTINGS);
        }
    }

    fn end_session<B: Bluetooth>(&mut self, radio: &mut B) {
        self.stop_scan(radio);
        self.stop_pairing(Error::Cancelled, radio);
        if let Some(pair) = &mut self.pair {
            pair.quiet = true;
        }
        self.candidates = Vec::new();
    }

    /// Follows the serial port opening and closing. Ending a session stops a running scan and
    /// cancels a pairing that has not saved its bond; a new session starts with nothing to report.
    pub fn session<B: Bluetooth>(&mut self, active: bool, radio: &mut B) {
        if self.serial.session(active) {
            self.end_session(radio);
        }
        self.dirty.clear();
        self.removed.clear();
        self.adapter_dirty = false;
        self.pairing_dirty = false;
        self.pairing = None;
        self.scan_done = None;
    }
}

impl Application {
    /// Backends deliver one owned event at a time, after returning from vendor callbacks.
    pub async fn event<S: RecordStore, B: Bluetooth>(
        &mut self,
        event: Event,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        if self.reboot_at.is_some() {
            return;
        }
        match event {
            Event::Ready => {
                self.manager.radio_ready = true;
                // A change the backend could not apply before it restarted is applied now.
                let ready = if self.manager.storage_ready {
                    match self.manager.apply_transports(radio) {
                        Ok(()) => self.manager.sync_bonds(store, radio).await,
                        Err(error) => Err(error),
                    }
                } else {
                    self.manager.load(store, radio).await
                };
                match ready {
                    Ok(()) => {}
                    Err(Error::StorageFailed) => self.manager.storage_ready = false,
                    Err(_) => self.manager.radio_ready = false,
                }
                self.adapter_dirty = true;
            }
            Event::Failed(error) | Event::Restarting(error) => {
                if self.scan.take().is_some() {
                    self.radio_scan = None;
                    self.scan_done = Some(p::ScanDone {
                        count: self.candidates.len() as u32,
                        truncated: self.truncated,
                    });
                }
                for slot in 0..devices::ACTIVE_CONNECTIONS {
                    if let Some(id) = self.manager.connections[slot].as_ref().map(|c| c.id) {
                        self.dropped(id, Some(error), now);
                    }
                }
                self.manager.radio_ready = false;
                if error == Error::StorageFailed {
                    self.manager.storage_ready = false;
                }
                self.adapter_dirty = true;
            }
            Event::Found {
                scan,
                peer,
                address,
                connectable,
                kind,
                name,
                rssi,
            } => {
                if connectable
                    && self.radio_scan.is_some_and(|(token, _, _)| token == scan)
                    && let Some(slot) = self.manager.peer(peer)
                {
                    self.manager.devices[slot].as_mut().unwrap().seen(now);
                }
                let Some(address) = address else { return };
                let scanning = self.scan.as_ref().is_some_and(|s| {
                    s.token == scan
                        && if peer.transport == Transport::Classic {
                            s.classic
                        } else {
                            s.ble
                        }
                });
                if !scanning {
                    return;
                }
                let name = devices::display_name(name.as_bytes());
                let rssi = rssi.filter(|r| (-127..=20).contains(r)).map(i32::from);
                if let Some(c) = self.candidates.iter_mut().find(|c| c.peer == peer) {
                    c.address = address;
                    if !name.is_empty() && c.name != name {
                        c.name = name;
                        c.dirty = true;
                    }
                    if kind != DeviceKind::Unknown && c.kind != kind {
                        c.kind = kind;
                        c.dirty = true;
                    }
                    if c.rssi != rssi {
                        c.rssi = rssi;
                        c.dirty = true;
                    }
                } else if self.candidates.len() == devices::SCAN_CANDIDATES {
                    self.truncated = true;
                } else {
                    let Some(seq) = self.candidate_seq.checked_add(1) else {
                        self.truncated = true;
                        return;
                    };
                    self.candidate_seq = seq;
                    self.candidates.push(Candidate {
                        id: format!("c_{seq}"),
                        peer,
                        address,
                        kind,
                        name,
                        rssi,
                        dirty: true,
                    });
                }
            }
            Event::Incoming { attempt, peer } => {
                if self.pair.is_some() {
                    let _ = radio.incoming(attempt, None, None);
                    return;
                }
                let layout = match self.manager.admits(peer, now) {
                    Some(slot) => {
                        let id = self.manager.devices[slot].as_ref().unwrap().policy.id;
                        crate::layouts::load(store, id, peer.transport).await
                    }
                    None => None,
                };
                if let Ok(Some(slot)) =
                    self.manager
                        .incoming(attempt, peer, now, layout.as_ref(), radio)
                {
                    self.mark(slot, DEVICE);
                }
            }
            Event::Prompt {
                link,
                method,
                value,
            } => {
                let current = self
                    .pair
                    .as_ref()
                    .is_some_and(|p| p.link == Some(link) && p.cancelling.is_none());
                if !current {
                    self.manager
                        .close(link, Some(Error::AuthenticationFailed), radio);
                    return;
                }
                let pair = self.pair.as_ref().unwrap();
                let numeric = matches!(
                    method,
                    PromptMethod::ConfirmPasskey | PromptMethod::DisplayPasskey
                );
                let valid = if numeric {
                    value
                        .as_ref()
                        .is_some_and(|v| v.len() == 6 && v.bytes().all(|b| b.is_ascii_digit()))
                } else {
                    !value.as_ref().is_some_and(|v| {
                        v.len() > 16 || !v.bytes().all(|b| (0x20..=0x7e).contains(&b))
                    })
                };
                if now >= pair.deadline
                    || pair.prompt.as_ref().is_some_and(|p| !p.answered)
                    || !valid
                {
                    self.stop_pairing(Error::AuthenticationFailed, radio);
                    return;
                }
                let deadline = pair.deadline.min(now.saturating_add(PROMPT_TIMEOUT_MS));
                let candidate = pair.candidate.clone();
                let step = match method {
                    PromptMethod::EnterPasskey | PromptMethod::EnterPin => {
                        p::pairing::Step::EnterCode(p::EnterCode {
                            kind: if method == PromptMethod::EnterPin {
                                p::CodeKind::Pin
                            } else {
                                p::CodeKind::Passkey
                            } as i32,
                        })
                    }
                    PromptMethod::ConfirmPasskey => p::pairing::Step::ConfirmCode(p::ConfirmCode {
                        passkey: value.as_deref().unwrap_or_default().into(),
                    }),
                    PromptMethod::DisplayPasskey | PromptMethod::DisplayPin => {
                        p::pairing::Step::ShowCode(p::ShowCode {
                            kind: if method == PromptMethod::DisplayPin {
                                p::CodeKind::Pin
                            } else {
                                p::CodeKind::Passkey
                            } as i32,
                            value: value.as_deref().unwrap_or_default().into(),
                        })
                    }
                };
                self.pair.as_mut().unwrap().prompt = Some(Prompt {
                    method,
                    deadline,
                    answered: method.display(),
                });
                self.set_pairing(&candidate, step);
            }
            Event::Bonded { link, identity } => {
                self.bonded(link, identity, store, radio, now).await
            }
            Event::Connected {
                link,
                descriptors,
                max_output,
                layout,
            } => match self.manager.connected(link, descriptors, max_output, now) {
                Ok(Some(slot)) => {
                    self.mark(slot, DEVICE | SETTINGS | WARNINGS);
                    if let Some(layout) = layout {
                        self.manager.connection_mut(link).unwrap().maps =
                            Some(crate::layouts::maps(&layout));
                        self.save_layout(slot, &layout, store).await;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    // The backend could not use the saved layout.
                    if layout.is_some()
                        && let Some(slot) = self.manager.connection(link).and_then(|c| c.device)
                    {
                        self.remove_layout(slot, store).await;
                    }
                    self.close(link, Some(e), radio);
                }
            },
            Event::Layout {
                link,
                descriptors,
                layout,
            } => {
                let maps = crate::layouts::maps(&layout);
                let Some((slot, unchanged)) = self
                    .manager
                    .connection(link)
                    .filter(|c| !c.closing && c.runtime.is_some())
                    .and_then(|c| Some((c.device?, c.maps == Some(maps))))
                else {
                    return;
                };
                // Changed report characteristics alone leave the parsed maps as they are.
                if unchanged {
                    return self.save_layout(slot, &layout, store).await;
                }
                match self.manager.relayout(link, descriptors) {
                    Ok(Some(slot)) => {
                        self.manager.connection_mut(link).unwrap().maps = Some(maps);
                        self.mark(slot, DEVICE | SETTINGS | WARNINGS);
                        self.save_layout(slot, &layout, store).await;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.remove_layout(slot, store).await;
                        self.close(link, Some(e), radio);
                    }
                }
            }
            Event::Security { link, security } => {
                if let Some(slot) = self.manager.security(link, security) {
                    self.mark(slot, DEVICE);
                }
            }
            Event::Disconnected { link, error } => {
                let ended = self
                    .manager
                    .connection(link)
                    .and_then(|c| Some((c.device?, c.error.or(error))));
                self.dropped(link, error, now);
                // A device whose HID layout could not be used is discovered again next time.
                if let Some((slot, Some(Error::UnsupportedHid))) = ended {
                    self.remove_layout(slot, store).await;
                }
                // A failed sync is retried at the next one.
                if self.pair.is_none() && self.manager.storage_ready {
                    let _ = self.manager.sync_bonds(store, radio).await;
                }
            }
            Event::Input(report) => {
                let slot = self.manager.connection(report.link).and_then(|c| c.device);
                match self.manager.input(&report, now) {
                    Ok(true) => {
                        if let Some(slot) = slot {
                            self.mark(slot, DEVICE);
                        }
                    }
                    Err(e) => self.close(report.link, Some(e), radio),
                    _ => {}
                }
            }
            Event::Written { id, result } => match self.manager.written(id, result, now) {
                Ok(Some(slot)) => self.mark(slot, DEVICE),
                Err(e) => self.close(id.link, Some(e), radio),
                _ => {}
            },
            Event::Information {
                link,
                uuid,
                instance,
                success,
                bytes,
            } => {
                if let Some(slot) = self
                    .manager
                    .connection(link)
                    .filter(|c| !c.closing && c.runtime.is_some())
                    .and_then(|c| c.device)
                {
                    let info = &mut self.manager.devices[slot].as_mut().unwrap().catalog.info;
                    if success {
                        crate::info::standard(info, uuid, instance, &bytes);
                    } else {
                        crate::info::standard_failed(info, uuid, instance);
                    }
                }
            }
            Event::Read {
                id,
                report_type,
                result,
            } => {
                if let Some(c) = self
                    .manager
                    .connections
                    .get_mut(id.link.slot as usize)
                    .and_then(Option::as_mut)
                    .filter(|c| c.id == id.link && !c.closing)
                    && let Some(slot) = c.device
                    && let Some(link) = &mut c.runtime
                {
                    link.report_read_complete(
                        id,
                        report_type,
                        result.as_ref().map_err(|e| *e),
                        &mut self.manager.devices[slot].as_mut().unwrap().catalog,
                        now,
                    );
                }
            }
        }
    }

    /// A failed save leaves the next connection to discover the device again.
    async fn save_layout<S: RecordStore>(
        &mut self,
        slot: usize,
        layout: &crate::bluetooth::Layout,
        store: &mut S,
    ) {
        if !self.manager.storage_ready {
            return;
        }
        let policy = &self.manager.devices[slot].as_ref().unwrap().policy;
        let (id, transport) = (policy.id, policy.peer.transport);
        let full = self.manager.storage_full();
        let saved = crate::layouts::save(store, id, transport, layout).await;
        self.refresh_available(store).await;
        // Whole-block allocation can take more than the file's size. A layout never takes
        // the room kept for pairing another device.
        if saved && self.manager.storage_full() {
            crate::layouts::remove(store, id).await;
            self.refresh_available(store).await;
        }
        self.adapter_dirty |= full != self.manager.storage_full();
    }

    /// A failed read keeps the last estimate.
    async fn refresh_available<S: RecordStore>(&mut self, store: &mut S) {
        if let Ok(bytes) = store.available().await {
            self.manager.available_bytes = bytes;
        }
    }

    /// Removes a saved layout that no longer describes the device.
    async fn remove_layout<S: RecordStore>(&mut self, slot: usize, store: &mut S) {
        if let Some(d) = self
            .manager
            .devices
            .get(slot)
            .and_then(Option::as_ref)
            .filter(|_| self.manager.storage_ready)
        {
            crate::layouts::remove(store, d.policy.id).await;
        }
    }

    async fn bonded<S: RecordStore, B: Bluetooth>(
        &mut self,
        link: LinkId,
        identity: Peer,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        // Stale callbacks cannot delete a newer connection's native bond.
        // Backends finish rejected-pair cleanup before Disconnected.
        if self
            .manager
            .connection(link)
            .is_none_or(|c| c.device.is_some())
        {
            return;
        }
        let current = self.pair.as_ref().is_some_and(|p| p.link == Some(link));
        if !current {
            self.manager
                .close(link, Some(Error::AuthenticationRejected), radio);
            return;
        }
        let retained = self.manager.peer(identity);
        let pair = self.pair.as_ref().unwrap();
        let result = if pair.cancelling.is_some() || now >= pair.deadline || !self.serial.active() {
            Err(Error::AuthenticationRejected)
        } else {
            let name = pair.name.clone();
            self.manager
                .bonded(link, identity, name.as_bytes(), store, radio)
                .await
        };
        match result {
            Ok(slot) => {
                self.manager.connection_mut(link).unwrap().deadline =
                    now.saturating_add(CONNECT_TIMEOUT_MS);
                let pair = self.pair.take().unwrap();
                for c in &mut self.candidates {
                    if c.id == pair.candidate {
                        c.peer = identity;
                    }
                }
                if !self.manager.devices[slot]
                    .as_ref()
                    .unwrap()
                    .effective_enabled
                {
                    self.close(link, None, radio);
                }
                self.mark(slot, DEVICE | SETTINGS | WARNINGS);
                self.adapter_dirty = true;
                let device = self.manager.devices[slot]
                    .as_ref()
                    .unwrap()
                    .policy
                    .device_id()
                    .0;
                self.pairing_ended(&pair, p::pairing::Step::Done(p::PairingDone { device }));
            }
            Err(error) => {
                let protected = retained.is_some();
                if let Some(pair) = &mut self.pair
                    && !protected
                {
                    // Adopt may already have succeeded before the record write
                    // failed. Explicit cleanup waits for disconnection.
                    pair.cleanup = Some(identity);
                }
                self.stop_pairing(error, radio);
            }
        }
    }

    /// Advances the pairing attempt: opens its link once other setup links have closed, and
    /// finishes a cancelled attempt once its link is gone.
    async fn advance_pair<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let Some(pair) = &self.pair else { return };
        if pair.link.is_some() {
            let prompt_expired = pair
                .prompt
                .as_ref()
                .is_some_and(|p| !p.answered && now >= p.deadline);
            if (pair.cancelling.is_none() && now >= pair.deadline) || prompt_expired {
                self.stop_pairing(Error::Timeout, radio);
            }
            return;
        }
        if let Some(error) = pair.cancelling {
            let cleanup = self.pair.as_mut().unwrap().cleanup.take();
            let result = if let Some(peer) = cleanup {
                radio.forget(peer).await
            } else {
                Ok(())
            };
            let restored = self.manager.finish_pair(store, radio).await;
            let code = result.err().or(restored.err()).unwrap_or(error);
            let pair = self.pair.take().unwrap();
            self.pairing_ended(
                &pair,
                p::pairing::Step::Failed(wire::error_code(code) as i32),
            );
            return;
        }
        if now >= pair.deadline {
            self.stop_pairing(Error::Timeout, radio);
            return;
        }
        if self
            .manager
            .connections
            .iter()
            .flatten()
            .any(|c| c.closing || c.runtime.is_none())
        {
            return;
        }
        let (address, expected, deadline) = (pair.address, pair.expected, pair.deadline);
        let result = async {
            if !self.manager.radio_ready || !self.manager.storage_ready {
                return Err(Error::RadioUnavailable);
            }
            self.manager
                .prepare_pair(address, expected, store, radio)
                .await?;
            self.manager.pair(address, deadline, radio)
        }
        .await;
        match result {
            Ok(id) => self.pair.as_mut().unwrap().link = Some(id),
            Err(error) => {
                let error = self
                    .manager
                    .finish_pair(store, radio)
                    .await
                    .err()
                    .unwrap_or(error);
                let pair = self.pair.take().unwrap();
                self.pairing_ended(
                    &pair,
                    p::pairing::Step::Failed(wire::error_code(error) as i32),
                );
            }
        }
    }

    fn update_scan<B: Bluetooth>(&mut self, radio: &mut B, reconnecting: bool, now: u64) {
        if !self.manager.radio_ready {
            return;
        }
        let mut wanted = self.scan.as_ref().map(|s| (s.token, s.classic, s.ble));
        // Exclusive backends alternate one-second windows while saved peers are
        // eligible. The logical scan stays open, retaining its token and results.
        if reconnecting && !radio.capabilities().ble_scan_and_connect && now % 2000 >= 1000 {
            wanted = wanted.map(|(token, classic, _)| (token, classic, false));
            if wanted.is_some_and(|(_, classic, ble)| !classic && !ble) {
                wanted = None;
            }
        }
        if wanted != self.radio_scan {
            let (token, classic, ble) = wanted.unwrap_or((self.scan_seq, false, false));
            if radio.scan(token, classic, ble).is_ok() {
                self.radio_scan = wanted;
            }
        }
    }

    /// Finishes unpairs whose link has now closed.
    async fn finish_unpairs<S: RecordStore, B: Bluetooth>(&mut self, store: &mut S, radio: &mut B) {
        let mut i = 0;
        while i < self.unpairing.len() {
            let id = self.unpairing[i];
            let Some(slot) = self
                .manager
                .devices
                .iter()
                .position(|d| d.as_ref().is_some_and(|d| d.policy.id == id))
            else {
                self.unpairing.swap_remove(i);
                continue;
            };
            if self.manager.link_for(slot).is_some() {
                i += 1;
                continue;
            }
            self.unpairing.swap_remove(i);
            if let Err(code) = self.remove(slot, store, radio).await
                && let Some(d) = self.manager.devices[slot].as_mut()
            {
                d.error = Some(code);
                self.mark(slot, DEVICE);
            }
        }
    }

    /// Marks devices whose settings, information or warnings changed since the last check.
    fn collect_changes(&mut self) {
        for slot in 0..self.manager.devices.len() {
            let Some(d) = self.manager.devices[slot].as_mut() else {
                continue;
            };
            let mut bits = 0;
            if core::mem::take(&mut d.catalog.catalog_changed)
                | (d.catalog.take_changed().count() != 0)
            {
                bits |= SETTINGS;
                // Read-only records are information on the device record.
                bits |= DEVICE;
            }
            if !d.catalog.info.changes().is_empty() {
                bits |= DEVICE;
            }
            if core::mem::take(&mut d.warnings_changed) {
                bits |= WARNINGS;
            }
            if bits != 0 {
                self.mark(slot, bits);
            }
        }
        while let Some(id) = self.manager.removed.pop() {
            self.removed.push_back(id.0);
            self.adapter_dirty = true;
        }
        let ready = self.manager.radio_ready && self.manager.storage_ready;
        if self.reported_ready != ready {
            self.reported_ready = ready;
            self.adapter_dirty = true;
        }
    }

    /// Writes at most one pending event, when the serial output is free.
    fn flush<B: Bluetooth>(&mut self, radio: &B) {
        if !self.serial.idle() {
            return;
        }
        if core::mem::take(&mut self.adapter_dirty) {
            let status = self.status(radio);
            return self.serial.event(Ev::Adapter(status));
        }
        if let Some(id) = self.removed.pop_front() {
            return self
                .serial
                .event(Ev::DeviceRemoved(p::DeviceRemoved { id }));
        }
        if core::mem::take(&mut self.pairing_dirty)
            && let Some(pairing) = self.pairing.clone()
        {
            return self.serial.event(Ev::Pairing(pairing));
        }
        let event = if self.scan_turn {
            self.scan_event().or_else(|| self.device_event())
        } else {
            self.device_event().or_else(|| self.scan_event())
        };
        if let Some(event) = event {
            self.scan_turn = !self.scan_turn;
            self.serial.event(event);
        }
    }

    /// The next device, settings or warnings event, starting from the slot after the last one
    /// sent.
    fn device_event(&mut self) -> Option<Ev> {
        let n = self.dirty.len();
        for k in 0..n {
            let slot = (self.next_slot + k) % n;
            let bits = self.dirty[slot];
            if bits == 0 {
                continue;
            }
            let event = if bits & DEVICE != 0 {
                self.dirty[slot] &= !DEVICE;
                wire::device(&self.manager, slot).map(Ev::Device)
            } else if bits & SETTINGS != 0 {
                self.dirty[slot] &= !SETTINGS;
                wire::settings(&self.manager, slot).map(Ev::Settings)
            } else {
                self.dirty[slot] = 0;
                wire::warnings(&self.manager, slot).map(Ev::Warnings)
            };
            if event.is_some() {
                self.next_slot = slot + 1;
                return event;
            }
        }
        None
    }

    fn scan_event(&mut self) -> Option<Ev> {
        if let Some(c) = self.candidates.iter_mut().find(|c| c.dirty) {
            c.dirty = false;
            return Some(Ev::ScanFound(p::Candidate {
                id: c.id.clone(),
                transport: wire::transport(c.peer.transport) as i32,
                name: c.name.clone().into(),
                kind: wire::kind(c.kind) as i32,
                rssi: c.rssi,
            }));
        }
        self.scan_done.take().map(Ev::ScanDone)
    }

    pub async fn poll<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        leds: u8,
        now: u64,
    ) {
        if let Some(deadline) = self.reboot_at {
            if ((self.serial.queued() == 0 && self.manager.forward.pending() == 0)
                || now >= deadline)
                && let Some(boot) = &self.build.bootloader
            {
                (boot.enter)();
            }
            return;
        }
        if self.scan.as_ref().is_some_and(|s| now >= s.deadline) {
            self.stop_scan(radio);
        }
        for index in 0..devices::ACTIVE_CONNECTIONS {
            let id = self.manager.connections[index].as_ref().map(|c| c.id);
            match self.manager.poll_link(index, leds, now, radio) {
                Ok(Some(slot)) => self.mark(slot, DEVICE),
                Err(e) => {
                    if let Some(id) = id {
                        self.close(id, Some(e), radio);
                    }
                }
                _ => {}
            }
        }
        self.setup(store).await;
        self.advance_pair(store, radio, now).await;
        self.finish_unpairs(store, radio).await;
        self.start_jobs(now);
        if self.pair.is_none() && self.manager.storage_ready && self.manager.radio_ready {
            for slot in 0..self.manager.devices.len() {
                if self.manager.connections.iter().flatten().count()
                    >= devices::ACTIVE_CONNECTIONS - 1
                    || self
                        .manager
                        .connections
                        .iter()
                        .flatten()
                        .any(|c| c.runtime.is_none() || c.closing)
                {
                    break;
                }
                let Some(d) = self.manager.devices[slot]
                    .as_ref()
                    .filter(|d| d.policy.peer.transport == Transport::Classic && d.page_due(now))
                else {
                    continue;
                };
                let id = d.policy.id;
                let layout = crate::layouts::load(store, id, Transport::Classic).await;
                match self.manager.connect(
                    slot,
                    false,
                    now.saturating_add(CONNECT_TIMEOUT_MS),
                    layout.as_ref(),
                    radio,
                ) {
                    Ok(_) => self.mark(slot, DEVICE),
                    // A radio busy with other link setup takes the page shortly, without
                    // reading the saved layout on every poll meanwhile.
                    Err(Error::Busy | Error::Capacity) => self.manager.devices[slot]
                        .as_mut()
                        .unwrap()
                        .defer_page(now.saturating_add(PAGE_BUSY_MS)),
                    Err(e) => {
                        self.manager.devices[slot].as_mut().unwrap().connection(
                            ConnectionState::Disconnected,
                            Some(e),
                            now,
                        );
                        self.mark(slot, DEVICE);
                    }
                }
            }
        }
        let available = self.pair.is_none()
            && self.manager.storage_ready
            && self.manager.radio_ready
            && self.manager.ble_admission();
        let peers: Vec<_> = self
            .manager
            .devices
            .iter()
            .filter_map(|device| {
                let d = device.as_ref()?;
                (available && d.policy.peer.transport == Transport::Ble && d.admit_due(now))
                    .then_some(d.policy.peer)
            })
            .collect();
        if self.manager.radio_ready
            && let Err(error) = radio.reconnect(&peers)
            && error != Error::Busy
        {
            self.event(Event::Failed(error), store, radio, now).await;
        }
        self.update_scan(radio, !peers.is_empty(), now);
        self.collect_changes();
        if self.serial.active() {
            self.flush(radio);
        }
    }

    fn dropped(&mut self, link: LinkId, error: Option<Error>, now: u64) {
        if let Some((slot, runtime)) = self.manager.disconnected(link, error, now) {
            if let Some(slot) = slot {
                self.mark(slot, DEVICE | SETTINGS);
                if let Some(mut runtime) = runtime {
                    runtime
                        .settings
                        .release(&mut self.manager.devices[slot].as_mut().unwrap().catalog);
                }
            }
            if let Some(pair) = &mut self.pair
                && pair.link == Some(link)
            {
                pair.link = None;
                pair.prompt = None;
                if pair.cancelling.is_none() {
                    pair.cancelling = Some(error.unwrap_or(Error::ConnectionFailed));
                }
            }
        }
    }
}
