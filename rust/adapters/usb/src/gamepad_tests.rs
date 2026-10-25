use crate::{gamepad, xinput};
use core::{
    future::{Future, pending, poll_fn},
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
};
use embassy_futures::block_on;
use embassy_usb::{Builder, Config, Handler, driver::*};
use std::{cell::RefCell, collections::VecDeque, rc::Rc, vec::Vec};

#[derive(Default)]
struct Traffic {
    allocations: Vec<EndpointInfo>,
    writes: Vec<(u8, Vec<u8>)>,
    reads: VecDeque<Vec<u8>>,
    endpoint_reads: VecDeque<(u8, Vec<u8>)>,
    setup: VecDeque<[u8; 8]>,
    response: Vec<u8>,
    control_data: VecDeque<Vec<u8>>,
    rejected: bool,
    fail_write: bool,
    block_write: bool,
    blocked_endpoint: Option<u8>,
    write_budget: Option<usize>,
    write_ticks: u64,
    live: usize,
    disabled: usize,
}

#[derive(Clone, Default)]
struct Host(Rc<RefCell<Traffic>>);

struct MockDriver {
    host: Host,
    next_in: usize,
    next_out: usize,
}

impl MockDriver {
    fn new(host: &Host) -> Self {
        // Different directions need not allocate the same endpoint numbers.
        Self {
            host: host.clone(),
            next_in: 1,
            next_out: 3,
        }
    }

