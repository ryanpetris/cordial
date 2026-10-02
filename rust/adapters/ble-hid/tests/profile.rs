use cordial_ble_hid::{
    Backend,
    native::{Data, Event as Raw, Host},
};
use cordial_core::model::{errors::ErrorCode as Error, identifiers::Transport, link::PromptMethod};
use cordial_core::{
    bluetooth::{
        Bluetooth, ConnectionSecurity, DatabaseHash, Descriptor, Event, Layout, LayoutReport,
        ReportMap, ReportType,
    },
    devices::Peer,
    link::{LinkId, ServiceId, WriteId},
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
    /// Every request in order, as (procedure, uuid or handle).
    log: Vec<(&'static str, u16)>,
    /// Hold CCCD write completions until the test releases them.
    hold: bool,
    held: VecDeque<Raw>,
    /// Number of upcoming CCCD writes that fail.
    fail_subscribes: usize,
    /// Completes HID service discovery with this error.
    services_error: Option<Error>,
    /// HID characteristic discovery reports a malformed declaration first.
    malformed: bool,
    /// Report map reads return a map that does not compile.
    bad_map: bool,
    /// Reads of this handle complete with an error response.
    read_error: Option<u16>,
    /// The device's GATT Database Hash; reading an absent one fails.
    hash: Option<[u8; 16]>,
    /// Reading the Database Hash completes with this error response.
    hash_error: Option<Error>,
    /// The host refuses to submit Database Hash reads.
    reject_hash: bool,
    /// The Database Hash value is delivered truncated.
    short_hash: bool,
    /// Number of upcoming accept-list, scan, offer and disconnect submissions
    /// the host cannot queue.
    full: usize,
}
#[derive(Clone)]
struct Mock(Rc<RefCell<State>>);
impl Mock {
    fn queue_full(&self) -> bool {
        let mut s = self.0.borrow_mut();
        let full = s.full > 0;
        s.full = s.full.saturating_sub(1);
        full
    }
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
    fn scan(&mut self, _: u64, enabled: bool) -> Result<(), Error> {
        if self.queue_full() {
            return Err(Error::Capacity);
        }
        self.0.borrow_mut().log.push(("scan", u16::from(enabled)));
        Ok(())
    }
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error> {
        if self.queue_full() {
            return Err(Error::Busy);
        }
        self.0.borrow_mut().reconnect.push(peers.to_vec());
        Ok(())
    }
    fn incoming(&mut self, attempt: u32, token: Option<u32>) -> Result<(), Error> {
        if self.queue_full() {
            return Err(Error::Capacity);
        }
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
    fn disconnect(&mut self, token: u32) -> Result<(), Error> {
        if self.queue_full() {
            return Err(Error::Capacity);
        }
        self.0
            .borrow_mut()
            .events
            .push_back(Raw::Disconnected { token, error: None });
        Ok(())
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
        self.0.borrow_mut().log.push(("services", uuid));
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
        let error = self.0.borrow().services_error;
        if let Some(error) = error {
            self.0.borrow_mut().events.push_back(Raw::Complete {
                request,
                result: Err(error),
            });
            return Ok(());
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
        self.0.borrow_mut().log.push(("characteristics", start));
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
        if self.0.borrow().malformed {
            self.0.borrow_mut().events.push_back(Raw::Characteristic {
                request,
                declaration: Some(start + 1),
                value: start + 1,
                properties: 2,
                uuid: 0x2a4b,
            });
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
        self.0.borrow_mut().log.push(("descriptors", start));
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
        self.0.borrow_mut().log.push(("read", handle));
        if handle >= 100 && self.0.borrow().fail_info_reads {
            return Err(Error::Timeout);
        }
        if self.0.borrow().read_error == Some(handle) {
            self.0.borrow_mut().events.push_back(Raw::Complete {
                request,
                result: Err(Error::Timeout),
            });
            return Ok(());
        }
        let mut map = if self.0.borrow().bad_map {
            vec![0xa1, 0x01]
        } else {
            MAP.to_vec()
        };
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
    fn read_by_uuid(&mut self, _: u32, request: u32, uuid: u16) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.log.push(("read_by_uuid", uuid));
        if s.reject_hash {
            return Err(Error::Busy);
        }
        let result = match (s.hash_error, s.hash) {
            (Some(error), _) => Err(error),
            (None, Some(hash)) if uuid == 0x2b2a => {
                let length = if s.short_hash { 8 } else { 16 };
                s.events.push_back(Raw::Data {
                    request,
                    offset: 0,
                    data: Data::new(&hash[..length])?,
                });
                Ok(())
            }
            _ => Err(Error::UnsupportedHid),
        };
        s.events.push_back(Raw::Complete { request, result });
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
        self.0.borrow_mut().log.push(("write", handle));
        self.0.borrow_mut().responses.push(response);
        self.0.borrow_mut().writes.push((handle, bytes.to_vec()));
        self.complete(request);
        Ok(())
    }
    fn subscribe(
        &mut self,
        _: u32,
        request: u32,
        _: u16,
        cccd: u16,
        indications: bool,
    ) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.log.push(("subscribe", cccd));
        s.responses.push(true);
        s.writes
            .push((cccd, vec![if indications { 2 } else { 1 }, 0]));
        let result = if s.fail_subscribes > 0 {
            s.fail_subscribes -= 1;
            Err(Error::Timeout)
        } else {
            Ok(())
        };
        let complete = Raw::Complete { request, result };
        if s.hold {
            s.held.push_back(complete);
        } else {
            s.events.push_back(complete);
        }
        Ok(())
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
                assert_eq!(
                    d.map.reports()[0].bits[0],
                    cordial_core::hid::Map::compile(MAP).unwrap().reports()[0].bits[0]
                );
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
    b.connect(LINK, PEER, true, None).unwrap();
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
    b.connect(LINK, PEER, true, None).unwrap();
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
    b.connect(LINK, PEER, false, None).unwrap();
    connected(&mut b);
    b.disconnect(LINK);
    block_on(b.poll());
    assert!(state.borrow().forgotten.is_empty());
    assert_eq!(state.borrow().bonds, [PEER]);
}
#[test]
fn host_restart_retires_links_and_allows_reconnect_after_resync() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, None).unwrap();
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
    assert!(b.connect(LINK, PEER, false, None).is_err());
    state.borrow_mut().events.push_back(Raw::Ready);
    block_on(b.poll());
    assert!(matches!(b.next_profile_event(), Some(Event::Ready)));
    b.connect(LINK, PEER, false, None).unwrap();
    connected(&mut b);
    assert!(state.borrow().forgotten.is_empty());
}
#[test]
fn host_restart_cancels_a_connect_still_in_the_native_queue() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, None).unwrap();
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
    b.connect(LINK, PEER, false, None).unwrap();
    connected(&mut b);
}
#[test]
fn descriptor_limit_applies_to_the_whole_fragmented_map() {
    for size in [512, 600, 2048, 2049] {
        let (mut b, state) = setup(true);
        state.borrow_mut().map_size = size;
        b.connect(LINK, PEER, false, None).unwrap();
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Security {
                security: SECURITY,
                ..
            })
        ));
        if size <= 2048 {
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
            b.connect(LINK, PEER, false, None).unwrap();
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
        b.connect(LINK, candidate, true, None).unwrap();
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
                true,
                None
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
    b.connect(LINK, PEER, true, None).unwrap();
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { .. })
    ));
    assert!(matches!(b.next_profile_event(), Some(Event::Bonded { .. })));
    state.borrow_mut().reject_hash = true;
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
    b.connect(LINK, PEER, false, None).unwrap();
    connected(&mut b);
    let old = state.borrow().token;
    b.disconnect(LINK);
    block_on(b.poll());
    b.next_profile_event();
    let new = LinkId {
        generation: 2,
        ..LINK
    };
    b.connect(new, PEER, false, None).unwrap();
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
    b.connect(LINK, PEER, false, None).unwrap();
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
            kind: cordial_core::model::link::DeviceKind::Unknown,
            address: PEER,
            connectable: true,
            scan: 1,
            peer: PEER,
            name: "Keyboard".into(),
            rssi: -40,
        });
    }
    b.connect(LINK, PEER, true, None).unwrap();
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
    backend.incoming(42, Some(LINK), None).unwrap();
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
        backend.incoming(43, Some(LINK), None),
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
        b.connect(LINK, PEER, true, None).unwrap();
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
            assert_eq!(result, Err(Error::HidReportTooLarge));
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
        b.connect(LINK, PEER, false, None).unwrap();
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
    b.connect(LINK, PEER, false, None).unwrap();
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

