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
    assert!(matches!(Map::compile(&absolute), Err(Error::Unsupported)));
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
    assert_eq!(drain(&mut forward).len(), 3);
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
    assert_eq!(drain(&mut forward).len(), 3);
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
