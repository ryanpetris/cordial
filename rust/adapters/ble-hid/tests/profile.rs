use cordial_ble_hid::{
    Backend,
    native::{Data, Event as Raw, Host},
};
use cordial_core::{
    bluetooth::{Bluetooth, ConnectionSecurity, Event, ReportType},
    devices::Peer,
    link::{LinkId, ServiceId, WriteId},
};
use cordial_protocol::{
    errors::ErrorCode as Error, identifiers::Transport, messages::PromptMethod,
};
use embassy_futures::block_on;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

const SECURITY: ConnectionSecurity = ConnectionSecurity {
    encrypted: Some(true),
    authenticated: Some(false),
    secure_connections: Some(true),
    key_size: Some(16),
    bonded: Some(true),
};
const PEER: Peer = Peer {
    address: [1, 2, 3, 4, 5, 6],
    random: false,
    transport: Transport::Ble,
};
const LINK: LinkId = LinkId {
    slot: 0,
    generation: 1,
};
const MAP: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x09, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xc0,
];
#[derive(Default)]
struct State {
    events: VecDeque<Raw>,
    token: u32,
    bonds: Vec<Peer>,
    forgotten: Vec<Peer>,
    writes: Vec<(u16, Vec<u8>)>,
    responses: Vec<bool>,
    output_properties: Option<u8>,
    connects: usize,
    reconnect: Vec<Vec<Peer>>,
    incoming: Vec<(u32, Option<u32>)>,
    reject_services: bool,
    information: bool,
    fail_info_reads: bool,
    map_size: usize,
}
#[derive(Clone)]
struct Mock(Rc<RefCell<State>>);
impl Mock {
    fn complete(&self, request: u32) {
        self.0.borrow_mut().events.push_back(Raw::Complete {
            request,
            result: Ok(()),
        });
    }
}
impl Host for Mock {
    fn scan_and_connect(&self) -> bool {
        false
    }
    fn adopt(&mut self, _: u32) -> Result<(), Error> {
        Ok(())
    }
    async fn changed(&self) {}
    fn start(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().events.push_back(Raw::Ready);
        Ok(())
    }
    fn next_event(&mut self) -> Option<Raw> {
        self.0.borrow_mut().events.pop_front()
    }
    fn scan(&mut self, _: u64, _: bool) -> Result<(), Error> {
        Ok(())
    }
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error> {
        self.0.borrow_mut().reconnect.push(peers.to_vec());
        Ok(())
    }
    fn incoming(&mut self, attempt: u32, token: Option<u32>) -> Result<(), Error> {
        self.0.borrow_mut().incoming.push((attempt, token));
        if let Some(token) = token {
            self.connect(token, PEER, false)?;
        }
        Ok(())
    }
    fn connect(&mut self, token: u32, peer: Peer, pairing: bool) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.token = token;
        s.connects += 1;
        if pairing {
            s.bonds.push(peer);
        }
        s.events.push_back(Raw::Connected {
            token,
            max_output: 244,
        });
        s.events.push_back(Raw::Security {
            token,
            identity: peer,
            security: SECURITY,
        });
        Ok(())
    }
    fn disconnect(&mut self, token: u32) {
        self.0
            .borrow_mut()
            .events
            .push_back(Raw::Disconnected { token, error: None });
    }
    fn pair_reply(
        &mut self,
        _: u32,
        _: PromptMethod,
        _: bool,
        _: Option<&str>,
    ) -> Result<(), Error> {
        Ok(())
    }
    fn services(&mut self, _: u32, request: u32, uuid: u16) -> Result<(), Error> {
        if uuid != 0x1812 {
            if self.0.borrow().information {
                let starts: &[u16] = match uuid {
                    0x180f => &[100, 140],
                    0x180a => &[200],
                    0x1800 => &[300],
                    _ => &[],
                };
                for start in starts {
                    self.0.borrow_mut().events.push_back(Raw::Service {
                        request,
                        start: *start,
                        end: *start + 19,
                    });
                }
            }
            self.complete(request);
            return Ok(());
        }
        if self.0.borrow().reject_services {
            return Err(Error::Busy);
        }
        for start in [10, 50] {
            self.0.borrow_mut().events.push_back(Raw::Service {
                request,
                start,
                end: start + 19,
            });
        }
        self.complete(request);
        Ok(())
    }
    fn characteristics(&mut self, _: u32, request: u32, start: u16, _: u16) -> Result<(), Error> {
        if start >= 100 {
            let uuid = match start {
                100 | 140 => 0x2a19,
                200 => 0x2a26,
                _ => 0x2a00,
            };
            self.0.borrow_mut().events.push_back(Raw::Characteristic {
                request,
                declaration: Some(start + 1),
                value: start + 2,
                properties: if start < 200 { 0x12 } else { 2 },
                uuid,
            });
            self.complete(request);
            return Ok(());
        }
        let output_properties = self.0.borrow().output_properties.unwrap_or(8);
        for (offset, uuid, properties) in [
            (1, 0x2a4b, 2),
            (4, 0x2a4d, 0x12),
            (10, 0x2a4d, output_properties),
        ] {
            self.0.borrow_mut().events.push_back(Raw::Characteristic {
                request,
                declaration: Some(start + offset),
                value: start + offset + 1,
                properties,
                uuid,
            });
        }
        self.complete(request);
        Ok(())
    }
    fn descriptors(&mut self, _: u32, request: u32, start: u16, _: u16) -> Result<(), Error> {
        if start >= 100 {
            self.0.borrow_mut().events.push_back(Raw::Descriptor {
                request,
                handle: start,
                uuid: 0x2902,
            });
            self.complete(request);
            return Ok(());
        }
        self.0.borrow_mut().events.push_back(Raw::Descriptor {
            request,
            handle: start,
            uuid: 0x2908,
        });
        if start == 16 || start == 56 {
            self.0.borrow_mut().events.push_back(Raw::Descriptor {
                request,
                handle: start + 1,
                uuid: 0x2902,
            });
        }
        self.complete(request);
        Ok(())
    }
    fn read(&mut self, _: u32, request: u32, handle: u16, _: bool) -> Result<(), Error> {
        if handle >= 100 && self.0.borrow().fail_info_reads {
            return Err(Error::Timeout);
        }
        let mut map = MAP.to_vec();
        // Zero-length Usage Page globals are legal and add no report fields.
        map.resize(self.0.borrow().map_size.max(map.len()), 0x04);
        let bytes: &[u8] = match handle {
            102 => &[51],
            142 => &[80],
            202 => b"1.2.3",
            302 => b"Full device name",
            12 | 52 => &map,
            16 | 56 => &[9, 1],
            22 | 62 => &[9, 2],
            _ => &[1, 2, 3],
        };
        for (i, part) in bytes.chunks(12).enumerate() {
            self.0.borrow_mut().events.push_back(Raw::Data {
                request,
                offset: (i * 12) as u16,
                data: Data::new(part)?,
            });
        }
        self.complete(request);
        Ok(())
    }
    fn write(
        &mut self,
        _: u32,
        request: u32,
        handle: u16,
        bytes: &[u8],
        response: bool,
    ) -> Result<(), Error> {
        self.0.borrow_mut().responses.push(response);
        self.0.borrow_mut().writes.push((handle, bytes.to_vec()));
        self.complete(request);
        Ok(())
    }
    fn subscribe(
        &mut self,
        token: u32,
        request: u32,
        _: u16,
        cccd: u16,
        indications: bool,
    ) -> Result<(), Error> {
        self.write(
            token,
            request,
            cccd,
            &[if indications { 2 } else { 1 }, 0],
            true,
        )
    }
    fn bonds(&mut self) -> Result<Vec<Peer>, Error> {
        Ok(self.0.borrow().bonds.clone())
    }
    async fn forget(&mut self, peer: Peer) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.bonds.retain(|p| *p != peer);
        s.forgotten.push(peer);
        Ok(())
    }
}
fn setup(saved: bool) -> (Backend<Mock>, Rc<RefCell<State>>) {
    let state = Rc::new(RefCell::new(State::default()));
    if saved {
        state.borrow_mut().bonds.push(PEER);
    }
    let mut backend = Backend::new(Mock(state.clone()), || 0).unwrap();
    backend.start().unwrap();
    block_on(backend.poll());
    assert!(matches!(backend.next_profile_event(), Some(Event::Ready)));
    (backend, state)
}
fn connected(backend: &mut Backend<Mock>) {
    block_on(backend.poll());
    let mut event = backend.next_profile_event().unwrap();
    if let Event::Security { security, .. } = event {
        assert_eq!(security, SECURITY);
        event = backend.next_profile_event().unwrap();
    }
    match event {
        Event::Connected {
            link, descriptors, ..
        } => {
            assert_eq!(link, LINK);
            assert_eq!(descriptors.len(), 2);
            for (i, d) in descriptors.iter().enumerate() {
                assert_eq!(d.service, ServiceId(i as u16));
                assert_eq!(&*d.bytes, MAP);
            }
        }
        _ => panic!("expected complete HID setup"),
    }
    // Optional metadata shares ATT; wait before tests submit HID writes.
    settle_information(backend);
}
fn settle_information(backend: &mut Backend<Mock>) {
    for _ in 0..80 {
        block_on(backend.poll());
    }
}
#[test]
fn pairing_waits_for_saved_policy_then_discovers_service_scoped_reports() {
    let (mut b, state) = setup(false);
    b.connect(LINK, PEER, true).unwrap();
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security {
            security: SECURITY,
            ..
        })
    ));
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Bonded {
            link: LINK,
            identity: PEER
        })
    ));
    assert!(b.next_profile_event().is_none());
    assert!(!b.can_write(LINK));
    assert!(state.borrow().writes.is_empty());
    b.adopt(LINK).unwrap();
    connected(&mut b);
    assert_eq!(state.borrow().writes, [(17, vec![1, 0]), (57, vec![1, 0])]);
    let token = state.borrow().token;
    state.borrow_mut().events.push_back(Raw::Notification {
        token,
        handle: 55,
        data: Data::new(&[8]).unwrap(),
    });
    block_on(b.poll());
    match b.next_profile_event().unwrap() {
        Event::Input(r) => {
            assert_eq!(r.service, ServiceId(1));
            assert_eq!(r.report_id, 9);
            assert_eq!(r.payload(), [8]);
        }
        _ => panic!("expected input"),
    }
    let id = WriteId {
        link: LINK,
        sequence: 7,
    };
    b.write(id, ServiceId(1), ReportType::Output, Some(9), &[2])
        .unwrap();
    assert!(!b.can_write(LINK));
    block_on(b.poll());
    assert!(
        matches!(b.next_profile_event(),Some(Event::Written { id:got,result:Ok(()) }) if got==id)
    );
    assert_eq!(state.borrow().writes.last(), Some(&(61, vec![2])));
    assert!(b.can_write(LINK));
}
#[test]
fn cancellation_removes_only_an_unadopted_new_bond() {
    let (mut b, state) = setup(false);
    b.connect(LINK, PEER, true).unwrap();
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { .. })
    ));
    assert!(matches!(b.next_profile_event(), Some(Event::Bonded { .. })));
    b.disconnect(LINK);
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected { .. })
    ));
    assert_eq!(state.borrow().forgotten, [PEER]);
    let (mut b, state) = setup(true);
    assert_eq!(state.borrow().connects, 0);
    b.connect(LINK, PEER, false).unwrap();
    connected(&mut b);
    b.disconnect(LINK);
    block_on(b.poll());
    assert!(state.borrow().forgotten.is_empty());
    assert_eq!(state.borrow().bonds, [PEER]);
}
#[test]
fn host_restart_retires_links_and_allows_reconnect_after_resync() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false).unwrap();
    connected(&mut b);
    let token = state.borrow().token;
    state.borrow_mut().events.extend([
        Raw::Disconnected {
            token,
            error: Some(Error::ConnectionFailed),
        },
        Raw::Restarting(Error::ConnectionFailed),
    ]);
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected { link: LINK, .. })
    ));
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Restarting(Error::ConnectionFailed))
    ));
    assert!(b.connect(LINK, PEER, false).is_err());
    state.borrow_mut().events.push_back(Raw::Ready);
    block_on(b.poll());
    assert!(matches!(b.next_profile_event(), Some(Event::Ready)));
    b.connect(LINK, PEER, false).unwrap();
    connected(&mut b);
    assert!(state.borrow().forgotten.is_empty());
}
#[test]
fn host_restart_cancels_a_connect_still_in_the_native_queue() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false).unwrap();
    // The host reset happens before the queued connect executes. Its resync
    // can precede the old command's Connected/Security callbacks.
    state.borrow_mut().events.push_front(Raw::Ready);
    state
        .borrow_mut()
        .events
        .push_front(Raw::Restarting(Error::ConnectionFailed));
    block_on(b.poll());
    assert!(matches!(b.next_profile_event(), Some(Event::Restarting(_))));
    assert!(matches!(b.next_profile_event(), Some(Event::Ready)));
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected { link: LINK, .. })
    ));
    assert!(b.next_profile_event().is_none());
    b.connect(LINK, PEER, false).unwrap();
    connected(&mut b);
}
#[test]
fn att_map_limit_applies_to_the_whole_fragmented_value() {
    for size in [512, 513] {
        let (mut b, state) = setup(true);
        state.borrow_mut().map_size = size;
        b.connect(LINK, PEER, false).unwrap();
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Security {
                security: SECURITY,
                ..
            })
        ));
        if size == 512 {
            assert!(matches!(
                b.next_profile_event(),
                Some(Event::Connected { .. })
            ));
        } else {
            assert!(matches!(
                b.next_profile_event(),
                Some(Event::Disconnected {
                    error: Some(Error::InputOverflow),
                    ..
                })
            ));
            state.borrow_mut().map_size = 0;
            b.connect(LINK, PEER, false).unwrap();
            connected(&mut b);
        }
        assert!(b.next_profile_event().is_none());
    }
}
#[test]
fn cancelled_pairing_reconciles_identity_before_admission_and_preserves_existing_bonds() {
    for security_delivered in [false, true] {
        let (mut b, state) = setup(true);
        let candidate = Peer {
            address: [2; 6],
            random: true,
            ..PEER
        };
        let identity = Peer {
            address: [3; 6],
            ..PEER
        };
        b.connect(LINK, candidate, true).unwrap();
        {
            let mut s = state.borrow_mut();
            s.bonds = vec![PEER, identity];
            s.events.clear();
            let token = s.token;
            if security_delivered {
                s.events.push_back(Raw::Security {
                    token,
                    identity,
                    security: SECURITY,
                });
            }
        }
        b.disconnect(LINK);
        let other = LinkId {
            slot: 1,
            generation: 2,
        };
        assert_eq!(
            b.connect(
                other,
                Peer {
                    address: [4; 6],
                    ..PEER
                },
                true
            ),
            Err(Error::Busy)
        );
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Disconnected { error: None, .. })
        ));
        assert_eq!(state.borrow().forgotten, [identity]);
        assert_eq!(state.borrow().bonds, [PEER]);
    }
}
#[test]
fn failed_adoption_setup_keeps_new_bond_cleanup_active() {
    let (mut b, state) = setup(false);
    b.connect(LINK, PEER, true).unwrap();
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { .. })
    ));
    assert!(matches!(b.next_profile_event(), Some(Event::Bonded { .. })));
    state.borrow_mut().reject_services = true;
    assert_eq!(b.adopt(LINK), Err(Error::Busy));
    b.disconnect(LINK);
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected { .. })
    ));
    assert_eq!(state.borrow().forgotten, [PEER]);
    assert!(state.borrow().bonds.is_empty());
}
#[test]
fn reused_slot_rejects_old_input_and_operation_results() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false).unwrap();
    connected(&mut b);
    let old = state.borrow().token;
    b.disconnect(LINK);
    block_on(b.poll());
    b.next_profile_event();
    let new = LinkId {
        generation: 2,
        ..LINK
    };
    b.connect(new, PEER, false).unwrap();
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { .. })
    ));
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Connected { .. })
    ));
    state.borrow_mut().events.push_back(Raw::Notification {
        token: old,
        handle: 15,
        data: Data::new(&[4]).unwrap(),
    });
    state.borrow_mut().events.push_back(Raw::Complete {
        request: old + 1,
        result: Err(Error::ConnectionFailed),
    });
    block_on(b.poll());
    assert!(b.next_profile_event().is_none());
    settle_information(&mut b);
    assert!(b.can_write(new));
}

