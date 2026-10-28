use cordial_core::{
    forward::Forwarder,
    hid::{self, Error, Gamepad, Held, Input, Map},
    link::{Link, LinkId, Profile, ServiceId},
    model::{errors::ErrorCode, identifiers::HostPlatform},
    settings::Catalog,
};

// Standard gamepad with six unsigned axes, 16 buttons, and an eight-way nullable hat.
const GAMEPAD: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x19, 0x30, 0x29, 0x35, 0x15, 0x00,
    0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x06, 0x81, 0x02, 0xc0, 0x05, 0x09, 0x19, 0x01, 0x29, 0x10,
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x10, 0x81, 0x02, 0x05, 0x01, 0x09, 0x39, 0x15, 0x01,
    0x25, 0x08, 0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x75,
    0x04, 0x81, 0x03, 0xc0,
];
const SPLIT: &[u8] = &[
    5, 1, 9, 4, 0xa1, 1, 0x85, 1, 5, 9, 0x19, 1, 0x29, 8, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8, 0x81,
    2, 5, 1, 9, 0x30, 0x15, 0x81, 0x25, 0x7f, 0x75, 8, 0x95, 1, 0x81, 2, 0x85, 2, 5, 9, 0x19, 9,
    0x29, 16, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8, 0x81, 2, 5, 1, 9, 0x31, 0x15, 0x81, 0x25, 0x7f,
    0x75, 8, 0x95, 1, 0x81, 2, 0xc0,
];

#[test]
fn standard_gamepad_and_joystick_keep_usage_identity_and_leave_forwarding_empty() {
    for application in [4, 5] {
        let mut descriptor = GAMEPAD.to_vec();
        descriptor[3] = application;
        let map = Map::compile(&descriptor).unwrap();
        assert_eq!(map.roles, hid::GAMEPAD);
        assert_eq!(
            cordial_core::wire::roles(map.roles),
            [cordial_protocol::Role::Gamepad as i32]
        );
        assert_eq!(hid::GAMEPAD, 32);
        assert_eq!(hid::RADIO, 16);
        assert!(map.supports_usage(0x90010));
        assert!(map.supports_usage(0x10035));
        assert!(!map.supports_usage(0x70004));
        let mut state = map.state().unwrap();
        assert_eq!(map.gamepad(&state), released(map.gamepad(&state)));
        let report = [0, 255, 128, 64, 192, 127, 0x81, 0x80, 3];
        assert_eq!(map.decode(&mut state, 0, &report), Ok(Input::default()));
        let pad = map.gamepad(&state);
        assert_eq!(pad.buttons, 0x8081);
        assert_eq!(pad.axes, [0, 65535, 32896, 16448, 49344, 32639, 0, 0]);
        assert_eq!(pad.axes_valid, 0x3f);
        assert_eq!(pad.hat, Some(2));
        assert!(pad.hat_valid);
        let mut forward = Forwarder::default();
        while forward.packet().is_some() {
            forward.complete();
        }
        forward
            .input(0, map.decode(&mut state, 0, &report).unwrap())
            .unwrap();
        assert!(forward.packet().is_none());
        map.decode(&mut state, 0, &[0; 9]).unwrap();
        assert_eq!(map.gamepad(&state).buttons, 0);
        assert_eq!(map.gamepad(&state).hat, None);
        state.clear();
        assert_eq!(map.gamepad(&state), released(map.gamepad(&state)));
    }
}

fn axis_descriptor(minimum: i32, maximum: u32, size: u8, flags: u8) -> Vec<u8> {
    let mut descriptor = vec![5, 1, 9, 5, 0xa1, 1, 9, 0x30, 0x17];
    descriptor.extend_from_slice(&minimum.to_le_bytes());
    descriptor.push(0x27);
    descriptor.extend_from_slice(&maximum.to_le_bytes());
    descriptor.extend_from_slice(&[0x75, size, 0x95, 1, 0x81, flags, 0xc0]);
    descriptor
}

#[test]
fn axes_scale_signed_unsigned_and_nonzero_ranges_without_assuming_trigger_presence() {
    for (minimum, maximum, size, samples) in [
        (-127, 127, 8, [-127i64, 0, 127]),
        (-32768, 32767, 16, [-32768, 0, 32767]),
        (10, 110, 8, [10, 60, 110]),
        (-1000, (-100i32) as u32, 16, [-1000, -550, -100]),
        (0, u32::MAX, 32, [0, 2147483648, 4294967295]),
        (
            i32::MIN,
            i32::MAX as u32,
            32,
            [i64::from(i32::MIN), 0, i64::from(i32::MAX)],
        ),
        (-127, 127, 64, [-127, 0, 127]),
    ] {
        let map = Map::compile(&axis_descriptor(minimum, maximum, size, 2)).unwrap();
        let mut state = map.state().unwrap();
        for (raw, expected) in samples.into_iter().zip([0, 32768, 65535]) {
            map.decode(&mut state, 0, &raw.to_le_bytes()[..usize::from(size / 8)])
                .unwrap();
            let pad = map.gamepad(&state);
            assert_eq!(pad.axes[0], expected, "{minimum}..{maximum}: {raw}");
            assert_eq!(pad.axes_valid, 1);
            assert_eq!(pad.axes[1..], [0; 7]);
        }
    }
    let map = Map::compile(&axis_descriptor(10, 110, 8, 0x42)).unwrap();
    let mut state = map.state().unwrap();
    map.decode(&mut state, 0, &[60]).unwrap();
    assert_eq!(map.gamepad(&state).axes_valid, 1);
    map.decode(&mut state, 0, &[0]).unwrap();
    assert_eq!(map.gamepad(&state).axes_valid, 0);
    let map = Map::compile(&axis_descriptor(10, 110, 8, 2)).unwrap();
    let mut state = map.state().unwrap();
    for (raw, expected) in [(0, 0), (255, 65535)] {
        map.decode(&mut state, 0, &[raw]).unwrap();
        assert_eq!(map.gamepad(&state).axes[0], expected);
    }
}

