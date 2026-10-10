//! The shell's command language: resource/action commands named after the
//! protocol's commands, pairing answers and completion.
use crate::{
    commands::{self, MAX_SCAN_SECONDS},
    controller::{Command, Pick, SettingInput, State, Target, Toggle},
    model::{self, Prompt, Type},
    profiles,
    ui::{
        catalog::{self, value_string},
        text::{Filter, quote, safe},
    },
};
use cordial_protocol::{self as p, Platform, Transport, profile_rule};
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

/// The transport row's arguments follow the supported transports.
const TRANSPORT_SET: &str = "adapter set transport";

/// The interface rows' arguments follow the supported interfaces.
const INTERFACE_SET: &str = "adapter set interface";
const INTERFACE_RESET: &str = "adapter reset interface";

/// Followed by a row for its profile form on adapters with profiles.
const DEVICE_SET: &str = "device set";

/// Commands offered only by adapters with profiles.
const PROFILE_COMMANDS: &[&str] = &[
    INTERFACE_SET,
    INTERFACE_RESET,
    "profile list",
    "profile show",
    "profile create",
    "profile copy",
    "profile delete",
    "profile rule list",
    "profile rule remap",
    "profile rule scale",
    "profile rule forget",
];

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
        TRANSPORT_SET,
        "classic | ble enabled on | off",
        "Enable or disable a Bluetooth transport (saved on the adapter)",
    ),
    spec(
        INTERFACE_SET,
        "",
        "Enable or disable a configuration interface, optionally selecting its profile",
    ),
    spec(INTERFACE_RESET, "", "Clear a disabled interface's profile"),
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
        "Pair a candidate by ID or a name only one candidate has; one already saved pairs again, keeping its settings; saved disabled when no enabled place is free; then connect",
    ),
    spec("pair accept", "[VALUE]", "Answer a pairing prompt"),
    spec("pair reject", "", "Reject a pairing prompt"),
    spec("pair cancel", "", "Cancel the pairing in progress"),
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
        "Disconnect and remove the saved bond",
    ),
    spec(
        "device refresh",
        "DEV",
        "Ask a connected device for current information and settings",
    ),
    spec(
        DEVICE_SET,
        "DEV enabled | trusted | blocked | hidpp on | off",
        "enabled uses a saved device for connections and needs a free enabled place; trusted allows automatic connections; blocked refuses connections; hidpp lets the adapter use HID++ for special keys and saved settings",
    ),
    spec(
        "profile list",
        "[--after ID]",
        "List profiles, with their roles; --after ID lists one page after that profile",
    ),
    spec("profile show", "PROFILE", "Show a profile's name and roles"),
    spec("profile create", "NAME", "Create an empty profile"),
    spec("profile copy", "PROFILE NAME", "Copy on the adapter"),
    spec("profile delete", "PROFILE", "Delete an unused profile"),
    spec("profile rule list", "PROFILE", "Show every rule"),
    spec(
        "profile rule remap",
        "PROFILE INPUT OUTPUTS",
        "Remap an on/off input to the outputs, held together",
    ),
    spec(
        "profile rule scale",
        "PROFILE INPUT N/D",
        "Multiply a value input by N/D; a negative N inverts it",
    ),
    spec(
        "profile rule forget",
        "PROFILE INPUT",
        "Forget a rule so the input passes through unchanged",
    ),
    spec(
        "warning list",
        "DEV",
        "What the adapter can't use on a device",
    ),
    spec(
        "setting list",
        "DEV",
        "Device settings: last observed values and saved preferences",
    ),
    spec("setting get", "DEV KEY", "One setting in detail"),
    spec(
        "setting set",
        "DEV KEY VALUE",
        "Save a preference on the adapter; applied now if HID++ is on",
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
        "[--raw] PATH LOCAL_FILE",
        "Download an adapter file, a saved record as JSON unless --raw; an existing local file is never replaced",
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
        "DEV is a device ID shown in output, or an unambiguous name. A number is always taken as an ID. Quote names containing spaces.",
    ),
    (
        &["pair start"],
        "CANDIDATE is a candidate ID shown in scan output, or an unambiguous name. Candidate and device IDs are separate.",
    ),
    (
        &["profile", INTERFACE_SET],
        "PROFILE is a profile ID or a unique name. LAYERS is one or more profile arguments in the order they apply, or none for an empty list.",
    ),
    (
        &["profile rule"],
        "A USAGE is PAGE:USAGE in hexadecimal, such as 07:39 for Caps Lock or 01:38 for the wheel. An INPUT is a USAGE. OUTPUTS is disabled or a comma-separated list of USAGEs, each optionally followed by @PAGE:USAGE naming the report's collection.",
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
saves the value on the adapter without applying it. While disconnected,
setting list shows what the adapter has saved, and setting forget still
returns a setting to Default. Only settings this version of Cordial recognizes can be
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
    help_on(st, None)
}

/// Whether a help row or note for `words` belongs to the commands that start with `topic`.
fn about(words: &str, topic: &str) -> bool {
    words == topic || words.starts_with(&format!("{topic} "))
}

