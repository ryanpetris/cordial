//! The shell's command language: resource/action commands named after the
//! wire protocol, pairing answers and completion.
use crate::{
    controller::{Auth, Command, SettingInput, State},
    ui::{
        catalog::{self, value_string},
        text::{Filter, quote},
    },
};
use cordial_protocol::{
    MAX_REQUEST_ID,
    identifiers::{HostPlatform, RequestId, ScanTransport},
    messages::{PairAction, PromptMethod},
    settings::{Setting, SettingKey, SettingType},
};
use std::{path::PathBuf, time::Instant};

/// A shell command: its words, argument syntax and description, in help order.
struct Spec {
    words: &'static str,
    args: &'static str,
    text: &'static str,
}
const fn spec(words: &'static str, args: &'static str, text: &'static str) -> Spec {
    Spec { words, args, text }
}

/// The scan row's arguments and description follow the offered transports.
const SCAN: &str = "discovery scan";

const SPECS: &[Spec] = &[
    spec("adapter list", "", "List attached adapters"),
    spec(
        "adapter select",
        "PORT",
        "Select an adapter; close the previous session",
    ),
    spec(
        "adapter status",
        "",
        "Adapter identity, readiness, platform, capacity and pending requests",
    ),
    spec(
        "adapter capabilities",
        "",
        "Optional functions this adapter's firmware offers",
    ),
    spec(SCAN, "", ""),
    spec(
        "device list",
        "[Saved|Paired|Enabled|Connected|Trusted]",
        "Saved devices, including disabled ones and ones needing pairing, and discovered candidates",
    ),
    spec("device get", "DEV", "Device details"),
    spec(
        "device info",
        "DEV",
        "What the device reports about itself, such as batteries, model and firmware",
    ),
    spec(
        "device info refresh",
        "DEV",
        "Ask a connected device for current information, then show it",
    ),
    spec(
        "pairing start",
        "CANDIDATE",
        "Pair a Nearby device by candidate ID or a name only one Nearby device has; one already saved pairs again, keeping its settings; saved disabled when no enabled place is free; then connect",
    ),
    spec(
        "pairing reply",
        "ID PROMPT accept [VALUE] | reject",
        "Answer a pairing prompt",
    ),
    spec(
        "device connect",
        "DEV",
        "Connect using a saved bond; clear reconnect pause",
    ),
    spec(
        "device disconnect",
        "DEV",
        "Keep bond; pause reconnect until connect or restart",
    ),
    spec(
        "device enabled set",
        "DEV on | off",
        "On uses a saved device for connections and needs a free enabled place; off disconnects it and keeps its bond and settings",
    ),
    spec(
        "device trusted set",
        "DEV on | off",
        "Allow or stop future unattended connections",
    ),
    spec(
        "device blocked set",
        "DEV on | off",
        "Persistently deny connections, or allow them again",
    ),
    spec(
        "device unpair",
        "DEV",
        "Disconnect and remove the saved bond; a Nearby entry is hidden",
    ),
    spec(
        "device hidpp set",
        "DEV on | off",
        "Allow HID++ special keys and applying device settings (saved per device)",
    ),
    spec("adapter name set", "NAME", "Rename adapter"),
    spec("adapter name reset", "", "Reset adapter name to default"),
    spec(
        "adapter platform set",
        "linux | windows | mac",
        "OS whose shortcuts HID++ keys send (saved on the adapter)",
    ),
    spec(
        "hidpp feature list",
        "DEV",
        "HID++ features discovered on a connected device",
    ),
    spec(
        "hidpp setting list",
        "DEV",
        "Device settings: last observed values and saved preferences",
    ),
    spec(
        "hidpp setting get",
        "DEV KEY",
        "One setting of a connected device in detail",
    ),
    spec(
        "hidpp setting set",
        "DEV KEY VALUE",
        "Save a preference on the dongle; applied now if HID++ is on",
    ),
    spec(
        "hidpp setting forget",
        "DEV KEY",
        "Default: forget the saved value; leave the device unchanged",
    ),
    spec(
        "hidpp setting refresh",
        "DEV",
        "Read current values from a connected device; changes nothing",
    ),
    spec(
        "hidpp setting apply",
        "DEV",
        "Reapply saved values now (HID++ on) and report each result",
    ),
    spec(
        "session monitor set",
        "on | off",
        "Toggle notifications in this session",
    ),
    spec(
        "request cancel",
        "REQUEST_ID",
        "Cancel scan, pairing, or an explicit connection",
    ),
    spec(
        "storage ls",
        "PATH",
        "List a directory of the adapter's filesystem",
    ),
    spec(
        "storage get",
        "PATH LOCAL_FILE",
        "Download an adapter file; an existing local file is never replaced",
    ),
    spec(
        "adapter bootloader enter",
        "",
        "Development only; HID stops while in BOOTSEL",
    ),
    spec("help", "[COMMAND]", "Show this help"),
    spec("quit", "", "Close control session; saved HID keeps working"),
    spec("exit", "", ""),
];

/// Paragraphs after the rows, each listed while any of its commands is offered.
const NOTES: &[(&[&str], &str)] = &[
    (
        &[
            "device get",
            "device info",
            "pairing start",
            "device connect",
            "device disconnect",
            "device enabled set",
            "device trusted set",
            "device blocked set",
            "device unpair",
            "device hidpp set",
            "hidpp",
        ],
        "DEV is an opaque ID or an unambiguous name. Quote names containing spaces.",
    ),
    (
        &["pairing reply"],
        "During a pairing prompt, enter the requested answer, or /COMMAND to run a command.",
    ),
    (&["ctrl-c"], ""),
    (
        &["discovery scan"],
        "Scan keeps running until discovery scan off, cancellation by ID, or exit. No service runs.",
    ),
    (&["direct"], ""),
    (
        &["storage"],
        "PATH is absolute on the adapter, such as /. A download is kept only when complete.",
    ),
    (
        &["hidpp"],
        "
Device settings are saved only when you set them. Saved values are reapplied
when the device reconnects with HID++ on, when HID++ is turned on, and after
an adapter platform change, overriding changes made on the device or from
another computer meanwhile.
Changes made on the device are shown but never saved or corrected. Device-wide
settings also affect other computers the device is paired with; current-host
settings apply only to this computer's host slot. A setting's values come from
the device: see hidpp setting get. Getting, setting and refreshing settings,
and listing features, need the device connected. With HID++ off they still
read the device, and hidpp setting set only saves the value on the dongle
without applying it; hidpp setting apply needs HID++ on. While disconnected,
hidpp setting list shows what the adapter last knew, and hidpp setting forget
still returns a setting to Default. Turning HID++ off resets the adapter's
temporary HID++ reporting and key diversions; it does not restore device
settings, and saved settings wait until HID++ is on again. Only settings this
cordial recognizes can be shown or changed.",
    ),
];

/// Commands that inspect or recover an adapter, or read its files, while it
/// is still starting or failed to, so they run without waiting for readiness.
pub const DIRECT: &[&str] = &[
    "adapter status",
    "adapter capabilities",
    "storage ls",
    "storage get",
    "adapter bootloader enter",
];

