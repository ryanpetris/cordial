//! The shell's command language: resource/action commands named after the
//! protocol's commands, pairing answers and completion.
use crate::{
    commands::{self, MAX_SCAN_SECONDS},
    controller::{Command, SettingInput, State, Toggle},
    model::{self, Prompt, Type},
    ui::{
        catalog::{self, value_string},
        text::{Filter, quote},
    },
};
use cordial_protocol::{self as p, Platform, Transport};
use std::path::PathBuf;

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
const SCAN: &str = "scan start";

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
        "Adapter identity, readiness, platform and capacity",
    ),
    spec("adapter set name", "NAME", "Rename adapter"),
    spec("adapter reset name", "", "Reset adapter name to default"),
    spec(
        "adapter set platform",
        "linux | windows | mac",
        "OS whose shortcuts HID++ keys send (saved on the adapter)",
    ),
    spec(
        "adapter bootloader",
        "",
        "Development only; HID stops while in BOOTSEL",
    ),
    spec(SCAN, "", ""),
    spec("scan stop", "", "Stop discovery"),
    spec(
        "device list",
        "[Saved|Enabled|Connected|Trusted]",
        "Saved devices, including disabled ones, and discovered candidates",
    ),
    spec("device get", "DEV", "Device details"),
    spec(
        "pair start",
        "CANDIDATE",
        "Pair a Nearby device by candidate ID or a name only one Nearby device has; one already saved pairs again, keeping its settings; saved disabled when no enabled place is free; then connect",
    ),
    spec("pair accept", "[VALUE]", "Answer a pairing prompt"),
    spec("pair reject", "", "Reject a pairing prompt"),
    spec("pair cancel", "", "Cancel the pairing"),
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
        "device unpair",
        "DEV",
        "Disconnect and remove the saved bond; a Nearby entry is hidden",
    ),
    spec(
        "device refresh",
        "DEV",
        "Ask a connected device for current information and settings",
    ),
    spec(
        "device set",
        "DEV enabled | trusted | blocked | hidpp on | off",
        "Enabled uses a saved device for connections and needs a free enabled place; trusted allows unattended connections; blocked denies connections; hidpp allows HID++ special keys and applying device settings",
    ),
    spec("warning list", "DEV", "Device warnings"),
    spec(
        "setting list",
        "DEV",
        "Device settings: last observed values and saved preferences",
    ),
    spec("setting get", "DEV KEY", "One setting in detail"),
    spec(
        "setting set",
        "DEV KEY VALUE",
        "Save a preference on the dongle; applied now if HID++ is on",
    ),
    spec(
        "setting forget",
        "DEV KEY",
        "Default: forget the saved value; leave the device unchanged",
    ),
    spec(
        "feature list",
        "DEV",
        "HID++ features discovered on a connected device",
    ),
    spec(
        "file list",
        "PATH",
        "List a directory of the adapter's filesystem",
    ),
    spec(
        "file get",
        "PATH LOCAL_FILE",
        "Download an adapter file; an existing local file is never replaced",
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
            "device connect",
            "device disconnect",
            "device unpair",
            "device refresh",
            "device set",
            "warning",
            "setting",
            "feature",
        ],
        "DEV is an opaque ID or an unambiguous name. Quote names containing spaces.",
    ),
    (
        &["pair accept"],
        "During a pairing prompt, enter the requested answer, or /COMMAND to run a command.",
    ),
    (&["ctrl-c"], ""),
    (
        &["scan start"],
        "A scan runs for SECONDS, 10 by default and at most 60, or until scan stop or exit. No service runs.",
    ),
    (&["direct"], ""),
    (
        &["file"],
        "PATH is absolute on the adapter, such as /. A download is kept only when complete.",
    ),
    (
        &["setting"],
        "
Device settings are saved only when you set them. Saved values are reapplied
when the device reconnects with HID++ on, when HID++ is turned on, and after
an adapter platform change, overriding changes made on the device or from
another computer meanwhile.
Changes made on the device are shown but never saved or corrected. A setting's
values come from the device: see setting get. With HID++ off, setting set only
saves the value on the dongle without applying it. While disconnected,
setting list shows what the adapter last knew, and setting forget still
returns a setting to Default. Only settings this cordial recognizes can be
shown or changed.",
    ),
];

