use cordial_core::{
    forward::Forwarder,
    hid::Map,
    hidpp::Error,
    link::{Link, LinkId, Profile, ServiceId},
    settings::Catalog,
};
use cordial_protocol::{
    hidpp::{FeatureId, ProtocolState},
    identifiers::{HostPlatform, NormalizationState, SettingsState, Transport},
    info::InfoKey,
    settings::SettingValue,
};

// Bluetooth GET captures: feature IDs, revisions, flags, protocol and battery.
// Device information responses use synthetic metadata.
const FEATURES: &[(u16, u8, u8)] = &[
    (0x0001, 2, 0),
    (0x0003, 6, 0),
    (0x0005, 3, 0),
    (0x1d4b, 0, 0),
    (0x0020, 0, 0),
    (0x0007, 0, 0),
    (0x0011, 0, 0),
    (0x1004, 5, 0),
    (0x8071, 4, 0),
    (0x8081, 0, 0),
    (0x1b10, 0, 0),
    (0x4523, 1, 0),
    (0x4540, 1, 0),
    (0x8040, 0, 0),
    (0x8101, 0, 0),
    (0x1b05, 1, 0),
    (0x8051, 0, 0),
    (0x00d0, 3, 0),
    (0x1802, 0, 0x70),
    (0x1803, 1, 0x70),
    (0x1807, 3, 0x70),
    (0x1817, 0, 0x70),
    (0x1805, 0, 0x60),
    (0x1830, 0, 0x70),
    (0x1890, 9, 0x68),
    (0x1891, 9, 0x68),
    (0x1e00, 0, 0x40),
    (0x1e02, 0, 0x60),
    (0x1602, 0, 0),
    (0x1eb0, 0, 0x70),
    (0x1861, 1, 0x70),
    (0x18b0, 1, 0x70),
];

