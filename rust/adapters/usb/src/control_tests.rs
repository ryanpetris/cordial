use super::*;
use core::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
};
use embassy_usb::driver::*;
use std::{collections::VecDeque, rc::Rc, vec::Vec};

#[derive(Default)]
struct Traffic {
    setup: VecDeque<[u8; 8]>,
    events: VecDeque<Event>,
    allocations: Vec<EndpointInfo>,
    enabled: Vec<u8>,
    halted: Vec<u8>,
    data1: Vec<u8>,
    active: Vec<u8>,
    enablements: Vec<(u8, bool)>,
    halt_events: Vec<(u8, &'static str)>,
    reconfiguration: bool,
    direct_enable: bool,
    epochs: [u32; 32],
    accessed: Vec<u8>,
    response: Vec<u8>,
    payload: Vec<u8>,
    rejected: usize,
    accepted: usize,
    writes: Vec<(u8, Vec<u8>)>,
    reads: VecDeque<Vec<u8>>,
    block_write: bool,
    otg: Option<OutBuffers>,
    reset_pending: bool,
    reset_hardware_pending: bool,
    reset_after_bus_poll: bool,
    reset_hardware_after_bus_poll: bool,
    reset_during_read: bool,
    reset_hardware_during_read: bool,
}

impl Traffic {
    fn interrupt_reset(&mut self) {
        self.reset_hardware_pending = true;
        if self.reset_pending {
            return;
        }
        if let Some(otg) = &mut self.otg {
            otg.dirty.fill(true);
        }
        for epoch in &mut self.epochs {
            *epoch += 1;
        }
        self.reset_pending = true;
    }
}

/// Models the OTG software slot independently of its receive registers.
#[derive(Default)]
struct OutBuffers {
    packets: [Option<Vec<u8>>; 6],
    enabled: [bool; 6],
    dirty: [bool; 6],
    nak: [bool; 6],
    pktcnt: [u8; 6],
    global_nak: bool,
    drain_count: usize,
    lost: usize,
    retries: usize,
    arrival: Option<(usize, Vec<u8>)>,
    error: Option<EndpointError>,
}
impl OutBuffers {
    fn send(&mut self, index: usize, bytes: &[u8]) -> bool {
        if !self.enabled[index] || self.global_nak || self.nak[index] || self.pktcnt[index] == 0 {
            self.retries += 1;
            return false;
        }
        if self.packets[index].is_some() {
            self.lost += 1;
        } else {
            self.packets[index] = Some(bytes.to_vec());
        }
        self.pktcnt[index] = 0;
        self.nak[index] = true;
        true
    }
    fn boundary(&mut self) {
        if let Some((index, bytes)) = self.arrival.take()
            && !self.send(index, &bytes)
        {
            self.arrival = Some((index, bytes));
        }
    }
}
#[derive(Clone, Default)]
struct Host(Rc<RefCell<Traffic>>);
struct MockDriver(Host);
struct MockEndpoint(Host, EndpointInfo);

impl MockDriver {
    fn allocate(
        &mut self,
        direction: Direction,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> MockEndpoint {
        let mut traffic = self.0.0.borrow_mut();
        let number = traffic
            .allocations
            .iter()
            .filter(|e| e.addr.direction() == direction)
            .count()
            + if direction == Direction::In { 1 } else { 2 };
        let info = EndpointInfo {
            addr: address.unwrap_or_else(|| EndpointAddress::from_parts(number, direction)),
            ep_type: kind,
            max_packet_size: size,
            interval_ms: interval,
        };
        traffic.allocations.push(info);
        MockEndpoint(self.0.clone(), info)
    }
}
impl<'d> Driver<'d> for MockDriver {
    type EndpointIn = MockEndpoint;
    type EndpointOut = MockEndpoint;
    type ControlPipe = Host;
    type Bus = Host;
    fn alloc_endpoint_in(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<MockEndpoint, EndpointAllocError> {
        Ok(self.allocate(Direction::In, kind, address, size, interval))
    }
    fn alloc_endpoint_out(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<MockEndpoint, EndpointAllocError> {
        Ok(self.allocate(Direction::Out, kind, address, size, interval))
    }
    fn start(self, _: u16) -> (Host, Host) {
        (self.0.clone(), self.0)
    }
}
impl Endpoint for MockEndpoint {
    fn info(&self) -> &EndpointInfo {
        &self.1
    }
    async fn wait_enabled(&mut self) {
        poll_fn(|_| {
            if self.0.0.borrow().enabled.contains(&self.1.addr.into()) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}
impl EndpointIn for MockEndpoint {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), EndpointError> {
        poll_fn(|_| {
            if self.0.0.borrow().block_write {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        let mut traffic = self.0.0.borrow_mut();
        if traffic.reconfiguration {
            if !traffic.enabled.contains(&self.1.addr.into()) {
                return Err(EndpointError::Disabled);
            }
            assert!(!traffic.active.contains(&self.1.addr.into()));
            traffic.active.push(self.1.addr.into());
        }
        traffic.writes.push((self.1.addr.into(), bytes.to_vec()));
        Ok(())
    }
}
impl EndpointOut for MockEndpoint {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, EndpointError> {
        if self.0.0.borrow().otg.is_some() {
            return poll_fn(|_| {
                let mut traffic = self.0.0.borrow_mut();
                // The pinned driver checks USBAEP before looking at out_size.
                if !traffic.enabled.contains(&self.1.addr.into()) {
                    return Poll::Ready(Err(EndpointError::Disabled));
                }
                let otg = traffic.otg.as_mut().unwrap();
                if let Some(error) = otg.error.take() {
                    return Poll::Ready(Err(error));
                }
                let index = self.1.addr.index();
                let Some(packet) = otg.packets[index].as_ref() else {
                    return Poll::Pending;
                };
                if packet.len() > bytes.len() {
                    return Poll::Ready(Err(EndpointError::BufferOverflow));
                }
                let length = packet.len();
                bytes[..length].copy_from_slice(packet);
                otg.packets[index] = None;
                otg.pktcnt[index] = 1;
                otg.nak[index] = false;
                otg.boundary();
                if core::mem::take(&mut traffic.reset_hardware_during_read) {
                    traffic.reset_hardware_pending = true;
                }
                if core::mem::take(&mut traffic.reset_during_read) {
                    traffic.interrupt_reset();
                }
                Poll::Ready(Ok(length))
            })
            .await;
        }
        let packet = poll_fn(|_| {
            if self.1.max_packet_size != 32 {
                return Poll::Pending;
            }
            self.0
                .0
                .borrow_mut()
                .reads
                .pop_front()
                .map_or(Poll::Pending, Poll::Ready)
        })
        .await;
        bytes[..packet.len()].copy_from_slice(&packet);
        Ok(packet.len())
    }
}
impl Host {
    fn access(&self, address: EndpointAddress) {
        let mut traffic = self.0.borrow_mut();
        // Match the OTG storage boundary and reject accesses to unallocated directions.
        assert!(address.index() < 6);
        assert!(address.index() == 0 || traffic.allocations.iter().any(|e| e.addr == address));
        traffic.accessed.push(address.into());
    }
}
impl Bus for Host {
    async fn enable(&mut self) {}
    async fn disable(&mut self) {
        let mut traffic = self.0.borrow_mut();
        traffic.enabled.clear();
        if let Some(otg) = &mut traffic.otg {
            otg.enabled.fill(false);
        }
    }
    async fn poll(&mut self) -> Event {
        let event = poll_fn(|_| {
            let mut traffic = self.0.borrow_mut();
            if core::mem::take(&mut traffic.reset_hardware_after_bus_poll) {
                traffic.reset_hardware_pending = true;
                return Poll::Pending;
            }
            if core::mem::take(&mut traffic.reset_after_bus_poll) {
                traffic.interrupt_reset();
                return Poll::Pending;
            }
            traffic
                .events
                .pop_front()
                .map_or(Poll::Pending, Poll::Ready)
        })
        .await;
        if matches!(event, Event::Reset | Event::PowerRemoved) {
            let mut traffic = self.0.borrow_mut();
            if matches!(event, Event::Reset) {
                traffic.reset_hardware_pending = false;
            }
            traffic.enabled.clear();
            traffic.halted.clear();
            traffic.data1.clear();
            traffic.active.clear();
            if let Some(otg) = &mut traffic.otg {
                otg.enabled.fill(false);
                otg.pktcnt.fill(1);
                otg.nak.fill(false);
            }
        }
        event
    }
    fn endpoint_set_enabled(&mut self, address: EndpointAddress, enabled: bool) {
        let mut traffic = self.0.borrow_mut();
        traffic.enablements.push((address.into(), enabled));
        if traffic.direct_enable {
            // RP's driver resets buffer control itself, including queued data.
            traffic.halted.retain(|e| *e != u8::from(address));
            traffic.data1.retain(|e| *e != u8::from(address));
        } else if traffic.reconfiguration {
            assert!(!traffic.active.contains(&address.into()));
        }
        traffic.enabled.retain(|e| *e != u8::from(address));
        traffic.active.retain(|e| *e != u8::from(address));
        if address.is_out()
            && let Some(otg) = &mut traffic.otg
        {
            otg.enabled[address.index()] = enabled;
        }
        if enabled {
            if traffic.reconfiguration {
                assert!(!traffic.halted.contains(&address.into()));
                assert!(!traffic.data1.contains(&address.into()));
            }
            traffic.enabled.push(address.into());
            if address.is_out() {
                traffic.active.push(address.into());
                if let Some(otg) = &mut traffic.otg {
                    otg.pktcnt[address.index()] = 1;
                    otg.nak[address.index()] = false;
                    otg.boundary();
                }
            }
        }
    }
    fn endpoint_set_stalled(&mut self, address: EndpointAddress, stalled: bool) {
        self.access(address);
        let mut traffic = self.0.borrow_mut();
        traffic.halted.retain(|e| *e != u8::from(address));
        if stalled {
            traffic.halted.push(address.into());
        }
    }
    fn endpoint_is_stalled(&mut self, address: EndpointAddress) -> bool {
        self.access(address);
        self.0.borrow().halted.contains(&address.into())
    }
    async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
        Err(Unsupported)
    }
}
impl ControlPipe for Host {
    fn max_packet_size(&self) -> usize {
        64
    }
    async fn setup(&mut self) -> [u8; 8] {
        poll_fn(|_| {
            self.0
                .borrow_mut()
                .setup
                .pop_front()
                .map_or(Poll::Pending, Poll::Ready)
        })
        .await
    }
    async fn data_out(
        &mut self,
        bytes: &mut [u8],
        _: bool,
        _: bool,
    ) -> Result<usize, EndpointError> {
        let payload = core::mem::take(&mut self.0.borrow_mut().payload);
        bytes[..payload.len()].copy_from_slice(&payload);
        Ok(payload.len())
    }
    async fn data_in(&mut self, bytes: &[u8], _: bool, last: bool) -> Result<(), EndpointError> {
        self.0.borrow_mut().response.extend_from_slice(bytes);
        self.0.borrow_mut().accepted += usize::from(last);
        Ok(())
    }
    async fn accept(&mut self) {
        self.0.borrow_mut().accepted += 1;
    }
    async fn reject(&mut self) {
        self.0.borrow_mut().rejected += 1;
    }
    async fn accept_set_address(&mut self, _: u8) {
        self.accept().await;
    }
}
fn poll<F: Future>(future: Pin<&mut F>) {
    assert!(
        future
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
}
fn exchange<F: Future>(
    host: &Host,
    mut running: Pin<&mut F>,
    request: [u8; 8],
    rejected: bool,
) -> Vec<u8> {
    {
        let mut traffic = host.0.borrow_mut();
        traffic.response.clear();
        traffic.rejected = 0;
        traffic.accepted = 0;
        traffic.setup.push_back(request);
    }
    // The USB task waits for the CDC worker to observe each session boundary before ACK.
    for _ in 0..3 {
        poll(running.as_mut());
    }
    let traffic = host.0.borrow();
    assert_eq!(
        traffic.rejected,
        usize::from(rejected),
        "request {request:?}"
    );
    assert_eq!(
        traffic.accepted,
        usize::from(!rejected),
        "request {request:?}"
    );
    traffic.response.clone()
}
fn configured<F: Future>(host: &Host, mut running: Pin<&mut F>) {
    exchange(host, running.as_mut(), [0, 5, 1, 0, 0, 0, 0, 0], false);
    exchange(host, running, [0, 9, 1, 0, 0, 0, 0, 0], false);
}

#[derive(Clone, Copy)]
struct Reconfiguration<'a>(&'a Host);
impl completion::Completion for Reconfiguration<'_> {
    fn disconnect(&self, _: bool) {}
    fn epoch(&self, address: EndpointAddress) -> u32 {
        self.0.0.borrow().epochs[address.index() + 16 * usize::from(address.is_in())]
    }
    fn reset_pending(&self) -> bool {
        let traffic = self.0.0.borrow();
        traffic.reset_pending || traffic.reset_hardware_pending
    }
    fn cancel_out_read(&self, address: EndpointAddress) {
        self.0.0.borrow_mut().otg.as_mut().unwrap().nak[address.index()] = true;
    }
    fn stalled(&self, address: EndpointAddress) -> bool {
        self.0.0.borrow().halted.contains(&address.into())
    }
    async fn wait(&self, address: EndpointAddress) -> Result<(), EndpointError> {
        poll_fn(|_| {
            let traffic = self.0.0.borrow();
            if !traffic.enabled.contains(&address.into()) {
                Poll::Ready(Err(EndpointError::Disabled))
            } else if traffic.active.contains(&address.into()) {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
    fn endpoint_set_enabled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, enabled: bool) {
        self.0.0.borrow_mut().epochs[address.index() + 16 * usize::from(address.is_in())] += 1;
        if address.is_out()
            && let Some(otg) = &mut self.0.0.borrow_mut().otg
        {
            assert!(!otg.global_nak);
            otg.dirty[address.index()] = true;
            otg.global_nak = true;
            otg.boundary();
        }
        if self.0.0.borrow().direct_enable {
            bus.endpoint_set_enabled(address, enabled);
            return;
        }
        completion::reconfigure_endpoint(
            bus,
            address,
            enabled,
            || {
                self.0
                    .0
                    .borrow_mut()
                    .active
                    .retain(|e| *e != u8::from(address))
            },
            |bus| {
                let traffic = self.0.0.borrow();
                assert!(!traffic.enabled.contains(&address.into()));
                assert!(!traffic.active.contains(&address.into()));
                drop(traffic);
                bus.endpoint_set_stalled(address, false);
                self.0
                    .0
                    .borrow_mut()
                    .data1
                    .retain(|e| *e != u8::from(address));
            },
        );
        if address.is_out()
            && let Some(otg) = &mut self.0.0.borrow_mut().otg
        {
            otg.nak[address.index()] = true;
            otg.boundary();
            otg.global_nak = false;
            otg.boundary();
        }
    }
    fn begin_out_drain(&self, address: EndpointAddress) -> bool {
        let mut traffic = self.0.0.borrow_mut();
        let Some(otg) = &mut traffic.otg else {
            return false;
        };
        if !otg.dirty[address.index()] {
            return false;
        }
        assert!(!otg.global_nak);
        otg.global_nak = true;
        otg.drain_count += 1;
        otg.boundary();
        traffic.active.retain(|e| *e != u8::from(address));
        true
    }
    fn finish_out_drain(
        &self,
        info: &EndpointInfo,
        mut result: Result<(), EndpointError>,
    ) -> Result<(), EndpointError> {
        let mut traffic = self.0.0.borrow_mut();
        let index = info.addr.index();
        if traffic.reset_pending || traffic.reset_hardware_pending {
            result = Err(EndpointError::Disabled);
        }
        let otg = traffic.otg.as_mut().unwrap();
        assert!(otg.global_nak);
        if result.is_ok() {
            assert!(otg.packets[index].is_none());
            otg.pktcnt[index] = 1;
            otg.nak[index] = false;
            otg.dirty[index] = false;
        } else {
            otg.nak[index] = true;
            assert!(otg.dirty[index]);
        }
        otg.boundary();
        otg.global_nak = false;
        otg.boundary();
        if result.is_ok() {
            traffic.data1.retain(|e| *e != u8::from(info.addr));
            traffic.active.push(info.addr.into());
        }
        result
    }
    fn endpoint_set_stalled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, stalled: bool) {
        bus.endpoint_set_stalled(address, stalled);
        let mut traffic = self.0.0.borrow_mut();
        if !stalled {
            traffic.data1.retain(|e| *e != u8::from(address));
        }
        if address.is_out()
            && let Some(otg) = &mut traffic.otg
            && otg.dirty[address.index()]
        {
            otg.nak[address.index()] = true;
        }
    }
    fn reset(&self) {
        let mut traffic = self.0.0.borrow_mut();
        if core::mem::take(&mut traffic.reset_pending) {
            return;
        }
        if let Some(otg) = &mut traffic.otg {
            otg.dirty.fill(true);
            for epoch in &mut traffic.epochs {
                *epoch += 1;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct OutHaltPlatform<'a> {
    host: &'a Host,
    halts: &'a [completion::OutHalt; 32],
}
impl OutHaltPlatform<'_> {
    fn halt(&self, address: EndpointAddress) -> &completion::OutHalt {
        &self.halts[address.index() + 16 * usize::from(address.is_in())]
    }
}
impl completion::Completion for OutHaltPlatform<'_> {
    fn disconnect(&self, _: bool) {}
    fn epoch(&self, address: EndpointAddress) -> u32 {
        completion::Completion::epoch(&Reconfiguration(self.host), address)
    }
    fn stalled(&self, address: EndpointAddress) -> bool {
        completion::Completion::stalled(&Reconfiguration(self.host), address)
    }
    async fn wait(&self, address: EndpointAddress) -> Result<(), EndpointError> {
        completion::Completion::wait(&Reconfiguration(self.host), address).await
    }
    fn endpoint_set_enabled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, enabled: bool) {
        self.halt(address).reset();
        completion::Completion::endpoint_set_enabled(
            &Reconfiguration(self.host),
            bus,
            address,
            enabled,
        );
    }
    fn endpoint_set_stalled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, stalled: bool) {
        if address.index() == 0 || address.is_in() {
            bus.endpoint_set_stalled(address, stalled);
            return;
        }
        completion::set_out_halt(
            self.halt(address),
            bus,
            address,
            stalled,
            || {
                let mut traffic = self.host.0.borrow_mut();
                let active = traffic.active.contains(&address.into());
                let enabled = traffic.enabled.contains(&address.into());
                if enabled {
                    traffic.halt_events.push((address.into(), "global NAK"));
                }
                if active {
                    traffic
                        .active
                        .retain(|candidate| *candidate != address.into());
                    traffic
                        .halt_events
                        .push((address.into(), "disable and stall"));
                } else {
                    traffic.halt_events.push((address.into(), "stall"));
                }
                if !traffic.halted.contains(&address.into()) {
                    traffic.halted.push(address.into());
                }
                if enabled {
                    traffic
                        .halt_events
                        .push((address.into(), "clear global NAK"));
                }
                active
            },
            || {
                let mut traffic = self.host.0.borrow_mut();
                traffic
                    .halt_events
                    .push((address.into(), "clear and DATA0"));
                traffic
                    .halted
                    .retain(|candidate| *candidate != address.into());
                traffic
                    .data1
                    .retain(|candidate| *candidate != address.into());
            },
        );
    }
    fn reset(&self) {
        for halt in self.halts {
            halt.reset();
        }
    }
}

#[test]
fn out_halt_quiesces_active_receive_and_rearms_only_that_receive() {
    let host = Host::default();
    host.0.borrow_mut().reconfiguration = true;
    let halts = [const { completion::OutHalt::new() }; 32];
    let platform = OutHaltPlatform {
        host: &host,
        halts: &halts,
    };
    let mut config = [0; 256];
    let mut bos = [0; 64];
    let mut msos = [0; 64];
    let mut control = [0; 64];
    let mut builder = Builder::new(
        completion::CompleteDriver {
            driver: MockDriver(host.clone()),
            completion: platform,
        },
        Config::new(0x1209, 0xc0d1),
        &mut config,
        &mut bos,
        &mut msos,
        &mut control,
    );
    let mut first = builder
        .function(0xff, 0, 0)
        .interface()
        .alt_setting(0xff, 0, 0, None)
        .endpoint_interrupt_out(None, 32, 1);
    let second = builder
        .function(0xff, 0, 0)
        .interface()
        .alt_setting(0xff, 0, 0, None)
        .endpoint_interrupt_out(None, 32, 1);
    let first_address = u8::from(first.info().addr);
    let second_address = u8::from(second.info().addr);
    let mut device = builder.build();
    let mut running = pin!(device.run());
    configured(&host, running.as_mut());
    host.0.borrow_mut().halt_events.clear();

    exchange(
        &host,
        running.as_mut(),
        [2, 3, 0, 0, first_address, 0, 0, 0],
        false,
    );
    assert_eq!(
        host.0.borrow().halt_events,
        [
            (first_address, "global NAK"),
            (first_address, "disable and stall"),
            (first_address, "clear global NAK"),
        ]
    );
    assert!(!host.0.borrow().active.contains(&first_address));
    assert!(host.0.borrow().active.contains(&second_address));

    // Repeating SET_FEATURE does not disable the endpoint again.
    exchange(
        &host,
        running.as_mut(),
        [2, 3, 0, 0, first_address, 0, 0, 0],
        false,
    );
    assert_eq!(host.0.borrow().halt_events.len(), 3);
    let enables = host.0.borrow().enablements.len();
    exchange(
        &host,
        running.as_mut(),
        [2, 1, 0, 0, first_address, 0, 0, 0],
        false,
    );
    assert_eq!(host.0.borrow().enablements.len(), enables + 1);
    assert_eq!(
        host.0.borrow().enablements.last(),
        Some(&(first_address, true))
    );
    assert!(host.0.borrow().active.contains(&first_address));
    assert!(host.0.borrow().active.contains(&second_address));

    // A repeated clear resets DATA0 without re-priming an active receive.
    host.0.borrow_mut().data1.push(first_address);
    let enables = host.0.borrow().enablements.len();
    exchange(
        &host,
        running.as_mut(),
        [2, 1, 0, 0, first_address, 0, 0, 0],
        false,
    );
    assert_eq!(host.0.borrow().enablements.len(), enables);
    assert!(!host.0.borrow().data1.contains(&first_address));

    // A completed packet remains in Embassy's buffer. HALT does not prime over it.
    host.0
        .borrow_mut()
        .active
        .retain(|address| *address != first_address);
    host.0.borrow_mut().reads.push_back(vec![7; 32]);
    host.0.borrow_mut().halt_events.clear();
    exchange(
        &host,
        running.as_mut(),
        [2, 3, 0, 0, first_address, 0, 0, 0],
        false,
    );
    assert_eq!(
        host.0.borrow().halt_events,
        [
            (first_address, "global NAK"),
            (first_address, "stall"),
            (first_address, "clear global NAK"),
        ]
    );
    let enables = host.0.borrow().enablements.len();
    exchange(
        &host,
        running.as_mut(),
        [2, 1, 0, 0, first_address, 0, 0, 0],
        false,
    );
    assert_eq!(host.0.borrow().enablements.len(), enables);
    let mut packet = [0; 32];
    assert_eq!(block_on(first.read(&mut packet)), Ok(32));
    assert_eq!(packet, [7; 32]);
    assert!(host.0.borrow().active.contains(&second_address));

    // Reconfiguration clears stopped-receive state before later clear requests.
    host.0.borrow_mut().active.push(first_address);
    exchange(
        &host,
        running.as_mut(),
        [2, 3, 0, 0, first_address, 0, 0, 0],
        false,
    );
    exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
    let enables = host.0.borrow().enablements.len();
    exchange(
        &host,
        running.as_mut(),
        [2, 1, 0, 0, first_address, 0, 0, 0],
        false,
    );
    assert_eq!(host.0.borrow().enablements.len(), enables);

    // Reset clears stopped-receive state before any later clear request.
    host.0
        .borrow_mut()
        .active
        .retain(|address| *address != first_address);
    completion::set_out_halt(
        platform.halt(first.info().addr),
        &mut host.clone(),
        first.info().addr,
        true,
        || true,
        || {},
    );
    completion::Completion::reset(&platform);
    let enables = host.0.borrow().enablements.len();
    completion::set_out_halt(
        platform.halt(first.info().addr),
        &mut host.clone(),
        first.info().addr,
        false,
        || false,
        || {},
    );
    assert_eq!(host.0.borrow().enablements.len(), enables);
}

#[test]
fn configuration_and_interface_selection_reset_halt_and_data0_before_priming() {
    let host = Host::default();
    host.0.borrow_mut().reconfiguration = true;
    let mut config = [0; 256];
    let mut bos = [0; 64];
    let mut msos = [0; 64];
    let mut control = [0; 64];
    let mut builder = Builder::new(
        completion::CompleteDriver {
            driver: MockDriver(host.clone()),
            completion: Reconfiguration(&host),
        },
        Config::new(0x1209, 0xc0d1),
        &mut config,
        &mut bos,
        &mut msos,
        &mut control,
    );
    let mut endpoints = Vec::new();
    for kind in [EndpointType::Bulk, EndpointType::Interrupt] {
        let mut function = builder.function(0xff, 0, 0);
        let mut interface = function.interface();
        let mut alt = interface.alt_setting(0xff, 0, 0, None);
        for direction in [Direction::In, Direction::Out] {
            let info = match direction {
                Direction::In => *alt.alloc_endpoint_in(kind, None, 8, 1).info(),
                Direction::Out => *alt.alloc_endpoint_out(kind, None, 8, 1).info(),
            };
            alt.endpoint_descriptor(
                &info,
                embassy_usb::descriptor::SynchronizationType::NoSynchronization,
                embassy_usb::descriptor::UsageType::DataEndpoint,
                &[],
            );
            endpoints.push(u8::from(info.addr));
        }
        interface.alt_setting(0xff, 0, 0, None);
    }
    let mut device = builder.build();
    let mut running = pin!(device.run());
    configured(&host, running.as_mut());
    for request in [
        [0, 9, 1, 0, 0, 0, 0, 0],
        [1, 11, 0, 0, 0, 0, 0, 0],
        [1, 11, 0, 0, 1, 0, 0, 0],
    ] {
        // Repeated selections reset both halted and unhalted DATA1 endpoints.
        for halted in [true, false] {
            exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
            for &address in &endpoints {
                exchange(
                    &host,
                    running.as_mut(),
                    [2, if halted { 3 } else { 1 }, 0, 0, address, 0, 0, 0],
                    false,
                );
            }
            host.0.borrow_mut().data1 = endpoints.clone();
            host.0.borrow_mut().active = endpoints.clone();
            exchange(&host, running.as_mut(), request, false);
            for (i, &address) in endpoints.iter().enumerate() {
                let selected = request[1] == 9 || i / 2 == usize::from(request[4]);
                let expected_halt = halted && !selected;
                assert_eq!(
                    exchange(
                        &host,
                        running.as_mut(),
                        [0x82, 0, 0, 0, address, 0, 2, 0],
                        false
                    ),
                    [u8::from(expected_halt), 0],
                );
                let traffic = host.0.borrow();
                assert!(traffic.enabled.contains(&address));
                assert_eq!(traffic.data1.contains(&address), !selected);
                assert_eq!(
                    traffic.active.contains(&address),
                    !selected || address & 0x80 == 0
                );
            }
        }
    }
    // An empty alternate setting disables only its interface, without priming it.
    exchange(&host, running.as_mut(), [1, 11, 1, 0, 0, 0, 0, 0], false);
    assert_eq!(host.0.borrow().enabled, endpoints[2..]);
    for address in &endpoints[..2] {
        assert!(!host.0.borrow().active.contains(address));
    }
    exchange(&host, running.as_mut(), [1, 11, 0, 0, 0, 0, 0, 0], false);
    assert_eq!(host.0.borrow().enabled.len(), endpoints.len());
    exchange(&host, running.as_mut(), [0, 9, 0, 0, 0, 0, 0, 0], false);
    assert!(host.0.borrow().enabled.is_empty());
    assert!(host.0.borrow().active.is_empty());
}

#[test]
fn reconfiguration_cancels_hid_chunks_and_dropped_write_cleanup() {
    for direct_enable in [false, true] {
        for request in [[1, 11, 0, 0, 0, 0, 0, 0], [0, 9, 1, 0, 0, 0, 0, 0]] {
            for halted in [false, true] {
                for blocked in [false, true] {
                    let host = Host::default();
                    host.0.borrow_mut().reconfiguration = true;
                    host.0.borrow_mut().direct_enable = direct_enable;
                    let mut config = [0; 256];
                    let mut bos = [0; 64];
                    let mut msos = [0; 64];
                    let mut control = [0; 64];
                    let mut state = hid::State::new();
                    let mut builder = Builder::new(
                        completion::CompleteDriver {
                            driver: MockDriver(host.clone()),
                            completion: Reconfiguration(&host),
                        },
                        Config::new(0x1209, 0xc0d1),
                        &mut config,
                        &mut bos,
                        &mut msos,
                        &mut control,
                    );
                    let mut writer = hid::HidWriter::<_, 69>::new(
                        &mut builder,
                        &mut state,
                        hid::Config {
                            report_descriptor: descriptor::REPORT_DESCRIPTOR,
                            request_handler: None,
                            poll_ms: 1,
                            max_packet_size: 64,
                            hid_subclass: hid::HidSubclass::No,
                            hid_boot_protocol: hid::HidBootProtocol::None,
                        },
                    );
                    let mut device = builder.build();
                    let mut running = pin!(device.run());
                    configured(&host, running.as_mut());
                    let mut report = [42; 69];
                    report[0] = 4;
                    let mut cx = Context::from_waker(Waker::noop());
                    host.0.borrow_mut().block_write = blocked;
                    {
                        let mut write = pin!(writer.write(&report));
                        poll(write.as_mut());
                        assert_eq!(writes(&host, 0x81).len(), usize::from(!blocked));
                        if halted {
                            exchange(&host, running.as_mut(), [2, 3, 0, 0, 0x81, 0, 0, 0], false);
                        }
                        exchange(&host, running.as_mut(), request, false);
                        host.0.borrow_mut().block_write = false;
                        assert_eq!(
                            write.as_mut().poll(&mut cx),
                            Poll::Ready(Err(EndpointError::Disabled))
                        );
                        assert_eq!(writes(&host, 0x81).len(), usize::from(!blocked));
                    }
                    // A dropped write still owns a FIFO packet until it completes or
                    // reconfiguration cancels it. Cancellation also interrupts cleanup.
                    {
                        let mut dropped = pin!(writer.write(&report));
                        poll(dropped.as_mut());
                    }
                    let submitted = writes(&host, 0x81).len();
                    {
                        let mut replacement = pin!(writer.write(&report));
                        poll(replacement.as_mut());
                        assert_eq!(writes(&host, 0x81).len(), submitted);
                        exchange(&host, running.as_mut(), request, false);
                        assert_eq!(
                            replacement.as_mut().poll(&mut cx),
                            Poll::Ready(Err(EndpointError::Disabled))
                        );
                    }
                    // The next report starts at its first byte and completes normally.
                    {
                        let mut next = pin!(writer.write(&report));
                        poll(next.as_mut());
                        assert_eq!(writes(&host, 0x81)[submitted], report[..64]);
                        host.0.borrow_mut().active.clear();
                        poll(next.as_mut());
                        assert_eq!(writes(&host, 0x81)[submitted + 1], report[64..]);
                        host.0.borrow_mut().active.clear();
                        assert_eq!(next.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
                    }
                    assert_eq!(writes(&host, 0x81).len(), submitted + 2);
                    // Cleanup of a write dropped before reconfiguration cannot wait
                    // on the cancelled packet or mistake the new packet for the old one.
                    {
                        let mut dropped = pin!(writer.write(&[1]));
                        poll(dropped.as_mut());
                    }
                    exchange(&host, running.as_mut(), request, false);
                    let mut replacement = pin!(writer.write(&[0]));
                    poll(replacement.as_mut());
                    assert_eq!(writes(&host, 0x81).last().unwrap(), &[0]);
                    poll(replacement.as_mut());
                    host.0.borrow_mut().active.clear();
                    assert_eq!(replacement.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
                }
            }
        }
    }
}

#[test]
fn cancelled_hid_report_keeps_forwarded_input_until_a_complete_retry() {
    for direct_enable in [false, true] {
        let host = Host::default();
        host.0.borrow_mut().reconfiguration = true;
        host.0.borrow_mut().direct_enable = direct_enable;
        let io = Io::new();
        let mut buffers = Buffers::new(&io);
        let usb = Usb::new(
            completion::CompleteDriver {
                driver: MockDriver(host.clone()),
                completion: Reconfiguration(&host),
            },
            "0000000000000000",
            "Test adapter",
            Topology {
                desktop: true,
                ..Default::default()
            },
            &io,
            &mut buffers,
        );
        let mut running = pin!(usb.run_until_rebuild());
        configured(&host, running.as_mut());
        let mut loops = Loops::new(&io, 0);
        let mut shared = shared(application(), support::Store::default());
        // Complete the initial held-state reports before submitting relative input.
        for now in 1..30 {
            loops.poll(&mut shared, now);
            poll(running.as_mut());
            host.0
                .borrow_mut()
                .active
                .retain(|address| address & 0x80 == 0);
            poll(running.as_mut());
        }
        assert!(shared.app.manager.forward.packet().is_none());
        let mut input = Input::default();
        input.consumer_motion[0] = 17;
        shared.app.manager.forward.input(0, input).unwrap();
        loops.poll(&mut shared, 30);
        let expected = shared
            .app
            .manager
            .forward
            .packet()
            .unwrap()
            .bytes()
            .to_vec();
        assert_eq!(expected.len(), 68);
        host.0.borrow_mut().writes.clear();
        poll(running.as_mut());
        assert_eq!(writes(&host, 0x83).len(), 1);
        assert_eq!(writes(&host, 0x83)[0].len(), 64);
        assert_eq!(writes(&host, 0x83)[0][0], 4);
        let generation = io.status().generation;
        exchange(&host, running.as_mut(), [1, 11, 0, 0, 2, 0, 0, 0], false);
        assert_eq!(io.status().generation, generation);
        let done = io.hid_done.try_receive().unwrap();
        assert!(!done.success);
        io.hid_done.try_send(done).ok().unwrap();
        assert_eq!(writes(&host, 0x83).len(), 1);
        loops.poll(&mut shared, 31);
        assert_eq!(
            shared.app.manager.forward.packet().unwrap().bytes(),
            expected
        );
        poll(running.as_mut());
        assert_eq!(writes(&host, 0x83)[1], writes(&host, 0x83)[0]);
        host.0
            .borrow_mut()
            .active
            .retain(|address| *address != 0x83);
        poll(running.as_mut());
        assert_eq!(writes(&host, 0x83)[2], expected[63..]);
        host.0
            .borrow_mut()
            .active
            .retain(|address| *address != 0x83);
        poll(running.as_mut());
        let done = io.hid_done.try_receive().unwrap();
        assert!(done.success);
        io.hid_done.try_send(done).ok().unwrap();
        loops.poll(&mut shared, 32);
        assert!(shared.app.manager.forward.packet().is_none());
    }
}

#[cfg(feature = "gamepad")]
fn gamepad_reconfiguration_resumes_transmission(xinput_mode: bool) {
    use cordial_core::outputs::{Plan, SlotSnapshot, Snapshots};
    for direct_enable in [false, true] {
        let host = Host::default();
        host.0.borrow_mut().reconfiguration = true;
        host.0.borrow_mut().direct_enable = direct_enable;
        let io = Io::new();
        let mut buffers = Buffers::new(&io);
        let usb = Usb::new(
            completion::CompleteDriver {
                driver: MockDriver(host.clone()),
                completion: Reconfiguration(&host),
            },
            "0000000000000000",
            "Test adapter",
            Topology {
                desktop: true,
                gamepads: Plan {
                    hid_gamepads: u8::from(!xinput_mode),
                    xinput_gamepads: u8::from(xinput_mode),
                },
                ..Default::default()
            },
            &io,
            &mut buffers,
        );
        let mut running = pin!(usb.run_until_rebuild());
        configured(&host, running.as_mut());
        let status = io.status();
        let mut snapshots = Snapshots::default();
        let slot = SlotSnapshot {
            occupant: Some(cordial_core::link::LinkId {
                slot: 0,
                generation: 1,
            }),
            generation: 1,
            gamepad: cordial_core::hid::Gamepad {
                buttons: 1,
                ..Default::default()
            },
        };
        if xinput_mode {
            snapshots.xinput[0] = slot;
        } else {
            snapshots.hid[0] = slot;
        }
        io.outputs.publish(status.generation, snapshots);
        poll(running.as_mut());
        let initial = writes(&host, 0x84)[0].clone();
        // Presence/neutral must be retried before the controller's input.
        exchange(&host, running.as_mut(), [1, 11, 0, 0, 3, 0, 0, 0], false);
        assert_eq!(io.status(), status);
        assert_eq!(writes(&host, 0x84), [initial.clone(), initial]);
        host.0
            .borrow_mut()
            .active
            .retain(|address| *address != 0x84);
        poll(running.as_mut());
        let report = writes(&host, 0x84)[2].clone();
        if xinput_mode {
            assert_eq!(
                &report[6..18],
                xinput::Input::encode(&slot.gamepad).as_bytes()
            );
        } else {
            assert_eq!(
                report,
                gamepad::Report::encode(1, &[0; 8], 0, None).as_bytes()
            );
        }
        // Selecting the desktop interface leaves the pending gamepad write intact.
        exchange(&host, running.as_mut(), [1, 11, 0, 0, 2, 0, 0, 0], false);
        assert_eq!(writes(&host, 0x84).len(), 3);
        assert!(host.0.borrow().active.contains(&0x84));
        exchange(&host, running.as_mut(), [1, 11, 0, 0, 3, 0, 0, 0], false);
        assert_eq!(io.status(), status);
        assert_eq!(writes(&host, 0x84)[3], report);
        // A disabled endpoint waits without new submissions or a bus-status event.
        completion::Completion::endpoint_set_enabled(
            &Reconfiguration(&host),
            &mut host.clone(),
            EndpointAddress::from(0x84),
            false,
        );
        for _ in 0..3 {
            poll(running.as_mut());
        }
        assert_eq!(writes(&host, 0x84).len(), 4);
        exchange(&host, running.as_mut(), [1, 11, 0, 0, 3, 0, 0, 0], false);
        assert_eq!(writes(&host, 0x84)[4], report);
        host.0
            .borrow_mut()
            .active
            .retain(|address| *address != 0x84);
        poll(running.as_mut());
        assert_eq!(writes(&host, 0x84).len(), 5);
        if xinput_mode {
            snapshots.xinput[0].gamepad.buttons = 2;
        } else {
            snapshots.hid[0].gamepad.buttons = 2;
        }
        io.outputs.publish(status.generation, snapshots);
        poll(running.as_mut());
        assert_eq!(writes(&host, 0x84).len(), 6);
        assert_ne!(writes(&host, 0x84)[5], report);
        assert_eq!(io.status(), status);
    }
}

#[cfg(feature = "gamepad")]
#[test]
fn hid_gamepad_retries_cancelled_input_without_a_status_change() {
    gamepad_reconfiguration_resumes_transmission(false);
}

#[cfg(feature = "gamepad")]
#[test]
fn xinput_retries_cancelled_input_without_a_status_change() {
    gamepad_reconfiguration_resumes_transmission(true);
}

#[test]
fn desktop_idle_cancellation_resumes_refresh_and_queued_input() {
    use embassy_time::Duration;
    for direct_enable in [false, true] {
        crate::tests::TICKS.with(|ticks| ticks.set(0));
        let host = Host::default();
        host.0.borrow_mut().reconfiguration = true;
        host.0.borrow_mut().direct_enable = direct_enable;
        let io = Io::new();
        let mut buffers = Buffers::new(&io);
        let usb = Usb::new(
            completion::CompleteDriver {
                driver: MockDriver(host.clone()),
                completion: Reconfiguration(&host),
            },
            "0000000000000000",
            "Test adapter",
            Topology {
                desktop: true,
                ..Default::default()
            },
            &io,
            &mut buffers,
        );
        let mut running = pin!(usb.run_until_rebuild());
        configured(&host, running.as_mut());
        exchange(
            &host,
            running.as_mut(),
            [0x21, 0x0a, 1, 1, 2, 0, 0, 0],
            false,
        );
        let status = io.status();
        crate::tests::TICKS.with(|ticks| ticks.set(Duration::from_millis(4).as_ticks()));
        poll(running.as_mut());
        let idle = writes(&host, 0x83)[0].clone();
        assert_eq!(idle.len(), 33);
        assert_eq!(idle[0], 1);
        exchange(&host, running.as_mut(), [1, 11, 0, 0, 2, 0, 0, 0], false);
        assert_eq!(io.status(), status);
        assert_eq!(writes(&host, 0x83), [idle.clone(), idle.clone()]);
        let mut bytes = [0; 69];
        bytes[0] = 1;
        bytes[1] = 0x10;
        io.hid_tx
            .try_send(HidTx {
                generation: status.generation,
                sequence: 1,
                bytes,
                length: 33,
            })
            .ok()
            .unwrap();
        host.0
            .borrow_mut()
            .active
            .retain(|address| *address != 0x83);
        poll(running.as_mut());
        assert_eq!(writes(&host, 0x83)[2], bytes[..33]);
        host.0
            .borrow_mut()
            .active
            .retain(|address| *address != 0x83);
        poll(running.as_mut());
        assert!(io.hid_done.try_receive().unwrap().success);
        crate::tests::TICKS.with(|ticks| ticks.set(Duration::from_millis(8).as_ticks()));
        poll(running.as_mut());
        assert_eq!(writes(&host, 0x83)[3], bytes[..33]);
        completion::Completion::endpoint_set_enabled(
            &Reconfiguration(&host),
            &mut host.clone(),
            EndpointAddress::from(0x83),
            false,
        );
        for _ in 0..3 {
            poll(running.as_mut());
        }
        assert_eq!(writes(&host, 0x83).len(), 4);
        exchange(&host, running.as_mut(), [1, 11, 0, 0, 2, 0, 0, 0], false);
        assert_eq!(writes(&host, 0x83)[4], bytes[..33]);
        host.0
            .borrow_mut()
            .active
            .retain(|address| *address != 0x83);
        poll(running.as_mut());
        assert_eq!(writes(&host, 0x83).len(), 5);
        assert_eq!(io.status(), status);
    }
}

#[test]
fn reconfiguration_cancels_out_reads_without_consuming_the_next_packet() {
    for direct_enable in [false, true] {
        let host = Host::default();
        host.0.borrow_mut().reconfiguration = true;
        host.0.borrow_mut().direct_enable = direct_enable;
        let mut config = [0; 256];
        let mut bos = [0; 64];
        let mut msos = [0; 64];
        let mut control = [0; 64];
        let mut builder = Builder::new(
            completion::CompleteDriver {
                driver: MockDriver(host.clone()),
                completion: Reconfiguration(&host),
            },
            Config::new(0x1209, 0xc0d1),
            &mut config,
            &mut bos,
            &mut msos,
            &mut control,
        );
        let mut endpoints = Vec::new();
        for _ in 0..2 {
            endpoints.push(
                builder
                    .function(0xff, 0, 0)
                    .interface()
                    .alt_setting(0xff, 0, 0, None)
                    .endpoint_interrupt_out(None, 32, 1),
            );
        }
        let mut device = builder.build();
        let mut running = pin!(device.run());
        configured(&host, running.as_mut());
        let mut cx = Context::from_waker(Waker::noop());
        let mut bytes = [0; 32];
        for request in [
            [1, 11, 0, 0, 0, 0, 0, 0],
            [0, 9, 1, 0, 0, 0, 0, 0],
            [0, 9, 0, 0, 0, 0, 0, 0],
        ] {
            {
                let mut read = pin!(endpoints[0].read(&mut bytes));
                poll(read.as_mut());
                exchange(&host, running.as_mut(), [1, 11, 0, 0, 1, 0, 0, 0], false);
                poll(read.as_mut());
                exchange(&host, running.as_mut(), request, false);
                // A packet arriving after reconfiguration belongs to the next read.
                host.0.borrow_mut().reads.push_back(vec![7; 32]);
                assert_eq!(
                    read.as_mut().poll(&mut cx),
                    Poll::Ready(Err(EndpointError::Disabled))
                );
                assert_eq!(host.0.borrow().reads.len(), 1);
            }
            if request[1] == 9 && request[2] == 0 {
                assert!(host.0.borrow().active.is_empty());
                exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
            }
            assert_eq!(block_on(endpoints[0].read(&mut bytes)), Ok(32));
            assert_eq!(bytes, [7; 32]);
            assert!(host.0.borrow().reads.is_empty());
        }
    }
}

#[test]
fn otg_reconfiguration_discards_unread_packets_before_accepting_retries() {
    for stale in [None, Some(vec![]), Some(vec![9; 64])] {
        for transition in 0..4 {
            let host = Host::default();
            host.0.borrow_mut().reconfiguration = true;
            host.0.borrow_mut().otg = Some(OutBuffers::default());
            let mut config = [0; 256];
            let mut bos = [0; 64];
            let mut msos = [0; 64];
            let mut control = [0; 64];
            let mut builder = Builder::new(
                completion::CompleteDriver {
                    driver: MockDriver(host.clone()),
                    completion: Reconfiguration(&host),
                },
                Config::new(0x1209, 0xc0d1),
                &mut config,
                &mut bos,
                &mut msos,
                &mut control,
            );
            let mut first = builder
                .function(0xff, 0, 0)
                .interface()
                .alt_setting(0xff, 0, 0, None)
                .endpoint_bulk_out(None, 64);
            let second = builder
                .function(0xff, 0, 0)
                .interface()
                .alt_setting(0xff, 0, 0, None)
                .endpoint_bulk_out(None, 64);
            let address = u8::from(first.info().addr);
            let index = first.info().addr.index();
            let unused = second.info().addr.index();
            let mut device = builder.build();
            let mut running = pin!(device.run());
            configured(&host, running.as_mut());
            let mut bytes = [0; 64];
            let mut cx = Context::from_waker(Waker::noop());
            {
                let mut old = pin!(first.read(&mut bytes));
                poll(old.as_mut());
                {
                    let mut traffic = host.0.borrow_mut();
                    let otg = traffic.otg.as_mut().unwrap();
                    if let Some(packet) = &stale {
                        assert!(otg.send(index, packet));
                    }
                    otg.arrival = Some((index, vec![7; 64]));
                }
                match transition {
                    0 | 1 => {
                        let request = if transition == 0 {
                            [1, 11, 0, 0, 0, 0, 0, 0]
                        } else {
                            [0, 9, 1, 0, 0, 0, 0, 0]
                        };
                        exchange(&host, running.as_mut(), request, false);
                        exchange(&host, running.as_mut(), request, false);
                    }
                    2 => {
                        exchange(&host, running.as_mut(), [0, 9, 0, 0, 0, 0, 0, 0], false);
                        let mut traffic = host.0.borrow_mut();
                        let otg = traffic.otg.as_mut().unwrap();
                        assert!(!otg.enabled[index]);
                        assert!(!otg.send(index, &[6; 64]));
                        assert_eq!(otg.packets[index], stale);
                        drop(traffic);
                        exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
                    }
                    _ => {
                        host.0.borrow_mut().events.push_back(Event::Reset);
                        poll(running.as_mut());
                        // Reset leaves the endpoint inactive and retains out_size.
                        let mut traffic = host.0.borrow_mut();
                        let otg = traffic.otg.as_mut().unwrap();
                        assert_eq!(otg.pktcnt[index], 1);
                        assert!(!otg.enabled[index]);
                        assert!(!otg.send(index, &[6; 64]));
                        assert_eq!(otg.packets[index], stale);
                        drop(traffic);
                        configured(&host, running.as_mut());
                    }
                }
                assert_eq!(
                    old.as_mut().poll(&mut cx),
                    Poll::Ready(Err(EndpointError::Disabled))
                );
            }
            // HALT, repeated clear and dropping a stalled read cannot bypass cleanup.
            for _ in 0..2 {
                exchange(
                    &host,
                    running.as_mut(),
                    [2, 3, 0, 0, address, 0, 0, 0],
                    false,
                );
            }
            {
                let mut cancelled = pin!(first.read(&mut bytes));
                poll(cancelled.as_mut());
            }
            assert!(host.0.borrow().otg.as_ref().unwrap().dirty[index]);
            for _ in 0..2 {
                exchange(
                    &host,
                    running.as_mut(),
                    [2, 1, 0, 0, address, 0, 0, 0],
                    false,
                );
            }
            assert_eq!(block_on(first.read(&mut bytes)), Ok(64));
            assert_eq!(bytes, [7; 64]);
            {
                let mut traffic = host.0.borrow_mut();
                let otg = traffic.otg.as_mut().unwrap();
                assert_eq!(otg.lost, 0);
                assert!(otg.retries >= 6);
                assert!(!otg.global_nak);
                assert!(!otg.dirty[index]);
                assert!(otg.dirty[unused] && otg.nak[unused]);
                assert!(otg.send(index, &[8; 64]));
                traffic.data1.push(address);
            }
            let drained = host.0.borrow().otg.as_ref().unwrap().drain_count;
            assert_eq!(block_on(first.read(&mut bytes)), Ok(64));
            assert_eq!(bytes, [8; 64]);
            assert_eq!(host.0.borrow().otg.as_ref().unwrap().drain_count, drained);
            assert!(host.0.borrow().data1.contains(&address));
        }
    }
}

#[test]
fn otg_reset_interrupt_blocks_reads_until_the_bus_processes_reset() {
    let host = Host::default();
    host.0.borrow_mut().reconfiguration = true;
    host.0.borrow_mut().otg = Some(OutBuffers::default());
    let mut config = [0; 256];
    let mut bos = [0; 64];
    let mut msos = [0; 64];
    let mut control = [0; 64];
    let mut builder = Builder::new(
        completion::CompleteDriver {
            driver: MockDriver(host.clone()),
            completion: Reconfiguration(&host),
        },
        Config::new(0x1209, 0xc0d1),
        &mut config,
        &mut bos,
        &mut msos,
        &mut control,
    );
    let mut endpoint = builder
        .function(0xff, 0, 0)
        .interface()
        .alt_setting(0xff, 0, 0, None)
        .endpoint_bulk_out(None, 64);
    let index = endpoint.info().addr.index();
    let mut device = builder.build();
    let mut running = pin!(device.run());
    configured(&host, running.as_mut());
    let mut bytes = [0; 64];
    let mut cx = Context::from_waker(Waker::noop());

    // The first poll cleans the reconfiguration hold, then waits for traffic.
    {
        let mut read = pin!(endpoint.read(&mut bytes));
        poll(read.as_mut());
    }
    assert!(!host.0.borrow().otg.as_ref().unwrap().dirty[index]);
    assert!(
        host.0
            .borrow_mut()
            .otg
            .as_mut()
            .unwrap()
            .send(index, &[9; 64])
    );

    // Hardware reset becomes visible after Cordial's IRQ sample but before this read runs.
    {
        let mut old = pin!(endpoint.read(&mut bytes));
        let epoch = host.0.borrow().epochs[index];
        host.0.borrow_mut().reset_hardware_after_bus_poll = true;
        poll(running.as_mut());
        let traffic = host.0.borrow();
        assert!(traffic.reset_hardware_pending);
        assert!(!traffic.reset_pending);
        assert_eq!(traffic.epochs[index], epoch);
        drop(traffic);
        assert_eq!(
            old.as_mut().poll(&mut cx),
            Poll::Ready(Err(EndpointError::Disabled))
        );
    }
    assert_eq!(
        host.0.borrow().otg.as_ref().unwrap().packets[index],
        Some(vec![9; 64])
    );
    assert!(host.0.borrow().otg.as_ref().unwrap().nak[index]);
    assert!(
        !host
            .0
            .borrow_mut()
            .otg
            .as_mut()
            .unwrap()
            .send(index, &[8; 64])
    );
    {
        let mut ready = pin!(endpoint.wait_enabled());
        assert!(ready.as_mut().poll(&mut cx).is_pending());
    }

    host.0.borrow_mut().events.push_back(Event::Reset);
    poll(running.as_mut());
    configured(&host, running.as_mut());
    {
        let traffic = host.0.borrow();
        assert!(!traffic.reset_hardware_pending);
        assert!(!traffic.reset_pending);
    }

    // A second reset interrupts the synchronous stale-packet drain.
    host.0.borrow_mut().reset_during_read = true;
    {
        let mut interrupted = pin!(endpoint.read(&mut bytes));
        assert_eq!(
            interrupted.as_mut().poll(&mut cx),
            Poll::Ready(Err(EndpointError::Disabled))
        );
    }
    {
        let traffic = host.0.borrow();
        let otg = traffic.otg.as_ref().unwrap();
        assert!(traffic.reset_pending);
        assert!(otg.dirty[index]);
        assert!(otg.nak[index]);
        assert!(otg.packets[index].is_none());
        assert!(!otg.global_nak);
    }
    {
        let mut ready = pin!(endpoint.wait_enabled());
        assert!(ready.as_mut().poll(&mut cx).is_pending());
    }

    host.0.borrow_mut().events.push_back(Event::Reset);
    poll(running.as_mut());
    configured(&host, running.as_mut());
    assert!(
        !host
            .0
            .borrow_mut()
            .otg
            .as_mut()
            .unwrap()
            .send(index, &[7; 64])
    );
    {
        let mut fresh = pin!(endpoint.read(&mut bytes));
        poll(fresh.as_mut());
        assert!(
            host.0
                .borrow_mut()
                .otg
                .as_mut()
                .unwrap()
                .send(index, &[7; 64])
        );
        assert_eq!(fresh.as_mut().poll(&mut cx), Poll::Ready(Ok(64)));
    }
    assert_eq!(bytes, [7; 64]);
    assert!(!host.0.borrow().otg.as_ref().unwrap().dirty[index]);

    // Live hardware reset after the underlying poll invalidates a completed read too.
    assert!(
        host.0
            .borrow_mut()
            .otg
            .as_mut()
            .unwrap()
            .send(index, &[5; 64])
    );
    let epoch = host.0.borrow().epochs[index];
    host.0.borrow_mut().reset_hardware_during_read = true;
    {
        let mut invalidated = pin!(endpoint.read(&mut bytes));
        assert_eq!(
            invalidated.as_mut().poll(&mut cx),
            Poll::Ready(Err(EndpointError::Disabled))
        );
    }
    let traffic = host.0.borrow();
    assert!(traffic.reset_hardware_pending);
    assert!(!traffic.reset_pending);
    assert_eq!(traffic.epochs[index], epoch);
    let otg = traffic.otg.as_ref().unwrap();
    assert!(!otg.dirty[index]);
    assert!(otg.nak[index]);
    assert!(otg.packets[index].is_none());
    drop(traffic);
    assert!(
        !host
            .0
            .borrow_mut()
            .otg
            .as_mut()
            .unwrap()
            .send(index, &[4; 64])
    );
    {
        let mut ready = pin!(endpoint.wait_enabled());
        assert!(ready.as_mut().poll(&mut cx).is_pending());
    }
}

#[test]
fn otg_failed_drain_releases_global_nak_and_keeps_cleanup_pending() {
    use completion::Completion;
    for error in [EndpointError::Disabled, EndpointError::BufferOverflow] {
        let host = Host::default();
        host.0.borrow_mut().otg = Some(OutBuffers::default());
        let platform = Reconfiguration(&host);
        let mut driver = completion::CompleteDriver {
            driver: MockDriver(host.clone()),
            completion: platform,
        };
        let mut endpoint = driver
            .alloc_endpoint_out(EndpointType::Bulk, None, 64, 0)
            .unwrap();
        let index = endpoint.info().addr.index();
        platform.endpoint_set_enabled(&mut host.clone(), endpoint.info().addr, true);
        {
            let mut traffic = host.0.borrow_mut();
            let otg = traffic.otg.as_mut().unwrap();
            otg.packets[index] = Some(vec![9; 64]);
            otg.error = Some(error);
            otg.arrival = Some((index, vec![7; 64]));
        }
        let mut bytes = [0; 64];
        assert_eq!(block_on(endpoint.read(&mut bytes)), Err(error));
        {
            let traffic = host.0.borrow();
            let otg = traffic.otg.as_ref().unwrap();
            assert!(!otg.global_nak);
            assert!(otg.dirty[index] && otg.nak[index]);
            assert_eq!(otg.packets[index], Some(vec![9; 64]));
        }
        assert_eq!(block_on(endpoint.read(&mut bytes)), Ok(64));
        assert_eq!(bytes, [7; 64]);
        assert_eq!(host.0.borrow().otg.as_ref().unwrap().lost, 0);
        // A disabled endpoint still owns its software packet until it is enabled.
        platform.endpoint_set_enabled(&mut host.clone(), endpoint.info().addr, false);
        host.0.borrow_mut().otg.as_mut().unwrap().packets[index] = Some(vec![]);
        assert_eq!(
            block_on(endpoint.read(&mut bytes)),
            Err(EndpointError::Disabled)
        );
        assert_eq!(
            host.0.borrow().otg.as_ref().unwrap().packets[index],
            Some(vec![])
        );
        assert!(!host.0.borrow().otg.as_ref().unwrap().global_nak);
    }
}

#[test]
fn endpoint_requests_validate_address_fields_state_and_recover() {
    let host = Host::default();
    let io = Io::new();
    let mut buffers = Buffers::new(&io);
    let usb = Usb::new(
        MockDriver(host.clone()),
        "0000000000000000",
        "Test adapter",
        Topology::default(),
        &io,
        &mut buffers,
    );
    let mut running = pin!(usb.run_until_rebuild());
    exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], true);
    exchange(&host, running.as_mut(), [0, 5, 1, 0, 0, 0, 0, 0], false);
    for address in [0, 0x80] {
        assert_eq!(
            exchange(
                &host,
                running.as_mut(),
                [0x82, 0, 0, 0, address, 0, 2, 0],
                false
            ),
            [0, 0]
        );
        exchange(
            &host,
            running.as_mut(),
            [2, 1, 0, 0, address, 0, 0, 0],
            false,
        );
    }
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, 0x81, 0, 2, 0],
        true,
    );
    exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
    for address in [0x81, 0x82, 2] {
        exchange(
            &host,
            running.as_mut(),
            [2, 3, 0, 0, address, 0, 0, 0],
            false,
        );
        assert_eq!(
            exchange(
                &host,
                running.as_mut(),
                [0x82, 0, 0, 0, address, 0, 2, 0],
                false
            ),
            [1, 0]
        );
        exchange(
            &host,
            running.as_mut(),
            [2, 1, 0, 0, address, 0, 0, 0],
            false,
        );
        assert_eq!(
            exchange(
                &host,
                running.as_mut(),
                [0x82, 0, 0, 0, address, 0, 2, 0],
                false
            ),
            [0, 0]
        );
    }
    let mut invalid = Vec::new();
    for index in [6u16, 0x86, 1, 0x83, 0x91, 0x181, 0xff81, 0x70] {
        for (kind, request, length) in [(0x82, 0, 2), (2, 1, 0), (2, 3, 0)] {
            invalid.push([
                kind,
                request,
                0,
                0,
                index as u8,
                (index >> 8) as u8,
                length,
                0,
            ]);
        }
    }
    invalid.extend([
        [0x82, 0, 1, 0, 0x81, 0, 2, 0],
        [0x82, 0, 0, 0, 0x81, 0, 0, 0],
        [0x82, 0, 0, 0, 0x81, 0, 1, 0],
        [0x82, 0, 0, 0, 0x81, 0, 3, 0],
        [0x82, 0, 0, 0, 0x81, 0, 2, 1],
        [2, 0, 0, 0, 0x81, 0, 2, 0],
        [0x82, 1, 0, 0, 0x81, 0, 0, 0],
        [2, 1, 1, 0, 0x81, 0, 0, 0],
        [2, 3, 0, 0, 0x81, 0, 1, 0],
    ]);
    for request in invalid {
        let accesses = host.0.borrow().accessed.len();
        exchange(&host, running.as_mut(), request, true);
        assert_eq!(host.0.borrow().accessed.len(), accesses);
        assert_eq!(
            exchange(
                &host,
                running.as_mut(),
                [0x82, 0, 0, 0, 0x81, 0, 2, 0],
                false
            ),
            [0, 0]
        );
    }
    exchange(&host, running.as_mut(), [0, 9, 0, 0, 0, 0, 0, 0], false);
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, 0x81, 0, 2, 0],
        true,
    );
    exchange(&host, running.as_mut(), [0x82, 0, 0, 0, 0, 0, 2, 0], false);
    exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
    host.0.borrow_mut().events.push_back(Event::Reset);
    poll(running.as_mut());
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, 0x81, 0, 2, 0],
        true,
    );
    configured(&host, running.as_mut());
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, 0x81, 0, 2, 0],
        false,
    );
}

