//! Command workflows: checks the view can predict, the requests, and waiting for effects that
//! arrive as events.
use crate::{
    controller::{Command, Outcome, RunOptions, Session, Subject, Toggle, Wait},
    error::Error,
    model::{self, Prompt},
    storage,
    ui::catalog,
    view::State,
};
use cordial_protocol::{
    self as p, CapacityReason, CodeKind, ErrorCode, IntegrationKind, keys, pairing,
    request::Command as C, response,
};
use std::{collections::BTreeMap, time::Duration};

pub(crate) const STARTING: &str =
    "the adapter is still starting; status, files and bootloader work meanwhile";

/// The longest adapter name, in UTF-8 bytes.
const NAME_BYTES: usize = 64;

/// The longest scan the adapter runs.
pub const MAX_SCAN_SECONDS: u32 = 60;

/// Whether an adapter name is accepted: 1 to 64 bytes without control characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= NAME_BYTES && !name.chars().any(char::is_control)
}

/// A refusal the Dongle would send, predicted from the view.
fn refused(code: ErrorCode, command: &'static str) -> Error {
    Error::code(code, Some(command))
}

fn no_capacity(reason: CapacityReason, command: &'static str) -> Error {
    Error::dongle(
        p::Error {
            code: ErrorCode::NoCapacity as i32,
            reason: reason as i32,
            outcome_unknown: false,
        },
        Some(command),
    )
}

/// The scan's transports: those named, or every one the adapter supports.
pub fn scan_transports(st: &State, named: &[p::Transport]) -> Result<Vec<p::Transport>, String> {
    let supported = model::transports(&st.status);
    if named.is_empty() {
        if supported.is_empty() {
            return Err("this adapter doesn't support scanning".into());
        }
        return Ok(supported);
    }
    if named.iter().any(|t| !supported.contains(t)) {
        return Err("this adapter does not support the requested scan".into());
    }
    Ok(named.to_vec())
}

/// Why `command` can't run on this adapter, as far as the view tells.
pub fn unsupported(st: &State, command: &Command) -> Option<&'static str> {
    let development = model::development(&st.status);
    match command {
        Command::Files(_) | Command::FileGet { .. } if !development => {
            Some("this adapter doesn't offer file access")
        }
        Command::Bootloader | Command::Features(_) if !development => {
            Some("this adapter doesn't offer development functions")
        }
        Command::Scan { transports, .. } if scan_transports(st, transports).is_err() => {
            Some("this adapter does not support the requested scan")
        }
        _ => None,
    }
}

