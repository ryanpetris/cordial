use cordial_core::{forward::*, hid::*};

fn key(held: &Held, usage: usize) -> bool {
    held.keys[usage / 8] & (1 << (usage % 8)) != 0
}
#[test]
fn captured_keyboard_descriptor_preserves_long_vendor_and_standard_reports() {
    // MX Keys 046D:B35B descriptor captured from Bluetooth sysfs. Device-specific
    // data is a test fixture; runtime qualification uses the HID application.
    let mut descriptor: Vec<u8> = include_str!("fixtures/mx_keys_descriptor.hex")
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect();
    let map = Map::compile(&descriptor).unwrap();
    assert_eq!(map.hidpp_reports, HIDPP_LONG);
    assert_eq!(map.reports().len(), 4);
    assert_eq!(
        map.roles,
        cordial_core::hid::KEYBOARD | cordial_core::hid::MOUSE | CONSUMER
    );
    let mut state = map.state().unwrap();
    let keys = map.decode(&mut state, 1, &[2, 4, 0, 0, 0, 0, 0]).unwrap();
    assert!(key(&keys.held, 225) && key(&keys.held, 4));
    let media = map.decode(&mut state, 3, &[0xe9, 0, 0, 0]).unwrap();
    assert!(media.held.consumers.contains(&0xe9));
    let vendor = map.decode(&mut state, 0x11, &[0; 19]).unwrap();
    assert!(key(&vendor.held, 4) && vendor.held.consumers.contains(&0xe9));
    let mut leds = [0xff, 0xaa];
    assert_eq!(map.led_report(0, 0x15, &mut leds), Some(1));
    assert_eq!(leds, [0x15, 0xaa]);
    assert_eq!(map.led_report(3, 31, &mut leds), None);
    descriptor[157] = 3;
    assert_eq!(Map::compile(&descriptor).unwrap().hidpp_reports, 0);
    descriptor[157] = 2;
    descriptor[154] = 0x44;
    assert_eq!(Map::compile(&descriptor).unwrap().hidpp_reports, 0);
}
#[test]
fn field_word_preserves_maximum_count_offset_and_report_index() {
    let mut descriptor = vec![5, 1, 9, 6, 0xa1, 1, 5, 7, 0x15, 0, 0x25, 1, 0x75, 1];
    for id in 1..=16 {
        descriptor
            .extend_from_slice(&[0x85, id, 0x96, 0xff, 0x0f, 0x81, 1, 0x95, 1, 9, 4, 0x81, 2]);
    }
    descriptor.push(0xc0);
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    let mut data = [0; REPORT_BYTES];
    data[511] = 0x80;
    assert!(key(&map.decode(&mut state, 16, &data).unwrap().held, 4));
    data[511] = 0;
    assert!(!key(&map.decode(&mut state, 16, &data).unwrap().held, 4));
    let descriptor = [
        5, 1, 9, 6, 0xa1, 1, 5, 7, 0x15, 0, 0x25, 1, 0x75, 1, 0x96, 0, 0x10, 9, 0, 9, 4, 0x81, 0,
        0xc0,
    ];
    let map = Map::compile(&descriptor).unwrap();
    data[511] = 0x80;
    assert!(key(
        &map.decode(&mut map.state().unwrap(), 0, &data)
            .unwrap()
            .held,
        4
    ));
}
#[test]
fn an_overflowing_union_keeps_a_valid_reports_key_release() {
    let mut descriptor = vec![
        0x05, 1, 0x09, 6, 0xa1, 1, 0x85, 1, 0x05, 7, 0x19, 0, 0x29, 0xff, 0x15, 0, 0x26, 0xff, 0,
        0x75, 8, 0x95, 1, 0x81, 0, 0xc0, 0x05, 0x0c, 0x09, 1, 0xa1, 1, 0x85, 1, 0x19, 0, 0x2a,
        0xff, 3, 0x15, 0, 0x26, 0xff, 3, 0x75, 16, 0x95, 1, 0x81, 0, 0xc0,
    ];
    for id in [2, 3] {
        descriptor.extend_from_slice(&[
            0x05, 0x0c, 0x09, 1, 0xa1, 1, 0x85, id, 0x19, 0, 0x2a, 0xff, 3, 0x15, 0, 0x26, 0xff, 3,
            0x75, 16, 0x95, 4, 0x81, 0, 0xc0,
        ]);
    }
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    map.decode(&mut state, 1, &[4, 0, 0]).unwrap();
    map.decode(&mut state, 2, &[1, 0, 2, 0, 3, 0, 4, 0])
        .unwrap();
    let next = map
        .decode(&mut state, 3, &[5, 0, 6, 0, 7, 0, 8, 0])
        .unwrap();
    assert!(key(&next.held, 4));
    assert_eq!(map.decode(&mut state, 1, &[0, 9, 0]), Err(Error::Overflow));
    let next = map
        .decode(&mut state, 2, &[1, 0, 2, 0, 3, 0, 0, 0])
        .unwrap();
    assert!(!key(&next.held, 4));
    assert_eq!(next.held.consumers, [1, 2, 3, 5, 6, 7, 8, 9]);
}
#[test]
fn keyboard_mouse_media_and_lock_indicators() {
    let map = Map::compile(KEYBOARD).unwrap();
    assert_eq!(map.roles, cordial_core::hid::KEYBOARD);
    assert!(!map.numbered);
    let mut state = map.state().unwrap();
    let mut report = [2, 0, 4, 5, 6, 7, 8, 9];
    let input = map.decode(&mut state, 0, &report).unwrap();
    assert!(key(&input.held, 225) && key(&input.held, 4) && key(&input.held, 9));
    assert!(!key(&input.held, 10));
    report[2] = 1;
    assert_eq!(map.decode(&mut state, 0, &report), Err(Error::Rollover));
    assert_eq!(map.decode(&mut state, 0, &report[..7]), Err(Error::Invalid));
    let mut leds = [0xff; 4];
    assert_eq!(map.led_report(0, 0x15, &mut leds), Some(1));
    assert_eq!(leds, [0x15, 0xff, 0xff, 0xff]);
    assert_eq!(
        map.decode(&mut state, 0, &[0; 8]).unwrap().held,
        Held::default()
    );

    let map = Map::compile(MOUSE).unwrap();
    let mut state = map.state().unwrap();
    let mut report = [0x81, 0x80, 0xff, 0x0f, 0x80, 0x81, 0x7f];
    let input = map.decode(&mut state, 9, &report).unwrap();
    assert_eq!(input.held.buttons, 0x8081);
    assert_eq!(input.motion, [-1, -2048, -127, 127]);
    report[..2].fill(0);
    report[5] = 0x80;
    let input = map.decode(&mut state, 9, &report).unwrap();
    assert_eq!(input.held.buttons, 0);
    assert_eq!(input.motion[2], -127);

    let map = Map::compile(NKRO_MEDIA).unwrap();
    let mut state = map.state().unwrap();
    let mut report = [0; 32];
    report[..4].copy_from_slice(&[0xf0, 0xff, 0xff, 0xff]);
    report[28] = 2;
    let input = map.decode(&mut state, 1, &report).unwrap();
    for usage in 4..32 {
        assert!(key(&input.held, usage));
    }
    assert!(key(&input.held, 225));
    let input = map.decode(&mut state, 2, &[0xe9, 0, 0xcd, 0]).unwrap();
    assert!(key(&input.held, 4));
    assert_eq!(&input.held.consumers[..2], &[0xcd, 0xe9]);
    report.fill(0);
    report[1] = 1;
    map.decode(&mut state, 3, &report).unwrap();
    let input = map.decode(&mut state, 1, &[0; 32]).unwrap();
    assert!(!key(&input.held, 4) && key(&input.held, 8));
    assert!(input.held.consumers.contains(&0xcd));
}

