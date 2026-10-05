//! Command workflows: checks the view can predict, the requests, and waiting for effects that
//! arrive as events.
use crate::{
    controller::{
        AdapterUpdate, Command, DeviceUpdate, Outcome, Pick, RunOptions, Session, Subject, Target,
        Toggle, Wait,
    },
    error::Error,
    model::{self, Prompt},
    profiles::{self, InterfaceUpdate},
    storage,
    ui::{catalog, text},
    view::State,
};
use cordial_client::paging::{self as paging, Page};
use cordial_protocol::{
    self as p, CapacityReason, CodeKind, ErrorCode, IntegrationKind, keys, pairing,
    profile_list_entry, profile_rule_change, request::Command as C, response,
};
use std::time::Duration;

pub(crate) const STARTING: &str =
    "the adapter is still starting; status, files and bootloader work meanwhile";

pub(crate) const NO_PROFILES: &str = "this adapter doesn't support profiles";

/// The longest adapter name, in UTF-8 bytes.
const NAME_BYTES: usize = 64;

/// The longest scan the adapter runs.
pub const MAX_SCAN_SECONDS: u32 = 60;

/// The name the adapter saves for `value`: trimmed, 1 to 64 bytes and without control
/// characters, or `None` when the adapter refuses it.
pub fn adapter_name(value: &str) -> Option<&str> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let name = value.trim();
    (!name.is_empty() && name.len() <= NAME_BYTES).then_some(name)
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

/// The scan's transports: those named, or every one the adapter supports, less those it has
/// disabled.
pub fn scan_transports(st: &State, named: &[p::Transport]) -> Result<Vec<p::Transport>, String> {
    let supported = model::transports(&st.status);
    if supported.is_empty() {
        return Err("this adapter doesn't support scanning".into());
    }
    let wanted = if named.is_empty() {
        supported.clone()
    } else {
        named.to_vec()
    };
    if wanted.iter().any(|t| !supported.contains(t)) {
        return Err("this adapter does not support the requested scan".into());
    }
    let usable: Vec<p::Transport> = wanted
        .iter()
        .copied()
        .filter(|t| model::transport_enabled(&st.status, *t) == Some(true))
        .collect();
    // With every transport disabled, the one named is the last wanted: BLE, when there are two.
    match wanted.last() {
        Some(last) if usable.is_empty() => Err(text::transport_disabled(*last)),
        _ => Ok(usable),
    }
}

/// Whether a command needs the adapter's profile support.
fn uses_profiles(command: &Command) -> bool {
    match command {
        Command::Interface { .. }
        | Command::Layers(..)
        | Command::Profiles { .. }
        | Command::AllProfiles
        | Command::ProfileShow(_)
        | Command::ProfileLookup(_)
        | Command::ProfileCreate(_)
        | Command::ProfileCopy(..)
        | Command::ProfileDelete(_)
        | Command::Rules(_)
        | Command::RuleChange(..) => true,
        Command::AdapterSave(update) => !update.interfaces.is_empty(),
        Command::DeviceSave(_, update) => update.layers.is_some(),
        _ => false,
    }
}