#[test]
fn live_security_updates_preserve_unknown_properties_without_repairing() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false).unwrap();
    connected(&mut b);
    let security = ConnectionSecurity {
        authenticated: Some(true),
        secure_connections: None,
        key_size: None,
        ..SECURITY
    };
    let token = state.borrow().token;
    state.borrow_mut().events.push_back(Raw::Security {
        token,
        identity: PEER,
        security,
    });
    block_on(b.poll());
    assert!(
        matches!(b.next_profile_event(), Some(Event::Security { security: got, .. }) if got == security)
    );
    assert!(b.next_profile_event().is_none());
    assert!(b.can_write(LINK));
    assert!(state.borrow().forgotten.is_empty());
}

#[test]
fn pairing_security_waits_for_room_for_both_events() {
    let (mut b, state) = setup(false);
    for _ in 0..7 {
        state.borrow_mut().events.push_back(Raw::Found {
            kind: cordial_protocol::messages::DeviceKind::Unknown,
            address: PEER,
            connectable: true,
            scan: 1,
            peer: PEER,
            name: "Keyboard".into(),
            rssi: -40,
        });
    }
    b.connect(LINK, PEER, true).unwrap();
    block_on(b.poll());
    // Seven discovery events leave only one slot; defer the two-event pairing.
    assert_eq!(state.borrow().events.len(), 2);
    assert!(matches!(b.next_profile_event(), Some(Event::Found { .. })));
    block_on(b.poll());
    assert!(state.borrow().events.is_empty());
    for _ in 0..6 {
        assert!(matches!(b.next_profile_event(), Some(Event::Found { .. })));
    }
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { .. })
    ));
    assert!(matches!(b.next_profile_event(), Some(Event::Bonded { .. })));
    assert!(b.next_profile_event().is_none());
}