/// The shell's help. Connected to an adapter, it lists only what that adapter
/// offers; without one it documents every command and claims nothing about
/// any adapter.
pub fn help(st: Option<&State>) -> String {
    let listed = |words: &str| st.is_none_or(|st| offered(words, st));
    let mut out = Vec::new();
    for r in SPECS
        .iter()
        .filter(|r| r.words != "exit" && listed(r.words))
    {
        let (syntax, text) = match r.words {
            SCAN => scan_row(st),
            "quit" => ("quit | exit".to_owned(), r.text.to_owned()),
            _ => (
                format!("{} {}", r.words, r.args).trim_end().to_owned(),
                r.text.to_owned(),
            ),
        };
        out.push(format!("{syntax:<32} {text}"));
    }
    out.push(String::new());
    for (words, text) in NOTES {
        match words[0] {
            "ctrl-c" => {
                // Rejecting and cancelling send commands; clearing input is local.
                let mut clauses = vec!["clears input", "rejects a prompt"];
                clauses.push("cancels a foreground operation");
                let last = clauses.pop().unwrap();
                out.push(format!("Ctrl-C {}, or {last}.", clauses.join(", ")));
            }
            "direct" => {
                let direct: Vec<&str> = DIRECT.iter().copied().filter(|w| listed(w)).collect();
                out.push(format!(
                    "Commands wait for the adapter to be ready, except {}.",
                    join_or(&direct)
                ));
            }
            _ if words.iter().any(|w| listed_prefix(w, st)) => out.push((*text).to_owned()),
            _ => {}
        }
    }
    out.join("\n")
}

fn join_or(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Whether any offered command starts with these words.
fn listed_prefix(words: &str, st: Option<&State>) -> bool {
    SPECS.iter().any(|s| {
        (s.words == words || s.words.starts_with(&format!("{words} ")))
            && st.is_none_or(|st| offered(s.words, st))
    })
}

/// The scan row's syntax and description for the offered transports.
fn scan_row(st: Option<&State>) -> (String, String) {
    let words = scan_words(st);
    let kinds: Vec<&str> = words
        .iter()
        .filter_map(|w| match *w {
            "on" => Some("both transports"),
            "le" => Some("BLE"),
            "bredr" => Some("Classic"),
            _ => None,
        })
        .collect();
    let text = match kinds.len() {
        1 => format!("Discover {} devices, or stop", kinds[0]),
        _ => format!("Discover {}, or stop", kinds.join(", ")),
    };
    (format!("{SCAN} {}", words.join(" | ")), text)
}

/// The scan arguments offered: both transports and each alone when the
/// adapter supports both, else its one transport, and off; none without a
/// transport to scan.
fn scan_words(st: Option<&State>) -> Vec<&'static str> {
    let Some(st) = st else {
        return vec!["on", "le", "bredr", "off"];
    };
    let mut out = match st.capabilities.scan_transport(ScanTransport::Both) {
        Some(ScanTransport::Both) => vec!["on", "le", "bredr"],
        Some(ScanTransport::Ble) => vec!["le"],
        Some(ScanTransport::Classic) => vec!["bredr"],
        None => Vec::new(),
    };
    if !out.is_empty() && Command::ScanOff.supported(st) {
        out.push("off");
    }
    out
}

/// A representative of the adapter command a shell command sends, to check
/// whether the adapter offers it.
fn sample(words: &str) -> Option<Command> {
    let id = String::new;
    Some(match words {
        "adapter status" => Command::Status,
        "adapter capabilities" => Command::Capabilities,
        "adapter name set" | "adapter name reset" => Command::Name(None),
        "adapter platform set" => Command::Platform(HostPlatform::Linux),
        "adapter bootloader enter" => Command::Bootloader,
        "session monitor set" => Command::Monitor(true),
        "request cancel" => Command::Cancel(RequestId::try_from(1).unwrap()),
        "device list" => Command::Devices,
        "device get" => Command::Info(id()),
        "device info" => Command::DeviceInfo(id()),
        "device info refresh" => Command::DeviceInfoRefresh(id()),
        "pairing start" => Command::Pair(id()),
        "pairing reply" => Command::PairReply {
            request: RequestId::try_from(1).unwrap(),
            prompt: id(),
            action: PairAction::Reject,
            value: None,
        },
        "device connect" => Command::Connect(id()),
        "device disconnect" => Command::Disconnect(id()),
        "device unpair" => Command::Remove(id()),
        "device enabled set" => Command::Enabled(id(), true),
        "device trusted set" => Command::Trusted(id(), true),
        "device blocked set" => Command::Blocked(id(), true),
        "device hidpp set" => Command::Hidpp(id(), true),
        "hidpp feature list" => Command::Features(id()),
        "hidpp setting list" => Command::Settings(id()),
        "hidpp setting get" => Command::SettingGet(id(), SettingKey::WheelMode),
        "hidpp setting set" => {
            Command::SettingSet(id(), SettingKey::WheelMode, SettingInput::Text(id()))
        }
        "hidpp setting forget" => Command::SettingForget(id(), SettingKey::WheelMode),
        "hidpp setting refresh" => Command::SettingsRefresh(id()),
        "hidpp setting apply" => Command::SettingsApply(id()),
        "storage ls" => Command::StorageList("/".into()),
        "storage get" => Command::StorageGet {
            path: "/".into(),
            local: PathBuf::new(),
            overwrite: false,
        },
        _ => return None,
    })
}

/// Whether the connected adapter offers a shell command. Local commands are
/// always offered.
fn offered(words: &str, st: &State) -> bool {
    match words {
        SCAN => !scan_words(Some(st)).is_empty(),
        // Nearby devices are shown and hidden locally; saved ones need the
        // wire command, which device completion checks per device.
        "device get" | "device unpair" => {
            sample(words).is_some_and(|c| c.supported(st))
                || !st.candidates.is_empty()
                || !scan_words(Some(st)).is_empty()
        }
        _ => sample(words).is_none_or(|c| c.supported(st)),
    }
}

/// The first words of the commands offered, as Tab completes them.
fn heads(st: Option<&State>) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for s in SPECS {
        let head = s.words.split(' ').next().unwrap();
        if st.is_none_or(|st| offered(s.words, st)) && !out.contains(&head) {
            out.push(head);
        }
    }
    out
}

/// Splits a command line into words, accepting quoted names and escaped
/// characters without invoking a shell.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let (mut escaped, mut started) = (false, false);
    for c in line.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            started = true;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                out.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            started = true;
            word.push(c);
        }
    }
    if quote.is_some() || escaped {
        return Err("unfinished quote or escape".into());
    }
    if started {
        out.push(word);
    }
    Ok(out)
}

/// A parsed command line: local commands, or an adapter command.
#[derive(Clone, Debug)]
pub enum Line {
    Help(Option<String>),
    List,
    Select(String),
    Quit,
    /// `device list`, listed with this filter after a refresh.
    Devices(Filter),
    Run(Command),
}