    fn allocate(
        &mut self,
        direction: Direction,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<MockEndpoint, EndpointAllocError> {
        assert!(address.is_none());
        let next = if direction == Direction::In {
            &mut self.next_in
        } else {
            &mut self.next_out
        };
        assert!(*next <= 15);
        let info = EndpointInfo {
            addr: EndpointAddress::from_parts(*next, direction),
            ep_type: kind,
            max_packet_size: size,
            interval_ms: interval,
        };
        *next += 1;
        self.host.0.borrow_mut().allocations.push(info);
        self.host.0.borrow_mut().live += 1;
        Ok(MockEndpoint {
            host: self.host.clone(),
            info,
        })
    }
}

impl<'d> Driver<'d> for MockDriver {
    type EndpointIn = MockEndpoint;
    type EndpointOut = MockEndpoint;
    type ControlPipe = Host;
    type Bus = MockBus;
    fn alloc_endpoint_in(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<MockEndpoint, EndpointAllocError> {
        self.allocate(Direction::In, kind, address, size, interval)
    }
    fn alloc_endpoint_out(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<MockEndpoint, EndpointAllocError> {
        self.allocate(Direction::Out, kind, address, size, interval)
    }
    fn start(self, size: u16) -> (MockBus, Host) {
        assert_eq!(size, 64);
        (MockBus(self.host.clone()), self.host)
    }
}

struct MockBus(Host);
impl Bus for MockBus {
    async fn enable(&mut self) {}
    async fn disable(&mut self) {
        self.0.0.borrow_mut().disabled += 1;
    }
    async fn poll(&mut self) -> Event {
        pending().await
    }
    fn endpoint_set_enabled(&mut self, _: EndpointAddress, _: bool) {}
    fn endpoint_set_stalled(&mut self, _: EndpointAddress, _: bool) {}
    fn endpoint_is_stalled(&mut self, _: EndpointAddress) -> bool {
        false
    }
    async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
        Ok(())
    }
}

struct MockEndpoint {
    host: Host,
    info: EndpointInfo,
}
impl Drop for MockEndpoint {
    fn drop(&mut self) {
        self.host.0.borrow_mut().live -= 1;
    }
}
impl Endpoint for MockEndpoint {
    fn info(&self) -> &EndpointInfo {
        &self.info
    }
    async fn wait_enabled(&mut self) {}
}
impl EndpointIn for MockEndpoint {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), EndpointError> {
        if self.host.0.borrow().fail_write {
            return Err(EndpointError::Disabled);
        }
        poll_fn(|_| {
            let traffic = self.host.0.borrow();
            if traffic.block_write
                || traffic.blocked_endpoint == Some(self.info.addr.into())
                || traffic.write_budget == Some(0)
            {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        assert!(bytes.len() <= self.info.max_packet_size as usize);
        let mut traffic = self.host.0.borrow_mut();
        if let Some(budget) = &mut traffic.write_budget {
            *budget -= 1;
        }
        crate::tests::TICKS.with(|ticks| ticks.set(ticks.get() + traffic.write_ticks));
        traffic.writes.push((self.info.addr.into(), bytes.to_vec()));
        Ok(())
    }
}
impl EndpointOut for MockEndpoint {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, EndpointError> {
        let packet = poll_fn(|_| {
            let mut traffic = self.host.0.borrow_mut();
            let packet = if let Some(index) = traffic
                .endpoint_reads
                .iter()
                .position(|p| p.0 == u8::from(self.info.addr))
            {
                traffic.endpoint_reads.remove(index).map(|p| p.1)
            } else {
                traffic.reads.pop_front()
            };
            packet.map_or(Poll::Pending, Poll::Ready)
        })
        .await;
        bytes[..packet.len()].copy_from_slice(&packet);
        Ok(packet.len())
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
    async fn data_out(&mut self, buf: &mut [u8], _: bool, _: bool) -> Result<usize, EndpointError> {
        let bytes = self
            .0
            .borrow_mut()
            .control_data
            .pop_front()
            .expect("control payload");
        buf[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
    async fn data_in(&mut self, bytes: &[u8], _: bool, _: bool) -> Result<(), EndpointError> {
        self.0.borrow_mut().response.extend_from_slice(bytes);
        Ok(())
    }
    async fn accept(&mut self) {}
    async fn reject(&mut self) {
        self.0.borrow_mut().rejected = true;
    }
    async fn accept_set_address(&mut self, _: u8) {}
}

fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn exchange<F: Future>(host: &Host, running: Pin<&mut F>, request: [u8; 8]) -> Vec<u8> {
    {
        let mut traffic = host.0.borrow_mut();
        traffic.response.clear();
        traffic.rejected = false;
        traffic.setup.push_back(request);
    }
    assert!(poll(running).is_pending());
    assert!(!host.0.borrow().rejected, "request {request:?} rejected");
    host.0.borrow().response.clone()
}

fn descriptors(bytes: &[u8]) -> Vec<&[u8]> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let size = bytes[offset] as usize;
        assert!(size >= 2 && offset + size <= bytes.len());
        result.push(&bytes[offset..offset + size]);
        offset += size;
    }
    result
}

#[test]
fn boot_keyboard_switches_formats_and_preserves_leds_and_idle_reports() {
    let host = Host::default();
    let io = crate::Io::new();
    let mut buffers = crate::Buffers::new(&io);
    let usb = crate::Usb::new(
        MockDriver::new(&host),
        "0000000000000000",
        "Test adapter",
        crate::Topology {
            desktop: true,
            ..Default::default()
        },
        &io,
        &mut buffers,
    );
    let endpoint = host
        .0
        .borrow()
        .allocations
        .iter()
        .find(|e| e.ep_type == EndpointType::Interrupt && e.max_packet_size == 64)
        .unwrap()
        .addr
        .into();
    crate::BusHandler(&io).configured(true);
    let mut running = pin!(usb.run_until_rebuild());
    let config = exchange(&host, running.as_mut(), [0x80, 6, 0, 2, 0, 0, 255, 0]);
    let parts = descriptors(&config);
    let keyboard = parts.iter().find(|d| d[1] == 4 && d[2] == 2).unwrap();
    assert_eq!(&keyboard[5..8], &[3, 1, 1]);
    assert_eq!(host.0.borrow().allocations.len(), 4);

    let mut keys = [0; 32];
    keys[0] = 1 << 4;
    keys[28] = 3;
    let send = |id: u8, data: &[u8], running: Pin<&mut _>| {
        let mut bytes = [0; 69];
        bytes[0] = id;
        bytes[1..1 + data.len()].copy_from_slice(data);
        io.hid_tx
            .try_send(crate::HidTx {
                generation: io.status().generation,
                sequence: 1,
                bytes,
                length: 1 + data.len(),
            })
            .ok()
            .unwrap();
        assert!(poll(running).is_pending());
        assert!(io.hid_done.try_receive().unwrap().success);
    };
    send(1, &keys, running.as_mut());
    assert_eq!(
        writes(&host, endpoint).last().unwrap(),
        &[&[1][..], &keys].concat()
    );
    exchange(&host, running.as_mut(), [0x21, 11, 0, 0, 2, 0, 0, 0]);
    assert_eq!(
        exchange(&host, running.as_mut(), [0xa1, 3, 0, 0, 2, 0, 1, 0]),
        [0]
    );
    send(1, &keys, running.as_mut());
    assert_eq!(
        writes(&host, endpoint).last().unwrap(),
        &[3, 0, 4, 0, 0, 0, 0, 0]
    );
    let before = writes(&host, endpoint).len();
    send(2, &[0; 10], running.as_mut());
    assert_eq!(writes(&host, endpoint).len(), before);
    assert_eq!(
        exchange(&host, running.as_mut(), [0xa1, 1, 0, 1, 2, 0, 8, 0]),
        [3, 0, 4, 0, 0, 0, 0, 0]
    );
    host.0.borrow_mut().control_data.push_back(vec![2]);
    exchange(&host, running.as_mut(), [0x21, 9, 0, 2, 2, 0, 1, 0]);
    assert_eq!(io.status().leds, 2);
    exchange(&host, running.as_mut(), [0x21, 10, 0, 5, 2, 0, 0, 0]);
    assert_eq!(
        exchange(&host, running.as_mut(), [0xa1, 2, 0, 0, 2, 0, 1, 0]),
        [5]
    );
    crate::tests::TICKS.with(|ticks| ticks.set(embassy_time::Duration::from_millis(21).as_ticks()));
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(writes(&host, endpoint).len(), before + 1);
    assert_eq!(
        writes(&host, endpoint).last().unwrap(),
        &[3, 0, 4, 0, 0, 0, 0, 0]
    );
    exchange(&host, running.as_mut(), [0x21, 11, 1, 0, 2, 0, 0, 0]);
    send(1, &[0; 32], running.as_mut());
    send(2, &[0; 10], running.as_mut());
    assert_eq!(
        writes(&host, endpoint).last().unwrap(),
        &[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    let before = writes(&host, endpoint).len();
    crate::tests::TICKS.with(|ticks| ticks.set(embassy_time::Duration::from_millis(42).as_ticks()));
    send(2, &[0; 10], running.as_mut());
    let packets = writes(&host, endpoint);
    assert_eq!(packets.len(), before + 2);
    assert_eq!(packets[before], [&[1][..], &[0; 32]].concat());
    assert_eq!(packets[before + 1][0], 2);
    exchange(&host, running.as_mut(), [0x21, 10, 0, 5, 2, 0, 0, 0]);
    assert_eq!(
        exchange(&host, running.as_mut(), [0xa1, 2, 0, 0, 2, 0, 1, 0]),
        [5]
    );
    crate::BusHandler(&io).reset();
    assert!(!io.status().boot_keyboard);
}

#[test]
fn overdue_idle_reports_allow_queued_key_presses_and_releases() {
    let host = Host::default();
    let io = crate::Io::new();
    let mut buffers = crate::Buffers::new(&io);
    let usb = crate::Usb::new(
        MockDriver::new(&host),
        "0000000000000000",
        "Test adapter",
        crate::Topology {
            desktop: true,
            ..Default::default()
        },
        &io,
        &mut buffers,
    );
    crate::BusHandler(&io).configured(true);
    let mut running = pin!(usb.run_until_rebuild());
    exchange(&host, running.as_mut(), [0x21, 10, 0, 1, 2, 0, 0, 0]);
    crate::tests::TICKS.with(|ticks| ticks.set(embassy_time::Duration::from_millis(20).as_ticks()));
    host.0.borrow_mut().write_ticks = embassy_time::Duration::from_millis(1).as_ticks();
    for (sequence, key) in [(1, 16), (2, 0)] {
        let mut bytes = [0; 69];
        bytes[0] = 1;
        bytes[1] = key;
        io.hid_tx
            .try_send(crate::HidTx {
                generation: io.status().generation,
                sequence,
                bytes,
                length: 33,
            })
            .ok()
            .unwrap();
        host.0.borrow_mut().write_budget = Some(4);
        assert!(poll(running.as_mut()).is_pending());
        let done = io
            .hid_done
            .try_receive()
            .expect("live input progresses despite overdue idle reports");
        assert!(done.success);
        assert_eq!(done.sequence, sequence);
        assert_eq!(io.reports.lock(|reports| reports.borrow()[0][0]), key);
    }
}

#[test]
fn composite_pools_use_allocated_endpoints_and_one_receiver_function() {
    for hid_count in 0..=4 {
        for xinput_count in 0..=4 {
            for set in [0, crate::Interface::Via.bit(), crate::Interface::Vial.bit()] {
                let host = Host::default();
                let io = crate::Io::new();
                let mut buffers = crate::Buffers::new(&io);
                let mut usb = crate::Usb::new(
                    MockDriver::new(&host),
                    "0000000000000000",
                    "Test adapter",
                    crate::Topology {
                        desktop: true,
                        interfaces: set,
                        gamepads: cordial_core::outputs::Plan {
                            hid_gamepads: hid_count as u8,
                            xinput_gamepads: xinput_count as u8,
                        },
                    },
                    &io,
                    &mut buffers,
                );
                let channels = &usb.receiver;
                let mut running = pin!(usb.device.run());
                let device_descriptor =
                    exchange(&host, running.as_mut(), [0x80, 6, 0, 1, 0, 0, 18, 0]);
                assert_eq!(&device_descriptor[4..7], &[0xef, 2, 1]);
                assert_eq!(&device_descriptor[8..12], &[9, 0x12, 0xd1, 0xc0]);
                let full;
                {
                    let bytes =
                        exchange(&host, running.as_mut(), [0x80, 6, 0, 2, 0, 0, 0xff, 0xff]);
                    let length = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
                    assert_eq!(length, bytes.len());
                    assert!(length <= 512);
                    assert_eq!(length, usize::from(io.topology().cost().config_bytes));
                    assert_eq!(
                        bytes[4] as usize,
                        3 + hid_count + xinput_count + usize::from(set != 0)
                    );
                    let parts = descriptors(&bytes);
                    let receiver_iads: Vec<_> = parts
                        .iter()
                        .filter(|d| d[1] == 11 && d[4..7] == [0xff, 0x5d, 0x81])
                        .collect();
                    assert_eq!(receiver_iads.len(), usize::from(xinput_count != 0));
                    if xinput_count != 0 {
                        assert_eq!(
                            &receiver_iads[0][2..4],
                            &[(3 + hid_count) as u8, xinput_count as u8]
                        );
                    }
                    let interfaces: Vec<_> = parts.iter().filter(|d| d[1] == 4).collect();
                    for (i, interface) in interfaces.iter().enumerate() {
                        assert_eq!(interface[2] as usize, i);
                    }
                    let capabilities: Vec<_> = parts.iter().filter(|d| d[1] == 0x22).collect();
                    assert_eq!(capabilities.len(), xinput_count);
                    for (index, channel) in channels.iter().flatten().enumerate() {
                        assert_eq!(channel.interface as usize, 3 + hid_count + index);
                        assert_eq!(
                            &interfaces[channel.interface as usize][4..8],
                            &[2, 0xff, 0x5d, 0x81]
                        );
                        assert_eq!(capabilities[index][5], u8::from(channel.input.info().addr));
                        assert_eq!(
                            capabilities[index][13],
                            u8::from(channel.output.info().addr)
                        );
                        assert_eq!(channel.input.info().max_packet_size, 32);
                        assert_eq!(channel.output.info().max_packet_size, 32);
                        assert_eq!(channel.input.info().interval_ms, 1);
                        assert_eq!(channel.output.info().interval_ms, 8);
                    }
                    full = bytes;
                }
                let endpoints: Vec<_> = descriptors(&full)
                    .into_iter()
                    .filter(|d| d[1] == 5)
                    .collect();
                let traffic = host.0.borrow();
                assert_eq!(endpoints.len(), traffic.allocations.len());
                for (desc, allocation) in endpoints.iter().zip(&traffic.allocations) {
                    assert_eq!(desc[2], u8::from(allocation.addr));
                    assert_eq!(
                        u16::from_le_bytes([desc[4], desc[5]]),
                        allocation.max_packet_size
                    );
                }
                assert_eq!(
                    traffic
                        .allocations
                        .iter()
                        .filter(|e| e.addr.is_in())
                        .count(),
                    3 + hid_count + xinput_count + usize::from(set != 0)
                );
                assert_eq!(
                    traffic
                        .allocations
                        .iter()
                        .filter(|e| e.addr.is_out())
                        .count(),
                    1 + xinput_count + usize::from(set != 0)
                );
                drop(traffic);
                if xinput_count > 0 {
                    let bos = exchange(&host, running.as_mut(), [0x80, 6, 0, 15, 0, 0, 64, 0]);
                    let capabilities = descriptors(&bos);
                    let os_capability = capabilities
                        .iter()
                        .find(|d| d[1] == 16 && d[2] == 5)
                        .unwrap();
                    assert_eq!(&os_capability[20..24], &[0, 0, 3, 6]);
                    assert_eq!(
                        &os_capability[24..28],
                        &[46, 0, xinput::MSOS_VENDOR_CODE, 0]
                    );
                    let os = exchange(
                        &host,
                        running.as_mut(),
                        [0xc0, xinput::MSOS_VENDOR_CODE, 0, 0, 7, 0, 64, 0],
                    );
                    assert_eq!(os.len(), 46);
                    assert_eq!(&os[8..10], &[46, 0]);
                    assert_eq!(&os[18..26], &[8, 0, 2, 0, (3 + hid_count) as u8, 0, 28, 0]);
                    assert_eq!(&os[26..30], &[20, 0, 3, 0]);
                    assert_eq!(&os[30..38], b"XUSB20\0\0");
                    assert_eq!(&os[38..], &[0; 8]);
                }
                if hid_count == 4 && xinput_count == 4 && set != 0 {
                    assert_eq!(full.len(), 455);
                }
            }
        }
    }
}

#[test]
fn hid_control_reports_and_interrupt_packets_preserve_neutral_and_extremes() {
    let host = Host::default();
    let reports = gamepad::Reports::new();
    let mut state = gamepad::State::new(&reports);
    let (mut config, mut bos, mut control) = ([0; 128], [0; 64], [0; 256]);
    let mut builder = Builder::new(
        MockDriver::new(&host),
        Config::new(0x1209, 0xc0d1),
        &mut config,
        &mut bos,
        &mut [],
        &mut control,
    );
    let mut pad = gamepad::Gamepad::new(&mut builder, &mut state);
    let mut device = builder.build();
    let mut running = pin!(device.run());
    assert_eq!(
        exchange(&host, running.as_mut(), [0x81, 6, 0, 0x22, 0, 0, 255, 0]),
        gamepad::REPORT_DESCRIPTOR
    );
    assert_eq!(
        exchange(
            &host,
            running.as_mut(),
            [0xa1, 1, 0, 1, 0, 0, gamepad::REPORT_LEN as u8, 0]
        ),
        gamepad::Report::NEUTRAL.as_bytes()
    );
    let axes = [0, u16::MAX, 32767, 32769, 34002, 27090, 0, 0];
    let report = gamepad::Report::encode(u32::MAX, &axes, 0x3f, Some(7));
    assert_eq!(&report.as_bytes()[..4], &[255; 4]);
    for (i, axis) in axes[..6].iter().enumerate() {
        assert_eq!(
            &report.as_bytes()[4 + i * 2..6 + i * 2],
            &((i32::from(*axis) - 32768) as i16).to_le_bytes()
        );
    }
    assert_eq!(report.as_bytes()[16], 7);
    reports.set(report);
    assert_eq!(
        exchange(
            &host,
            running.as_mut(),
            [0xa1, 1, 0, 1, 0, 0, gamepad::REPORT_LEN as u8, 0]
        ),
        report.as_bytes()
    );
    block_on(pad.ready());
    block_on(pad.write(report)).unwrap();
    reports.set(gamepad::Report::NEUTRAL);
    block_on(pad.write(gamepad::Report::NEUTRAL)).unwrap();
    assert_eq!(
        host.0.borrow().writes.last().unwrap().1,
        gamepad::Report::NEUTRAL.as_bytes()
    );
    for invalid in [None, Some(8), Some(255)] {
        assert_eq!(
            gamepad::Report::encode(0, &axes, 0, invalid),
            gamepad::Report::NEUTRAL
        );
    }
    let masked = gamepad::Report::encode(0, &axes, 1, None);
    assert_eq!(&masked.as_bytes()[4..6], &i16::MIN.to_le_bytes());
    assert_eq!(&masked.as_bytes()[6..16], &[0; 10]);
    assert!(poll(pin!(reports.changed()).as_mut()).is_ready());
    assert!(poll(pin!(reports.changed()).as_mut()).is_pending());
    exchange(&host, running.as_mut(), [0x21, 10, 0, 5, 0, 0, 0, 0]);
    assert_eq!(reports.idle_ms(), 20);
    assert!(poll(pin!(reports.changed()).as_mut()).is_ready());
    assert_eq!(
        exchange(&host, running.as_mut(), [0xa1, 2, 0, 0, 0, 0, 1, 0]),
        [5]
    );
    reports.reset();
    assert_eq!(reports.idle_ms(), u32::MAX);
    assert_eq!(
        exchange(&host, running.as_mut(), [0xa1, 3, 0, 0, 0, 0, 1, 0]),
        [1]
    );
}

#[test]
fn xinput_fixed_mapping_has_safe_absent_controls_and_full_ranges() {
    let all = canonical_xinput(
        0x7cdb,
        &[0, 0, 0, u16::MAX, u16::MAX, 0, 0, u16::MAX],
        0xdb,
        Some(1),
    );
    assert_eq!(
        all.as_bytes(),
        &[0xf9, 0xf7, 0, 255, 0, 0x80, 0xff, 0x7f, 0xff, 0x7f, 1, 0x80]
    );
    assert_eq!(
        canonical_xinput(0, &[u16::MAX; 8], 0, None),
        xinput::Input::default()
    );
    for (hat, bits) in [1, 9, 8, 10, 2, 6, 4, 5].into_iter().enumerate() {
        assert_eq!(
            canonical_xinput(0, &[0; 8], 0, Some(hat as u8)).as_bytes()[0],
            bits
        );
    }
    assert_eq!(
        canonical_xinput(1 << 31, &[0; 8], 0, Some(255)),
        xinput::Input::default()
    );
}

#[test]
fn canonical_state_reaches_both_usb_endpoints() {
    let host = Host::default();
    let reports = gamepad::Reports::new();
    let mut state = gamepad::State::new(&reports);
    let mut control = xinput::Control::new([0; 7]);
    let (mut config, mut bos, mut msos, mut control_buf) = ([0; 256], [0; 64], [0; 64], [0; 256]);
    let mut builder = Builder::new(
        MockDriver::new(&host),
        Config::new(0x1209, 0xc0d1),
        &mut config,
        &mut bos,
        &mut msos,
        &mut control_buf,
    );
    let mut hid = gamepad::Gamepad::new(&mut builder, &mut state);
    let mut channel = xinput::new(&mut builder, &mut control, 1)[0]
        .take()
        .unwrap();
    let _device = builder.build();
    let mut protocol = xinput::Protocol::new([0; 6]);
    protocol.assign(true);
    assert_eq!(
        block_on(protocol.write(&mut channel.input)),
        Ok(Some(xinput::Sent::Presence))
    );

    // A/B/X/Y/LB/RB/Back/Start/LS/RS/Guide keep their HID usage positions.
    let buttons = [
        (1, 0x1000u16),
        (2, 0x2000),
        (5, 0x4000),
        (4, 0x8000),
        (7, 0x0100),
        (8, 0x0200),
        (11, 0x0020),
        (12, 0x0010),
        (14, 0x0040),
        (15, 0x0080),
        (13, 0x0400),
    ];
    for (bits, expected) in (1..=32)
        .map(|usage| {
            let expected = buttons
                .iter()
                .find(|&&(source, _)| source == usage)
                .map_or(0, |&(_, target)| target);
            (1 << (usage - 1), expected)
        })
        .chain([(0x7cdb, 0xf7f0), (0, 0)])
    {
        let mut pad = cordial_core::hid::Gamepad {
            buttons: bits,
            ..Default::default()
        };
        let mut expected_hid = *gamepad::Report::NEUTRAL.as_bytes();
        let mut expected_xinput = [0; 12];
        if bits != 0 {
            pad.axes = [0, 65535, 0, 65535, 0, 0, 65535, 257];
            pad.axes_valid = 0xdb;
            expected_hid[4..16].copy_from_slice(&[0, 128, 255, 127, 0, 0, 255, 127, 0, 128, 0, 0]);
            expected_hid[18..22].copy_from_slice(&[255, 255, 1, 1]);
            expected_xinput[2..].copy_from_slice(&[255, 1, 0, 128, 1, 128, 255, 127, 255, 127]);
        }
        let report = gamepad::Report::encode(pad.buttons, &pad.axes, pad.axes_valid, pad.hat);
        expected_hid[..4].copy_from_slice(&bits.to_le_bytes());
        assert_eq!(report.as_bytes(), &expected_hid);
        block_on(hid.write(report)).unwrap();
        assert_eq!(host.0.borrow().writes.last().unwrap().1, expected_hid);

        let input = xinput::Input::encode(&pad);
        expected_xinput[..2].copy_from_slice(&expected.to_le_bytes());
        assert_eq!(input.as_bytes(), &expected_xinput);
        protocol.update(input);
        assert_eq!(
            block_on(protocol.write(&mut channel.input)),
            Ok(Some(xinput::Sent::Input))
        );
        let mut expected_packet = [0; 29];
        expected_packet[1] = 1;
        expected_packet[3] = 0xf0;
        expected_packet[5] = 0x13;
        expected_packet[6..18].copy_from_slice(&expected_xinput);
        let traffic = host.0.borrow();
        let (endpoint, packet) = traffic.writes.last().unwrap();
        assert_eq!(*endpoint, u8::from(channel.input.info().addr));
        assert_eq!(packet, &expected_packet);
    }
}

#[test]
fn canonical_axes_preserve_validity_ranges_and_transport_y_direction() {
    let centered = [32768, 32768, 32768, 32768, 32768, 32768, 0, 0];
    assert_eq!(
        gamepad::Report::encode(0, &centered, 0xff, None),
        gamepad::Report::NEUTRAL
    );
    assert_eq!(
        canonical_xinput(0, &centered, 0xff, None),
        xinput::Input::default()
    );
    assert_eq!(
        gamepad::Report::encode(0, &[u16::MAX; 8], 0, None),
        gamepad::Report::NEUTRAL
    );
    for (value, signed, inverted) in [
        (0, i16::MIN, i16::MAX),
        (32767, -1, 1),
        (32768, 0, 0),
        (32769, 1, -1),
        (u16::MAX, i16::MAX, -32767),
    ] {
        for (index, hid_offset) in [4, 6, 8, 10, 12, 14, 18, 20].into_iter().enumerate() {
            let axes = [value; 8];
            let valid = 1 << index;
            let mut expected_hid = *gamepad::Report::NEUTRAL.as_bytes();
            let hid_axis = if index < 6 {
                signed.to_le_bytes()
            } else {
                value.to_le_bytes()
            };
            expected_hid[hid_offset..hid_offset + 2].copy_from_slice(&hid_axis);
            assert_eq!(
                gamepad::Report::encode(0, &axes, valid, None).as_bytes(),
                &expected_hid
            );

            let mut expected_xinput = [0; 12];
            match index {
                0 | 1 | 3 | 4 => {
                    let offset = match index {
                        0 => 4,
                        1 => 6,
                        3 => 8,
                        _ => 10,
                    };
                    let axis = if index == 1 || index == 4 {
                        inverted
                    } else {
                        signed
                    };
                    expected_xinput[offset..offset + 2].copy_from_slice(&axis.to_le_bytes());
                }
                6 | 7 => {
                    expected_xinput[index - 4] = if value == u16::MAX {
                        255
                    } else if value == 0 {
                        0
                    } else {
                        127
                    }
                }
                _ => {}
            }
            assert_eq!(
                canonical_xinput(0, &axes, valid, None).as_bytes(),
                &expected_xinput
            );
        }
    }
    for (value, expected) in [(0, 0), (256, 0), (257, 1), (65535, 255)] {
        assert_eq!(
            &canonical_xinput(0, &[value; 8], 0xc0, None).as_bytes()[2..4],
            &[expected; 2]
        );
    }
}

#[test]
fn gamepad_reports_preserve_usage_identity_and_simultaneous_controls() {
    let axes = [0, 65535, 32768, 1, 65534, 16384, 0, 0];
    let report =
        gamepad::Report::encode(0x8000_0001, &axes, 0x3f, Some(3)).with_controls(0xff, true, true);
    assert_eq!(report.as_bytes()[17], 0x3f);
    let map = cordial_core::hid::Map::compile(gamepad::REPORT_DESCRIPTOR).unwrap();
    let mut state = map.state().unwrap();
    map.decode(&mut state, 0, report.as_bytes()).unwrap();
    let decoded = map.gamepad(&state);
    assert_eq!(decoded.buttons, 0x8000_0001);
    assert_eq!(decoded.axes, axes);
    assert_eq!(decoded.axes_valid, 0xff);
    assert_eq!(decoded.hat, Some(3));
    assert_eq!(decoded.dpad, 0x0f);
    assert!(decoded.start && decoded.select);
    map.decode(&mut state, 0, gamepad::Report::NEUTRAL.as_bytes())
        .unwrap();
    let released = map.gamepad(&state);
    assert_eq!(released.buttons, 0);
    assert_eq!(
        released.axes,
        [32768, 32768, 32768, 32768, 32768, 32768, 0, 0]
    );
    assert_eq!(released.hat, None);
    assert_eq!(released.dpad, 0);
    assert!(!released.start && !released.select);

    let explicit = xinput::Input::default().with_controls(0xff, true, true);
    assert_eq!(explicit.as_bytes()[0], 0x3f);
}

#[test]
fn receiver_presence_ack_reset_and_reassignment_use_actual_endpoints() {
    let host = Host::default();
    let (mut config, mut bos, mut msos, mut control_buf) = ([0; 256], [0; 64], [0; 64], [0; 256]);
    let mut control = xinput::Control::new([0; 7]);
    let mut builder = Builder::new(
        MockDriver::new(&host),
        Config::new(0x1209, 0xc0d1),
        &mut config,
        &mut bos,
        &mut msos,
        &mut control_buf,
    );
    let mut channel = xinput::new(&mut builder, &mut control, 1)[0]
        .take()
        .unwrap();
    let mut device = builder.build();
    let mut running = pin!(device.run());
    assert_eq!(
        exchange(&host, running.as_mut(), [0xc0, 1, 1, 0, 0, 0, 7, 0]),
        [0; 7]
    );
    assert_eq!(
        exchange(&host, running.as_mut(), [0xc0, 1, 1, 0, 0, 0, 3, 0]),
        [0; 3]
    );
    let mut protocol = xinput::Protocol::new([0; 6]);
    let send = |p: &mut xinput::Protocol, ep: &mut MockEndpoint| block_on(p.write(ep)).unwrap();
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Presence)
    );
    assert_eq!(host.0.borrow().writes.last().unwrap().1, [8, 8]);
    let mut status = [0; 12];
    status[..4].copy_from_slice(&[8, 0, 15, 0xc0]);
    assert!(protocol.receive(&status));
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Presence)
    );
    assert_eq!(host.0.borrow().writes.last().unwrap().1, [8, 8]);
    let mut ack = [0; 12];
    ack[3] = 0x40;
    assert!(protocol.receive(&ack));
    assert!(!protocol.pending());
    protocol.assign(true);
    let input = canonical_xinput(1, &[0; 8], 0, Some(0));
    protocol.update(input);
    host.0.borrow_mut().fail_write = true;
    assert_eq!(
        block_on(protocol.write(&mut channel.input)),
        Err(EndpointError::Disabled)
    );
    assert!(protocol.pending());
    host.0.borrow_mut().fail_write = false;
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Presence)
    );
    assert_eq!(host.0.borrow().writes.last().unwrap().1, [8, 0x80]);
    // Linux starts consuming input without the Windows controller-info ACKs.
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Input)
    );
    assert_eq!(host.0.borrow().writes.last().unwrap().1.len(), 29);
    assert_eq!(
        &host.0.borrow().writes.last().unwrap().1[6..18],
        input.as_bytes()
    );
    assert!(protocol.receive(&ack));
    host.0.borrow_mut().fail_write = true;
    assert_eq!(
        block_on(protocol.write(&mut channel.input)),
        Err(EndpointError::Disabled)
    );
    host.0.borrow_mut().fail_write = false;
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Information)
    );
    let info = host.0.borrow().writes.last().unwrap().1.clone();
    assert_eq!(&info[..6], &[0, 15, 0, 0xf0, 0xf0, 0xcc]);
    assert_eq!(info[16], 0x13);
    assert!(protocol.receive(&ack));
    assert!(!protocol.pending());
    protocol.idle();
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Idle)
    );
    assert_eq!(host.0.borrow().writes.last().unwrap().1[1], 0);
    protocol.reset();
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Presence)
    );
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Input)
    );
    assert_eq!(
        &host.0.borrow().writes.last().unwrap().1[6..18],
        input.as_bytes()
    );
    protocol.receive(&ack);
    protocol.assign(false);
    assert_eq!(
        send(&mut protocol, &mut channel.input),
        Some(xinput::Sent::Presence)
    );
    assert_eq!(host.0.borrow().writes.last().unwrap().1, [8, 8]);
    assert!(!protocol.pending());
    protocol.assign(true);
    send(&mut protocol, &mut channel.input);
    send(&mut protocol, &mut channel.input);
    assert_eq!(&host.0.borrow().writes.last().unwrap().1[6..18], &[0; 12]);
    let mut led = [0; 12];
    led[..4].copy_from_slice(&[0, 0, 8, 0x42]);
    let mut rumble = [0; 12];
    rumble[..7].copy_from_slice(&[0, 1, 15, 0xc0, 0, 255, 255]);
    for packet in [led, rumble] {
        host.0.borrow_mut().reads.push_back(packet.to_vec());
        let mut received = [0; 32];
        let length = block_on(channel.output.read(&mut received)).unwrap();
        assert!(protocol.receive(&received[..length]));
        assert!(!protocol.pending());
    }
    assert!(!protocol.receive(&ack[..11]));
    assert!(!protocol.receive(&[255; 12]));
    // An unread IN endpoint does not stop OUT reception; canceled input stays pending.
    protocol.update(input);
    host.0.borrow_mut().block_write = true;
    {
        let mut writing = pin!(protocol.write(&mut channel.input));
        assert!(poll(writing.as_mut()).is_pending());
        host.0.borrow_mut().reads.push_back(status.to_vec());
        let mut received = [0; 32];
        assert_eq!(block_on(channel.output.read(&mut received)).unwrap(), 12);
    }
    host.0.borrow_mut().block_write = false;
    assert!(protocol.pending());
    protocol.assign(false);
    send(&mut protocol, &mut channel.input);
    assert!(!protocol.pending());
    assert_eq!(host.0.borrow().writes.last().unwrap().1, [8, 8]);
}

