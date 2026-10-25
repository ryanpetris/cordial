//! Validate control requests and drain CDC OUT before acknowledging session boundaries.
use super::*;
use core::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
};
use embassy_usb::driver::{
    Bus, ControlPipe, Direction, Endpoint, EndpointAddress, EndpointAllocError, EndpointError,
    EndpointType, Event, Unsupported,
};

/// Allocation and enablement observed at the driver boundary, independently for each direction.
pub(crate) struct ControlState {
    allocated: u32,
    active: u32,
    isochronous: u32,
    addressed: bool,
}
impl ControlState {
    pub const fn new() -> Self {
        Self {
            allocated: 0,
            active: 0,
            isochronous: 0,
            addressed: false,
        }
    }
    fn allocate(&mut self, endpoint: &impl Endpoint) {
        let bit = endpoint_bit(endpoint.info().addr);
        self.allocated |= bit;
        if endpoint.info().ep_type == EndpointType::Isochronous {
            self.isochronous |= bit;
        }
    }
    fn reset(&mut self) {
        self.active = 0;
        self.addressed = false;
    }
}
fn endpoint_bit(address: EndpointAddress) -> u32 {
    1 << (address.index() + if address.is_in() { 16 } else { 0 })
}

pub struct SessionDriver<'a, D> {
    pub driver: D,
    pub io: &'a Io,
}
pub struct SessionBus<'a, B> {
    bus: B,
    io: &'a Io,
}
pub struct SessionControl<'a, P> {
    pipe: P,
    io: &'a Io,
}
impl<'d, D: Driver<'d>> Driver<'d> for SessionDriver<'d, D> {
    type EndpointOut = D::EndpointOut;
    type EndpointIn = D::EndpointIn;
    type Bus = SessionBus<'d, D::Bus>;
    type ControlPipe = SessionControl<'d, D::ControlPipe>;
    fn alloc_endpoint_out(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Self::EndpointOut, EndpointAllocError> {
        let endpoint = self
            .driver
            .alloc_endpoint_out(kind, address, size, interval)?;
        self.io
            .control
            .lock(|state| state.borrow_mut().allocate(&endpoint));
        Ok(endpoint)
    }
    fn alloc_endpoint_in(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Self::EndpointIn, EndpointAllocError> {
        let endpoint = self
            .driver
            .alloc_endpoint_in(kind, address, size, interval)?;
        self.io
            .control
            .lock(|state| state.borrow_mut().allocate(&endpoint));
        Ok(endpoint)
    }
    fn start(self, size: u16) -> (Self::Bus, Self::ControlPipe) {
        let (bus, pipe) = self.driver.start(size);
        (
            SessionBus { bus, io: self.io },
            SessionControl { pipe, io: self.io },
        )
    }
}
impl<B: Bus> Bus for SessionBus<'_, B> {
    async fn enable(&mut self) {
        self.io.control.lock(|state| state.borrow_mut().reset());
        self.bus.enable().await;
    }
    async fn disable(&mut self) {
        self.io.control.lock(|state| state.borrow_mut().reset());
        self.bus.disable().await;
    }
    async fn poll(&mut self) -> Event {
        let event = self.bus.poll().await;
        if matches!(event, Event::Reset | Event::PowerRemoved) {
            self.io.control.lock(|state| state.borrow_mut().reset());
        }
        event
    }
    fn endpoint_set_enabled(&mut self, address: EndpointAddress, enabled: bool) {
        self.bus.endpoint_set_enabled(address, enabled);
        self.io.control.lock(|state| {
            let mut state = state.borrow_mut();
            let bit = endpoint_bit(address);
            if enabled {
                state.active |= bit;
            } else {
                state.active &= !bit;
            }
        });
    }
    fn endpoint_set_stalled(&mut self, address: EndpointAddress, stalled: bool) {
        self.bus.endpoint_set_stalled(address, stalled);
    }
    fn endpoint_is_stalled(&mut self, address: EndpointAddress) -> bool {
        self.bus.endpoint_is_stalled(address)
    }
    fn force_reset(&mut self) -> Result<(), Unsupported> {
        self.bus.force_reset()
    }
    async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
        self.bus.remote_wakeup().await
    }
}
impl<P> SessionControl<'_, P> {
    fn valid_request(&self, req: Request) -> bool {
        if req.request_type != RequestType::Standard {
            return configuration::valid_request(self.io, req);
        }
        if req.recipient == Recipient::Device && req.request == Request::GET_DESCRIPTOR {
            // All Cordial platforms are full-speed only. USB 2.0 section 9.6.2
            // requires a request error for descriptors describing another speed.
            return !matches!(req.value >> 8, 6 | 7);
        }
        if req.recipient == Recipient::Device && req.request == Request::SET_ADDRESS {
            return !self.io.status().configured
                && req.direction == Direction::Out
                && req.value <= 127
                && req.index == 0
                && req.length == 0;
        }
        if req.recipient == Recipient::Interface && req.request == Request::SET_INTERFACE {
            return self.io.status().configured
                && req.direction == Direction::Out
                && req.index <= 255
                && req.value <= 255
                && req.length == 0;
        }
        self.io.control.lock(|state| {
            let state = state.borrow();
            if req.recipient == Recipient::Device && req.request == Request::SET_CONFIGURATION {
                return state.addressed
                    && req.direction == Direction::Out
                    && req.value <= 1
                    && req.index == 0
                    && req.length == 0;
            }
            if req.recipient != Recipient::Endpoint {
                return true;
            }
            if req.index & !0x8f != 0 || !state.addressed {
                return false;
            }
            let address = EndpointAddress::from(req.index as u8);
            let bit = endpoint_bit(address);
            if address.index() != 0
                && (!self.io.status().configured || state.allocated & state.active & bit == 0)
            {
                return false;
            }
            match req.request {
                Request::GET_STATUS => {
                    req.direction == Direction::In && req.value == 0 && req.length == 2
                }
                Request::CLEAR_FEATURE | Request::SET_FEATURE => {
                    req.direction == Direction::Out
                        && req.value == Request::FEATURE_ENDPOINT_HALT
                        && req.length == 0
                        && state.isochronous & bit == 0
                }
                _ => false,
            }
        })
    }
}
impl<P: ControlPipe> ControlPipe for SessionControl<'_, P> {
    fn max_packet_size(&self) -> usize {
        self.pipe.max_packet_size()
    }
    async fn setup(&mut self) -> [u8; 8] {
        loop {
            let mut request = self.pipe.setup().await;
            // Embassy dispatches standard endpoint requests directly to the bus,
            // before class handlers. Reject here before an invalid address reaches it.
            if !self.valid_request(Request::parse(&request)) {
                self.pipe.reject().await;
                continue;
            }
            if request[..4] == [0x80, 6, 3, 3] {
                request[2] = configuration::SERIAL_INDEX;
            }
            return request;
        }
    }
    async fn data_out(
        &mut self,
        buf: &mut [u8],
        first: bool,
        last: bool,
    ) -> Result<usize, EndpointError> {
        self.pipe.data_out(buf, first, last).await
    }
    async fn data_in(&mut self, data: &[u8], first: bool, last: bool) -> Result<(), EndpointError> {
        self.pipe.data_in(data, first, last).await
    }

    async fn accept(&mut self) {
        let session = self.io.status().session;
        // The host can send its new handshake as soon as this ACK arrives.
        // Complete the boundary drain first, never discard it after that ACK.
        self.io
            .rx_drained
            .receiver()
            .unwrap()
            .get_and(|s| *s == session)
            .await;
        self.pipe.accept().await;
    }
    async fn reject(&mut self) {
        self.pipe.reject().await;
    }
    async fn accept_set_address(&mut self, addr: u8) {
        self.pipe.accept_set_address(addr).await;
        self.io
            .control
            .lock(|state| state.borrow_mut().addressed = addr != 0);
    }
}