pub(crate) fn execute(
    session: &Session,
    command: &Command,
    options: &RunOptions,
) -> Result<Outcome, Error> {
    let st = session.state();
    if !st.available {
        return Err(Error::new(
            "the control session is unavailable; select an adapter",
        ));
    }
    if !st.ready() && !command.direct() {
        return Err(Error::new(STARTING));
    }
    if let Some(reason) = unsupported(&st, command) {
        return Err(Error::new(reason));
    }
    let wait = &options.wait;
    match command {
        Command::Status => {
            let r = session.call(C::GetStatus(p::GetStatus {}), "adapter status", true)?;
            match r.result {
                Some(response::Result::Status(status)) => Ok(Outcome::Status(status)),
                _ => Err(unexpected()),
            }
        }
        Command::Name(name) => {
            let name = name.clone().unwrap_or_default();
            if !name.is_empty() && !valid_name(&name) {
                return Err(Error::new("invalid adapter name"));
            }
            let status = set_adapter(
                session,
                p::SetAdapter {
                    name: Some(name),
                    platform: None,
                },
            )?;
            Ok(Outcome::Name(status.name))
        }
        Command::Platform(platform) => {
            let status = set_adapter(
                session,
                p::SetAdapter {
                    name: None,
                    platform: Some(*platform as i32),
                },
            )?;
            Ok(Outcome::Platform(status.platform()))
        }
        Command::Bootloader => {
            session.call(
                C::EnterBootloader(p::EnterBootloader {}),
                "adapter bootloader",
                true,
            )?;
            Ok(Outcome::Bootloader)
        }
        Command::Scan {
            transports,
            seconds,
        } => scan(session, &st, transports, *seconds, options),
        Command::ScanStop => {
            let was_running = st.scanning.is_some();
            session.call(C::StopScan(p::StopScan {}), "scan stop", true)?;
            Ok(Outcome::ScanStopped { was_running })
        }
        Command::Pair(target) => pair(session, target, options),
        Command::Accept(value) => accept(session, &st, value.as_deref()),
        Command::Reject => {
            if !st.pairing.as_ref().is_some_and(model::answerable) {
                return Err(refused(ErrorCode::NoPrompt, "pair reject"));
            }
            session.call(C::RejectPrompt(p::RejectPrompt {}), "pair reject", true)?;
            Ok(Outcome::Answered)
        }
        Command::CancelPairing => {
            if !st.pairing.as_ref().is_some_and(model::pairing_running) {
                return Err(Error::new("there is no pairing to cancel"));
            }
            session.call(C::CancelPairing(p::CancelPairing {}), "pair cancel", true)?;
            Ok(Outcome::PairingCancelled)
        }
        Command::Devices => {
            session.call(C::ListDevices(p::ListDevices {}), "device list", true)?;
            Ok(Outcome::Devices)
        }
        Command::Files(path) => {
            let r = session.call(
                C::ListFiles(p::ListFiles { path: path.clone() }),
                "file list",
                true,
            )?;
            match r.result {
                Some(response::Result::Files(list)) => Ok(Outcome::Files {
                    path: path.clone(),
                    entries: list.entries,
                }),
                _ => Err(unexpected()),
            }
        }
        Command::FileGet {
            path,
            local,
            overwrite,
        } => {
            storage::check_destination(local, *overwrite)?;
            let r = session.call(
                C::ReadFile(p::ReadFile { path: path.clone() }),
                "file get",
                true,
            )?;
            let Some(response::Result::File(file)) = r.result else {
                return Err(unexpected());
            };
            let bytes = storage::save(local, *overwrite, &file.data)?;
            Ok(Outcome::FileSaved {
                path: path.clone(),
                local: local.clone(),
                bytes,
            })
        }
        Command::Get(target)
        | Command::Connect(target)
        | Command::Disconnect(target)
        | Command::Unpair(target)
        | Command::Refresh(target)
        | Command::Set(target, ..)
        | Command::Warnings(target)
        | Command::Settings(target)
        | Command::SettingGet(target, _)
        | Command::SettingSet(target, ..)
        | Command::SettingForget(target, _)
        | Command::SettingsSave { device: target, .. }
        | Command::Features(target) => device_command(session, &st, command, target, wait),
    }
}

fn unexpected() -> Error {
    Error::new("the adapter returned an unexpected result")
}

fn set_adapter(session: &Session, update: p::SetAdapter) -> Result<p::Status, Error> {
    let name = if update.name.is_some() {
        "adapter name"
    } else {
        "adapter platform"
    };
    match session.call(C::SetAdapter(update), name, true)?.result {
        Some(response::Result::Status(status)) => Ok(status),
        _ => Err(unexpected()),
    }
}

fn scan(
    session: &Session,
    st: &State,
    named: &[p::Transport],
    seconds: u32,
    options: &RunOptions,
) -> Result<Outcome, Error> {
    let transports = scan_transports(st, named).map_err(Error::new)?;
    if seconds > MAX_SCAN_SECONDS {
        return Err(Error::new("scan duration must be between 1s and 60s"));
    }
    session.call(
        C::StartScan(p::StartScan {
            transports: transports.iter().map(|t| *t as i32).collect(),
            seconds,
        }),
        "scan start",
        true,
    )?;
    if !options.one_shot {
        return Ok(Outcome::ScanStarted(transports));
    }
    let done = session.cell.wait_for(&options.wait, |st| {
        st.scanning.is_none().then_some(st.last_scan)
    });
    match done {
        Ok(done) => {
            let done = done.unwrap_or_default();
            Ok(Outcome::ScanFinished {
                count: done.count,
                truncated: done.truncated,
            })
        }
        Err(error) => {
            let _ = session.connection.stop_scan();
            Err(error)
        }
    }
}

/// Resolves a pair target: a candidate ID, else a name exactly one candidate has.
fn resolve_candidate(st: &State, target: &str) -> Result<p::Candidate, Error> {
    if let Some(c) = st.candidate(target) {
        return Ok(c.clone());
    }
    let mut matches = st.candidates.iter().filter(|c| c.name == target);
    let candidate = matches.next().ok_or_else(|| {
        Error::new(
            "put the device in pairing mode, scan, and pair its Nearby entry or candidate ID",
        )
    })?;
    if matches.next().is_some() {
        return Err(Error::new(
            "more than one nearby device has that name; select its candidate ID in the interactive CLI or TUI",
        ));
    }
    Ok(candidate.clone())
}