fn snapshot(link: u64, epoch: u64, buttons: u32) -> cordial_core::outputs::SlotSnapshot {
    cordial_core::outputs::SlotSnapshot {
        occupant: Some(cordial_core::link::LinkId {
            slot: link as u8,
            generation: epoch,
        }),
        generation: epoch,
        gamepad: cordial_core::hid::Gamepad {
            buttons,
            ..Default::default()
        },
    }
}
fn publish(
    io: &crate::Io,
    hid: cordial_core::outputs::SlotSnapshot,
    xinput: cordial_core::outputs::SlotSnapshot,
) {
    let mut snapshots = cordial_core::outputs::Snapshots::default();
    snapshots.hid[0] = hid;
    snapshots.xinput[0] = xinput;
    io.outputs.publish(io.status().generation, snapshots);
}
fn writes(host: &Host, endpoint: u8) -> Vec<Vec<u8>> {
    host.0
        .borrow()
        .writes
        .iter()
        .filter(|w| w.0 == endpoint)
        .map(|w| w.1.clone())
        .collect()
}

#[test]
fn hid_workers_and_control_reads_encode_mapped_directions() {
    let host = Host::default();
    let io = crate::Io::new();
    let mut buffers = crate::Buffers::new(&io);
    let usb = crate::Usb::new(
        MockDriver::new(&host),
        "0000000000000000",
        "Test adapter",
        crate::Topology {
            desktop: true,
            interfaces: 0,
            gamepads: cordial_core::outputs::Plan {
                hid_gamepads: 1,
                xinput_gamepads: 0,
            },
        },
        &io,
        &mut buffers,
    );
    let endpoint = host
        .0
        .borrow()
        .allocations
        .iter()
        .find(|e| e.max_packet_size == gamepad::REPORT_LEN as u16)
        .unwrap()
        .addr
        .into();
    crate::BusHandler(&io).configured(true);
    let mut running = pin!(usb.run_until_rebuild());
    assert!(poll(running.as_mut()).is_pending());
    for (dpad, hat) in [8, 0, 4, 8, 2, 1, 3, 2, 6, 7, 5, 6, 8, 0, 4, 8]
        .into_iter()
        .enumerate()
    {
        let mut slot = snapshot(1, 1, 0x8000_0001);
        slot.gamepad.dpad = dpad as u8;
        slot.gamepad.start = true;
        slot.gamepad.select = true;
        slot.gamepad.hat = Some(3);
        host.0.borrow_mut().blocked_endpoint = Some(endpoint);
        publish(&io, slot, Default::default());
        let mut expected = *gamepad::Report::NEUTRAL.as_bytes();
        expected[..4].copy_from_slice(&0x8000_0001u32.to_le_bytes());
        expected[16] = hat;
        expected[17] = dpad as u8 | 0x30;
        assert_eq!(
            exchange(
                &host,
                running.as_mut(),
                [0xa1, 1, 0, 1, 3, 0, gamepad::REPORT_LEN as u8, 0]
            ),
            expected,
            "control D-pad {dpad:#x}"
        );
        host.0.borrow_mut().blocked_endpoint = None;
        assert!(poll(running.as_mut()).is_pending());
        assert_eq!(
            writes(&host, endpoint).last().unwrap(),
            &expected,
            "interrupt D-pad {dpad:#x}"
        );
    }
}