#[test]
fn full_speed_descriptors_stall_other_speeds_then_continue_enumeration() {
    let host = Host::default();
    let io = Io::new();
    let mut buffers = Buffers::new(&io);
    let usb = Usb::new(
        MockDriver(host.clone()),
        "0000000000000000",
        "Test adapter",
        Topology::default(),
        &io,
        &mut buffers,
    );
    let mut running = pin!(usb.run_until_rebuild());
    for descriptor in [6, 7] {
        exchange(
            &host,
            running.as_mut(),
            [0x80, 6, 0, descriptor, 0, 0, 64, 0],
            true,
        );
        let device = exchange(&host, running.as_mut(), [0x80, 6, 0, 1, 0, 0, 18, 0], false);
        assert_eq!(&device[..2], &[18, 1]);
    }
    for (index, descriptor) in [(0, 2), (1, 3), (2, 3), (3, 3), (0, 15)] {
        let bytes = exchange(
            &host,
            running.as_mut(),
            [0x80, 6, index, descriptor, 0, 0, 255, 0],
            false,
        );
        assert_eq!(bytes[1], descriptor);
    }
}

#[test]
fn endpoint_enablement_follows_alternate_settings_and_msos_still_works() {
    let host = Host::default();
    let io = Io::new();
    let mut config = [0; 256];
    let mut bos = [0; 64];
    let mut msos = [0; 64];
    let mut control = [0; 64];
    let mut bus_handler = BusHandler(&io);
    let mut cdc = cdc_acm::State::new();
    let mut builder = Builder::new(
        serial::SessionDriver {
            driver: MockDriver(host.clone()),
            io: &io,
        },
        Config::new(0x1209, 0xc0d1),
        &mut config,
        &mut bos,
        &mut msos,
        &mut control,
    );
    builder.handler(&mut bus_handler);
    builder.msos_descriptor(embassy_usb::msos::windows_version::WIN8_1, 0x20);
    let cdc = cdc_acm::CdcAcmClass::new(&mut builder, &mut cdc, 64);
    let mut function = builder.function(0xff, 0, 0);
    let mut interface = function.interface();
    let number = interface.interface_number().0;
    interface.alt_setting(0xff, 0, 0, None);
    let endpoint = interface
        .alt_setting(0xff, 0, 0, None)
        .endpoint_interrupt_in(None, 8, 1);
    let mut alt = interface.alt_setting(0xff, 0, 0, None);
    let iso = alt.alloc_endpoint_in(EndpointType::Isochronous, None, 8, 1);
    alt.endpoint_descriptor(
        iso.info(),
        embassy_usb::descriptor::SynchronizationType::NoSynchronization,
        embassy_usb::descriptor::UsageType::DataEndpoint,
        &[],
    );
    drop(function);
    let address = endpoint.info().addr.into();
    let iso_address = iso.info().addr.into();
    let (_, rx) = cdc.split();
    let mut device = builder.build();
    let mut running = pin!(embassy_futures::join::join(
        device.run(),
        serial::receive(rx, &io)
    ));
    exchange(
        &host,
        running.as_mut(),
        [1, 11, 1, 0, number, 0, 0, 0],
        true,
    );
    configured(&host, running.as_mut());
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, address, 0, 2, 0],
        true,
    );
    exchange(
        &host,
        running.as_mut(),
        [1, 11, 1, 0, number, 0, 0, 0],
        false,
    );
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, address, 0, 2, 0],
        false,
    );
    for request in [
        [1, 11, 0, 0, number, 1, 0, 0],
        [1, 11, 0, 1, number, 0, 0, 0],
        [1, 11, 0, 0, number, 0, 1, 0],
        [1, 11, 3, 0, number, 0, 0, 0],
    ] {
        exchange(&host, running.as_mut(), request, true);
        exchange(
            &host,
            running.as_mut(),
            [0x82, 0, 0, 0, address, 0, 2, 0],
            false,
        );
    }
    exchange(
        &host,
        running.as_mut(),
        [1, 11, 2, 0, number, 0, 0, 0],
        false,
    );
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, address, 0, 2, 0],
        true,
    );
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, iso_address, 0, 2, 0],
        false,
    );
    exchange(
        &host,
        running.as_mut(),
        [2, 3, 0, 0, iso_address, 0, 0, 0],
        true,
    );
    exchange(
        &host,
        running.as_mut(),
        [2, 1, 0, 0, iso_address, 0, 0, 0],
        true,
    );
    exchange(
        &host,
        running.as_mut(),
        [1, 11, 0, 0, number, 0, 0, 0],
        false,
    );
    exchange(
        &host,
        running.as_mut(),
        [0x82, 0, 0, 0, iso_address, 0, 2, 0],
        true,
    );
    let bos = exchange(
        &host,
        running.as_mut(),
        [0x80, 6, 0, 15, 0, 0, 64, 0],
        false,
    );
    assert_eq!(bos[1], 15);
    assert!(bos.len() > 5);
    let msos = exchange(
        &host,
        running.as_mut(),
        [0xc0, 0x20, 0, 0, 7, 0, 64, 0],
        false,
    );
    assert_eq!(&msos[..4], &[10, 0, 0, 0]);
}