/// The layout the mock host's HID services describe.
fn layout() -> Layout {
    let report = |service, kind, value, properties, cccd| LayoutReport {
        service,
        kind,
        id: 9,
        value,
        properties,
        cccd,
    };
    Layout {
        maps: vec![ReportMap(MAP.to_vec()); 2],
        reports: vec![
            report(0, ReportType::Input, 15, 0x12, 17),
            report(0, ReportType::Output, 21, 8, 0),
            report(1, ReportType::Input, 55, 0x12, 57),
            report(1, ReportType::Output, 61, 8, 0),
        ],
        hash: None,
    }
}
/// A saved layout that differs from the device's in one report ID.
fn changed() -> Layout {
    let mut layout = layout();
    layout.reports[0].id = 7;
    layout
}
fn notify(state: &Rc<RefCell<State>>, handle: u16, bytes: &[u8]) {
    let token = state.borrow().token;
    state.borrow_mut().events.push_back(Raw::Notification {
        token,
        handle,
        data: Data::new(bytes).unwrap(),
    });
}
fn release(state: &Rc<RefCell<State>>) {
    let mut s = state.borrow_mut();
    let held = std::mem::take(&mut s.held);
    s.events.extend(held);
}
fn hid_requests(state: &Rc<RefCell<State>>) -> usize {
    state
        .borrow()
        .log
        .iter()
        .filter(|e| **e == ("services", 0x1812))
        .count()
}
/// Polls long enough for setup, information and verification to finish,
/// returning every profile event in order.
fn drain(b: &mut Backend<Mock>) -> Vec<Event> {
    let mut events = Vec::new();
    for _ in 0..150 {
        block_on(b.poll());
        while let Some(e) = b.next_profile_event() {
            events.push(e);
        }
    }
    events
}
fn admitted_from_saved(b: &mut Backend<Mock>) {
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { link: LINK, .. })
    ));
    match b.next_profile_event() {
        Some(Event::Connected {
            link: LINK,
            descriptors,
            max_output: 244,
            layout: None,
        }) => assert_eq!(descriptors.len(), 2),
        _ => panic!("expected admission from the saved layout"),
    }
}

