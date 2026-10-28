//! Command workflows: checks the view can predict, the requests, and waiting for effects that
//! arrive as events.
use crate::{
    controller::{
        AdapterUpdate, Command, DeviceUpdate, Outcome, Pick, Progress, RunOptions, Session,
        Subject, Target, Toggle, Wait,
    },
    error::Error,
    model::{self, Prompt},
    profiles::{self, Deletion, InterfaceUpdate},
    records,
    settings::{self, Field, Setting},
    storage,
    ui::{catalog, text},
    view::State,
};
use cordial_protocol::{
    self as p, CodeKind, ErrorCode, ErrorReason, IntegrationKind, pairing, profile_list_entry,
    profile_rule_change, request::Command as C, response,
};
use std::time::Duration;

pub(crate) const STARTING: &str =
    "the adapter is still starting; status, restart, files and bootloader work meanwhile";

pub(crate) const NO_PROFILES: &str = "this adapter doesn't support profiles";

/// How a profile deletion's error starts once some of its changes took effect.
pub(crate) const DELETE_INCOMPLETE: &str = "the profile deletion is incomplete";

/// The longest adapter name, in UTF-8 bytes.
const NAME_BYTES: usize = 64;

/// The longest scan the adapter runs.
pub const MAX_SCAN_SECONDS: u32 = 60;

const FILE_CHUNK_BYTES: usize = p::STORAGE_MIGRATION_CHUNK_BYTES;
const UPLOAD_TEMP: &str = "/.file-upload";

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