#[test]
fn workers_settle_reassignment_keep_control_truth_and_leave_cdc_and_keyboard_running() {
    let host = Host::default();
    let io = crate::Io::new();
    let topology = crate::Topology {
        desktop: true,
        interfaces: 0,
        gamepads: cordial_core::outputs::Plan {
            hid_gamepads: 1,
            xinput_gamepads: 1,
        },
    };
    let mut buffers = crate::Buffers::new(&io);
    let usb = crate::Usb::new(
        MockDriver::new(&host),
        "0000000000000000",
        "Test adapter",
        topology,
        &io,
        &mut buffers,
    );
    let pad = host
        .0
        .borrow()
        .allocations
        .iter()
        .find(|e| e.max_packet_size == gamepad::REPORT_LEN as u16)
        .unwrap()
        .addr
        .into();
    let receiver = usb.receiver[0].as_ref().unwrap();
    let rx_in = u8::from(receiver.input.info().addr);
    let rx_out = u8::from(receiver.output.info().addr);
    crate::BusHandler(&io).configured(true);
    io.dtr(true);
    publish(&io, snapshot(1, 1, 1), snapshot(2, 2, 1));
    let mut running = pin!(usb.run_until_rebuild());
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(writes(&host, pad)[0], gamepad::Report::NEUTRAL.as_bytes());
    assert_eq!(writes(&host, pad).last().unwrap()[0], 1);
    assert_eq!(writes(&host, rx_in)[0], [8, 0x80]);

    host.0.borrow_mut().blocked_endpoint = Some(pad);
    publish(&io, snapshot(1, 1, 2), snapshot(2, 2, 1));
    assert!(poll(running.as_mut()).is_pending());
    publish(&io, snapshot(3, 3, 4), snapshot(2, 2, 1));
    assert_eq!(io.outputs.reports(0).current().as_bytes()[0], 4);
    io.hid_tx
        .try_send(crate::HidTx {
            generation: io.status().generation,
            sequence: 1,
            bytes: [1; 69],
            length: 2,
        })
        .ok()
        .unwrap();
    io.serial_tx
        .try_send(crate::SerialTx {
            session: io.status().session,
            token: None,
            bytes: [7; 63],
            length: 1,
        })
        .ok()
        .unwrap();
    assert!(poll(running.as_mut()).is_pending());
    assert!(io.hid_done.try_receive().unwrap().success);
    assert_eq!(io.serial_done.try_receive().unwrap().length, 1);
    host.0.borrow_mut().blocked_endpoint = None;
    assert!(poll(running.as_mut()).is_pending());
    let packets = writes(&host, pad);
    assert!(
        !packets.iter().any(|p| p[0] == 2),
        "queued prior-owner input is discarded"
    );
    assert_eq!(
        packets[packets.len() - 2],
        gamepad::Report::NEUTRAL.as_bytes()
    );
    assert_eq!(packets.last().unwrap()[0], 4);

    host.0.borrow_mut().blocked_endpoint = Some(rx_in);
    publish(&io, snapshot(3, 3, 4), snapshot(2, 2, 2));
    assert!(poll(running.as_mut()).is_pending());
    // OUT consumes LED and rumble while the host leaves this IN endpoint unread.
    for bytes in [
        [0, 0, 8, 0x40, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 1, 0x0f, 0xc0, 0, 7, 8, 0, 0, 0, 0, 0],
    ] {
        host.0
            .borrow_mut()
            .endpoint_reads
            .push_back((rx_out, bytes.to_vec()));
    }
    assert!(poll(running.as_mut()).is_pending());
    assert!(host.0.borrow().endpoint_reads.is_empty());
    publish(&io, snapshot(3, 3, 4), snapshot(4, 4, 16));
    host.0.borrow_mut().blocked_endpoint = None;
    assert!(poll(running.as_mut()).is_pending());
    let packets = writes(&host, rx_in);
    assert_eq!(packets[packets.len() - 3], [8, 8]);
    assert_eq!(packets[packets.len() - 2], [8, 0x80]);
    assert_eq!(&packets.last().unwrap()[6..8], &[0, 0x40]);

    crate::BusHandler(&io).suspended(true);
    assert!(poll(running.as_mut()).is_pending());
    let before = host.0.borrow().writes.len();
    publish(&io, snapshot(3, 3, 8), snapshot(4, 4, 8));
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(before, host.0.borrow().writes.len());
    crate::BusHandler(&io).suspended(false);
    publish(&io, snapshot(3, 3, 8), snapshot(4, 4, 8));
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(writes(&host, pad).last().unwrap()[0], 8);
    assert_eq!(&writes(&host, rx_in).last().unwrap()[6..8], &[0, 0x80]);

    crate::BusHandler(&io).reset();
    assert!(poll(running.as_mut()).is_pending());
    crate::BusHandler(&io).configured(true);
    publish(&io, snapshot(3, 3, 8), snapshot(4, 4, 8));
    assert!(poll(running.as_mut()).is_pending());
    let packets = writes(&host, rx_in);
    assert_eq!(packets[packets.len() - 2], [8, 0x80]);
    publish(&io, Default::default(), Default::default());
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(
        writes(&host, pad).last().unwrap(),
        gamepad::Report::NEUTRAL.as_bytes()
    );
    assert_eq!(writes(&host, rx_in).last().unwrap(), &[8, 8]);
    assert_eq!(io.outputs.reports(0).current(), gamepad::Report::NEUTRAL);
    assert!(!io.reconnect.signaled());
}