#[test]
fn discovery_reports_the_complete_layout() {
    let (mut b, _) = setup(true);
    b.connect(LINK, PEER, false, None).unwrap();
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { .. })
    ));
    match b.next_profile_event() {
        Some(Event::Connected {
            layout: Some(found),
            ..
        }) => {
            assert_eq!(found, layout());
            assert!(found.valid(Transport::Ble));
        }
        _ => panic!("expected a discovered layout"),
    }
}

#[test]
fn saved_layout_admits_at_security_and_rewrites_cccds_before_writes() {
    let (mut b, state) = setup(true);
    state.borrow_mut().hold = true;
    b.connect(LINK, PEER, false, Some(&layout())).unwrap();
    // Input that arrives after encryption but before the host reports
    // security is delivered once the link is admitted.
    let token = state.borrow().token;
    state.borrow_mut().events.insert(
        1,
        Raw::Notification {
            token,
            handle: 15,
            data: Data::new(&[4]).unwrap(),
        },
    );
    admitted_from_saved(&mut b);
    assert!(
        matches!(b.next_profile_event(), Some(Event::Input(r)) if r.service == ServiceId(0) && r.report_id == 9 && r.payload() == [4])
    );
    assert_eq!(state.borrow().log, [("subscribe", 17)]);
    assert!(!b.can_write(LINK));

    notify(&state, 55, &[5]);
    block_on(b.poll());
    assert!(
        matches!(b.next_profile_event(), Some(Event::Input(r)) if r.service == ServiceId(1) && r.payload() == [5])
    );
    assert!(!b.can_write(LINK));
    release(&state);
    block_on(b.poll());
    assert_eq!(state.borrow().log.last(), Some(&("subscribe", 57)));
    assert!(!b.can_write(LINK));
    release(&state);
    block_on(b.poll());
    assert!(b.can_write(LINK));

    // Verification follows the information pass and finds the same layout.
    state.borrow_mut().hold = false;
    for event in drain(&mut b) {
        assert!(
            matches!(event, Event::Information { .. }),
            "an unchanged layout produces no events"
        );
    }
    let log = state.borrow().log.clone();
    let information = log.iter().position(|e| *e == ("services", 0x1800)).unwrap();
    let verify = log.iter().position(|e| *e == ("services", 0x1812)).unwrap();
    assert!(information < verify);
    assert_eq!(hid_requests(&state), 1);
    assert!(log[verify..].contains(&("subscribe", 17)));
    assert!(log[verify..].contains(&("subscribe", 57)));
    assert!(b.can_write(LINK));
    notify(&state, 15, &[6]);
    block_on(b.poll());
    assert!(matches!(b.next_profile_event(), Some(Event::Input(r)) if r.report_id == 9));
}

