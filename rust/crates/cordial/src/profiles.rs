//! Profiles: what the adapter supports, device layers, configuration interfaces, HID usages and
//! rules in words, and the checks made before asking the adapter to change any of them.
use crate::{error::Error, model, view::State};
use cordial_protocol::{
    self as p, ConfigurationInterface, ErrorCode, ResourceType, Role, profile_rule,
    profile_rule_change,
};

/// The longest profile name, in UTF-8 bytes.
pub const NAME_BYTES: usize = 64;

/// What profile rules the adapter can apply, or `None` when its board has no profiles.
pub fn support(status: &p::Status) -> Option<&p::ProfileSupport> {
    status.profile_support.as_ref()
}

/// Whether the adapter supports profiles at all.
pub fn available(status: &p::Status) -> bool {
    support(status).is_some()
}

/// A profile resource the adapter reports.
pub fn resource(status: &p::Status, kind: ResourceType) -> Option<&p::ResourceCapacity> {
    model::resource(&support(status)?.resources, kind)
}

/// The most profiles one device's layers can list, when the adapter reports it.
pub fn max_layers(status: &p::Status) -> Option<u32> {
    resource(status, ResourceType::Layers)?.limit
}

/// The most outputs one remap can hold, when the adapter reports it.
fn max_outputs(support: &p::ProfileSupport) -> Option<u32> {
    model::resource(&support.resources, ResourceType::Outputs)?.limit
}

/// The kinds of input a profile's rules change that this build knows.
pub fn roles(profile: &p::Profile) -> Vec<Role> {
    model::known_roles(&profile.roles)
}

/// A device's saved layers: the profiles it applies, in order.
pub fn layers(d: &p::Device) -> &[u32] {
    d.profiles.as_ref().map_or(&[], |l| l.profiles.as_slice())
}

/// A profile's name, or its ID while the name isn't known.
pub fn name_of(st: &State, id: u32) -> String {
    st.profile(id)
        .map_or_else(|| id.to_string(), |p| p.name.clone())
}

/// The configuration interfaces this build can name, in protocol order.
pub const INTERFACES: [ConfigurationInterface; 2] =
    [ConfigurationInterface::Via, ConfigurationInterface::Vial];

/// An interface as the shell spells it.
pub fn interface_word(i: ConfigurationInterface) -> &'static str {
    match i {
        ConfigurationInterface::Via => "via",
        ConfigurationInterface::Vial => "vial",
        ConfigurationInterface::Unspecified => "",
    }
}

pub fn interface_from_word(word: &str) -> Option<ConfigurationInterface> {
    INTERFACES.into_iter().find(|i| interface_word(*i) == word)
}

/// An interface as labels show it.
pub fn interface_label(i: ConfigurationInterface) -> &'static str {
    match i {
        ConfigurationInterface::Via => "VIA",
        ConfigurationInterface::Vial => "Vial",
        ConfigurationInterface::Unspecified => "Unknown Interface",
    }
}

/// The interfaces the adapter supports that this build knows, in the adapter's order.
pub fn interfaces(status: &p::Status) -> Vec<&p::ConfigurationInterfaceSupport> {
    status
        .configuration_interfaces
        .iter()
        .filter(|s| INTERFACES.iter().any(|i| *i as i32 == s.interface))
        .collect()
}

/// The adapter's preferences for one interface, when it supports the interface.
pub fn interface(
    status: &p::Status,
    i: ConfigurationInterface,
) -> Option<&p::ConfigurationInterfaceSupport> {
    status
        .configuration_interfaces
        .iter()
        .find(|s| s.interface == i as i32)
}

/// A change to one configuration interface's preferences; `None` leaves a preference unchanged
/// and a profile of 0 clears it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceUpdate {
    pub interface: ConfigurationInterface,
    pub enabled: Option<bool>,
    pub profile: Option<u32>,
}

impl InterfaceUpdate {
    pub fn wire(&self) -> p::ConfigurationInterfaceUpdate {
        p::ConfigurationInterfaceUpdate {
            interface: self.interface as i32,
            enabled: self.enabled,
            profile: self.profile,
        }
    }
}