/// The help for the commands that start with `topic`, or the whole help without one. A topic
/// the adapter doesn't offer is documented as it is without an adapter.
pub fn help_on(st: Option<&State>, topic: Option<&str>) -> String {
    let topic = topic.map(|t| if t == "exit" { "quit" } else { t });
    let st = match topic {
        Some(t)
            if st.is_some_and(|st| {
                !SPECS
                    .iter()
                    .any(|r| about(r.words, t) && offered(r.words, st))
            }) =>
        {
            None
        }
        _ => st,
    };
    let listed = |words: &str| st.is_none_or(|st| offered(words, st));
    let shown = |words: &str| topic.is_none_or(|t| about(words, t));
    let mut out = Vec::new();
    for r in SPECS
        .iter()
        .filter(|r| r.words != "exit" && listed(r.words) && shown(r.words))
    {
        let (syntax, text) = match r.words {
            SCAN => scan_row(st),
            TRANSPORT_SET => (
                format!(
                    "{TRANSPORT_SET} {} enabled on | off",
                    transport_words(st).join(" | ")
                ),
                r.text.to_owned(),
            ),
            INTERFACE_SET => (
                format!(
                    "{INTERFACE_SET} {} on | off [PROFILE]",
                    interface_words(st).join(" | ")
                ),
                r.text.to_owned(),
            ),
            INTERFACE_RESET => (
                format!(
                    "{INTERFACE_RESET} {} profile",
                    interface_words(st).join(" | ")
                ),
                r.text.to_owned(),
            ),
            "quit" => ("quit | exit".to_owned(), r.text.to_owned()),
            _ => (
                format!("{} {}", r.words, r.args).trim_end().to_owned(),
                r.text.to_owned(),
            ),
        };
        out.push(format!("{syntax:<32} {text}"));
        if r.words == INTERFACE_SET {
            out.push(format!(
                "{:<32} Select the profile a configuration interface edits",
                format!(
                    "{INTERFACE_SET} {} profile PROFILE",
                    interface_words(st).join(" | ")
                )
            ));
        }
        if r.words == DEVICE_SET && profiles_offered(st) {
            out.push(format!(
                "{:<32} Set a device's layers",
                "device set DEV profiles LAYERS"
            ));
        }
    }
    if let Some(topic) = topic {
        let notes: Vec<&str> = NOTES
            .iter()
            .filter(|(words, text)| {
                !text.is_empty()
                    && words.iter().any(|w| {
                        SPECS
                            .iter()
                            .any(|r| about(r.words, topic) && about(r.words, w) && listed(r.words))
                    })
            })
            .map(|(_, text)| text.trim_start_matches('\n'))
            .collect();
        if !notes.is_empty() {
            out.push(String::new());
            out.extend(notes.into_iter().map(str::to_owned));
        }
        return out.join("\n");
    }
    out.push(String::new());
    for (words, text) in NOTES {
        match words[0] {
            "ctrl-c" => {
                out.push("Ctrl-C quits the shell.".into());
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
        _ => "Discover devices on both transports, or on one".to_owned(),
    };
    let syntax = match words.len() {
        1 => format!("{SCAN} [{}] [SECONDS]", words[0]),
        _ => format!("{SCAN} [{}] [SECONDS]", words.join(" | ")),
    };
    (syntax, text)
}

/// The transports a scan can name: those enabled.
fn scan_words(st: Option<&State>) -> Vec<&'static str> {
    match st {
        Some(st) => words_of(model::enabled_transports(&st.status)),
        None => vec!["classic", "ble"],
    }
}

/// The transports that can be enabled or disabled: those supported.
fn transport_words(st: Option<&State>) -> Vec<&'static str> {
    match st {
        Some(st) => words_of(model::transports(&st.status)),
        None => vec!["classic", "ble"],
    }
}

fn words_of(transports: Vec<Transport>) -> Vec<&'static str> {
    transports
        .into_iter()
        .filter_map(|t| match t {
            Transport::Classic => Some("classic"),
            Transport::Ble => Some("ble"),
            Transport::Unspecified => None,
        })
        .collect()
}

/// The interfaces that can be named: those the adapter supports, or every one this build knows
/// without an adapter to describe.
fn interface_words(st: Option<&State>) -> Vec<&'static str> {
    match st {
        Some(st) => profiles::INTERFACES
            .into_iter()
            .filter(|i| profiles::interface(&st.status, *i).is_some())
            .map(profiles::interface_word)
            .collect(),
        None => profiles::INTERFACES
            .into_iter()
            .map(profiles::interface_word)
            .collect(),
    }
}

/// Whether profile commands are offered: always without an adapter to describe.
fn profiles_offered(st: Option<&State>) -> bool {
    st.is_none_or(|st| profiles::available(&st.status))
}