#[test]
fn verification_switches_to_a_changed_layout_before_later_input() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, Some(&changed())).unwrap();
    admitted_from_saved(&mut b);
    let mut events = Vec::new();
    for n in 0..150u8 {
        notify(&state, 15, &[n]);
        block_on(b.poll());
        while let Some(e) = b.next_profile_event() {
            events.push(e);
        }
    }
    let switches: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Event::Layout { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(switches.len(), 1);
    let switch = switches[0];
    match &events[switch] {
        Event::Layout {
            link,
            descriptors,
            layout: found,
        } => {
            assert_eq!(*link, LINK);
            assert_eq!(descriptors.len(), 2);
            assert_eq!(*found, layout());
        }
        _ => unreachable!(),
    }
    let ids = |events: &[Event]| -> Vec<u8> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Input(r) => Some(r.report_id),
                _ => None,
            })
            .collect()
    };
    let before = ids(&events[..switch]);
    let after = ids(&events[switch + 1..]);
    assert!(!before.is_empty() && before.iter().all(|&id| id == 7));
    assert!(!after.is_empty() && after.iter().all(|&id| id == 9));
    assert_eq!(before.len() + after.len(), 150);
    assert_eq!(hid_requests(&state), 1);
    assert!(b.can_write(LINK));
}

#[test]
fn failed_verification_keeps_the_saved_layout() {
    for case in 0..3 {
        let (mut b, state) = setup(true);
        match case {
            0 => state.borrow_mut().reject_services = true,
            1 => state.borrow_mut().services_error = Some(Error::Timeout),
            _ => state.borrow_mut().read_error = Some(12),
        }
        b.connect(LINK, PEER, false, Some(&changed())).unwrap();
        admitted_from_saved(&mut b);
        for event in drain(&mut b) {
            assert!(matches!(event, Event::Information { .. }));
        }
        assert_eq!(hid_requests(&state), 1);
        assert!(b.can_write(LINK));
        notify(&state, 15, &[1]);
        block_on(b.poll());
        assert!(matches!(b.next_profile_event(), Some(Event::Input(r)) if r.report_id == 7));
    }
}

#[test]
fn cccd_failure_starts_verification_at_once() {
    let (mut b, state) = setup(true);
    state.borrow_mut().fail_subscribes = 1;
    b.connect(LINK, PEER, false, Some(&layout())).unwrap();
    admitted_from_saved(&mut b);
    // The remaining CCCDs are still written before verification starts.
    assert_eq!(
        state.borrow().log[..3],
        [
            ("subscribe", 17),
            ("subscribe", 57),
            ("read_by_uuid", 0x2b2a)
        ]
    );
    for event in drain(&mut b) {
        assert!(matches!(event, Event::Information { .. }));
    }
    let log = state.borrow().log.clone();
    assert!(log[3..].contains(&("subscribe", 17)));
    assert!(log[3..].contains(&("subscribe", 57)));
    assert_eq!(hid_requests(&state), 1);
    assert!(b.can_write(LINK));
}

#[test]
fn unusable_saved_layouts_are_discovered_again() {
    let mut no_cccd = layout();
    no_cccd.reports[2].cccd = 0;
    let mut no_input = layout();
    no_input.reports.retain(|r| r.kind != ReportType::Input);
    let mut no_notify = layout();
    no_notify.reports[0].properties = 0x02;
    // Valid by shape but not a compilable report map.
    let bad_map = vec![0xa1, 0x01];
    assert!(Descriptor::from_slice(ServiceId(0), &bad_map).is_err());
    let mut uncompilable = layout();
    uncompilable.maps[1] = ReportMap(bad_map);
    for saved in [
        Layout::default(),
        no_cccd,
        no_input,
        no_notify,
        uncompilable,
    ] {
        let (mut b, state) = setup(true);
        b.connect(LINK, PEER, false, Some(&saved)).unwrap();
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Security { .. })
        ));
        assert!(
            matches!(b.next_profile_event(), Some(Event::Connected { layout: Some(found), .. }) if found == layout())
        );
        assert_eq!(
            state.borrow().log[..2],
            [("read_by_uuid", 0x2b2a), ("services", 0x1812)]
        );
        for event in drain(&mut b) {
            assert!(matches!(event, Event::Information { .. }));
        }
        assert_eq!(hid_requests(&state), 1);
    }
}