/// The adapter's configuration interfaces as they would be once `updates` apply in order.
/// Updates for interfaces the adapter doesn't support change nothing.
pub fn configured(
    status: &p::Status,
    updates: &[InterfaceUpdate],
) -> Vec<p::ConfigurationInterfaceSupport> {
    let mut out = status.configuration_interfaces.clone();
    for u in updates {
        if let Some(s) = out.iter_mut().find(|s| s.interface == u.interface as i32) {
            if let Some(on) = u.enabled {
                s.enabled = on;
            }
            if let Some(profile) = u.profile {
                s.profile = profile;
            }
        }
    }
    out
}

/// An interface's label, or "Unknown {number}" for one this build does not know.
fn label_of(interface: i32) -> String {
    match ConfigurationInterface::try_from(interface) {
        Ok(i) if i != ConfigurationInterface::Unspecified => interface_label(i).into(),
        _ => format!("Unknown {interface}"),
    }
}

/// Why the adapter would refuse `updates`: an interface it doesn't support, an enabled interface
/// without a profile, or conflicting interfaces enabled together.
pub fn interface_refusal(status: &p::Status, updates: &[InterfaceUpdate]) -> Option<Error> {
    if updates
        .iter()
        .any(|u| interface(status, u.interface).is_none())
    {
        return Some(Error::code(
            ErrorCode::Unsupported,
            Some("adapter interface"),
        ));
    }
    let result = configured(status, updates);
    let enabled: Vec<&p::ConfigurationInterfaceSupport> =
        result.iter().filter(|s| s.enabled).collect();
    if let Some(s) = enabled.iter().find(|s| s.profile == 0) {
        return Some(Error::new(format!(
            "choose a profile for {} before turning it on",
            label_of(s.interface)
        )));
    }
    for a in &enabled {
        if let Some(b) = enabled
            .iter()
            .find(|b| b.interface != a.interface && a.conflicts.contains(&b.interface))
        {
            return Some(Error::new(format!(
                "{} and {} can't both be on. Turn one off first",
                label_of(a.interface),
                label_of(b.interface)
            )));
        }
    }
    None
}

/// Why the adapter would refuse to delete a profile, or `None` when nothing uses it.
pub fn in_use(st: &State, id: u32) -> Option<String> {
    if let Some(s) = st
        .status
        .configuration_interfaces
        .iter()
        .find(|s| s.profile == id)
    {
        let label = label_of(s.interface);
        return Some(format!(
            "{label} is using this profile. Pick a different profile for {label} first"
        ));
    }
    st.devices
        .iter()
        .find(|d| layers(d).contains(&id))
        .map(|d| {
            let name = crate::ui::text::display_name(Some(&d.name));
            format!("{name} is using this profile. Remove it from {name}'s profiles first")
        })
}

/// A device whose layers list a profile being deleted, with its name and layers as seen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Detach {
    pub device: u32,
    pub name: String,
    pub layers: Vec<u32>,
}

/// A configuration interface that uses a profile being deleted, and whether it was on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unassign {
    pub interface: ConfigurationInterface,
    pub enabled: bool,
}

/// The changes deleting a profile makes, in order: each device that lists it keeps its other
/// layers in order, the configuration interfaces that use it are turned off and cleared, and then
/// the profile is deleted. Each change is made only while its target is still as seen here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Deletion {
    pub profile: u32,
    pub devices: Vec<Detach>,
    pub interfaces: Vec<Unassign>,
}

impl Deletion {
    /// What deleting `profile` changes, from the view.
    pub fn of(st: &State, profile: u32) -> Self {
        let devices = st
            .devices
            .iter()
            .filter(|d| layers(d).contains(&profile))
            .map(|d| Detach {
                device: d.id,
                name: crate::ui::text::display_name(Some(&d.name)),
                layers: layers(d).to_vec(),
            })
            .collect();
        let interfaces = st
            .status
            .configuration_interfaces
            .iter()
            .filter(|s| s.profile == profile)
            .filter_map(|s| {
                Some(Unassign {
                    interface: ConfigurationInterface::try_from(s.interface).ok()?,
                    enabled: s.enabled,
                })
            })
            .collect();
        Self {
            profile,
            devices,
            interfaces,
        }
    }

    /// The layers a device keeps: its others, in order.
    pub fn kept(&self, d: &Detach) -> Vec<u32> {
        d.layers
            .iter()
            .copied()
            .filter(|id| *id != self.profile)
            .collect()
    }

    /// Turns off and clears every interface that uses the profile, in one adapter update.
    pub fn interface_updates(&self) -> Vec<InterfaceUpdate> {
        self.interfaces
            .iter()
            .map(|u| InterfaceUpdate {
                interface: u.interface,
                enabled: Some(false),
                profile: Some(0),
            })
            .collect()
    }
}

