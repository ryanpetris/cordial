use cordial_core::{forward::Forwarder, link::*, settings::Catalog};
use cordial_protocol::{
    errors::ErrorCode,
    identifiers::{HostPlatform, SettingsState},
};

const KEYBOARD: &[u8] = &[
    0x05, 1, 0x09, 6, 0xa1, 1, 0x05, 7, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8,
    0x81, 2, 0x75, 8, 0x95, 1, 0x81, 1, 0x05, 8, 0x19, 1, 0x29, 5, 0x75, 1, 0x95, 5, 0x91, 2, 0x75,
    3, 0x95, 1, 0x91, 1, 0x05, 7, 0x19, 0, 0x29, 0x65, 0x15, 0, 0x25, 0x65, 0x75, 8, 0x95, 6, 0x81,
    0, 0xc0,
];
fn drain(f: &mut Forwarder) -> Vec<Vec<u8>> {
    let mut reports = vec![];
    while let Some(packet) = f.packet() {
        reports.push(packet.bytes().to_vec());
        f.complete();
    }
    reports
}
fn link(c: &mut Catalog, generation: u64) -> Link {
    Link::new(
        LinkId {
            slot: 0,
            generation,
        },
        vec![
            Profile::compile(ServiceId(1), KEYBOARD).unwrap(),
            Profile::compile(ServiceId(2), KEYBOARD).unwrap(),
        ],
        255,
        false,
        HostPlatform::Linux,
        c,
    )
    .unwrap()
}
#[test]
fn service_maps_share_held_keys_and_disconnect_releases_them() {
    let mut c = Catalog::default();
    let mut l = link(&mut c, 1);
    let mut f = Forwarder::default();
    drain(&mut f);
    l.poll(&mut c, &mut f, 0).unwrap();
    l.poll(&mut c, &mut f, 1).unwrap();
    assert_eq!(l.settings.state, SettingsState::Unsupported);
    let down = [0, 0, 4, 0, 0, 0, 0, 0];
    for service in [1, 2] {
        l.input(ServiceId(service), 0, &down, &mut c, &mut f, 2)
            .unwrap();
    }
    assert_eq!(drain(&mut f).len(), 1);
    l.input(ServiceId(1), 0, &[0; 8], &mut c, &mut f, 3)
        .unwrap();
    assert!(drain(&mut f).is_empty());
    assert_eq!(
        l.input(ServiceId(9), 0, &down, &mut c, &mut f, 3),
        Err(ErrorCode::ConnectionFailed)
    );
    l.disconnected(&mut c, &mut f);
    assert_eq!(drain(&mut f), vec![vec![0; 32]]);
}
#[test]
fn indicators_wait_for_completion_and_late_completion_cannot_cross_reconnect() {
    let mut c = Catalog::default();
    let mut l = link(&mut c, 1);
    let mut f = Forwarder::default();
    let out = l.output(2, 0).unwrap().unwrap();
    assert_eq!(out.payload, &[2]);
    assert_eq!(out.service, ServiceId(1));
    assert_eq!(out.report_id, None);
    let old = out.id;
    assert!(l.output(4, 1).unwrap().is_none());
    l.output_complete(old, true, &mut c, &mut f, 2).unwrap();
    let out = l.output(4, 3).unwrap().unwrap();
    assert_eq!(out.payload, &[4]);
    assert_eq!(out.service, ServiceId(1));
    let next = out.id;
    l.output_complete(old, true, &mut c, &mut f, 4).unwrap();
    assert!(l.output(4, 4).unwrap().is_none());
    l.output_complete(next, true, &mut c, &mut f, 5).unwrap();
    let out = l.output(4, 6).unwrap().unwrap();
    assert_eq!(out.service, ServiceId(2));
    let next = out.id;
    l.output_complete(next, true, &mut c, &mut f, 7).unwrap();
    assert!(l.output(4, 8).unwrap().is_none());
    l.disconnected(&mut c, &mut f);
    let mut l = link(&mut c, 2);
    let new = l.output(1, 9).unwrap().unwrap().id;
    l.output_complete(old, true, &mut c, &mut f, 10).unwrap();
    assert!(l.output(1, 11).unwrap().is_none());
    l.output_complete(new, false, &mut c, &mut f, 12).unwrap();
    assert_eq!(l.warnings & 2, 2);
}