#[test]
fn early_notifications_wait_for_readiness_and_drop_the_oldest() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, None).unwrap();
    for n in 0..9 {
        notify(&state, 15, &[n]);
    }
    notify(&state, 99, &[9]);
    let mut events = Vec::new();
    for _ in 0..3 {
        block_on(b.poll());
        while let Some(e) = b.next_profile_event() {
            events.push(e);
        }
    }
    assert!(matches!(events[0], Event::Security { .. }));
    assert!(matches!(events[1], Event::Connected { .. }));
    let payloads: Vec<u8> = events[2..]
        .iter()
        .map(|e| match e {
            Event::Input(r) => r.payload()[0],
            _ => panic!("expected buffered input"),
        })
        .collect();
    assert_eq!(payloads, [2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn incoming_connection_uses_the_saved_layout() {
    let (mut b, state) = setup(true);
    b.reconnect(&[PEER]).unwrap();
    state.borrow_mut().events.push_back(Raw::Incoming {
        attempt: 42,
        peer: PEER,
    });
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Incoming { attempt: 42, .. })
    ));
    b.incoming(42, Some(LINK), Some(&layout())).unwrap();
    admitted_from_saved(&mut b);
    assert_eq!(hid_requests(&state), 0);
}

#[test]
fn verification_closes_a_link_whose_layout_is_unusable() {
    for malformed in [false, true] {
        let (mut b, state) = setup(true);
        b.connect(LINK, PEER, false, Some(&changed())).unwrap();
        admitted_from_saved(&mut b);
        if malformed {
            state.borrow_mut().malformed = true;
        } else {
            state.borrow_mut().bad_map = true;
        }
        let events: Vec<_> = drain(&mut b)
            .into_iter()
            .filter(|e| !matches!(e, Event::Information { .. }))
            .collect();
        assert_eq!(hid_requests(&state), 1);
        assert!(matches!(
            events[..],
            [Event::Disconnected {
                link: LINK,
                error: Some(Error::UnsupportedHid)
            }]
        ));
    }
}

#[test]
fn hid_writes_run_between_verification_steps() {
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, Some(&layout())).unwrap();
    admitted_from_saved(&mut b);
    let id = WriteId {
        link: LINK,
        sequence: 3,
    };
    let mut wrote = None;
    let mut written = false;
    for _ in 0..150 {
        block_on(b.poll());
        while let Some(e) = b.next_profile_event() {
            match e {
                Event::Written {
                    id: got,
                    result: Ok(()),
                } if got == id => written = true,
                Event::Information { .. } => {}
                _ => panic!("unexpected event"),
            }
        }
        if wrote.is_none() && hid_requests(&state) == 1 && b.can_write(LINK) {
            b.write(id, ServiceId(1), ReportType::Output, Some(9), &[2])
                .unwrap();
            wrote = Some(state.borrow().log.len());
        }
    }
    assert!(written);
    let log = state.borrow().log.clone();
    let wrote = wrote.unwrap();
    assert_eq!(log[wrote - 1], ("write", 61));
    // Verification resumes after the write and finishes its last report.
    assert!(log[wrote..].contains(&("subscribe", 57)));
    assert!(b.can_write(LINK));
}

const HASH: [u8; 16] = [7; 16];
fn hashed(hash: [u8; 16]) -> Layout {
    Layout {
        hash: Some(DatabaseHash(hash)),
        ..layout()
    }
}

#[test]
fn discovery_reads_the_database_hash() {
    for (hash, short) in [(None, false), (Some(HASH), false), (Some(HASH), true)] {
        let (mut b, state) = setup(true);
        state.borrow_mut().hash = hash;
        state.borrow_mut().short_hash = short;
        b.connect(LINK, PEER, false, None).unwrap();
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Security { .. })
        ));
        match b.next_profile_event() {
            Some(Event::Connected {
                layout: Some(found),
                ..
            }) => assert_eq!(
                found,
                Layout {
                    hash: hash.filter(|_| !short).map(DatabaseHash),
                    ..layout()
                }
            ),
            _ => panic!("expected a discovered layout"),
        }
        assert_eq!(state.borrow().log[0], ("read_by_uuid", 0x2b2a));
    }
}

#[test]
fn matching_hash_admits_the_saved_layout() {
    let (mut b, state) = setup(true);
    state.borrow_mut().hash = Some(HASH);
    b.connect(LINK, PEER, false, Some(&hashed(HASH))).unwrap();
    admitted_from_saved(&mut b);
    assert_eq!(
        state.borrow().log[..2],
        [("read_by_uuid", 0x2b2a), ("subscribe", 17)]
    );
    assert_eq!(hid_requests(&state), 0);
    // Verification reads the same hash and layout again.
    for event in drain(&mut b) {
        assert!(matches!(event, Event::Information { .. }));
    }
    assert_eq!(hid_requests(&state), 1);
    assert!(b.can_write(LINK));
}