/// Whether the connected adapter offers a shell command. Local commands are
/// always offered.
fn offered(words: &str, st: &State) -> bool {
    let development = model::development(&st.status);
    if PROFILE_COMMANDS.contains(&words) {
        return profiles_offered(Some(st))
            && (!words.contains("interface") || !interface_words(Some(st)).is_empty());
    }
    match words {
        SCAN | "scan stop" => !scan_words(Some(st)).is_empty(),
        "adapter bootloader" | "feature list" | "file list" | "file get" => development,
        TRANSPORT_SET => !transport_words(Some(st)).is_empty(),
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

/// Takes the flag `--NAME` out of `words`, wherever it is: whether it was there, or an error when
/// it is repeated.
fn take_flag(words: &mut Vec<&str>, name: &str) -> Result<bool, ()> {
    let flag = format!("--{name}");
    let Some(i) = words.iter().position(|w| *w == flag) else {
        return Ok(false);
    };
    words.remove(i);
    if words.contains(&flag.as_str()) {
        return Err(());
    }
    Ok(true)
}

/// Takes `--NAME VALUE` or `--NAME=VALUE` out of `words`, wherever it is.
fn take_option<'a>(words: &mut Vec<&'a str>, name: &str) -> Result<Option<&'a str>, ()> {
    let flag = format!("--{name}");
    let prefix = format!("{flag}=");
    let Some(i) = words
        .iter()
        .position(|w| *w == flag || w.starts_with(&prefix))
    else {
        return Ok(None);
    };
    let word = words.remove(i);
    let value = match word.strip_prefix(&prefix) {
        Some(value) => value,
        None if i < words.len() => words.remove(i),
        None => return Err(()),
    };
    if value.is_empty() || words.iter().any(|w| *w == flag || w.starts_with(&prefix)) {
        return Err(());
    }
    Ok(Some(value))
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
    let target = |i: usize| Target::parse(words[i]);
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
        (TRANSPORT_SET, 3) => match (transport(words[0]), words[1], on_off(words[2])) {
            (Some(t), "enabled", Some(on)) => Line::Run(Command::Transport(t, on)),
            _ => return usage(),
        },
        (INTERFACE_SET, 2 | 3) => {
            let Some(interface) = profiles::interface_from_word(words[0]) else {
                return usage();
            };
            let (enabled, profile) = match (words[1], words.get(2)) {
                ("profile", Some(word)) => (None, Some(Pick::Profile(Target::parse(word)?))),
                (word, rest) => match on_off(word) {
                    Some(on) => (
                        Some(on),
                        rest.map(|w| Target::parse(w).map(Pick::Profile))
                            .transpose()?,
                    ),
                    None => return usage(),
                },
            };
            Line::Run(Command::Interface {
                interface,
                enabled,
                profile,
            })
        }
        (INTERFACE_RESET, 2) if words[1] == "profile" => {
            match profiles::interface_from_word(words[0]) {
                Some(interface) => Line::Run(Command::Interface {
                    interface,
                    enabled: None,
                    profile: Some(Pick::Clear),
                }),
                None => return usage(),
            }
        }
        ("profile list", 0) => Line::Run(Command::AllProfiles),
        ("profile list", 1 | 2) => {
            let mut rest = words.to_vec();
            let after = match take_option(&mut rest, "after") {
                Ok(Some(word)) if rest.is_empty() => word
                    .parse()
                    .map_err(|_| format!("{} is not a profile ID", quote(word)))?,
                _ => return usage(),
            };
            Line::Run(Command::Profiles { after })
        }
        ("profile show", 1) => Line::Run(Command::ProfileShow(target(0)?)),
        ("profile create", 1) => Line::Run(Command::ProfileCreate(arg(0))),
        ("profile copy", 2) => Line::Run(Command::ProfileCopy(target(0)?, arg(1))),
        ("profile delete", 1) => Line::Run(Command::ProfileDelete(target(0)?)),
        ("profile rule list", 1) => Line::Run(Command::Rules(target(0)?)),
        ("profile rule remap", 3) => {
            let rule = p::ProfileRule {
                input: Some(profiles::parse_usage(words[1])?),
                effect: Some(profile_rule::Effect::Remap(profile_rule::Remap {
                    outputs: profiles::parse_outputs(words[2])?,
                })),
            };
            Line::Run(Command::RuleChange(target(0)?, profiles::save_rule(rule)))
        }
        ("profile rule scale", 3) => {
            let rule = p::ProfileRule {
                input: Some(profiles::parse_usage(words[1])?),
                effect: Some(profile_rule::Effect::Scale(profiles::parse_ratio(
                    words[2],
                )?)),
            };
            Line::Run(Command::RuleChange(target(0)?, profiles::save_rule(rule)))
        }
        ("profile rule forget", 2) => Line::Run(Command::RuleChange(
            target(0)?,
            profiles::forget_rule(profiles::parse_usage(words[1])?),
        )),
        (DEVICE_SET, 3..) if words[1] == "profiles" => {
            let layers = match &words[2..] {
                ["none"] => Vec::new(),
                list => list
                    .iter()
                    .map(|w| Target::parse(w))
                    .collect::<Result<_, _>>()?,
            };
            Line::Run(Command::Layers(target(0)?, layers))
        }
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
        ("pair start", 1) => Line::Run(Command::Pair(target(0)?)),
        ("pair accept", 0) => Line::Run(Command::Accept(None)),
        ("pair accept", 1) if !words[0].is_empty() => Line::Run(Command::Accept(Some(arg(0)))),
        ("pair reject", 0) => Line::Run(Command::Reject),
        ("pair cancel", 0) => Line::Run(Command::CancelPairing),
        ("device get", 1) => Line::Run(Command::Get(target(0)?)),
        ("device connect", 1) => Line::Run(Command::Connect(target(0)?)),
        ("device disconnect", 1) => Line::Run(Command::Disconnect(target(0)?)),
        ("device unpair", 1) => Line::Run(Command::Unpair(target(0)?)),
        ("device refresh", 1) => Line::Run(Command::Refresh(target(0)?)),
        ("device set", 3) => match (toggle(words[1]), on_off(words[2])) {
            (Some(t), Some(on)) => Line::Run(Command::Set(target(0)?, t, on)),
            _ => return usage(),
        },
        ("warning list", 1) => Line::Run(Command::Warnings(target(0)?)),
        ("setting list", 1) => Line::Run(Command::Settings(target(0)?)),
        ("setting get", 2) => Line::Run(Command::SettingGet(target(0)?, setting_key(words[1])?)),
        ("setting forget", 2) => {
            Line::Run(Command::SettingForget(target(0)?, setting_key(words[1])?))
        }
        ("setting set", 3) => Line::Run(Command::SettingSet(
            target(0)?,
            setting_key(words[1])?,
            SettingInput::Text(arg(2)),
        )),
        ("feature list", 1) => Line::Run(Command::Features(target(0)?)),
        ("file list", 1) => Line::Run(Command::Files(arg(0))),
        ("file get", 2 | 3) => {
            let mut rest = words.to_vec();
            let raw = take_flag(&mut rest, "raw");
            match rest[..] {
                [path, local] if !local.is_empty() && raw.is_ok() => Line::Run(Command::FileGet {
                    path: path.to_owned(),
                    local: PathBuf::from(local),
                    overwrite: false,
                    raw: raw == Ok(true),
                }),
                _ => return usage(),
            }
        }
        _ => return usage(),
    };
    Ok(line)
}

