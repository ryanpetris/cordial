use cordial_core::{
    hid::{HIDPP_LONG, HIDPP_SHORT, Held},
    hidpp::*,
};
use cordial_protocol::identifiers::{HostPlatform, NormalizationState};

fn respond(client: &mut Client, packet: &[u8; 19], parameters: &[u8], now: &mut u64) -> bool {
    let mut reply = [0; 19];
    reply[..3].copy_from_slice(&packet[..3]);
    reply[3..3 + parameters.len()].copy_from_slice(parameters);
    *now += 1;
    client.receive(0x11, &reply, *now)
}
fn activate(platform: HostPlatform, version: u8) -> (Client, u64) {
    let mut client = Client::new(HIDPP_SHORT | HIDPP_LONG);
    let mut now = 100;
    client.configure(true, platform, now);
    let controls: [u16; 4] = [0xc7, 0xc8, 0xe0, 0xd1];
    let mut resets = 0;
    let mut writes = 0;
    while let Some(packet) = client.next_output(now) {
        let mut response = [0; 16];
        match (packet[1], packet[2] >> 4) {
            (0, 1) => response[..3].copy_from_slice(&[4, 5, packet[5]]),
            (0, 0) => {
                response[0] = match &packet[3..5] {
                    [0, 0x20] => 5,
                    [0x1b, 4] => 8,
                    _ => panic!("unexpected feature"),
                };
                response[2] = version;
            }
            (5, 1) => {
                assert_eq!(&packet[3..], &[0; 16]);
                resets += 1;
            }
            (8, 0) => response[0] = controls.len() as u8,
            (8, 1) => {
                response[..2].copy_from_slice(&controls[packet[3] as usize].to_be_bytes());
                response[4] = 0x7a;
            }
            (8, 2) => {
                response[..2].copy_from_slice(&packet[3..5]);
                response[2] = 0x54; // Existing persistent/raw/XY bits are not setters.
                response[4] = 0x7f;
                response[5] = 1;
            }
            (8, 3) => {
                assert_eq!(packet[5], 3); // Only the temporary-diversion valid bit.
                assert_eq!(&packet[6..], &[0; 13]);
                response[..6].copy_from_slice(&packet[3..9]);
                writes += 1;
            }
            _ => panic!("unexpected request"),
        }
        respond(&mut client, &packet, &response, &mut now);
        assert!(client.next_output(now).is_none()); // Reply precedes transport completion.
        client.tx_complete(true, now);
        now += 1;
    }
    assert_eq!(resets, 1);
    assert_eq!(writes, 3);
    assert_eq!(client.status, NormalizationState::Active);
    assert_eq!(client.error, None);
    (client, now)
}
fn notification(client: &mut Client, cids: &[u16], now: u64) -> bool {
    let mut bytes = [0; 19];
    bytes[..3].copy_from_slice(&[0xff, 8, 0]);
    for (i, cid) in cids.iter().enumerate() {
        bytes[3 + 2 * i..5 + 2 * i].copy_from_slice(&cid.to_be_bytes());
    }
    client.receive(0x11, &bytes, now)
}
#[test]
fn normalization_uses_temporary_diversion_and_standard_host_translations() {
    for platform in [
        HostPlatform::Linux,
        HostPlatform::Windows,
        HostPlatform::Mac,
    ] {
        for revision in [0, 3, 4, 5] {
            let (mut client, now) = activate(platform, revision);
            assert!(notification(&mut client, &[0xc7, 0xc8, 0xe0], now));
            assert!(client.held.consumers.contains(&0x70));
            assert!(client.held.consumers.contains(&0x6f));
            match platform {
                HostPlatform::Linux => assert_eq!(client.held.keys[28], 8),
                HostPlatform::Windows => {
                    assert_eq!(client.held.keys[28], 8);
                    assert_eq!(client.held.keys[0x2b / 8], 1 << (0x2b % 8));
                }
                HostPlatform::Mac => assert!(client.held.consumers.contains(&0x29f)),
            }
            let before = client.held;
            assert!(!notification(&mut client, &[0xc7, 0, 0xc8], now));
            assert_eq!(client.held, before);
            assert!(!notification(&mut client, &[0xc7, 0xc7], now));
            assert_eq!(client.held, before);
            assert!(notification(&mut client, &[], now));
            assert_eq!(client.held, Held::default());
        }
    }
}
#[test]
fn disabled_probe_is_read_only_and_settings_keep_transport_ownership() {
    let mut client = Client::new(HIDPP_LONG);
    let mut now = 1;
    client.configure(false, HostPlatform::Linux, now);
    let probe = client.next_output(now).unwrap();
    assert_eq!(&probe[..3], &[0xff, 0, 0x11]);
    respond(&mut client, &probe, &[4, 5, 0xa5], &mut now);
    assert!(client.next_output(now).is_none());
    client.tx_complete(true, now);
    assert_eq!(client.status, NormalizationState::Off);
    assert!(client.idle());
    assert!(client.exchange(12, 1, &[2], now));
    assert!(!client.exchange_sent);
    let packet = client.next_output(now).unwrap();
    assert!(client.exchange_sent);
    assert!(!client.quiesce());
    let mut stale = packet;
    stale[2] ^= 1;
    respond(&mut client, &stale, &[9, 8, 7], &mut now);
    assert!(client.response().is_none());
    respond(&mut client, &packet, &[9, 8, 7], &mut now);
    assert_eq!(
        &client.response().unwrap().unwrap().bytes()[..3],
        &[9, 8, 7]
    );
    assert!(client.response().is_none());
    assert!(!client.idle());
    client.tx_complete(true, now);
    assert!(client.idle());
    assert!(client.exchange(12, 0, &[], now));
    client.next_output(now).unwrap();
    client.tx_complete(true, now);
    assert!(!client.tick(now + TIMEOUT_MS));
    assert_eq!(client.response(), Some(Err(Error::Timeout)));
    assert_eq!(client.status, NormalizationState::Off);
}
#[test]
fn disable_resets_and_uncertain_reset_releases_held_keys() {
    let (mut client, mut now) = activate(HostPlatform::Linux, 4);
    notification(&mut client, &[0xc7], now);
    assert!(client.configure(false, HostPlatform::Linux, now));
    assert_eq!(client.held, Held::default());
    let packet = client.next_output(now).unwrap();
    assert_eq!((packet[1], packet[2] >> 4), (5, 1));
    respond(&mut client, &packet, &[0; 3], &mut now);
    client.tx_complete(true, now);
    assert_eq!(client.status, NormalizationState::Off);

    let (mut client, mut now) = activate(HostPlatform::Windows, 4);
    notification(&mut client, &[0xe0], now);
    client.configure(true, HostPlatform::Linux, now);
    for parameters in [&[4, 5, 0xa5][..], &[5, 0, 0], &[8, 0, 4]] {
        let request = client.next_output(now).unwrap();
        respond(&mut client, &request, parameters, &mut now);
        client.tx_complete(true, now);
    }
    let reset = client.next_output(now).unwrap();
    assert_eq!(reset[1], 5);
    client.tx_complete(true, now);
    assert!(client.tick(now + TIMEOUT_MS));
    assert_eq!(client.held, Held::default());
    assert_eq!(client.error, Some(Error::Timeout));
}