#[test]
fn changed_or_unreadable_hash_discovers_before_admission() {
    for (device, error, short) in [
        (Some([8; 16]), None, false),
        (None, None, false),
        // Any ATT error response, such as Read Not Permitted.
        (Some(HASH), Some(Error::UnsupportedHid), false),
        // A value that is not 16 bytes is no hash.
        (Some(HASH), None, true),
    ] {
        let (mut b, state) = setup(true);
        state.borrow_mut().hash = device;
        state.borrow_mut().hash_error = error;
        state.borrow_mut().short_hash = short;
        b.connect(LINK, PEER, false, Some(&hashed(HASH))).unwrap();
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Security { .. })
        ));
        match b.next_profile_event() {
            Some(Event::Connected {
                layout: Some(found),
                ..
            }) => assert_eq!(
                found,
                Layout {
                    hash: if error.is_some() || short {
                        None
                    } else {
                        device.map(DatabaseHash)
                    },
                    ..layout()
                }
            ),
            _ => panic!("expected discovery before admission"),
        }
        assert_eq!(
            state.borrow().log[..2],
            [("read_by_uuid", 0x2b2a), ("services", 0x1812)]
        );
        // The discovered layout is not verified again.
        for event in drain(&mut b) {
            assert!(matches!(event, Event::Information { .. }));
        }
        assert_eq!(hid_requests(&state), 1);
    }
}

#[test]
fn verification_reports_a_changed_hash() {
    let (mut b, state) = setup(true);
    state.borrow_mut().hash = Some(HASH);
    // Without a saved hash the layout is admitted without a check.
    b.connect(LINK, PEER, false, Some(&layout())).unwrap();
    admitted_from_saved(&mut b);
    assert_eq!(state.borrow().log[0], ("subscribe", 17));
    let events: Vec<_> = drain(&mut b)
        .into_iter()
        .filter(|e| !matches!(e, Event::Information { .. }))
        .collect();
    match &events[..] {
        [Event::Layout { layout: found, .. }] => assert_eq!(*found, hashed(HASH)),
        _ => panic!("expected the new hash to be reported"),
    }
}

#[test]
fn hash_read_failures_that_reveal_nothing_never_drop_the_hash() {
    // Before admission from a saved layout, the link closes as a failed setup.
    for (error, rejected) in [(Error::Timeout, false), (Error::Busy, true)] {
        let (mut b, state) = setup(true);
        state.borrow_mut().hash = Some(HASH);
        state.borrow_mut().hash_error = (!rejected).then_some(error);
        state.borrow_mut().reject_hash = rejected;
        b.connect(LINK, PEER, false, Some(&hashed(HASH))).unwrap();
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Security { .. })
        ));
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Disconnected { link: LINK, error: Some(got) })
                if got == error
        ));
        assert_eq!(hid_requests(&state), 0);
    }

    // Discovery fails like any other setup failure.
    let (mut b, state) = setup(true);
    state.borrow_mut().hash_error = Some(Error::Timeout);
    b.connect(LINK, PEER, false, None).unwrap();
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { .. })
    ));
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected {
            error: Some(Error::Timeout),
            ..
        })
    ));

    // Verification is abandoned and the saved layout stays in use.
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, Some(&layout())).unwrap();
    admitted_from_saved(&mut b);
    state.borrow_mut().hash_error = Some(Error::Timeout);
    for event in drain(&mut b) {
        assert!(matches!(event, Event::Information { .. }));
    }
    assert!(state.borrow().log.contains(&("read_by_uuid", 0x2b2a)));
    assert_eq!(hid_requests(&state), 0);
    assert!(b.can_write(LINK));
    notify(&state, 15, &[1]);
    block_on(b.poll());
    assert!(matches!(b.next_profile_event(), Some(Event::Input(r)) if r.report_id == 9));
}

fn closed_deaf(b: &mut Backend<Mock>) {
    let events: Vec<_> = drain(b)
        .into_iter()
        .filter(|e| !matches!(e, Event::Information { .. }))
        .collect();
    assert!(
        matches!(
            events[..],
            [Event::Disconnected {
                link: LINK,
                error: Some(Error::ConnectionFailed)
            }]
        ),
        "a link without its subscriptions reconnects"
    );
}

#[test]
fn failed_subscription_on_discovered_handles_closes_the_link() {
    // During discovery before admission.
    let (mut b, state) = setup(true);
    state.borrow_mut().fail_subscribes = 1;
    b.connect(LINK, PEER, false, None).unwrap();
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Security { .. })
    ));
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected {
            error: Some(Error::ConnectionFailed),
            ..
        })
    ));

    // During verification, for an unchanged and for a changed layout.
    for saved in [layout(), changed()] {
        let (mut b, state) = setup(true);
        b.connect(LINK, PEER, false, Some(&saved)).unwrap();
        admitted_from_saved(&mut b);
        assert!(b.can_write(LINK));
        state.borrow_mut().fail_subscribes = 1;
        closed_deaf(&mut b);
        assert_eq!(hid_requests(&state), 1);
    }
}