#[test]
fn otg_raw_worker_discards_old_commands_and_executes_a_fresh_retry_once() {
    for transition in 0..5 {
        let host = Host::default();
        host.0.borrow_mut().otg = Some(OutBuffers::default());
        let io = Io::new();
        let (mut app, store, _) = profile_application();
        let interface = Interface::ALL[0];
        app.manager.preference.configuration_interfaces = vec![enabled(interface, 1)];
        let mut shared = shared(app, store);
        let mut loops = Loops::new(&io, interface.bit());
        let mut buffers = Buffers::new(&io);
        let usb = Usb::new(
            completion::CompleteDriver {
                driver: MockDriver(host.clone()),
                completion: Reconfiguration(&host),
            },
            "0000000000000000",
            "Test adapter",
            Topology {
                interfaces: interface.bit(),
                ..Default::default()
            },
            &io,
            &mut buffers,
        );
        let out = host
            .0
            .borrow()
            .allocations
            .iter()
            .find(|e| e.addr.is_out() && e.max_packet_size == 32)
            .unwrap()
            .addr
            .index();
        let input = host
            .0
            .borrow()
            .allocations
            .iter()
            .rev()
            .find(|e| e.addr.is_in())
            .unwrap()
            .addr
            .into();
        let mut running = pin!(usb.run_until_rebuild());
        configured(&host, running.as_mut());
        assert!(
            host.0
                .borrow_mut()
                .otg
                .as_mut()
                .unwrap()
                .send(out, &set_keycode())
        );
        // The device task handles setup before the endpoint worker next runs.
        match transition {
            0 => {
                exchange(&host, running.as_mut(), [1, 11, 0, 0, 2, 0, 0, 0], false);
            }
            1 => {
                exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
            }
            2 => {
                host.0.borrow_mut().events.push_back(Event::Reset);
                poll(running.as_mut());
                configured(&host, running.as_mut());
            }
            3 => {
                // The bus future has already returned Pending when reset preempts the worker.
                host.0.borrow_mut().reset_after_bus_poll = true;
                poll(running.as_mut());
                assert!(host.0.borrow().reset_pending);
                assert_eq!(
                    host.0.borrow().otg.as_ref().unwrap().packets[out],
                    Some(set_keycode().to_vec())
                );
                host.0.borrow_mut().events.push_back(Event::Reset);
                poll(running.as_mut());
                configured(&host, running.as_mut());
            }
            _ => {
                // Embassy sees reset after Cordial's IRQ-side sample.
                let epoch = host.0.borrow().epochs[out];
                host.0.borrow_mut().reset_hardware_after_bus_poll = true;
                poll(running.as_mut());
                let traffic = host.0.borrow();
                assert!(traffic.reset_hardware_pending);
                assert!(!traffic.reset_pending);
                assert_eq!(traffic.epochs[out], epoch);
                assert_eq!(
                    traffic.otg.as_ref().unwrap().packets[out],
                    Some(set_keycode().to_vec())
                );
                drop(traffic);
                host.0.borrow_mut().events.push_back(Event::Reset);
                poll(running.as_mut());
                assert!(!host.0.borrow().reset_hardware_pending);
                configured(&host, running.as_mut());
            }
        }
        loops.poll(&mut shared, 1);
        assert_eq!(loaded_rules(&shared.app, 1), 0);
        assert!(writes(&host, input).is_empty());
        assert!(io.raw[0].rx.is_empty());
        assert!(
            host.0
                .borrow_mut()
                .otg
                .as_mut()
                .unwrap()
                .send(out, &set_keycode())
        );
        poll(running.as_mut());
        loops.poll(&mut shared, 2);
        poll(running.as_mut());
        assert_eq!(loaded_rules(&shared.app, 1), 1);
        assert_eq!(writes(&host, input).len(), 1);
        let saved = shared.store.writes.len();
        for tick in 3..6 {
            loops.poll(&mut shared, tick);
            poll(running.as_mut());
        }
        assert_eq!(writes(&host, input).len(), 1);
        assert_eq!(shared.store.writes.len(), saved);
        assert_eq!(host.0.borrow().otg.as_ref().unwrap().lost, 0);
    }
}