#[test]
fn hats_center_out_of_range_with_or_without_null_state() {
    for flags in [2, 0x42] {
        for (minimum, maximum, multiplier) in [(0, 7, 1), (1, 8, 1), (0, 3, 2), (1, 4, 2)] {
            let descriptor = [
                5, 1, 9, 5, 0xa1, 1, 9, 0x39, 0x15, minimum, 0x25, maximum, 0x75, 4, 0x95, 1, 0x81,
                flags, 0xc0,
            ];
            let map = Map::compile(&descriptor).unwrap();
            let mut state = map.state().unwrap();
            for raw in 0..=15 {
                map.decode(&mut state, 0, &[minimum]).unwrap();
                map.decode(&mut state, 0, &[raw]).unwrap();
                let expected = (minimum..=maximum)
                    .contains(&raw)
                    .then(|| (raw - minimum) * multiplier);
                assert_eq!(map.gamepad(&state).hat, expected);
                assert!(map.gamepad(&state).hat_valid);
            }
        }
    }
}

#[test]
fn explicit_desktop_controls_and_all_32_numbered_buttons_stay_separate() {
    let descriptor = [
        5, 1, 9, 5, 0xa1, 1, 0x19, 0x90, 0x29, 0x93, 9, 0x3d, 9, 0x3e, 0x15, 0, 0x25, 1, 0x75, 1,
        0x95, 6, 0x81, 2, 0x75, 2, 0x95, 1, 0x81, 3, 5, 9, 0x19, 1, 0x29, 32, 0x75, 1, 0x95, 32,
        0x81, 2, 0xc0,
    ];
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    assert_eq!(
        map.decode(&mut state, 0, &[0x3f, 0xff, 0xff, 0xff, 0xff])
            .unwrap()
            .held,
        Held::default()
    );
    let pad = map.gamepad(&state);
    assert_eq!(pad.buttons, u32::MAX);
    assert_eq!(pad.dpad, 15);
    assert!(pad.start && pad.select);
    map.decode(&mut state, 0, &[0; 5]).unwrap();
    assert_eq!(map.gamepad(&state), released(map.gamepad(&state)));
}

#[test]
fn absolute_button_selectors_preserve_usage_offsets_and_simultaneous_presses() {
    let descriptor = [
        5, 1, 9, 4, 0xa1, 1, 5, 9, 0x19, 0, 0x29, 32, 0x15, 10, 0x25, 42, 0x75, 8, 0x95, 4, 0x81,
        0, 0xc0,
    ];
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    map.decode(&mut state, 0, &[11, 11, 42, 255]).unwrap();
    assert_eq!(map.gamepad(&state).buttons, 0x80000001);
    map.decode(&mut state, 0, &[10, 0, 255, 10]).unwrap();
    assert_eq!(map.gamepad(&state).buttons, 0);
}