#[test]
fn bounded_descriptors_and_qualified_vendor_reports() {
    for suffix in [&[0xb4][..], &[0x85, 1]] {
        let mut invalid = KEYBOARD.to_vec();
        invalid.extend_from_slice(suffix);
        assert!(matches!(Map::compile(&invalid), Err(Error::Invalid)));
    }
    assert!(matches!(
        Map::compile(&KEYBOARD[..KEYBOARD.len() - 1]),
        Err(Error::Invalid)
    ));
    let mut absolute = MOUSE.to_vec();
    absolute[41] = 2;
    assert!(Map::compile(&absolute).is_ok());
    let mut descriptor = NKRO_MEDIA.to_vec();
    let vendor = [
        0x06, 0x00, 0xff, 0x09, 1, 0xa1, 1, 0x85, 0x10, 0x75, 8, 0x95, 6, 0x15, 0, 0x26, 0xff, 0,
        0x09, 1, 0x81, 0, 0x09, 1, 0x91, 0, 0x85, 0x11, 0x95, 19, 0x09, 2, 0x81, 0, 0x09, 2, 0x91,
        0, 0xc0,
    ];
    descriptor.extend_from_slice(&vendor);
    let map = Map::compile(&descriptor).unwrap();
    assert_eq!(map.hidpp_reports, HIDPP_SHORT | HIDPP_LONG);
    let mut state = map.state().unwrap();
    let mut report = [0; 32];
    report[0] = 0x10;
    map.decode(&mut state, 1, &report).unwrap();
    let input = map.decode(&mut state, 0x11, &[0; 19]).unwrap();
    assert!(key(&input.held, 4));
    assert!(map.led_report(4, 31, &mut [0; 20]).is_none());
    descriptor[NKRO_MEDIA.len() + 1] = 1;
    assert_eq!(Map::compile(&descriptor).unwrap().hidpp_reports, 0);
    descriptor[NKRO_MEDIA.len() + 1] = 0;
    descriptor[NKRO_MEDIA.len() + 29] = 18;
    assert_eq!(
        Map::compile(&descriptor).unwrap().hidpp_reports,
        HIDPP_SHORT
    );
}

fn drain(forward: &mut Forwarder) -> Vec<Packet> {
    let mut packets = Vec::new();
    while let Some(packet) = forward.packet().cloned() {
        packets.push(packet);
        forward.complete();
        assert!(packets.len() < 512);
    }
    assert_eq!(forward.pending(), 0);
    packets
}

#[test]
fn usb_completion_and_source_removal_preserve_releases() {
    let mut forward = Forwarder::default();
    assert_eq!(drain(&mut forward).len(), 5);
    let mut pressed = Input::default();
    pressed.held.keys[0] = 1 << 4;
    forward.input(0, pressed).unwrap();
    let packet = forward.packet().unwrap().clone();
    assert_eq!(packet.id, REPORT_KEYBOARD);
    assert_eq!(packet.bytes()[0], 1 << 4);
    forward.remove(0);
    assert_eq!(forward.packet().unwrap().bytes(), packet.bytes());
    forward.complete();
    let packets = drain(&mut forward);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].bytes(), &[0; 32]);

    forward.input(0, pressed).unwrap();
    forward.input(1, pressed).unwrap();
    drain(&mut forward);
    forward.remove(0);
    assert!(drain(&mut forward).is_empty());
    forward.remove(1);
    assert_eq!(drain(&mut forward)[0].bytes(), &[0; 32]);
}