#[test]
fn raw_control_snapshot_and_interrupt_commands_share_the_response_lifecycle() {
    for interface in Interface::ALL {
        for desktop in [false, true] {
            let host = Host::default();
            let io = Io::new();
            let (mut app, store, _) = profile_application();
            app.manager.preference.configuration_interfaces = vec![enabled(interface, 1)];
            let mut shared = shared(app, store);
            let mut loops = Loops::new(&io, interface.bit());
            let mut buffers = Buffers::new(&io);
            let usb = Usb::new(
                MockDriver(host.clone()),
                "0000000000000000",
                "Test adapter",
                Topology {
                    desktop,
                    interfaces: interface.bit(),
                    ..Default::default()
                },
                &io,
                &mut buffers,
            );
            let number = 2 + u8::from(desktop);
            let endpoint = host
                .0
                .borrow()
                .allocations
                .iter()
                .rev()
                .find(|e| e.addr.is_in())
                .unwrap()
                .addr
                .into();
            let get = [0xa1, 1, 0, 1, number, 0, 32, 0];
            let mut running = pin!(usb.run_until_rebuild());
            exchange(&host, running.as_mut(), get, true);
            configured(&host, running.as_mut());
            assert_eq!(exchange(&host, running.as_mut(), get, false), [0; 32]);
            host.0.borrow_mut().payload = set_keycode().to_vec();
            exchange(
                &host,
                running.as_mut(),
                [0x21, 9, 0, 2, number, 0, 32, 0],
                false,
            );
            assert!(io.raw[0].rx.is_full());
            loops.poll(&mut shared, 1);
            assert_eq!(loaded_rules(&shared.app, 1), 1);
            assert!(io.raw[0].tx.is_full());
            host.0.borrow_mut().block_write = true;
            poll(running.as_mut());
            assert_eq!(exchange(&host, running.as_mut(), get, false), [0; 32]);
            host.0.borrow_mut().block_write = false;
            poll(running.as_mut());
            let current = writes(&host, endpoint).last().unwrap().clone();
            assert_eq!(current.len(), 32);
            assert_eq!(current[0], 5);
            let written = writes(&host, endpoint).len();
            host.0.borrow_mut().events.push_back(Event::Suspend);
            poll(running.as_mut());
            assert!(io.status().suspended);
            host.0.borrow_mut().events.push_back(Event::Resume);
            poll(running.as_mut());
            assert!(!io.status().suspended);
            for length in [1, 7, 32, 64] {
                let mut request = get;
                request[6] = length;
                assert_eq!(
                    exchange(&host, running.as_mut(), request, false),
                    current[..usize::from(length).min(32)]
                );
            }
            assert_eq!(writes(&host, endpoint).len(), written);
            assert!(io.raw[0].rx.is_empty());
            assert!(io.raw[0].tx.is_empty());
            loops.poll(&mut shared, 2);
            poll(running.as_mut());
            assert_eq!(loaded_rules(&shared.app, 1), 1);
            assert_eq!(writes(&host, endpoint).len(), written);
            for request in [
                [0xa1, 1, 1, 1, number, 0, 32, 0],
                [0xa1, 1, 0, 2, number, 0, 32, 0],
                [0xa1, 1, 0, 3, number, 0, 32, 0],
                [0xa1, 1, 0, 1, number, 0, 0, 0],
                [0xa1, 2, 0, 0, number, 0, 1, 0],
                [0x21, 10, 0, 0, number, 0, 0, 0],
                [0x21, 10, 0, 5, number, 0, 0, 0],
                [0x21, 9, 0, 2, number, 0, 31, 0],
                [0xa1, 9, 0, 2, number, 0, 32, 0],
            ] {
                exchange(&host, running.as_mut(), request, true);
                assert_eq!(exchange(&host, running.as_mut(), get, false), current);
            }
            // Interrupt OUT still executes a normal command and produces one 32-byte reply.
            host.0.borrow_mut().reads.push_back(vec![1; 32]);
            poll(running.as_mut());
            loops.poll(&mut shared, 3);
            poll(running.as_mut());
            assert_eq!(writes(&host, endpoint).len(), written + 1);
            assert_eq!(
                exchange(&host, running.as_mut(), get, false),
                *writes(&host, endpoint).last().unwrap()
            );
            host.0.borrow_mut().events.push_back(Event::Reset);
            poll(running.as_mut());
            configured(&host, running.as_mut());
            assert_eq!(exchange(&host, running.as_mut(), get, false), [0; 32]);
        }
    }
}