#[test]
fn rebuild_drops_every_endpoint_and_receiver_before_allocating_the_next_pools() {
    let host = Host::default();
    let io = crate::Io::new();
    for (count, interfaces) in [
        (4, crate::Interface::Via.bit()),
        (0, 0),
        (1, crate::Interface::Vial.bit()),
        (3, 0),
        (2, crate::Interface::Via.bit()),
        (4, 0),
        (0, crate::Interface::Vial.bit()),
    ] {
        {
            let mut buffers = crate::Buffers::new(&io);
            let topology = crate::Topology {
                desktop: true,
                interfaces,
                gamepads: cordial_core::outputs::Plan {
                    hid_gamepads: count,
                    xinput_gamepads: count,
                },
            };
            let usb = crate::Usb::new(
                MockDriver::new(&host),
                "0000000000000000",
                "Test adapter",
                topology,
                &io,
                &mut buffers,
            );
            crate::BusHandler(&io).configured(true);
            io.outputs
                .publish(io.status().generation, Default::default());
            let mut running = pin!(usb.run_until_rebuild());
            assert!(poll(running.as_mut()).is_pending());
            assert_eq!(
                host.0.borrow().live,
                3 + 3 * count as usize + 2 * usize::from(interfaces != 0)
            );
            io.reconnect.signal(topology);
            assert_eq!(poll(running.as_mut()), Poll::Ready(topology));
        }
        assert_eq!(host.0.borrow().live, 0);
        // Every worker has released its Watch receiver as well as its endpoints.
        let receivers: Vec<_> = (0..crate::STATUS_RECEIVERS)
            .map(|_| io.status.receiver().unwrap())
            .collect();
        assert!(io.status.receiver().is_none());
        drop(receivers);
    }
    assert_eq!(host.0.borrow().disabled, 7);
}