/// Why `command` can't run on this adapter, as far as the view tells.
pub fn unsupported(st: &State, command: &Command) -> Option<String> {
    let development = model::development(&st.status);
    match command {
        Command::Files(_) | Command::FileGet { .. } if !development => {
            Some("this adapter doesn't offer file access".into())
        }
        Command::Bootloader | Command::Features(_) if !development => {
            Some("this adapter doesn't offer development functions".into())
        }
        Command::Scan { transports, .. } => scan_transports(st, transports).err(),
        _ if uses_profiles(command) && !profiles::available(&st.status) => Some(NO_PROFILES.into()),
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
    match command {
        Command::Status => {
            let r = session.call(C::GetStatus(p::GetStatus {}), "adapter status", true)?;
            match r.result {
                Some(response::Result::Status(status)) => Ok(Outcome::Status(status)),
                _ => Err(unexpected()),
            }
        }
        Command::Name(name) => {
            // The adapter saves the name trimmed, so the trimmed name is sent and shown.
            let name = match name.as_deref() {
                None | Some("") => String::new(),
                Some(value) => adapter_name(value)
                    .ok_or_else(|| Error::new("invalid adapter name"))?
                    .to_owned(),
            };
            let before = session.cell.update(|st| st.status.name.clone());
            set_adapter(
                session,
                p::SetAdapter {
                    name: Some(name.clone()),
                    ..Default::default()
                },
                "adapter name",
            )?;
            if !name.is_empty() {
                return Ok(Outcome::Name { name, reset: false });
            }
            // Only the adapter knows its default name; it arrives as an adapter event. No event
            // follows when the name was already the default.
            let name = session
                .cell
                .wait_for(&Wait::timeout(Duration::from_secs(2)), |st| {
                    (st.status.name != before).then(|| st.status.name.clone())
                })
                .unwrap_or(before);
            Ok(Outcome::Name { name, reset: true })
        }
        Command::Platform(platform) => {
            set_adapter(
                session,
                p::SetAdapter {
                    platform: Some(*platform as i32),
                    ..Default::default()
                },
                "adapter platform",
            )?;
            Ok(Outcome::Platform(*platform))
        }
        Command::Transport(transport, on) => {
            if !model::supports(&st.status, *transport) {
                return Err(refused(ErrorCode::Unsupported, "adapter transport"));
            }
            set_adapter(
                session,
                p::SetAdapter {
                    transports: vec![p::TransportUpdate {
                        transport: *transport as i32,
                        enabled: Some(*on),
                    }],
                    ..Default::default()
                },
                "adapter transport",
            )?;
            Ok(Outcome::Transport(*transport, *on))
        }
        Command::Interface {
            interface,
            enabled,
            profile,
        } => {
            let profile = match profile {
                None => None,
                Some(Pick::Clear) => Some(0),
                Some(Pick::Profile(target)) => Some(resolve_profile(session, &st, target)?.id),
            };
            let update = AdapterUpdate {
                interfaces: vec![InterfaceUpdate {
                    interface: *interface,
                    enabled: *enabled,
                    profile,
                }],
                ..Default::default()
            };
            save_adapter(session, &st, &update, "adapter interface")?;
            Ok(Outcome::Interface(*interface, session.state().status))
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
                return Err(Error::new("no pairing is in progress"));
            }
            session.call(C::CancelPairing(p::CancelPairing {}), "pair cancel", true)?;
            Ok(Outcome::PairingCancelled)
        }
        Command::Devices => {
            read_pages::<p::DeviceList>(session, "device list", true, |after| {
                C::ListDevices(p::ListDevices {
                    after: after.copied().unwrap_or(0),
                })
            })?;
            Ok(Outcome::Devices)
        }
        Command::Files(path) => {
            let entries = read_pages::<p::FileList>(session, "file list", true, |after| {
                C::ListFiles(p::ListFiles {
                    path: path.clone(),
                    after: after.cloned().unwrap_or_default(),
                })
            })?;
            Ok(Outcome::Files {
                path: path.clone(),
                entries,
            })
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
        Command::Profiles { after } => {
            let list = read_page::<p::ProfileList>(
                session,
                C::ListProfiles(p::ListProfiles { after: *after }),
                "profile list",
                true,
            )?;
            Ok(Outcome::Profiles {
                after: *after,
                list,
            })
        }
        Command::AllProfiles => {
            let entries = read_pages::<p::ProfileList>(session, "profile list", true, |after| {
                C::ListProfiles(p::ListProfiles {
                    after: after.copied().unwrap_or(0),
                })
            })?;
            Ok(Outcome::Profiles {
                after: 0,
                list: p::ProfileList { entries, end: true },
            })
        }
        Command::ProfileShow(target) => {
            let id = match target {
                Target::Id(id) => *id,
                Target::Name(_) => resolve_profile(session, &st, target)?.id,
            };
            let profile = get_profile(session, id, true).map_err(|e| missing(e, target))?;
            Ok(Outcome::Profile(profile))
        }
        Command::ProfileLookup(id) => Ok(Outcome::Profile(get_profile(session, *id, true)?)),
        Command::ProfileCreate(name) => {
            if !profiles::valid_name(name) {
                return Err(Error::new("invalid profile name"));
            }
            let _pending = session.pending("profile create", 0);
            let request = C::CreateProfile(p::CreateProfile { name: name.clone() });
            let id = created_result(session.call(request, "profile create", true)?)?;
            Ok(Outcome::Profile(p::Profile {
                id,
                name: name.clone(),
                roles: Vec::new(),
            }))
        }
        Command::ProfileCopy(source, name) => {
            if !profiles::valid_name(name) {
                return Err(Error::new("invalid profile name"));
            }
            let source = resolve_profile(session, &st, source)?;
            let request = C::CopyProfile(p::CopyProfile {
                profile: source.id,
                name: name.clone(),
            });
            let _pending = session.pending("profile copy", 0);
            let id = created_result(session.call(request, "profile copy", true)?)?;
            Ok(Outcome::Profile(p::Profile {
                id,
                name: name.clone(),
                roles: source.roles,
            }))
        }
        Command::ProfileDelete(target) => {
            // An ID is deleted as given, even when the profile's name can't be read.
            let profile = match target {
                Target::Id(id) => st.profile(*id).cloned().unwrap_or_else(|| p::Profile {
                    id: *id,
                    name: id.to_string(),
                    ..Default::default()
                }),
                Target::Name(_) => resolve_profile(session, &st, target)?,
            };
            if let Some(reason) = profiles::in_use(&st, profile.id) {
                return Err(Error::new(reason));
            }
            let _pending = session.pending("profile delete", profile.id);
            session
                .call(
                    C::DeleteProfile(p::DeleteProfile {
                        profile: profile.id,
                    }),
                    "profile delete",
                    true,
                )
                .map_err(|e| missing(e, target))?;
            Ok(Outcome::ProfileDeleted(profile))
        }
        Command::Rules(target) => {
            let profile = resolve_profile(session, &st, target)?;
            let rules =
                read_pages::<p::ProfileRules>(session, "profile rule list", true, |after| {
                    C::ListProfileRules(p::ListProfileRules {
                        profile: profile.id,
                        after: after.copied(),
                    })
                })?;
            Ok(Outcome::Rules { profile, rules })
        }
        Command::RuleChange(target, change) => {
            if let Some(p::profile_rule_change::Change::Rule(rule)) = &change.change
                && let Some(support) = profiles::support(&st.status)
                && let Some(reason) = profiles::rule_refusal(support, rule)
            {
                return Err(Error::new(reason));
            }
            let profile = resolve_profile(session, &st, target)?;
            let _pending = session.pending("profile rule", profile.id);
            session.call(
                C::SetProfileRules(p::SetProfileRules {
                    profile: profile.id,
                    changes: vec![change.clone()],
                }),
                "profile rule",
                true,
            )?;
            // The profile now holds the rule sent in its saved form, or none for a forgotten
            // input or a rule that changes nothing.
            let rules = match (&change.change, profiles::support(&st.status)) {
                (Some(profile_rule_change::Change::Rule(rule)), Some(support)) => {
                    cordial_client::rules::normalized(support, rule)
                        .into_iter()
                        .collect()
                }
                (Some(profile_rule_change::Change::Rule(rule)), None) => vec![rule.clone()],
                _ => Vec::new(),
            };
            Ok(Outcome::Rules { profile, rules })
        }
        Command::AdapterSave(update) => {
            save_adapter(session, &st, update, "adapter settings")?;
            Ok(Outcome::AdapterSaved(session.state().status))
        }
        Command::Get(target)
        | Command::Connect(target)
        | Command::Disconnect(target)
        | Command::Unpair(target)
        | Command::Refresh(target)
        | Command::Set(target, ..)
        | Command::Layers(target, _)
        | Command::Warnings(target)
        | Command::Settings(target)
        | Command::SettingGet(target, _)
        | Command::SettingSet(target, ..)
        | Command::SettingForget(target, _)
        | Command::Features(target) => {
            let d = resolve_device(&st, target)?;
            device_command(session, &st, command, d)
        }
        Command::SettingsSave { device, .. } | Command::DeviceSave(device, _) => {
            let d = resolve_device(&st, &Target::Id(*device))?;
            device_command(session, &st, command, d)
        }
    }
}

/// An adapter's UNSUPPORTED refusal of work on `transports`, as the reason when the adapter now
/// has one of them disabled.
fn transport_refusal(session: &Session, transports: &[p::Transport], error: Error) -> Error {
    if error.code_of() != Some(ErrorCode::Unsupported) {
        return error;
    }
    let st = session.state();
    match transports
        .iter()
        .find(|t| model::transport_disabled(&st.status, **t))
    {
        Some(t) => Error::new(text::transport_disabled(*t)),
        None => error,
    }
}

fn unexpected() -> Error {
    Error::new("the adapter returned an unexpected result")
}

/// Reads one page of a listing.
fn read_page<P: Page>(
    session: &Session,
    request: C,
    name: &'static str,
    reported: bool,
) -> Result<P, Error> {
    session
        .call(request, name, reported)?
        .result
        .and_then(P::from_result)
        .ok_or_else(unexpected)
}

/// Reads a listing from the start, one request per page; `request` builds the request for the
/// page after a key.
fn read_pages<P: Page>(
    session: &Session,
    name: &'static str,
    reported: bool,
    request: impl Fn(Option<&P::Key>) -> C,
) -> Result<Vec<P::Entry>, Error> {
    paging::read_pages(
        |after| read_page::<P>(session, request(after), name, reported),
        unexpected,
    )
}

/// Saves adapter preferences. The view takes the values sent once the adapter accepts them.
fn set_adapter(session: &Session, update: p::SetAdapter, name: &'static str) -> Result<(), Error> {
    session.call(C::SetAdapter(update), name, true)?;
    Ok(())
}

fn get_profile(session: &Session, id: u32, reported: bool) -> Result<p::Profile, Error> {
    profile_result(session.call(
        C::GetProfile(p::GetProfile { profile: id }),
        "profile show",
        reported,
    )?)
}

fn profile_result(r: p::Response) -> Result<p::Profile, Error> {
    match r.result {
        Some(response::Result::Profile(profile)) => Ok(profile),
        _ => Err(unexpected()),
    }
}

fn created_result(r: p::Response) -> Result<u32, Error> {
    match r.result {
        Some(response::Result::ProfileCreated(created)) => Ok(created.profile),
        _ => Err(unexpected()),
    }
}

fn not_found(target: &Target) -> Error {
    Error::new(format!(
        "profile {} not found; use profile list",
        text::quote(&target.to_string())
    ))
}

/// A NOT_FOUND refusal of a profile command, as the profile the user named.
fn missing(error: Error, target: &Target) -> Error {
    match error.code_of() {
        Some(ErrorCode::NotFound) => not_found(target),
        _ => error,
    }
}

/// Resolves a profile: an ID, else a name exactly one profile has. Names are found by reading
/// every page of the adapter's profiles.
fn resolve_profile(session: &Session, st: &State, target: &Target) -> Result<p::Profile, Error> {
    match target {
        Target::Id(id) => match st.profile(*id) {
            Some(profile) => Ok(profile.clone()),
            None => get_profile(session, *id, false).map_err(|e| missing(e, target)),
        },
        Target::Name(name) => {
            let all = all_profiles(session)?;
            let id = unique_name(&all, name, target)?;
            Ok(all.into_iter().find(|p| p.id == id).unwrap_or_default())
        }
    }
}

/// Resolves several profiles in order, reading the profile pages at most once for their names.
fn resolve_profiles(session: &Session, st: &State, targets: &[Target]) -> Result<Vec<u32>, Error> {
    let mut pages = None;
    targets
        .iter()
        .map(|target| match target {
            Target::Id(_) => resolve_profile(session, st, target).map(|p| p.id),
            Target::Name(name) => {
                if pages.is_none() {
                    pages = Some(all_profiles(session)?);
                }
                unique_name(pages.as_deref().unwrap_or_default(), name, target)
            }
        })
        .collect()
}

/// Every readable saved profile, read page by page.
fn all_profiles(session: &Session) -> Result<Vec<p::Profile>, Error> {
    let entries = read_pages::<p::ProfileList>(session, "profile list", false, |after| {
        C::ListProfiles(p::ListProfiles {
            after: after.copied().unwrap_or(0),
        })
    })?;
    Ok(entries
        .into_iter()
        .filter_map(|entry| match entry.entry {
            Some(profile_list_entry::Entry::Profile(profile)) => Some(profile),
            _ => None,
        })
        .collect())
}

/// The ID of the only profile with `name`.
fn unique_name(all: &[p::Profile], name: &str, target: &Target) -> Result<u32, Error> {
    let mut named = all.iter().filter(|p| p.name == name);
    match (named.next(), named.next()) {
        (Some(p), None) => Ok(p.id),
        (None, _) => Err(not_found(target)),
        _ => Err(Error::new(format!(
            "profile name {} is ambiguous; use its ID",
            text::quote(name)
        ))),
    }
}

/// Saves `update` in one SetAdapter request.
fn save_adapter(
    session: &Session,
    st: &State,
    update: &AdapterUpdate,
    pending: &'static str,
) -> Result<(), Error> {
    for (transport, _) in &update.transports {
        if !model::supports(&st.status, *transport) {
            return Err(refused(ErrorCode::Unsupported, "adapter transport"));
        }
    }
    if !update.interfaces.is_empty() {
        if !profiles::available(&st.status) {
            return Err(Error::new(NO_PROFILES));
        }
        if let Some(error) = profiles::interface_refusal(&st.status, &update.interfaces) {
            return Err(error);
        }
    }
    let request = p::SetAdapter {
        platform: update.platform.map(|platform| platform as i32),
        transports: update
            .transports
            .iter()
            .map(|(t, on)| p::TransportUpdate {
                transport: *t as i32,
                enabled: Some(*on),
            })
            .collect(),
        configuration_interfaces: update
            .interfaces
            .iter()
            .map(InterfaceUpdate::wire)
            .collect(),
        ..Default::default()
    };
    let _pending = session.pending(pending, 0);
    set_adapter(session, request, pending)
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
    session
        .call(
            C::StartScan(p::StartScan {
                transports: transports.iter().map(|t| *t as i32).collect(),
                seconds,
            }),
            "scan start",
            true,
        )
        .map_err(|e| transport_refusal(session, &transports, e))?;
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

/// Resolves a candidate: its ID, else a name exactly one candidate has.
fn resolve_candidate(st: &State, target: &Target) -> Result<p::Candidate, Error> {
    let found = match target {
        Target::Id(id) => st.candidate(*id).cloned(),
        Target::Name(name) => {
            let mut matches = st.candidates.iter().filter(|c| c.name == *name);
            let first = matches.next().cloned();
            if first.is_some() && matches.next().is_some() {
                return Err(Error::new(
                    "more than one nearby device has that name; use its candidate ID",
                ));
            }
            first
        }
    };
    found.ok_or_else(|| {
        Error::new("put the device in pairing mode, scan, and pair its candidate ID or name")
    })
}

fn pair(session: &Session, target: &Target, options: &RunOptions) -> Result<Outcome, Error> {
    let mut st = session.state();
    if options.one_shot && resolve_candidate(&st, target).is_err() {
        // A one-shot pair finds its device with a scan of its own.
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
    let transport = candidate.transport();
    if model::transport_disabled(&st.status, transport) {
        return Err(Error::new(text::transport_disabled(transport)));
    }
    if model::transport(candidate.transport).is_none_or(|t| !model::supports(&st.status, t)) {
        return Err(refused(ErrorCode::Unsupported, "pair start"));
    }
    let _pending = session.pending("pair start", candidate.id);
    session
        .call(
            C::StartPairing(p::StartPairing {
                candidate: candidate.id,
            }),
            "pair start",
            true,
        )
        .map_err(|e| transport_refusal(session, &[transport], e))?;
    let ended = session.cell.wait_for(&options.wait, |st| {
        let p = st
            .pairing
            .as_ref()
            .filter(|p| p.candidate == candidate.id)?;
        match &p.step {
            Some(pairing::Step::Done(done)) => Some(Ok(done.device)),
            Some(pairing::Step::Failed(code)) => Some(Err(model::code(*code))),
            _ => None,
        }
    });
    let device = match ended {
        Ok(Ok(device)) => device,
        Ok(Err(code)) => {
            return Err(transport_refusal(
                session,
                &[transport],
                Error::code(code, Some("pair start")),
            ));
        }
        Err(error) => {
            let _ = session.connection.cancel_pairing();
            return Err(error);
        }
    };
    // The saved record follows the pairing's end as a device event.
    let record = session
        .cell
        .wait_for(&Wait::timeout(Duration::from_secs(2)), |st| {
            st.device(device).cloned()
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
            return Err(Error::new(
                "this pairing needs a code; use pair accept CODE",
            ));
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

/// Resolves a saved device: its ID, else a name exactly one saved device has.
pub(crate) fn resolve_device(st: &State, target: &Target) -> Result<p::Device, Error> {
    match target {
        Target::Id(id) => st
            .device(*id)
            .cloned()
            .ok_or_else(|| Error::new(format!("device {id} not found; use device list"))),
        Target::Name(name) => {
            let mut matches = st.devices.iter().filter(|d| d.name == *name);
            match (matches.next(), matches.next()) {
                (Some(d), None) => Ok(d.clone()),
                (Some(_), Some(_)) => Err(Error::new(format!(
                    "device name {} is ambiguous; use the device ID",
                    text::quote(name)
                ))),
                (None, _) => Err(Error::new(format!(
                    "device {} not found; use device list",
                    text::quote(name)
                ))),
            }
        }
    }
}

fn subject(d: &p::Device) -> Subject {
    Subject {
        id: d.id,
        name: d.name.clone(),
    }
}

fn device_result(r: p::Response) -> Result<p::Device, Error> {
    match r.result {
        Some(response::Result::Device(d)) => Ok(d),
        _ => Err(unexpected()),
    }
}

fn device_command(
    session: &Session,
    st: &State,
    command: &Command,
    d: p::Device,
) -> Result<Outcome, Error> {
    let id = d.id;
    let subject = subject(&d);
    match command {
        Command::Get(_) => {
            let device = device_result(session.call(
                C::GetDevice(p::GetDevice { device: id }),
                "device get",
                true,
            )?)?;
            // A device has warnings only while it has a link.
            if device.state() != p::DeviceState::Disconnected {
                list_warnings(session, id, false)?;
            }
            Ok(Outcome::Device { subject, device })
        }
        Command::Connect(_) => {
            if model::inactive(&d) == Some(p::InactiveReason::TransportDisabled) {
                return Err(Error::new(text::transport_disabled(d.transport())));
            }
            if d.blocked {
                return Err(refused(ErrorCode::Blocked, "device connect"));
            }
            if !d.enabled {
                return Err(refused(ErrorCode::Disabled, "device connect"));
            }
            if model::inactive(&d) == Some(p::InactiveReason::Capacity) {
                return Err(no_capacity(CapacityReason::Enabled, "device connect"));
            }
            let transport = d.transport();
            let _pending = session.pending("device connect", id);
            let device = device_result(
                session
                    .call(
                        C::ConnectDevice(p::ConnectDevice { device: id }),
                        "device connect",
                        true,
                    )
                    .map_err(|e| transport_refusal(session, &[transport], e))?,
            )?;
            Ok(Outcome::Device { subject, device })
        }
        Command::Disconnect(_) => {
            let _pending = session.pending("device disconnect", id);
            let device = device_result(session.call(
                C::DisconnectDevice(p::DisconnectDevice { device: id }),
                "device disconnect",
                true,
            )?)?;
            Ok(Outcome::Device { subject, device })
        }
        Command::Unpair(_) => {
            let _pending = session.pending("device unpair", id);
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
            let update = match toggle {
                Toggle::Enabled => DeviceUpdate {
                    enabled: Some(*on),
                    ..Default::default()
                },
                Toggle::Trusted => DeviceUpdate {
                    trusted: Some(*on),
                    ..Default::default()
                },
                Toggle::Blocked => DeviceUpdate {
                    blocked: Some(*on),
                    ..Default::default()
                },
                Toggle::Hidpp => DeviceUpdate {
                    hidpp: Some(*on),
                    ..Default::default()
                },
            };
            save_device(session, st, &d, subject, &update, pending_name(*toggle))
        }
        Command::Layers(_, targets) => {
            let layers = resolve_profiles(session, st, targets)?;
            let update = DeviceUpdate {
                layers: Some(layers),
                ..Default::default()
            };
            save_device(session, st, &d, subject, &update, "device set profiles")
        }
        Command::DeviceSave(_, update) => {
            // A change to HID++ is tracked as such, so the settings it starts or stops wait
            // for it.
            let name = if update.hidpp.is_some() {
                pending_name(Toggle::Hidpp)
            } else {
                "device set"
            };
            save_device(session, st, &d, subject, update, name)
        }
        Command::Warnings(_) => Ok(Outcome::Warnings {
            subject,
            warnings: list_warnings(session, id, true)?,
        }),
        Command::Settings(_) => {
            let _pending = session.pending("setting list", id);
            list_settings(session, id, true)?;
            Ok(Outcome::Settings(subject))
        }
        Command::SettingGet(_, key) => {
            let settings = list_settings(session, id, true)?;
            let setting = find(&settings, key, "setting get")?;
            Ok(Outcome::Setting { subject, setting })
        }
        Command::SettingSet(_, key, input) => {
            let settings = list_settings(session, id, false)?;
            let setting = find(&settings, key, "setting set")?;
            let value = catalog::setting_value(&setting, input).map_err(Error::new)?;
            save(session, &subject, vec![(setting, value)], Vec::new())
        }
        Command::SettingForget(_, key) => {
            let settings = list_settings(session, id, false)?;
            let setting = find(&settings, key, "setting forget")?;
            save(session, &subject, Vec::new(), vec![setting])
        }
        Command::SettingsSave { set, forget, .. } => {
            let known = st.settings_of(id);
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
            let features = read_pages::<p::FeatureList>(session, "feature list", true, |after| {
                C::ListFeatures(p::ListFeatures {
                    device: id,
                    after: after.copied(),
                })
            })?;
            Ok(Outcome::Features { subject, features })
        }
        _ => unreachable!(),
    }
}

/// Saves `update` for device `d` in one SetDevice request.
fn save_device(
    session: &Session,
    st: &State,
    d: &p::Device,
    subject: Subject,
    update: &DeviceUpdate,
    pending: &'static str,
) -> Result<Outcome, Error> {
    if update.enabled == Some(true) && !d.enabled && enabled_full(st, d) {
        return Err(no_capacity(CapacityReason::Enabled, "device set"));
    }
    if let Some(layers) = &update.layers {
        let Some(max) = profiles::max_layers(&st.status) else {
            return Err(Error::new(NO_PROFILES));
        };
        if layers.len() > max as usize {
            return Err(Error::new(format!(
                "a device can use at most {max} profiles"
            )));
        }
    }
    let request = p::SetDevice {
        device: d.id,
        enabled: update.enabled,
        trusted: update.trusted,
        blocked: update.blocked,
        integrations: update
            .hidpp
            .map(|on| p::IntegrationUpdate {
                kind: IntegrationKind::Hidpp as i32,
                enabled: Some(on),
            })
            .into_iter()
            .collect(),
        profiles: update
            .layers
            .clone()
            .map(|profiles| p::ProfileLayers { profiles }),
    };
    // A missing profile and a missing device are told apart by what the request carried.
    let name = if request.profiles.is_some() {
        "device set profiles"
    } else {
        "device set"
    };
    let _pending = session.pending(pending, d.id);
    session.call(C::SetDevice(request), name, true)?;
    // The view took the preferences sent once the adapter accepted them.
    let device = session
        .state()
        .device(d.id)
        .cloned()
        .unwrap_or_else(|| d.clone());
    Ok(Outcome::Device { subject, device })
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

fn list_settings(session: &Session, id: u32, reported: bool) -> Result<Vec<p::Setting>, Error> {
    read_pages::<p::DeviceSettings>(session, "setting list", reported, |after| {
        C::ListSettings(p::ListSettings {
            device: id,
            after: after.cloned(),
        })
    })
}

fn list_warnings(
    session: &Session,
    id: u32,
    reported: bool,
) -> Result<Vec<p::DeviceWarning>, Error> {
    read_pages::<p::DeviceWarnings>(session, "warning list", reported, |after| {
        C::ListWarnings(p::ListWarnings {
            device: id,
            after: after.copied(),
        })
    })
}

/// The setting with `key`. A key the catalog doesn't know is never shown, so it isn't found.
fn find(settings: &[p::Setting], key: &str, command: &'static str) -> Result<p::Setting, Error> {
    settings
        .iter()
        .find(|s| s.key == key && keys::lookup(&s.key).is_some())
        .cloned()
        .ok_or_else(|| refused(ErrorCode::NotFound, command))
}

/// Saves values and forgets saved values in one request.
fn save(
    session: &Session,
    subject: &Subject,
    set: Vec<(p::Setting, p::value::Value)>,
    forget: Vec<p::Setting>,
) -> Result<Outcome, Error> {
    let _pending = session.pending("setting set", subject.id);
    let set_keys: Vec<String> = set.iter().map(|(s, _)| s.key.clone()).collect();
    let forget_keys: Vec<String> = forget.iter().map(|s| s.key.clone()).collect();
    let name = if set.is_empty() {
        "setting forget"
    } else {
        "setting set"
    };
    let changes = set
        .into_iter()
        .map(|(s, value)| model::save_change(&s, value))
        .chain(forget.iter().map(model::forget_change))
        .collect();
    session.call(
        C::SetSettings(p::SetSettings {
            device: subject.id,
            changes,
        }),
        name,
        true,
    )?;
    // The view took the values sent once the adapter accepted them.
    let settings = session.state().settings_of(subject.id).to_vec();
    Ok(Outcome::Saved {
        subject: subject.clone(),
        set: set_keys,
        forget: forget_keys,
        settings,
    })
}