fn descriptor() -> Vec<u8> {
    include_str!("fixtures/g515-bluetooth.hex")
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

fn link(catalog: &mut Catalog, generation: u64) -> Link {
    Link::new(
        LinkId {
            slot: 0,
            generation,
        },
        vec![Profile::compile(ServiceId(0), &descriptor()).unwrap()],
        19,
        true,
        HostPlatform::Linux,
        catalog,
    )
    .unwrap()
}

fn answer(packet: &[u8]) -> [u8; 19] {
    let mut reply = [0; 19];
    reply[..3].copy_from_slice(&packet[..3]);
    let p = &mut reply[3..];
    let feature = if packet[1] == 0 {
        0
    } else {
        FEATURES[packet[1] as usize - 1].0
    };
    match (feature, packet[2] >> 4) {
        (0, 1) => p[..3].copy_from_slice(&[4, 2, packet[5]]),
        (0, 0) => {
            let id = u16::from_be_bytes([packet[3], packet[4]]);
            if let Some(index) = FEATURES.iter().position(|f| f.0 == id) {
                let f = FEATURES[index];
                p[..3].copy_from_slice(&[(index + 1) as u8, f.2, f.1]);
            }
        }
        (1, 0) => p[0] = FEATURES.len() as u8,
        (1, 1) => {
            let f = FEATURES[packet[3] as usize - 1];
            p[..2].copy_from_slice(&f.0.to_be_bytes());
            p[2] = f.2;
            p[3] = f.1;
        }
        (3, 0) => {} // No firmware entities or optional serial in synthetic metadata.
        (5, 0) => p[0] = 11,
        (5, 1) => p[..11].copy_from_slice(b"G515 LS TKL"),
        (5, 2) => p[0] = 2,
        (0x1004, 0) => p[..2].copy_from_slice(&[0x0f, 0x0f]),
        (0x1004, 1) => p[..4].copy_from_slice(&[0x41, 0x04, 0, 0]),
        other => panic!("unexpected feature operation {other:?}"),
    }
    reply
}

fn pump(
    link: &mut Link,
    catalog: &mut Catalog,
    forward: &mut Forwarder,
    now: &mut u64,
) -> Vec<(u8, u8)> {
    let mut requests = Vec::new();
    for _ in 0..500 {
        *now += 1;
        link.poll(catalog, forward, *now).unwrap();
        if let Some(output) = link.output(0, *now).unwrap() {
            let id = output.id;
            let report = output.report_id;
            let payload = output.payload.to_vec();
            link.output_complete(id, true, catalog, forward, *now)
                .unwrap();
            if report == Some(0x11) {
                requests.push((payload[1], payload[2] >> 4));
                let reply = answer(&payload);
                link.input(ServiceId(0), 0x11, &reply, catalog, forward, *now)
                    .unwrap();
            }
        }
        if !link.busy() && link.client.idle() {
            return requests;
        }
    }
    panic!("G515 setup did not settle");
}

#[test]
fn bluetooth_g515_keeps_protocol_battery_and_input_without_translation_controls() {
    let map = Map::compile(&descriptor()).unwrap();
    assert_eq!(map.hidpp_reports, cordial_core::hid::HIDPP_LONG);
    assert_eq!(map.roles, 7);
    let mut catalog = Catalog::default();
    catalog.info.battery.configure(Transport::Ble, true);
    let mut link = link(&mut catalog, 1);
    let mut forward = Forwarder::default();
    let mut now = 0;
    assert_eq!(link.client.protocol, ProtocolState::Unknown);
    let requests = pump(&mut link, &mut catalog, &mut forward, &mut now);
    let detected = ProtocolState::Detected { major: 4, minor: 2 };
    assert_eq!(link.client.protocol, detected);
    assert_eq!(link.hidpp_found(), Some(true));
    assert_eq!(link.client.status, NormalizationState::Unsupported);
    assert_eq!(link.client.error, Some(Error::ControlsUnavailable));
    assert_eq!(link.settings.state, SettingsState::Ready);
    assert_eq!(link.settings.error, None);
    assert_eq!(
        catalog.info.battery.field(InfoKey::BatteryPercent).value,
        SettingValue::Integer(65)
    );
    assert_eq!(
        catalog.info.battery.field(InfoKey::BatteryCharging).value,
        SettingValue::Bool(false)
    );
    assert_eq!(catalog.features().len(), FEATURES.len() + 1);
    assert!(
        catalog
            .features()
            .iter()
            .find(|f| f.id == FeatureId::CONFIG_CHANGE)
            .unwrap()
            .supported()
    );
    for id in [0x8071, 0x8081, 0x8040, 0x1b05] {
        assert!(
            !catalog
                .features()
                .iter()
                .find(|f| f.id == FeatureId(id))
                .unwrap()
                .supported()
        );
    }
    assert!(
        !requests.contains(&(5, 1)),
        "missing controls must not cause a reset"
    );
    while forward.packet().is_some() {
        forward.complete();
    }
    let mut keys = [0; 16];
    keys[1] = 1; // Usage 0x04 in the captured bitmap keyboard report.
    link.input(ServiceId(0), 0x18, &keys, &mut catalog, &mut forward, now)
        .unwrap();
    assert!(forward.packet().unwrap().bytes().iter().any(|b| *b != 0));
    forward.complete();
    link.reconfigure(false, HostPlatform::Mac, &mut catalog);
    let requests = pump(&mut link, &mut catalog, &mut forward, &mut now);
    assert_eq!(link.client.protocol, detected);
    assert_eq!(link.client.status, NormalizationState::Off);
    assert_eq!(link.settings.state, SettingsState::Ready);
    assert!(
        !requests.contains(&(0, 1)),
        "a preference change retains negotiation"
    );
    assert!(
        !requests.contains(&(5, 1)),
        "read-only discovery must not reset"
    );
    link.disconnected(&mut catalog, &mut forward);
    let next = self::link(&mut catalog, 2);
    assert_eq!(next.client.protocol, ProtocolState::Unknown);
}