fn setting_key(word: &str) -> Result<SettingKey, String> {
    let valid = !word.is_empty()
        && word.len() <= 64
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if !valid {
        return Err(format!(
            "{} is not a setting key; use hidpp setting list DEV to list them",
            quote(word)
        ));
    }
    SettingKey::from_name(word).ok_or_else(|| {
        format!("Cordial doesn't recognize the setting {word}; a newer version may support it")
    })
}

fn request_id(word: &str) -> Option<RequestId> {
    word.parse::<u32>()
        .ok()
        .filter(|id| (1..=MAX_REQUEST_ID).contains(id))
        .and_then(|id| RequestId::try_from(id).ok())
}

fn on_off(word: &str) -> Option<bool> {
    match word {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// The command a line names and its arguments; errors are complete messages.
fn find(words: &[&str]) -> Result<(&'static Spec, usize), String> {
    let found = SPECS
        .iter()
        .filter_map(|s| {
            let n = s.words.split(' ').count();
            (words.len() >= n && words[..n].join(" ") == s.words).then_some((s, n))
        })
        .max_by_key(|(_, n)| *n);
    if let Some(found) = found {
        return Ok(found);
    }
    let known = SPECS
        .iter()
        .any(|s| s.words.split(' ').next() == Some(words[0]));
    Err(if known {
        format!("unknown {} command; use help", quote(words[0]))
    } else {
        format!("unknown command {}; use help", quote(words[0]))
    })
}

/// Parses split words. Errors are complete messages.
pub fn parse(args: &[String]) -> Result<Line, String> {
    let all: Vec<&str> = args.iter().map(String::as_str).collect();
    if all.is_empty() {
        return Err("empty command".into());
    }
    let (spec, n) = find(&all)?;
    let cmd = spec.words;
    let words = &all[n..];
    let usage = || Err(format!("invalid arguments for {cmd}; use help"));
    let arg = |i: usize| words[i].to_owned();
    let flag = |i: usize| on_off(words[i]);
    let line = match (cmd, words.len()) {
        ("help", 0) => Line::Help(None),
        ("help", _) if listed_prefix(&words.join(" "), None) => Line::Help(Some(words.join(" "))),
        ("help", _) => return Err(format!("unknown command {}", quote(&words.join(" ")))),
        ("quit" | "exit", 0) => Line::Quit,
        ("adapter select", 1) if !words[0].is_empty() => Line::Select(arg(0)),
        ("adapter list", 0) => Line::List,
        ("adapter status", 0) => Line::Run(Command::Status),
        ("adapter capabilities", 0) => Line::Run(Command::Capabilities),
        ("adapter name set", 1) => Line::Run(Command::Name(Some(words[0].into()))),
        ("adapter name reset", 0) => Line::Run(Command::Name(None)),
        ("adapter platform set", 1) => Line::Run(Command::Platform(match words[0] {
            "linux" => HostPlatform::Linux,
            "windows" => HostPlatform::Windows,
            "mac" => HostPlatform::Mac,
            _ => return usage(),
        })),
        ("adapter bootloader enter", 0) => Line::Run(Command::Bootloader),
        ("session monitor set", 1) => match flag(0) {
            Some(on) => Line::Run(Command::Monitor(on)),
            None => return usage(),
        },
        (SCAN, 1) => Line::Run(match words[0] {
            "on" => Command::Scan(ScanTransport::Both),
            "le" => Command::Scan(ScanTransport::Ble),
            "bredr" => Command::Scan(ScanTransport::Classic),
            "off" => Command::ScanOff,
            _ => return usage(),
        }),
        ("device list", 0) => Line::Devices(Filter::All),
        ("device list", 1) => Line::Devices(match words[0].to_lowercase().as_str() {
            "saved" => Filter::Saved,
            "paired" => Filter::Paired,
            "enabled" => Filter::Enabled,
            "connected" => Filter::Connected,
            "trusted" => Filter::Trusted,
            _ => return usage(),
        }),
        ("request cancel", 1) => match request_id(words[0]) {
            Some(id) => Line::Run(Command::Cancel(id)),
            None => return usage(),
        },
        ("pairing reply", 3 | 4) => {
            let (Some(request), action) = (request_id(words[0]), words[2]) else {
                return usage();
            };
            let action = match (action, words.len()) {
                ("accept", _) => PairAction::Accept,
                ("reject", 3) => PairAction::Reject,
                _ => return usage(),
            };
            Line::Run(Command::PairReply {
                request,
                prompt: arg(1),
                action,
                value: words
                    .get(3)
                    .filter(|v| !v.is_empty())
                    .map(|v| (*v).to_owned()),
            })
        }
        ("hidpp feature list", 1) => Line::Run(Command::Features(arg(0))),
        ("hidpp setting list", 1) => Line::Run(Command::Settings(arg(0))),
        ("hidpp setting refresh", 1) => Line::Run(Command::SettingsRefresh(arg(0))),
        ("hidpp setting apply", 1) => Line::Run(Command::SettingsApply(arg(0))),
        ("hidpp setting get", 2) => Line::Run(Command::SettingGet(arg(0), setting_key(words[1])?)),
        ("hidpp setting forget", 2) => {
            Line::Run(Command::SettingForget(arg(0), setting_key(words[1])?))
        }
        ("hidpp setting set", 3) => Line::Run(Command::SettingSet(
            arg(0),
            setting_key(words[1])?,
            SettingInput::Text(arg(2)),
        )),
        ("device hidpp set", 2) => match flag(1) {
            Some(on) => Line::Run(Command::Hidpp(arg(0), on)),
            None => return usage(),
        },
        ("device enabled set", 2) => match flag(1) {
            Some(on) => Line::Run(Command::Enabled(arg(0), on)),
            None => return usage(),
        },
        ("device trusted set", 2) => match flag(1) {
            Some(on) => Line::Run(Command::Trusted(arg(0), on)),
            None => return usage(),
        },
        ("device blocked set", 2) => match flag(1) {
            Some(on) => Line::Run(Command::Blocked(arg(0), on)),
            None => return usage(),
        },
        ("device get", 1) => Line::Run(Command::Info(arg(0))),
        ("device info", 1) => Line::Run(Command::DeviceInfo(arg(0))),
        ("device info refresh", 1) => Line::Run(Command::DeviceInfoRefresh(arg(0))),
        ("pairing start", 1) => Line::Run(Command::Pair(arg(0))),
        ("device connect", 1) => Line::Run(Command::Connect(arg(0))),
        ("device disconnect", 1) => Line::Run(Command::Disconnect(arg(0))),
        ("device unpair", 1) => Line::Run(Command::Remove(arg(0))),
        ("storage ls", 1) => Line::Run(Command::StorageList(arg(0))),
        ("storage get", 2) if !words[1].is_empty() => Line::Run(Command::StorageGet {
            path: arg(0),
            local: PathBuf::from(words[1]),
            overwrite: false,
        }),
        _ => return usage(),
    };
    Ok(line)
}

/// Checks a command against the connected adapter's capabilities. A scan of
/// both transports becomes a scan of the ones the adapter supports.
pub fn offer(command: Command, st: &State) -> Result<Command, String> {
    let command = match command {
        Command::Scan(t) => match st.capabilities.scan_transport(t) {
            Some(t) => Command::Scan(t),
            None => {
                let reason = Command::Scan(t).unsupported(st);
                return Err(reason
                    .unwrap_or("this adapter doesn't support scanning on that transport")
                    .into());
            }
        },
        command => command,
    };
    match command.unsupported(st) {
        Some(reason) => Err(reason.into()),
        None => Ok(command),
    }
}

/// Turns a typed pairing answer into its reply: yes or no for a comparison,
/// six digits for a passkey, or 1–16 printable ASCII characters for a PIN.
pub fn answer(auth: &Auth, text: &str) -> Result<Command, String> {
    if auth.display {
        return Err("no active authentication question".into());
    }
    if Instant::now() >= auth.expires {
        return Err("authentication prompt expired".into());
    }
    let (action, value) = match auth.prompt.method {
        PromptMethod::ConfirmPasskey => match text.trim().to_lowercase().as_str() {
            "yes" | "y" | "accept" => (PairAction::Accept, None),
            "no" | "n" | "reject" => (PairAction::Reject, None),
            _ => return Err("answer yes or no".into()),
        },
        PromptMethod::EnterPasskey => {
            if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err("passkey must contain exactly six digits".into());
            }
            (PairAction::Accept, Some(text.to_owned()))
        }
        PromptMethod::EnterPin => {
            if text.is_empty() || text.len() > 16 {
                return Err("PIN must contain 1 to 16 printable ASCII characters".into());
            }
            if !text.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
                return Err("PIN must be printable ASCII".into());
            }
            (PairAction::Accept, Some(text.to_owned()))
        }
        _ => return Err("unsupported authentication method".into()),
    };
    Ok(Command::PairReply {
        request: auth.request,
        prompt: auth.prompt.prompt_id.clone(),
        action,
        value,
    })
}