fn paired_pad(
    mode: cordial_protocol::OutputMode,
) -> (
    crate::owner::Shared<crate::tests::support::Store, crate::tests::support::Radio>,
    cordial_core::link::LinkId,
) {
    use cordial_core::{
        bluetooth::Descriptor, devices::Policies, link::ServiceId, outputs::Preference,
    };
    let (mut manager, mut store, mut radio) = crate::tests::support::setup();
    let mut policy = block_on(Policies { store: &mut store }.load(77)).unwrap();
    policy.output_preferences.push(Preference {
        role: cordial_protocol::Role::Gamepad,
        mode,
    });
    block_on(Policies { store: &mut store }.save(&policy)).unwrap();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    manager.output_support = crate::outputs::SUPPORT;
    let device = manager.find(77).unwrap();
    let link = manager
        .connect(device, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    manager
        .connected(
            link,
            vec![Descriptor::from_slice(ServiceId(0), gamepad::REPORT_DESCRIPTOR).unwrap()],
            64,
            0,
        )
        .unwrap();
    let mut app = crate::tests::application();
    app.manager = manager;
    (crate::owner::Shared { app, store, radio }, link)
}

fn queue_pad(
    shared: &mut crate::owner::Shared<crate::tests::support::Store, crate::tests::support::Radio>,
    link: cordial_core::link::LinkId,
    buttons: u32,
    controls: bool,
    axis: u16,
) {
    let report = gamepad::Report::encode(buttons, &[axis; 8], 0x3f, controls.then_some(1))
        .with_controls(if controls { 5 } else { 0 }, controls, controls);
    shared
        .radio
        .events
        .push_back(cordial_core::bluetooth::Event::Input(
            cordial_core::bluetooth::InputReport::new(
                link,
                cordial_core::link::ServiceId(0),
                0,
                report.as_bytes(),
            )
            .unwrap(),
        ));
}

#[test]
fn remapped_and_disabled_hat_directions_reach_usb_without_raw_hat_passthrough() {
    use cordial_core::profiles::{Effect, GAMEPAD, Output, Rule, Rules};
    let host = Host::default();
    let io = crate::Io::new();
    let (mut shared, link) = paired_pad(cordial_protocol::OutputMode::Hid);
    let manager = &mut shared.app.manager;
    let device = manager.connection(link).unwrap().device.unwrap();
    manager.devices[device].as_mut().unwrap().layers = vec![1];
    let rules = Rules::new(vec![
        Rule {
            collection: GAMEPAD,
            input: 0x10090,
            effect: Effect::Remap(vec![Output {
                collection: GAMEPAD,
                usage: 0x10093,
            }]),
        },
        Rule {
            collection: GAMEPAD,
            input: 0x10092,
            effect: Effect::Remap(vec![]),
        },
    ])
    .unwrap();
    let update = manager
        .forward
        .prepare_maps(link.slot as usize, vec![Rc::new(RefCell::new(rules))])
        .unwrap();
    manager.forward.apply_maps(update);
    manager.reconcile_outputs();
    let mut buffers = crate::Buffers::new(&io);
    let usb = crate::Usb::new(
        MockDriver::new(&host),
        "0000000000000000",
        "Test adapter",
        crate::Topology {
            desktop: true,
            interfaces: 0,
            gamepads: shared.app.manager.outputs.plan(),
        },
        &io,
        &mut buffers,
    );
    let endpoint = host
        .0
        .borrow()
        .allocations
        .iter()
        .find(|e| e.max_packet_size == gamepad::REPORT_LEN as u16)
        .unwrap()
        .addr
        .into();
    crate::BusHandler(&io).configured(true);
    let mut priority = crate::owner::Priority::new(&io);
    block_on(priority.pass(&mut shared, 0));
    let mut running = pin!(usb.run_until_rebuild());
    assert!(poll(running.as_mut()).is_pending());
    for (source_hat, (dpad, hat)) in [
        (8, 6),
        (8, 6),
        (0, 8),
        (2, 4),
        (2, 4),
        (10, 5),
        (8, 6),
        (8, 6),
        (0, 8),
    ]
    .into_iter()
    .enumerate()
    {
        // Release each source so overlapping mapped directions get fresh press-time outputs.
        for source in [None, Some(source_hat as u8)] {
            let report =
                gamepad::Report::encode(1, &[32768; 8], 0x3f, source).with_controls(0, true, true);
            shared
                .radio
                .events
                .push_back(cordial_core::bluetooth::Event::Input(
                    cordial_core::bluetooth::InputReport::new(
                        link,
                        cordial_core::link::ServiceId(0),
                        0,
                        report.as_bytes(),
                    )
                    .unwrap(),
                ));
        }
        block_on(priority.pass(&mut shared, source_hat as u64 + 1));
        assert_eq!(
            shared.app.manager.forward.gamepad(link.slot as usize).hat,
            None
        );
        assert!(poll(running.as_mut()).is_pending());
        let mut expected = *gamepad::Report::NEUTRAL.as_bytes();
        expected[0] = 1;
        expected[16] = hat;
        expected[17] = dpad | 0x30;
        assert_eq!(
            writes(&host, endpoint).last().unwrap(),
            &expected,
            "source hat {source_hat}"
        );
        assert_eq!(
            exchange(
                &host,
                running.as_mut(),
                [0xa1, 1, 0, 1, 3, 0, gamepad::REPORT_LEN as u8, 0]
            ),
            expected
        );
    }
}

#[test]
fn priority_batches_and_busy_workers_preserve_digital_transitions_for_both_modes() {
    use cordial_protocol::OutputMode;
    for mode in [OutputMode::Hid, OutputMode::Xinput] {
        let host = Host::default();
        let io = crate::Io::new();
        let (mut shared, link) = paired_pad(mode);
        let mut buffers = crate::Buffers::new(&io);
        let usb = crate::Usb::new(
            MockDriver::new(&host),
            "0000000000000000",
            "Test adapter",
            crate::Topology {
                desktop: true,
                interfaces: 0,
                gamepads: shared.app.manager.outputs.plan(),
            },
            &io,
            &mut buffers,
        );
        let endpoint = if mode == OutputMode::Xinput {
            usb.receiver[0].as_ref().unwrap().input.info().addr.into()
        } else {
            host.0
                .borrow()
                .allocations
                .iter()
                .find(|e| e.max_packet_size == gamepad::REPORT_LEN as u16)
                .unwrap()
                .addr
                .into()
        };
        crate::BusHandler(&io).configured(true);
        let mut priority = crate::owner::Priority::new(&io);
        block_on(priority.pass(&mut shared, 0));
        let mut running = pin!(usb.run_until_rebuild());
        assert!(poll(running.as_mut()).is_pending());
        let input = |buttons, controls: bool, axis| {
            if mode == OutputMode::Hid {
                gamepad::Report::encode(buttons, &[axis; 8], 0x3f, controls.then_some(1))
                    .with_controls(if controls { 5 } else { 0 }, controls, controls)
                    .as_bytes()
                    .to_vec()
            } else {
                xinput::Input::encode(&cordial_core::hid::Gamepad {
                    buttons,
                    axes: [axis, axis, axis, axis, axis, axis, 0, 0],
                    axes_valid: 0xff,
                    axes_declared: 0xff,
                    hat: None,
                    ..Default::default()
                })
                .with_controls(if controls { 5 } else { 0 }, controls, controls)
                .as_bytes()
                .to_vec()
            }
        };
        let packets = || {
            writes(&host, endpoint)
                .into_iter()
                .filter_map(|p| {
                    if mode == OutputMode::Hid {
                        Some(p)
                    } else if p.len() == 29 && p[1] == 1 {
                        Some(p[6..18].to_vec())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        host.0.borrow_mut().writes.clear();
        // Both events reach the real Priority pass before any USB worker runs.
        queue_pad(&mut shared, link, 1, true, 12345);
        queue_pad(&mut shared, link, 0, false, 23456);
        block_on(priority.pass(&mut shared, 1));
        assert!(poll(running.as_mut()).is_pending());
        assert_eq!(packets(), [input(1, true, 23456), input(0, false, 23456)]);
        host.0.borrow_mut().writes.clear();
        host.0.borrow_mut().blocked_endpoint = Some(endpoint);
        queue_pad(&mut shared, link, 2, true, 34567);
        block_on(priority.pass(&mut shared, 2));
        assert!(poll(running.as_mut()).is_pending());
        queue_pad(&mut shared, link, 0, false, 45678);
        block_on(priority.pass(&mut shared, 3));
        queue_pad(&mut shared, link, 16, true, 56789);
        queue_pad(&mut shared, link, 0, false, 65535);
        block_on(priority.pass(&mut shared, 4));
        assert!(poll(running.as_mut()).is_pending());
        host.0.borrow_mut().blocked_endpoint = None;
        assert!(poll(running.as_mut()).is_pending());
        assert_eq!(
            packets(),
            [
                input(2, true, 34567),
                input(0, false, 65535),
                input(16, true, 65535),
                input(0, false, 65535)
            ]
        );

        // An axis-only transfer must not consume a digital edge that arrives while it is busy.
        host.0.borrow_mut().writes.clear();
        host.0.borrow_mut().blocked_endpoint = Some(endpoint);
        queue_pad(&mut shared, link, 0, false, 33333);
        block_on(priority.pass(&mut shared, 4));
        assert!(poll(running.as_mut()).is_pending());
        queue_pad(&mut shared, link, 1, true, 44444);
        queue_pad(&mut shared, link, 0, false, 55555);
        block_on(priority.pass(&mut shared, 4));
        host.0.borrow_mut().blocked_endpoint = None;
        assert!(poll(running.as_mut()).is_pending());
        assert_eq!(
            packets(),
            [
                input(0, false, 33333),
                input(1, true, 55555),
                input(0, false, 55555)
            ]
        );

        // Overflow releases held controls before restoring current digital and axis state.
        host.0.borrow_mut().writes.clear();
        host.0.borrow_mut().blocked_endpoint = Some(endpoint);
        queue_pad(&mut shared, link, 1, true, 10000);
        block_on(priority.pass(&mut shared, 5));
        assert!(poll(running.as_mut()).is_pending());
        for i in 0..40 {
            queue_pad(
                &mut shared,
                link,
                if i % 2 == 0 { 2 } else { 16 },
                true,
                20000,
            );
        }
        block_on(priority.pass(&mut shared, 6));
        host.0.borrow_mut().blocked_endpoint = None;
        assert!(poll(running.as_mut()).is_pending());
        let recovered = packets();
        assert_eq!(recovered[1], input(0, false, 20000));
        assert_eq!(recovered.last(), Some(&input(16, true, 20000)));
        queue_pad(&mut shared, link, 0, false, 20000);
        block_on(priority.pass(&mut shared, 7));
        assert!(poll(running.as_mut()).is_pending());
        assert_eq!(packets().last(), Some(&input(0, false, 20000)));
        // Reset retains axes and held controls but discards historical transitions.
        queue_pad(&mut shared, link, 2, true, 54321);
        block_on(priority.pass(&mut shared, 8));
        crate::BusHandler(&io).reset();
        crate::BusHandler(&io).configured(true);
        block_on(priority.pass(&mut shared, 9));
        assert!(poll(running.as_mut()).is_pending());
        assert_eq!(packets().last(), Some(&input(2, true, 54321)));
    }
}

#[test]
fn xinput_status_and_ack_do_not_block_out_and_keep_handshake_order() {
    let host = Host::default();
    let io = crate::Io::new();
    let mut buffers = crate::Buffers::new(&io);
    let usb = crate::Usb::new(
        MockDriver::new(&host),
        "0000000000000000",
        "Test adapter",
        crate::Topology {
            desktop: true,
            interfaces: 0,
            gamepads: cordial_core::outputs::Plan {
                hid_gamepads: 0,
                xinput_gamepads: 1,
            },
        },
        &io,
        &mut buffers,
    );
    let channel = usb.receiver[0].as_ref().unwrap();
    let endpoint = channel.input.info().addr.into();
    let output = channel.output.info().addr.into();
    crate::BusHandler(&io).configured(true);
    publish(&io, Default::default(), snapshot(1, 1, 1));
    let mut running = pin!(usb.run_until_rebuild());
    assert!(poll(running.as_mut()).is_pending());
    host.0.borrow_mut().writes.clear();
    host.0.borrow_mut().blocked_endpoint = Some(endpoint);
    publish(&io, Default::default(), snapshot(1, 1, 2));
    assert!(poll(running.as_mut()).is_pending());
    let commands = [
        [8, 0, 15, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 8, 0x42, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 1, 15, 0xc0, 0, 7, 8, 0, 0, 0, 0, 0],
    ];
    for command in commands {
        host.0
            .borrow_mut()
            .endpoint_reads
            .push_back((output, command.to_vec()));
        assert!(poll(running.as_mut()).is_pending());
        assert!(host.0.borrow().endpoint_reads.is_empty());
    }
    host.0.borrow_mut().blocked_endpoint = None;
    assert!(poll(running.as_mut()).is_pending());
    let packets = writes(&host, endpoint);
    assert_eq!(packets[1], [8, 0x80]);
    assert_eq!(
        packets[2][1], 15,
        "ACK follows the new presence, before input"
    );
    assert_eq!(packets[3][1], 1);
    host.0
        .borrow_mut()
        .endpoint_reads
        .push_back((output, commands[1].to_vec()));
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(
        writes(&host, endpoint).len(),
        4,
        "second ACK finishes the exchange"
    );
    // Pending commands from a reset cannot acknowledge the new bus generation.
    host.0.borrow_mut().blocked_endpoint = Some(endpoint);
    publish(&io, Default::default(), snapshot(1, 1, 4));
    assert!(poll(running.as_mut()).is_pending());
    for command in commands[..2].iter() {
        host.0
            .borrow_mut()
            .endpoint_reads
            .push_back((output, command.to_vec()));
    }
    assert!(poll(running.as_mut()).is_pending());
    crate::BusHandler(&io).reset();
    crate::BusHandler(&io).configured(true);
    publish(&io, Default::default(), snapshot(1, 1, 4));
    host.0.borrow_mut().writes.clear();
    host.0.borrow_mut().blocked_endpoint = None;
    assert!(poll(running.as_mut()).is_pending());
    let packets = writes(&host, endpoint);
    assert_eq!(packets[0], [8, 0x80]);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[1][1], 1);
}

fn canonical_xinput(
    buttons: u32,
    axes: &[u16; 8],
    axes_valid: u8,
    hat: Option<u8>,
) -> xinput::Input {
    xinput::Input::encode(&cordial_core::hid::Gamepad {
        buttons,
        axes: *axes,
        axes_valid,
        hat,
        ..Default::default()
    })
}

#[path = "../../../crates/cordial-core/tests/support/gamepad_descriptors.rs"]
#[allow(dead_code)]
pub(super) mod descriptors;

#[test]
fn public_controller_reports_preserve_raw_hid_usages() {
    use cordial_core::hid::Map;
    for descriptor in [
        descriptors::XBOX_SERIES,
        descriptors::XBOX_ONE_S,
        descriptors::XBOX_COMPACT,
        descriptors::DS4_BASIC,
    ] {
        let ds4 = descriptor == descriptors::DS4_BASIC;
        let compact = descriptor == descriptors::XBOX_COMPACT;
        let map = Map::compile(descriptor).unwrap();
        let mut state = map.state().unwrap();
        assert_eq!(map.gamepad(&state).axes_valid, 0);
        let neutral = if ds4 {
            vec![1, 128, 128, 128, 128, 8, 0, 0, 0, 0]
        } else {
            let mut report = vec![1, 0, 128, 0, 128, 0, 128, 0, 128, 0, 0, 0, 0, 0, 0, 0, 0];
            if compact {
                report.pop();
            }
            report
        };
        let mut right_up = neutral.clone();
        let mut triggers = neutral.clone();
        if ds4 {
            right_up[3..5].copy_from_slice(&[255, 0]);
            triggers[6] = 0x0c;
            triggers[8..10].fill(255);
        } else {
            right_up[5..9].copy_from_slice(&[255, 255, 0, 0]);
            triggers[9..13].copy_from_slice(&[255, 3, 255, 3]);
        }
        let center = if ds4 { 32896 } else { 32768 };
        let right = if compact { [3, 4] } else { [2, 5] };
        let trigger_axes = if ds4 {
            [3, 4]
        } else if compact {
            [2, 5]
        } else {
            [6, 7]
        };
        for (report, sticks, trigger_values) in [
            (&neutral, [center; 4], [0, 0]),
            (&right_up, [center, center, 65535, 0], [0, 0]),
            (&triggers, [center; 4], [65535, 65535]),
        ] {
            map.decode(&mut state, report[0], &report[1..]).unwrap();
            let pad = map.gamepad(&state);
            assert_eq!(pad.axes_valid, if ds4 || compact { 0x3f } else { 0xe7 });
            for (index, expected) in [0, 1, right[0], right[1]].into_iter().zip(sticks) {
                assert_eq!(pad.axes[index], expected);
            }
            for (index, expected) in trigger_axes.into_iter().zip(trigger_values) {
                assert_eq!(pad.axes[index], expected);
            }
            assert_eq!(pad.hat, None);
            assert_eq!(
                pad.buttons,
                if ds4 && report == &triggers { 0xc0 } else { 0 }
            );
        }
        for source in 1..=if ds4 {
            14
        } else if compact {
            10
        } else {
            15
        } {
            let mut report = neutral.clone();
            let bit = if ds4 {
                5 * 8 + 4 + source - 1
            } else {
                14 * 8 + source - 1
            };
            report[bit / 8] |= 1 << (bit % 8);
            map.decode(&mut state, report[0], &report[1..]).unwrap();
            assert_eq!(map.gamepad(&state).buttons, 1 << (source - 1));
        }
        map.decode(&mut state, neutral[0], &neutral[1..]).unwrap();
        assert_eq!(map.gamepad(&state).buttons, 0);
    }
}

#[test]
fn raw_axis_reports_preserve_declarations_when_samples_are_null() {
    use cordial_core::hid::Map;
    for usages in [
        &[0x30, 0x31, 0x32, 0x33, 0x34, 0x35][..],
        &[0x30, 0x31, 0x32, 0x35],
        &[0x30, 0x31, 0x33, 0x34, 0xc5],
        &[0x30, 0x31, 0x32, 0x35, 0xc4],
    ] {
        // Independent nullable reports make sample validity differ from declarations.
        let mut descriptor = vec![5, 1, 9, 5, 0xa1, 1];
        for (i, &usage) in usages.iter().enumerate() {
            descriptor.extend_from_slice(&[
                0x85,
                i as u8 + 1,
                5,
                if usage >= 0xc4 { 2 } else { 1 },
                9,
                usage,
                0x15,
                0,
                0x25,
                100,
                0x75,
                8,
                0x95,
                1,
                0x81,
                0x42,
            ]);
        }
        descriptor.push(0xc0);
        let map = Map::compile(&descriptor).unwrap();
        let mut state = map.state().unwrap();
        let declarations = usages.iter().fold(0, |mask, usage| {
            mask | 1
                << match usage {
                    0xc5 => 6,
                    0xc4 => 7,
                    usage => usage - 0x30,
                }
        });
        assert_eq!(map.gamepad(&state).axes_declared, declarations);
        for i in (0..usages.len()).rev() {
            map.decode(&mut state, i as u8 + 1, &[100]).unwrap();
        }
        let full = map.gamepad(&state);
        assert_eq!(full.axes_valid, declarations);
        for i in 0..8 {
            if declarations & (1 << i) != 0 {
                assert_eq!(full.axes[i], u16::MAX);
            }
        }
        for i in (0..usages.len()).rev() {
            map.decode(&mut state, i as u8 + 1, &[255]).unwrap();
            let pad = map.gamepad(&state);
            assert_eq!(pad.axes_declared, declarations);
            assert_eq!(pad.axes_valid.count_ones() as usize, i);
        }
    }
}

#[test]
fn platform_allocations_and_serial_recovery_match_actual_builder_costs() {
    use cordial_core::usb::{Demand, allocate};
    for budget in [crate::Budget::PICO, crate::Budget::ESP] {
        for desktop in [false, true] {
            for interfaces in [0, 1, 2] {
                let host = Host::default();
                let io = crate::Io::with_budget(budget);
                let topology = allocate(
                    Demand {
                        desktop: if desktop { 0x41 } else { 0xff },
                        editor: if interfaces != 0 { 0x44 } else { 0xff },
                        interfaces,
                        ..Default::default()
                    },
                    io.budget,
                )
                .topology;
                assert!(topology.fits(io.budget));
                let mut buffers = crate::Buffers::new(&io);
                let mut usb = crate::Usb::new(
                    MockDriver::new(&host),
                    "0000000000000000",
                    "Test adapter",
                    topology,
                    &io,
                    &mut buffers,
                );
                let mut running = pin!(usb.device.run());
                let bytes = exchange(&host, running.as_mut(), [0x80, 6, 0, 2, 0, 0, 0xff, 0xff]);
                let cost = topology.cost();
                assert_eq!(bytes.len(), usize::from(cost.config_bytes));
                assert_eq!(bytes[4], cost.interfaces);
                let allocations = &host.0.borrow().allocations;
                assert_eq!(
                    allocations.iter().filter(|a| a.addr.is_in()).count(),
                    usize::from(cost.in_endpoints)
                );
                assert_eq!(
                    allocations.iter().filter(|a| a.addr.is_out()).count(),
                    usize::from(cost.out_endpoints)
                );
                assert_eq!(
                    allocations
                        .iter()
                        .filter(|a| a.addr.is_out())
                        .map(|a| a.max_packet_size)
                        .sum::<u16>(),
                    cost.out_packet_bytes
                );
            }
        }
    }
    let host = Host::default();
    let io = crate::Io::with_budget(crate::Budget::ESP);
    let mut buffers = crate::Buffers::new(&io);
    let invalid = crate::Topology {
        desktop: true,
        interfaces: 3,
        gamepads: cordial_core::outputs::Plan {
            hid_gamepads: 4,
            xinput_gamepads: 4,
        },
    };
    let mut usb = crate::Usb::new(
        MockDriver::new(&host),
        "0000000000000000",
        "Test adapter",
        invalid,
        &io,
        &mut buffers,
    );
    let mut running = pin!(usb.device.run());
    let bytes = exchange(&host, running.as_mut(), [0x80, 6, 0, 2, 0, 0, 0xff, 0xff]);
    assert_eq!(io.topology(), crate::Topology::default());
    assert_eq!(bytes[4], 2);
    assert_eq!(
        bytes.len(),
        usize::from(crate::Topology::default().cost().config_bytes)
    );
}