#[test]
fn accept_list_admission_uses_current_bond_and_never_queues_duplicate_updates() {
    let (mut backend, state) = setup(true);
    backend.reconnect(&[PEER]).unwrap();
    backend.reconnect(&[PEER]).unwrap();
    assert_eq!(state.borrow().reconnect.len(), 1);
    state.borrow_mut().events.push_back(Raw::Incoming {
        attempt: 42,
        peer: PEER,
    });
    block_on(backend.poll());
    assert!(matches!(
        backend.next_profile_event(),
        Some(Event::Incoming {
            attempt: 42,
            peer: PEER
        })
    ));
    assert_eq!(state.borrow().connects, 0);
    backend.incoming(42, Some(LINK)).unwrap();
    connected(&mut backend);
    assert_eq!(state.borrow().incoming.len(), 1);
    assert!(state.borrow().incoming[0].1.is_some());
    assert!(state.borrow().forgotten.is_empty());

    let (mut backend, state) = setup(false);
    state.borrow_mut().events.push_back(Raw::Incoming {
        attempt: 43,
        peer: PEER,
    });
    block_on(backend.poll());
    backend.next_profile_event();
    assert_eq!(
        backend.incoming(43, Some(LINK)),
        Err(Error::AuthenticationFailed)
    );
    assert_eq!(state.borrow().incoming, [(43, None)]);
    assert_eq!(state.borrow().connects, 0);
}