#[test]
fn failed_saved_subscription_closes_when_verification_cannot_rewrite_it() {
    let (mut b, state) = setup(true);
    state.borrow_mut().fail_subscribes = 1;
    state.borrow_mut().services_error = Some(Error::Timeout);
    b.connect(LINK, PEER, false, Some(&layout())).unwrap();
    admitted_from_saved(&mut b);
    closed_deaf(&mut b);
}

#[test]
fn disabled_ble_declines_every_connection_until_enabled() {
    let (mut b, state) = setup(true);
    b.set_transport(Transport::Classic, true).unwrap();
    b.reconnect(&[PEER]).unwrap();
    b.scan(1, false, true).unwrap();
    b.connect(LINK, PEER, false, None).unwrap();
    connected(&mut b);
    state.borrow_mut().events.push_back(Raw::Incoming {
        attempt: 5,
        peer: PEER,
    });
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Incoming { attempt: 5, .. })
    ));

    b.set_transport(Transport::Ble, false).unwrap();
    assert_eq!(state.borrow().reconnect, [vec![PEER], vec![]]);
    assert_eq!(state.borrow().incoming, [(5, None)]);
    assert_eq!(state.borrow().log.last(), Some(&("scan", 0)));
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected { link: LINK, .. })
    ));
    assert_eq!(b.incoming(5, Some(LINK), None), Err(Error::NotConnected));

    // Connections, scans and offers are declined while disabled.
    let other = LinkId {
        slot: 1,
        generation: 2,
    };
    assert_eq!(
        b.connect(other, PEER, false, None),
        Err(Error::UnsupportedTransport)
    );
    assert_eq!(b.scan(1, false, true), Err(Error::UnsupportedTransport));
    b.scan(1, false, false).unwrap();
    b.reconnect(&[PEER]).unwrap();
    state.borrow_mut().events.push_back(Raw::Incoming {
        attempt: 6,
        peer: PEER,
    });
    block_on(b.poll());
    assert!(b.next_profile_event().is_none());
    assert_eq!(state.borrow().incoming, [(5, None), (6, None)]);
    assert_eq!(state.borrow().reconnect.len(), 2);

    // Re-enabling restores the application's latest accept list.
    b.set_transport(Transport::Ble, true).unwrap();
    assert_eq!(state.borrow().reconnect.last(), Some(&vec![PEER]));
    b.scan(1, false, true).unwrap();
    b.connect(other, PEER, false, None).unwrap();
}

#[test]
fn disabled_ble_survives_a_host_restart() {
    let (mut b, state) = setup(true);
    b.reconnect(&[PEER]).unwrap();
    b.set_transport(Transport::Ble, false).unwrap();
    state
        .borrow_mut()
        .events
        .extend([Raw::Restarting(Error::ConnectionFailed), Raw::Ready]);
    block_on(b.poll());
    assert!(matches!(b.next_profile_event(), Some(Event::Restarting(_))));
    assert!(matches!(b.next_profile_event(), Some(Event::Ready)));
    // The application reapplies its settings after Ready.
    b.set_transport(Transport::Ble, false).unwrap();
    b.reconnect(&[PEER]).unwrap();
    assert_eq!(state.borrow().reconnect, [vec![PEER], vec![]]);
    assert_eq!(
        b.connect(LINK, PEER, false, None),
        Err(Error::UnsupportedTransport)
    );

    // Enabled again after another restart, the accept list is restored.
    b.set_transport(Transport::Ble, true).unwrap();
    state
        .borrow_mut()
        .events
        .extend([Raw::Restarting(Error::ConnectionFailed), Raw::Ready]);
    block_on(b.poll());
    while b.next_profile_event().is_some() {}
    b.set_transport(Transport::Ble, true).unwrap();
    assert_eq!(
        state.borrow().reconnect,
        [vec![PEER], vec![], vec![PEER], vec![PEER]]
    );
    b.connect(LINK, PEER, false, None).unwrap();
}