/// Checks a command against what the connected adapter offers.
pub fn offer(command: Command, st: &State) -> Result<Command, String> {
    match commands::unsupported(st, &command) {
        Some(reason) => Err(reason),
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

/// Resolves a device word as the controller does: an ID, else a name exactly one saved device
/// has.
pub fn resolve(word: &str, st: &State) -> Option<u32> {
    let mut named = match Target::parse(word).ok()? {
        Target::Id(id) => return st.device(id).map(|d| d.id),
        Target::Name(name) => st.devices.iter().filter(move |d| d.name == name),
    };
    match (named.next(), named.next()) {
        (Some(d), None) => Some(d.id),
        _ => None,
    }
}

/// Resolves a pair target as the controller does: a candidate ID, else a name exactly one
/// candidate has.
pub fn resolve_candidate(word: &str, st: &State) -> Option<u32> {
    let mut named = match Target::parse(word).ok()? {
        Target::Id(id) => return st.candidate(id).map(|c| c.id),
        Target::Name(name) => st.candidates.iter().filter(move |c| c.name == name),
    };
    match (named.next(), named.next()) {
        (Some(c), None) => Some(c.id),
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
        Some((min, max, fine)) if model::step_count(min, max, fine) <= 20 => (fine, 0),
        Some((_, _, fine)) => (fine, fine.saturating_mul(10)),
        None => (1, 10),
    }
}

/// A word as the shell line takes it back, quoted when needed. A word with characters the
/// terminal can't show safely is never offered: shown escaped, it wouldn't name the same thing.
fn shell_word(word: String) -> Option<String> {
    if safe(&word) != word {
        None
    } else if word.contains(char::is_whitespace) || word.contains(['"', '\'', '\\']) {
        Some(quote(&word))
    } else {
        Some(word)
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
            .filter_map(|c| shell_word(value_string(c)))
            .collect();
    }
    if let Some((min, max, fine)) = model::range(s)
        && model::step_count(min, max, fine) < 16
    {
        return (0..16)
            .map_while(|i| fine.checked_mul(i).and_then(|d| min.checked_add(d)))
            .take_while(|n| *n <= max)
            .map(|n| n.to_string())
            .collect();
    }
    Vec::new()
}

/// IDs, and quoted names that identify exactly one, of the candidates `pair start` takes or the
/// saved devices other commands take, for those `accepts`. A name that is a number is reached
/// by its ID.
fn device_words(st: &State, cmd: &str, accepts: impl Fn(u32) -> bool) -> Vec<String> {
    let pairing = cmd == "pair start";
    let names: Vec<(u32, &str)> = if pairing {
        st.candidates
            .iter()
            .map(|c| (c.id, c.name.as_str()))
            .collect()
    } else {
        st.devices.iter().map(|d| (d.id, d.name.as_str())).collect()
    };
    let mut words: Vec<String> = Vec::new();
    for (id, name) in names {
        if !accepts(id) {
            continue;
        }
        let names_id = if pairing {
            resolve_candidate(name, st) == Some(id)
        } else {
            resolve(name, st) == Some(id)
        };
        if !name.is_empty() && names_id && matches!(Target::parse(name), Ok(Target::Name(_))) {
            words.extend(shell_word(name.to_owned()));
        }
        words.push(id.to_string());
    }
    words.sort();
    words.dedup();
    words
}

/// Whether a device's state allows the command: a candidate of a disabled transport doesn't
/// pair, a disabled or blocked device or one of a disabled transport can't be connected, and
/// refreshing needs a connected device.
fn usable(cmd: &str, id: u32, st: &State) -> bool {
    let disabled = |transport: Transport| model::transport_disabled(&st.status, transport);
    if cmd == "pair start" {
        return st.candidate(id).is_some_and(|c| !disabled(c.transport()));
    }
    let Some(d) = st.device(id) else {
        return false;
    };
    match cmd {
        "device connect" => d.enabled && !d.blocked && !disabled(d.transport()),
        "device refresh" | "feature list" => model::connected(d),
        _ => true,
    }
}

/// Commands whose first argument is a device or candidate word.
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

/// Commands whose first argument is a profile word.
const PROFILE_TARGETS: &[&str] = &[
    "profile show",
    "profile copy",
    "profile delete",
    "profile rule list",
    "profile rule remap",
    "profile rule scale",
    "profile rule forget",
];

/// The value that changes a saved device's preference, or both without one.
fn policy_words(word: &str, field: &str, st: &State) -> Vec<String> {
    let current = resolve(word, st)
        .and_then(|id| st.device(id))
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

/// Each known profile's ID, and its name when the name identifies it. Completion knows only the
/// profiles this session has seen.
fn profile_words(st: &State) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for profile in st.profiles.values() {
        let unique = st
            .profiles
            .values()
            .filter(|p| p.name == profile.name)
            .count()
            == 1;
        let named = !profile.name.is_empty()
            && profile.name != "none"
            && matches!(Target::parse(&profile.name), Ok(Target::Name(_)));
        if unique && named {
            words.extend(shell_word(profile.name.clone()));
        }
        words.push(profile.id.to_string());
    }
    words.sort();
    words.dedup();
    words
}

/// The cached settings of a saved device named in a command line. Completion
/// never reads from the adapter.
fn known_settings<'a>(word: &str, st: &'a State) -> Vec<&'a p::Setting> {
    let Some(id) = resolve(word, st) else {
        return Vec::new();
    };
    catalog::presented(st.settings_of(id))
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
    let profiles = || st.map(profile_words).unwrap_or_default();
    let arguments = match (cmd, args.len()) {
        ("adapter set platform", 0) => owned(&["linux", "windows", "mac"]),
        (TRANSPORT_SET, 0) => owned(&transport_words(st)),
        (TRANSPORT_SET, 1) => owned(&["enabled"]),
        (TRANSPORT_SET, 2) => {
            let current = st.and_then(|st| {
                transport(&args[0]).and_then(|t| model::transport_enabled(&st.status, t))
            });
            match current {
                Some(true) => owned(&["off"]),
                Some(false) => owned(&["on"]),
                None => owned(&["on", "off"]),
            }
        }
        (INTERFACE_SET | INTERFACE_RESET, 0) => owned(&interface_words(st)),
        (INTERFACE_SET, 1) => owned(&["on", "off", "profile"]),
        (INTERFACE_RESET, 1) => owned(&["profile"]),
        (INTERFACE_SET, 2) => profiles(),
        (SCAN, 0) => owned(&scan_words(st)),
        ("device list", 0) => owned(&["Saved", "Enabled", "Connected", "Trusted"]),
        ("help", 0) => heads(st).into_iter().map(str::to_owned).collect(),
        (DEVICE_SET, 1) => {
            let mut words = owned(&["enabled", "trusted", "blocked", "hidpp"]);
            if profiles_offered(st) {
                words.push("profiles".into());
            }
            words
        }
        (DEVICE_SET, 2) if args[1] == "profiles" => {
            let mut words = owned(&["none"]);
            words.extend(profiles());
            words
        }
        (DEVICE_SET, 3..) if args[1] == "profiles" && args[2] != "none" => profiles(),
        (DEVICE_SET, 2) => st
            .map(|st| policy_words(&args[0], &args[1], st))
            .unwrap_or_else(|| owned(&["on", "off"])),
        (_, 0) if PROFILE_TARGETS.contains(&cmd) => profiles(),
        ("profile list", 0) => owned(&["--after"]),
        ("file get", 0) => owned(&["--raw"]),
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
    use cordial_protocol::{CodeKind, ConfigurationInterface, DeviceState, keys};

    fn words(line: &str) -> Vec<String> {
        split(line).unwrap()
    }

    fn run(line: &str) -> Command {
        match parse(&words(line)).unwrap() {
            Line::Run(c) => c,
            other => panic!("{other:?}"),
        }
    }

    pub fn device(id: u32, name: &str) -> p::Device {
        p::Device {
            id,
            transport: Transport::Ble as i32,
            name: name.into(),
            kinds: vec![p::Kind::Keyboard as i32],
            state: DeviceState::Connected as i32,
            enabled: true,
            trusted: true,
            ..Default::default()
        }
    }

    pub fn candidate(id: u32, name: &str) -> p::Candidate {
        p::Candidate {
            id,
            transport: Transport::Ble as i32,
            name: name.into(),
            kinds: vec![p::Kind::Keyboard as i32],
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
                    enabled: Some(true),
                },
                p::TransportSupport {
                    transport: Transport::Ble as i32,
                    max_enabled: Some(7),
                    enabled: Some(true),
                },
            ],
            info: vec![p::Info {
                key: keys::BUILD_DEVELOPMENT.into(),
                value: Some(model::wire_value(p::value::Value::Bool(true))),
            }],
            profile_support: None,
            configuration_interfaces: Vec::new(),
        }
    }

    /// Profile support with VIA and Vial, which conflict, both disabled.
    pub fn with_profiles(status: &mut p::Status) {
        status.profile_support = Some(p::ProfileSupport {
            remap_inputs: vec![p::UsageRange {
                collection: None,
                usage_page: 7,
                min: 4,
                max: 0xe7,
            }],
            max_remap_outputs: 4,
            max_layers: 4,
            memory_budget: 4096,
            ..Default::default()
        });
        status.configuration_interfaces = [
            (ConfigurationInterface::Via, ConfigurationInterface::Vial),
            (ConfigurationInterface::Vial, ConfigurationInterface::Via),
        ]
        .into_iter()
        .map(|(i, other)| p::ConfigurationInterfaceSupport {
            interface: i as i32,
            enabled: false,
            profile: 0,
            conflicts: vec![other as i32],
        })
        .collect();
    }

    pub fn state() -> State {
        State {
            session: 1,
            port: "/dev/ttyACM0".into(),
            status: status(),
            devices: vec![
                device(1, "Keyboard"),
                p::Device {
                    state: DeviceState::Disconnected as i32,
                    ..device(2, "Mouse")
                },
            ],
            candidates: vec![candidate(1, "New Keyboard"), candidate(2, "Keyboard")],
            available: true,
            loaded: true,
            ..State::default()
        }
    }

    #[test]
    fn extreme_integer_ranges_neither_overflow_nor_enumerate_far() {
        let ranged = |min: i64, max: i64, step: u64| p::Setting {
            key: "pointer.sensor.0.dpi".into(),
            r#type: Some(p::setting::Type::Integer(p::IntegerSetting {
                limits: Some(p::integer_setting::Limits::Range(p::IntegerRange {
                    min,
                    max,
                    step,
                })),
                ..Default::default()
            })),
            ..Default::default()
        };
        let all = ranged(i64::MIN, i64::MAX, 1);
        assert_eq!(steps(&all), (1, 10));
        assert!(value_words(&all).is_empty());
        assert!(model::accepts(&all, &p::value::Value::Integer(i64::MAX)));
        let top = ranged(i64::MAX - 2, i64::MAX, 1);
        assert_eq!(value_words(&top).len(), 3);
        let wide = ranged(0, i64::MAX, u64::MAX);
        assert_eq!(steps(&wide), (i64::MAX, 0));
        assert_eq!(value_words(&wide), ["0", &i64::MAX.to_string()]);
        assert!(value_words(&ranged(5, 1, 1)).is_empty());
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
        assert_eq!(run("pair start 12"), Command::Pair(Target::Id(12)));
        assert_eq!(
            run("pair start 'New Keyboard'"),
            Command::Pair(Target::Name("New Keyboard".into()))
        );
        assert_eq!(
            run("pair accept 123456"),
            Command::Accept(Some("123456".into()))
        );
        assert_eq!(run("pair accept"), Command::Accept(None));
        assert_eq!(run("pair reject"), Command::Reject);
        assert_eq!(run("pair cancel"), Command::CancelPairing);
        assert_eq!(
            run("device set 1 enabled off"),
            Command::Set(Target::Id(1), Toggle::Enabled, false)
        );
        assert_eq!(
            run("device set 'My Mouse' hidpp on"),
            Command::Set(Target::Name("My Mouse".into()), Toggle::Hidpp, true)
        );
        assert!(parse(&words("device set 1 enabled maybe")).is_err());
        assert_eq!(run("device refresh 1"), Command::Refresh(Target::Id(1)));
        assert_eq!(run("warning list 1"), Command::Warnings(Target::Id(1)));
        assert_eq!(
            run("setting set 1 pointer.sensor.0.dpi 1600"),
            Command::SettingSet(
                Target::Id(1),
                "pointer.sensor.0.dpi".into(),
                SettingInput::Text("1600".into())
            )
        );
        assert_eq!(
            parse(&words("setting get 1 future.key")).unwrap_err(),
            "Cordial doesn't recognize the setting future.key; a newer version may support it"
        );
        assert_eq!(
            run("file get / out.bin"),
            Command::FileGet {
                path: "/".into(),
                local: "out.bin".into(),
                overwrite: false,
                raw: false,
            }
        );
        for line in ["file get --raw /a.pb a.pb", "file get /a.pb --raw a.pb"] {
            assert_eq!(
                run(line),
                Command::FileGet {
                    path: "/a.pb".into(),
                    local: "a.pb".into(),
                    overwrite: false,
                    raw: true,
                }
            );
        }
        for line in [
            "file get --raw --raw /a.pb a.pb",
            "file get --raw /a.pb",
            "file get /a.pb a.pb b",
            "file get /a.pb \"\"",
        ] {
            assert!(parse(&words(line)).is_err(), "{line}");
        }
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
    fn numbers_are_always_ids() {
        assert_eq!(Target::parse("007"), Ok(Target::Id(7)));
        assert_eq!(Target::parse("7a"), Ok(Target::Name("7a".into())));
        assert_eq!(Target::parse(""), Ok(Target::Name(String::new())));
        assert!(Target::parse("99999999999").is_err());
        assert!(parse(&words("device get 99999999999")).is_err());
        let mut st = state();
        // A device named with a number is reached by its ID, never by the name.
        st.devices[1].name = "1".into();
        assert_eq!(resolve("1", &st), Some(1));
        assert_eq!(resolve("2", &st), Some(2));
        assert_eq!(resolve("3", &st), None);
        let devices = complete("device get ", Some(&st));
        assert_eq!(
            devices,
            ["device get 1", "device get 2", "device get Keyboard"]
        );
    }

    #[test]
    fn completion_offers_only_names_that_round_trip() {
        let mut st = state();
        st.devices[1].name = "Desk \u{202e}draobyeK".into();
        st.candidates[0].name = "Pad\u{9b}2J".into();
        st.candidates[1].name = "Two Words".into();
        assert_eq!(
            complete("device get ", Some(&st)),
            ["device get 1", "device get 2", "device get Keyboard"]
        );
        let pair = complete("pair start ", Some(&st));
        assert_eq!(
            pair,
            ["pair start \"Two Words\"", "pair start 1", "pair start 2"]
        );
        let line = &pair[0];
        assert_eq!(split(line).unwrap(), ["pair", "start", "Two Words"]);
    }

    #[test]
    fn names_resolve_only_when_unique_among_their_kind() {
        let mut st = state();
        // A device and a candidate may share a name; each command takes only one kind.
        assert_eq!(resolve("Keyboard", &st), Some(1));
        assert_eq!(resolve_candidate("Keyboard", &st), Some(2));
        assert_eq!(resolve("New Keyboard", &st), None);
        // Candidate 1 and device 1 are different things.
        assert_eq!(resolve_candidate("1", &st), Some(1));
        st.devices.push(device(5, "Keyboard"));
        assert_eq!(resolve("Keyboard", &st), None);
    }

    #[test]
    fn completion_walks_command_words_then_arguments() {
        let st = state();
        assert!(complete("dev", Some(&st)).contains(&"device ".to_owned()));
        assert!(complete("device s", Some(&st)).contains(&"device set ".to_owned()));
        assert_eq!(
            complete("device set 1 ", Some(&st)),
            [
                "device set 1 enabled",
                "device set 1 trusted",
                "device set 1 blocked",
                "device set 1 hidpp"
            ]
        );
        assert_eq!(
            complete("device set 1 enabled ", Some(&st)),
            ["device set 1 enabled off"]
        );
        let connect = complete("device connect ", Some(&st));
        assert!(connect.contains(&"device connect 1".to_owned()));
        assert!(!connect.iter().any(|c| c.contains("New Keyboard")));
        let pair = complete("pair start ", Some(&st));
        assert!(pair.contains(&"pair start \"New Keyboard\"".to_owned()));
        assert!(pair.contains(&"pair start 2".to_owned()));
        assert_eq!(
            complete("scan start ", Some(&st)),
            ["scan start classic", "scan start ble"]
        );
    }

    #[test]
    fn help_for_a_command_shows_its_rows_and_notes() {
        let st = state();
        assert!(matches!(
            parse(&words("help pair")).unwrap(),
            Line::Help(Some(t)) if t == "pair"
        ));
        let pair = help_on(Some(&st), Some("pair"));
        assert!(pair.starts_with("pair start CANDIDATE"), "{pair}");
        assert!(
            pair.contains("pair cancel") && pair.contains("CANDIDATE is"),
            "{pair}"
        );
        assert!(
            !pair.contains("device list") && !pair.contains("DEV is"),
            "{pair}"
        );
        let get = help_on(Some(&st), Some("device get"));
        assert_eq!(
            get.lines().next().unwrap().split_whitespace().next(),
            Some("device")
        );
        assert_eq!(
            get.lines().filter(|l| l.starts_with("device")).count(),
            1,
            "{get}"
        );
        assert!(help_on(None, Some("exit")).starts_with("quit | exit"));
        let mut production = state();
        production.status.info.clear();
        assert!(help_on(Some(&production), Some("file get")).starts_with("file get [--raw] PATH"));
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
    fn transports_are_enabled_and_disabled_only_when_supported() {
        assert_eq!(
            run("adapter set transport classic enabled on"),
            Command::Transport(Transport::Classic, true)
        );
        assert_eq!(
            run("adapter set transport ble enabled off"),
            Command::Transport(Transport::Ble, false)
        );
        assert!(parse(&words("adapter set transport ble enabled yes")).is_err());
        assert!(parse(&words("adapter set transport ble on")).is_err());

        let mut st = state();
        assert!(
            help(Some(&st)).contains("adapter set transport classic | ble enabled on | off"),
            "{}",
            help(Some(&st))
        );
        assert_eq!(
            complete("adapter set transport ", Some(&st)),
            ["adapter set transport classic", "adapter set transport ble"]
        );
        assert_eq!(
            complete("adapter set transport ble ", Some(&st)),
            ["adapter set transport ble enabled"]
        );
        assert_eq!(
            complete("adapter set transport classic enabled ", Some(&st)),
            ["adapter set transport classic enabled off"]
        );
        // While Classic is disabled, its devices neither connect nor pair, and no scan names it.
        st.status.transports[0].enabled = Some(false);
        st.devices[1].transport = Transport::Classic as i32;
        st.candidates[0].transport = Transport::Classic as i32;
        assert_eq!(
            complete("adapter set transport classic enabled ", Some(&st)),
            ["adapter set transport classic enabled on"]
        );
        let connect = complete("device connect ", Some(&st));
        assert!(connect.contains(&"device connect 1".to_owned()));
        assert!(!connect.iter().any(|c| c.ends_with(" 2")));
        let pair = complete("pair start ", Some(&st));
        assert!(pair.contains(&"pair start 2".to_owned()));
        assert!(!pair.iter().any(|c| c.ends_with(" 1")));
        assert_eq!(complete("scan start ", Some(&st)), ["scan start ble"]);
        let scan = |named: Vec<Transport>| Command::Scan {
            transports: named,
            seconds: 0,
        };
        assert_eq!(
            offer(scan(vec![Transport::Classic]), &st),
            Err("Bluetooth Classic is disabled on the adapter".into())
        );
        assert!(offer(scan(vec![]), &st).is_ok());
        st.status.transports[1].enabled = Some(false);
        assert_eq!(
            offer(scan(vec![]), &st),
            Err("Bluetooth LE is disabled on the adapter".into())
        );
        assert!(!help(Some(&st)).contains("scan start"));

        // A transport without its enabled field is enabled.
        st.status.transports[0].enabled = None;
        assert_eq!(model::enabled_transports(&st.status), [Transport::Classic]);

        // A BLE-only adapter lists only BLE.
        st.status
            .transports
            .retain(|t| t.transport != Transport::Classic as i32);
        assert!(
            help(Some(&st)).contains("adapter set transport ble enabled on | off"),
            "{}",
            help(Some(&st))
        );
        assert_eq!(
            complete("adapter set transport ", Some(&st)),
            ["adapter set transport ble"]
        );
        assert!(help(None).contains("adapter set transport classic | ble enabled on | off"));
    }

    #[test]
    fn profile_commands_parse() {
        assert_eq!(
            run("profile list --after 16"),
            Command::Profiles { after: 16 }
        );
        assert_eq!(
            run("profile list --after=3"),
            Command::Profiles { after: 3 }
        );
        assert_eq!(run("profile list"), Command::AllProfiles);
        assert!(parse(&words("profile list --after")).is_err());
        assert!(parse(&words("profile list --after x")).is_err());
        assert_eq!(
            run("profile create 'Work Keys'"),
            Command::ProfileCreate("Work Keys".into())
        );
        assert!(parse(&words("profile create")).is_err());
        assert_eq!(
            run("profile copy 3 Games"),
            Command::ProfileCopy(Target::Id(3), "Games".into())
        );
        assert_eq!(
            run("profile delete Work"),
            Command::ProfileDelete(Target::Name("Work".into()))
        );
        assert_eq!(run("profile show 4"), Command::ProfileShow(Target::Id(4)));
        assert_eq!(
            run("adapter set interface via on 2"),
            Command::Interface {
                interface: ConfigurationInterface::Via,
                enabled: Some(true),
                profile: Some(Pick::Profile(Target::Id(2)))
            }
        );
        assert_eq!(
            run("adapter set interface vial off"),
            Command::Interface {
                interface: ConfigurationInterface::Vial,
                enabled: Some(false),
                profile: None
            }
        );
        assert_eq!(
            run("adapter set interface vial profile Work"),
            Command::Interface {
                interface: ConfigurationInterface::Vial,
                enabled: None,
                profile: Some(Pick::Profile(Target::Name("Work".into())))
            }
        );
        assert_eq!(
            run("adapter reset interface via profile"),
            Command::Interface {
                interface: ConfigurationInterface::Via,
                enabled: None,
                profile: Some(Pick::Clear)
            }
        );
        assert!(parse(&words("adapter set interface none on")).is_err());
        assert!(parse(&words("adapter set interface via profile")).is_err());
        assert_eq!(
            run("device set 1 profiles Work 4"),
            Command::Layers(
                Target::Id(1),
                vec![Target::Name("Work".into()), Target::Id(4)]
            )
        );
        assert_eq!(
            run("device set Mouse profiles none"),
            Command::Layers(Target::Name("Mouse".into()), Vec::new())
        );
        assert!(parse(&words("device set 1 profiles")).is_err());
        for gone in [
            "adapter set default-profile Work",
            "adapter set keyboard-profile Work",
            "adapter set editing-profile Work",
            "profile set 3 pointer 3/2",
            "device set 1 keyboard-profile default",
            "profile create keyboard Work",
        ] {
            assert!(parse(&words(gone)).is_err(), "{gone}");
        }
    }

    #[test]
    fn rule_commands_parse_usages() {
        let usage = |usage_page, usage| p::Usage { usage_page, usage };
        let Command::RuleChange(Target::Name(name), change) =
            run("profile rule remap Work 07:39 07:e0,07:04@01:06")
        else {
            panic!()
        };
        assert_eq!(name, "Work");
        let Some(p::profile_rule_change::Change::Rule(rule)) = change.change else {
            panic!()
        };
        assert_eq!(rule.input, Some(usage(7, 0x39)));
        assert_eq!(profiles::rule_words(&rule), "07:39 remap 07:e0,07:04@01:06");
        let Command::RuleChange(_, change) = run("profile rule scale 2 01:38 -1/1") else {
            panic!()
        };
        let Some(p::profile_rule_change::Change::Rule(rule)) = change.change else {
            panic!()
        };
        assert_eq!(profiles::rule_words(&rule), "01:38 scale -1/1");
        let Command::RuleChange(_, change) = run("profile rule forget 2 09:01") else {
            panic!()
        };
        assert_eq!(profiles::change_target(&change), Some(&usage(9, 1)));
        assert_eq!(
            run("profile rule list Work"),
            Command::Rules(Target::Name("Work".into()))
        );
        for bad in [
            "profile rule remap 2 07:39",
            "profile rule remap 2 7x:39 07:04",
            "profile rule scale 2 01:38 0/1",
            "profile rule forget 2 09:01 --collection 01:02",
            "profile rule list 2 --collection 01:02",
        ] {
            assert!(parse(&words(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn profile_commands_follow_the_adapter() {
        let mut st = state();
        assert!(!help(Some(&st)).contains("profile list"));
        assert_eq!(
            offer(Command::ProfileCreate("Work".into()), &st),
            Err("this adapter doesn't support profiles".into())
        );
        assert!(!complete("device set 1 ", Some(&st)).contains(&"device set 1 profiles".into()));
        with_profiles(&mut st.status);
        let shown = help(Some(&st));
        assert!(shown.contains("profile rule remap"), "{shown}");
        assert!(
            shown.contains("adapter set interface via | vial on | off [PROFILE]"),
            "{shown}"
        );
        assert!(shown.contains("device set DEV profiles LAYERS"), "{shown}");
        assert_eq!(
            complete("device set 1 p", Some(&st)),
            ["device set 1 profiles"]
        );
        st.profiles.insert(
            3,
            p::Profile {
                id: 3,
                name: "Work Keys".into(),
                roles: Vec::new(),
            },
        );
        assert_eq!(
            complete("device set 1 profiles ", Some(&st)),
            [
                "device set 1 profiles none",
                "device set 1 profiles \"Work Keys\"",
                "device set 1 profiles 3"
            ]
        );
        assert_eq!(
            complete("adapter set interface ", Some(&st)),
            ["adapter set interface via", "adapter set interface vial"]
        );
        // Without interfaces, the interface commands aren't offered.
        st.status.configuration_interfaces.clear();
        assert!(!help(Some(&st)).contains("adapter set interface"));
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