#[test]
fn output_prefers_advertised_write_commands_and_falls_back_to_requests() {
    for (properties, length, response) in [
        (0x0c, 1, Some(false)),
        (0x04, 1, Some(false)),
        (0x08, 1, Some(true)),
        (0x0c, 245, Some(true)),
        (0x04, 245, None),
    ] {
        let (mut b, state) = setup(false);
        state.borrow_mut().output_properties = Some(properties);
        b.connect(LINK, PEER, true).unwrap();
        block_on(b.poll());
        while b.next_profile_event().is_some() {}
        b.adopt(LINK).unwrap();
        connected(&mut b);
        state.borrow_mut().responses.clear();
        let id = WriteId {
            link: LINK,
            sequence: 7,
        };
        let result = b.write(
            id,
            ServiceId(1),
            ReportType::Output,
            Some(9),
            &vec![2; length],
        );
        let Some(response) = response else {
            assert_eq!(result, Err(Error::UnsupportedHid));
            assert!(state.borrow().responses.is_empty());
            assert!(b.can_write(LINK));
            continue;
        };
        result.unwrap();
        assert_eq!(state.borrow().responses.last(), Some(&response));
        assert!(!b.can_write(LINK));
        block_on(b.poll());
        assert!(
            matches!(b.next_profile_event(), Some(Event::Written { id: got, result: Ok(()) }) if got == id)
        );
        assert!(b.can_write(LINK));
    }
}