/// Resolves a device word as the controller does: an ID, else a name that
/// identifies exactly one saved device or candidate. Returns the ID and
/// whether it is saved. `pair` resolves with [`resolve_candidate`] instead.
pub fn resolve(word: &str, st: &State) -> Option<(String, bool)> {
    if let Some(d) = st.devices.iter().find(|d| d.device_id.0 == word) {
        return Some((d.device_id.0.clone(), true));
    }
    if let Some(c) = st.candidates.iter().find(|c| c.candidate_id.0 == word) {
        return Some((c.candidate_id.0.clone(), false));
    }
    let mut matches: Vec<(String, bool)> = Vec::new();
    let mut add = |m: (String, bool)| {
        if !matches.iter().any(|(id, _)| *id == m.0) {
            matches.push(m);
        }
    };
    for d in &st.devices {
        if d.name.as_deref() == Some(word) {
            add((d.device_id.0.clone(), true));
        }
    }
    for c in &st.candidates {
        if c.name.as_deref() == Some(word) {
            add((c.candidate_id.0.clone(), false));
        }
    }
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}

/// Resolves a pair target as the controller does: a candidate ID, else a
/// name that exactly one Nearby candidate has. Saved devices are never
/// targets, so a saved device sharing the name doesn't make it ambiguous.
pub fn resolve_candidate(word: &str, st: &State) -> Option<String> {
    if let Some(c) = st.candidates.iter().find(|c| c.candidate_id.0 == word) {
        return Some(c.candidate_id.0.clone());
    }
    let mut named = st
        .candidates
        .iter()
        .filter(|c| c.name.as_deref() == Some(word));
    match (named.next(), named.next()) {
        (Some(c), None) => Some(c.candidate_id.0.clone()),
        _ => None,
    }
}

/// Splits a command line before the word being typed, which may be empty or
/// an unfinished quoted name.
fn last_word(line: &str) -> (&str, &str) {
    let (mut start, mut escaped) = (0, false);
    let mut quote: Option<char> = None;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c.is_whitespace() {
            start = i + c.len_utf8();
        }
    }
    line.split_at(start)
}

/// A setting's fine and coarse integer increments; coarse is zero when the
/// range is small enough for fine steps alone.
pub fn steps(s: &Setting) -> (i64, i64) {
    let fine = s.step.filter(|s| *s > 0).unwrap_or(1);
    match (s.min, s.max) {
        (Some(min), Some(max)) if (max - min) / fine <= 20 => (fine, 0),
        _ => (fine, fine * 10),
    }
}

fn shell_word(word: String) -> String {
    if word.contains(char::is_whitespace) || word.contains(['"', '\'', '\\']) {
        quote(&word)
    } else {
        word
    }
}

/// A setting's legal values as words: on and off, its choices, or a short
/// integer range.
fn value_words(s: &Setting) -> Vec<String> {
    if s.kind == SettingType::Bool {
        return vec!["on".into(), "off".into()];
    }
    if !s.choices.is_empty() {
        return s
            .choices
            .iter()
            .map(|c| shell_word(value_string(c)))
            .collect();
    }
    if let (SettingType::Integer, Some(min), Some(max)) = (s.kind, s.min, s.max) {
        let (fine, _) = steps(s);
        if (max - min) / fine < 16 {
            return (0..)
                .map(|i| min + i * fine)
                .take_while(|n| *n <= max)
                .map(|n| n.to_string())
                .collect();
        }
    }
    Vec::new()
}

/// Device and candidate IDs, and quoted names that identify exactly one,
/// for those whose ID `accepts`.
fn device_words(st: &State, cmd: &str, accepts: impl Fn(&str) -> bool) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let names = st
        .devices
        .iter()
        .map(|d| (d.device_id.0.clone(), d.name.clone()))
        .chain(
            st.candidates
                .iter()
                .map(|c| (c.candidate_id.0.clone(), c.name.clone())),
        );
    for (id, name) in names {
        if !accepts(&id) {
            continue;
        }
        // Pair names resolve among candidates only, as the controller does.
        let names_id = |n: &String| match cmd {
            "pairing start" => resolve_candidate(n, st).is_some_and(|r| r == id),
            _ => resolve(n, st).is_some_and(|(r, _)| r == id),
        };
        if let Some(name) = name.filter(names_id) {
            words.push(quote(&name));
        }
        words.push(id);
    }
    words.sort();
    words.dedup();
    words
}

/// The command a device word completes for, to offer only the devices the
/// adapter can act on with it.
fn device_command(cmd: &str, id: &str) -> Option<Command> {
    let id = id.to_owned();
    Some(match sample(cmd)? {
        Command::Info(_) => Command::Info(id),
        Command::DeviceInfo(_) => Command::DeviceInfo(id),
        Command::DeviceInfoRefresh(_) => Command::DeviceInfoRefresh(id),
        Command::Pair(_) => Command::Pair(id),
        Command::Connect(_) => Command::Connect(id),
        Command::Disconnect(_) => Command::Disconnect(id),
        Command::Remove(_) => Command::Remove(id),
        Command::Enabled(..) => Command::Enabled(id, true),
        Command::Trusted(..) => Command::Trusted(id, true),
        Command::Blocked(..) => Command::Blocked(id, true),
        Command::Hidpp(..) => Command::Hidpp(id, true),
        Command::Features(_) => Command::Features(id),
        Command::Settings(_) => Command::Settings(id),
        _ => return None,
    })
}