fn no_capacity(reason: ErrorReason, command: &'static str) -> Error {
    Error::refusal(
        p::Error {
            code: ErrorCode::NoCapacity as i32,
            reason: reason as i32,
            ..Default::default()
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
        | Command::Profiles
        | Command::ProfileShow(_)
        | Command::ProfileLookup(_)
        | Command::ProfileCreate(_)
        | Command::ProfileCopy(..)
        | Command::ProfileDelete(_)
        | Command::ProfileDeletePlanned(_)
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
        Command::Files(_)
        | Command::FileGet { .. }
        | Command::FilePut { .. }
        | Command::FileDelete(_)
            if !development =>
        {
            Some("this adapter doesn't offer file access".into())
        }
        Command::Bootloader | Command::Features(_) if !development => {
            Some("this adapter doesn't offer development functions".into())
        }
        Command::Scan { transports, .. } => scan_transports(st, transports).err(),
        Command::GamepadOutput(_, mode)
        | Command::DeviceSave(
            _,
            DeviceUpdate {
                gamepad: Some(mode),
                ..
            },
        ) if *mode != p::OutputMode::Disabled
            && !model::gamepad_modes(&st.status).contains(mode) =>
        {
            Some(format!(
                "this adapter doesn't offer {} gamepad output",
                text::output_mode_name(*mode)
            ))
        }
        _ if uses_profiles(command) && !profiles::available(&st.status) => Some(NO_PROFILES.into()),
        _ => None,
    }
}

/// Runs `command`; a profile deletion reports its progress to `report` as it goes.
pub(crate) fn execute(
    session: &Session,
    command: &Command,
    options: &RunOptions,
    report: &dyn Fn(Progress),
) -> Result<Outcome, Error> {
    let st = session.state();
    if !st.available {
        return Err(Error::new(
            "the control session is unavailable; select an adapter",
        ));
    }
    // Until Cordial knows it may use the adapter, only its status is read and restart sent.
    if let Some(e) = crate::migration::refusal(&st.status, command) {
        return Err(e);
    }
    if !st.ready() && !command.direct() {
        return Err(Error::new(STARTING));
    }
    if let Some(reason) = unsupported(&st, command) {
        return Err(Error::new(reason));
    }
    // Concurrent uploads must not mix chunks in the shared scratch file, and other file
    // operations must not change it before an upload is verified and moved.
    let _files = match command {
        Command::Files(_)
        | Command::FileGet { .. }
        | Command::FilePut { .. }
        | Command::FileDelete(_) => Some(session.files.lock().unwrap()),
        _ => None,
    };
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
        Command::Restart => {
            session.call(C::Restart(p::Restart {}), "adapter restart", true)?;
            Ok(Outcome::Restart)
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
            session.call(C::ListDevices(p::ListDevices {}), "device list", true)?;
            Ok(Outcome::Devices)
        }
        Command::Files(path) => {
            let request = C::ListFiles(p::ListFiles { path: path.clone() });
            let Some(response::Result::Files(list)) =
                session.call(request, "file list", true)?.result
            else {
                return Err(unexpected());
            };
            Ok(Outcome::Files {
                path: path.clone(),
                entries: list.entries,
            })
        }
        Command::FileGet {
            path,
            local,
            overwrite,
            raw,
        } => {
            let expected = records::destination(path, local, *raw);
            storage::check_destination(&expected, *overwrite)?;
            let r = session.call(
                C::ReadFile(p::ReadFile {
                    path: path.clone(),
                    ..Default::default()
                }),
                "file get",
                true,
            )?;
            // A read without a length returns the whole file.
            let Some(response::Result::File(file)) = r.result else {
                return Err(unexpected());
            };
            if file.path != *path || file.offset != 0 || file.data.len() as u64 != file.size {
                return Err(unexpected());
            }
            let (local, data, unconverted) = if *raw {
                (local.clone(), file.data, None)
            } else {
                records::convert(path, local, file.data)
            };
            // Replacing was allowed for the expected destination only; a fallback to another
            // name never replaces a file.
            let bytes =
                storage::save(&local, *overwrite && local == expected, &data).map_err(|e| {
                    if local == expected {
                        e
                    } else {
                        // The pre-check was of another name, so this one is named here.
                        Error::new(format!(
                            "the file couldn't be converted, and saving its bytes to {} failed: {}",
                            text::safe(&local.display().to_string()),
                            e.message
                        ))
                    }
                })?;
            Ok(Outcome::FileSaved {
                path: path.clone(),
                local,
                bytes,
                json: !*raw && unconverted.is_none(),
                unconverted,
            })
        }
        Command::FilePut { local, path } => {
            file_path(path)?;
            let data = std::fs::read(local)
                .map_err(|e| Error::new(format!("couldn't read the local file: {e}")))?;
            u32::try_from(data.len())
                .map_err(|_| Error::new("the local file is too large for the adapter"))?;
            let staged = data.len() > FILE_CHUNK_BYTES;
            if staged && path == UPLOAD_TEMP {
                return Err(Error::new(
                    "the upload destination is also the temporary transfer path",
                ));
            }
            let destination = if staged { UPLOAD_TEMP } else { path };
            options.wait.check()?;
            if staged {
                check_upload_path(session).map_err(|e| {
                    Error::new(format!(
                        "couldn't prepare the adapter for upload: {}",
                        text::error_line(&e)
                    ))
                })?;
            }
            if data.is_empty() {
                write_file(session, destination, 0, &[])?;
            } else {
                for (index, chunk) in data.chunks(FILE_CHUNK_BYTES).enumerate() {
                    options.wait.check()?;
                    write_file(session, destination, index * FILE_CHUNK_BYTES, chunk)?;
                }
            }
            if staged {
                verify_file(session, destination, &data, &options.wait)?;
                options.wait.check()?;
                file_mutation(
                    session,
                    C::MoveFile(p::MoveFile {
                        source: UPLOAD_TEMP.into(),
                        destination: path.clone(),
                    }),
                    "file put",
                )?;
            }
            verify_file(session, path, &data, &options.wait).map_err(|e| {
                Error::new(format!(
                    "the adapter acknowledged the upload, but its contents couldn't be verified: {}",
                    text::error_line(&e)
                ))
            })?;
            Ok(Outcome::FileUploaded {
                path: path.clone(),
                bytes: data.len() as u64,
            })
        }
        Command::FileDelete(path) => {
            file_path(path)?;
            if path == "/format.pb" {
                return Err(Error::new(
                    "the adapter's storage format file cannot be deleted",
                ));
            }
            file_mutation(
                session,
                C::DeleteFile(p::DeleteFile { path: path.clone() }),
                "file delete",
            )?;
            Ok(Outcome::FileDeleted(path.clone()))
        }
        Command::Profiles => Ok(Outcome::Profiles(list_profiles(session, true)?)),
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
                Target::Id(id) => known_profile(&st, *id),
                Target::Name(_) => resolve_profile(session, &st, target)?,
            };
            delete_profile(
                session,
                &Deletion::of(&st, profile.id),
                target,
                &options.wait,
                report,
            )?;
            Ok(Outcome::ProfileDeleted(profile))
        }
        Command::ProfileDeletePlanned(plan) => {
            let profile = known_profile(&st, plan.profile);
            delete_profile(
                session,
                plan,
                &Target::Id(plan.profile),
                &options.wait,
                report,
            )?;
            Ok(Outcome::ProfileDeleted(profile))
        }
        Command::Rules(target) => {
            let profile = resolve_profile(session, &st, target)?;
            let request = C::ListProfileRules(p::ListProfileRules {
                profile: profile.id,
            });
            let Some(response::Result::ProfileRules(list)) =
                session.call(request, "profile rule list", true)?.result
            else {
                return Err(unexpected());
            };
            Ok(Outcome::Rules {
                profile,
                rules: list.rules,
            })
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
        | Command::GamepadOutput(target, _)
        | Command::Layers(target, _)
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
/// has one of them disabled. A refusal with more to report stays as it is.
fn transport_refusal(session: &Session, transports: &[p::Transport], error: Error) -> Error {
    let plain = error
        .dongle
        .as_ref()
        .is_some_and(|e| e.code() == ErrorCode::Unsupported && model::plain(e));
    if !plain {
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
    Error::new(crate::error::UNEXPECTED)
}

fn file_path(path: &str) -> Result<(), Error> {
    let internal = path.strip_prefix("/.file-write-").is_some_and(|suffix| {
        suffix.len() == 16
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if !path.starts_with('/')
        || path.len() > 192
        || path.ends_with('/')
        || path.contains("//")
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| part == "." || part == "..")
        || internal
    {
        return Err(Error::new("invalid adapter file path"));
    }
    Ok(())
}

fn file_mutation(session: &Session, command: C, name: &'static str) -> Result<(), Error> {
    match session.call(command, name, true)?.result {
        None => Ok(()),
        _ => Err(unexpected()),
    }
}

fn check_upload_path(session: &Session) -> Result<(), Error> {
    let response = session.call(
        C::ListFiles(p::ListFiles { path: "/".into() }),
        "file put",
        true,
    )?;
    let Some(response::Result::Files(list)) = response.result else {
        return Err(unexpected());
    };
    if list
        .entries
        .iter()
        .any(|entry| entry.name == UPLOAD_TEMP[1..])
    {
        return Err(Error::new(format!(
            "{UPLOAD_TEMP} already exists on the adapter"
        )));
    }
    Ok(())
}

fn write_file(session: &Session, path: &str, offset: usize, data: &[u8]) -> Result<(), Error> {
    file_mutation(
        session,
        C::WriteFile(p::WriteFile {
            path: path.into(),
            offset: offset as u64,
            data: data.into(),
        }),
        "file put",
    )
}

fn verify_file(session: &Session, path: &str, data: &[u8], wait: &Wait) -> Result<(), Error> {
    let mut offset = 0;
    loop {
        wait.check()?;
        let response = session.call(
            C::ReadFile(p::ReadFile {
                path: path.into(),
                offset: offset as u64,
                length: Some(FILE_CHUNK_BYTES as u32),
            }),
            "file put",
            true,
        )?;
        let end = (offset + FILE_CHUNK_BYTES).min(data.len());
        verify_chunk(response, path, offset, data.len(), &data[offset..end])?;
        if end == data.len() {
            return Ok(());
        }
        offset = end;
    }
}

fn verify_chunk(
    response: p::Response,
    path: &str,
    offset: usize,
    size: usize,
    expected: &[u8],
) -> Result<(), Error> {
    match response.result {
        Some(response::Result::File(file))
            if file.path == path
                && file.offset == offset as u64
                && file.size == size as u64
                && file.data == expected =>
        {
            Ok(())
        }
        _ => Err(unexpected()),
    }
}

fn list_profiles(session: &Session, reported: bool) -> Result<p::ProfileList, Error> {
    let request = C::ListProfiles(p::ListProfiles {});
    match session.call(request, "profile list", reported)?.result {
        Some(response::Result::Profiles(list)) => Ok(list),
        _ => Err(unexpected()),
    }
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

/// Resolves a profile: an ID, else a name exactly one profile has. Names are found by listing
/// the adapter's profiles.
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

/// Resolves several profiles in order, listing the profiles at most once for their names.
fn resolve_profiles(session: &Session, st: &State, targets: &[Target]) -> Result<Vec<u32>, Error> {
    let mut listed = None;
    targets
        .iter()
        .map(|target| match target {
            Target::Id(_) => resolve_profile(session, st, target).map(|p| p.id),
            Target::Name(name) => {
                if listed.is_none() {
                    listed = Some(all_profiles(session)?);
                }
                unique_name(listed.as_deref().unwrap_or_default(), name, target)
            }
        })
        .collect()
}

/// Every readable saved profile.
fn all_profiles(session: &Session) -> Result<Vec<p::Profile>, Error> {
    Ok(list_profiles(session, false)?
        .entries
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

/// A profile as the view names it, or its ID while its name isn't known.
fn known_profile(st: &State, id: u32) -> p::Profile {
    st.profile(id).cloned().unwrap_or_else(|| p::Profile {
        id,
        name: id.to_string(),
        ..Default::default()
    })
}

/// Makes `plan`'s changes in order, then deletes its profile. Each change is made only while its
/// target is still as the plan saw it, and nothing beyond the plan is changed. The first change
/// that can't be made stops the rest, and changes already made stay; the adapter refuses to
/// delete a profile still in use. Each change is reported once the adapter accepts it.
fn delete_profile(
    session: &Session,
    plan: &Deletion,
    target: &Target,
    wait: &Wait,
    report: &dyn Fn(Progress),
) -> Result<(), Error> {
    let _pending = session.pending("profile delete", plan.profile);
    let mut changed = false;
    for (i, detach) in plan.devices.iter().enumerate() {
        let st = session.state();
        let Some(d) = st
            .device(detach.device)
            .filter(|d| profiles::layers(d) == detach.layers.as_slice())
            .cloned()
        else {
            return Err(incomplete(
                changed,
                Error::new(format!(
                    "{}'s profiles changed before Cordial could update them",
                    detach.name
                )),
            ));
        };
        let update = DeviceUpdate {
            layers: Some(plan.kept(detach)),
            ..Default::default()
        };
        save_device(
            session,
            &st,
            &d,
            subject(&d),
            &update,
            "device set profiles",
        )
        .map_err(|e| incomplete(changed, e))?;
        changed = true;
        report(Progress {
            done: i + 1,
            waiting: false,
        });
    }
    let mut reconnect = None;
    if !plan.interfaces.is_empty() {
        let st = session.state();
        if let Some(u) = plan.interfaces.iter().find(|u| {
            profiles::interface(&st.status, u.interface)
                .is_none_or(|s| s.profile != plan.profile || s.enabled != u.enabled)
        }) {
            return Err(incomplete(
                changed,
                Error::new(format!(
                    "{} changed before Cordial could update it",
                    profiles::interface_label(u.interface)
                )),
            ));
        }
        let update = AdapterUpdate {
            interfaces: plan.interface_updates(),
            ..Default::default()
        };
        let restarts = plan
            .interfaces
            .iter()
            .any(|u| profiles::interface(&st.status, u.interface).is_some_and(|s| s.active));
        if restarts {
            reconnect = Some(session.profile_reconnect(plan.profile));
        }
        save_adapter(session, &st, &update, "adapter interface")
            .map_err(|e| incomplete(changed, e))?;
        changed = true;
        let done = plan.devices.len() + plan.interfaces.len();
        report(Progress {
            done,
            waiting: restarts,
        });
        if let Some(reconnect) = &mut reconnect {
            reconnect
                .reconnect(wait)
                .map_err(|e| incomplete(changed, e))?;
            report(Progress {
                done,
                waiting: false,
            });
        } else {
            session
                .call(C::GetStatus(p::GetStatus {}), "adapter status", true)
                .map_err(|e| incomplete(changed, e))?;
        }
        if plan.interfaces.iter().any(|u| {
            profiles::interface(&session.state().status, u.interface)
                .is_none_or(|s| s.enabled || s.active || s.profile != 0)
        }) {
            return Err(incomplete(
                changed,
                Error::new(
                    "the adapter's interface settings changed before the profile could be deleted",
                ),
            ));
        }
    }
    if let Some(reason) = profiles::in_use(&session.state(), plan.profile) {
        return Err(incomplete(changed, Error::new(reason)));
    }
    wait.check().map_err(|e| incomplete(changed, e))?;
    session
        .call(
            C::DeleteProfile(p::DeleteProfile {
                profile: plan.profile,
            }),
            "profile delete",
            true,
        )
        .map_err(|e| incomplete(changed, missing(e, target)))?;
    drop(reconnect);
    Ok(())
}

/// A profile deletion's failure, marked incomplete once some of its changes took effect. The
/// adapter's error stays as it is.
fn incomplete(changed: bool, error: Error) -> Error {
    if !changed {
        return error;
    }
    Error {
        message: format!("{DELETE_INCOMPLETE}: {}", error.message),
        ..error
    }
}

/// A profile deletion's failure as its cause, and whether some of its changes took effect.
pub(crate) fn deletion_failure(error: &Error) -> (Error, bool) {
    match error
        .message
        .strip_prefix(DELETE_INCOMPLETE)
        .and_then(|m| m.strip_prefix(": "))
    {
        Some(cause) => (
            Error {
                message: cause.to_owned(),
                ..error.clone()
            },
            true,
        ),
        None => (error.clone(), false),
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

/// Starts a scan and returns its transports.
fn start_scan(
    session: &Session,
    st: &State,
    named: &[p::Transport],
    seconds: u32,
) -> Result<Vec<p::Transport>, Error> {
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
    Ok(transports)
}

fn scan(
    session: &Session,
    st: &State,
    named: &[p::Transport],
    seconds: u32,
    options: &RunOptions,
) -> Result<Outcome, Error> {
    let transports = start_scan(session, st, named, seconds)?;
    if !options.one_shot {
        return Ok(Outcome::ScanStarted(transports));
    }
    let done = session.cell.wait_for(&options.wait, |st| {
        st.scanning.is_none().then(|| st.last_scan.clone())
    });
    match done {
        Ok(done) => {
            let done = done.unwrap_or_default();
            Ok(Outcome::ScanFinished {
                count: done.count,
                error: done.error,
            })
        }
        Err(error) => {
            let _ = session.connection().stop_scan();
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
        // A one-shot pair finds its device with a scan of its own, which ends once it has.
        start_scan(session, &st, &[], 0)?;
        let found = session.cell.wait_for(&options.wait, |st| {
            (st.scanning.is_none() || resolve_candidate(st, target).is_ok()).then_some(())
        });
        let stopped = match session.state().scanning {
            Some(_) => session
                .call(C::StopScan(p::StopScan {}), "scan stop", true)
                .map(drop),
            None => Ok(()),
        };
        found?;
        stopped?;
        st = session.state();
    }
    let candidate = resolve_candidate(&st, target)?;
    if model::storage_full(&st.status) {
        return Err(no_capacity(ErrorReason::Storage, "pair start"));
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
        if st.paired_candidate == Some(candidate.id) {
            return Some(Ok(None));
        }
        let p = st
            .pairing
            .as_ref()
            .filter(|p| p.candidate == candidate.id)?;
        match &p.step {
            Some(pairing::Step::Done(done)) => Some(Ok(Some(done.device))),
            Some(pairing::Step::Error(e)) => Some(Err(e.clone())),
            _ => None,
        }
    });
    let device = match ended {
        Ok(Ok(device)) => device,
        Ok(Err(e)) => {
            return Err(transport_refusal(
                session,
                &[transport],
                Error::dongle(e, Some("pair start")),
            ));
        }
        Err(error) => {
            let _ = session.connection().cancel_pairing();
            return Err(error);
        }
    };
    let Some(device) = device else {
        return Ok(Outcome::PairedWithoutDevice {
            name: candidate.name.clone(),
        });
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
                return Err(no_capacity(ErrorReason::EnabledDevices, "device connect"));
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
        Command::GamepadOutput(_, mode) => {
            let update = DeviceUpdate {
                gamepad: Some(*mode),
                ..Default::default()
            };
            save_device(
                session,
                st,
                &d,
                subject,
                &update,
                "device set gamepad-output",
            )
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
        Command::Settings(_) => {
            let _pending = session.pending("setting list", id);
            list_settings(session, id, true)?;
            Ok(Outcome::Settings(subject))
        }
        Command::SettingGet(_, field) => {
            let settings = list_settings(session, id, true)?;
            let setting = find(&settings, field, "setting get")?;
            Ok(Outcome::Setting { subject, setting })
        }
        Command::SettingSet(_, field, input) => {
            let settings = list_settings(session, id, false)?;
            let setting = find(&settings, field, "setting set")?;
            let value = catalog::setting_value(&setting, input).map_err(Error::new)?;
            save(session, &subject, vec![(setting.field, value)], Vec::new())
        }
        Command::SettingForget(_, field) => {
            let settings = list_settings(session, id, false)?;
            let setting = find(&settings, field, "setting forget")?;
            save(session, &subject, Vec::new(), vec![setting.field])
        }
        Command::SettingsSave { set, forget, .. } => {
            let known = st.settings_of(id);
            for (field, value) in set {
                let setting = find(&known, field, "setting set")?;
                if !settings::accepts(&setting, value) {
                    return Err(refused(ErrorCode::BadArgs, "setting set"));
                }
            }
            for field in forget {
                find(&known, field, "setting forget")?;
            }
            save(session, &subject, set.clone(), forget.clone())
        }
        Command::Features(_) => {
            let request = C::ListFeatures(p::ListFeatures { device: id });
            let Some(response::Result::Features(list)) =
                session.call(request, "feature list", true)?.result
            else {
                return Err(unexpected());
            };
            Ok(Outcome::Features {
                subject,
                features: list.features,
            })
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
    let enabled = update.enabled.unwrap_or(d.enabled);
    let blocked = update.blocked.unwrap_or(d.blocked);
    // The adapter takes an update that changes nothing as done, whatever room it has.
    let changes = enabled != d.enabled
        || blocked != d.blocked
        || update.trusted.is_some_and(|v| v != d.trusted)
        || update.hidpp.is_some_and(|v| v != model::hidpp_enabled(d))
        || update
            .gamepad
            .is_some_and(|m| Some(m) != model::gamepad_mode(d))
        || update
            .layers
            .as_ref()
            .is_some_and(|l| l.as_slice() != profiles::layers(d));
    if changes && capacity_refused(st, d, enabled, blocked) {
        return Err(no_capacity(ErrorReason::EnabledDevices, "device set"));
    }
    if let Some(layers) = &update.layers {
        if !profiles::available(&st.status) {
            return Err(Error::new(NO_PROFILES));
        }
        if let Some(max) = profiles::max_layers(&st.status)
            && layers.len() > max as usize
        {
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
        output_preferences: update
            .gamepad
            .map(|mode| p::OutputPreference {
                role: p::Role::Gamepad as i32,
                mode: mode as i32,
            })
            .into_iter()
            .collect(),
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

/// Whether the adapter would refuse a device's preferences for lack of room, as it decides: the
/// device isn't in use now, the preferences make it usable on a transport the adapter has enabled,
/// and that transport has every place in use.
pub fn capacity_refused(st: &State, d: &p::Device, enabled: bool, blocked: bool) -> bool {
    let usable = model::transport(d.transport).is_some_and(|t| {
        model::supports(&st.status, t) && !model::transport_disabled(&st.status, t)
    });
    d.inactive.is_some() && enabled && !blocked && usable && enabled_full(st, d)
}

/// Whether the device's transport already has as many devices in use as the adapter allows.
/// A device is in use when the adapter reports no reason it is inactive.
pub fn enabled_full(st: &State, d: &p::Device) -> bool {
    let Some(transport) = model::transport(d.transport) else {
        return false;
    };
    let Some(max) = model::enabled_devices(&st.status, transport).and_then(|r| r.limit) else {
        return false;
    };
    let enabled = st
        .devices
        .iter()
        .filter(|o| o.id != d.id && o.inactive.is_none() && o.transport == d.transport)
        .count();
    enabled >= max as usize
}

/// Every setting of a device.
fn list_settings(session: &Session, id: u32, reported: bool) -> Result<Vec<Setting>, Error> {
    let request = C::ListSettings(p::ListSettings { device: id });
    match session.call(request, "setting list", reported)?.result {
        Some(response::Result::Settings(list)) => Ok(settings::settings(&list.objects)),
        _ => Err(unexpected()),
    }
}

/// The setting of `field`.
fn find(settings: &[Setting], field: &Field, command: &'static str) -> Result<Setting, Error> {
    settings
        .iter()
        .find(|s| s.field == *field)
        .cloned()
        .ok_or_else(|| refused(ErrorCode::NotFound, command))
}

/// Saves values and forgets saved values in one request.
fn save(
    session: &Session,
    subject: &Subject,
    set: Vec<(Field, settings::Value)>,
    forget: Vec<Field>,
) -> Result<Outcome, Error> {
    let _pending = session.pending("setting set", subject.id);
    let name = if set.is_empty() {
        "setting forget"
    } else {
        "setting set"
    };
    let changes = set
        .iter()
        .map(|(field, value)| settings::update(*field, Some(*value)))
        .chain(forget.iter().map(|field| settings::update(*field, None)))
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
    Ok(Outcome::Saved {
        subject: subject.clone(),
        set: set.into_iter().map(|(field, _)| field).collect(),
        forget,
        settings: session.state().settings_of(subject.id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_deletion_steps_preserve_transport_uncertainty_and_the_failing_command() {
        use cordial_client::Error as E;
        for cause in [
            E::Timeout,
            E::Closed,
            E::Protocol,
            E::UnexpectedResponse,
            E::Io(std::sync::Arc::new(std::io::Error::other(
                "connection failed",
            ))),
        ] {
            let error = incomplete(true, Error::from_client(cause, "profile delete"));
            assert!(error.unconfirmed(), "{error}");
            assert_eq!(error.command, Some("profile delete"));
            assert!(deletion_failure(&error).1);
        }
        let error = incomplete(
            true,
            Error::code(p::ErrorCode::InUse, Some("profile delete")),
        );
        assert!(!error.unconfirmed());
    }

    #[test]
    fn upload_readback_requires_matching_metadata_length_and_bytes() {
        let file = p::FileData {
            path: "/data.bin".into(),
            offset: 512,
            size: 515,
            data: vec![1, 2, 3],
        };
        let check = |file| {
            verify_chunk(
                p::Response {
                    result: Some(response::Result::File(file)),
                },
                "/data.bin",
                512,
                515,
                &[1, 2, 3],
            )
        };
        check(file.clone()).unwrap();
        for invalid in [
            p::FileData {
                path: "/other.bin".into(),
                ..file.clone()
            },
            p::FileData {
                offset: 0,
                ..file.clone()
            },
            p::FileData {
                size: 516,
                ..file.clone()
            },
            p::FileData {
                data: vec![1, 2],
                ..file.clone()
            },
            p::FileData {
                data: vec![1, 2, 3, 4],
                ..file.clone()
            },
            p::FileData {
                data: vec![1, 2, 4],
                ..file.clone()
            },
        ] {
            let error = check(invalid).unwrap_err();
            assert_eq!(error.message, crate::error::UNEXPECTED);
            assert!(error.unconfirmed());
        }
        assert!(verify_chunk(p::Response::default(), "/data.bin", 0, 0, &[]).is_err());
    }

    #[test]
    fn adapter_file_paths_use_the_protocol_byte_limit_and_reserved_names() {
        for valid in [
            "/a",
            "/devices/1/device.pb",
            "/.file-upload",
            "/.file-write-not-internal",
        ] {
            file_path(valid).unwrap();
        }
        file_path(&format!("/{}", "a".repeat(191))).unwrap();
        assert!(file_path(&format!("/{}", "a".repeat(192))).is_err());
        for invalid in [
            "/a\\b",
            "/a\n",
            "/./a",
            "/a/..",
            "",
            "/.file-write-0123456789abcdef",
        ] {
            assert!(file_path(invalid).is_err(), "{invalid:?}");
        }
    }
}