#[test]
fn standard_battery_reports_are_scaled_queried_and_cleared_without_vendor_fallback() {
    use cordial_core::bluetooth::{InputReport, ReportType};
    use cordial_protocol::{
        identifiers::Transport, info::InfoKey as K, settings::SettingValue as V,
    };
    let mut descriptor = KEYBOARD.to_vec();
    // Numbered Input battery strength 0..255 plus Feature charging flag.
    // The keyboard must be numbered too when adding numbered reports.
    descriptor.splice(0..0, [0x85, 1]);
    descriptor.extend_from_slice(&[
        5, 6, 9, 1, 0xa1, 1, 0x85, 2, 9, 0x20, 0x15, 0, 0x26, 0xff, 0, 0x75, 8, 0x95, 1, 0x81, 2,
        0x85, 3, 5, 0x85, 9, 0x44, 0x25, 1, 0xb1, 2, 0xc0,
    ]);
    let mut c = Catalog::default();
    c.info.battery.configure(Transport::Classic, false);
    let mut l = Link::new(
        LinkId {
            slot: 0,
            generation: 1,
        },
        vec![Profile::compile(ServiceId(0), &descriptor).unwrap()],
        64,
        false,
        HostPlatform::Linux,
        &mut c,
    )
    .unwrap();
    let mut f = Forwarder::default();
    for now in 0..4 {
        l.poll(&mut c, &mut f, now).unwrap();
    }
    let read = l.battery_read(&c, 4).unwrap();
    assert_eq!(read.report_id, Some(2));
    assert_eq!(read.kind, ReportType::Input);
    let reply = InputReport::new(l.id, ServiceId(0), 2, &[128]).unwrap();
    l.battery_read_complete(read.id, read.kind, Ok(&reply), &mut c);
    assert_eq!(
        c.info.battery.field(K::BatteryPercent).value,
        V::Integer(50)
    );
    let read = l.battery_read(&c, 5).unwrap();
    assert_eq!(read.report_id, Some(3));
    assert_eq!(read.kind, ReportType::Feature);
    let reply = InputReport::new(l.id, ServiceId(0), 3, &[1]).unwrap();
    l.battery_read_complete(read.id, read.kind, Ok(&reply), &mut c);
    assert_eq!(
        c.info.battery.field(K::BatteryCharging).value,
        V::Bool(true)
    );
    l.input(ServiceId(0), 2, &[0], &mut c, &mut f, 6).unwrap();
    assert_eq!(c.info.battery.field(K::BatteryPercent).value, V::Integer(0));
    assert!(l.battery_read(&c, 7).is_none());
    let read = l.battery_read(&c, 60_008).unwrap();
    assert_eq!(
        read.report_id,
        Some(3),
        "only Feature reports require polling"
    );
    l.reconfigure(true, HostPlatform::Linux, &mut c);
    l.battery_read_complete(read.id, read.kind, Ok(&reply), &mut c);
    assert!(
        !c.info.battery.field(K::BatteryCharging).available,
        "old-source read discarded"
    );
}

#[test]
fn hid_charging_flags_clear_unknown_and_accept_array_status() {
    use cordial_core::{bluetooth::ReportType, hid::Map};
    use cordial_protocol::{info::InfoKey as K, settings::SettingValue as V};
    for (data, flags, range) in [(vec![1, 0], 2, 1), (vec![0x45, 0x40], 0, 0x47)] {
        let mut d = KEYBOARD.to_vec();
        d.splice(0..0, [0x85, 1]);
        d.extend_from_slice(&[
            5, 0x85, 9, 1, 0xa1, 1, 0x85, 2, 0x15, 0, 0x25, range, 0x75, 8, 0x95, 1,
        ]);
        if flags == 2 {
            d.extend_from_slice(&[9, 0x45]);
        } else {
            d.extend_from_slice(&[0x19, 0, 0x29, 0x47]);
        }
        d.extend_from_slice(&[0xb1, flags, 0xc0]);
        // Restrict the array to the declared state range and matching logical minimum.
        if flags == 0 {
            let n = d.len();
            d[n - 7..n - 3].copy_from_slice(&[0x19, 0x40, 0x29, 0x47]);
            d.splice(n - 3..n - 3, [0x15, 0x40]);
        }
        let m = Map::compile(&d).unwrap();
        for (v, expected) in data.into_iter().zip([V::Bool(false), V::Null]) {
            let mut value = None;
            m.battery(2, ReportType::Feature, &[v], |_, k, v| {
                if k == K::BatteryCharging {
                    value = Some(v)
                }
            });
            assert_eq!(value, Some(expected));
        }
    }
}