/// Profiles that devices and configuration interfaces use whose names aren't known yet.
pub fn unnamed(st: &State) -> Vec<u32> {
    if !available(&st.status) {
        return Vec::new();
    }
    let mut ids: Vec<u32> = st
        .devices
        .iter()
        .flat_map(|d| layers(d).iter().copied())
        .chain(st.status.configuration_interfaces.iter().map(|s| s.profile))
        .filter(|id| *id != 0 && st.profile(*id).is_none())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Whether a profile name is accepted: 1 to 64 bytes without control characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= NAME_BYTES && !name.chars().any(char::is_control)
}

/// A copy's suggested name, kept within the adapter's 64-byte limit.
pub fn copy_name(name: &str) -> String {
    const SUFFIX: &str = " Copy";
    let mut base = name.to_owned();
    while base.len() + SUFFIX.len() > NAME_BYTES {
        base.pop();
    }
    format!("{}{SUFFIX}", base.trim_end())
}

/// A usage as the CLI spells it: `PAGE:USAGE` in lowercase hexadecimal, such as `07:39`.
pub fn usage_words(u: &p::Usage) -> String {
    format!("{:02x}:{:02x}", u.usage_page, u.usage)
}

/// Parses a usage typed as `PAGE:USAGE` in hexadecimal, each at most 16 bits.
pub fn parse_usage(word: &str) -> Result<p::Usage, String> {
    let hex = |part: &str| {
        (!part.is_empty() && part.len() <= 4 && part.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(part, 16).ok())
            .flatten()
    };
    word.split_once(':')
        .and_then(|(page, usage)| {
            Some(p::Usage {
                usage_page: hex(page)?,
                usage: hex(usage)?,
            })
        })
        .ok_or_else(|| {
            format!(
                "{} is not a usage; use PAGE:USAGE in hexadecimal, such as 07:39",
                crate::ui::text::quote(word)
            )
        })
}

/// Parses a rule input: its usage, then `@PAGE:USAGE` naming the collection it arrives in, such
/// as `07:39@01:06`.
pub fn parse_input(word: &str) -> Result<p::ProfileRuleRef, String> {
    let (input, collection) = word.split_once('@').ok_or_else(|| {
        format!(
            "{} has no collection; add @PAGE:USAGE, such as 07:39@01:06",
            crate::ui::text::quote(word)
        )
    })?;
    Ok(p::ProfileRuleRef {
        input: Some(parse_usage(input)?),
        collection: Some(parse_usage(collection)?),
    })
}

/// Parses remap outputs: `disabled`, or a comma-separated list of usages, each optionally
/// followed by `@PAGE:USAGE` naming the collection of the report that carries it.
pub fn parse_outputs(word: &str) -> Result<Vec<profile_rule::Output>, String> {
    if word == "disabled" {
        return Ok(Vec::new());
    }
    word.split(',')
        .map(|part| {
            let (usage, collection) = match part.split_once('@') {
                Some((usage, collection)) => (usage, Some(parse_usage(collection)?)),
                None => (part, None),
            };
            Ok(profile_rule::Output {
                usage: Some(parse_usage(usage)?),
                collection,
            })
        })
        .collect()
}

/// Parses a scale ratio `N/D`: a nonzero numerator, negative to invert, and a positive
/// denominator.
pub fn parse_ratio(word: &str) -> Result<profile_rule::Scale, String> {
    let invalid = || {
        format!(
            "{} is not a ratio; use N/D with a nonzero N and a positive D, such as -1/1",
            crate::ui::text::quote(word)
        )
    };
    let (n, d) = word.split_once('/').ok_or_else(invalid)?;
    let numerator: i32 = n.parse().map_err(|_| invalid())?;
    let denominator: u32 = d.parse().map_err(|_| invalid())?;
    if numerator == 0 || denominator == 0 {
        return Err(invalid());
    }
    Ok(profile_rule::Scale {
        numerator,
        denominator,
    })
}

/// A rule for the input `at` with `effect`.
pub fn rule(at: p::ProfileRuleRef, effect: profile_rule::Effect) -> p::ProfileRule {
    p::ProfileRule {
        input: at.input,
        collection: at.collection,
        effect: Some(effect),
    }
}

