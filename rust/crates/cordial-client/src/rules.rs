//! The saved form of profile rules. A successful `SetProfileRules` means the Dongle holds each
//! rule sent in this form, so a client applies [`normalized`] to the rules it sent instead of
//! listing them again.
use crate::order::usage_order;
use cordial_protocol::{self as p, profile_rule};
use std::cmp::Ordering;

const fn usage(usage_page: u32, usage: u32) -> p::Usage {
    p::Usage { usage_page, usage }
}

const DESKTOP_PAGE: u32 = 0x01;
const CONSUMER_PAGE: u32 = 0x0c;

/// The application collections of the Dongle's USB reports.
pub const KEYBOARD: p::Usage = usage(DESKTOP_PAGE, 0x06);
pub const MOUSE: p::Usage = usage(DESKTOP_PAGE, 0x02);
pub const CONSUMER: p::Usage = usage(CONSUMER_PAGE, 0x01);
pub const SYSTEM: p::Usage = usage(DESKTOP_PAGE, 0x80);

/// The only report collection with a `RemapOutput` range in `support.ranges` carrying `output`.
pub fn output_collection(support: &p::ProfileSupport, output: &p::Usage) -> Option<p::Usage> {
    let mut found = support
        .ranges
        .iter()
        .filter(|r| {
            r.kind == p::usage_range::Kind::RemapOutput as i32
                && r.usage_page == output.usage_page
                && (r.min..=r.max).contains(&output.usage)
        })
        .map(|r| r.collection);
    let first = found.next()??;
    found.all(|c| c == Some(first)).then_some(first)
}