fn scans(state: &Rc<RefCell<State>>) -> Vec<u16> {
    state
        .borrow()
        .log
        .iter()
        .filter(|e| e.0 == "scan")
        .map(|e| e.1)
        .collect()
}
#[test]
fn transport_changes_finish_once_the_host_queue_drains() {
    let (mut b, state) = setup(true);
    b.reconnect(&[PEER]).unwrap();
    b.scan(1, false, true).unwrap();
    b.connect(LINK, PEER, false, None).unwrap();
    connected(&mut b);
    state.borrow_mut().events.push_back(Raw::Incoming {
        attempt: 5,
        peer: PEER,
    });
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Incoming { attempt: 5, .. })
    ));

    // Every submission is refused, now and on a repeated call.
    state.borrow_mut().full = 10;
    b.set_transport(Transport::Ble, false).unwrap();
    b.set_transport(Transport::Ble, false).unwrap();
    assert_eq!(state.borrow().reconnect, [vec![PEER]]);
    assert!(state.borrow().incoming.is_empty());
    assert_eq!(scans(&state), [1]);
    assert_eq!(
        b.connect(
            LinkId {
                slot: 1,
                generation: 2
            },
            PEER,
            false,
            None
        ),
        Err(Error::UnsupportedTransport)
    );

    // The queue drains without a host restart; polling completes the change.
    // The retried disconnect is reported on the following poll.
    state.borrow_mut().full = 0;
    block_on(b.poll());
    block_on(b.poll());
    assert_eq!(state.borrow().reconnect, [vec![PEER], vec![]]);
    assert_eq!(state.borrow().incoming, [(5, None)]);
    assert_eq!(scans(&state), [1, 0]);
    let mut disconnected = false;
    while let Some(event) = b.next_profile_event() {
        disconnected |= matches!(event, Event::Disconnected { link: LINK, .. });
    }
    assert!(disconnected);
    let submissions = (
        state.borrow().reconnect.len(),
        state.borrow().incoming.len(),
    );
    for _ in 0..3 {
        block_on(b.poll());
    }
    assert_eq!(
        (
            state.borrow().reconnect.len(),
            state.borrow().incoming.len()
        ),
        submissions,
        "a finished change is not submitted again"
    );
    assert_eq!(scans(&state), [1, 0]);

    // Re-enabling restores the accept list once the queue drains.
    state.borrow_mut().full = 1;
    b.set_transport(Transport::Ble, true).unwrap();
    assert_eq!(state.borrow().reconnect.len(), 2);
    block_on(b.poll());
    assert_eq!(state.borrow().reconnect.last(), Some(&vec![PEER]));
    b.connect(LINK, PEER, false, None).unwrap();
}

#[test]
fn reconnect_failures_during_encryption_keep_the_host_classification() {
    for error in [Error::ConnectionFailed, Error::AuthenticationFailed] {
        let (mut b, state) = setup(true);
        b.connect(LINK, PEER, false, Some(&layout())).unwrap();
        {
            // The link drops before the host reports security.
            let mut s = state.borrow_mut();
            let token = s.token;
            s.events.retain(|e| !matches!(e, Raw::Security { .. }));
            s.events.push_back(Raw::Disconnected {
                token,
                error: Some(error),
            });
        }
        block_on(b.poll());
        assert!(matches!(
            b.next_profile_event(),
            Some(Event::Disconnected { link: LINK, error: Some(got) }) if got == error
        ));
        assert!(state.borrow().forgotten.is_empty());
    }

    // Security the host reports without bonded encryption rejects the keys.
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, None).unwrap();
    {
        let mut s = state.borrow_mut();
        for event in s.events.iter_mut() {
            if let Raw::Security { security, .. } = event {
                security.encrypted = Some(false);
            }
        }
    }
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected {
            error: Some(Error::AuthenticationFailed),
            ..
        })
    ));
}

#[test]
fn a_disconnect_the_host_cannot_queue_is_retried() {
    // An explicit disconnect.
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, None).unwrap();
    connected(&mut b);
    state.borrow_mut().full = 2;
    b.disconnect(LINK);
    assert!(!b.can_write(LINK));
    notify(&state, 15, &[1]);
    block_on(b.poll());
    // The closing link admits nothing while its request waits.
    assert!(b.next_profile_event().is_none());
    block_on(b.poll());
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected {
            link: LINK,
            error: None
        })
    ));
    assert!(b.next_profile_event().is_none());

    // A link the profile closes after an error.
    let (mut b, state) = setup(true);
    b.connect(LINK, PEER, false, None).unwrap();
    state.borrow_mut().full = 1;
    for event in state.borrow_mut().events.iter_mut() {
        if let Raw::Security { security, .. } = event {
            security.encrypted = Some(false);
        }
    }
    block_on(b.poll());
    assert!(b.next_profile_event().is_none());
    block_on(b.poll());
    assert!(matches!(
        b.next_profile_event(),
        Some(Event::Disconnected {
            error: Some(Error::AuthenticationFailed),
            ..
        })
    ));
}