#[test]
fn optional_battery_collections_keep_input_and_select_lowest() {
    use cordial_core::{battery::Battery, bluetooth::ReportType, hid::Map};
    use cordial_protocol::{
        identifiers::Transport, info::InfoKey as K, settings::SettingValue as V,
    };
    let mut d = KEYBOARD.to_vec();
    d.splice(0..0, [0x85, 1]);
    for report in [2, 3, 4, 5] {
        d.extend_from_slice(&[
            5, 0x84, 9, 0x12, 0xa1, 1, 5, 0x85, 0x85, report, 0x15, 0, 0x25, 100, 0x75, 8, 0x95, 1,
            9, 0x64, 0x81, 2, 9, 0x44, 0x25, 1, 0x81, 2, 0xc0,
        ]);
    }
    let m = Map::compile(&d).unwrap();
    let mut b = Battery::default();
    b.configure(Transport::Classic, false);
    b.connection(true, false);
    for (report, bytes) in [(5, [20, 0]), (2, [70, 1]), (3, [90, 1]), (4, [80, 1])] {
        m.battery(report, ReportType::Input, &bytes, |i, k, v| {
            b.hid_reading(i, k, v)
        });
    }
    assert_eq!(b.field(K::BatteryPercent).value, V::Integer(20));
    assert_eq!(b.field(K::BatteryCharging).value, V::Bool(false));
    d.extend_from_slice(&[
        5, 0x85, 9, 1, 0xa1, 1, 0x85, 4, 0x15, 0, 0x25, 100, 0x75, 8, 0x95, 20, 9, 0x64, 0xb1, 2,
        0xc0,
    ]);
    assert!(
        Map::compile(&d).is_ok(),
        "optional battery overflow must not reject HID"
    );
}

#[test]
fn legacy_battery_reads_short_reports_and_discards_reconfigured_reply() {
    use cordial_protocol::{
        identifiers::Transport, info::InfoKey as K, settings::SettingValue as V,
    };
    let mut descriptor = KEYBOARD.to_vec();
    descriptor.splice(0..0, [0x85, 1]);
    descriptor.extend_from_slice(&[
        0x06, 0, 0xff, 9, 1, 0xa1, 1, 0x85, 0x10, 0x75, 8, 0x95, 6, 0x15, 0, 0x26, 0xff, 0, 9, 1,
        0x81, 0, 9, 1, 0x91, 0, 0xc0,
    ]);
    let mut c = Catalog::default();
    c.info.battery.configure(Transport::Classic, true);
    let mut l = Link::new(
        LinkId {
            slot: 0,
            generation: 1,
        },
        vec![Profile::compile(ServiceId(0), &descriptor).unwrap()],
        64,
        true,
        HostPlatform::Linux,
        &mut c,
    )
    .unwrap();
    let mut f = Forwarder::default();
    let mut reads = 0;
    for now in 0..100 {
        l.poll(&mut c, &mut f, now).unwrap();
        let Some(out) = l.output(0, now).unwrap() else {
            continue;
        };
        let (id, report, payload) = (out.id, out.report_id, out.payload.to_vec());
        l.output_complete(id, true, &mut c, &mut f, now).unwrap();
        if report != Some(0x10) {
            continue;
        }
        assert_eq!(payload.len(), 6);
        let response = match (payload[1], payload[2]) {
            (0, _) => [0xff, 0x8f, payload[1], payload[2], 1, 0],
            (0x81, 0) => [0xff, 0x81, 0, 0, 0, 0],
            (0x80, 0) => {
                assert_eq!(payload[3], 0x10);
                [0xff, 0x80, 0, 0, 0, 0]
            }
            (0x81, 0x0d) => {
                reads += 1;
                if reads == 2 {
                    l.reconfigure(false, HostPlatform::Linux, &mut c);
                }
                [0xff, 0x81, 0x0d, 51, 0, 0x50]
            }
            other => panic!("unexpected request {other:?}"),
        };
        l.input(ServiceId(0), 0x10, &response, &mut c, &mut f, now)
            .unwrap();
        l.poll(&mut c, &mut f, now).unwrap();
        if reads == 1 && !l.battery_busy(&c) {
            assert_eq!(
                c.info.battery.field(K::BatteryPercent).value,
                V::Integer(51)
            );
            assert_eq!(
                c.info.battery.field(K::BatteryCharging).value,
                V::Bool(true)
            );
            l.start_information(&mut c, now).unwrap();
        }
        if reads == 2 {
            assert_eq!(c.info.battery.field(K::BatteryPercent).value, V::Null);
            break;
        }
    }
    assert_eq!(reads, 2);
}