/// Commands that inspect or recover an adapter, or read its files, while it
/// is still starting or failed to, so they run without waiting for readiness.
pub const DIRECT: &[&str] = &[
    "adapter status",
    "file list",
    "file get",
    "adapter bootloader",
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
                out.push(
                    "Ctrl-C clears input, rejects a prompt, or cancels a foreground operation."
                        .into(),
                );
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
        .map(|w| match *w {
            "ble" => "BLE",
            _ => "Classic",
        })
        .collect();
    let text = match kinds.len() {
        1 => format!("Discover {} devices", kinds[0]),
        _ => "Discover devices on both transports, or one".to_owned(),
    };
    let syntax = match words.len() {
        1 => format!("{SCAN} [{}] [SECONDS]", words[0]),
        _ => format!("{SCAN} [{}] [SECONDS]", words.join(" | ")),
    };
    (syntax, text)
}

/// The transports a scan can name.
fn scan_words(st: Option<&State>) -> Vec<&'static str> {
    let Some(st) = st else {
        return vec!["classic", "ble"];
    };
    model::transports(&st.status)
        .into_iter()
        .filter_map(|t| match t {
            Transport::Classic => Some("classic"),
            Transport::Ble => Some("ble"),
            Transport::Unspecified => None,
        })
        .collect()
}