#[test]
fn optional_information_discovers_subscribes_and_preserves_hid_on_failure() {
    for fail in [false, true] {
        let (mut b, state) = setup(true);
        state.borrow_mut().information = true;
        state.borrow_mut().fail_info_reads = fail;
        b.connect(LINK, PEER, false).unwrap();
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Security { .. })
        ));
        assert!(
            matches!(b.next_profile_event(), Some(Event::Connected { .. })),
            "HID is admitted before optional discovery"
        );
        let mut values = Vec::new();
        let mut wrote = false;
        let mut completed_write = false;
        for _ in 0..100 {
            block_on(b.poll());
            while let Some(e) = b.next_profile_event() {
                match e {
                    Event::Information {
                        uuid,
                        instance,
                        bytes,
                        ..
                    } => values.push((uuid, instance, bytes)),
                    Event::Written { result: Ok(()), .. } => completed_write = true,
                    _ => panic!("optional failure must not disconnect HID"),
                }
            }
            if !wrote && b.info_busy(LINK) && b.can_write(LINK) {
                b.write(
                    WriteId {
                        link: LINK,
                        sequence: 55,
                    },
                    ServiceId(1),
                    ReportType::Output,
                    Some(9),
                    &[2],
                )
                .unwrap();
                wrote = true;
            }
        }
        assert!(
            wrote && completed_write,
            "HID output must progress while information remains queued"
        );
        assert!(!b.info_busy(LINK));
        assert!(b.can_write(LINK));
        assert_eq!(values.len(), 5);
        assert_eq!(values[0].0, 0x180f);
        assert_eq!(&*values[0].2, &[2]);
        assert!(values.iter().any(|(u, n, p)| *u == 0x2a19
            && *n == 1
            && &**p == if fail { &[][..] } else { &[80][..] }));
        assert!(state.borrow().writes.contains(&(103, vec![1, 0])));
        assert!(state.borrow().writes.contains(&(143, vec![1, 0])));
        let token = state.borrow().token;
        for (handle, bytes) in [(102, &[50][..]), (15, &[4][..])] {
            state.borrow_mut().events.push_back(Raw::Notification {
                token,
                handle,
                data: Data::new(bytes).unwrap(),
            });
        }
        block_on(b.poll());
        assert!(
            matches!(b.next_profile_event(),Some(Event::Information{uuid:0x2a19,instance:0,bytes,..}) if *bytes==[50])
        );
        assert!(matches!(b.next_profile_event(), Some(Event::Input(_))));
        b.refresh_info(LINK).unwrap();
        assert!(b.info_busy(LINK));
    }
}

// Most HID tests use peers without information services. Consume the empty BAS
// discovery result while keeping every HID/security/completion event visible.
trait ProfileEvents {
    fn next_profile_event(&mut self) -> Option<Event>;
}
impl ProfileEvents for Backend<Mock> {
    fn next_profile_event(&mut self) -> Option<Event> {
        loop {
            match self.next_event() {
                Some(Event::Information {
                    uuid: 0x180f,
                    bytes,
                    ..
                }) if *bytes == [0] => {}
                event => return event,
            }
        }
    }
}
#[test]
fn successful_discovery_without_bas_reports_zero_instances() {
    let (mut b, _) = setup(true);
    b.connect(LINK, PEER, false).unwrap();
    let mut zero = false;
    for _ in 0..20 {
        block_on(b.poll());
        while let Some(e) = b.next_event() {
            if matches!(e, Event::Information { uuid: 0x180f, success: true, bytes, .. } if *bytes == [0])
            {
                zero = true;
            }
        }
    }
    assert!(zero);
}