fn writes(host: &Host, endpoint: u8) -> Vec<Vec<u8>> {
    host.0
        .borrow()
        .writes
        .iter()
        .filter(|(address, _)| *address == endpoint)
        .map(|(_, bytes)| bytes.clone())
        .collect()
}

#[test]
fn raw_buffered_interrupt_command_is_discarded_when_suspend_changes_generation() {
    for direct_enable in [false, true] {
        for interface in Interface::ALL {
            let host = Host::default();
            host.0.borrow_mut().reconfiguration = true;
            host.0.borrow_mut().direct_enable = direct_enable;
            let io = Io::new();
            let (mut app, store, _) = profile_application();
            app.manager.preference.configuration_interfaces = vec![enabled(interface, 1)];
            let mut shared = shared(app, store);
            let mut loops = Loops::new(&io, interface.bit());
            let mut buffers = Buffers::new(&io);
            let usb = Usb::new(
                completion::CompleteDriver {
                    driver: MockDriver(host.clone()),
                    completion: Reconfiguration(&host),
                },
                "0000000000000000",
                "Test adapter",
                Topology {
                    interfaces: interface.bit(),
                    ..Default::default()
                },
                &io,
                &mut buffers,
            );
            let mut running = pin!(usb.run_until_rebuild());
            configured(&host, running.as_mut());
            let generation = io.status().generation;
            let epochs = host.0.borrow().epochs;

            // Suspend leaves the buffered OUT packet available to the pending read.
            host.0.borrow_mut().reads.push_back(set_keycode().to_vec());
            host.0.borrow_mut().events.push_back(Event::Suspend);
            poll(running.as_mut());
            assert!(io.status().suspended);
            assert_ne!(io.status().generation, generation);
            assert_eq!(host.0.borrow().epochs, epochs);
            assert!(host.0.borrow().reads.is_empty());
            loops.poll(&mut shared, 1);
            assert_eq!(loaded_rules(&shared.app, 1), 0);
            assert!(io.raw[0].rx.is_empty());
            assert!(io.raw[0].tx.is_empty());
            assert!(writes(&host, 0x83).is_empty());

            host.0.borrow_mut().events.push_back(Event::Resume);
            poll(running.as_mut());
            host.0.borrow_mut().reads.push_back(set_keycode().to_vec());
            poll(running.as_mut());
            loops.poll(&mut shared, 2);
            assert_eq!(loaded_rules(&shared.app, 1), 1);
            poll(running.as_mut());
            assert_eq!(writes(&host, 0x83), [set_keycode().to_vec()]);
            let get = [0xa1, 1, 0, 1, 2, 0, 32, 0];
            assert_eq!(exchange(&host, running.as_mut(), get, false), [0; 32]);
            host.0
                .borrow_mut()
                .active
                .retain(|address| *address != 0x83);
            poll(running.as_mut());
            assert_eq!(exchange(&host, running.as_mut(), get, false), set_keycode());
            assert!(io.raw[0].rx.is_empty());
            assert!(io.raw[0].tx.is_empty());
        }
    }
}