pub(crate) trait Receiver {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, EndpointError>;
    async fn wait_connection(&mut self);
}
impl<'d, D: Driver<'d>> Receiver for cdc_acm::Receiver<'d, D> {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, EndpointError> {
        self.read_packet(bytes).await
    }
    async fn wait_connection(&mut self) {
        self.wait_connection().await;
    }
}

pub(crate) async fn receive(mut rx: impl Receiver, io: &Io) -> ! {
    let mut status_rx = io.status.receiver().unwrap();
    let mut session = None;
    loop {
        let status = io.status();
        let mut packet = SerialRx {
            session: status.session,
            bytes: [0; 64],
            length: 0,
        };
        if session != Some(status.session) {
            // Poll until the controller has no already-buffered OUT packet.
            // A pending read is dropped; this must not wait for new host data.
            loop {
                let mut read = pin!(rx.read(&mut packet.bytes));
                let result = poll_fn(|cx| Poll::Ready(read.as_mut().poll(cx))).await;
                if !matches!(result, Poll::Ready(Ok(_))) {
                    break;
                }
            }
            session = Some(status.session);
            io.rx_drained.sender().send(status.session);
        }
        if !status.serial_open() {
            status_rx.changed_and(|s| s.session != status.session).await;
            continue;
        }
        match select(
            status_rx.changed_and(|s| s.session != status.session),
            rx.read(&mut packet.bytes),
        )
        .await
        {
            Either::Second(Ok(length)) => {
                packet.length = length;
                if length != 0 {
                    let _ = select(
                        status_rx.changed_and(|s| s.session != status.session),
                        io.serial_rx.send(packet),
                    )
                    .await;
                    io.changed.signal(());
                }
            }
            Either::Second(Err(_)) => {
                let _ = select(
                    status_rx.changed_and(|s| s.session != status.session),
                    rx.wait_connection(),
                )
                .await;
            }
            Either::First(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::{
        cell::Cell,
        task::{Context, Waker},
    };
    struct Rx<'a>(&'a Channel<CS, &'static [u8], 1>);
    impl Receiver for Rx<'_> {
        async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, EndpointError> {
            let packet = self.0.receive().await;
            bytes[..packet.len()].copy_from_slice(packet);
            Ok(packet.len())
        }
        async fn wait_connection(&mut self) {
            core::future::pending::<()>().await;
        }
    }
    struct Pipe<'a>(&'a Cell<usize>);
    impl ControlPipe for Pipe<'_> {
        fn max_packet_size(&self) -> usize {
            64
        }
        async fn setup(&mut self) -> [u8; 8] {
            core::future::pending().await
        }
        async fn data_out(
            &mut self,
            _: &mut [u8],
            _: bool,
            _: bool,
        ) -> Result<usize, EndpointError> {
            unreachable!()
        }
        async fn data_in(&mut self, _: &[u8], _: bool, _: bool) -> Result<(), EndpointError> {
            unreachable!()
        }
        async fn accept(&mut self) {
            self.0.set(self.0.get() + 1);
        }
        async fn reject(&mut self) {
            unreachable!()
        }
        async fn accept_set_address(&mut self, _: u8) {
            unreachable!()
        }
    }
    fn dtr(io: &Io, value: bool) {
        BusHandler(io).control_out(
            Request {
                direction: embassy_usb::driver::Direction::Out,
                request_type: RequestType::Class,
                recipient: Recipient::Interface,
                request: 0x22,
                value: u16::from(value),
                index: 0,
                length: 0,
            },
            &[],
        );
    }
    #[test]
    fn buffered_old_request_is_drained_before_reopen_ack() {
        let io = Io::new();
        let hardware = Channel::new();
        let acknowledgements = Cell::new(0);
        let mut pipe = SessionControl {
            pipe: Pipe(&acknowledgements),
            io: &io,
        };
        let mut task = pin!(receive(Rx(&hardware), &io));
        let mut cx = Context::from_waker(Waker::noop());
        BusHandler(&io).configured(true);
        dtr(&io, true);
        assert!(task.as_mut().poll(&mut cx).is_pending());
        embassy_futures::block_on(pipe.accept());
        let old_session = io.status().session;
        hardware.try_send(b"first\n").unwrap();
        assert!(task.as_mut().poll(&mut cx).is_pending());
        hardware.try_send(b"second\n").unwrap();
        assert!(task.as_mut().poll(&mut cx).is_pending());
        hardware.try_send(b"old request\n").unwrap();
        // Even a close/open cycle before RX next runs must invalidate old bytes.
        dtr(&io, false);
        dtr(&io, true);
        let mut ack = pin!(pipe.accept());
        assert!(ack.as_mut().poll(&mut cx).is_pending());
        assert_eq!(acknowledgements.get(), 1);
        assert!(task.as_mut().poll(&mut cx).is_pending());
        assert!(hardware.is_empty());
        assert!(ack.as_mut().poll(&mut cx).is_ready());
        assert_eq!(acknowledgements.get(), 2);
        assert_eq!(
            io.serial_rx.try_receive().ok().unwrap().session,
            old_session
        );
        assert!(io.serial_rx.is_empty());
        // A handshake sent after ACK is retained in the new session.
        hardware.try_send(b"new handshake\n").unwrap();
        assert!(task.as_mut().poll(&mut cx).is_pending());
        let packet = io.serial_rx.try_receive().ok().unwrap();
        assert_eq!(&packet.bytes[..packet.length], b"new handshake\n");
        assert_eq!(packet.session, io.status().session);
    }
}
