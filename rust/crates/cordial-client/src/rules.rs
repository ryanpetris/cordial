//! The saved form of profile rules. A successful `SetProfileRules` means the Dongle holds each
//! rule sent in this form, so a client applies [`normalized`] to the rules it sent instead of
//! listing them again.
use crate::paging::usage_order;
use cordial_protocol::{self as p, profile_rule};
use std::cmp::Ordering;

const fn usage(usage_page: u32, usage: u32) -> p::Usage {
    p::Usage { usage_page, usage }
}

const DESKTOP_PAGE: u32 = 0x01;
const BUTTON_PAGE: u32 = 0x09;
const KEYBOARD_PAGE: u32 = 0x07;
const CONSUMER_PAGE: u32 = 0x0c;

/// The application collections of the Dongle's USB reports.
pub const KEYBOARD: p::Usage = usage(DESKTOP_PAGE, 0x06);
pub const MOUSE: p::Usage = usage(DESKTOP_PAGE, 0x02);
pub const CONSUMER: p::Usage = usage(CONSUMER_PAGE, 0x01);
pub const SYSTEM: p::Usage = usage(DESKTOP_PAGE, 0x80);

/// The relative values a mouse reports: X, Y, wheel and AC Pan.
const AXES: [p::Usage; 4] = [
    usage(DESKTOP_PAGE, 0x30),
    usage(DESKTOP_PAGE, 0x31),
    usage(DESKTOP_PAGE, 0x38),
    usage(CONSUMER_PAGE, 0x238),
];

/// The application collection device input of `input` arrives in, or `None` for a usage page
/// the Dongle does not forward.
pub fn input_collection(input: &p::Usage) -> Option<p::Usage> {
    match input.usage_page {
        _ if AXES.contains(input) => Some(MOUSE),
        KEYBOARD_PAGE => Some(KEYBOARD),
        BUTTON_PAGE => Some(MOUSE),
        CONSUMER_PAGE => Some(CONSUMER),
        DESKTOP_PAGE => Some(SYSTEM),
        _ => None,
    }
}

/// The collection of the only report in `support.remap_outputs` that carries `output`.
pub fn output_collection(support: &p::ProfileSupport, output: &p::Usage) -> Option<p::Usage> {
    let mut found = support
        .remap_outputs
        .iter()
        .filter(|r| r.usage_page == output.usage_page && (r.min..=r.max).contains(&output.usage))
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
/// `None` when the rule changes nothing, a remap of its input to only itself in the collection
/// the input arrives in or a scale by 1, since saving such a rule forgets the input's rule.
pub fn normalized(support: &p::ProfileSupport, rule: &p::ProfileRule) -> Option<p::ProfileRule> {
    let mut rule = rule.clone();
    let input = rule.input.unwrap_or_default();
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
                usage: Some(input),
                collection: input_collection(&input),
            };
            itself.collection.is_some() && remap.outputs == [itself]
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
        None => false,
    };
    (!identity).then_some(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(collection: p::Usage, page: u32, min: u32, max: u32) -> p::UsageRange {
        p::UsageRange {
            collection: Some(collection),
            usage_page: page,
            min,
            max,
        }
    }

    fn support() -> p::ProfileSupport {
        p::ProfileSupport {
            remap_outputs: vec![
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

    fn remap(input: p::Usage, outputs: Vec<profile_rule::Output>) -> p::ProfileRule {
        p::ProfileRule {
            input: Some(input),
            effect: Some(profile_rule::Effect::Remap(profile_rule::Remap { outputs })),
        }
    }

    fn scale(input: p::Usage, numerator: i32, denominator: u32) -> p::ProfileRule {
        p::ProfileRule {
            input: Some(input),
            effect: Some(profile_rule::Effect::Scale(profile_rule::Scale {
                numerator,
                denominator,
            })),
        }
    }

    #[test]
    fn remaps_fill_collections_and_sort_outputs_once() {
        let rule = remap(
            usage(KEYBOARD_PAGE, 0x39),
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
                usage(KEYBOARD_PAGE, 0x39),
                vec![
                    out(KEYBOARD_PAGE, 0x04, Some(KEYBOARD)),
                    out(KEYBOARD_PAGE, 0xe0, Some(KEYBOARD)),
                    out(CONSUMER_PAGE, 0xe9, Some(CONSUMER)),
                    out(0xff00, 1, None),
                ],
            ))
        );
        let disabled = remap(usage(KEYBOARD_PAGE, 0x39), Vec::new());
        assert_eq!(normalized(&support(), &disabled), Some(disabled));
    }

    #[test]
    fn identity_rules_are_forgotten() {
        let key = usage(KEYBOARD_PAGE, 0x39);
        assert_eq!(
            normalized(
                &support(),
                &remap(key, vec![out(KEYBOARD_PAGE, 0x39, None)])
            ),
            None
        );
        let twice = vec![
            out(KEYBOARD_PAGE, 0x39, Some(KEYBOARD)),
            out(KEYBOARD_PAGE, 0x39, None),
        ];
        assert_eq!(normalized(&support(), &remap(key, twice)), None);
        // A System Control button the System report carries.
        let sleep = usage(DESKTOP_PAGE, 0x82);
        assert_eq!(
            normalized(
                &support(),
                &remap(sleep, vec![out(DESKTOP_PAGE, 0x82, None)])
            ),
            None
        );
        // The same usage in another report changes the input.
        let other = remap(key, vec![out(KEYBOARD_PAGE, 0x39, Some(MOUSE))]);
        assert_eq!(normalized(&support(), &other), Some(other));
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
    fn inputs_arrive_in_their_collections() {
        assert_eq!(input_collection(&usage(KEYBOARD_PAGE, 4)), Some(KEYBOARD));
        assert_eq!(input_collection(&usage(BUTTON_PAGE, 1)), Some(MOUSE));
        assert_eq!(input_collection(&usage(CONSUMER_PAGE, 0x238)), Some(MOUSE));
        assert_eq!(
            input_collection(&usage(CONSUMER_PAGE, 0xe9)),
            Some(CONSUMER)
        );
        assert_eq!(input_collection(&usage(DESKTOP_PAGE, 0x38)), Some(MOUSE));
        assert_eq!(input_collection(&usage(DESKTOP_PAGE, 0x81)), Some(SYSTEM));
        assert_eq!(input_collection(&usage(0xff00, 1)), None);
    }
}