fn pair(session: &Session, target: &str, options: &RunOptions) -> Result<Outcome, Error> {
    let mut st = session.state();
    if options.one_shot && st.candidate(target).is_none() {
        // A one-shot pair finds its device by name with a scan of its own.
        scan(session, &st, &[], 0, options)?;
        st = session.state();
    }
    let candidate = resolve_candidate(&st, target)?;
    if model::storage_full(&st.status) {
        return Err(no_capacity(CapacityReason::Storage, "pair start"));
    }
    if st.pairing.as_ref().is_some_and(model::pairing_running) {
        return Err(refused(ErrorCode::Busy, "pair start"));
    }
    if model::transport(candidate.transport).is_none_or(|t| !model::supports(&st.status, t)) {
        return Err(refused(ErrorCode::Unsupported, "pair start"));
    }
    let _pending = session.pending("pair start", Some(&candidate.id));
    session.call(
        C::StartPairing(p::StartPairing {
            candidate: candidate.id.clone(),
        }),
        "pair start",
        true,
    )?;
    let ended = session.cell.wait_for(&options.wait, |st| {
        let p = st
            .pairing
            .as_ref()
            .filter(|p| p.candidate == candidate.id)?;
        match &p.step {
            Some(pairing::Step::Done(done)) => Some(Ok(done.device.clone())),
            Some(pairing::Step::Failed(code)) => Some(Err(model::code(*code))),
            _ => None,
        }
    });
    let device = match ended {
        Ok(Ok(device)) => device,
        Ok(Err(code)) => return Err(Error::code(code, Some("pair start"))),
        Err(error) => {
            let _ = session.connection.cancel_pairing();
            return Err(error);
        }
    };
    // The saved record follows the pairing's end as a device event.
    let record = session
        .cell
        .wait_for(&Wait::timeout(Duration::from_secs(2)), |st| {
            st.device(&device).cloned()
        })
        .ok();
    Ok(Outcome::Paired {
        subject: Subject {
            name: record
                .as_ref()
                .map_or_else(|| candidate.name.clone(), |d| d.name.clone()),
            id: device,
        },
        device: record,
    })
}

/// Checks a typed pairing code: six digits for a passkey, or 1 to 16 printable ASCII
/// characters for a PIN.
pub fn check_code(kind: CodeKind, value: &str) -> Result<(), String> {
    match kind {
        CodeKind::Pin => {
            if value.is_empty() || value.len() > 16 {
                return Err("PIN must contain 1 to 16 printable ASCII characters".into());
            }
            if !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
                return Err("PIN must be printable ASCII".into());
            }
        }
        _ => {
            if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("passkey must contain exactly six digits".into());
            }
        }
    }
    Ok(())
}

fn accept(session: &Session, st: &State, value: Option<&str>) -> Result<Outcome, Error> {
    let prompt = st.pairing.as_ref().and_then(model::prompt);
    let value = match (prompt, value) {
        (Some(Prompt::EnterCode(kind)), Some(value)) => {
            check_code(kind, value).map_err(Error::new)?;
            value
        }
        (Some(Prompt::EnterCode(_)), None) => {
            return Err(Error::new("the pairing asks for a code"));
        }
        (Some(Prompt::ConfirmCode(_)), _) => "",
        _ => return Err(refused(ErrorCode::NoPrompt, "pair accept")),
    };
    session.call(
        C::AcceptPrompt(p::AcceptPrompt {
            value: value.into(),
        }),
        "pair accept",
        true,
    )?;
    Ok(Outcome::Answered)
}

/// A resolved device word.
enum Resolved {
    Saved(p::Device),
    Candidate(p::Candidate),
}

/// Resolves a device word: an ID, else a name exactly one saved device or candidate has.
fn resolve(st: &State, target: &str) -> Result<Resolved, Error> {
    if let Some(d) = st.device(target) {
        return Ok(Resolved::Saved(d.clone()));
    }
    if let Some(c) = st.candidate(target) {
        return Ok(Resolved::Candidate(c.clone()));
    }
    let mut matches: BTreeMap<String, Resolved> = BTreeMap::new();
    for d in st.devices.iter().filter(|d| d.name == target) {
        matches.insert(d.id.clone(), Resolved::Saved(d.clone()));
    }
    for c in st.candidates.iter().filter(|c| c.name == target) {
        matches
            .entry(c.id.clone())
            .or_insert_with(|| Resolved::Candidate(c.clone()));
    }
    if matches.len() > 1 {
        return Err(Error::new(format!(
            "name {target:?} is ambiguous; use a device or candidate ID"
        )));
    }
    matches.pop_first().map(|(_, r)| r).ok_or_else(|| {
        Error::new(format!(
            "device {target:?} not found; scan or use a saved device ID"
        ))
    })
}