/// The input and collection a rule is for.
pub fn rule_ref(rule: &p::ProfileRule) -> p::ProfileRuleRef {
    p::ProfileRuleRef {
        input: rule.input,
        collection: rule.collection,
    }
}

/// A change that saves `rule`.
pub fn save_rule(rule: p::ProfileRule) -> p::ProfileRuleChange {
    p::ProfileRuleChange {
        change: Some(profile_rule_change::Change::Rule(rule)),
    }
}

/// A change that forgets the rule for the input `at`.
pub fn forget_rule(at: p::ProfileRuleRef) -> p::ProfileRuleChange {
    p::ProfileRuleChange {
        change: Some(profile_rule_change::Change::Forget(at)),
    }
}

/// The input and collection a change names.
pub fn change_target(change: &p::ProfileRuleChange) -> Option<p::ProfileRuleRef> {
    match &change.change {
        Some(profile_rule_change::Change::Rule(r)) => Some(rule_ref(r)),
        Some(profile_rule_change::Change::Forget(f)) => Some(*f),
        None => None,
    }
}

/// A usage, then `@` and its collection when there is one.
fn qualified_words(usage: Option<&p::Usage>, collection: Option<&p::Usage>) -> String {
    let usage = usage.map_or_else(String::new, usage_words);
    match collection {
        Some(c) => format!("{usage}@{}", usage_words(c)),
        None => usage,
    }
}

/// A rule input as the CLI spells it: `USAGE@COLLECTION`.
pub fn input_words(at: &p::ProfileRuleRef) -> String {
    qualified_words(at.input.as_ref(), at.collection.as_ref())
}

/// An output as the CLI spells it: its usage, then `@` and its report's collection.
fn output_words(o: &profile_rule::Output) -> String {
    qualified_words(o.usage.as_ref(), o.collection.as_ref())
}