/// Whether a device's state allows the command: only scanned candidates
/// pair, a device that needs pairing again can't be connected, and policy
/// applies only to saved devices.
fn usable(cmd: &str, id: &str, st: &State) -> bool {
    let saved = st.devices.iter().find(|d| d.device_id.0 == id);
    match cmd {
        "pairing start" => saved.is_none(),
        "device connect" => saved.is_none_or(|d| d.effective_enabled),
        "device enabled set"
        | "device trusted set"
        | "device blocked set"
        | "device info"
        | "device info refresh" => saved.is_some(),
        _ => true,
    }
}

/// The value that changes a saved device's policy, or both without one.
fn policy_words(cmd: &str, word: &str, st: &State) -> Vec<String> {
    let current = resolve(word, st)
        .filter(|(_, saved)| *saved)
        .and_then(|(id, _)| st.devices.iter().find(|d| d.device_id.0 == id))
        .map(|d| match cmd {
            "device enabled set" => d.enabled,
            "device trusted set" => d.trusted,
            "device blocked set" => d.blocked,
            _ => d.hidpp_enabled,
        });
    match current {
        Some(true) => vec!["off".into()],
        Some(false) => vec!["on".into()],
        None => vec!["on".into(), "off".into()],
    }
}

/// The cached settings of a saved device named in a command line. Completion
/// never reads from the adapter.
fn known_settings<'a>(word: &str, st: &'a State) -> Vec<&'a Setting> {
    let Some((id, true)) = resolve(word, st) else {
        return Vec::new();
    };
    st.settings
        .iter()
        .find(|(d, _)| d.0 == id)
        .map(|(_, c)| catalog::presented(&c.settings))
        .unwrap_or_default()
}

/// The words that can follow `words`. Connected, only what the adapter
/// offers is completed.
fn completions(words: &[String], st: Option<&State>) -> Vec<String> {
    let owned = |list: &[&str]| list.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let typed: Vec<&str> = words.iter().map(String::as_str).collect();
    let offers = |w: &str| st.is_none_or(|st| offered(w, st));
    // Command words: the next word of every offered command they begin.
    let mut next: Vec<String> = Vec::new();
    for s in SPECS.iter().filter(|s| offers(s.words)) {
        let parts: Vec<&str> = s.words.split(' ').collect();
        if parts.len() > typed.len() && parts[..typed.len()] == typed[..] {
            let w = parts[typed.len()].to_owned();
            if !next.contains(&w) {
                next.push(w);
            }
        }
    }
    // A complete command that also begins a longer one, such as device info
    // and device info refresh, offers both the next word and its arguments.
    let exact = find(&typed).ok().filter(|(_, n)| *n == typed.len());
    if !next.is_empty() && exact.is_none() {
        return next;
    }
    let Some((spec, n)) = exact.or_else(|| find(&typed).ok()) else {
        return next;
    };
    let cmd = spec.words;
    if !offers(cmd) {
        return next;
    }
    let args = &words[n..];
    let devices = |cmd: &str| {
        st.map(|st| {
            device_words(st, cmd, |id| {
                device_command(cmd, id).is_none_or(|c| c.supported(st)) && usable(cmd, id, st)
            })
        })
        .unwrap_or_default()
    };
    let arguments = match (cmd, args.len()) {
        ("adapter platform set", 0) => owned(&["linux", "windows", "mac"]),
        ("session monitor set", 0) => owned(&["on", "off"]),
        (SCAN, 0) => owned(&scan_words(st)),
        ("device list", 0) => owned(&["Saved", "Paired", "Enabled", "Connected", "Trusted"]),
        ("help", 0) => heads(st).into_iter().map(str::to_owned).collect(),
        (
            "device enabled set" | "device trusted set" | "device blocked set" | "device hidpp set",
            1,
        ) => st
            .map(|st| policy_words(cmd, &args[0], st))
            .unwrap_or_else(|| owned(&["on", "off"])),
        ("hidpp setting get" | "hidpp setting set" | "hidpp setting forget", 1) => st
            .map(|st| known_settings(&args[0], st))
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.writable || cmd == "hidpp setting get")
            .map(|s| crate::ui::text::wire(&s.key))
            .collect(),
        ("hidpp setting set", 2) => st
            .map(|st| known_settings(&args[0], st))
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.writable && crate::ui::text::wire(&s.key) == args[1])
            .map(value_words)
            .unwrap_or_default(),
        (_, 0) if device_command(cmd, "").is_some() => devices(cmd),
        ("hidpp setting refresh" | "hidpp setting apply", 0) => devices(cmd),
        _ => Vec::new(),
    };
    next.extend(arguments);
    next
}

