//! Profiles: what the adapter supports, device layers, configuration interfaces, HID usages and
//! rules in words, and the checks made before asking the adapter to change any of them.
use crate::{error::Error, model, view::State};
use cordial_protocol::{
    self as p, ConfigurationInterface, ErrorCode, Role, profile_rule, profile_rule_change,
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

/// The most profiles one device's layers can list, when the adapter has profiles.
pub fn max_layers(status: &p::Status) -> Option<u32> {
    support(status).map(|s| s.max_layers)
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

/// Whether saving `updates` reconnects USB: an interface is enabled or disabled, or an enabled
/// interface changes its profile.
pub fn reconnects(status: &p::Status, updates: &[InterfaceUpdate]) -> bool {
    reconnected(
        &status.configuration_interfaces,
        &configured(status, updates),
    )
}

/// Whether going from the `before` interfaces to the `after` ones reconnects USB.
pub fn reconnected(
    before: &[p::ConfigurationInterfaceSupport],
    after: &[p::ConfigurationInterfaceSupport],
) -> bool {
    before.iter().any(|old| {
        after
            .iter()
            .find(|new| new.interface == old.interface)
            .is_some_and(|new| {
                old.enabled != new.enabled || new.enabled && old.profile != new.profile
            })
    })
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

/// A change that saves `rule`.
pub fn save_rule(rule: p::ProfileRule) -> p::ProfileRuleChange {
    p::ProfileRuleChange {
        change: Some(profile_rule_change::Change::Rule(rule)),
    }
}

/// A change that forgets the rule for `input`.
pub fn forget_rule(input: p::Usage) -> p::ProfileRuleChange {
    p::ProfileRuleChange {
        change: Some(profile_rule_change::Change::Forget(p::ProfileRuleRef {
            input: Some(input),
        })),
    }
}

/// The input a change names.
pub fn change_target(change: &p::ProfileRuleChange) -> Option<&p::Usage> {
    match &change.change {
        Some(profile_rule_change::Change::Rule(r)) => r.input.as_ref(),
        Some(profile_rule_change::Change::Forget(f)) => f.input.as_ref(),
        None => None,
    }
}

/// An output as the CLI spells it: its usage, then `@` and its report's collection.
fn output_words(o: &profile_rule::Output) -> String {
    let usage = o.usage.as_ref().map_or_else(String::new, usage_words);
    match &o.collection {
        Some(c) => format!("{usage}@{}", usage_words(c)),
        None => usage,
    }
}

/// A rule as the CLI spells it, in the form `profile rule` takes: `INPUT remap OUTPUTS` or
/// `INPUT scale N/D`.
pub fn rule_words(rule: &p::ProfileRule) -> String {
    let input = rule.input.as_ref().map_or_else(String::new, usage_words);
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
        None => "unknown".into(),
    };
    format!("{input} {effect}")
}

fn in_range(r: &p::UsageRange, u: &p::Usage) -> bool {
    r.usage_page == u.usage_page && (r.min..=r.max).contains(&u.usage)
}

/// Whether a rule input is within `ranges`.
fn accepts_input(ranges: &[p::UsageRange], input: &p::Usage) -> bool {
    ranges.iter().any(|r| in_range(r, input))
}

/// The collection of the report that carries an output: the one named, or the only one whose
/// range holds the usage. `Err` says why there is none.
fn output_collection(
    support: &p::ProfileSupport,
    o: &profile_rule::Output,
) -> Result<Option<p::Usage>, String> {
    let usage = o.usage.unwrap_or_default();
    let mut matching = support
        .remap_outputs
        .iter()
        .filter(|r| in_range(r, &usage))
        .map(|r| r.collection);
    match &o.collection {
        Some(c) => matching
            .any(|r| r.as_ref().is_none_or(|r| r == c))
            .then_some(Some(*c))
            .ok_or_else(|| format!("this adapter can't produce {}", output_words(o))),
        None => {
            let mut found: Vec<Option<p::Usage>> = Vec::new();
            for c in matching {
                if !found.contains(&c) {
                    found.push(c);
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
    }
}

/// Why the adapter would refuse to save `rule`, as far as its profile support tells.
pub fn rule_refusal(support: &p::ProfileSupport, rule: &p::ProfileRule) -> Option<String> {
    let input = rule.input.unwrap_or_default();
    match &rule.effect {
        Some(profile_rule::Effect::Remap(remap)) => {
            if !accepts_input(&support.remap_inputs, &input) {
                return Some(format!("this adapter can't remap {}", usage_words(&input)));
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
            (seen.len() > support.max_remap_outputs as usize).then(|| {
                format!(
                    "a remap holds at most {} outputs",
                    support.max_remap_outputs
                )
            })
        }
        Some(profile_rule::Effect::Scale(scale)) => {
            if !accepts_input(&support.scale_inputs, &input) {
                return Some(format!("this adapter can't scale {}", usage_words(&input)));
            }
            (scale.numerator == 0 || scale.denominator == 0)
                .then(|| "a scale needs a nonzero numerator and denominator".into())
        }
        None => Some("a rule needs a remap or a scale".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(usage_page: u32, usage: u32) -> p::Usage {
        p::Usage { usage_page, usage }
    }

    fn range(page: u32, min: u32, max: u32, collection: Option<p::Usage>) -> p::UsageRange {
        p::UsageRange {
            collection,
            usage_page: page,
            min,
            max,
        }
    }

    fn support() -> p::ProfileSupport {
        p::ProfileSupport {
            remap_inputs: vec![range(0x07, 0x04, 0xe7, None), range(0x09, 1, 16, None)],
            scale_inputs: vec![range(0x01, 0x30, 0x38, None)],
            remap_outputs: vec![
                range(0x07, 0x04, 0xe7, Some(usage(1, 6))),
                range(0x09, 1, 8, Some(usage(1, 2))),
                range(0x09, 1, 8, Some(usage(1, 6))),
            ],
            memory_budget: 4096,
            max_remap_outputs: 2,
            max_layers: 4,
            memory_used: 0,
        }
    }

    fn remap(input: p::Usage, outputs: &str) -> p::ProfileRule {
        p::ProfileRule {
            input: Some(input),
            effect: Some(profile_rule::Effect::Remap(profile_rule::Remap {
                outputs: parse_outputs(outputs).unwrap(),
            })),
        }
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
    fn outputs_and_ratios_parse() {
        assert!(parse_outputs("disabled").unwrap().is_empty());
        let outputs = parse_outputs("07:e0,07:04@01:06").unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].collection, None);
        assert_eq!(outputs[1].collection, Some(usage(1, 6)));
        assert!(parse_outputs("07:e0,").is_err());
        let scale = parse_ratio("-1/1").unwrap();
        assert_eq!((scale.numerator, scale.denominator), (-1, 1));
        for bad in ["0/1", "1/0", "1/-2", "1", "a/b"] {
            assert!(parse_ratio(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rules_read_as_the_commands_that_make_them() {
        let rule = remap(usage(9, 1), "09:02@01:02");
        assert_eq!(rule_words(&rule), "09:01 remap 09:02@01:02");
        let disabled = remap(usage(7, 0x39), "disabled");
        assert_eq!(rule_words(&disabled), "07:39 remap disabled");
        let scale = p::ProfileRule {
            input: Some(usage(1, 0x38)),
            effect: Some(profile_rule::Effect::Scale(profile_rule::Scale {
                numerator: -1,
                denominator: 1,
            })),
        };
        assert_eq!(rule_words(&scale), "01:38 scale -1/1");
    }

    #[test]
    fn rules_are_checked_against_the_support() {
        let s = support();
        assert_eq!(rule_refusal(&s, &remap(usage(7, 0x39), "07:04")), None);
        assert_eq!(rule_refusal(&s, &remap(usage(9, 1), "09:02@01:02")), None);
        assert!(rule_refusal(&s, &remap(usage(9, 17), "09:02@01:02")).is_some());
        // An output carried by several reports needs its collection.
        assert_eq!(
            rule_refusal(&s, &remap(usage(7, 4), "09:02")).unwrap(),
            "several reports carry 09:02; add @PAGE:USAGE to choose one"
        );
        assert!(rule_refusal(&s, &remap(usage(7, 4), "09:02@01:0a")).is_some());
        assert!(rule_refusal(&s, &remap(usage(7, 4), "0c:e9")).is_some());
        assert_eq!(
            rule_refusal(&s, &remap(usage(7, 4), "07:e0,07:e1,07:04")).unwrap(),
            "a remap holds at most 2 outputs"
        );
        // Repeated outputs count once, as the adapter keeps each once.
        assert_eq!(
            rule_refusal(&s, &remap(usage(7, 4), "07:05,07:05@01:06,07:06")),
            None
        );
        assert_eq!(rule_refusal(&s, &remap(usage(7, 4), "disabled")), None);
        let scale = |input: p::Usage| p::ProfileRule {
            input: Some(input),
            effect: Some(profile_rule::Effect::Scale(profile_rule::Scale {
                numerator: 2,
                denominator: 1,
            })),
        };
        assert_eq!(rule_refusal(&s, &scale(usage(1, 0x38))), None);
        assert!(rule_refusal(&s, &scale(usage(7, 4))).is_some());
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
        assert!(reconnects(&status, &[update(Via, Some(true), Some(2))]));
        // A disabled interface's profile changes without reconnecting.
        assert!(!reconnects(&status, &[update(Vial, None, Some(4))]));
        let mut enabled = status.clone();
        enabled.configuration_interfaces[1].enabled = true;
        assert!(reconnects(&enabled, &[update(Vial, None, Some(4))]));
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
    fn copy_names_fit() {
        assert_eq!(copy_name("Work"), "Work Copy");
        assert_eq!(copy_name(&"x".repeat(64)).len(), 64);
        assert_eq!(copy_name(&"é".repeat(32)).len(), 63);
        assert!(valid_name("Work") && !valid_name("") && !valid_name("a\u{7}"));
        assert!(!valid_name(&"x".repeat(65)));
    }
}