/// A rule as the CLI spells it, in the form `profile rule` takes: `INPUT remap OUTPUTS`,
/// `INPUT scale N/D`, or `INPUT axis AXIS [invert]`.
pub fn rule_words(rule: &p::ProfileRule) -> String {
    let input = input_words(&rule_ref(rule));
    let effect = match &rule.effect {
        Some(profile_rule::Effect::Remap(r)) if r.outputs.is_empty() => "remap disabled".into(),
        Some(profile_rule::Effect::Remap(r)) => format!(
            "remap {}",
            r.outputs
                .iter()
                .map(output_words)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Some(profile_rule::Effect::Scale(s)) => format!("scale {}/{}", s.numerator, s.denominator),
        Some(profile_rule::Effect::Axis(a)) => format!(
            "axis {}{}",
            a.destination.as_ref().map_or_else(String::new, usage_words),
            if a.invert { " invert" } else { "" }
        ),
        None => "unknown".into(),
    };
    format!("{input} {effect}")
}

fn in_range(r: &p::UsageRange, u: &p::Usage) -> bool {
    r.usage_page == u.usage_page && (r.min..=r.max).contains(&u.usage)
}

/// Whether `ranges` grant `kind` for `usage` in `collection`.
fn accepts(
    ranges: &[p::UsageRange],
    kind: p::usage_range::Kind,
    usage: &p::Usage,
    collection: Option<&p::Usage>,
) -> bool {
    ranges
        .iter()
        .any(|r| r.kind == kind as i32 && r.collection.as_ref() == collection && in_range(r, usage))
}

/// The collection of the report that carries an output: the one named, or the only one whose
/// range holds the usage. `Err` says why there is none.
fn output_collection(
    support: &p::ProfileSupport,
    o: &profile_rule::Output,
) -> Result<Option<p::Usage>, String> {
    let usage = o.usage.unwrap_or_default();
    if o.collection.is_some() {
        return accepts(
            &support.ranges,
            p::usage_range::Kind::RemapOutput,
            &usage,
            o.collection.as_ref(),
        )
        .then_some(o.collection)
        .ok_or_else(|| format!("this adapter can't produce {}", output_words(o)));
    }
    let mut found: Vec<Option<p::Usage>> = Vec::new();
    for r in support
        .ranges
        .iter()
        .filter(|r| r.kind == p::usage_range::Kind::RemapOutput as i32 && in_range(r, &usage))
    {
        if !found.contains(&r.collection) {
            found.push(r.collection);
        }
    }
    match found.len() {
        0 => Err(format!(
            "this adapter can't produce {}",
            usage_words(&usage)
        )),
        1 => Ok(found.remove(0)),
        _ => Err(format!(
            "several reports carry {}; add @PAGE:USAGE to choose one",
            usage_words(&usage)
        )),
    }
}

/// Why the adapter would refuse to save `rule`, as far as its profile support tells.
pub fn rule_refusal(support: &p::ProfileSupport, rule: &p::ProfileRule) -> Option<String> {
    let input = rule.input.unwrap_or_default();
    let collection = rule.collection.as_ref();
    let named = input_words(&rule_ref(rule));
    match &rule.effect {
        Some(profile_rule::Effect::Remap(remap)) => {
            if !accepts(
                &support.ranges,
                p::usage_range::Kind::RemapInput,
                &input,
                collection,
            ) {
                return Some(format!("this adapter can't remap {named}"));
            }
            // The adapter keeps each output once, so a repeated output doesn't count twice.
            let mut seen: Vec<(p::Usage, Option<p::Usage>)> = Vec::new();
            for o in &remap.outputs {
                let resolved = match output_collection(support, o) {
                    Ok(c) => (o.usage.unwrap_or_default(), c),
                    Err(why) => return Some(why),
                };
                if !seen.contains(&resolved) {
                    seen.push(resolved);
                }
            }
            let max = max_outputs(support)?;
            (seen.len() > max as usize).then(|| format!("a remap holds at most {max} outputs"))
        }
        Some(profile_rule::Effect::Scale(scale)) => {
            if !accepts(
                &support.ranges,
                p::usage_range::Kind::ScaleInput,
                &input,
                collection,
            ) {
                return Some(format!("this adapter can't scale {named}"));
            }
            (scale.numerator == 0 || scale.denominator == 0)
                .then(|| "a scale needs a nonzero numerator and denominator".into())
        }
        Some(profile_rule::Effect::Axis(axis)) => {
            if !accepts(
                &support.ranges,
                p::usage_range::Kind::AxisInput,
                &input,
                collection,
            ) {
                return Some(format!("this adapter can't map {named} to an axis"));
            }
            let destination = axis.destination.unwrap_or_default();
            (!accepts(
                &support.ranges,
                p::usage_range::Kind::AxisOutput,
                &destination,
                collection,
            ))
            .then(|| {
                format!(
                    "this adapter can't produce {}",
                    qualified_words(Some(&destination), collection)
                )
            })
        }
        None => Some("a rule needs a remap, a scale, or an axis".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(usage_page: u32, usage: u32) -> p::Usage {
        p::Usage { usage_page, usage }
    }

    const KEYBOARD: p::Usage = p::Usage {
        usage_page: 1,
        usage: 6,
    };
    const MOUSE: p::Usage = p::Usage {
        usage_page: 1,
        usage: 2,
    };
    const GAMEPAD: p::Usage = p::Usage {
        usage_page: 1,
        usage: 5,
    };

    fn range(
        kind: p::usage_range::Kind,
        page: u32,
        min: u32,
        max: u32,
        collection: p::Usage,
    ) -> p::UsageRange {
        p::UsageRange {
            kind: kind as i32,
            collection: Some(collection),
            usage_page: page,
            min,
            max,
        }
    }

    fn support() -> p::ProfileSupport {
        p::ProfileSupport {
            ranges: vec![
                range(p::usage_range::Kind::RemapInput, 0x07, 0x04, 0xe7, KEYBOARD),
                range(p::usage_range::Kind::RemapInput, 0x09, 1, 16, MOUSE),
                range(p::usage_range::Kind::RemapInput, 0x09, 1, 16, GAMEPAD),
                range(p::usage_range::Kind::ScaleInput, 0x01, 0x30, 0x38, MOUSE),
                range(
                    p::usage_range::Kind::RemapOutput,
                    0x07,
                    0x04,
                    0xe7,
                    KEYBOARD,
                ),
                range(p::usage_range::Kind::RemapOutput, 0x09, 1, 8, MOUSE),
                range(p::usage_range::Kind::RemapOutput, 0x09, 1, 8, KEYBOARD),
                range(p::usage_range::Kind::AxisInput, 0x01, 0x30, 0x35, GAMEPAD),
                range(p::usage_range::Kind::AxisOutput, 0x01, 0x30, 0x35, GAMEPAD),
            ],
            resources: vec![
                p::ResourceCapacity {
                    r#type: ResourceType::Memory as i32,
                    count: Some(0),
                    limit: Some(4096),
                },
                p::ResourceCapacity {
                    r#type: ResourceType::Outputs as i32,
                    count: None,
                    limit: Some(2),
                },
                p::ResourceCapacity {
                    r#type: ResourceType::Layers as i32,
                    count: None,
                    limit: Some(4),
                },
            ],
        }
    }

    fn remap(input: &str, outputs: &str) -> p::ProfileRule {
        rule(
            parse_input(input).unwrap(),
            profile_rule::Effect::Remap(profile_rule::Remap {
                outputs: parse_outputs(outputs).unwrap(),
            }),
        )
    }

    fn scale(input: &str, numerator: i32) -> p::ProfileRule {
        rule(
            parse_input(input).unwrap(),
            profile_rule::Effect::Scale(profile_rule::Scale {
                numerator,
                denominator: 1,
            }),
        )
    }

    fn axis(input: &str, destination: p::Usage, invert: bool) -> p::ProfileRule {
        rule(
            parse_input(input).unwrap(),
            profile_rule::Effect::Axis(profile_rule::Axis {
                destination: Some(destination),
                invert,
            }),
        )
    }

    #[test]
    fn usages_parse_and_print_in_hexadecimal() {
        assert_eq!(parse_usage("07:39").unwrap(), usage(7, 0x39));
        assert_eq!(parse_usage("0c:238").unwrap(), usage(0x0c, 0x238));
        assert_eq!(parse_usage("FF60:61").unwrap(), usage(0xff60, 0x61));
        for bad in [
            "", "07", "07:", ":39", "7:g1", "10000:1", "07:39:1", "0x07:39",
        ] {
            assert!(parse_usage(bad).is_err(), "{bad}");
        }
        assert_eq!(usage_words(&usage(0x0c, 0x238)), "0c:238");
        assert_eq!(usage_words(&usage(1, 2)), "01:02");
    }

    #[test]
    fn inputs_name_their_collection() {
        let at = parse_input("09:01@01:05").unwrap();
        assert_eq!(
            (at.input, at.collection),
            (Some(usage(9, 1)), Some(GAMEPAD))
        );
        assert_eq!(input_words(&at), "09:01@01:05");
        assert_eq!(
            parse_input("09:01").unwrap_err(),
            "\"09:01\" has no collection; add @PAGE:USAGE, such as 07:39@01:06"
        );
        for bad in ["09:01@", "09:01@01", "@01:05", "09:01@01:05@01:02"] {
            assert!(parse_input(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn outputs_and_ratios_parse() {
        assert!(parse_outputs("disabled").unwrap().is_empty());
        let outputs = parse_outputs("07:e0,07:04@01:06").unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].collection, None);
        assert_eq!(outputs[1].collection, Some(KEYBOARD));
        assert!(parse_outputs("07:e0,").is_err());
        let scale = parse_ratio("-1/1").unwrap();
        assert_eq!((scale.numerator, scale.denominator), (-1, 1));
        for bad in ["0/1", "1/0", "1/-2", "1", "a/b"] {
            assert!(parse_ratio(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rules_read_as_the_commands_that_make_them() {
        let rule = remap("09:01@01:02", "09:02@01:02");
        assert_eq!(rule_words(&rule), "09:01@01:02 remap 09:02@01:02");
        let disabled = remap("07:39@01:06", "disabled");
        assert_eq!(rule_words(&disabled), "07:39@01:06 remap disabled");
        assert_eq!(
            rule_words(&scale("01:38@01:02", -1)),
            "01:38@01:02 scale -1/1"
        );
        assert_eq!(
            rule_words(&axis("01:30@01:05", usage(1, 0x33), true)),
            "01:30@01:05 axis 01:33 invert"
        );
        assert_eq!(
            rule_words(&axis("01:31@01:05", usage(1, 0x31), false)),
            "01:31@01:05 axis 01:31"
        );
    }

    #[test]
    fn rules_are_checked_against_the_support() {
        let s = support();
        assert_eq!(rule_refusal(&s, &remap("07:39@01:06", "07:04")), None);
        // A button is a separate input in each collection the adapter accepts it in.
        assert_eq!(rule_refusal(&s, &remap("09:01@01:02", "09:02@01:02")), None);
        assert_eq!(rule_refusal(&s, &remap("09:01@01:05", "07:04")), None);
        assert_eq!(
            rule_refusal(&s, &remap("07:39@01:02", "07:04")).unwrap(),
            "this adapter can't remap 07:39@01:02"
        );
        assert!(rule_refusal(&s, &remap("09:11@01:02", "09:02@01:02")).is_some());
        // An output carried by several reports needs its collection.
        assert_eq!(
            rule_refusal(&s, &remap("07:04@01:06", "09:02")).unwrap(),
            "several reports carry 09:02; add @PAGE:USAGE to choose one"
        );
        assert_eq!(
            rule_refusal(&s, &remap("07:04@01:06", "09:02@01:05")).unwrap(),
            "this adapter can't produce 09:02@01:05"
        );
        assert!(rule_refusal(&s, &remap("07:04@01:06", "0c:e9")).is_some());
        assert_eq!(
            rule_refusal(&s, &remap("07:04@01:06", "07:e0,07:e1,07:04")).unwrap(),
            "a remap holds at most 2 outputs"
        );
        // Repeated outputs count once, as the adapter keeps each once.
        assert_eq!(
            rule_refusal(&s, &remap("07:04@01:06", "07:05,07:05@01:06,07:06")),
            None
        );
        assert_eq!(rule_refusal(&s, &remap("07:04@01:06", "disabled")), None);
        assert_eq!(rule_refusal(&s, &scale("01:38@01:02", 2)), None);
        assert!(rule_refusal(&s, &scale("07:04@01:06", 2)).is_some());
        // A Game Pad axis is not a mouse value to scale, nor a mouse value an axis.
        assert!(rule_refusal(&s, &scale("01:30@01:05", 2)).is_some());
        assert_eq!(
            rule_refusal(&s, &axis("01:30@01:02", usage(1, 0x31), false)).unwrap(),
            "this adapter can't map 01:30@01:02 to an axis"
        );
        assert_eq!(
            rule_refusal(&s, &axis("01:30@01:05", usage(1, 0x35), true)),
            None
        );
        assert_eq!(
            rule_refusal(&s, &axis("01:30@01:05", usage(1, 0x38), false)).unwrap(),
            "this adapter can't produce 01:38@01:05"
        );
    }

    #[test]
    fn capability_ranges_grant_only_their_kind() {
        use p::usage_range::Kind;
        let kinds = [
            Kind::RemapInput,
            Kind::RemapOutput,
            Kind::ScaleInput,
            Kind::AxisInput,
            Kind::AxisOutput,
        ];
        for (required, rule) in [
            (Kind::RemapInput, remap("07:04@01:06", "disabled")),
            (Kind::RemapOutput, remap("07:04@01:06", "07:05")),
            (Kind::RemapOutput, remap("07:04@01:06", "07:05@01:06")),
            (Kind::ScaleInput, scale("01:30@01:02", 2)),
            (Kind::AxisInput, axis("01:30@01:05", usage(1, 0x31), true)),
            (Kind::AxisOutput, axis("01:30@01:05", usage(1, 0x31), true)),
        ] {
            assert_eq!(rule_refusal(&support(), &rule), None);
            for other in kinds
                .into_iter()
                .map(|k| k as i32)
                .chain([Kind::Unspecified as i32, i32::MAX])
            {
                if other == required as i32 {
                    continue;
                }
                let mut s = support();
                for range in &mut s.ranges {
                    if range.kind == required as i32 {
                        range.kind = other;
                    }
                }
                assert!(
                    rule_refusal(&s, &rule).is_some(),
                    "{required:?} granted by {other}"
                );
            }
        }
    }

    fn interface_support(
        i: ConfigurationInterface,
        enabled: bool,
        profile: u32,
        conflicts: &[ConfigurationInterface],
    ) -> p::ConfigurationInterfaceSupport {
        p::ConfigurationInterfaceSupport {
            interface: i as i32,
            enabled,
            active: enabled,
            profile,
            conflicts: conflicts.iter().map(|c| *c as i32).collect(),
        }
    }

    #[test]
    fn interface_changes_respect_profiles_and_conflicts() {
        use ConfigurationInterface::{Via, Vial};
        let status = p::Status {
            profile_support: Some(support()),
            configuration_interfaces: vec![
                interface_support(Via, false, 0, &[Vial]),
                interface_support(Vial, false, 3, &[Via]),
                // An interface this build doesn't know is kept and never named.
                p::ConfigurationInterfaceSupport {
                    interface: 9,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(interfaces(&status).len(), 2);
        let update = |i, enabled: Option<bool>, profile: Option<u32>| InterfaceUpdate {
            interface: i,
            enabled,
            profile,
        };
        let refusal = |updates: &[InterfaceUpdate]| {
            interface_refusal(&status, updates).map(|e| crate::ui::text::error_line(&e))
        };
        assert_eq!(
            refusal(&[update(Via, Some(true), None)]).unwrap(),
            "choose a profile for VIA before turning it on"
        );
        assert_eq!(refusal(&[update(Via, Some(true), Some(2))]), None);
        assert_eq!(refusal(&[update(Vial, Some(true), None)]), None);
        assert_eq!(
            refusal(&[
                update(Via, Some(true), Some(2)),
                update(Vial, Some(true), None)
            ])
            .unwrap(),
            "VIA and Vial can't both be on. Turn one off first"
        );
        // Updates apply in order, so a later one replaces an earlier one.
        assert_eq!(
            refusal(&[
                update(Via, Some(true), Some(2)),
                update(Via, Some(false), None)
            ]),
            None
        );
        // An interface the adapter doesn't support is refused as such.
        let mut via_only = status.clone();
        via_only.configuration_interfaces.remove(1);
        let error = interface_refusal(&via_only, &[update(Vial, Some(false), None)]).unwrap();
        assert_eq!(error.code_of(), Some(ErrorCode::Unsupported));
    }

    #[test]
    fn refusals_are_separate_sentences() {
        let mut st = State::default();
        st.status.profile_support = Some(support());
        st.status.configuration_interfaces = vec![interface_support(
            ConfigurationInterface::Vial,
            false,
            1,
            &[],
        )];
        st.devices.push(p::Device {
            name: "Mouse".into(),
            profiles: Some(p::ProfileLayers {
                profiles: vec![2, 5],
            }),
            ..Default::default()
        });
        let reasons = [in_use(&st, 1).unwrap(), in_use(&st, 5).unwrap()];
        for reason in reasons {
            assert!(!reason.contains(';') && !reason.ends_with('.'), "{reason}");
        }
        assert_eq!(in_use(&st, 3), None);
        assert_eq!(unnamed(&st), [1, 2, 5]);
    }

    #[test]
    fn a_deletion_lists_every_user_and_keeps_the_other_layers_in_order() {
        let mut st = State::default();
        st.status.profile_support = Some(support());
        st.status.configuration_interfaces = vec![
            interface_support(ConfigurationInterface::Via, true, 5, &[]),
            interface_support(ConfigurationInterface::Vial, false, 2, &[]),
        ];
        let device = |id, name: &str, layers: Vec<u32>| p::Device {
            id,
            name: name.into(),
            profiles: Some(p::ProfileLayers { profiles: layers }),
            ..Default::default()
        };
        st.devices = vec![
            device(1, "Mouse", vec![2, 5, 3]),
            device(2, "Pad", vec![3]),
            device(3, "", vec![5]),
        ];
        let plan = Deletion::of(&st, 5);
        let names: Vec<&str> = plan.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Mouse", "(unnamed)"]);
        assert_eq!(plan.kept(&plan.devices[0]), [2, 3]);
        assert!(plan.kept(&plan.devices[1]).is_empty());
        assert_eq!(
            plan.interfaces,
            [Unassign {
                interface: ConfigurationInterface::Via,
                enabled: true
            }]
        );
        assert_eq!(
            plan.interface_updates(),
            [InterfaceUpdate {
                interface: ConfigurationInterface::Via,
                enabled: Some(false),
                profile: Some(0),
            }]
        );
        // The plan's interface changes are accepted as they stand.
        assert!(interface_refusal(&st.status, &plan.interface_updates()).is_none());
        let unused = Deletion::of(&st, 9);
        assert!(unused.devices.is_empty() && unused.interfaces.is_empty());
    }

    #[test]
    fn copy_names_fit() {
        assert_eq!(copy_name("Work"), "Work Copy");
        assert_eq!(copy_name(&"x".repeat(64)).len(), 64);
        assert_eq!(copy_name(&"é".repeat(32)).len(), 63);
        assert!(valid_name("Work") && !valid_name("") && !valid_name("a\u{7}"));
        assert!(!valid_name(&"x".repeat(65)));
    }
}