/// Complete command lines for Tab to cycle through from `base`.
pub fn complete(base: &str, st: Option<&State>) -> Vec<String> {
    if !base.contains(char::is_whitespace) {
        return heads(st)
            .into_iter()
            .filter(|c| c.starts_with(base))
            .map(|c| format!("{c} "))
            .collect();
    }
    let (head, partial) = last_word(base);
    let Ok(words) = split(head) else {
        return Vec::new();
    };
    if words.is_empty() {
        return Vec::new();
    }
    let command_word = |v: &str| {
        SPECS.iter().any(|s| {
            let parts: Vec<&str> = s.words.split(' ').collect();
            parts.len() > words.len()
                && parts[..words.len()].iter().zip(&words).all(|(a, b)| a == b)
                && parts[words.len()] == v
        })
    };
    completions(&words, st)
        .into_iter()
        .filter(|v| v.starts_with(partial))
        .map(|v| {
            // Command words take a space so the next word can follow.
            let space = if command_word(&v) { " " } else { "" };
            format!("{head}{v}{space}")
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::controller::DeviceSettings;
    use cordial_protocol::settings::SettingValue;
    use cordial_protocol::{
        identifiers::*,
        messages::{
            Candidate, Capabilities, Capability, ConnectionSecurity, Device, DeviceKind, Status,
        },
    };

    fn words(line: &str) -> Vec<String> {
        split(line).unwrap()
    }

    pub fn device(id: &str, name: &str) -> Device {
        Device {
            device_id: DeviceId(id.into()),
            pairing_state: PairingState::Paired,
            name: Some(name.into()),
            transport: Transport::Ble,
            roles: vec![Role::Keyboard],
            state: ConnectionState::Connected,
            security: Some(ConnectionSecurity {
                encrypted: Some(true),
                authenticated: Some(false),
                secure_connections: Some(true),
                key_size: Some(16),
                bonded: Some(true),
            }),
            enabled: true,
            effective_enabled: true,
            enabled_reason: None,
            transport_supported: true,
            validation_error: None,
            trusted: true,
            blocked: false,
            reconnect: Reconnect::Auto,
            last_error: None,
            warnings: Vec::new(),
            hidpp_enabled: true,
            normalization_state: NormalizationState::Active,
            normalization_error: None,
            settings_state: SettingsState::Ready,
            settings_error: None,
            settings_revision: 0,
        }
    }

    pub fn candidate(id: &str, name: &str) -> Candidate {
        Candidate {
            candidate_id: CandidateId(id.into()),
            name: Some(name.into()),
            kind: DeviceKind::Keyboard,
            transport: Transport::Ble,
            rssi: Some(-55),
        }
    }

    pub fn status() -> Status {
        serde_json::from_value(serde_json::json!({
            "protocol": 1, "firmware_version": "0.0.0", "hardware_config": "pico_w",
            "radio_backend":"pico-sdk-cyw43","hardware_digest": "abc", "adapter_id": "E6613008E35A4733",
            "build_profile": "development", "boot_id": "b", "session_id": "s",
            "limits": {"max_line_bytes": 4096, "max_pending_requests": 4, "saved_devices": 8,
                "active_connections": 4, "scan_candidates": 16, "hidpp_settings": 32,
                "hidpp_saved_settings": 16, "hidpp_sensors": 2, "hidpp_firmware_entities": 2,
                "hidpp_setting_choices": 16, "hidpp_features": 256},
            "counts": {"saved": 2, "paired": 2, "preferred_enabled": 2, "enabled": 2, "connected": 2},
            "capacity": {"enabled": [{"transports": ["classic", "ble"], "limit": 7, "enabled": 2}],
                "pairing": [{"transport": "classic", "available": true, "reason": null, "estimated_additional": 20},
                    {"transport": "ble", "available": true, "reason": null, "estimated_additional": 20}]},
            "revision": 3, "name": "Test adapter", "host_platform": "linux",
            "monitor": true, "radio_ready": true, "storage_ready": true,
            "heartbeat": {"interval_ms": 5000, "timeout_ms": 15000, "remaining_ms": 15000},
            "pending": []
        }))
        .unwrap()
    }

    pub fn state() -> State {
        State {
            session: 1,
            port: "/dev/ttyACM0".into(),
            capabilities: Capabilities(vec![
                Capability::Classic,
                Capability::Ble,
                Capability::Debug,
                Capability::StorageManagement,
            ]),
            status: status(),
            devices: vec![device("d_1", "Test keyboard"), device("d_2", "Mouse")],
            candidates: vec![
                candidate("c_1", "Test keyboard"),
                candidate("c_2", "Other one"),
            ],
            pending: Vec::new(),
            auth: None,
            available: true,
            current: true,
            monitor: true,
            saturated: false,
            revision: 3,
            ready: true,
            waiting: false,
            ready_error: None,
            settings: Default::default(),
            info: Default::default(),
        }
    }

    #[test]
    fn split_quotes_and_escapes() {
        assert_eq!(words(r#"pair "Test keyboard""#), ["pair", "Test keyboard"]);
        assert_eq!(
            words(r"pair Test\ keyboard ''"),
            ["pair", "Test keyboard", ""]
        );
        assert_eq!(words(r#"a 'b\c' "d\"e""#), ["a", r"b\c", r#"d"e"#]);
        assert!(split(r#"pair "unfinished"#).is_err());
        assert!(split("pair x\\").is_err());
    }

    #[test]
    fn parse_resource_actions_and_usage() {
        assert!(
            matches!(parse(&words("device unpair d_1")), Ok(Line::Run(Command::Remove(d))) if d == "d_1")
        );
        assert!(matches!(
            parse(&words("device list Paired")),
            Ok(Line::Devices(Filter::Paired))
        ));
        assert!(matches!(
            parse(&words("discovery scan le")),
            Ok(Line::Run(Command::Scan(ScanTransport::Ble)))
        ));
        assert!(matches!(
            parse(&words("adapter status")),
            Ok(Line::Run(Command::Status))
        ));
        assert!(matches!(
            parse(&words("adapter capabilities")),
            Ok(Line::Run(Command::Capabilities))
        ));
        assert!(matches!(parse(&words("adapter list")), Ok(Line::List)));
        assert!(
            matches!(parse(&words("adapter select /dev/x")), Ok(Line::Select(p)) if p == "/dev/x")
        );
        for (line, on) in [
            ("device enabled set d_1 on", true),
            ("device enabled set d_1 off", false),
        ] {
            assert!(
                matches!(parse(&words(line)), Ok(Line::Run(Command::Enabled(d, v))) if d == "d_1" && v == on)
            );
        }
        assert!(matches!(
            parse(&words("device trusted set d_1 off")),
            Ok(Line::Run(Command::Trusted(_, false)))
        ));
        assert!(matches!(
            parse(&words("device blocked set d_1 on")),
            Ok(Line::Run(Command::Blocked(_, true)))
        ));
        assert!(matches!(
            parse(&words("device hidpp set d_1 on")),
            Ok(Line::Run(Command::Hidpp(_, true)))
        ));
        assert!(matches!(
            parse(&words("adapter name reset")),
            Ok(Line::Run(Command::Name(None)))
        ));
        assert!(matches!(
            parse(&words("adapter name set \"Desk keyboard\"")),
            Ok(Line::Run(Command::Name(Some(name)))) if name == "Desk keyboard"
        ));
        assert!(matches!(
            parse(&words("adapter platform set mac")),
            Ok(Line::Run(Command::Platform(HostPlatform::Mac)))
        ));
        assert!(matches!(
            parse(&words("adapter bootloader enter")),
            Ok(Line::Run(Command::Bootloader))
        ));
        assert!(matches!(
            parse(&words("session monitor set off")),
            Ok(Line::Run(Command::Monitor(false)))
        ));
        assert!(matches!(
            parse(&words("storage ls /")),
            Ok(Line::Run(Command::StorageList(p))) if p == "/"
        ));
        assert!(matches!(
            parse(&words("storage get /device.json out.json")),
            Ok(Line::Run(Command::StorageGet { path, local, overwrite: false }))
                if path == "/device.json" && local == *"out.json"
        ));
        assert_eq!(
            parse(&words("discovery scan up")).unwrap_err(),
            "invalid arguments for discovery scan; use help"
        );
        assert_eq!(
            parse(&words("device enabled set d_1 yes")).unwrap_err(),
            "invalid arguments for device enabled set; use help"
        );
        assert_eq!(
            parse(&words("device frob")).unwrap_err(),
            "unknown \"device\" command; use help"
        );
        assert_eq!(
            parse(&words("help frob")).unwrap_err(),
            "unknown command \"frob\""
        );
        assert!(matches!(
            parse(&words("help device")),
            Ok(Line::Help(Some(_)))
        ));
        assert!(parse(&words("request cancel 0")).is_err());
        assert!(parse(&words("pairing reply 5 p_1 reject 1")).is_err());
        match parse(&words("pairing reply 5 p_1 accept 042731")) {
            Ok(Line::Run(Command::PairReply { request, value, .. })) => {
                assert_eq!(request.get(), 5);
                assert_eq!(value.as_deref(), Some("042731"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse(&words("hidpp setting set d_1 wheel.mode ratchet")),
            Ok(Line::Run(Command::SettingSet(
                _,
                SettingKey::WheelMode,
                SettingInput::Text(_)
            )))
        ));
        assert!(
            parse(&words("hidpp setting get d_1 Wheel"))
                .unwrap_err()
                .contains("is not a setting key")
        );
        assert!(
            parse(&words("hidpp setting get d_1 wheel.x"))
                .unwrap_err()
                .contains("doesn't recognize")
        );
        assert!(matches!(
            parse(&words("hidpp setting list d_1")),
            Ok(Line::Run(Command::Settings(_)))
        ));
        assert!(parse(&words("hidpp setting refresh")).is_err());
        assert!(matches!(
            parse(&words("device info d_1")),
            Ok(Line::Run(Command::DeviceInfo(d))) if d == "d_1"
        ));
        assert!(matches!(
            parse(&words("device info refresh d_1")),
            Ok(Line::Run(Command::DeviceInfoRefresh(d))) if d == "d_1"
        ));
        assert!(matches!(
            parse(&words("device get d_1")),
            Ok(Line::Run(Command::Info(_)))
        ));
        assert!(parse(&words("device info refresh")).is_err());
    }

    #[test]
    fn device_info_completes_refresh_and_saved_devices() {
        let st = state();
        let c = complete("device info ", Some(&st));
        assert!(c.contains(&"device info refresh ".to_owned()), "{c:?}");
        assert!(c.contains(&"device info d_1".to_owned()), "{c:?}");
        assert!(
            !c.iter().any(|w| w.contains("c_")),
            "candidates have no info: {c:?}"
        );
        let c = complete("device info refresh ", Some(&st));
        assert!(c.contains(&"device info refresh d_2".to_owned()), "{c:?}");
    }

    #[test]
    fn names_resolve_only_when_unique() {
        let mut st = state();
        assert_eq!(resolve("d_2", &st), Some(("d_2".into(), true)));
        assert_eq!(
            resolve("Test keyboard", &st),
            None,
            "saved and nearby share the name"
        );
        assert_eq!(resolve("Other one", &st), Some(("c_2".into(), false)));
        // Candidates are never associated with saved devices.
        assert_eq!(resolve("c_1", &st), Some(("c_1".into(), false)));
        st.candidates[0].name = Some("Nearby only".into());
        assert_eq!(resolve("Test keyboard", &st), Some(("d_1".into(), true)));
    }

    #[test]
    fn pair_targets_resolve_among_candidates_only() {
        let mut st = state();
        // A saved device sharing the name doesn't make the candidate ambiguous.
        assert_eq!(resolve_candidate("Test keyboard", &st), Some("c_1".into()));
        assert_eq!(resolve_candidate("c_2", &st), Some("c_2".into()));
        assert_eq!(resolve_candidate("d_1", &st), None, "saved IDs never pair");
        assert_eq!(
            resolve_candidate("Mouse", &st),
            None,
            "saved names never pair"
        );
        let pair = complete("pairing start ", Some(&st));
        assert!(
            pair.contains(&"pairing start \"Test keyboard\"".to_owned()),
            "{pair:?}"
        );
        // The shell hands the typed word to the controller unchanged.
        assert!(matches!(
            parse(&words(r#"pairing start "Test keyboard""#)),
            Ok(Line::Run(Command::Pair(t))) if t == "Test keyboard"
        ));
        // Two Nearby entries sharing a name are ambiguous.
        st.candidates[1].name = Some("Test keyboard".into());
        assert_eq!(resolve_candidate("Test keyboard", &st), None);
        let pair = complete("pairing start ", Some(&st));
        assert!(
            !pair.iter().any(|w| w.contains("Test keyboard")),
            "{pair:?}"
        );
        assert!(
            pair.contains(&"pairing start c_1".to_owned())
                && pair.contains(&"pairing start c_2".to_owned())
        );
    }

    #[test]
    fn completion_walks_command_words_then_arguments() {
        let mut st = state();
        assert_eq!(complete("dev", Some(&st)), ["device "]);
        let c = complete("device ", Some(&st));
        for w in ["list", "get", "connect", "enabled", "unpair", "hidpp"] {
            assert!(c.contains(&format!("device {w} ")), "{w}: {c:?}");
        }
        assert_eq!(complete("device en", Some(&st)), ["device enabled "]);
        assert_eq!(
            complete("device enabled ", Some(&st)),
            ["device enabled set "]
        );
        let c = complete("device connect ", Some(&st));
        assert!(
            c.contains(&"device connect d_1".to_string())
                && c.contains(&"device connect \"Other one\"".to_string())
        );
        assert!(!c.iter().any(|w| w.contains("Test keyboard")));
        // Policy completes the value that changes the device.
        assert_eq!(
            complete("device enabled set d_1 ", Some(&st)),
            ["device enabled set d_1 off"]
        );
        st.devices[0].trusted = false;
        assert_eq!(
            complete("device trusted set d_1 ", Some(&st)),
            ["device trusted set d_1 on"]
        );
        assert!(
            !complete("device blocked set ", Some(&st))
                .iter()
                .any(|w| w.contains("c_"))
        );
        assert_eq!(
            complete("device hidpp set d_1 o", Some(&st)),
            ["device hidpp set d_1 off"]
        );
        assert_eq!(
            complete("adapter platform set w", None),
            ["adapter platform set windows"]
        );
        assert_eq!(complete("sto", Some(&st)), ["storage "]);
        assert_eq!(
            complete("storage ", Some(&st)),
            ["storage ls ", "storage get "]
        );
        let mut mode = catalog::tests::setting(SettingKey::WheelMode);
        mode.writable = true;
        mode.choices = vec![
            SettingValue::Text("freespin".into()),
            SettingValue::Text("ratchet".into()),
        ];
        let info = catalog::tests::setting(SettingKey::WheelInfo);
        st.settings.insert(
            DeviceId("d_2".into()),
            DeviceSettings {
                loaded: true,
                current: true,
                settings: vec![info, mode],
                ..Default::default()
            },
        );
        assert_eq!(
            complete("hidpp setting set Mouse w", Some(&st)),
            ["hidpp setting set Mouse wheel.mode"]
        );
        assert_eq!(complete("hidpp setting get d_2 ", Some(&st)).len(), 2);
        assert_eq!(
            complete("hidpp setting set d_2 wheel.mode r", Some(&st)),
            ["hidpp setting set d_2 wheel.mode ratchet"]
        );
    }

    /// The fixture adapter with only these capabilities.
    pub fn offering(caps: &[Capability]) -> State {
        let mut st = state();
        st.capabilities = Capabilities(caps.to_vec());
        st
    }

    #[test]
    fn scan_follows_independent_transports() {
        use Capability::{Ble, Classic};
        let cases: [(&[Capability], &[&str], &str); 4] = [
            (
                &[Classic, Ble],
                &["on", "le", "bredr", "off"],
                "discovery scan on | le | bredr | off",
            ),
            (&[Ble], &["le", "off"], "discovery scan le | off"),
            (&[Classic], &["bredr", "off"], "discovery scan bredr | off"),
            (&[], &[], ""),
        ];
        for (caps, words, syntax) in cases {
            let st = offering(caps);
            let scans: Vec<String> = complete("discovery scan ", Some(&st))
                .into_iter()
                .map(|w| w["discovery scan ".len()..].to_owned())
                .collect();
            assert_eq!(scans, words, "{caps:?}");
            let text = help(Some(&st));
            if syntax.is_empty() {
                assert!(!text.contains("\ndiscovery scan"), "{text}");
                assert!(complete("disc", Some(&st)).is_empty());
                assert!(offer(Command::Scan(ScanTransport::Both), &st).is_err());
            } else {
                assert!(text.contains(syntax), "{text}");
            }
        }
        let ble = offering(&[Ble]);
        assert!(help(Some(&ble)).contains("Discover BLE devices, or stop"));
        assert!(matches!(
            offer(Command::Scan(ScanTransport::Both), &ble),
            Ok(Command::Scan(ScanTransport::Ble))
        ));
        assert!(offer(Command::Scan(ScanTransport::Classic), &ble).is_err());
    }

    #[test]
    fn devices_complete_only_on_their_transport() {
        let classic = offering(&[Capability::Classic]);
        assert!(
            complete("pairing start ", Some(&classic)).is_empty(),
            "fixture devices are BLE"
        );
        assert!(complete("device connect ", Some(&classic)).is_empty());
        assert!(
            !complete("device trusted set ", Some(&classic)).is_empty(),
            "management needs no radio"
        );
        let ble = offering(&[Capability::Ble]);
        assert!(complete("pairing start ", Some(&ble)).contains(&"pairing start c_2".to_owned()));
    }

    #[test]
    fn no_transports_keeps_management_and_files() {
        let st = offering(&[Capability::StorageManagement]);
        let text = help(Some(&st));
        for absent in [
            "\npairing start",
            "\ndevice connect",
            "\ndiscovery scan",
            "\nadapter bootloader",
        ] {
            assert!(!text.contains(absent), "{absent} in {text}");
        }
        for present in [
            "\ndevice trusted set DEV on | off",
            "\ndevice unpair DEV",
            "\nhidpp setting list DEV",
            "\nadapter platform set",
            "\npairing reply",
            "\nstorage ls PATH",
            "\nstorage get PATH LOCAL_FILE",
        ] {
            assert!(text.contains(present), "{present} missing from {text}");
        }
        assert_eq!(complete("pairing ", Some(&st)), ["pairing reply "]);
        assert!(offer(Command::Pair("c_2".into()), &st).is_err());
        assert!(offer(Command::Trusted("d_1".into(), true), &st).is_ok());
    }

    #[test]
    fn optional_capabilities_gate_files_and_bootloader() {
        let mut st = offering(&[Capability::Ble]);
        st.candidates.clear();
        let text = help(Some(&st));
        assert!(
            !text.contains("\nstorage") && !text.contains("\nadapter bootloader"),
            "{text}"
        );
        assert!(!text.contains("PATH is absolute"));
        assert!(
            text.contains("except adapter status and adapter capabilities."),
            "{text}"
        );
        assert!(complete("sto", Some(&st)).is_empty());
        assert!(!complete("adapter ", Some(&st)).contains(&"adapter bootloader ".to_owned()));
        assert!(offer(Command::Bootloader, &st).is_err());
        assert!(offer(Command::StorageList("/".into()), &st).is_err());
        assert!(offer(Command::Status, &st).is_ok(), "status is mandatory");
        assert!(offer(Command::Capabilities, &st).is_ok());
        // The build profile is diagnostic; only capabilities gate features.
        let mut dev = offering(&[Capability::Debug, Capability::StorageManagement]);
        dev.status.build_profile = cordial_protocol::messages::BuildProfile::Production;
        assert!(offer(Command::Bootloader, &dev).is_ok());
        assert!(help(Some(&dev)).contains("\nstorage ls"));
    }

    #[test]
    fn standalone_help_documents_everything() {
        let text = help(None);
        for row in [
            "discovery scan on | le | bredr | off",
            "\npairing start CANDIDATE",
            "\nadapter bootloader enter",
            "\ndevice trusted set DEV on | off",
            "\nstorage get PATH LOCAL_FILE",
            "\nquit | exit",
            "Ctrl-C clears input, rejects a prompt, or cancels a foreground operation.",
            "except adapter status, adapter capabilities, storage ls, storage get and adapter bootloader enter.",
        ] {
            assert!(text.contains(row), "{row}");
        }
        assert_eq!(complete("adapter boot", None), ["adapter bootloader "]);
        assert_eq!(
            complete("", None),
            [
                "adapter ",
                "discovery ",
                "device ",
                "pairing ",
                "hidpp ",
                "session ",
                "request ",
                "storage ",
                "help ",
                "quit ",
                "exit "
            ]
        );
    }

    #[test]
    fn answers_follow_the_prompt() {
        let prompt = |method| Auth {
            request: RequestId::try_from(5).unwrap(),
            prompt: cordial_protocol::messages::Prompt {
                candidate_id: CandidateId("c_1".into()),
                prompt_id: "p_1".into(),
                method,
                expires_in_ms: 30000,
                value: None,
            },
            display: false,
            expires: Instant::now() + std::time::Duration::from_secs(30),
            expires_in_ms: 30000,
        };
        let a = prompt(PromptMethod::EnterPasskey);
        assert_eq!(
            answer(&a, "12345").unwrap_err(),
            "passkey must contain exactly six digits"
        );
        assert!(
            matches!(answer(&a, "012345"), Ok(Command::PairReply { value: Some(v), .. }) if v == "012345")
        );
        let c = prompt(PromptMethod::ConfirmPasskey);
        assert!(matches!(
            answer(&c, " N "),
            Ok(Command::PairReply {
                action: PairAction::Reject,
                value: None,
                ..
            })
        ));
        assert_eq!(answer(&c, "maybe").unwrap_err(), "answer yes or no");
        let p = prompt(PromptMethod::EnterPin);
        assert_eq!(answer(&p, "é").unwrap_err(), "PIN must be printable ASCII");
        let mut expired = prompt(PromptMethod::EnterPin);
        expired.expires = Instant::now();
        assert_eq!(
            answer(&expired, "1234").unwrap_err(),
            "authentication prompt expired"
        );
    }

    #[test]
    fn pairing_state_limits_pair_and_connect() {
        let mut st = state();
        st.devices[1].pairing_state = PairingState::NeedsPairing;
        st.devices[1].effective_enabled = false;
        st.devices[1].enabled_reason = Some(cordial_protocol::errors::DisabledReason::Invalid);
        let connect = complete("device connect ", Some(&st));
        assert!(
            connect.contains(&"device connect d_1".to_owned()),
            "{connect:?}"
        );
        assert!(
            !connect.contains(&"device connect d_2".to_owned()),
            "{connect:?}"
        );
        assert!(complete("device list S", Some(&st)).contains(&"device list Saved".to_owned()));
        let paired = crate::ui::text::devices(&st, Filter::Paired);
        assert!(
            paired.contains("d_1") && !paired.contains("d_2"),
            "{paired}"
        );
        let saved = crate::ui::text::devices(&st, Filter::Saved);
        assert!(saved.contains("d_2  Mouse  ble  needs_pairing"), "{saved}");
        let all = crate::ui::text::devices(&st, Filter::All);
        assert!(
            all.contains("c_1  Test keyboard  ble  candidate\n"),
            "{all}"
        );
    }
}