/// Whether the connected adapter offers a shell command. Local commands are
/// always offered.
fn offered(words: &str, st: &State) -> bool {
    let development = model::development(&st.status);
    match words {
        SCAN | "scan stop" => !scan_words(Some(st)).is_empty(),
        "adapter bootloader" | "feature list" | "file list" | "file get" => development,
        _ => true,
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

fn setting_key(word: &str) -> Result<String, String> {
    let valid = !word.is_empty()
        && word.len() <= 64
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if !valid {
        return Err(format!(
            "{} is not a setting key; use setting list DEV to list them",
            quote(word)
        ));
    }
    if catalog::info_for(word).is_none() {
        return Err(format!(
            "Cordial doesn't recognize the setting {word}; a newer version may support it"
        ));
    }
    Ok(word.to_owned())
}

fn on_off(word: &str) -> Option<bool> {
    match word {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

fn toggle(word: &str) -> Option<Toggle> {
    Some(match word {
        "enabled" => Toggle::Enabled,
        "trusted" => Toggle::Trusted,
        "blocked" => Toggle::Blocked,
        "hidpp" => Toggle::Hidpp,
        _ => return None,
    })
}

fn transport(word: &str) -> Option<Transport> {
    match word {
        "classic" => Some(Transport::Classic),
        "ble" => Some(Transport::Ble),
        _ => None,
    }
}

fn seconds(word: &str) -> Option<u32> {
    word.parse()
        .ok()
        .filter(|s| (1..=MAX_SCAN_SECONDS).contains(s))
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
    let line = match (cmd, words.len()) {
        ("help", 0) => Line::Help(None),
        ("help", _) if listed_prefix(&words.join(" "), None) => Line::Help(Some(words.join(" "))),
        ("help", _) => return Err(format!("unknown command {}", quote(&words.join(" ")))),
        ("quit" | "exit", 0) => Line::Quit,
        ("adapter select", 1) if !words[0].is_empty() => Line::Select(arg(0)),
        ("adapter list", 0) => Line::List,
        ("adapter status", 0) => Line::Run(Command::Status),
        ("adapter set name", 1) if !words[0].is_empty() => Line::Run(Command::Name(Some(arg(0)))),
        ("adapter reset name", 0) => Line::Run(Command::Name(None)),
        ("adapter set platform", 1) => Line::Run(Command::Platform(match words[0] {
            "linux" => Platform::Linux,
            "windows" => Platform::Windows,
            "mac" => Platform::Mac,
            _ => return usage(),
        })),
        ("adapter bootloader", 0) => Line::Run(Command::Bootloader),
        (SCAN, 0..=2) => {
            let mut transports = Vec::new();
            let mut time = 0;
            for (i, w) in words.iter().enumerate() {
                match (transport(w), seconds(w)) {
                    (Some(t), _) if i == 0 => transports.push(t),
                    (_, Some(s)) if i == words.len() - 1 => time = s,
                    _ => return usage(),
                }
            }
            Line::Run(Command::Scan {
                transports,
                seconds: time,
            })
        }
        ("scan stop", 0) => Line::Run(Command::ScanStop),
        ("device list", 0) => Line::Devices(Filter::All),
        ("device list", 1) => Line::Devices(match words[0].to_lowercase().as_str() {
            "saved" => Filter::Saved,
            "enabled" => Filter::Enabled,
            "connected" => Filter::Connected,
            "trusted" => Filter::Trusted,
            _ => return usage(),
        }),
        ("pair start", 1) => Line::Run(Command::Pair(arg(0))),
        ("pair accept", 0) => Line::Run(Command::Accept(None)),
        ("pair accept", 1) if !words[0].is_empty() => Line::Run(Command::Accept(Some(arg(0)))),
        ("pair reject", 0) => Line::Run(Command::Reject),
        ("pair cancel", 0) => Line::Run(Command::CancelPairing),
        ("device get", 1) => Line::Run(Command::Get(arg(0))),
        ("device connect", 1) => Line::Run(Command::Connect(arg(0))),
        ("device disconnect", 1) => Line::Run(Command::Disconnect(arg(0))),
        ("device unpair", 1) => Line::Run(Command::Unpair(arg(0))),
        ("device refresh", 1) => Line::Run(Command::Refresh(arg(0))),
        ("device set", 3) => match (toggle(words[1]), on_off(words[2])) {
            (Some(t), Some(on)) => Line::Run(Command::Set(arg(0), t, on)),
            _ => return usage(),
        },
        ("warning list", 1) => Line::Run(Command::Warnings(arg(0))),
        ("setting list", 1) => Line::Run(Command::Settings(arg(0))),
        ("setting get", 2) => Line::Run(Command::SettingGet(arg(0), setting_key(words[1])?)),
        ("setting forget", 2) => Line::Run(Command::SettingForget(arg(0), setting_key(words[1])?)),
        ("setting set", 3) => Line::Run(Command::SettingSet(
            arg(0),
            setting_key(words[1])?,
            SettingInput::Text(arg(2)),
        )),
        ("feature list", 1) => Line::Run(Command::Features(arg(0))),
        ("file list", 1) => Line::Run(Command::Files(arg(0))),
        ("file get", 2) if !words[1].is_empty() => Line::Run(Command::FileGet {
            path: arg(0),
            local: PathBuf::from(words[1]),
            overwrite: false,
        }),
        _ => return usage(),
    };
    Ok(line)
}

/// Checks a command against what the connected adapter offers.
pub fn offer(command: Command, st: &State) -> Result<Command, String> {
    match commands::unsupported(st, &command) {
        Some(reason) => Err(reason.into()),
        None => Ok(command),
    }
}

/// Turns a typed pairing answer into its command: yes or no for a comparison,
/// six digits for a passkey, or 1 to 16 printable ASCII characters for a PIN.
pub fn answer(prompt: &Prompt, text: &str) -> Result<Command, String> {
    match prompt {
        Prompt::ConfirmCode(_) => match text.trim().to_lowercase().as_str() {
            "yes" | "y" | "accept" => Ok(Command::Accept(None)),
            "no" | "n" | "reject" => Ok(Command::Reject),
            _ => Err("answer yes or no".into()),
        },
        Prompt::EnterCode(kind) => {
            commands::check_code(*kind, text)?;
            Ok(Command::Accept(Some(text.to_owned())))
        }
        Prompt::ShowCode(..) => Err("no active authentication question".into()),
    }
}

/// The open prompt this computer answers, if any.
pub fn answerable(st: &State) -> Option<Prompt> {
    st.pairing
        .as_ref()
        .filter(|p| model::answerable(p))
        .and_then(model::prompt)
}

/// Resolves a device word as the controller does: an ID, else a name that
/// identifies exactly one saved device or candidate. Returns the ID and
/// whether it is saved.
pub fn resolve(word: &str, st: &State) -> Option<(String, bool)> {
    if let Some(d) = st.device(word) {
        return Some((d.id.clone(), true));
    }
    if let Some(c) = st.candidate(word) {
        return Some((c.id.clone(), false));
    }
    let mut matches: Vec<(String, bool)> = Vec::new();
    let mut add = |m: (String, bool)| {
        if !matches.iter().any(|(id, _)| *id == m.0) {
            matches.push(m);
        }
    };
    for d in st.devices.iter().filter(|d| d.name == word) {
        add((d.id.clone(), true));
    }
    for c in st.candidates.iter().filter(|c| c.name == word) {
        add((c.id.clone(), false));
    }
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}

/// Resolves a pair target as the controller does: a candidate ID, else a
/// name that exactly one Nearby candidate has.
pub fn resolve_candidate(word: &str, st: &State) -> Option<String> {
    if let Some(c) = st.candidate(word) {
        return Some(c.id.clone());
    }
    let mut named = st.candidates.iter().filter(|c| c.name == word);
    match (named.next(), named.next()) {
        (Some(c), None) => Some(c.id.clone()),
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
pub fn steps(s: &p::Setting) -> (i64, i64) {
    match model::range(s) {
        Some((min, max, fine)) if (max - min) / fine <= 20 => (fine, 0),
        Some((_, _, fine)) => (fine, fine * 10),
        None => (1, 10),
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
fn value_words(s: &p::Setting) -> Vec<String> {
    if model::kind(s) == Some(Type::Bool) {
        return vec!["on".into(), "off".into()];
    }
    let choices = model::choices(s);
    if !choices.is_empty() {
        return choices
            .iter()
            .map(|c| shell_word(value_string(c)))
            .collect();
    }
    if let Some((min, max, fine)) = model::range(s)
        && (max - min) / fine < 16
    {
        return (0..)
            .map(|i| min + i * fine)
            .take_while(|n| *n <= max)
            .map(|n| n.to_string())
            .collect();
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
        .map(|d| (d.id.clone(), d.name.clone()))
        .chain(st.candidates.iter().map(|c| (c.id.clone(), c.name.clone())));
    for (id, name) in names {
        if !accepts(&id) {
            continue;
        }
        // Pair names resolve among candidates only, as the controller does.
        let names_id = match cmd {
            "pair start" => resolve_candidate(&name, st).is_some_and(|r| r == id),
            _ => resolve(&name, st).is_some_and(|(r, _)| r == id),
        };
        if !name.is_empty() && names_id {
            words.push(quote(&name));
        }
        words.push(id);
    }
    words.sort();
    words.dedup();
    words
}

/// Whether a device's state allows the command: only scanned candidates
/// pair, a disabled or blocked device can't be connected, and the
/// remaining device commands need a saved device.
fn usable(cmd: &str, id: &str, st: &State) -> bool {
    let saved = st.device(id);
    match cmd {
        "pair start" => saved.is_none(),
        "device get" | "device unpair" => true,
        "device connect" => saved.is_some_and(|d| d.enabled && !d.blocked),
        "device refresh" | "feature list" => saved.is_some_and(model::connected),
        _ => saved.is_some(),
    }
}

/// Commands whose first argument is a device word.
const DEVICE_COMMANDS: &[&str] = &[
    "device get",
    "pair start",
    "device connect",
    "device disconnect",
    "device unpair",
    "device refresh",
    "device set",
    "warning list",
    "setting list",
    "setting get",
    "setting set",
    "setting forget",
    "feature list",
];

/// The value that changes a saved device's preference, or both without one.
fn policy_words(word: &str, field: &str, st: &State) -> Vec<String> {
    let current = resolve(word, st)
        .filter(|(_, saved)| *saved)
        .and_then(|(id, _)| st.device(&id))
        .and_then(|d| {
            Some(match toggle(field)? {
                Toggle::Enabled => d.enabled,
                Toggle::Trusted => d.trusted,
                Toggle::Blocked => d.blocked,
                Toggle::Hidpp => model::hidpp_enabled(d),
            })
        });
    match current {
        Some(true) => vec!["off".into()],
        Some(false) => vec!["on".into()],
        None => vec!["on".into(), "off".into()],
    }
}

/// The cached settings of a saved device named in a command line. Completion
/// never reads from the adapter.
fn known_settings<'a>(word: &str, st: &'a State) -> Vec<&'a p::Setting> {
    let Some((id, true)) = resolve(word, st) else {
        return Vec::new();
    };
    catalog::presented(st.settings_of(&id))
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
    let arguments = match (cmd, args.len()) {
        ("adapter set platform", 0) => owned(&["linux", "windows", "mac"]),
        (SCAN, 0) => owned(&scan_words(st)),
        ("device list", 0) => owned(&["Saved", "Enabled", "Connected", "Trusted"]),
        ("help", 0) => heads(st).into_iter().map(str::to_owned).collect(),
        ("device set", 1) => owned(&["enabled", "trusted", "blocked", "hidpp"]),
        ("device set", 2) => st
            .map(|st| policy_words(&args[0], &args[1], st))
            .unwrap_or_else(|| owned(&["on", "off"])),
        ("setting get" | "setting set" | "setting forget", 1) => st
            .map(|st| known_settings(&args[0], st))
            .unwrap_or_default()
            .into_iter()
            .filter(|s| cmd != "setting forget" || model::saved(s).is_some())
            .map(|s| s.key.clone())
            .collect(),
        ("setting set", 2) => st
            .map(|st| known_settings(&args[0], st))
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.key == args[1])
            .map(value_words)
            .unwrap_or_default(),
        (_, 0) if DEVICE_COMMANDS.contains(&cmd) => st
            .map(|st| device_words(st, cmd, |id| usable(cmd, id, st)))
            .unwrap_or_default(),
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
    use cordial_protocol::{CodeKind, DeviceState, keys};

    fn words(line: &str) -> Vec<String> {
        split(line).unwrap()
    }

    pub fn device(id: &str, name: &str) -> p::Device {
        p::Device {
            id: id.into(),
            transport: Transport::Ble as i32,
            name: name.into(),
            kind: p::Kind::Keyboard as i32,
            state: DeviceState::Connected as i32,
            enabled: true,
            trusted: true,
            ..Default::default()
        }
    }

    pub fn candidate(id: &str, name: &str) -> p::Candidate {
        p::Candidate {
            id: id.into(),
            transport: Transport::Ble as i32,
            name: name.into(),
            kind: p::Kind::Keyboard as i32,
            rssi: Some(-50),
        }
    }

    pub fn status() -> p::Status {
        p::Status {
            id: "ADAPTER".into(),
            name: "Desk".into(),
            platform: Platform::Linux as i32,
            ready: true,
            transports: vec![
                p::TransportSupport {
                    transport: Transport::Classic as i32,
                    max_enabled: Some(4),
                },
                p::TransportSupport {
                    transport: Transport::Ble as i32,
                    max_enabled: Some(7),
                },
            ],
            info: vec![p::Info {
                key: keys::BUILD_DEVELOPMENT.into(),
                value: Some(model::wire_value(p::value::Value::Bool(true))),
            }],
        }
    }

    pub fn state() -> State {
        State {
            session: 1,
            port: "/dev/ttyACM0".into(),
            status: status(),
            devices: vec![
                device("d_1", "Keyboard"),
                p::Device {
                    state: DeviceState::Disconnected as i32,
                    ..device("d_2", "Mouse")
                },
            ],
            candidates: vec![
                candidate("c_1", "New Keyboard"),
                candidate("c_2", "Keyboard"),
            ],
            available: true,
            loaded: true,
            ..State::default()
        }
    }

    #[test]
    fn split_quotes_and_escapes() {
        assert_eq!(
            words("a 'b c' \"d\\\"e\" f\\ g"),
            ["a", "b c", "d\"e", "f g"]
        );
        assert!(split("'open").is_err());
        assert_eq!(words("''"), [""]);
    }

    #[test]
    fn parse_resource_actions_and_usage() {
        let run = |line: &str| match parse(&words(line)).unwrap() {
            Line::Run(c) => c,
            other => panic!("{other:?}"),
        };
        assert_eq!(run("adapter status"), Command::Status);
        assert_eq!(
            run("adapter set name Desk"),
            Command::Name(Some("Desk".into()))
        );
        assert_eq!(run("adapter reset name"), Command::Name(None));
        assert_eq!(
            run("adapter set platform mac"),
            Command::Platform(Platform::Mac)
        );
        assert_eq!(
            run("scan start"),
            Command::Scan {
                transports: vec![],
                seconds: 0
            }
        );
        assert_eq!(
            run("scan start ble 30"),
            Command::Scan {
                transports: vec![Transport::Ble],
                seconds: 30
            }
        );
        assert_eq!(
            run("scan start 5"),
            Command::Scan {
                transports: vec![],
                seconds: 5
            }
        );
        assert!(parse(&words("scan start 61")).is_err());
        assert!(parse(&words("scan start 5 ble")).is_err());
        assert_eq!(run("pair start c_1"), Command::Pair("c_1".into()));
        assert_eq!(
            run("pair accept 123456"),
            Command::Accept(Some("123456".into()))
        );
        assert_eq!(run("pair accept"), Command::Accept(None));
        assert_eq!(run("pair reject"), Command::Reject);
        assert_eq!(run("pair cancel"), Command::CancelPairing);
        assert_eq!(
            run("device set d_1 enabled off"),
            Command::Set("d_1".into(), Toggle::Enabled, false)
        );
        assert_eq!(
            run("device set 'My Mouse' hidpp on"),
            Command::Set("My Mouse".into(), Toggle::Hidpp, true)
        );
        assert!(parse(&words("device set d_1 enabled maybe")).is_err());
        assert_eq!(run("device refresh d_1"), Command::Refresh("d_1".into()));
        assert_eq!(run("warning list d_1"), Command::Warnings("d_1".into()));
        assert_eq!(
            run("setting set d_1 pointer.sensor.0.dpi 1600"),
            Command::SettingSet(
                "d_1".into(),
                "pointer.sensor.0.dpi".into(),
                SettingInput::Text("1600".into())
            )
        );
        assert_eq!(
            parse(&words("setting get d_1 future.key")).unwrap_err(),
            "Cordial doesn't recognize the setting future.key; a newer version may support it"
        );
        assert_eq!(
            run("file get / out.bin"),
            Command::FileGet {
                path: "/".into(),
                local: "out.bin".into(),
                overwrite: false
            }
        );
        assert!(matches!(
            parse(&words("device list enabled")).unwrap(),
            Line::Devices(Filter::Enabled)
        ));
        assert_eq!(
            parse(&words("device frob")).unwrap_err(),
            "unknown \"device\" command; use help"
        );
    }

    #[test]
    fn names_resolve_only_when_unique() {
        let st = state();
        assert_eq!(resolve("Mouse", &st), Some(("d_2".into(), true)));
        assert_eq!(
            resolve("Keyboard", &st),
            None,
            "a device and a candidate share it"
        );
        assert_eq!(resolve_candidate("Keyboard", &st), Some("c_2".into()));
        assert_eq!(resolve("c_1", &st), Some(("c_1".into(), false)));
    }

    #[test]
    fn completion_walks_command_words_then_arguments() {
        let st = state();
        assert!(complete("dev", Some(&st)).contains(&"device ".to_owned()));
        assert!(complete("device s", Some(&st)).contains(&"device set ".to_owned()));
        assert_eq!(
            complete("device set d_1 ", Some(&st)),
            [
                "device set d_1 enabled",
                "device set d_1 trusted",
                "device set d_1 blocked",
                "device set d_1 hidpp"
            ]
        );
        assert_eq!(
            complete("device set d_1 enabled ", Some(&st)),
            ["device set d_1 enabled off"]
        );
        let connect = complete("device connect ", Some(&st));
        assert!(connect.contains(&"device connect d_1".to_owned()));
        assert!(!connect.iter().any(|c| c.ends_with("c_1")));
        assert_eq!(
            complete("scan start ", Some(&st)),
            ["scan start classic", "scan start ble"]
        );
    }

    #[test]
    fn development_commands_are_offered_only_by_development_firmware() {
        let mut st = state();
        assert!(help(Some(&st)).contains("file list"));
        st.status.info.clear();
        let shown = help(Some(&st));
        assert!(!shown.contains("file list") && !shown.contains("adapter bootloader"));
        assert!(shown.contains("except adapter status."), "{shown}");
        assert!(help(None).contains("adapter bootloader"));
    }

    #[test]
    fn answers_follow_the_prompt() {
        let confirm = Prompt::ConfirmCode("123456".into());
        assert_eq!(answer(&confirm, "yes"), Ok(Command::Accept(None)));
        assert_eq!(answer(&confirm, "n"), Ok(Command::Reject));
        assert!(answer(&confirm, "123456").is_err());
        let passkey = Prompt::EnterCode(CodeKind::Passkey);
        assert_eq!(
            answer(&passkey, "012345"),
            Ok(Command::Accept(Some("012345".into())))
        );
        assert!(answer(&passkey, "12345").is_err());
        let pin = Prompt::EnterCode(CodeKind::Pin);
        assert!(answer(&pin, "0000").is_ok());
        assert!(answer(&pin, "").is_err());
        assert!(answer(&Prompt::ShowCode(CodeKind::Passkey, "1".into()), "y").is_err());
    }
}