#[test]
fn raw_reply_retries_interface_cancellation_without_reexecuting_the_command() {
    for direct_enable in [false, true] {
        for interface in Interface::ALL {
            let host = Host::default();
            host.0.borrow_mut().reconfiguration = true;
            host.0.borrow_mut().direct_enable = direct_enable;
            let io = Io::new();
            let (mut app, store, _) = profile_application();
            app.manager.preference.configuration_interfaces = vec![enabled(interface, 1)];
            let mut shared = shared(app, store);
            let mut loops = Loops::new(&io, interface.bit());
            let mut buffers = Buffers::new(&io);
            let usb = Usb::new(
                completion::CompleteDriver {
                    driver: MockDriver(host.clone()),
                    completion: Reconfiguration(&host),
                },
                "0000000000000000",
                "Test adapter",
                Topology {
                    interfaces: interface.bit(),
                    ..Default::default()
                },
                &io,
                &mut buffers,
            );
            let mut running = pin!(usb.run_until_rebuild());
            configured(&host, running.as_mut());
            let get = [0xa1, 1, 0, 1, 2, 0, 32, 0];
            let generation = io.status().generation;
            io.raw[0]
                .tx
                .try_send(RawPacket {
                    generation,
                    interface,
                    bytes: [9; 32],
                })
                .ok()
                .unwrap();
            poll(running.as_mut());
            host.0
                .borrow_mut()
                .active
                .retain(|address| *address != 0x83);
            poll(running.as_mut());
            assert_eq!(exchange(&host, running.as_mut(), get, false), [9; 32]);

            host.0.borrow_mut().payload = set_keycode().to_vec();
            exchange(&host, running.as_mut(), [0x21, 9, 0, 2, 2, 0, 32, 0], false);
            loops.poll(&mut shared, 1);
            assert_eq!(loaded_rules(&shared.app, 1), 1);
            poll(running.as_mut());
            let reply = writes(&host, 0x83)[1].clone();
            assert_eq!(reply, set_keycode());
            io.raw[0]
                .tx
                .try_send(RawPacket {
                    generation,
                    interface,
                    bytes: [7; 32],
                })
                .ok()
                .unwrap();
            exchange(&host, running.as_mut(), [1, 11, 0, 0, 2, 0, 0, 0], false);
            assert_eq!(io.status().generation, generation);
            assert_eq!(
                writes(&host, 0x83),
                [vec![9; 32], reply.clone(), reply.clone()]
            );
            assert_eq!(exchange(&host, running.as_mut(), get, false), [9; 32]);
            assert!(io.raw[0].tx.is_full());
            assert!(io.raw[0].rx.is_empty());

            // Endpoint readiness, rather than a bus-status event, resumes the same reply.
            completion::Completion::endpoint_set_enabled(
                &Reconfiguration(&host),
                &mut host.clone(),
                EndpointAddress::from(0x83),
                false,
            );
            for _ in 0..3 {
                poll(running.as_mut());
            }
            assert_eq!(writes(&host, 0x83).len(), 3);
            assert_eq!(exchange(&host, running.as_mut(), get, false), [9; 32]);
            exchange(&host, running.as_mut(), [1, 11, 0, 0, 2, 0, 0, 0], false);
            assert_eq!(io.status().generation, generation);
            assert_eq!(writes(&host, 0x83)[3], reply);
            assert!(io.raw[0].tx.is_full());
            host.0
                .borrow_mut()
                .active
                .retain(|address| *address != 0x83);
            poll(running.as_mut());
            assert_eq!(exchange(&host, running.as_mut(), get, false), reply);
            assert_eq!(writes(&host, 0x83)[4], [7; 32]);
            assert!(host.0.borrow().active.contains(&0x83));
            host.0
                .borrow_mut()
                .active
                .retain(|address| *address != 0x83);
            poll(running.as_mut());
            assert_eq!(exchange(&host, running.as_mut(), get, false), [7; 32]);
            loops.poll(&mut shared, 1 + cordial_core::application::EDITOR_BATCH_MS);
            assert_eq!(rules(&mut shared.store, 1), 1);
            loops.poll(&mut shared, 2 + cordial_core::application::EDITOR_BATCH_MS);
            poll(running.as_mut());
            let key = cordial_core::storage::record_key(cordial_core::profiles::RULES, 1);
            assert_eq!(
                shared
                    .store
                    .writes
                    .iter()
                    .filter(|saved| **saved == key)
                    .count(),
                1
            );
            assert_eq!(writes(&host, 0x83).len(), 5);
            assert!(io.raw[0].rx.is_empty());
            assert!(io.raw[0].tx.is_empty());
        }
    }
}