#[test]
fn relative_and_vendor_controls_never_enter_ordinary_forwarding() {
    let mut descriptor = GAMEPAD.to_vec();
    descriptor.pop();
    descriptor.extend_from_slice(&[
        5, 1, 9, 0x30, 0x15, 0x81, 0x25, 0x7f, 0x75, 8, 0x95, 1, 0x81, 6, 5, 7, 9, 4, 0x15, 0,
        0x25, 1, 0x81, 2, 5, 12, 9, 0xe9, 0x81, 2, 0x06, 0, 0xff, 9, 1, 0x81, 2, 0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    assert_eq!(map.roles, hid::GAMEPAD);
    assert_eq!(map.decode(&mut state, 0, &[255; 13]), Ok(Input::default()));
    assert_eq!(map.gamepad(&state).axes[0], 65535);
    assert!(!map.supports_usage(0x70004));
    assert!(map.supports_usage(0xc00e9));
    assert!(matches!(
        Map::compile(&axis_descriptor(-127, 127, 8, 6)),
        Err(Error::Unsupported)
    ));
}

#[test]
fn split_reports_preserve_other_reports_and_errors_clear_gamepad_state() {
    let map = Map::compile(SPLIT).unwrap();
    let mut state = map.state().unwrap();
    map.decode(&mut state, 1, &[1, 127]).unwrap();
    assert_eq!(map.gamepad(&state).axes_valid, 1);
    map.decode(&mut state, 2, &[2, 0x81]).unwrap();
    assert_eq!(map.gamepad(&state).buttons, 0x201);
    assert_eq!(map.gamepad(&state).axes[..2], [65535, 0]);
    map.decode(&mut state, 1, &[0, 0]).unwrap();
    assert_eq!(map.gamepad(&state).buttons, 0x200);
    assert_eq!(map.gamepad(&state).axes[..2], [32768, 0]);
    assert_eq!(map.decode(&mut state, 2, &[0]), Err(Error::Invalid));
    assert_eq!(map.gamepad(&state), released(map.gamepad(&state)));
    map.decode(&mut state, 2, &[2, 0x81, 0xff]).unwrap();
    assert_eq!(map.gamepad(&state).buttons, 0x200);
    assert_eq!(map.decode(&mut state, 9, &[0; 2]), Err(Error::Invalid));
    assert_eq!(map.gamepad(&state), released(map.gamepad(&state)));
    map.decode(&mut state, 1, &[1, 0]).unwrap();
    assert_eq!(
        map.decode(&mut state, 1, &[0; hid::REPORT_BYTES + 1]),
        Err(Error::Invalid)
    );
    assert_eq!(map.gamepad(&state), released(map.gamepad(&state)));
}

fn link(slot: u8, catalog: &mut Catalog) -> Link {
    Link::new(
        LinkId {
            slot,
            generation: 1,
        },
        vec![
            Profile::compile(ServiceId(1), SPLIT).unwrap(),
            Profile::compile(ServiceId(2), SPLIT).unwrap(),
        ],
        64,
        false,
        HostPlatform::Linux,
        catalog,
    )
    .unwrap()
}

#[test]
fn services_and_links_keep_separate_state_and_disconnect_releases_controls() {
    let mut catalog = Catalog::default();
    let mut other_catalog = Catalog::default();
    let mut link = link(0, &mut catalog);
    let mut other = self::link(1, &mut other_catalog);
    let mut forward = Forwarder::default();
    assert_eq!(link.roles, hid::GAMEPAD);
    link.input(ServiceId(1), 1, &[1, 127], &mut catalog, &mut forward, 0)
        .unwrap();
    link.input(ServiceId(2), 2, &[2, 0x81], &mut catalog, &mut forward, 0)
        .unwrap();
    assert_eq!(link.gamepad().buttons, 0x201);
    assert_eq!(link.gamepad().axes_valid, 3);
    other
        .input(
            ServiceId(1),
            1,
            &[4, 0],
            &mut other_catalog,
            &mut forward,
            0,
        )
        .unwrap();
    assert_eq!(other.gamepad().buttons, 4);
    link.input(ServiceId(1), 1, &[0, 0], &mut catalog, &mut forward, 1)
        .unwrap();
    assert_eq!(link.gamepad().buttons, 0x200);
    assert_eq!(other.gamepad().buttons, 4);
    assert_eq!(
        link.input(ServiceId(1), 1, &[0], &mut catalog, &mut forward, 2),
        Err(ErrorCode::ConnectionFailed)
    );
    assert_eq!(link.gamepad(), released(link.gamepad()));
    assert_eq!(other.gamepad().buttons, 4);
    other.disconnected(&mut other_catalog, &mut forward);
    assert_eq!(other.gamepad(), released(other.gamepad()));
}

fn mixed_descriptor() -> Vec<u8> {
    let mut descriptor = SPLIT.to_vec();
    descriptor.extend_from_slice(&[
        5, 1, 9, 6, 0xa1, 1, 0x85, 3, 5, 7, 0x19, 0, 0x29, 0x65, 0x15, 0, 0x25, 0x65, 0x75, 8,
        0x95, 1, 0x81, 0, 0xc0, 5, 1, 9, 2, 0xa1, 1, 5, 9, 9, 1, 0x15, 0, 0x25, 1, 0x75, 1, 0x95,
        1, 0x81, 2, 0x75, 7, 0x81, 3, 0xc0,
    ]);
    descriptor
}

fn vendor_reports() -> Vec<u8> {
    vec![
        5, 1, 9, 5, 0xa1, 1, 6, 0, 0xff, 9, 1, 0x85, 4, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 2,
        0x81, 2, 0x85, 255, 9, 1, 0x96, 0, 4, 0x81, 2, 0x85, 5, 9, 1, 0x95, 2, 0xb1, 2, 0x85, 6, 9,
        1, 0x91, 2, 0xc0,
    ]
}

fn vendor_input_with_shared_indicator(kind: u8, input_bytes: u16) -> Vec<u8> {
    let second = SPLIT.windows(2).position(|item| item == [0x85, 2]).unwrap();
    let mut descriptor = SPLIT[..second].to_vec();
    descriptor.push(0xc0);
    descriptor.extend_from_slice(&mixed_descriptor()[SPLIT.len()..]);
    descriptor.extend_from_slice(&[
        5, 1, 9, 5, 0xa1, 1, 0x85, 2, 6, 0, 0xff, 9, 1, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x96,
    ]);
    descriptor.extend_from_slice(&input_bytes.to_le_bytes());
    descriptor.extend_from_slice(&[
        0x81, 2, 9, 1, 0x95, 1, kind, 2, 5, 8, 9, 1, 0x25, 1, 0x75, 1, kind, 2, 0x75, 7, kind, 3,
        0xc0,
    ]);
    descriptor
}

#[test]
fn shared_output_and_feature_ids_do_not_retain_vendor_input() {
    use cordial_core::{bluetooth::ReportType, hid::IndicatorValue};

    for (kind, report_type, index) in [
        (0x91, ReportType::Output, 1),
        (0xb1, ReportType::Feature, 2),
    ] {
        let map = Map::compile(&vendor_input_with_shared_indicator(kind, 2)).unwrap();
        let mut state = map.state().unwrap();
        map.decode(&mut state, 1, &[1, 127]).unwrap();
        let held = map.decode(&mut state, 3, &[4, 1]).unwrap().held;
        assert_eq!(held.keys[0], 1 << 4);
        assert_eq!(held.buttons, 1);
        let pad = map.gamepad(&state);
        assert_eq!(map.decode(&mut state, 2, &[255; 2]), Err(Error::Ignored));
        assert_eq!(map.gamepad(&state), pad);
        assert_eq!(map.decode(&mut state, 1, &[1, 127]).unwrap().held, held);

        let ri = map
            .reports()
            .iter()
            .position(|report| report.id == 2)
            .unwrap();
        let mut bits = [0; 3];
        bits[index] = 16;
        assert_eq!(map.reports()[ri].bits, bits);
        assert_eq!(
            map.observed_indicators(2, report_type, &[42, 1]),
            IndicatorValue { bits: 1, known: 1 }
        );
        if report_type == ReportType::Output {
            let mut output = [0; 2];
            assert_eq!(
                map.indicator_report(ri, 1, IndicatorValue::default(), None, false, &mut output)
                    .unwrap_err()
                    .reason,
                hid::IndicatorError::ReadRequired
            );
            let encoding = map
                .indicator_report(
                    ri,
                    1,
                    IndicatorValue::default(),
                    Some(&[42, 0]),
                    false,
                    &mut output,
                )
                .unwrap()
                .unwrap();
            assert_eq!(encoding.length, 2);
            assert_eq!(output, [42, 1]);
        } else {
            assert_eq!(
                map.indicator_feedback().collect::<Vec<_>>(),
                [(2, report_type)]
            );
        }
        assert_eq!(map.decode(&mut state, 1, &[1]), Err(Error::Invalid));
        assert_eq!(map.gamepad(&state), released(pad));
        assert_eq!(map.decode(&mut state, 1, &[0, 0]).unwrap().held, held);
    }
}

#[test]
fn oversized_vendor_input_sharing_output_or_feature_id_is_skipped() {
    for kind in [0x91, 0xb1] {
        let map = Map::compile(&vendor_input_with_shared_indicator(kind, 1024)).unwrap();
        let mut state = map.state().unwrap();
        map.decode(&mut state, 1, &[1, 127]).unwrap();
        let pad = map.gamepad(&state);
        assert_eq!(map.decode(&mut state, 2, &[255; 1024]), Err(Error::Ignored));
        assert_eq!(map.gamepad(&state), pad);
    }
}

#[test]
fn shared_id_vendor_input_never_forwards_link_controls_or_first_input() {
    for kind in [0x91, 0xb1] {
        let mut catalog = Catalog::default();
        let mut link = Link::new(
            LinkId {
                slot: 0,
                generation: 1,
            },
            vec![
                Profile::compile(ServiceId(1), &vendor_input_with_shared_indicator(kind, 2))
                    .unwrap(),
            ],
            64,
            false,
            HostPlatform::Linux,
            &mut catalog,
        )
        .unwrap();
        let mut forward = Forwarder::default();
        for held in [false, true] {
            if held {
                link.input(ServiceId(1), 1, &[1, 127], &mut catalog, &mut forward, 0)
                    .unwrap();
                link.input(ServiceId(1), 3, &[4, 1], &mut catalog, &mut forward, 0)
                    .unwrap();
                assert!(link.take_input_forwarded());
            }
            while forward.packet().is_some() {
                forward.complete();
            }
            let pad = link.gamepad();
            assert_eq!(
                link.input(ServiceId(1), 2, &[255; 2], &mut catalog, &mut forward, 0),
                Ok(false)
            );
            assert_eq!(link.gamepad(), pad);
            assert!(forward.packet().is_none());
            assert!(!link.take_input_forwarded());
            if held {
                link.input(ServiceId(1), 1, &[1, 127], &mut catalog, &mut forward, 0)
                    .unwrap();
                assert!(link.take_input_forwarded());
                assert!(forward.packet().is_none());
            }
        }
    }
}

#[test]
fn omitted_vendor_reports_preserve_map_controls_and_useful_reports_keep_validation() {
    assert!(matches!(
        Map::compile(&vendor_reports()),
        Err(Error::Unsupported)
    ));
    let descriptor = [mixed_descriptor(), vendor_reports()].concat();
    let map = Map::compile(&descriptor).unwrap();
    assert_eq!(map.reports().len(), 3);
    let mut state = map.state().unwrap();
    for invalid_id in [5, 6, 7] {
        map.decode(&mut state, 1, &[1, 127]).unwrap();
        let held = map.decode(&mut state, 3, &[4, 1]).unwrap().held;
        map.decode(&mut state, 2, &[0, 0]).unwrap();
        let pad = map.gamepad(&state);
        for (id, payload) in [(4, &[255; 2][..]), (255, &[255; 1024][..])] {
            assert_eq!(map.decode(&mut state, id, payload), Err(Error::Ignored));
            assert_eq!(map.gamepad(&state), pad);
            assert_eq!(map.decode(&mut state, 2, &[0, 0]).unwrap().held, held);
        }
        assert_eq!(
            map.decode(&mut state, invalid_id, &[0; 2]),
            Err(Error::Invalid)
        );
        assert_eq!(map.gamepad(&state), released(pad));
        assert_eq!(map.decode(&mut state, 1, &[0, 0]).unwrap().held, held);
        assert_eq!(
            map.decode(&mut state, 3, &[0, 0]).unwrap().held,
            Held::default()
        );
    }
    map.decode(&mut state, 1, &[1, 127]).unwrap();
    assert_eq!(map.decode(&mut state, 1, &[1]), Err(Error::Invalid));
    assert_eq!(map.gamepad(&state), released(map.gamepad(&state)));
}

#[test]
fn omitted_vendor_reports_never_forward_or_release_link_controls() {
    let descriptor = [mixed_descriptor(), vendor_reports()].concat();
    let mut catalog = Catalog::default();
    let mut link = Link::new(
        LinkId {
            slot: 0,
            generation: 1,
        },
        vec![Profile::compile(ServiceId(1), &descriptor).unwrap()],
        64,
        false,
        HostPlatform::Linux,
        &mut catalog,
    )
    .unwrap();
    let mut forward = Forwarder::default();
    while forward.packet().is_some() {
        forward.complete();
    }
    for (id, payload) in [(4, &[255; 2][..]), (255, &[255; 1024][..])] {
        assert_eq!(
            link.input(ServiceId(1), id, payload, &mut catalog, &mut forward, 0),
            Ok(false)
        );
        assert!(!link.take_input_forwarded());
        assert!(forward.packet().is_none());
    }
    link.input(ServiceId(1), 1, &[1, 127], &mut catalog, &mut forward, 0)
        .unwrap();
    link.input(ServiceId(1), 3, &[4, 1], &mut catalog, &mut forward, 0)
        .unwrap();
    link.input(ServiceId(1), 2, &[0, 0], &mut catalog, &mut forward, 0)
        .unwrap();
    assert!(link.take_input_forwarded());
    let pad = link.gamepad();
    assert_eq!(pad.buttons, 1);
    assert_eq!(pad.axes[0], 65535);
    while forward.packet().is_some() {
        forward.complete();
    }
    for (id, payload) in [(4, &[255; 2][..]), (255, &[255; 1024][..])] {
        link.input(ServiceId(1), id, payload, &mut catalog, &mut forward, 0)
            .unwrap();
        assert_eq!(link.gamepad(), pad);
        assert!(!link.take_input_forwarded());
        assert!(forward.packet().is_none());
        // A useful gamepad report forwards the profile's retained keyboard and mouse state.
        link.input(ServiceId(1), 2, &[0, 0], &mut catalog, &mut forward, 0)
            .unwrap();
        assert!(link.take_input_forwarded());
        assert!(forward.packet().is_none());
    }
    link.input(ServiceId(1), 1, &[0, 0], &mut catalog, &mut forward, 0)
        .unwrap();
    assert_eq!(link.gamepad().buttons, 0);
    assert_eq!(link.gamepad().axes[0], 32768);
    link.input(ServiceId(1), 3, &[0, 0], &mut catalog, &mut forward, 0)
        .unwrap();
    let mut releases = Vec::new();
    while let Some(packet) = forward.packet() {
        releases.push((packet.id, packet.bytes().to_vec()));
        forward.complete();
    }
    assert!(
        releases
            .iter()
            .any(|(id, bytes)| *id == 1 && bytes.iter().all(|b| *b == 0))
    );
    assert!(releases.iter().any(|(id, bytes)| *id == 2 && bytes[0] == 0));
    for (id, payload) in [(7, &[0; 2][..]), (1, &[0][..])] {
        link.input(ServiceId(1), 1, &[1, 127], &mut catalog, &mut forward, 0)
            .unwrap();
        assert_eq!(
            link.input(ServiceId(1), id, payload, &mut catalog, &mut forward, 0),
            Err(ErrorCode::ConnectionFailed)
        );
        assert_eq!(link.gamepad(), released(link.gamepad()));
    }
}

#[test]
fn vendor_fields_sharing_useful_report_ids_keep_offsets_and_length_checks() {
    let mut vendor = vendor_reports();
    // The vendor prefix shares report 1 with the gamepad that follows it.
    vendor[12] = 1;
    let descriptor = [vendor, SPLIT.to_vec()].concat();
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    assert_eq!(
        map.decode(&mut state, 1, &[0, 0, 1, 127]),
        Ok(Input::default())
    );
    let pad = map.gamepad(&state);
    assert_eq!(pad.buttons, 1);
    assert_eq!(pad.axes[0], 65535);
    assert_eq!(map.decode(&mut state, 1, &[1, 127]), Err(Error::Invalid));
    assert_eq!(map.gamepad(&state), released(pad));
    map.decode(&mut state, 1, &[0; 4]).unwrap();
    assert_eq!(map.gamepad(&state).buttons, 0);
}

#[test]
fn mixed_applications_keep_keyboard_mouse_and_gamepad_state_independent() {
    let map = Map::compile(&mixed_descriptor()).unwrap();
    assert_eq!(map.roles, hid::KEYBOARD | hid::MOUSE | hid::GAMEPAD);
    let mut state = map.state().unwrap();
    map.decode(&mut state, 1, &[1, 127]).unwrap();
    let held = map.decode(&mut state, 3, &[4, 1]).unwrap().held;
    assert_eq!(held.keys[0], 1 << 4);
    assert_eq!(held.buttons, 1);
    assert_eq!(map.gamepad(&state).buttons, 1);
    assert_eq!(map.decode(&mut state, 2, &[2, 0]).unwrap().held, held);
    assert_eq!(map.gamepad(&state).buttons, 0x201);
    let gamepad = map.gamepad(&state);
    assert_eq!(map.decode(&mut state, 3, &[1, 0]), Err(Error::Rollover));
    assert_eq!(map.gamepad(&state), gamepad);
    assert_eq!(map.decode(&mut state, 1, &[0, 0]).unwrap().held, held);
    assert_eq!(
        map.decode(&mut state, 3, &[0, 0]).unwrap().held,
        Held::default()
    );
}

#[test]
fn consumer_overflow_clears_gamepad_without_hiding_the_error() {
    let mut descriptor = SPLIT.to_vec();
    descriptor.extend_from_slice(&[
        5, 12, 9, 1, 0xa1, 1, 0x85, 3, 0x19, 0, 0x29, 9, 0x15, 0, 0x25, 9, 0x75, 8, 0x95, 9, 0x81,
        0, 0xc0,
    ]);
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    map.decode(&mut state, 1, &[1, 0]).unwrap();
    assert_eq!(
        map.decode(&mut state, 3, &[1, 2, 3, 4, 5, 6, 7, 8, 9]),
        Err(Error::Overflow)
    );
    assert_eq!(map.gamepad(&state), released(map.gamepad(&state)));
}

#[test]
fn forwarding_overflow_clears_all_gamepad_services() {
    let mut catalog = Catalog::default();
    let mut link = Link::new(
        LinkId {
            slot: 0,
            generation: 1,
        },
        vec![Profile::compile(ServiceId(1), &mixed_descriptor()).unwrap()],
        64,
        false,
        HostPlatform::Linux,
        &mut catalog,
    )
    .unwrap();
    let mut forward = Forwarder::default();
    while forward.packet().is_some() {
        forward.complete();
    }
    link.input(ServiceId(1), 1, &[1, 127], &mut catalog, &mut forward, 0)
        .unwrap();
    for i in 0..hid::QUEUE {
        forward
            .input(
                2 + i % 2,
                Input {
                    motion: [1, 0, 0, 0],
                    ..Input::default()
                },
            )
            .unwrap();
    }
    assert_eq!(
        link.input(ServiceId(1), 3, &[4, 0], &mut catalog, &mut forward, 1),
        Err(ErrorCode::InputOverflow)
    );
    assert_eq!(link.gamepad(), released(link.gamepad()));
}

#[test]
fn one_report_can_carry_gamepad_keyboard_and_mouse_fields() {
    let mut descriptor = mixed_descriptor();
    let id = descriptor
        .windows(2)
        .position(|bytes| bytes == [0x85, 3])
        .unwrap();
    descriptor[id + 1] = 1;
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    let held = map.decode(&mut state, 1, &[1, 127, 4, 1]).unwrap().held;
    assert_eq!(held.keys[0], 1 << 4);
    assert_eq!(held.buttons, 1);
    assert_eq!(map.gamepad(&state).buttons, 1);
    assert_eq!(map.gamepad(&state).axes[0], 65535);
    assert_eq!(map.decode(&mut state, 2, &[2, 0]).unwrap().held, held);
    assert_eq!(map.gamepad(&state).buttons, 0x201);
    assert_eq!(
        map.decode(&mut state, 1, &[0; 4]).unwrap().held,
        Held::default()
    );
    assert_eq!(map.gamepad(&state).buttons, 0x200);
}

#[test]
fn repeated_scalars_choose_the_first_valid_report_and_keep_neutral_hat() {
    let descriptor = [
        5, 1, 9, 5, 0xa1, 1, 0x85, 1, 9, 0x30, 0x15, 0, 0x25, 100, 0x75, 8, 0x95, 1, 0x81, 0x42, 9,
        0x39, 0x25, 7, 0x81, 0x42, 0x85, 2, 9, 0x30, 0x25, 100, 0x81, 0x42, 9, 0x39, 0x25, 7, 0x81,
        0x42, 0xc0,
    ];
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    map.decode(&mut state, 2, &[100, 4]).unwrap();
    assert_eq!(map.gamepad(&state).axes[0], 65535);
    assert_eq!(map.gamepad(&state).hat, Some(4));
    map.decode(&mut state, 1, &[50, 255]).unwrap();
    assert_eq!(map.gamepad(&state).axes[0], 32768);
    assert_eq!(map.gamepad(&state).hat, None);
    assert!(map.gamepad(&state).hat_valid);
    map.decode(&mut state, 1, &[255, 255]).unwrap();
    assert_eq!(map.gamepad(&state).axes[0], 65535);
}

#[test]
fn keyboard_rollover_preserves_gamepad_controls_across_services() {
    let mut catalog = Catalog::default();
    let mut forward = Forwarder::default();
    let mut link = Link::new(
        LinkId {
            slot: 0,
            generation: 1,
        },
        vec![
            Profile::compile(ServiceId(1), &mixed_descriptor()).unwrap(),
            Profile::compile(ServiceId(2), SPLIT).unwrap(),
        ],
        64,
        false,
        HostPlatform::Linux,
        &mut catalog,
    )
    .unwrap();
    for (service, id, bytes) in [(1, 1, [1, 127]), (2, 2, [2, 0x81])] {
        link.input(
            ServiceId(service),
            id,
            &bytes,
            &mut catalog,
            &mut forward,
            0,
        )
        .unwrap();
    }
    let held = link.gamepad();
    assert_eq!(held.buttons, 0x201);
    assert_eq!(held.axes[..2], [65535, 0]);
    assert_eq!(
        link.input(ServiceId(1), 3, &[1, 0], &mut catalog, &mut forward, 1),
        Ok(false)
    );
    assert_eq!(link.gamepad(), held);
    assert_eq!(
        link.input(ServiceId(2), 2, &[0], &mut catalog, &mut forward, 2),
        Err(ErrorCode::ConnectionFailed)
    );
    assert_eq!(link.gamepad(), released(link.gamepad()));
}

fn released(pad: Gamepad) -> Gamepad {
    Gamepad {
        axes_declared: pad.axes_declared,
        layout: pad.layout,
        ..Gamepad::default()
    }
}

#[path = "support/gamepad_descriptors.rs"]
mod descriptors;

#[test]
fn broad_consumer_arrays_keep_active_usages_and_release_reports_independently() {
    for maximum in [0x29cu16, u16::MAX] {
        let mut descriptor = descriptors::CONSUMER_ARRAY.to_vec();
        let offset = descriptor
            .windows(3)
            .position(|b| b == [0x2a, 255, 255])
            .unwrap();
        descriptor[offset + 1..offset + 3].copy_from_slice(&maximum.to_le_bytes());
        let map = Map::compile(&descriptor).unwrap();
        let mut state = map.state().unwrap();
        map.decode(&mut state, 1, &[128, 3]).unwrap();
        let mut selectors = [0; 18];
        selectors[..2].copy_from_slice(&maximum.to_le_bytes());
        map.decode(&mut state, 2, &selectors).unwrap();
        assert_eq!(
            map.gamepad(&state).consumers,
            [maximum, 0, 0, 0, 0, 0, 0, 0]
        );
        for (i, slot) in selectors.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            slot.copy_from_slice(&(i as u16 + 1).to_le_bytes());
        }
        map.decode(&mut state, 2, &selectors).unwrap();
        let pad = map.gamepad(&state);
        assert_eq!(pad.consumers, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(pad.buttons, 3);
        assert_eq!(pad.axes[0], 32896);
        // Repeated selectors consume one slot; later distinct selectors still fit.
        selectors[2..4].copy_from_slice(&1u16.to_le_bytes());
        map.decode(&mut state, 2, &selectors).unwrap();
        assert_eq!(map.gamepad(&state).consumers, [1, 3, 4, 5, 6, 7, 8, 9]);
        selectors[..4].fill(0);
        map.decode(&mut state, 2, &selectors).unwrap();
        assert_eq!(map.gamepad(&state).consumers, [3, 4, 5, 6, 7, 8, 9, 0]);
        map.decode(&mut state, 1, &[0, 0]).unwrap();
        assert_eq!(map.gamepad(&state).buttons, 0);
        assert_eq!(map.gamepad(&state).consumers, [3, 4, 5, 6, 7, 8, 9, 0]);
        map.decode(&mut state, 2, &[0; 18]).unwrap();
        assert_eq!(map.gamepad(&state).consumers, [0; 8]);
    }
}

#[test]
fn consumer_variables_deduplicate_spans_and_keep_controls_after_the_eighth_usage() {
    let map = Map::compile(descriptors::CONSUMER_VARIABLES).unwrap();
    let mut state = map.state().unwrap();
    map.decode(&mut state, 0, &[255, 255]).unwrap();
    assert_eq!(map.gamepad(&state).consumers, [1, 2, 3, 4, 5, 6, 7, 8]);
    map.decode(&mut state, 0, &[0, 255]).unwrap();
    assert_eq!(map.gamepad(&state).consumers, [2, 3, 4, 5, 6, 7, 8, 9]);
    map.decode(&mut state, 0, &[0, 128]).unwrap();
    assert_eq!(map.gamepad(&state).consumers, [9, 0, 0, 0, 0, 0, 0, 0]);
    map.decode(&mut state, 0, &[0, 0]).unwrap();
    assert_eq!(map.gamepad(&state).consumers, [0; 8]);
}

#[test]
fn overflowing_consumer_reports_release_and_promote_stored_controls() {
    let mut descriptor = descriptors::CONSUMER_ARRAY.to_vec();
    descriptor.pop();
    descriptor.extend_from_slice(&[0x85, 3, 0x19, 0, 0x2a, 255, 255, 0x95, 1, 0x81, 0, 0xc0]);
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    let mut selectors = [0; 18];
    for (i, slot) in selectors
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .take(8)
        .enumerate()
    {
        slot.copy_from_slice(&(i as u16 + 1).to_le_bytes());
    }
    map.decode(&mut state, 3, &[9, 0]).unwrap();
    map.decode(&mut state, 2, &selectors).unwrap();
    assert_eq!(map.gamepad(&state).consumers, [1, 2, 3, 4, 5, 6, 7, 8]);
    selectors[..2].fill(0);
    map.decode(&mut state, 2, &selectors).unwrap();
    assert_eq!(map.gamepad(&state).consumers, [2, 3, 4, 5, 6, 7, 8, 9]);
    map.decode(&mut state, 3, &[0, 0]).unwrap();
    assert_eq!(map.gamepad(&state).consumers, [2, 3, 4, 5, 6, 7, 8, 0]);
    assert_eq!(map.decode(&mut state, 3, &[]), Err(Error::Invalid));
    assert_eq!(map.gamepad(&state).consumers, [0; 8]);
}

#[test]
fn services_merge_bounded_active_consumers_without_rejecting_the_device() {
    let mut catalog = Catalog::default();
    let mut link = Link::new(
        LinkId {
            slot: 0,
            generation: 1,
        },
        vec![
            Profile::compile(ServiceId(1), descriptors::CONSUMER_ARRAY).unwrap(),
            Profile::compile(ServiceId(2), descriptors::CONSUMER_ARRAY).unwrap(),
        ],
        64,
        true,
        HostPlatform::Linux,
        &mut catalog,
    )
    .unwrap();
    let mut forward = Forwarder::default();
    let mut first = [0; 18];
    let mut second = [0; 18];
    for (i, slot) in first.as_chunks_mut::<2>().0.iter_mut().take(5).enumerate() {
        slot.copy_from_slice(&(i as u16 + 1).to_le_bytes());
    }
    for (i, slot) in second.as_chunks_mut::<2>().0.iter_mut().take(6).enumerate() {
        slot.copy_from_slice(&(i as u16 + 5).to_le_bytes());
    }
    link.input(ServiceId(2), 2, &second, &mut catalog, &mut forward, 0)
        .unwrap();
    link.input(ServiceId(1), 2, &first, &mut catalog, &mut forward, 0)
        .unwrap();
    assert_eq!(link.gamepad().consumers, [1, 2, 3, 4, 5, 6, 7, 8]);
    link.input(ServiceId(1), 2, &[0; 18], &mut catalog, &mut forward, 0)
        .unwrap();
    assert_eq!(link.gamepad().consumers, [5, 6, 7, 8, 9, 10, 0, 0]);
    link.input(ServiceId(2), 1, &[255, 128], &mut catalog, &mut forward, 0)
        .unwrap();
    assert_eq!(link.gamepad().buttons, 128);
    link.input(ServiceId(2), 2, &[0; 18], &mut catalog, &mut forward, 0)
        .unwrap();
    assert_eq!(link.gamepad().consumers, [0; 8]);
    assert_eq!(link.gamepad().buttons, 128);
}

#[test]
fn public_descriptors_select_only_verified_control_geometry() {
    use hid::GamepadLayout::{Ds4Basic, Generic, LunaBluetooth, XboxBluetooth, XboxCompact};
    for (descriptor, layout, axes) in [
        (descriptors::XBOX_SERIES, XboxBluetooth, 0xe7),
        (descriptors::XBOX_ONE_S, XboxBluetooth, 0xe7),
        (descriptors::XBOX_COMPACT, XboxCompact, 0x3f),
        (descriptors::DS4_BASIC, Ds4Basic, 0x3f),
        (descriptors::LUNA_BLE, LunaBluetooth, 0xe7),
        (GAMEPAD, Generic, 0x3f),
    ] {
        let map = Map::compile(descriptor).unwrap();
        let state = map.state().unwrap();
        assert_eq!(map.gamepad(&state).layout, layout);
        assert_eq!(map.gamepad(&state).axes_declared, axes);
        assert_eq!(map.gamepad(&state).axes_valid, 0);
    }
    // Changing the report ID, range, usage, width, button set, or offset rejects
    // the fixed convention even when the same number of axes/buttons remains.
    for (descriptor, pattern, replacement) in [
        (descriptors::DS4_BASIC, &[0x85, 1][..], &[0x85, 9][..]),
        (descriptors::DS4_BASIC, &[0x26, 255, 0], &[0x26, 254, 0]),
        (descriptors::DS4_BASIC, &[9, 0x35], &[9, 0x34]),
        (descriptors::DS4_BASIC, &[0x75, 8], &[0x75, 9]),
        (descriptors::DS4_BASIC, &[0x29, 14], &[0x29, 13]),
        (descriptors::DS4_BASIC, &[0x75, 6], &[0x75, 7]),
        (descriptors::XBOX_SERIES, &[0x85, 1], &[0x85, 9]),
        (descriptors::XBOX_SERIES, &[0x26, 255, 3], &[0x26, 254, 3]),
        (descriptors::XBOX_SERIES, &[9, 0x35], &[9, 0x34]),
        (descriptors::XBOX_SERIES, &[0x75, 16], &[0x75, 17]),
        (descriptors::XBOX_SERIES, &[0x29, 15], &[0x29, 14]),
        (descriptors::XBOX_SERIES, &[0x75, 6], &[0x75, 7]),
    ] {
        let mut changed = descriptor.to_vec();
        let offset = changed
            .windows(pattern.len())
            .position(|b| b == pattern)
            .unwrap();
        changed[offset..offset + pattern.len()].copy_from_slice(replacement);
        let map = Map::compile(&changed).unwrap();
        assert_eq!(
            map.gamepad(&map.state().unwrap()).layout,
            Generic,
            "{pattern:?} -> {replacement:?}"
        );
    }
}

#[test]
fn xbox_simulation_triggers_scale_independently_and_consumer_buttons_do_not_forward() {
    for descriptor in [descriptors::XBOX_SERIES, descriptors::XBOX_ONE_S] {
        let map = Map::compile(descriptor).unwrap();
        assert_eq!(map.roles, hid::GAMEPAD);
        assert!(map.supports_usage(0x200c5));
        assert!(map.supports_usage(0x200c4));
        let mut state = map.state().unwrap();
        for (left, right) in [(0u16, 0u16), (1023, 0), (0, 1023), (1023, 1023)] {
            let mut report = [0; 16];
            report[8..10].copy_from_slice(&left.to_le_bytes());
            report[10..12].copy_from_slice(&right.to_le_bytes());
            report[15] = 1; // Consumer Share or AC Back remains outside keyboard forwarding.
            assert_eq!(map.decode(&mut state, 1, &report), Ok(Input::default()));
            let pad = map.gamepad(&state);
            assert_eq!(
                pad.axes[6..],
                [
                    if left == 0 { 0 } else { 65535 },
                    if right == 0 { 0 } else { 65535 }
                ]
            );
            assert_eq!(pad.axes_valid, 0xe7);
            assert_eq!(pad.axes_declared, 0xe7);
        }
        if descriptor == descriptors::XBOX_ONE_S {
            assert_eq!(map.decode(&mut state, 2, &[1]), Ok(Input::default()));
            assert_eq!(
                map.gamepad(&state).layout,
                hid::GamepadLayout::XboxBluetooth
            );
        }
    }
}

#[test]
fn descriptor_assignments_survive_nulls_split_reports_services_and_clear() {
    // Rx/Ry and Brake are separate from Z/Rz and Accelerator. All allow nulls.
    let first = [
        5, 1, 9, 5, 0xa1, 1, 0x85, 1, 9, 0x33, 9, 0x34, 0x15, 0, 0x25, 100, 0x75, 8, 0x95, 2, 0x81,
        0x42, 5, 2, 9, 0xc5, 0x95, 1, 0x81, 0x42, 0xc0,
    ];
    let second = [
        5, 1, 9, 5, 0xa1, 1, 0x85, 2, 9, 0x32, 9, 0x35, 0x15, 0, 0x25, 100, 0x75, 8, 0x95, 2, 0x81,
        0x42, 5, 2, 9, 0xc4, 0x95, 1, 0x81, 0x42, 0xc0,
    ];
    let descriptor = [first.as_slice(), second.as_slice()].concat();
    let map = Map::compile(&descriptor).unwrap();
    let mut state = map.state().unwrap();
    let mut catalog = Catalog::default();
    let mut forward = Forwarder::default();
    let mut link = Link::new(
        LinkId {
            slot: 0,
            generation: 1,
        },
        vec![
            Profile::compile(ServiceId(1), &first).unwrap(),
            Profile::compile(ServiceId(2), &second).unwrap(),
        ],
        64,
        false,
        HostPlatform::Linux,
        &mut catalog,
    )
    .unwrap();
    assert_eq!(link.gamepad().axes_declared, 0xfc);
    for (id, report) in [
        (2, [100, 0, 100]),
        (1, [255; 3]),
        (1, [0, 100, 0]),
        (2, [255; 3]),
    ] {
        map.decode(&mut state, id, &report).unwrap();
        link.input(
            ServiceId(u16::from(id)),
            id,
            &report,
            &mut catalog,
            &mut forward,
            0,
        )
        .unwrap();
        let pad = map.gamepad(&state);
        assert_eq!(pad.axes_declared, 0xfc);
        assert_eq!(pad.layout, hid::GamepadLayout::Generic);
        assert_eq!(link.gamepad(), pad);
    }
    state.clear();
    link.disconnected(&mut catalog, &mut forward);
    assert_eq!(link.gamepad(), map.gamepad(&state));
    assert_eq!(link.gamepad().axes_declared, 0xfc);
    assert_eq!(link.gamepad().axes_valid, 0);
}
