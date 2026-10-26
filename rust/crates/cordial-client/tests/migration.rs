use cordial_client::{
    Error as ClientError,
    migration::{
        self, AdapterPort, Change, Checkpoint, Discovery, Edge, Format, Graph, Outcome, Snapshot,
        Transformation, Transport,
    },
};
use cordial_protocol::{
    self as p, migration as ipc,
    request::Command,
    response,
    storage::{self as saved, v1},
};
use prost::Message;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const ID: &str = "fixture-adapter";
type Files = BTreeMap<String, Vec<u8>>;

fn fixture() -> Files {
    let peer = v1::Peer {
        address: vec![2; 6],
        transport: v1::Transport::Classic.into(),
        ..Default::default()
    };
    let mut files = Files::new();
    files.insert(
        "/format.pb".into(),
        v1::Format {
            format: 1,
            initialized: true,
        }
        .encode_to_vec(),
    );
    files.insert(
        "/identity.pb".into(),
        v1::Identity {
            address: vec![1; 6],
            ir: vec![0; 16],
            er: vec![3; 16],
            irk: vec![
                0x58, 0xe2, 0xfc, 0xce, 0xfa, 0x7e, 0x30, 0x61, 0x36, 0x7f, 0x1d, 0x57, 0xa4, 0xe7,
                0x45, 0x5a,
            ],
        }
        .encode_to_vec(),
    );
    files.insert(
        "/sequence.pb".into(),
        v1::Sequence {
            next_device: 2,
            next_profile: 2,
        }
        .encode_to_vec(),
    );
    files.insert(
        "/adapter.pb".into(),
        v1::Adapter {
            name: Some("Fixture".into()),
            ..Default::default()
        }
        .encode_to_vec(),
    );
    files.insert(
        "/devices/1/device.pb".into(),
        v1::Device {
            policy: Some(v1::Policy {
                id: 1,
                peer: Some(peer.clone()),
                name: "Keyboard".into(),
                enabled: true,
                ..Default::default()
            }),
            bond: Some(v1::Bond {
                owner: 1,
                identity: Some(peer),
                keys: Some(v1::bond::Keys::Classic(v1::ClassicKeys {
                    link_key: vec![7; 16],
                    key_type: 4,
                })),
            }),
        }
        .encode_to_vec(),
    );
    files.insert(
        "/devices/1/settings.pb".into(),
        v1::Settings {
            preferences: vec![v1::Preference {
                integration: v1::Integration::Hidpp.into(),
                value: 5,
                metadata: Some(v1::SettingMetadata {
                    key: "backlight.level".into(),
                    feature: 0x1982,
                    revision: 3,
                    scope: v1::SettingScope::Device.into(),
                    range: Some(v1::Range {
                        min: 0,
                        max: 7,
                        step: 1,
                    }),
                    ..Default::default()
                }),
            }],
        }
        .encode_to_vec(),
    );
    files.insert(
        "/devices/1/layout.pb".into(),
        v1::Layout {
            maps: vec![vec![1]],
            ..Default::default()
        }
        .encode_to_vec(),
    );
    files.insert(
        "/profiles/1/profile.pb".into(),
        v1::Profile {
            name: "Work".into(),
            roles: vec![v1::Role::Keyboard.into()],
        }
        .encode_to_vec(),
    );
    files.insert(
        "/profiles/1/rules.pb".into(),
        v1::Rules {
            rules: vec![v1::Rule {
                input: cordial_core::profiles::usage(7, 4),
                collection: cordial_core::profiles::KEYBOARD,
                effect: Some(v1::rule::Effect::Remap(v1::Remap {
                    outputs: vec![v1::Output {
                        usage: cordial_core::profiles::usage(7, 5),
                        collection: cordial_core::profiles::KEYBOARD,
                    }],
                })),
            }],
        }
        .encode_to_vec(),
    );
    files.insert("/devices/2/settings.pb".into(), vec![0xff]); // Inactive child.
    files.insert("/extension/nested/opaque".into(), vec![0xa5; 1200]);
    files.insert("/extension/empty".into(), vec![]);
    files.insert("/.file-upload".into(), vec![0xff; 700]);
    files
}
fn current(mut files: Files) -> Files {
    files.remove("/.file-upload");
    files.remove("/devices/2/settings.pb");
    for (path, bytes) in &mut files {
        if path == "/format.pb" {
            *bytes = cordial_core::migration::convert_root(bytes).unwrap();
        } else if !matches!(
            cordial_core::migration::classify_path(path, cordial_core::storage::FileType::File),
            Ok(cordial_core::migration::Path::Unknown)
        ) {
            *bytes = cordial_core::migration::normalize_path(1, path, bytes)
                .unwrap_or_else(|e| panic!("{path}: {e:?}"))
                .bytes;
        }
    }
    files
}
const STORAGE_FAILED: &str = "the adapter couldn't read or write its saved data";
fn assisted_error() -> p::Error {
    p::Error {
        code: p::ErrorCode::StorageMigrationRequired.into(),
        reason: p::ErrorReason::Storage.into(),
        outcome: p::ErrorOutcome::NoEffect.into(),
        ..Default::default()
    }
}
fn storage_failure() -> p::Error {
    p::Error {
        code: p::ErrorCode::StorageFailed.into(),
        reason: p::ErrorReason::Storage.into(),
        outcome: p::ErrorOutcome::NoEffect.into(),
        ..Default::default()
    }
}
fn status(target: u32, current: Option<u32>, ready: bool) -> p::Status {
    p::Status {
        id: ID.into(),
        ready,
        information: Some(p::AdapterInformation {
            storage_format: current,
            target_storage_format: Some(target),
            ..Default::default()
        }),
        errors: if current.is_none() {
            vec![storage_failure()]
        } else if current != Some(target) && !ready {
            vec![assisted_error()]
        } else {
            vec![]
        },
        ..Default::default()
    }
}
#[derive(Clone)]
struct Fault {
    key: String,
    after: bool,
    reboot: bool,
    finish_on_device: bool,
}
struct State {
    files: Files,
    target: u32,
    assisted: bool,
    ready: bool,
    requests: Vec<String>,
    mutations: Vec<Files>,
    faults: VecDeque<Fault>,
    fault_min_mutations: usize,
    cancel_at: Option<usize>,
    cancel: Arc<AtomicBool>,
    handles: usize,
    opened: Vec<String>,
    incompatible_on_open: Option<usize>,
    ports: Vec<AdapterPort>,
    status_id: String,
    bootstrap: VecDeque<p::Status>,
    listing: Option<Vec<p::FileEntry>>,
    corrupt_read: Option<&'static str>,
    corrupt_read_min_mutations: usize,
    waits: usize,
    cancel_after_waits: Option<usize>,
    cleanup_scratch_on_reopen: bool,
    not_found: Option<p::Error>,
}
impl State {
    fn new(files: Files) -> Self {
        let mut state = Self {
            files,
            target: 2,
            assisted: false,
            ready: false,
            requests: vec![],
            mutations: vec![],
            faults: VecDeque::new(),
            fault_min_mutations: 0,
            cancel_at: None,
            cancel: Arc::new(AtomicBool::new(false)),
            handles: 0,
            opened: vec![],
            incompatible_on_open: None,
            ports: vec![AdapterPort {
                port: "fixture-port".into(),
                adapter_id: ID.into(),
            }],
            status_id: ID.into(),
            bootstrap: VecDeque::new(),
            listing: None,
            corrupt_read: None,
            corrupt_read_min_mutations: 0,
            waits: 0,
            cancel_after_waits: None,
            cleanup_scratch_on_reopen: false,
            not_found: None,
        };
        state.startup();
        state
    }
    fn current(&self) -> Option<u32> {
        let root = self.files.get("/format.pb")?;
        if (self.target == 5 || self.target == 10)
            && let [format] = root.as_slice()
        {
            return Some(u32::from(*format));
        }
        match cordial_core::migration::classify_root(Some(root)).ok()? {
            cordial_core::migration::Root::Legacy { .. } => Some(1),
            cordial_core::migration::Root::Current { .. } => Some(2),
            cordial_core::migration::Root::Unsupported { format, .. } => Some(format),
            cordial_core::migration::Root::Missing => None,
        }
    }
    fn startup(&mut self) {
        let current = self.current();
        self.assisted = current.is_some_and(|format| format != self.target);
        self.ready = current == Some(self.target);
        if self.assisted || self.ready {
            self.files.remove("/.file-upload");
        }
    }
    fn status(&self) -> p::Status {
        let mut status = status(self.target, self.current(), self.ready);
        if self.assisted && self.current().is_some() {
            status.errors = vec![assisted_error()];
        }
        status
    }
}
struct Fake(Arc<Mutex<State>>);
struct Session {
    state: Arc<Mutex<State>>,
    closed: bool,
}
impl Discovery for Fake {
    type Connection = Session;
    fn discover(&mut self) -> std::io::Result<Vec<AdapterPort>> {
        Ok(self.0.lock().unwrap().ports.clone())
    }
    fn open(&mut self, port: &str) -> cordial_client::Result<Session> {
        let mut state = self.0.lock().unwrap();
        assert_eq!(
            state.handles, 0,
            "port opened before old handles were released"
        );
        state.handles += 1;
        if state.cleanup_scratch_on_reopen && !state.opened.is_empty() {
            state.files.remove("/.file-upload");
        }
        state.opened.push(port.into());
        Ok(Session {
            state: self.0.clone(),
            closed: false,
        })
    }
    fn wait(&mut self, cancelled: &AtomicBool) {
        let mut state = self.0.lock().unwrap();
        state.waits += 1;
        if state.cancel_after_waits.is_some_and(|n| state.waits >= n) {
            cancelled.store(true, Ordering::Release);
        }
    }
}
fn key(command: &Command) -> String {
    match command {
        Command::GetStatus(_) => "status".into(),
        Command::ListFiles(v) => format!("list:{}", v.path),
        Command::ReadFile(v) => format!("read:{}:{}", v.path, v.offset),
        Command::WriteFile(v) => format!("write:{}:{}", v.path, v.offset),
        Command::MoveFile(v) => format!("move:{}", v.destination),
        Command::DeleteFile(v) => format!("delete:{}", v.path),
        Command::Restart(_) => "restart".into(),
        _ => panic!("unexpected migration command"),
    }
}
impl Transport for Session {
    fn close(&mut self) {
        if !self.closed {
            self.state.lock().unwrap().handles -= 1;
            self.closed = true;
        }
    }
    fn request(&mut self, command: Command, _: &AtomicBool) -> cordial_client::Result<p::Response> {
        let mut s = self.state.lock().unwrap();
        if s.incompatible_on_open == Some(s.opened.len()) {
            let hello = p::AdapterHello {
                protocol_version: Some(p::CURRENT_PROTOCOL_VERSION + 1),
                ..Default::default()
            };
            return Err(ClientError::Incompatible(
                cordial_client::compatibility::NativeHello::decode(&hello.encode_to_vec()).unwrap(),
            ));
        }
        let key = key(&command);
        assert!(
            !s.cancel.load(Ordering::Acquire),
            "command issued after cancellation"
        );
        if key != "status" && key != "restart" {
            assert!(s.assisted, "production file access is disabled: {key}");
        }
        s.requests.push(key.clone());
        let fault = if s.mutations.len() >= s.fault_min_mutations
            && s.faults.front().is_some_and(|f| f.key == key)
        {
            s.faults.pop_front()
        } else {
            None
        };
        if fault.as_ref().is_some_and(|f| !f.after) {
            return Err(ClientError::Closed);
        }
        let result = match command {
            Command::GetStatus(_) => {
                let mut status = s.bootstrap.pop_front().unwrap_or_else(|| s.status());
                status.id = s.status_id.clone();
                s.assisted = status.errors.iter().any(|e| *e == assisted_error());
                s.ready = status.ready;
                Some(response::Result::Status(status))
            }
            Command::ListFiles(v) => {
                let entries = if let Some(entries) = &s.listing {
                    entries.clone()
                } else {
                    let prefix = format!("{}/", v.path.trim_end_matches('/'));
                    let mut entries = BTreeMap::new();
                    for (path, bytes) in &s.files {
                        if path == "/.file-upload" {
                            continue;
                        }
                        if let Some(rest) = path.strip_prefix(&prefix) {
                            let (name, directory, size) = match rest.split_once('/') {
                                Some((n, _)) => (n, true, 0),
                                None => (rest, false, bytes.len()),
                            };
                            entries.insert(
                                name.to_string(),
                                p::FileEntry {
                                    name: name.into(),
                                    directory,
                                    size: size as u64,
                                },
                            );
                        }
                    }
                    entries.into_values().collect()
                };
                Some(response::Result::Files(p::FileList { entries }))
            }
            Command::ReadFile(v) => {
                assert!((1..=512).contains(&v.length.unwrap()));
                match s.files.get(&v.path) {
                    None => Some(response::Result::Error(s.not_found.clone().unwrap_or(
                        p::Error {
                            code: p::ErrorCode::NotFound.into(),
                            outcome: p::ErrorOutcome::NoEffect.into(),
                            ..Default::default()
                        },
                    ))),
                    Some(bytes) => {
                        let start = usize::try_from(v.offset).unwrap();
                        assert!(start <= bytes.len());
                        let end = bytes.len().min(start + v.length.unwrap() as usize);
                        let mut data = p::FileData {
                            path: v.path,
                            offset: v.offset,
                            size: bytes.len() as u64,
                            data: bytes[start..end].into(),
                        };
                        match if s.mutations.len() >= s.corrupt_read_min_mutations {
                            s.corrupt_read
                        } else {
                            None
                        } {
                            Some("path") => data.path.push('x'),
                            Some("offset") => data.offset += 1,
                            Some("size") => data.size += 1,
                            Some("short") if !data.data.is_empty() => {
                                data.data.pop();
                            }
                            Some("long") => data.data.push(0),
                            _ => {}
                        }
                        Some(response::Result::File(data))
                    }
                }
            }
            Command::WriteFile(v) => {
                assert!(v.data.len() <= 512);
                if v.offset == 0 {
                    s.files.insert(v.path, v.data);
                } else {
                    let file = s.files.get_mut(&v.path).unwrap();
                    let start = v.offset as usize;
                    assert!(start <= file.len());
                    file.resize(file.len().max(start + v.data.len()), 0);
                    file[start..start + v.data.len()].copy_from_slice(&v.data);
                }
                None
            }
            Command::MoveFile(v) => {
                let bytes = s.files.remove(&v.source).unwrap();
                s.files.insert(v.destination, bytes);
                None
            }
            Command::DeleteFile(v) => {
                assert_ne!(v.path, "/format.pb");
                s.files.remove(&v.path);
                None
            }
            Command::Restart(_) => {
                s.startup();
                None
            }
            _ => unreachable!(),
        };
        if key.starts_with("write:") || key.starts_with("move:") || key.starts_with("delete:") {
            let files = s.files.clone();
            s.mutations.push(files);
            if s.cancel_at == Some(s.mutations.len()) {
                s.cancel.store(true, Ordering::Release);
            }
        }
        if let Some(fault) = fault {
            if fault.finish_on_device {
                s.files = current(s.files.clone());
            }
            if fault.reboot || fault.finish_on_device {
                s.startup();
            }
            return Err(ClientError::Closed);
        }
        Ok(p::Response { result })
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}
fn run(
    state: &Arc<Mutex<State>>,
    graph: &Graph,
) -> (migration::Result<Outcome>, Vec<ipc::MigrationEvent>) {
    let cancelled = state.lock().unwrap().cancel.clone();
    let mut events = vec![];
    let result = migration::migrate_with(
        &mut Fake(state.clone()),
        graph,
        ID,
        Some("wrong-hint"),
        &cancelled,
        |e| events.push(e),
    );
    assert_eq!(state.lock().unwrap().handles, 0);
    (result, events)
}
fn standard(
    state: State,
) -> (
    Arc<Mutex<State>>,
    migration::Result<Outcome>,
    Vec<ipc::MigrationEvent>,
) {
    let state = Arc::new(Mutex::new(state));
    let (result, events) = run(&state, &Graph::standard());
    (state, result, events)
}

#[test]
fn full_conversion_preserves_unknowns_and_commits_root_last() {
    let mut original = fixture();
    original.insert("/devices/1/extension/data".into(), vec![0xff; 700]);
    original.insert("/profiles/1/extension/data".into(), vec![0xfe; 700]);
    let (state, result, events) = standard(State::new(original.clone()));
    assert_eq!(result.unwrap(), Outcome::Success);
    let s = state.lock().unwrap();
    assert_eq!(s.files, current(original));
    assert_eq!(
        s.files["/sequence.pb"],
        saved::Sequence {
            format: 2,
            next_device: 2,
            next_profile: 2
        }
        .encode_to_vec()
    );
    let writes: Vec<_> = s
        .requests
        .iter()
        .filter(|s| s.starts_with("write:") || s.starts_with("move:") || s.starts_with("delete:"))
        .collect();
    assert_eq!(writes.last().unwrap().as_str(), "write:/format.pb:0");
    assert!(matches!(
        events.last().unwrap().event,
        Some(ipc::migration_event::Event::Success(_))
    ));
}

#[test]
fn every_durable_prefix_recovers_without_the_original_host_snapshot() {
    let original = fixture();
    let (complete, result, _) = standard(State::new(original.clone()));
    result.unwrap();
    let count = complete.lock().unwrap().mutations.len();
    for prefix in 1..=count {
        let mut state = State::new(original.clone());
        state.cancel_at = Some(prefix);
        let (state, result, events) = standard(state);
        assert_eq!(result.unwrap(), Outcome::Cancelled);
        assert!(matches!(
            events.last().unwrap().event,
            Some(ipc::migration_event::Event::Cancelled(_))
        ));
        let durable = state.lock().unwrap().files.clone();
        drop(state);
        let (state, result, _) = standard(State::new(durable));
        assert_eq!(result.unwrap(), Outcome::Success, "prefix {prefix}");
        assert_eq!(
            state.lock().unwrap().files,
            current(original.clone()),
            "prefix {prefix}"
        );
    }
}

#[test]
fn unsupported_targets_and_changed_targets_are_bootstrap_only() {
    for target in [0, 1, 3, 50] {
        let mut state = State::new(fixture());
        state.target = target;
        state.startup();
        let (state, result, events) = standard(state);
        assert!(result.unwrap_err().reason.contains("updated client"));
        assert_eq!(state.lock().unwrap().requests, ["status"]);
        assert!(matches!(
            events.last().unwrap().event,
            Some(ipc::migration_event::Event::Failure(_))
        ));
    }
    let mut state = State::new(fixture());
    state.faults.push_back(Fault {
        key: "read:/adapter.pb:0".into(),
        after: false,
        reboot: false,
        finish_on_device: false,
    });
    state
        .bootstrap
        .extend([status(2, Some(1), false), status(7, Some(1), false)]);
    let (state, result, _) = standard(state);
    assert!(result.is_err());
    assert!(state.lock().unwrap().mutations.is_empty());
    assert_eq!(state.lock().unwrap().requests.last().unwrap(), "status");
}

#[test]
fn absent_target_rejects_present_information_before_inventory() {
    for current in [None, Some(1), Some(2)] {
        for ready in [false, true] {
            for errors in [vec![], vec![assisted_error()]] {
                let original = without_scratch(fixture());
                let mut state = State::new(original.clone());
                let mut bootstrap = status(2, current, ready);
                bootstrap
                    .information
                    .as_mut()
                    .unwrap()
                    .target_storage_format = None;
                bootstrap.errors = errors;
                state.bootstrap.push_back(bootstrap);
                let (state, result, events) = standard(state);
                let error = result.unwrap_err();
                assert!(error.reason.contains("missing storage target"));
                assert!(!error.outcome_unknown);
                let s = state.lock().unwrap();
                assert_eq!(s.requests, ["status"]);
                assert_eq!(s.files, original);
                assert!(s.mutations.is_empty());
                assert_eq!(s.handles, 0);
                assert!(matches!(
                    events.last().unwrap().event,
                    Some(ipc::migration_event::Event::Failure(_))
                ));
            }
        }
    }
}

#[test]
fn no_marker_never_authorizes_files_regardless_of_informational_current() {
    for reported in [None, Some(1), Some(2), Some(99)] {
        for ready in [false, true] {
            for errors in [vec![], vec![storage_failure()]] {
                let original = current(fixture());
                let mut state = State::new(original.clone());
                let mut bootstrap = status(2, reported, ready);
                bootstrap.errors = errors.clone();
                state.bootstrap.push_back(bootstrap);
                let (state, result, _) = standard(state);
                if !errors.is_empty() {
                    assert_eq!(result.unwrap_err().reason, STORAGE_FAILED);
                } else if ready {
                    assert_eq!(result.unwrap(), Outcome::Success);
                } else {
                    assert_eq!(result.unwrap_err().reason, "the adapter isn't ready");
                }
                let s = state.lock().unwrap();
                assert_eq!(s.requests, ["status"]);
                assert_eq!(s.files, original);
            }
        }
    }
    let mut state = State::new(fixture());
    state.bootstrap.push_back(status(3, None, false));
    let (state, result, _) = standard(state);
    assert!(result.unwrap_err().reason.contains("updated client"));
    assert_eq!(state.lock().unwrap().requests, ["status"]);
}

#[test]
fn marker_uses_downloaded_source_despite_absent_matching_or_misleading_current() {
    for reported in [None, Some(1), Some(2), Some(99)] {
        let mut state = State::new(fixture());
        let mut bootstrap = status(2, reported, false);
        bootstrap.errors = vec![assisted_error()];
        state.bootstrap.push_back(bootstrap);
        let (state, result, events) = standard(state);
        assert_eq!(result.unwrap(), Outcome::Success);
        assert_eq!(state.lock().unwrap().files, current(fixture()));
        assert!(events.iter().any(|e| matches!(
            &e.event,
            Some(ipc::migration_event::Event::Progress(v))
                if v.source_format == 1 && v.target_format == 2
        )));
    }
}

fn without_scratch(mut files: Files) -> Files {
    files.remove("/.file-upload");
    files
}

#[test]
fn blocked_known_storage_errors_precede_inventory_and_preserve_details() {
    let with = |outcome: p::ErrorOutcome| p::Error {
        outcome: outcome.into(),
        ..storage_failure()
    };
    let related = |error: p::Error| p::Error {
        related: vec![error],
        ..assisted_error()
    };
    let full = p::Error {
        code: p::ErrorCode::StorageFull.into(),
        ..assisted_error()
    };
    let also = format!("the adapter's saved data must be migrated; also {STORAGE_FAILED}");
    for (errors, reason) in [
        (vec![storage_failure()], STORAGE_FAILED.to_owned()),
        (
            vec![with(p::ErrorOutcome::Unspecified)],
            STORAGE_FAILED.into(),
        ),
        (vec![with(p::ErrorOutcome::Unknown)], STORAGE_FAILED.into()),
        (vec![with(p::ErrorOutcome::Complete)], STORAGE_FAILED.into()),
        (vec![related(storage_failure())], also.clone()),
        (vec![related(with(p::ErrorOutcome::Unknown))], also),
        (
            vec![assisted_error(), storage_failure()],
            STORAGE_FAILED.into(),
        ),
        (
            vec![assisted_error(), full],
            "the adapter's storage is full".into(),
        ),
    ] {
        let mut state = State::new(fixture());
        let mut bootstrap = status(2, Some(1), false);
        bootstrap.errors = errors;
        state.bootstrap.push_back(bootstrap);
        let (state, result, events) = standard(state);
        let error = result.unwrap_err();
        assert_eq!(
            (error.reason.as_str(), error.outcome_unknown),
            (reason.as_str(), false)
        );
        assert!(matches!(
            &events.last().unwrap().event,
            Some(ipc::migration_event::Event::Failure(f))
                if f.reason == reason && !f.outcome_unknown
        ));
        assert_eq!(state.lock().unwrap().requests, ["status"]);
    }
}

#[test]
fn only_a_plain_not_found_reply_proves_a_file_is_absent() {
    let missing = p::Error {
        code: p::ErrorCode::NotFound.into(),
        outcome: p::ErrorOutcome::NoEffect.into(),
        ..Default::default()
    };
    for (error, reason, unknown) in [
        (
            p::Error {
                outcome: p::ErrorOutcome::Unknown.into(),
                ..missing.clone()
            },
            "the adapter couldn't find a migration file".to_owned(),
            true,
        ),
        (
            p::Error {
                related: vec![storage_failure()],
                ..missing
            },
            format!("the adapter couldn't find a migration file; also {STORAGE_FAILED}"),
            false,
        ),
    ] {
        let original = fixture();
        let mut state = State::new(original.clone());
        state.not_found = Some(error);
        let (state, result, _) = standard(state);
        let error = result.unwrap_err();
        assert_eq!(error.reason, reason);
        assert!(error.outcome_unknown || !unknown);
        let s = state.lock().unwrap();
        assert_eq!(s.files.get("/format.pb"), original.get("/format.pb"));
        assert!(!s.requests.iter().any(|r| r == "restart"));
    }
}

#[test]
fn only_the_exact_migration_marker_authorizes_file_access() {
    for error in [
        p::Error {
            code: p::ErrorCode::NotReady.into(),
            ..assisted_error()
        },
        p::Error {
            reason: p::ErrorReason::Radio.into(),
            ..assisted_error()
        },
        p::Error {
            outcome: p::ErrorOutcome::Unknown.into(),
            ..assisted_error()
        },
        p::Error {
            related: vec![assisted_error()],
            ..assisted_error()
        },
    ] {
        let mut state = State::new(fixture());
        state.bootstrap.push_back(p::Status {
            errors: vec![error],
            ..status(2, Some(1), false)
        });
        let (state, result, _) = standard(state);
        assert!(result.is_err());
        assert_eq!(state.lock().unwrap().requests, ["status"]);
    }
}

#[test]
fn marker_with_unknown_downloaded_source_stops_before_writes() {
    let mut state = State::new(fixture());
    state.files.insert("/format.pb".into(), vec![8, 99, 16, 1]);
    state.bootstrap.push_back(p::Status {
        errors: vec![assisted_error()],
        ..status(2, Some(1), false)
    });
    let (state, result, _) = standard(state);
    assert!(
        result
            .unwrap_err()
            .reason
            .contains("unsupported or missing storage root")
    );
    let s = state.lock().unwrap();
    assert!(s.requests.iter().any(|r| r == "read:/format.pb:0"));
    assert!(s.mutations.is_empty());
    assert!(!s.requests.iter().any(|r| r == "restart"));
}

#[test]
fn matching_ready_storage_needs_only_status_with_file_access_disabled() {
    let original = current(fixture());
    let (state, result, _) = standard(State::new(original.clone()));
    assert_eq!(result.unwrap(), Outcome::Success);
    let s = state.lock().unwrap();
    assert!(!s.assisted);
    assert_eq!(s.requests, ["status"]);
    assert_eq!(s.files, original);
}

#[test]
fn newer_committed_storage_is_preserved_without_inventory() {
    let mut files = fixture();
    files.insert(
        "/format.pb".into(),
        saved::Format {
            format: 3,
            initialized: true,
        }
        .encode_to_vec(),
    );
    let mut state = State::new(files.clone());
    state.assisted = false;
    state.bootstrap.push_back(p::Status {
        errors: vec![storage_failure()],
        ..status(2, Some(3), false)
    });
    let (state, result, _) = standard(state);
    assert_eq!(result.unwrap_err().reason, STORAGE_FAILED);
    let s = state.lock().unwrap();
    assert_eq!(s.requests, ["status"]);
    assert_eq!(s.files, without_scratch(files));
}

#[test]
fn malformed_and_ambiguous_datasets_make_no_writes() {
    for (path, bytes) in [
        ("/format.pb", vec![8, 1, 8, 1]),
        ("/sequence.pb", vec![8, 2, 16, 1, 24, 2]),
        ("/identity.pb", vec![]),
        ("/adapter.pb", vec![0xff]),
    ] {
        let mut state = State::new(fixture());
        state.files.insert(path.into(), bytes);
        state.bootstrap.push_back(status(2, Some(1), false));
        let (state, result, _) = standard(state);
        assert!(result.is_err(), "{path}");
        let s = state.lock().unwrap();
        assert!(s.requests.iter().any(|r| r == "read:/format.pb:0"));
        assert!(s.mutations.is_empty());
    }
}

#[test]
fn read_metadata_and_exact_chunk_lengths_are_checked_before_conversion() {
    for corruption in ["path", "offset", "size", "short", "long"] {
        let mut state = State::new(fixture());
        state.corrupt_read = Some(corruption);
        let (state, result, _) = standard(state);
        assert!(
            result.unwrap_err().reason.contains("mismatch"),
            "{corruption}"
        );
        assert!(state.lock().unwrap().mutations.is_empty());
    }
}

#[test]
fn inventory_caps_and_checked_sizes_precede_download() {
    for listing in [
        vec![p::FileEntry {
            name: "oversize".into(),
            size: 64 * 1024 * 1024 + 1,
            ..Default::default()
        }],
        vec![p::FileEntry {
            name: "overflow".into(),
            size: u64::MAX,
            ..Default::default()
        }],
        (0..16_385)
            .map(|n| p::FileEntry {
                name: format!("file-{n:05}"),
                ..Default::default()
            })
            .collect(),
        vec![p::FileEntry {
            name: "..".into(),
            ..Default::default()
        }],
    ] {
        let mut state = State::new(fixture());
        state.listing = Some(listing);
        let (state, result, _) = standard(state);
        assert!(result.is_err());
        assert_eq!(state.lock().unwrap().requests, ["status", "list:/"]);
    }
}

#[test]
fn missing_adapter_waits_without_a_deadline_until_explicit_cancel() {
    let mut state = State::new(fixture());
    state.ports.clear();
    state.cancel_after_waits = Some(100_000);
    let (state, result, events) = standard(state);
    assert_eq!(result.unwrap(), Outcome::Cancelled);
    assert_eq!(state.lock().unwrap().waits, 100_000);
    assert!(state.lock().unwrap().requests.is_empty());
    assert!(matches!(
        events.last().unwrap().event,
        Some(ipc::migration_event::Event::Cancelled(_))
    ));
}

#[test]
fn usb_metadata_filters_candidates_and_bootstrap_confirms_the_identity() {
    let mut state = State::new(fixture());
    state.ports.insert(
        0,
        AdapterPort {
            port: "wrong-hint".into(),
            adapter_id: "other-adapter".into(),
        },
    );
    let (state, result, _) = standard(state);
    result.unwrap();
    assert!(
        state
            .lock()
            .unwrap()
            .opened
            .iter()
            .all(|p| p == "fixture-port")
    );
    let mut state = State::new(fixture());
    state.status_id = "another-adapter".into();
    state.cancel_after_waits = Some(4);
    let (state, result, _) = standard(state);
    assert_eq!(result.unwrap(), Outcome::Cancelled);
    assert_eq!(state.lock().unwrap().requests, ["status"; 4]);
}

#[test]
fn lost_write_readback_and_restart_acknowledgements_resume_by_content() {
    for (key, after) in [
        ("write:/adapter.pb:0", true),
        ("write:/adapter.pb:0", false),
        ("read:/adapter.pb:0", true),
        ("write:/format.pb:0", true),
        ("write:/format.pb:0", false),
        ("delete:/devices/2/settings.pb", true),
        ("delete:/devices/2/settings.pb", false),
        ("restart", true),
        ("restart", false),
    ] {
        let mut state = State::new(fixture());
        if key == "read:/adapter.pb:0" {
            state.fault_min_mutations = 1;
        }
        state.faults.push_back(Fault {
            key: key.into(),
            after,
            reboot: false,
            finish_on_device: false,
        });
        let (state, result, _) = standard(state);
        assert_eq!(result.unwrap(), Outcome::Success, "{key}");
        assert!(state.lock().unwrap().faults.is_empty());
        assert_eq!(state.lock().unwrap().files, current(fixture()));
    }
}

#[test]
fn status_failure_after_a_lost_write_or_restart_keeps_the_outcome_unknown() {
    for key in ["write:/adapter.pb:0", "write:/format.pb:0", "restart"] {
        let mut state = State::new(fixture());
        state.faults.push_back(Fault {
            key: key.into(),
            after: true,
            reboot: false,
            finish_on_device: false,
        });
        let mut blocked = status(2, Some(2), false);
        blocked.errors = vec![storage_failure()];
        state.bootstrap.extend([status(2, Some(1), false), blocked]);
        let (state, result, events) = standard(state);
        let error = result.unwrap_err();
        assert_eq!(error.reason, STORAGE_FAILED);
        assert!(error.outcome_unknown, "{key}");
        let s = state.lock().unwrap();
        let lost = s.requests.iter().position(|r| r == key).unwrap();
        assert_eq!(&s.requests[lost + 1..], ["status"]);
        assert!(matches!(
            &events.last().unwrap().event,
            Some(ipc::migration_event::Event::Failure(f)) if f.outcome_unknown
        ));
    }
}

#[test]
fn restart_waits_through_normal_startup_without_issuing_file_commands() {
    let mut state = State::new(fixture());
    state.bootstrap.extend([
        status(2, Some(1), false),
        status(2, Some(2), false),
        status(2, Some(2), false),
        status(2, Some(2), true),
    ]);
    let (state, result, _) = standard(state);
    assert_eq!(result.unwrap(), Outcome::Success);
    let s = state.lock().unwrap();
    let complete = s.requests.iter().position(|s| s == "restart").unwrap();
    assert_eq!(&s.requests[complete + 1..], ["status"; 3]);
}

#[test]
fn final_root_ack_loss_reads_back_latched_root_without_rewriting_files() {
    for reported in [None, Some(1), Some(2), Some(99)] {
        let mut state = State::new(fixture());
        state.bootstrap.extend([
            status(2, Some(1), false),
            p::Status {
                errors: vec![assisted_error()],
                ..status(2, reported, false)
            },
            p::Status {
                errors: vec![],
                ..status(2, reported, true)
            },
        ]);
        state.faults.push_back(Fault {
            key: "write:/format.pb:0".into(),
            after: true,
            reboot: false,
            finish_on_device: false,
        });
        let (state, result, _) = standard(state);
        assert_eq!(result.unwrap(), Outcome::Success);
        let s = state.lock().unwrap();
        let root = s
            .requests
            .iter()
            .position(|r| r == "write:/format.pb:0")
            .unwrap();
        assert_eq!(
            &s.requests[root + 1..],
            [
                "status",
                "read:/format.pb:0",
                "read:/format.pb:4",
                "restart",
                "status"
            ]
        );
        assert_eq!(s.files, current(fixture()));
    }
}

#[test]
fn active_download_waits_through_unready_status_without_marker_or_file_access() {
    for reported in [None, Some(1), Some(2), Some(99)] {
        let mut state = State::new(fixture());
        state.bootstrap.extend([
            status(2, Some(1), false),
            p::Status {
                errors: vec![],
                ..status(2, reported, false)
            },
            p::Status {
                errors: vec![assisted_error()],
                ..status(2, reported, false)
            },
        ]);
        state.faults.push_back(Fault {
            key: "read:/adapter.pb:0".into(),
            after: false,
            reboot: false,
            finish_on_device: false,
        });
        let (state, result, _) = standard(state);
        assert_eq!(result.unwrap(), Outcome::Success);
        let s = state.lock().unwrap();
        let read = s
            .requests
            .iter()
            .position(|r| r == "read:/adapter.pb:0")
            .unwrap();
        assert_eq!(
            &s.requests[read + 1..read + 4],
            ["status", "status", "read:/adapter.pb:0"]
        );
        assert_eq!(s.waits, 1);
    }
}

#[test]
fn final_root_ack_loss_waits_for_bootstrap_after_a_real_reset() {
    let mut state = State::new(fixture());
    let mut booting = status(2, None, false);
    booting.errors.clear();
    state
        .bootstrap
        .extend([status(2, Some(1), false), booting, status(2, Some(2), true)]);
    state.faults.push_back(Fault {
        key: "write:/format.pb:0".into(),
        after: true,
        reboot: true,
        finish_on_device: false,
    });
    let (state, result, _) = standard(state);
    assert_eq!(result.unwrap(), Outcome::Success);
    let s = state.lock().unwrap();
    let root = s
        .requests
        .iter()
        .position(|r| r == "write:/format.pb:0")
        .unwrap();
    assert_eq!(&s.requests[root + 1..], ["status"; 2]);
}

#[test]
fn active_restart_retries_marker_and_waits_without_it_but_fails_on_storage_errors() {
    for errors in [vec![], vec![assisted_error()], vec![storage_failure()]] {
        let mut state = State::new(fixture());
        let mut booting = status(2, None, false);
        booting.errors = errors.clone();
        state.bootstrap.extend([
            status(2, Some(1), false),
            booting,
            status(2, Some(2), false),
            status(2, Some(2), true),
        ]);
        let (state, result, _) = standard(state);
        let s = state.lock().unwrap();
        let restart = s.requests.iter().position(|r| r == "restart").unwrap();
        if errors == [storage_failure()] {
            let error = result.unwrap_err();
            assert_eq!(error.reason, STORAGE_FAILED);
            assert!(error.outcome_unknown);
            assert_eq!(&s.requests[restart + 1..], ["status"]);
        } else {
            assert_eq!(result.unwrap(), Outcome::Success);
            let expected = if errors == [assisted_error()] {
                vec!["status", "restart", "status", "status"]
            } else {
                vec!["status"; 3]
            };
            assert_eq!(&s.requests[restart + 1..], expected);
        }
    }
}

#[test]
fn restart_succeeds_with_normal_warnings_and_fails_on_other_errors() {
    use p::ErrorCode as C;
    let error = |code: C| p::Error {
        code: code.into(),
        ..Default::default()
    };
    for code in [
        C::StorageFull,
        C::ProfileMemoryLow,
        C::UsbRequiredCapacityExceeded,
        C::UsbReservedCapacityExceeded,
    ] {
        let mut state = State::new(fixture());
        state.bootstrap.extend([
            status(2, Some(1), false),
            p::Status {
                errors: vec![error(code)],
                ..status(2, Some(2), false)
            },
            p::Status {
                errors: vec![error(code)],
                ..status(2, Some(2), true)
            },
        ]);
        let (state, result, _) = standard(state);
        assert_eq!(result.unwrap(), Outcome::Success, "{code:?}");
        let s = state.lock().unwrap();
        let restart = s.requests.iter().position(|r| r == "restart").unwrap();
        assert_eq!(&s.requests[restart + 1..], ["status"; 2]);
        assert_eq!(s.files, current(fixture()));
    }
    let radio = p::Error {
        reason: p::ErrorReason::Radio.into(),
        ..error(C::NotReady)
    };
    let tied = p::Error {
        related: vec![storage_failure()],
        ..error(C::StorageFull)
    };
    for (errors, reason) in [
        (vec![storage_failure()], STORAGE_FAILED.to_owned()),
        (
            vec![error(C::StorageFull), storage_failure()],
            format!("the adapter's storage is full; {STORAGE_FAILED}"),
        ),
        (
            vec![radio],
            "the adapter's Bluetooth radio isn't ready".into(),
        ),
        (
            vec![error(C::Internal)],
            "the adapter hit an unexpected failure".into(),
        ),
        (
            vec![error(C::Unknown)],
            "the adapter reported an error this version of Cordial doesn't recognize".into(),
        ),
        (
            vec![tied],
            format!("the adapter's storage is full; also {STORAGE_FAILED}"),
        ),
    ] {
        let mut state = State::new(fixture());
        state.bootstrap.extend([
            status(2, Some(1), false),
            p::Status {
                errors,
                ..status(2, Some(2), true)
            },
        ]);
        let (state, result, _) = standard(state);
        let error = result.unwrap_err();
        assert_eq!(error.reason, reason);
        assert!(error.outcome_unknown);
        let s = state.lock().unwrap();
        let restart = s.requests.iter().position(|r| r == "restart").unwrap();
        assert_eq!(&s.requests[restart + 1..], ["status"]);
    }
}

#[test]
fn active_restart_remains_cancellable_while_normal_startup_is_not_ready() {
    let mut state = State::new(fixture());
    state
        .bootstrap
        .extend([status(2, Some(1), false), status(2, Some(2), false)]);
    state.cancel_after_waits = Some(1);
    let (state, result, _) = standard(state);
    assert_eq!(result.unwrap(), Outcome::Cancelled);
    let s = state.lock().unwrap();
    let restart = s.requests.iter().position(|r| r == "restart").unwrap();
    assert_eq!(&s.requests[restart + 1..], ["status"]);
}

#[test]
fn a_changed_target_after_final_root_loss_cannot_be_mistaken_for_success() {
    let mut state = State::new(fixture());
    state.faults.push_back(Fault {
        key: "write:/format.pb:0".into(),
        after: true,
        reboot: true,
        finish_on_device: false,
    });
    state
        .bootstrap
        .extend([status(2, Some(1), false), status(3, Some(3), true)]);
    let (state, result, events) = standard(state);
    assert!(result.unwrap_err().outcome_unknown);
    assert_eq!(state.lock().unwrap().requests.last().unwrap(), "status");
    assert!(matches!(
        events.last().unwrap().event,
        Some(ipc::migration_event::Event::Failure(_))
    ));
}

#[test]
fn invalid_readback_is_failure_with_unknown_outcome_and_never_commits_the_root() {
    let mut state = State::new(fixture());
    state.corrupt_read = Some("size");
    state.corrupt_read_min_mutations = 1;
    let (state, result, events) = standard(state);
    assert!(result.unwrap_err().outcome_unknown);
    assert_eq!(
        saved::Format::decode(state.lock().unwrap().files["/format.pb"].as_slice())
            .unwrap()
            .format,
        1
    );
    assert!(
        matches!(&events.last().unwrap().event, Some(ipc::migration_event::Event::Failure(v)) if v.outcome_unknown)
    );
}

#[test]
fn empty_and_unusable_cache_records_are_deleted_and_current_roots_are_not_rewritten() {
    let mut files = fixture();
    files.insert("/adapter.pb".into(), vec![]);
    files.insert("/devices/1/layout.pb".into(), vec![0xff]);
    let (state, result, _) = standard(State::new(files));
    result.unwrap();
    let s = state.lock().unwrap();
    assert!(!s.files.contains_key("/adapter.pb"));
    assert!(!s.files.contains_key("/devices/1/layout.pb"));
    drop(s);
    let mut files = current(fixture());
    files.insert("/.file-upload".into(), vec![0xff; 512]);
    let (state, result, _) = standard(State::new(files));
    result.unwrap();
    let s = state.lock().unwrap();
    assert!(
        !s.requests
            .iter()
            .any(|v| v.starts_with("write:") || v.starts_with("move:"))
    );
    assert!(!s.files.contains_key("/.file-upload"));
}

#[test]
fn arbitrary_mixed_v1_v2_files_keep_their_committed_nested_payloads() {
    let original = fixture();
    let target = current(original.clone());
    let paths: Vec<_> = target
        .keys()
        .filter(|p| p.as_str() != "/format.pb" && !p.starts_with("/extension/"))
        .collect();
    assert_eq!(paths.len(), 8);
    for mask in 0u32..(1 << paths.len()) {
        let mut files = original.clone();
        for (n, path) in paths.iter().enumerate() {
            if mask & (1 << n) != 0 {
                files.insert((*path).clone(), target[*path].clone());
            }
        }
        let (state, result, _) = standard(State::new(files));
        assert_eq!(result.unwrap(), Outcome::Success, "mask {mask}");
        assert_eq!(state.lock().unwrap().files, target, "mask {mask}");
    }
    let mut files = original;
    let sequence = saved::Sequence {
        format: 2,
        next_device: 5,
        next_profile: 7,
    }
    .encode_to_vec();
    files.insert("/sequence.pb".into(), sequence.clone());
    let (state, result, _) = standard(State::new(files));
    result.unwrap();
    assert_eq!(state.lock().unwrap().files["/sequence.pb"], sequence);
}

#[test]
fn final_root_loss_or_on_device_completion_never_reads_disabled_files() {
    for (key, on_device) in [("write:/format.pb:0", false), ("read:/adapter.pb:0", true)] {
        let mut state = State::new(fixture());
        state.faults.push_back(Fault {
            key: key.into(),
            after: true,
            reboot: !on_device,
            finish_on_device: on_device,
        });
        let (state, result, _) = standard(state);
        assert_eq!(result.unwrap(), Outcome::Success);
        assert_eq!(state.lock().unwrap().requests.last().unwrap(), "status");
        assert_eq!(state.lock().unwrap().files, current(fixture()));
    }
}

#[test]
fn fresh_matching_unready_adapter_without_marker_reports_readiness_without_restart() {
    for errors in [vec![], vec![storage_failure()]] {
        let mut state = State::new(current(fixture()));
        let mut bootstrap = status(2, Some(2), false);
        bootstrap.errors = errors;
        state.bootstrap.push_back(bootstrap);
        let (state, result, _) = standard(state);
        assert!(result.is_err());
        assert_eq!(state.lock().unwrap().requests, ["status"]);
    }
}

#[test]
fn fresh_marker_with_target_root_validates_dataset_then_restarts_without_writes() {
    for reported in [None, Some(1), Some(2), Some(99)] {
        for malformed in [false, true] {
            let mut files = current(fixture());
            if malformed {
                files.insert("/adapter.pb".into(), vec![0xff]);
            }
            let mut state = State::new(files.clone());
            let mut bootstrap = status(2, reported, false);
            bootstrap.errors = vec![assisted_error()];
            state.bootstrap.push_back(bootstrap);
            let (state, result, _) = standard(state);
            let s = state.lock().unwrap();
            assert!(s.requests.iter().any(|r| r == "list:/"));
            assert!(s.mutations.is_empty());
            assert_eq!(s.files, files);
            if malformed {
                assert!(result.is_err());
                assert!(!s.requests.iter().any(|r| r == "restart"));
            } else {
                assert_eq!(result.unwrap(), Outcome::Success);
                assert_eq!(&s.requests[s.requests.len() - 2..], ["restart", "status"]);
            }
        }
    }
}

fn large_fixture() -> Files {
    let mut files = fixture();
    let device = v1::Device::decode(files["/devices/1/device.pb"].as_slice()).unwrap();
    let mut policy = device.policy.unwrap().encode_to_vec();
    prost::encoding::bytes::encode(99, &vec![0x61; 1600], &mut policy);
    let mut raw = vec![];
    prost::encoding::bytes::encode(1, &policy, &mut raw);
    prost::encoding::message::encode(2, &device.bond.unwrap(), &mut raw);
    files.insert("/devices/1/device.pb".into(), raw);
    files
}

#[test]
fn chunked_replacements_verify_scratch_move_and_destination_despite_lost_replies() {
    for key in [
        "write:/.file-upload:0",
        "write:/.file-upload:512",
        "move:/devices/1/device.pb",
        "read:/.file-upload:512",
    ] {
        for after in [false, true] {
            let original = large_fixture();
            let mut state = State::new(original.clone());
            state.faults.push_back(Fault {
                key: key.into(),
                after,
                reboot: false,
                finish_on_device: false,
            });
            let (state, result, _) = standard(state);
            assert_eq!(result.unwrap(), Outcome::Success, "{key}, after={after}");
            let s = state.lock().unwrap();
            assert!(s.faults.is_empty());
            assert_eq!(s.files, current(original));
            let moved = s
                .requests
                .iter()
                .position(|s| s == "move:/devices/1/device.pb")
                .unwrap();
            assert!(
                s.requests[..moved]
                    .iter()
                    .any(|s| s == "read:/.file-upload:0")
            );
            assert!(
                s.requests[moved + 1..]
                    .iter()
                    .any(|s| s == "read:/devices/1/device.pb:0")
            );
        }
    }
}

#[test]
fn partial_large_scratch_files_are_not_source_records_on_fresh_resume() {
    let original = large_fixture();
    let (complete, result, _) = standard(State::new(original.clone()));
    result.unwrap();
    let prefixes = complete.lock().unwrap().mutations.clone();
    for files in prefixes {
        let (state, result, _) = standard(State::new(files));
        assert_eq!(result.unwrap(), Outcome::Success);
        assert_eq!(state.lock().unwrap().files, current(original.clone()));
    }
}

#[test]
fn scratch_readback_reconnect_rebuilds_temporary_data_cleaned_by_startup() {
    let mut state = State::new(large_fixture());
    state.cleanup_scratch_on_reopen = true;
    state.faults.push_back(Fault {
        key: "read:/.file-upload:512".into(),
        after: true,
        reboot: false,
        finish_on_device: false,
    });
    let (state, result, _) = standard(state);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(state.lock().unwrap().files, current(large_fixture()));
}

fn invalid(reason: &str) -> migration::Error {
    migration::Error {
        reason: reason.into(),
        outcome_unknown: false,
    }
}
fn synthetic_source(s: &Snapshot) -> migration::Result<u32> {
    match s.file("/format.pb") {
        Some([id]) => Ok(u32::from(*id)),
        _ => Err(invalid("invalid synthetic root")),
    }
}
fn validate_one(s: &Snapshot) -> migration::Result<()> {
    if s.file("/source").is_some()
        || s.file("/left").is_some() && s.file("/right").is_some()
        || s.file("/merged").is_some()
    {
        Ok(())
    } else {
        Err(invalid("source data is missing"))
    }
}
fn validate_five(s: &Snapshot) -> migration::Result<()> {
    if (s.file("/left").is_some() && s.file("/right").is_some() || s.file("/merged").is_some())
        && s.file("/source").is_none()
    {
        Ok(())
    } else {
        Err(invalid("split data is missing"))
    }
}
fn validate_ten(s: &Snapshot) -> migration::Result<()> {
    if s.file("/merged").is_some()
        && s.file("/source").is_none()
        && s.file("/left").is_none()
        && s.file("/right").is_none()
    {
        Ok(())
    } else {
        Err(invalid("merged data is missing"))
    }
}
fn split(s: &Snapshot) -> migration::Result<Transformation> {
    let bytes = if let Some(bytes) = s.file("/source").or_else(|| s.file("/merged")) {
        bytes.to_vec()
    } else {
        [
            s.file("/left").ok_or_else(|| invalid("missing left"))?,
            s.file("/right").ok_or_else(|| invalid("missing right"))?,
        ]
        .concat()
    };
    let middle = bytes.len() / 2;
    Ok(Transformation {
        root: vec![5],
        changes: vec![
            Change::replace("/left", bytes[..middle].into()),
            Change::replace("/right", bytes[middle..].into()),
            Change::delete("/source"),
        ],
    })
}
fn merge(s: &Snapshot) -> migration::Result<Transformation> {
    let bytes = if let Some(bytes) = s.file("/merged") {
        bytes.to_vec()
    } else {
        [
            s.file("/left").ok_or_else(|| invalid("missing left"))?,
            s.file("/right").ok_or_else(|| invalid("missing right"))?,
        ]
        .concat()
    };
    Ok(Transformation {
        root: vec![10],
        changes: vec![
            Change::replace("/merged", bytes),
            Change::delete("/left"),
            Change::delete("/right"),
        ],
    })
}
fn shortcut(s: &Snapshot) -> migration::Result<Transformation> {
    let bytes = s
        .file("/source")
        .or_else(|| s.file("/merged"))
        .ok_or_else(|| invalid("missing source"))?;
    Ok(Transformation {
        root: vec![10],
        changes: vec![
            Change::replace("/merged", bytes.into()),
            Change::delete("/source"),
        ],
    })
}
fn graph(checkpoint: Checkpoint, direct: bool) -> Graph {
    let mut edges = vec![
        Edge {
            from: 1,
            to: 5,
            checkpoint,
            allocation_bound: synthetic_bound,
            transform: split,
        },
        Edge {
            from: 5,
            to: 10,
            checkpoint: Checkpoint::Immediate,
            allocation_bound: synthetic_bound,
            transform: merge,
        },
    ];
    if direct {
        edges.push(Edge {
            from: 1,
            to: 10,
            checkpoint: Checkpoint::Immediate,
            allocation_bound: synthetic_bound,
            transform: shortcut,
        });
    }
    Graph::new(
        vec![
            Format {
                id: 1,
                validate: validate_one,
            },
            Format {
                id: 5,
                validate: validate_five,
            },
            Format {
                id: 10,
                validate: validate_ten,
            },
        ],
        edges,
        synthetic_source,
    )
    .unwrap()
}
fn synthetic_bound(s: &Snapshot) -> migration::Result<usize> {
    Ok(s.files().map(|(_, b)| b.len()).sum::<usize>() * 4 + 4096)
}
fn synthetic_state() -> State {
    let mut s = State::new(Files::from([
        ("/format.pb".into(), vec![1]),
        ("/source".into(), b"split and merge payload".to_vec()),
        ("/unknown".into(), b"keep".to_vec()),
    ]));
    s.target = 10;
    s.startup();
    s
}
#[test]
fn graph_uses_deterministic_shortest_routes_with_gaps_and_shortcuts() {
    let g = graph(Checkpoint::Immediate, true);
    assert_eq!(
        g.route(1, 10)
            .unwrap()
            .iter()
            .map(|e| (e.from, e.to))
            .collect::<Vec<_>>(),
        [(1, 10)]
    );
    assert!(g.route(10, 1).is_err());
    assert!(g.route(1, 8).is_err());
    assert_eq!(
        graph(Checkpoint::Immediate, false)
            .route(1, 10)
            .unwrap()
            .len(),
        2
    );
    let s = Arc::new(Mutex::new(synthetic_state()));
    let (result, events) = run(&s, &g);
    assert_eq!(result.unwrap(), Outcome::Success);
    let checkpoints = events
        .iter()
        .filter_map(|e| match &e.event {
            Some(ipc::migration_event::Event::Checkpoint(v)) => Some(v.committed_format),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(checkpoints, [10]);
}

#[test]
fn intermediate_checkpoint_updates_source_progress_and_resumes_lost_root_ack() {
    let graph = graph(Checkpoint::Immediate, false);
    let mut state = synthetic_state();
    state.faults.push_back(Fault {
        key: "write:/format.pb:0".into(),
        after: true,
        reboot: true,
        finish_on_device: false,
    });
    let state = Arc::new(Mutex::new(state));
    let (result, events) = run(&state, &graph);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(events.iter().any(|event| matches!(
        &event.event,
        Some(ipc::migration_event::Event::Progress(progress))
            if progress.checkpoint_format == 10 && progress.source_format == 5
    )));
    let s = state.lock().unwrap();
    assert!(s.faults.is_empty());
    assert_eq!(s.files["/format.pb"], [10]);
    assert_eq!(s.requests.iter().filter(|r| *r == "restart").count(), 1);
}

#[test]
fn reconnect_rejects_a_changed_target_even_when_both_targets_are_supported() {
    let graph = graph(Checkpoint::Immediate, false);
    let mut state = synthetic_state();
    state
        .bootstrap
        .extend([status(10, Some(1), false), status(5, Some(1), false)]);
    state.faults.push_back(Fault {
        key: "read:/format.pb:0".into(),
        after: false,
        reboot: false,
        finish_on_device: false,
    });
    let state = Arc::new(Mutex::new(state));
    let (result, _) = run(&state, &graph);
    assert!(result.unwrap_err().reason.contains("target changed"));
    let s = state.lock().unwrap();
    assert_eq!(s.requests.last().unwrap(), "status");
    assert!(s.mutations.is_empty());
}

#[test]
fn split_merge_recovers_from_every_durable_prefix_with_immediate_or_proven_deferred_roots() {
    for checkpoint in [Checkpoint::Immediate, Checkpoint::MayDefer] {
        let g = graph(checkpoint, false);
        let s = Arc::new(Mutex::new(synthetic_state()));
        let (result, events) = run(&s, &g);
        assert_eq!(result.unwrap(), Outcome::Success);
        let checkpoints = events
            .iter()
            .filter_map(|e| match &e.event {
                Some(ipc::migration_event::Event::Checkpoint(v)) => Some(v.committed_format),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            checkpoints,
            if checkpoint == Checkpoint::Immediate {
                vec![5, 10]
            } else {
                vec![10]
            }
        );
        let expected = s.lock().unwrap().files.clone();
        let prefixes = s.lock().unwrap().mutations.clone();
        drop(s);
        for files in prefixes {
            let mut state = State::new(files);
            state.target = 10;
            state.startup();
            let s = Arc::new(Mutex::new(state));
            let (result, _) = run(&s, &g);
            assert_eq!(result.unwrap(), Outcome::Success);
            assert_eq!(s.lock().unwrap().files, expected);
            assert_eq!(s.lock().unwrap().files["/unknown"], b"keep");
        }
    }
}

fn unsafe_split(s: &Snapshot) -> migration::Result<Transformation> {
    let mut split = split(s)?;
    split.changes.swap(0, 2);
    Ok(split)
}
#[test]
fn destructive_split_order_is_rejected_before_any_mutation() {
    let g = Graph::new(
        vec![
            Format {
                id: 1,
                validate: validate_one,
            },
            Format {
                id: 5,
                validate: validate_five,
            },
        ],
        vec![Edge {
            from: 1,
            to: 5,
            checkpoint: Checkpoint::Immediate,
            allocation_bound: synthetic_bound,
            transform: unsafe_split,
        }],
        synthetic_source,
    )
    .unwrap();
    let mut state = synthetic_state();
    state.target = 5;
    state.startup();
    let s = Arc::new(Mutex::new(state));
    let (result, _) = run(&s, &g);
    assert!(result.is_err());
    assert!(s.lock().unwrap().mutations.is_empty());
}

fn strict_split(s: &Snapshot) -> migration::Result<Transformation> {
    if s.file("/merged").is_some() {
        return Err(invalid("future representation needs a checkpoint"));
    }
    split(s)
}
#[test]
fn a_deferrable_edge_keeps_its_checkpoint_when_composition_is_not_recoverable() {
    let g = Graph::new(
        vec![
            Format {
                id: 1,
                validate: validate_one,
            },
            Format {
                id: 5,
                validate: validate_five,
            },
            Format {
                id: 10,
                validate: validate_ten,
            },
        ],
        vec![
            Edge {
                from: 1,
                to: 5,
                checkpoint: Checkpoint::MayDefer,
                allocation_bound: synthetic_bound,
                transform: strict_split,
            },
            Edge {
                from: 5,
                to: 10,
                checkpoint: Checkpoint::Immediate,
                allocation_bound: synthetic_bound,
                transform: merge,
            },
        ],
        synthetic_source,
    )
    .unwrap();
    let s = Arc::new(Mutex::new(synthetic_state()));
    let (result, events) = run(&s, &g);
    assert_eq!(result.unwrap(), Outcome::Success);
    let checkpoints = events
        .iter()
        .filter_map(|e| match &e.event {
            Some(ipc::migration_event::Event::Checkpoint(v)) => Some(v.committed_format),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(checkpoints, [5, 10]);
}

fn excessive_bound(_: &Snapshot) -> migration::Result<usize> {
    Ok(p::STORAGE_MIGRATION_MAX_MEMORY_BYTES)
}
fn must_not_allocate(_: &Snapshot) -> migration::Result<Transformation> {
    panic!("allocation must be rejected before transform runs")
}
#[test]
fn transform_working_memory_is_bounded_before_calling_the_converter() {
    let g = Graph::new(
        vec![
            Format {
                id: 1,
                validate: validate_one,
            },
            Format {
                id: 5,
                validate: validate_five,
            },
        ],
        vec![Edge {
            from: 1,
            to: 5,
            checkpoint: Checkpoint::Immediate,
            allocation_bound: excessive_bound,
            transform: must_not_allocate,
        }],
        synthetic_source,
    )
    .unwrap();
    let mut state = synthetic_state();
    state.target = 5;
    state.startup();
    let s = Arc::new(Mutex::new(state));
    let (result, _) = run(&s, &g);
    assert!(result.unwrap_err().reason.contains("RAM"));
    assert!(s.lock().unwrap().mutations.is_empty());
}

#[test]
fn unsupported_protocol_stops_migration_at_bootstrap_and_after_reconnect() {
    for open in [1, 2] {
        let mut state = State::new(fixture());
        state.incompatible_on_open = Some(open);
        state.cancel_after_waits = Some(1);
        if open == 2 {
            state.faults.push_back(Fault {
                key: "read:/format.pb:0".into(),
                after: false,
                reboot: false,
                finish_on_device: false,
            });
        }
        let (state, result, events) = standard(state);
        assert!(result.unwrap_err().reason.contains("Update Cordial"));
        let state = state.lock().unwrap();
        assert_eq!(state.opened.len(), open);
        assert_eq!(state.waits, 0);
        assert!(state.mutations.is_empty());
        assert!(matches!(
            events.last().unwrap().event,
            Some(ipc::migration_event::Event::Failure(_))
        ));
    }
}