#[test]
fn raw_pending_reply_discards_stale_sessions_and_retargeted_interfaces() {
    for direct_enable in [false, true] {
        for waiting in [false, true] {
            for transition in ["reset", "unconfigure", "retarget"] {
                let host = Host::default();
                host.0.borrow_mut().reconfiguration = true;
                host.0.borrow_mut().direct_enable = direct_enable;
                let io = Io::new();
                let mut buffers = Buffers::new(&io);
                let usb = Usb::new(
                    completion::CompleteDriver {
                        driver: MockDriver(host.clone()),
                        completion: Reconfiguration(&host),
                    },
                    "0000000000000000",
                    "Test adapter",
                    Topology {
                        interfaces: Interface::Via.bit(),
                        ..Default::default()
                    },
                    &io,
                    &mut buffers,
                );
                let mut running = pin!(usb.run_until_rebuild());
                configured(&host, running.as_mut());
                if waiting {
                    completion::Completion::endpoint_set_enabled(
                        &Reconfiguration(&host),
                        &mut host.clone(),
                        EndpointAddress::from(0x83),
                        false,
                    );
                }
                let send = |interface, bytes| {
                    io.raw[0]
                        .tx
                        .try_send(RawPacket {
                            generation: io.status().generation,
                            interface,
                            bytes,
                        })
                        .ok()
                        .unwrap();
                };
                send(Interface::Via, [1; 32]);
                poll(running.as_mut());
                send(Interface::Via, [2; 32]);
                match transition {
                    "reset" => {
                        host.0.borrow_mut().events.push_back(Event::Reset);
                        poll(running.as_mut());
                        configured(&host, running.as_mut());
                    }
                    "unconfigure" => {
                        exchange(&host, running.as_mut(), [0, 9, 0, 0, 0, 0, 0, 0], false);
                        exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
                    }
                    "retarget" => {
                        io.set_interfaces(Interface::Vial.bit());
                        exchange(&host, running.as_mut(), [1, 11, 0, 0, 2, 0, 0, 0], false);
                    }
                    _ => unreachable!(),
                }
                assert!(io.raw[0].tx.is_empty());
                let submitted = writes(&host, 0x83).len();
                assert_eq!(submitted, usize::from(!waiting));
                let get = [0xa1, 1, 0, 1, 2, 0, 32, 0];
                assert_eq!(exchange(&host, running.as_mut(), get, false), [0; 32]);
                send(
                    if transition == "retarget" {
                        Interface::Vial
                    } else {
                        Interface::Via
                    },
                    [3; 32],
                );
                poll(running.as_mut());
                assert_eq!(writes(&host, 0x83)[submitted], [3; 32]);
                assert_eq!(exchange(&host, running.as_mut(), get, false), [0; 32]);
                // Old write cleanup must not clear the fresh packet's active state.
                for _ in 0..3 {
                    poll(running.as_mut());
                }
                assert!(host.0.borrow().active.contains(&0x83));
                assert_eq!(writes(&host, 0x83).len(), submitted + 1);
                host.0
                    .borrow_mut()
                    .active
                    .retain(|address| *address != 0x83);
                poll(running.as_mut());
                assert_eq!(exchange(&host, running.as_mut(), get, false), [3; 32]);
            }
        }
    }
}