#[test]
fn motion_is_conserved_and_consumer_overflow_is_explicit() {
    let mut forward = Forwarder::default();
    drain(&mut forward);
    for _ in 0..2 {
        forward
            .input(
                0,
                Input {
                    motion: [MOTION_LIMIT, -MOTION_LIMIT, 1, -1],
                    ..Input::default()
                },
            )
            .unwrap();
    }
    let mut sum = [0i64; 4];
    for packet in drain(&mut forward) {
        assert_eq!(packet.id, REPORT_MOUSE);
        for (i, value) in sum.iter_mut().enumerate() {
            *value += i64::from(i16::from_le_bytes([
                packet.bytes()[2 + 2 * i],
                packet.bytes()[3 + 2 * i],
            ]));
        }
    }
    assert_eq!(sum, [2 * MOTION_LIMIT, -2 * MOTION_LIMIT, 2, -2]);
    let mut controls = Input::default();
    for usage in (1..=8).rev() {
        controls.held.consumer(usage).unwrap();
    }
    assert_eq!(controls.held.consumers, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(controls.held.consumer(9), Err(Error::Overflow));
    forward.input(0, controls).unwrap();
    drain(&mut forward);
    let mut ninth = Input::default();
    ninth.held.consumer(9).unwrap();
    assert_eq!(forward.input(1, ninth), Err(Error::Overflow));
    forward.remove(0);
    forward.input(1, ninth).unwrap();
    let packets = drain(&mut forward);
    assert_eq!(packets.last().unwrap().bytes()[..2], [9, 0]);
    forward.enable(false);
    forward.input(1, Input::default()).unwrap();
    assert!(forward.packet().is_none());
    forward.enable(true);
    assert_eq!(drain(&mut forward).len(), 5);
}

const KEYBOARD: &[u8] = &[
    0x05, 1, 0x09, 6, 0xa1, 1, 0x05, 7, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8,
    0x81, 2, 0x75, 8, 0x95, 1, 0x81, 1, 0x05, 8, 0x19, 1, 0x29, 5, 0x75, 1, 0x95, 5, 0x91, 2, 0x75,
    3, 0x95, 1, 0x91, 1, 0x05, 7, 0x19, 0, 0x29, 0x65, 0x15, 0, 0x25, 0x65, 0x75, 8, 0x95, 6, 0x81,
    0, 0xc0,
];
const MOUSE: &[u8] = &[
    0x05, 1, 0x09, 2, 0xa1, 1, 0x85, 9, 0x05, 9, 0x19, 1, 0x29, 16, 0x15, 0, 0x25, 1, 0x75, 1,
    0x95, 16, 0x81, 2, 0x05, 1, 0x09, 0x30, 0x09, 0x31, 0x16, 0, 0xf8, 0x26, 0xff, 7, 0x75, 12,
    0x95, 2, 0x81, 6, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 8, 0x95, 1, 0x81, 6, 0x05, 12,
    0x0a, 0x38, 2, 0x81, 6, 0xc0,
];
const NKRO_MEDIA: &[u8] = &[
    0x05, 1, 0x09, 6, 0xa1, 1, 0x85, 1, 0x05, 7, 0x19, 0, 0x2a, 0xff, 0, 0x15, 0, 0x25, 1, 0x75, 1,
    0x96, 0, 1, 0x81, 2, 0xa4, 0x85, 2, 0x05, 12, 0x19, 0, 0x2a, 0xff, 3, 0x15, 0, 0x26, 0xff, 3,
    0x75, 16, 0x95, 2, 0x81, 0, 0xb4, 0x85, 3, 0x19, 0, 0x2a, 0xff, 0, 0x81, 2, 0xc0,
];

fn consumer_field(usage: u16, size: u8, minimum: i16, maximum: u16, flags: u8) -> Vec<u8> {
    let [min_lo, min_hi] = minimum.to_le_bytes();
    let [max_lo, max_hi] = maximum.to_le_bytes();
    let [usage_lo, usage_hi] = usage.to_le_bytes();
    vec![
        5, 12, 9, 1, 0xa1, 1, 0x0a, usage_lo, usage_hi, 0x16, min_lo, min_hi, 0x26, max_lo, max_hi,
        0x75, size, 0x95, 1, 0x81, flags, 0xc0,
    ]
}

#[test]
fn consumer_counts_keep_order_and_retry_bytes_until_completion() {
    let map = Map::compile(&consumer_field(0xe0, 16, i16::MIN, i16::MAX as u16, 6)).unwrap();
    let mut state = map.state().unwrap();
    let mut forward = Forwarder::default();
    drain(&mut forward);
    let axis = CONSUMER_AXES.iter().position(|&u| u == 0xe0).unwrap();
    for value in [i16::MAX, -3] {
        let input = map.decode(&mut state, 0, &value.to_le_bytes()).unwrap();
        assert_eq!(input.consumer_motion[axis], i64::from(value));
        assert_eq!(input.held, Held::default());
        forward.input(0, input).unwrap();
    }
    let first = forward.packet().unwrap().clone();
    assert_eq!(first.id, REPORT_CONSUMER_MOTION);
    assert_eq!(first.bytes(), forward.packet().unwrap().bytes());
    assert_eq!(
        i32::from_le_bytes(first.bytes()[axis * 4..axis * 4 + 4].try_into().unwrap()),
        i32::from(i16::MAX)
    );
    forward.complete();
    let second = forward.packet().unwrap().clone();
    assert_eq!(
        i32::from_le_bytes(second.bytes()[axis * 4..axis * 4 + 4].try_into().unwrap()),
        -3
    );
    forward.complete();
    assert!(forward.packet().is_none());
    let mut large = Input::default();
    large.consumer_motion[axis] = MOTION_LIMIT;
    forward.input(0, large).unwrap();
    let sum: i64 = drain(&mut forward)
        .iter()
        .map(|p| {
            i64::from(i32::from_le_bytes(
                p.bytes()[axis * 4..axis * 4 + 4].try_into().unwrap(),
            ))
        })
        .sum();
    assert_eq!(sum, MOTION_LIMIT);
    forward.enable(false);
    forward.input(0, large).unwrap();
    forward.enable(true);
    assert!(
        drain(&mut forward)
            .iter()
            .all(|p| p.id != REPORT_CONSUMER_MOTION)
    );
}

#[test]
fn preferred_linear_buttons_emit_only_edges() {
    let map = Map::compile(&consumer_field(0xe0, 2, -1, 1, 6)).unwrap();
    let mut state = map.state().unwrap();
    let axis = CONSUMER_AXES.iter().position(|&u| u == 0xe0).unwrap();
    for (raw, expected) in [(1, 1), (1, 0), (0, 0), (3, -1), (3, 0)] {
        assert_eq!(
            map.decode(&mut state, 0, &[raw]).unwrap().consumer_motion[axis],
            expected
        );
    }
    state.clear();
    assert_eq!(
        map.decode(&mut state, 0, &[1]).unwrap().consumer_motion[axis],
        1
    );
}

#[test]
fn relative_on_off_controls_use_distinct_reports_and_rearm_after_completion() {
    let mut forward = Forwarder::default();
    drain(&mut forward);
    for (minimum, size, report, value) in [
        (0, 1, REPORT_CONSUMER_TOGGLE, 1),
        (-1, 2, REPORT_CONSUMER_ON_OFF, 3),
    ] {
        let map = Map::compile(&consumer_field(
            0xe2,
            size,
            minimum,
            1,
            if minimum == 0 { 6 } else { 0x26 },
        ))
        .unwrap();
        let mut state = map.state().unwrap();
        let switch = CONSUMER_SWITCHES.iter().position(|&u| u == 0xe2).unwrap();
        for raw in [value, value, 0, value] {
            let input = map.decode(&mut state, 0, &[raw]).unwrap();
            assert_eq!(input.held, Held::default());
            forward.input(0, input).unwrap();
        }
        let packets = drain(&mut forward);
        assert_eq!(packets.len(), 4);
        assert!(packets.iter().all(|p| p.id == report));
        let width = if size == 1 { 1 } else { 2 };
        let bit = switch * width;
        for pair in packets.as_chunks::<2>().0 {
            assert_eq!(
                (pair[0].bytes()[bit / 8] >> (bit % 8)) & ((1 << width) - 1),
                value
            );
            assert!(pair[1].bytes().iter().all(|&v| v == 0));
        }
    }
}

#[test]
fn one_shot_release_precedes_bulk_motion_and_keeps_other_sources_held() {
    let map = Map::compile(&consumer_field(0xcd, 1, 0, 1, 6)).unwrap();
    let mut state = map.state().unwrap();
    let mut input = map.decode(&mut state, 0, &[1]).unwrap();
    input.motion[0] = MOTION_LIMIT;
    let mut forward = Forwarder::default();
    drain(&mut forward);
    let mut other = Input::default();
    other.held.consumer(0xe9).unwrap();
    forward.input(1, other).unwrap();
    drain(&mut forward);
    forward.input(0, input).unwrap();
    let packets = drain(&mut forward);
    assert_eq!(packets[0].id, REPORT_CONSUMER);
    assert_eq!(packets[0].bytes()[..4], [0xcd, 0, 0xe9, 0]);
    assert_eq!(packets[1].id, REPORT_CONSUMER);
    assert_eq!(packets[1].bytes()[..4], [0xe9, 0, 0, 0]);
    assert!(packets[2..].iter().all(|p| p.id == REPORT_MOUSE));
    assert_eq!(
        map.decode(&mut state, 0, &[1]).unwrap().pulses,
        Held::default()
    );
}

#[test]
fn rejected_reports_do_not_change_relative_keyboard_latches() {
    let descriptor = [
        5, 1, 9, 6, 0xa1, 1, 5, 7, 9, 4, 0x15, 0xff, 0x25, 1, 0x75, 2, 0x95, 1, 0x81, 6, 0x19, 0,
        0x29, 0x65, 0x15, 0, 0x25, 0x65, 0x75, 8, 0x81, 0, 0xc0,
    ];
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    assert_eq!(map.decode(&mut state, 0, &[5, 0]), Err(Error::Rollover));
    assert!(!key(&map.decode(&mut state, 0, &[0, 0]).unwrap().held, 4));
    assert!(key(&map.decode(&mut state, 0, &[1, 0]).unwrap().held, 4));
    assert!(key(&map.decode(&mut state, 0, &[0, 0]).unwrap().held, 4));
    assert!(!key(&map.decode(&mut state, 0, &[3, 0]).unwrap().held, 4));
}

#[test]
fn indicator_ranges_arrays_and_unrelated_values_are_preserved() {
    let mut descriptor = KEYBOARD.to_vec();
    let end = descriptor.pop().unwrap();
    assert_eq!(end, 0xc0);
    descriptor.extend_from_slice(&[
        5, 8, 9, 2, 0x15, 0, 0x25, 100, 0x75, 8, 0x95, 1, 0x91, 2, 0x06, 0, 0xff, 9, 1, 0x91, 2,
        0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    let mut bytes = [0; 3];
    assert_eq!(
        encode_indicators(&map, 0, 2, None, None, false, &mut bytes),
        Err(IndicatorError::ReadRequired)
    );
    assert_eq!(
        encode_indicators(&map, 0, 2, None, Some(&[0xff, 7, 42]), false, &mut bytes),
        Ok(Some(3))
    );
    assert_eq!(bytes, [0xe2, 1, 42]);
    let mut descriptor = KEYBOARD.to_vec();
    descriptor.pop();
    descriptor.extend_from_slice(&[
        5, 8, 0x19, 0, 0x29, 5, 0x15, 0, 0x25, 5, 0x75, 8, 0x95, 3, 0x91, 0, 0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    let mut bytes = [0; 4];
    assert_eq!(
        encode_indicators(&map, 0, 0x15, None, None, false, &mut bytes),
        Ok(Some(4))
    );
    assert_eq!(bytes, [0x15, 1, 3, 5]);
    assert_eq!(
        encode_indicators(&map, 0, 31, None, None, false, &mut bytes),
        Err(IndicatorError::ArrayCapacity)
    );
}

#[test]
fn mixed_volatile_and_relative_output_values_use_neutral_encodings() {
    let mut descriptor = KEYBOARD.to_vec();
    descriptor.pop();
    descriptor.extend_from_slice(&[
        0x06, 0, 0xff, 9, 1, 0x15, 0, 0x25, 100, 0x75, 8, 0x95, 1, 0x91, 0xc2, 9, 2, 0x15, 0xff,
        0x25, 1, 0x91, 6, 0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    let mut bytes = [0; 3];
    assert_eq!(
        encode_indicators(&map, 0, 2, None, None, false, &mut bytes),
        Ok(Some(3))
    );
    assert_eq!(bytes, [2, 101, 0]);
}

#[test]
fn absolute_values_preserve_zero_and_resume_the_latest_state() {
    let map = Map::compile(&consumer_field(0xe0, 8, 0, 100, 2)).unwrap();
    let mut state = map.state().unwrap();
    let axis = CONSUMER_AXES.iter().position(|&u| u == 0xe0).unwrap();
    let mut forward = Forwarder::default();
    drain(&mut forward);
    forward
        .input(0, map.decode(&mut state, 0, &[0]).unwrap())
        .unwrap();
    let packets = drain(&mut forward);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].id, REPORT_CONSUMER_VALUES);
    assert_eq!(&packets[0].bytes()[2 * axis..2 * axis + 2], &[0, 0]);
    forward.enable(false);
    let mut latest = map.decode(&mut state, 0, &[100]).unwrap();
    latest.position = [Some(1_000), Some(2_000), None];
    latest.motion = [99; 4];
    forward.input(0, latest).unwrap();
    forward.enable(true);
    let packets = drain(&mut forward);
    let absolute = packets
        .iter()
        .find(|p| p.id == REPORT_CONSUMER_VALUES)
        .unwrap();
    assert_eq!(
        &absolute.bytes()[2 * axis..2 * axis + 2],
        &65_534u16.to_le_bytes()
    );
    let pointer = packets
        .iter()
        .find(|p| p.id == REPORT_POINTER_POSITION)
        .unwrap();
    assert_eq!(pointer.bytes(), &[0xe8, 3, 0xd0, 7, 0xff, 0xff]);
    assert!(
        packets
            .iter()
            .filter(|p| p.id == REPORT_MOUSE)
            .all(|p| p.bytes()[2..].iter().all(|&v| v == 0))
    );
    forward.remove(0);
    forward.resync();
    assert!(
        drain(&mut forward)
            .iter()
            .all(|p| ![REPORT_POINTER_POSITION, REPORT_CONSUMER_VALUES].contains(&p.id))
    );
}

#[test]
fn removing_or_suspending_a_source_rearms_relative_switch_reports() {
    for suspend in [false, true] {
        let mut forward = Forwarder::default();
        drain(&mut forward);
        let mut input = Input::default();
        input.consumer_switches[0] = 1;
        forward.input(0, input).unwrap();
        assert_eq!(forward.packet().unwrap().id, REPORT_CONSUMER_TOGGLE);
        if suspend {
            forward.enable(false);
            forward.enable(true);
        } else {
            forward.remove(0);
            forward.complete();
        }
        forward.input(1, input).unwrap();
        let packets: Vec<_> = drain(&mut forward)
            .into_iter()
            .filter(|p| p.id == REPORT_CONSUMER_TOGGLE)
            .collect();
        assert_eq!(packets.len(), 3);
        assert_eq!(packets[0].bytes(), &[0; 5]);
        assert_eq!(packets[1].bytes(), &[1, 0, 0, 0, 0]);
        assert_eq!(packets[2].bytes(), &[0; 5]);
    }
}

#[test]
fn null_samples_keep_the_previous_valid_edge_state() {
    let map = Map::compile(&consumer_field(0xe2, 2, 0, 1, 0x46)).unwrap();
    let mut state = map.state().unwrap();
    let index = CONSUMER_SWITCHES.iter().position(|&u| u == 0xe2).unwrap();
    for (raw, expected) in [(0, 0), (2, 0), (1, 1), (2, 0), (1, 0), (0, 0), (1, 1)] {
        assert_eq!(
            map.decode(&mut state, 0, &[raw]).unwrap().consumer_switches[index],
            expected
        );
    }
}

#[test]
fn selector_empty_and_volatile_unchanged_values_do_not_require_null_state() {
    let mut descriptor = KEYBOARD.to_vec();
    descriptor.pop();
    descriptor.extend_from_slice(&[
        5, 8, 0x19, 1, 0x29, 5, 0x15, 1, 0x25, 5, 0x75, 8, 0x95, 5, 0x91, 0, 0x06, 0, 0xff, 9, 1,
        0x15, 0, 0x25, 10, 0x95, 1, 0x91, 0x82, 0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    let mut bytes = [0xff; 7];
    assert_eq!(
        encode_indicators(&map, 0, 0, None, None, false, &mut bytes),
        Ok(Some(7))
    );
    assert_eq!(bytes, [0, 0, 0, 0, 0, 0, 11]);
}

fn encode_indicators(
    map: &Map,
    report: usize,
    leds: u8,
    confirmed: Option<u8>,
    baseline: Option<&[u8]>,
    rearm: bool,
    payload: &mut [u8],
) -> Result<Option<usize>, IndicatorError> {
    map.indicator_report(
        report,
        leds,
        cordial_core::hid::IndicatorValue {
            bits: confirmed.unwrap_or(0),
            known: confirmed.map_or(0, |_| 31),
        },
        baseline,
        rearm,
        payload,
    )
    .map(|r| r.map(|r| r.length))
    .map_err(|e| e.reason)
}

#[test]
fn null_samples_retain_absolute_and_relative_key_holds() {
    for (minimum, flags, release) in [(0, 0x42, 0), (-1, 0x46, 3)] {
        let descriptor = [
            5,
            1,
            9,
            6,
            0xa1,
            1,
            5,
            7,
            9,
            4,
            0x15,
            minimum as u8,
            0x25,
            1,
            0x75,
            2,
            0x95,
            1,
            0x81,
            flags,
            0xc0,
        ];
        let map = Map::compile(&descriptor).unwrap();
        let mut state = map.state().unwrap();
        for (raw, held) in [
            (1, true),
            (2, true),
            (1, true),
            (release, false),
            (2, false),
        ] {
            assert_eq!(
                key(&map.decode(&mut state, 0, &[raw]).unwrap().held, 4),
                held
            );
        }
    }
}

#[test]
fn padded_signed_values_and_indicator_bits_use_the_declared_field_width() {
    let descriptor = consumer_field(0xe0, 64, -1, 1, 6);
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    let axis = CONSUMER_AXES.iter().position(|&u| u == 0xe0).unwrap();
    assert_eq!(
        map.decode(&mut state, 0, &(-1i64).to_le_bytes())
            .unwrap()
            .consumer_motion[axis],
        -1
    );
    let mut descriptor = KEYBOARD.to_vec();
    descriptor.pop();
    descriptor.extend_from_slice(&[
        5, 8, 9, 2, 0x15, 0xff, 0x25, 1, 0x75, 64, 0x95, 1, 0x91, 6, 0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    let mut bytes = [0; 9];
    encode_indicators(&map, 0, 0, None, None, false, &mut bytes).unwrap();
    assert_eq!(bytes, [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
}

#[test]
fn one_shot_represses_a_usage_held_by_another_source() {
    let mut forward = Forwarder::default();
    drain(&mut forward);
    let mut held = Input::default();
    held.held.consumer(0xcd).unwrap();
    held.held.consumer(0xe9).unwrap();
    forward.input(1, held).unwrap();
    drain(&mut forward);
    let mut pulse = Input::default();
    pulse.pulses.consumer(0xcd).unwrap();
    forward.input(0, pulse).unwrap();
    let release = forward.packet().unwrap().clone();
    assert_eq!(release.id, REPORT_CONSUMER);
    assert_eq!(&release.bytes()[..4], &[0xe9, 0, 0, 0]);
    assert_eq!(forward.packet().unwrap().bytes(), release.bytes());
    forward.complete();
    let packets = drain(&mut forward);
    assert_eq!(packets.len(), 1);
    assert_eq!(&packets[0].bytes()[..4], &[0xcd, 0, 0xe9, 0]);
    forward.remove(0);
    assert!(drain(&mut forward).is_empty());
    forward.remove(1);
    assert_eq!(drain(&mut forward)[0].bytes(), &[0; 16]);
}

#[test]
fn custom_input_bytes_leave_standard_keys_available_with_a_field_diagnostic() {
    let mut descriptor = KEYBOARD.to_vec();
    descriptor.pop();
    descriptor.extend_from_slice(&[
        5, 7, 9, 4, 0x15, 0, 0x25, 1, 0x75, 8, 0x95, 1, 0x82, 2, 1, 0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    assert_eq!(map.limitations().len(), 1);
    assert_eq!(
        map.limitations()[0].code,
        cordial_core::model::errors::WarningCode::BufferedInputUnsupported
    );
    assert_eq!(map.limitations()[0].usage_page, 7);
    let mut bytes = [0; 9];
    bytes[2] = 5;
    assert!(key(
        &map.decode(&mut map.state().unwrap(), 0, &bytes)
            .unwrap()
            .held,
        5
    ));
}

#[test]
fn an_unknown_toggle_does_not_block_independent_known_lights() {
    let mut descriptor = KEYBOARD.to_vec();
    descriptor.pop();
    descriptor.extend_from_slice(&[
        5, 8, 9, 2, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x91, 6, 9, 3, 0x91, 6, 0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    let mut bytes = [0; 2];
    let encoded = map
        .indicator_report(
            0,
            6,
            IndicatorValue { bits: 0, known: 2 },
            None,
            false,
            &mut bytes,
        )
        .unwrap()
        .unwrap();
    assert_eq!(bytes, [6, 1]);
    assert_eq!(encoded.unknown, 4);
    assert_eq!(
        map.indicator_locations(0, encoded.unknown)
            .collect::<Vec<_>>(),
        vec![(2, 0x80003), (9, 0x80003)]
    );
}

#[test]
fn the_last_history_bit_is_distinct_from_a_field_without_history() {
    let mut descriptor = vec![5, 1, 9, 6, 0xa1, 1, 5, 7, 9, 4, 0x15, 0, 0x25, 1, 0x75, 1];
    for report in 1..=16 {
        descriptor.extend_from_slice(&[
            0x85, report, 0x96, 0xff, 0x0f, 9, 4, 0x81, 6, 0x95, 1, 9, 4, 0x81, 6,
        ]);
    }
    descriptor.push(0xc0);
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    let mut bytes = [0; REPORT_BYTES];
    bytes[511] = 0x80;
    assert!(key(&map.decode(&mut state, 16, &bytes).unwrap().pulses, 4));
    assert_eq!(
        map.decode(&mut state, 16, &bytes).unwrap().pulses,
        Held::default()
    );
    bytes[511] = 0;
    map.decode(&mut state, 16, &bytes).unwrap();
    bytes[511] = 0x80;
    assert!(key(&map.decode(&mut state, 16, &bytes).unwrap().pulses, 4));
}

#[test]
fn input_array_high_flags_do_not_change_selector_semantics() {
    let descriptor = [
        5, 1, 9, 6, 0xa1, 1, 5, 7, 0x19, 0, 0x29, 0x65, 0x15, 0, 0x25, 0x65, 0x75, 8, 0x95, 1,
        0x82, 0, 1, 0xc0,
    ];
    let map = Map::compile(&descriptor).unwrap();
    assert!(map.limitations().is_empty());
    assert!(key(
        &map.decode(&mut map.state().unwrap(), 0, &[4]).unwrap().held,
        4
    ));
}

#[test]
fn relative_selector_items_emit_independent_pulses_and_rearm_when_cleared() {
    // Two independent selector fields include the same key. An ordinary held
    // key and another selector's previous sample do not suppress a new pulse.
    let descriptor = [
        5, 1, 9, 6, 0xa1, 1, 5, 7, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 9, 4, 0x81, 2, 0x75, 7,
        0x81, 1, 0x75, 8, 9, 0, 9, 4, 0x81, 4, 9, 0, 9, 4, 0x81, 4, 0xc0,
    ];
    let map = Map::compile(&descriptor).unwrap();
    assert!(map.limitations().is_empty());
    let mut state = map.state().unwrap();
    let first = map.decode(&mut state, 0, &[1, 1, 0]).unwrap();
    assert!(key(&first.held, 4) && key(&first.pulses, 4));
    assert!(!key(
        &map.decode(&mut state, 0, &[1, 1, 0]).unwrap().pulses,
        4
    ));
    assert!(key(
        &map.decode(&mut state, 0, &[1, 1, 1]).unwrap().pulses,
        4
    ));
    assert!(!key(
        &map.decode(&mut state, 0, &[1, 0, 1]).unwrap().pulses,
        4
    ));
    assert!(key(
        &map.decode(&mut state, 0, &[1, 1, 1]).unwrap().pulses,
        4
    ));
    state.clear();
    assert!(key(
        &map.decode(&mut state, 0, &[0, 1, 0]).unwrap().pulses,
        4
    ));
}

#[test]
fn unsupported_numeric_array_selectors_do_not_fill_held_control_history() {
    let mut descriptor = vec![
        5, 12, 9, 1, 0xa1, 1, 0x15, 0, 0x25, 10, 0x75, 8, 0x95, 10, 9, 0,
    ];
    for usage in cordial_core::hid::CONSUMER_AXES.into_iter().take(9) {
        descriptor.extend_from_slice(&[0x0a, usage as u8, (usage >> 8) as u8]);
    }
    descriptor.extend_from_slice(&[9, 0xcd, 0x81, 4, 0xc0]);
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    let payload = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    assert_eq!(
        map.decode(&mut state, 0, &payload)
            .unwrap()
            .pulses
            .consumers[0],
        0xcd
    );
    assert_eq!(
        map.decode(&mut state, 0, &payload).unwrap().pulses,
        Held::default()
    );
}

#[test]
fn absolute_numeric_selectors_are_diagnosed_without_losing_ordinary_controls() {
    let bytes = [
        5, 12, 9, 1, 0xa1, 1, 9, 0, 9, 0xe0, 9, 0xcd, 0x15, 0, 0x25, 2, 0x75, 8, 0x95, 2, 0x81, 0,
        0xc0,
    ];
    let map = Map::compile(&bytes).unwrap();
    assert_eq!(
        map.limitations()[0].code,
        cordial_core::model::errors::WarningCode::NumericSelectorUnsupported
    );
    assert_eq!(map.limitations()[0].usage, 0xe0);
    let input = map.decode(&mut map.state().unwrap(), 0, &[1, 2]).unwrap();
    assert_eq!(input.held.consumers[0], 0xcd);
}

#[test]
fn contact_controls_forward_as_held_keys_and_standard_relative_switches() {
    for usage in [0x500, 0x501, 0x502, 0x514] {
        let map = Map::compile(&consumer_field(usage, 1, 0, 1, 2)).unwrap();
        assert_eq!(
            map.decode(&mut map.state().unwrap(), 0, &[1])
                .unwrap()
                .held
                .consumers[0],
            usage
        );
    }
    for usage in [0x500, 0x501, 0x502] {
        for (minimum, size, raw) in [(0, 1, 1), (-1, 2, 3)] {
            let map = Map::compile(&consumer_field(usage, size, minimum, 1, 6)).unwrap();
            let index = CONSUMER_SWITCHES.iter().position(|&u| u == usage).unwrap();
            let mut forward = Forwarder::default();
            drain(&mut forward);
            forward
                .input(0, map.decode(&mut map.state().unwrap(), 0, &[raw]).unwrap())
                .unwrap();
            let packets = drain(&mut forward);
            assert_eq!(packets.len(), 2);
            let bits = if minimum == 0 { 1 } else { 2 };
            assert_eq!(
                packets[0].id,
                if minimum == 0 {
                    REPORT_CONSUMER_TOGGLE
                } else {
                    REPORT_CONSUMER_ON_OFF
                }
            );
            assert_eq!(
                (packets[0].bytes()[index * bits / 8] >> (index * bits % 8)) & ((1 << bits) - 1),
                raw
            );
            assert_eq!(
                packets[0].bytes().len(),
                (CONSUMER_SWITCHES.len() * bits).div_ceil(8)
            );
        }
    }
}

#[test]
fn repeated_on_off_controls_compose_in_field_order() {
    let mute = CONSUMER_SWITCHES.iter().position(|&u| u == 0xe2).unwrap();
    for (signed_first, expected) in [(true, -1), (false, 1)] {
        let mut bytes = vec![5, 12, 9, 1, 0xa1, 1, 0x75, 2, 0x95, 1];
        for signed in [signed_first, !signed_first] {
            bytes.extend_from_slice(&[
                9,
                0xe2,
                0x15,
                if signed { 0xff } else { 0 },
                0x25,
                1,
                0x81,
                6,
            ]);
        }
        bytes.push(0xc0);
        let map = Map::compile(&bytes).unwrap();
        let input = map.decode(&mut map.state().unwrap(), 0, &[5]).unwrap();
        assert_eq!(input.consumer_switches[mute], expected);
        assert_ne!(input.explicit_switches & (1 << mute), 0);
    }
    let bytes = [
        5, 12, 9, 1, 0xa1, 1, 0x75, 1, 0x95, 2, 9, 0xe2, 9, 0xe2, 0x15, 0, 0x25, 1, 0x81, 6, 0xc0,
    ];
    let map = Map::compile(&bytes).unwrap();
    assert_eq!(
        map.decode(&mut map.state().unwrap(), 0, &[3])
            .unwrap()
            .consumer_switches[mute],
        0
    );
    let bytes = [
        5, 12, 9, 1, 0xa1, 1, 0x75, 8, 0x95, 2, 9, 0, 9, 0xe2, 0x15, 0, 0x25, 1, 0x81, 4, 0xc0,
    ];
    let map = Map::compile(&bytes).unwrap();
    assert_eq!(
        map.decode(&mut map.state().unwrap(), 0, &[1, 1])
            .unwrap()
            .consumer_switches[mute],
        1
    );
}

#[test]
fn repeated_ordered_usage_sequences_share_storage_and_keep_selector_order() {
    let mut bytes = vec![
        5, 1, 9, 6, 0xa1, 1, 5, 7, 0x75, 8, 0x95, 1, 0x15, 0, 0x25, 2,
    ];
    for field in 0..96 {
        bytes.extend_from_slice(&[
            9,
            0,
            9,
            if field % 2 == 0 { 5 } else { 4 },
            9,
            if field % 2 == 0 { 4 } else { 5 },
            0x81,
            0,
        ]);
    }
    bytes.push(0xc0);
    let map = Map::compile(&bytes).unwrap();
    let mut payload = [0; 96];
    payload[0] = 1;
    let input = map.decode(&mut map.state().unwrap(), 0, &payload).unwrap();
    assert!(key(&input.held, 5) && !key(&input.held, 4));
    payload[0] = 0;
    payload[1] = 1;
    let input = map.decode(&mut map.state().unwrap(), 0, &payload).unwrap();
    assert!(key(&input.held, 4) && !key(&input.held, 5));
}

#[test]
fn repeated_one_shot_fields_forward_each_edge_with_bounded_queue_admission() {
    let mut bytes = vec![5, 12, 9, 1, 0xa1, 1, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1];
    for _ in 0..3 {
        bytes.extend_from_slice(&[9, 0xb5, 0x81, 6]);
    }
    bytes.push(0xc0);
    let map = Map::compile(&bytes).unwrap();
    let mut state = map.state().unwrap();
    let input = map.decode(&mut state, 0, &[7]).unwrap();
    let mut forward = Forwarder::default();
    drain(&mut forward);
    forward.input(0, input).unwrap();
    let packets = drain(&mut forward);
    assert_eq!(packets.len(), 6);
    for pair in packets.as_chunks::<2>().0 {
        assert_eq!(pair[0].id, REPORT_CONSUMER);
        assert_eq!(&pair[0].bytes()[..2], &0xb5u16.to_le_bytes());
        assert_eq!(pair[1].bytes(), &[0; 16]);
    }
    assert_eq!(
        map.decode(&mut state, 0, &[7])
            .unwrap()
            .pulse_repetition_count,
        0
    );
    for n in 0..QUEUE - 1 {
        forward
            .input(
                1 + n % 2,
                Input {
                    motion: [1, 0, 0, 0],
                    ..Input::default()
                },
            )
            .unwrap();
    }
    assert_eq!(
        forward.input(0, input),
        Err(cordial_core::hid::Error::Overflow)
    );
}

#[test]
fn a_consumer_range_spanning_a_numeric_control_is_not_diagnosed() {
    // A keyboard's Consumer array covering usages 1 through 0x29c, which includes AC Pan.
    let bytes = [
        5, 12, 9, 1, 0xa1, 1, 0x15, 1, 0x26, 0x9c, 2, 0x19, 1, 0x2a, 0x9c, 2, 0x75, 16, 0x95, 1,
        0x81, 0, 0xc0,
    ];
    let map = Map::compile(&bytes).unwrap();
    assert!(map.limitations().is_empty());
    let input = map
        .decode(&mut map.state().unwrap(), 0, &[0xcd, 0])
        .unwrap();
    assert_eq!(input.held.consumers[0], 0xcd);
}

#[test]
fn system_and_radio_inputs_preserve_simultaneous_state_and_releases() {
    use cordial_core::forward::{REPORT_RADIO, REPORT_SYSTEM};
    let descriptor = [
        5, 1, 9, 0x80, 0xa1, 1, 0x85, 1, 0x19, 0x81, 0x29, 0x83, 0x15, 0, 0x25, 1, 0x75, 1, 0x95,
        3, 0x81, 2, 0x75, 5, 0x95, 1, 0x81, 3, 0xc0, 5, 1, 9, 0x0c, 0xa1, 1, 0x85, 2, 9, 0xc6,
        0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2, 9, 0xc8, 0x81, 2, 0x75, 6, 0x81, 3, 0xc0,
    ];
    let packets = |f: &mut Forwarder| {
        drain(f)
            .into_iter()
            .map(|p| (p.id, p.bytes().to_vec()))
            .collect::<Vec<_>>()
    };
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    let mut forward = Forwarder::default();
    drain(&mut forward);
    forward
        .input(0, map.decode(&mut state, 1, &[3]).unwrap())
        .unwrap();
    let output = packets(&mut forward);
    assert_eq!(
        output,
        vec![(REPORT_SYSTEM, vec![3, 0, 0, 0, 0x20, 0, 0, 0])]
    );
    forward
        .input(0, map.decode(&mut state, 2, &[3]).unwrap())
        .unwrap();
    assert_eq!(packets(&mut forward), vec![(REPORT_RADIO, vec![3])]);
    forward
        .input(0, map.decode(&mut state, 2, &[0]).unwrap())
        .unwrap();
    assert_eq!(packets(&mut forward), vec![(REPORT_RADIO, vec![0])]);
    forward.remove(0);
    let output = packets(&mut forward);
    assert!(output.contains(&(REPORT_SYSTEM, vec![0, 0, 0, 0, 0x20, 0, 0, 0])));
    assert!(output.contains(&(REPORT_RADIO, vec![4])));
}

#[test]
fn native_usage_support_requires_a_reachable_field_value() {
    let variable = Map::compile(&[
        5, 12, 9, 1, 0xa1, 1, 0x19, 0xb5, 0x29, 0xb7, 0x15, 0, 0x25, 1, 0x75, 8, 0x95, 1, 0x81, 2,
        0xc0,
    ])
    .unwrap();
    assert!(variable.supports_usage(0xc00b5));
    assert!(!variable.supports_usage(0xc00b6));
    let array = Map::compile(&[
        5, 12, 9, 1, 0xa1, 1, 0x19, 0, 0x2a, 0xff, 3, 0x15, 0, 0x25, 0x7f, 0x75, 8, 0x95, 1, 0x81,
        0, 0xc0,
    ])
    .unwrap();
    assert!(array.supports_usage(0xc0070));
    assert!(!array.supports_usage(0xc00b5));
}

#[test]
fn sliders_preserve_unknown_state_and_toggle_press_edges() {
    use cordial_core::hid::{ROTATION_KNOWN, ROTATION_STATE};
    // Two nullable absolute sliders. A first null report does not mean Off.
    let map = Map::compile(&[
        5, 1, 9, 0x80, 0xa1, 1, 9, 0xca, 9, 0xc8, 0x15, 0, 0x25, 1, 0x75, 2, 0x95, 2, 0x81, 0x42,
        0xc0,
    ])
    .unwrap();
    let mut state = map.state().unwrap();
    assert_eq!(
        map.decode(&mut state, 0, &[0x0a]).unwrap().held,
        Held::default()
    );
    let on = map.decode(&mut state, 0, &[5]).unwrap().held;
    assert_eq!(on.system, ROTATION_KNOWN | ROTATION_STATE);
    assert_eq!(on.radio, 6);
    assert_eq!(map.decode(&mut state, 0, &[0x0a]).unwrap().held, on);
    let off = map.decode(&mut state, 0, &[0]).unwrap().held;
    assert_eq!(off.system, ROTATION_KNOWN);
    assert_eq!(off.radio, 4);
    let map = Map::compile(&[
        5, 1, 9, 0x0c, 0xa1, 1, 9, 0xc8, 0x15, 0, 0x25, 1, 0x75, 8, 0x95, 1, 0x81, 6, 0xc0,
    ])
    .unwrap();
    let mut state = map.state().unwrap();
    assert_eq!(map.decode(&mut state, 0, &[1]).unwrap().pulses.radio, 1);
    assert_eq!(map.decode(&mut state, 0, &[1]).unwrap().pulses.radio, 0);
    assert_eq!(map.decode(&mut state, 0, &[0]).unwrap().held.radio, 0);
    assert_eq!(map.decode(&mut state, 0, &[1]).unwrap().pulses.radio, 1);
}

#[test]
fn slider_updates_use_the_latest_source_and_report() {
    use cordial_core::forward::{REPORT_RADIO, REPORT_SYSTEM};
    let mut forward = Forwarder::default();
    drain(&mut forward);
    let update = |value| Input {
        sliders: [Some(value); 2],
        ..Input::default()
    };
    forward.input(0, update(true)).unwrap();
    drain(&mut forward);
    forward.input(1, update(true)).unwrap();
    drain(&mut forward);
    forward.input(1, update(false)).unwrap();
    let packets = drain(&mut forward);
    assert!(
        packets
            .iter()
            .any(|p| p.id == REPORT_RADIO && p.bytes() == [0])
    );
    assert!(
        packets
            .iter()
            .any(|p| p.id == REPORT_SYSTEM && p.bytes() == [0; 8])
    );
    forward.remove(1);
    let packets = drain(&mut forward);
    assert!(
        packets
            .iter()
            .any(|p| p.id == REPORT_RADIO && p.bytes() == [4])
    );
    assert!(
        packets
            .iter()
            .any(|p| p.id == REPORT_SYSTEM && p.bytes()[4] == 0x20)
    );
    // Removing the latest source must not restore another source's stale On state.
    assert!(drain(&mut forward).is_empty());

    let map = Map::compile(&[
        5, 1, 9, 0x0c, 0xa1, 1, 0x85, 1, 9, 0xc8, 0x15, 0, 0x25, 1, 0x75, 8, 0x95, 1, 0x81, 2,
        0x85, 2, 9, 0xc8, 0x81, 2, 0xc0,
    ])
    .unwrap();
    let mut state = map.state().unwrap();
    forward
        .input(0, map.decode(&mut state, 1, &[1]).unwrap())
        .unwrap();
    drain(&mut forward);
    forward
        .input(0, map.decode(&mut state, 2, &[1]).unwrap())
        .unwrap();
    drain(&mut forward);
    forward
        .input(0, map.decode(&mut state, 2, &[0]).unwrap())
        .unwrap();
    assert!(
        drain(&mut forward)
            .iter()
            .any(|p| p.id == REPORT_RADIO && p.bytes() == [0])
    );
}