fn subject(d: &p::Device) -> Subject {
    Subject {
        id: d.id.clone(),
        name: d.name.clone(),
    }
}

fn device_result(r: p::Response) -> Result<p::Device, Error> {
    match r.result {
        Some(response::Result::Device(d)) => Ok(d),
        _ => Err(unexpected()),
    }
}

fn settings_result(r: p::Response) -> Result<Vec<p::Setting>, Error> {
    match r.result {
        Some(response::Result::Settings(s)) => Ok(s.settings),
        _ => Err(unexpected()),
    }
}

fn device_command(
    session: &Session,
    st: &State,
    command: &Command,
    target: &str,
    _wait: &Wait,
) -> Result<Outcome, Error> {
    let d = match resolve(st, target)? {
        Resolved::Saved(d) => d,
        Resolved::Candidate(c) => {
            let subject = Subject {
                id: c.id.clone(),
                name: c.name.clone(),
            };
            return match command {
                Command::Get(_) => Ok(Outcome::Candidate(c)),
                Command::Unpair(_) => {
                    session.cell.update(|st| st.hidden.insert(c.id.clone()));
                    Ok(Outcome::Hidden(subject))
                }
                _ => Err(Error::new("pair the candidate before using this command")),
            };
        }
    };
    let id = d.id.clone();
    let subject = subject(&d);
    match command {
        Command::Get(_) => {
            let device = device_result(session.call(
                C::GetDevice(p::GetDevice { device: id.clone() }),
                "device get",
                true,
            )?)?;
            session.call(
                C::ListWarnings(p::ListWarnings { device: id }),
                "warning list",
                false,
            )?;
            Ok(Outcome::Device { subject, device })
        }
        Command::Connect(_) => {
            if d.blocked {
                return Err(refused(ErrorCode::Blocked, "device connect"));
            }
            if !d.enabled {
                return Err(refused(ErrorCode::Disabled, "device connect"));
            }
            if model::inactive(&d) == Some(p::InactiveReason::Capacity) {
                return Err(no_capacity(CapacityReason::Enabled, "device connect"));
            }
            let _pending = session.pending("device connect", Some(&id));
            let device = device_result(session.call(
                C::ConnectDevice(p::ConnectDevice { device: id }),
                "device connect",
                true,
            )?)?;
            Ok(Outcome::Device { subject, device })
        }
        Command::Disconnect(_) => {
            let _pending = session.pending("device disconnect", Some(&id));
            let device = device_result(session.call(
                C::DisconnectDevice(p::DisconnectDevice { device: id }),
                "device disconnect",
                true,
            )?)?;
            Ok(Outcome::Device { subject, device })
        }
        Command::Unpair(_) => {
            let _pending = session.pending("device unpair", Some(&id));
            session.call(
                C::UnpairDevice(p::UnpairDevice { device: id }),
                "device unpair",
                true,
            )?;
            Ok(Outcome::Unpaired(subject))
        }
        Command::Refresh(_) => {
            if !model::connected(&d) {
                return Err(refused(ErrorCode::NotConnected, "device refresh"));
            }
            session.call(
                C::RefreshDevice(p::RefreshDevice { device: id }),
                "device refresh",
                true,
            )?;
            Ok(Outcome::Refreshing(subject))
        }
        Command::Set(_, toggle, on) => {
            let mut update = p::SetDevice {
                device: id.clone(),
                ..Default::default()
            };
            match toggle {
                Toggle::Enabled => {
                    if *on && !d.enabled && enabled_full(st, &d) {
                        return Err(no_capacity(CapacityReason::Enabled, "device set"));
                    }
                    update.enabled = Some(*on);
                }
                Toggle::Trusted => update.trusted = Some(*on),
                Toggle::Blocked => update.blocked = Some(*on),
                Toggle::Hidpp => update.integrations.push(p::IntegrationUpdate {
                    kind: IntegrationKind::Hidpp as i32,
                    enabled: Some(*on),
                }),
            }
            let _pending = session.pending(pending_name(*toggle), Some(&id));
            let device = device_result(session.call(C::SetDevice(update), "device set", true)?)?;
            Ok(Outcome::Device { subject, device })
        }
        Command::Warnings(_) => {
            let r = session.call(
                C::ListWarnings(p::ListWarnings { device: id }),
                "warning list",
                true,
            )?;
            match r.result {
                Some(response::Result::Warnings(w)) => Ok(Outcome::Warnings {
                    subject,
                    warnings: w.warnings,
                }),
                _ => Err(unexpected()),
            }
        }
        Command::Settings(_) => {
            let _pending = session.pending("setting list", Some(&id));
            list_settings(session, &id, true)?;
            Ok(Outcome::Settings(subject))
        }
        Command::SettingGet(_, key) => {
            let settings = list_settings(session, &id, true)?;
            let setting = find(&settings, key, "setting get")?;
            Ok(Outcome::Setting { subject, setting })
        }
        Command::SettingSet(_, key, input) => {
            let settings = list_settings(session, &id, false)?;
            let setting = find(&settings, key, "setting set")?;
            let value = catalog::setting_value(&setting, input).map_err(Error::new)?;
            save(session, &subject, vec![(setting, value)], Vec::new())
        }
        Command::SettingForget(_, key) => {
            let settings = list_settings(session, &id, false)?;
            let setting = find(&settings, key, "setting forget")?;
            save(session, &subject, Vec::new(), vec![setting])
        }
        Command::SettingsSave { set, forget, .. } => {
            let known = st.settings_of(&id);
            let set = set
                .iter()
                .map(|(key, value)| {
                    let setting = find(known, key, "setting set")?;
                    if !model::accepts(&setting, value) {
                        return Err(refused(ErrorCode::BadArgs, "setting set"));
                    }
                    Ok((setting, value.clone()))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            let forget = forget
                .iter()
                .map(|key| find(known, key, "setting forget"))
                .collect::<Result<Vec<_>, Error>>()?;
            save(session, &subject, set, forget)
        }
        Command::Features(_) => {
            let r = session.call(
                C::ListFeatures(p::ListFeatures { device: id }),
                "feature list",
                true,
            )?;
            match r.result {
                Some(response::Result::Features(list)) => Ok(Outcome::Features {
                    subject,
                    features: list.features,
                }),
                _ => Err(unexpected()),
            }
        }
        _ => unreachable!(),
    }
}

/// The progress name of a device preference change.
pub fn pending_name(toggle: Toggle) -> &'static str {
    match toggle {
        Toggle::Enabled => "device set enabled",
        Toggle::Trusted => "device set trusted",
        Toggle::Blocked => "device set blocked",
        Toggle::Hidpp => "device set hidpp",
    }
}

/// Whether the device's transport already has as many devices in use as the adapter allows.
/// A device is in use when the adapter reports no reason it is inactive.
pub fn enabled_full(st: &State, d: &p::Device) -> bool {
    let Some(transport) = model::transport(d.transport) else {
        return false;
    };
    let Some(max) = model::max_enabled(&st.status, transport) else {
        return false;
    };
    let enabled = st
        .devices
        .iter()
        .filter(|o| o.id != d.id && o.inactive.is_none() && o.transport == d.transport)
        .count();
    enabled >= max as usize
}

fn list_settings(session: &Session, id: &str, reported: bool) -> Result<Vec<p::Setting>, Error> {
    settings_result(session.call(
        C::ListSettings(p::ListSettings { device: id.into() }),
        "setting list",
        reported,
    )?)
}

/// The setting with `key`. A key the catalog doesn't know is never shown, so it isn't found.
fn find(settings: &[p::Setting], key: &str, command: &'static str) -> Result<p::Setting, Error> {
    settings
        .iter()
        .find(|s| s.key == key && keys::lookup(&s.key).is_some())
        .cloned()
        .ok_or_else(|| refused(ErrorCode::NotFound, command))
}

fn save(
    session: &Session,
    subject: &Subject,
    set: Vec<(p::Setting, p::value::Value)>,
    forget: Vec<p::Setting>,
) -> Result<Outcome, Error> {
    let _pending = session.pending("setting set", Some(&subject.id));
    let mut settings = Vec::new();
    let set_keys: Vec<String> = set.iter().map(|(s, _)| s.key.clone()).collect();
    if !set.is_empty() {
        let changes = set
            .into_iter()
            .map(|(s, value)| p::SettingChange {
                integration: s.integration,
                key: s.key,
                value: Some(model::wire_value(value)),
            })
            .collect();
        settings = settings_result(session.call(
            C::SetSettings(p::SetSettings {
                device: subject.id.clone(),
                changes,
            }),
            "setting set",
            true,
        )?)?;
    }
    let forget_keys: Vec<String> = forget.iter().map(|s| s.key.clone()).collect();
    if !forget.is_empty() {
        settings = settings_result(session.call(
            C::ForgetSettings(p::ForgetSettings {
                device: subject.id.clone(),
                settings: forget.iter().map(model::reference).collect(),
            }),
            "setting forget",
            true,
        )?)?;
    }
    Ok(Outcome::Saved {
        subject: subject.clone(),
        set: set_keys,
        forget: forget_keys,
        settings,
    })
}
