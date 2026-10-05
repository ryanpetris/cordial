//! Drain old CDC OUT bytes before acknowledging a serial session boundary.
use super::*;
use core::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
};
use embassy_usb::driver::{
    ControlPipe, EndpointAddress, EndpointAllocError, EndpointError, EndpointType,
};

pub struct SessionDriver<'a, D> {
    pub driver: D,
    pub io: &'a Io,
}
pub struct SessionControl<'a, P> {
    pipe: P,
    descriptor: crate::configuration::DescriptorFilter,
    io: &'a Io,
}
impl<'d, D: Driver<'d>> Driver<'d> for SessionDriver<'d, D> {
    type EndpointOut = D::EndpointOut;
    type EndpointIn = D::EndpointIn;
    type Bus = D::Bus;
    type ControlPipe = SessionControl<'d, D::ControlPipe>;
    fn alloc_endpoint_out(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Self::EndpointOut, EndpointAllocError> {
        self.driver
            .alloc_endpoint_out(kind, address, size, interval)
    }
    fn alloc_endpoint_in(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Self::EndpointIn, EndpointAllocError> {
        self.driver.alloc_endpoint_in(kind, address, size, interval)
    }
    fn start(self, size: u16) -> (Self::Bus, Self::ControlPipe) {
        let (bus, pipe) = self.driver.start(size);
        (
            bus,
            SessionControl {
                pipe,
                io: self.io,
                descriptor: Default::default(),
            },
        )
    }
}
impl<P: ControlPipe> ControlPipe for SessionControl<'_, P> {
    fn max_packet_size(&self) -> usize {
        self.pipe.max_packet_size()
    }
    async fn setup(&mut self) -> [u8; 8] {
        let mut request = self.pipe.setup().await;
        if request[..4] == [0x80, 6, 3, 3] {
            request[2] = configuration::SERIAL_INDEX;
        }
        // GET_DESCRIPTOR(Configuration) omits the Raw HID slots no enabled interface uses.
        self.descriptor.hide = if request[0] == 0x80 && request[1] == 6 && request[3] == 2 {
            crate::configuration::DescriptorFilter::hidden(self.io.interfaces())
        } else {
            0
        };
        request
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
        if self.descriptor.hide == 0 {
            return self.pipe.data_in(data, first, last).await;
        }
        let mut bytes = [0; 64];
        bytes[..data.len()].copy_from_slice(data);
        match self
            .descriptor
            .packet(&mut bytes[..data.len()], first, last)
        {
            Some((size, last)) => self.pipe.data_in(&bytes[..size], first, last).await,
            None => Ok(()),
        }
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
            descriptor: Default::default(),
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