fn output_order(a: &profile_rule::Output, b: &profile_rule::Output) -> Ordering {
    let key = |u: &Option<p::Usage>| u.unwrap_or_default();
    usage_order(&key(&a.usage), &key(&b.usage))
        .then_with(|| usage_order(&key(&a.collection), &key(&b.collection)))
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `rule` as the Dongle saves it: a missing output collection resolved to the only report that
/// carries the usage, each output kept once in ascending order, and a scale in lowest terms.
/// `None` when the rule changes nothing, since saving such a rule forgets the rule for its input
/// and collection: a remap of its input to only itself in the input's collection, a scale by 1,
/// or an axis routed to itself without inversion.
pub fn normalized(support: &p::ProfileSupport, rule: &p::ProfileRule) -> Option<p::ProfileRule> {
    let mut rule = rule.clone();
    let (input, collection) = (rule.input, rule.collection);
    let identity = match &mut rule.effect {
        Some(profile_rule::Effect::Remap(remap)) => {
            for o in &mut remap.outputs {
                if o.collection.is_none() {
                    o.collection = o.usage.and_then(|u| output_collection(support, &u));
                }
            }
            remap.outputs.sort_by(output_order);
            remap.outputs.dedup();
            let itself = profile_rule::Output {
                usage: input,
                collection,
            };
            input.is_some() && collection.is_some() && remap.outputs == [itself]
        }
        Some(profile_rule::Effect::Scale(scale)) => {
            let n = i64::from(scale.numerator);
            let d = i64::from(scale.denominator);
            let divisor = gcd(n.unsigned_abs(), d.unsigned_abs()) as i64;
            if divisor != 0 {
                // Dividing by a positive divisor keeps the sign and never grows the magnitude.
                scale.numerator = (n / divisor) as i32;
                scale.denominator = (d / divisor) as u32;
            }
            scale.numerator == 1 && scale.denominator == 1
        }
        Some(profile_rule::Effect::Axis(axis)) => {
            input.is_some() && axis.destination == input && !axis.invert
        }
        None => false,
    };
    (!identity).then_some(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUTTON_PAGE: u32 = 0x09;
    const KEYBOARD_PAGE: u32 = 0x07;
    const GAME_PAD: p::Usage = usage(DESKTOP_PAGE, 0x05);

    fn range(collection: p::Usage, page: u32, min: u32, max: u32) -> p::UsageRange {
        p::UsageRange {
            kind: p::usage_range::Kind::RemapOutput as i32,
            collection: Some(collection),
            usage_page: page,
            min,
            max,
        }
    }

    fn support() -> p::ProfileSupport {
        p::ProfileSupport {
            ranges: vec![
                range(KEYBOARD, KEYBOARD_PAGE, 0x04, 0xff),
                range(CONSUMER, CONSUMER_PAGE, 0x01, 0x237),
                range(MOUSE, BUTTON_PAGE, 1, 16),
                range(SYSTEM, DESKTOP_PAGE, 0x81, 0x8f),
                // A usage two reports carry has no single collection.
                range(KEYBOARD, 0xff00, 1, 1),
                range(MOUSE, 0xff00, 1, 1),
            ],
            ..Default::default()
        }
    }

    fn out(page: u32, id: u32, collection: Option<p::Usage>) -> profile_rule::Output {
        profile_rule::Output {
            usage: Some(usage(page, id)),
            collection,
        }
    }

    fn rule(input: p::Usage, collection: p::Usage, effect: profile_rule::Effect) -> p::ProfileRule {
        p::ProfileRule {
            input: Some(input),
            collection: Some(collection),
            effect: Some(effect),
        }
    }

    fn remap(
        input: p::Usage,
        collection: p::Usage,
        outputs: Vec<profile_rule::Output>,
    ) -> p::ProfileRule {
        rule(
            input,
            collection,
            profile_rule::Effect::Remap(profile_rule::Remap { outputs }),
        )
    }

    fn scale(input: p::Usage, numerator: i32, denominator: u32) -> p::ProfileRule {
        rule(
            input,
            MOUSE,
            profile_rule::Effect::Scale(profile_rule::Scale {
                numerator,
                denominator,
            }),
        )
    }

    fn axis(input: p::Usage, destination: p::Usage, invert: bool) -> p::ProfileRule {
        rule(
            input,
            GAME_PAD,
            profile_rule::Effect::Axis(profile_rule::Axis {
                destination: Some(destination),
                invert,
            }),
        )
    }

    #[test]
    fn output_collection_matches_only_remap_output_ranges() {
        use p::usage_range::Kind;
        let button = usage(BUTTON_PAGE, 1);
        let mut s = p::ProfileSupport::default();
        for kind in [
            Kind::Unspecified as i32,
            Kind::RemapInput as i32,
            Kind::ScaleInput as i32,
            Kind::AxisInput as i32,
            Kind::AxisOutput as i32,
            i32::MAX,
        ] {
            let mut r = range(GAME_PAD, BUTTON_PAGE, 1, 16);
            r.kind = kind;
            s.ranges.push(r);
            assert_eq!(output_collection(&s, &button), None);
        }
        s.ranges.push(range(MOUSE, BUTTON_PAGE, 1, 16));
        assert_eq!(output_collection(&s, &button), Some(MOUSE));
        s.ranges.push(range(GAME_PAD, BUTTON_PAGE, 1, 16));
        assert_eq!(output_collection(&s, &button), None);
    }

    #[test]
    fn remaps_fill_collections_and_sort_outputs_once() {
        let space = usage(KEYBOARD_PAGE, 0x39);
        let rule = remap(
            space,
            KEYBOARD,
            vec![
                out(KEYBOARD_PAGE, 0xe0, None),
                out(CONSUMER_PAGE, 0xe9, None),
                out(KEYBOARD_PAGE, 0x04, Some(KEYBOARD)),
                out(KEYBOARD_PAGE, 0xe0, Some(KEYBOARD)),
                out(0xff00, 1, None),
            ],
        );
        assert_eq!(
            normalized(&support(), &rule),
            Some(remap(
                space,
                KEYBOARD,
                vec![
                    out(KEYBOARD_PAGE, 0x04, Some(KEYBOARD)),
                    out(KEYBOARD_PAGE, 0xe0, Some(KEYBOARD)),
                    out(CONSUMER_PAGE, 0xe9, Some(CONSUMER)),
                    out(0xff00, 1, None),
                ],
            ))
        );
        let disabled = remap(space, KEYBOARD, Vec::new());
        assert_eq!(normalized(&support(), &disabled), Some(disabled));
    }

    #[test]
    fn identity_rules_are_forgotten() {
        let key = usage(KEYBOARD_PAGE, 0x39);
        assert_eq!(
            normalized(
                &support(),
                &remap(key, KEYBOARD, vec![out(KEYBOARD_PAGE, 0x39, None)])
            ),
            None
        );
        let twice = vec![
            out(KEYBOARD_PAGE, 0x39, Some(KEYBOARD)),
            out(KEYBOARD_PAGE, 0x39, None),
        ];
        assert_eq!(normalized(&support(), &remap(key, KEYBOARD, twice)), None);
        let sleep = usage(DESKTOP_PAGE, 0x82);
        assert_eq!(
            normalized(
                &support(),
                &remap(sleep, SYSTEM, vec![out(DESKTOP_PAGE, 0x82, None)])
            ),
            None
        );
        // The same usage in another collection changes the input.
        let other = remap(key, KEYBOARD, vec![out(KEYBOARD_PAGE, 0x39, Some(MOUSE))]);
        assert_eq!(normalized(&support(), &other), Some(other));
        let button = usage(BUTTON_PAGE, 1);
        let gamepad = remap(button, GAME_PAD, vec![out(BUTTON_PAGE, 1, None)]);
        assert_eq!(
            normalized(&support(), &gamepad),
            Some(remap(
                button,
                GAME_PAD,
                vec![out(BUTTON_PAGE, 1, Some(MOUSE))]
            ))
        );
        assert_eq!(
            normalized(&support(), &scale(usage(DESKTOP_PAGE, 0x38), 3, 3)),
            None
        );
        let invert = scale(usage(DESKTOP_PAGE, 0x38), -2, 2);
        assert_eq!(
            normalized(&support(), &invert),
            Some(scale(usage(DESKTOP_PAGE, 0x38), -1, 1))
        );
    }

    #[test]
    fn axis_rules_round_trip_unless_they_change_nothing() {
        let x = usage(DESKTOP_PAGE, 0x30);
        let y = usage(DESKTOP_PAGE, 0x31);
        assert_eq!(normalized(&support(), &axis(x, x, false)), None);
        for kept in [axis(x, x, true), axis(x, y, false), axis(x, y, true)] {
            assert_eq!(normalized(&support(), &kept), Some(kept));
        }
    }

    #[test]
    fn scales_are_kept_in_lowest_terms() {
        let x = usage(DESKTOP_PAGE, 0x30);
        assert_eq!(
            normalized(&support(), &scale(x, 6, 4)),
            Some(scale(x, 3, 2))
        );
        assert_eq!(
            normalized(&support(), &scale(x, i32::MIN, 1 << 31)),
            Some(scale(x, -1, 1))
        );
    }

    #[test]
    fn rules_order_by_input_then_collection() {
        use crate::order::{rule_order, rule_ref};
        let key = usage(KEYBOARD_PAGE, 0x39);
        let button = usage(BUTTON_PAGE, 1);
        let mut rules = [
            remap(button, GAME_PAD, Vec::new()),
            remap(button, MOUSE, Vec::new()),
            remap(key, KEYBOARD, Vec::new()),
        ];
        rules.sort_by(|a, b| rule_order(&rule_ref(a), &rule_ref(b)));
        let order: Vec<_> = rules.iter().map(|r| (r.input, r.collection)).collect();
        assert_eq!(
            order,
            [
                (Some(key), Some(KEYBOARD)),
                (Some(button), Some(MOUSE)),
                (Some(button), Some(GAME_PAD)),
            ]
        );
    }
}
