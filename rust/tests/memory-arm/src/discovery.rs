//! Scripted public HID++ replies exercise the production discovery allocator.
use cordial_core::{features::Engine, hid::HIDPP_LONG, hidpp::Client, settings::Catalog};
use cordial_protocol::{identifiers::SettingsState, settings::SettingKey};

const KNOWN: &[(u16, u8)] = &[
    (1, 2),
    (3, 4),
    (5, 2),
    (0x1000, 0),
    (0x40a2, 0),
    (0x1982, 3),
    (0x2201, 1),
    (0x2110, 0),
    (0x2121, 1),
    (0x2150, 0),
];
fn feature(index: u8) -> (u16, u8) {
    if index == 0 {
        (0, 0)
    } else {
        KNOWN
            .get(index as usize - 1)
            .copied()
            .unwrap_or((0x8000 + index as u16, 0))
    }
}
fn answer(mut packet: [u8; 19]) -> [u8; 19] {
    let id = feature(packet[1]).0;
    let command = packet[2] >> 4;
    let a = &packet[3..];
    let mut p = [0u8; 16];
    match (id, command) {
        (0, 0) => {
            let wanted = u16::from_be_bytes([a[0], a[1]]);
            let index = (1..=255).find(|&i| feature(i).0 == wanted).unwrap();
            p[..3].copy_from_slice(&[index, 0, feature(index).1]);
        }
        (1, 0) => p[0] = 255,
        (1, 1) => {
            let (id, version) = feature(a[0]);
            p[..2].copy_from_slice(&id.to_be_bytes());
            p[3] = version;
        }
        (3, 0) => p.copy_from_slice(&[
            2, 0x12, 0x34, 0x56, 0x78, 0, 2, 0xab, 0xcd, 0, 0, 0, 0, 3, 1, 0,
        ]),
        (3, 1) => p[..8].copy_from_slice(&[a[0], b'A', b'B', b'C', 0x12, 0x34, 0x56, 0x78]),
        (3, 2) => p[..12].copy_from_slice(b"ABCD12345678"),
        (5, 0) => p[0] = 64,
        (5, 1) => p.fill(b'\\'),
        (5, 2) => p[0] = 0,
        (0x1000, 1) => p[..2].copy_from_slice(&[100, 6]),
        (0x1000, 0) => p[..3].copy_from_slice(&[70, 50, 0]),
        (0x40a2, 0) => p[..2].copy_from_slice(&[0, 1]),
        (0x1982, 0) => {
            p.copy_from_slice(&[1, 0x1f, 0x3f, 0x7f, 0, 3, 6, 0, 12, 0, 18, 0, 0, 0, 0, 0])
        }
        (0x1982, 2) => p[..4].copy_from_slice(&[8, 3, 5, 0]),
        (0x2201, 0) => p[0] = 2,
        (0x2201, 1) => {
            p[..13].copy_from_slice(&[a[0], 1, 0x90, 3, 0x20, 6, 0x40, 8, 0, 10, 0, 12, 0])
        }
        (0x2201, 2) => p[..5].copy_from_slice(&[a[0], 3, 0x20, 3, 0x20]),
        (0x2110, 0) => p[..3].copy_from_slice(&[2, 20, 18]),
        (0x2121, 0) => p[..4].copy_from_slice(&[8, 12, 0, 0]),
        (0x2121, 1) => p[0] = 0,
        (0x2150, 0) => p[..8].copy_from_slice(&[0, 18, 0, 90, 0, 3, 0, 1]),
        (0x2150, 1) => p[..2].copy_from_slice(&[0, 6]),
        _ => panic!("unexpected discovery request {id:04x}/{command}"),
    }
    packet[3..].copy_from_slice(&p);
    packet
}
pub fn run(catalog: &mut Catalog) {
    let mut engine = Engine::default();
    let mut client = Client::new(HIDPP_LONG);
    client.protocol = [2, 0];
    catalog.connection(true, false);
    engine.activate(catalog, 100).unwrap();
    for now in 101..1101 {
        client.tick(now);
        engine.poll(catalog, &mut client, now);
        if let Some(packet) = client.next_output(now) {
            client.receive(0x11, &answer(packet), now);
            engine.poll(catalog, &mut client, now);
            client.tx_complete(true, now);
        }
        if !engine.busy() || engine.done() {
            assert_eq!(engine.state, SettingsState::Ready);
            assert_eq!(catalog.features().len(), 256);
            assert_eq!(catalog.records().len(), SettingKey::ALL.len());
            catalog.connection(true, true);
            return;
        }
    }
    panic!("discovery did not complete");
}