#[test]
fn raw_snapshot_ignores_pending_and_stale_write_completions() {
    for transition in [
        "suspend",
        "reset",
        "reconfigure",
        "unconfigure",
        "disconnect",
    ] {
        let host = Host::default();
        let io = Io::new();
        let mut buffers = Buffers::new(&io);
        let usb = Usb::new(
            MockDriver(host.clone()),
            "0000000000000000",
            "Test adapter",
            Topology {
                interfaces: Interface::Via.bit(),
                ..Default::default()
            },
            &io,
            &mut buffers,
        );
        let mut running = pin!(usb.run_until_rebuild());
        configured(&host, running.as_mut());
        let get = [0xa1, 1, 0, 1, 2, 0, 32, 0];
        let send = |bytes| {
            io.raw[0]
                .tx
                .try_send(RawPacket {
                    generation: io.status().generation,
                    interface: Interface::Via,
                    bytes,
                })
                .ok()
                .unwrap()
        };
        send([1; 32]);
        poll(running.as_mut());
        assert_eq!(exchange(&host, running.as_mut(), get, false), [1; 32]);
        host.0.borrow_mut().block_write = true;
        send([3; 32]);
        poll(running.as_mut());
        send([4; 32]);
        assert_eq!(exchange(&host, running.as_mut(), get, false), [1; 32]);
        assert!(
            io.raw[0].tx.is_full(),
            "control reads do not consume queued replies"
        );
        match transition {
            "suspend" => {
                host.0.borrow_mut().events.push_back(Event::Suspend);
                poll(running.as_mut());
                assert!(io.status().suspended);
                host.0.borrow_mut().events.push_back(Event::Resume);
                poll(running.as_mut());
                assert!(!io.status().suspended);
            }
            "reset" => {
                host.0.borrow_mut().events.push_back(Event::Reset);
                poll(running.as_mut());
                assert!(io.raw[0].input.lock(|input| input.borrow().is_none()));
                configured(&host, running.as_mut());
            }
            "reconfigure" => {
                exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
            }
            "unconfigure" => {
                exchange(&host, running.as_mut(), [0, 9, 0, 0, 0, 0, 0, 0], false);
                assert!(io.raw[0].input.lock(|input| input.borrow().is_none()));
                exchange(&host, running.as_mut(), [0, 9, 1, 0, 0, 0, 0, 0], false);
            }
            "disconnect" => {
                host.0.borrow_mut().events.push_back(Event::PowerRemoved);
                poll(running.as_mut());
                assert!(io.raw[0].input.lock(|input| input.borrow().is_none()));
                host.0.borrow_mut().events.push_back(Event::PowerDetected);
                poll(running.as_mut());
                configured(&host, running.as_mut());
            }
            _ => unreachable!(),
        }
        let expected = if transition == "suspend" {
            [1; 32]
        } else {
            [0; 32]
        };
        assert_eq!(exchange(&host, running.as_mut(), get, false), expected);
        assert!(io.raw[0].tx.is_empty());
        host.0.borrow_mut().block_write = false;
        poll(running.as_mut());
        assert_eq!(exchange(&host, running.as_mut(), get, false), expected);
        assert!(io.raw[0].tx.is_empty());
        send([5; 32]);
        poll(running.as_mut());
        assert_eq!(exchange(&host, running.as_mut(), get, false), [5; 32]);
    }
}

#[test]
fn raw_snapshot_clears_when_usb_rebuilds() {
    let io = Io::new();
    for interface in [Interface::Via, Interface::Vial, Interface::Via] {
        let host = Host::default();
        let mut buffers = Buffers::new(&io);
        let usb = Usb::new(
            MockDriver(host.clone()),
            "0000000000000000",
            "Test adapter",
            Topology {
                interfaces: interface.bit(),
                ..Default::default()
            },
            &io,
            &mut buffers,
        );
        let mut running = pin!(usb.run_until_rebuild());
        configured(&host, running.as_mut());
        let get = [0xa1, 1, 0, 1, 2, 0, 32, 0];
        assert_eq!(exchange(&host, running.as_mut(), get, false), [0; 32]);
        for bytes in [[1; 32], [2; 32]] {
            io.raw[0]
                .tx
                .try_send(RawPacket {
                    generation: io.status().generation,
                    interface,
                    bytes,
                })
                .ok()
                .unwrap();
            poll(running.as_mut());
            assert_eq!(exchange(&host, running.as_mut(), get, false), [1; 32]);
            host.0.borrow_mut().block_write = true;
        }
        io.reconnect.signal(Topology::default());
        assert_eq!(
            running
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Topology::default())
        );
        assert!(!io.status().configured);
        assert!(io.raw[0].input.lock(|input| input.borrow().is_none()));
    }
}

#[cfg(feature = "gamepad")]
#[test]
fn raw_control_interface_numbers_follow_gamepad_compositions() {
    for interface in Interface::ALL {
        for (hid_gamepads, xinput_gamepads) in [(1, 0), (0, 1), (1, 1)] {
            let host = Host::default();
            let io = Io::new();
            let mut buffers = Buffers::new(&io);
            let topology = Topology {
                interfaces: interface.bit(),
                gamepads: cordial_core::outputs::Plan {
                    hid_gamepads,
                    xinput_gamepads,
                },
                ..Default::default()
            };
            assert!(topology.fits(io.budget));
            let usb = Usb::new(
                MockDriver(host.clone()),
                "0000000000000000",
                "Test adapter",
                topology,
                &io,
                &mut buffers,
            );
            let mut running = pin!(usb.run_until_rebuild());
            configured(&host, running.as_mut());
            let number = 2 + hid_gamepads + xinput_gamepads;
            assert_eq!(
                exchange(
                    &host,
                    running.as_mut(),
                    [0xa1, 1, 0, 1, number, 0, 32, 0],
                    false
                ),
                [0; 32]
            );
            exchange(
                &host,
                running.as_mut(),
                [0x21, 10, 0, 5, number, 0, 0, 0],
                true,
            );
            let descriptor = exchange(
                &host,
                running.as_mut(),
                [0x81, 6, 0, 0x22, number, 0, 255, 0],
                false,
            );
            assert_eq!(descriptor, configuration::REPORT_DESCRIPTOR);
            if xinput_gamepads != 0 {
                let msos = exchange(
                    &host,
                    running.as_mut(),
                    [0xc0, crate::xinput::MSOS_VENDOR_CODE, 0, 0, 7, 0, 64, 0],
                    false,
                );
                assert_eq!(&msos[..4], &[10, 0, 0, 0]);
            }
        }
    }
}
