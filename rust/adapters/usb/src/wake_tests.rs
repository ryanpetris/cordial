use crate::{
    Buffers, Io, Topology, Usb,
    completion::{CompleteDriver, Completion},
};
use core::{
    cell::{Cell, RefCell},
    future::{Future, pending, poll_fn},
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
};
use embassy_time::{Duration, Timer};
use embassy_usb::driver::*;
use std::collections::VecDeque;

#[derive(Default)]
struct Host {
    events: RefCell<VecDeque<Event>>,
    setup: RefCell<VecDeque<[u8; 8]>>,
    response: RefCell<std::vec::Vec<u8>>,
    pulses: Cell<usize>,
    signaling: Cell<bool>,
    frames: Cell<bool>,
    disabled: Cell<usize>,
}
struct Driver<'a>(&'a Host, [usize; 2]);
struct Bus<'a>(&'a Host);
struct Pipe<'a>(&'a Host);
struct Ep(EndpointInfo);
impl Endpoint for Ep {
    fn info(&self) -> &EndpointInfo {
        &self.0
    }
    async fn wait_enabled(&mut self) {
        pending().await
    }
}
impl EndpointIn for Ep {
    async fn write(&mut self, _: &[u8]) -> Result<(), EndpointError> {
        pending().await
    }
}
impl EndpointOut for Ep {
    async fn read(&mut self, _: &mut [u8]) -> Result<usize, EndpointError> {
        pending().await
    }
}
impl Driver<'_> {
    fn alloc(
        &mut self,
        direction: Direction,
        ep_type: EndpointType,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<Ep, EndpointAllocError> {
        let next = &mut self.1[usize::from(direction == Direction::In)];
        *next += 1;
        Ok(Ep(EndpointInfo {
            addr: EndpointAddress::from_parts(*next, direction),
            ep_type,
            max_packet_size,
            interval_ms,
        }))
    }
}
impl<'d> embassy_usb::driver::Driver<'d> for Driver<'d> {
    type EndpointOut = Ep;
    type EndpointIn = Ep;
    type Bus = Bus<'d>;
    type ControlPipe = Pipe<'d>;
    fn alloc_endpoint_out(
        &mut self,
        kind: EndpointType,
        _: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Ep, EndpointAllocError> {
        self.alloc(Direction::Out, kind, size, interval)
    }
    fn alloc_endpoint_in(
        &mut self,
        kind: EndpointType,
        _: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Ep, EndpointAllocError> {
        self.alloc(Direction::In, kind, size, interval)
    }
    fn start(self, _: u16) -> (Bus<'d>, Pipe<'d>) {
        (Bus(self.0), Pipe(self.0))
    }
}
impl embassy_usb::driver::Bus for Bus<'_> {
    async fn enable(&mut self) {}
    async fn disable(&mut self) {
        self.0.disabled.set(self.0.disabled.get() + 1);
    }
    async fn poll(&mut self) -> Event {
        poll_fn(|_| {
            self.0
                .events
                .borrow_mut()
                .pop_front()
                .map_or(Poll::Pending, Poll::Ready)
        })
        .await
    }
    fn endpoint_set_enabled(&mut self, _: EndpointAddress, _: bool) {}
    fn endpoint_set_stalled(&mut self, _: EndpointAddress, _: bool) {}
    fn endpoint_is_stalled(&mut self, _: EndpointAddress) -> bool {
        false
    }
    async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
        self.0.pulses.set(self.0.pulses.get() + 1);
        self.0.signaling.set(true);
        Timer::after_millis(10).await;
        self.0.signaling.set(false);
        Ok(())
    }
}
impl ControlPipe for Pipe<'_> {
    fn max_packet_size(&self) -> usize {
        64
    }
    async fn setup(&mut self) -> [u8; 8] {
        poll_fn(|_| {
            self.0
                .setup
                .borrow_mut()
                .pop_front()
                .map_or(Poll::Pending, Poll::Ready)
        })
        .await
    }
    async fn data_out(&mut self, _: &mut [u8], _: bool, _: bool) -> Result<usize, EndpointError> {
        Ok(0)
    }
    async fn data_in(&mut self, bytes: &[u8], _: bool, _: bool) -> Result<(), EndpointError> {
        self.0.response.borrow_mut().extend_from_slice(bytes);
        Ok(())
    }
    async fn accept(&mut self) {}
    async fn reject(&mut self) {
        panic!("valid control request rejected");
    }
    async fn accept_set_address(&mut self, _: u8) {}
}
#[derive(Clone, Copy)]
struct Platform<'a>(&'a Host);
impl Completion for Platform<'_> {
    fn disconnect(&self, disconnected: bool) {
        if disconnected {
            self.0.signaling.set(false);
        }
    }
    async fn wait(&self, _: EndpointAddress) -> Result<(), EndpointError> {
        Ok(())
    }
    fn suspended(&self) {
        self.0.frames.set(false);
    }
    async fn wait_resume(&self) {
        poll_fn(|_| {
            if self.0.frames.get() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}
fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}
fn time(ms: u64) {
    crate::tests::TICKS.with(|ticks| ticks.set(Duration::from_millis(ms).as_ticks()));
}
fn configure(host: &Host) {
    host.setup
        .borrow_mut()
        .extend([[0, 5, 1, 0, 0, 0, 0, 0], [0, 9, 1, 0, 0, 0, 0, 0]]);
}

#[test]
fn host_controls_wake_and_ready_waits_for_frames_then_rebuild_disables_an_active_pulse() {
    time(0);
    let host = Host::default();
    let io = Io::new();
    let mut buffers = Buffers::new(&io);
    let usb = Usb::new(
        CompleteDriver {
            driver: Driver(&host, [0; 2]),
            completion: Platform(&host),
        },
        "0000000000000000",
        "Test adapter",
        Topology::default(),
        &io,
        &mut buffers,
    );
    let mut running = pin!(usb.run_until_rebuild());
    host.events
        .borrow_mut()
        .extend([Event::PowerDetected, Event::Reset]);
    configure(&host);
    // Configuration descriptor bmAttributes advertises standard remote wake.
    host.setup
        .borrow_mut()
        .push_back([0x80, 6, 0, 2, 0, 0, 9, 0]);
    assert!(poll(running.as_mut()).is_pending());
    // SET_CONFIGURATION waits for the CDC worker to drain the new session.
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(host.response.borrow()[7] & 0x20, 0x20);
    assert!(io.status().configured);
    host.events.borrow_mut().push_back(Event::Suspend);
    assert!(poll(running.as_mut()).is_pending());
    io.wake.request(io.status());
    time(10);
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(host.pulses.get(), 0);
    host.events.borrow_mut().push_back(Event::Resume);
    host.setup.borrow_mut().push_back([0, 3, 1, 0, 0, 0, 0, 0]);
    assert!(poll(running.as_mut()).is_pending());
    host.events.borrow_mut().push_back(Event::Suspend);
    assert!(poll(running.as_mut()).is_pending());
    io.wake.request(io.status());
    time(14);
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(host.pulses.get(), 0);
    time(15);
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(host.pulses.get(), 1);
    assert!(host.signaling.get());
    for _ in 0..10 {
        io.wake.request(io.status());
    }
    time(25);
    assert!(poll(running.as_mut()).is_pending());
    assert!(!host.signaling.get());
    assert!(!io.status().input_ready());
    host.events.borrow_mut().push_back(Event::Resume);
    assert!(poll(running.as_mut()).is_pending());
    assert!(!io.status().input_ready());
    host.frames.set(true);
    assert!(poll(running.as_mut()).is_pending());
    assert!(io.status().input_ready());
    assert_eq!(host.pulses.get(), 1);

    // CLEAR_FEATURE revokes permission without requiring disconnect.
    host.setup.borrow_mut().push_back([0, 1, 1, 0, 0, 0, 0, 0]);
    assert!(poll(running.as_mut()).is_pending());
    host.events.borrow_mut().push_back(Event::Suspend);
    assert!(poll(running.as_mut()).is_pending());
    io.wake.request(io.status());
    time(35);
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(host.pulses.get(), 1);

    host.events.borrow_mut().push_back(Event::Resume);
    host.setup.borrow_mut().push_back([0, 3, 1, 0, 0, 0, 0, 0]);
    assert!(poll(running.as_mut()).is_pending());
    host.events.borrow_mut().push_back(Event::Suspend);
    assert!(poll(running.as_mut()).is_pending());
    io.wake.request(io.status());
    assert!(poll(running.as_mut()).is_pending());
    time(40);
    assert!(poll(running.as_mut()).is_pending());
    assert!(host.signaling.get());
    io.reconnect.signal(Topology::default());
    assert_eq!(poll(running.as_mut()), Poll::Ready(Topology::default()));
    assert!(!host.signaling.get());
    assert_eq!(host.disabled.get(), 1);
    assert!(!io.status().configured);
}

#[test]
fn reset_during_remote_wake_clears_permission_before_the_next_enumeration() {
    time(0);
    let host = Host::default();
    let io = Io::new();
    let mut buffers = Buffers::new(&io);
    let usb = Usb::new(
        CompleteDriver {
            driver: Driver(&host, [0; 2]),
            completion: Platform(&host),
        },
        "0000000000000000",
        "Test adapter",
        Topology::default(),
        &io,
        &mut buffers,
    );
    let mut running = pin!(usb.run_until_rebuild());
    host.events.borrow_mut().push_back(Event::PowerDetected);
    configure(&host);
    host.setup.borrow_mut().push_back([0, 3, 1, 0, 0, 0, 0, 0]);
    assert!(poll(running.as_mut()).is_pending());
    assert!(poll(running.as_mut()).is_pending());
    host.events.borrow_mut().push_back(Event::Suspend);
    assert!(poll(running.as_mut()).is_pending());
    io.wake.request(io.status());
    assert!(poll(running.as_mut()).is_pending());
    time(5);
    assert!(poll(running.as_mut()).is_pending());
    assert!(host.signaling.get());
    host.events.borrow_mut().push_back(Event::Reset);
    io.wake.request(io.status());
    time(15);
    assert!(poll(running.as_mut()).is_pending());
    assert!(!host.signaling.get());
    assert!(!io.status().configured);
    assert!(!io.status().suspended);
    configure(&host);
    assert!(poll(running.as_mut()).is_pending());
    assert!(poll(running.as_mut()).is_pending());
    host.events.borrow_mut().push_back(Event::Suspend);
    assert!(poll(running.as_mut()).is_pending());
    io.wake.request(io.status());
    assert!(poll(running.as_mut()).is_pending());
    time(25);
    assert!(poll(running.as_mut()).is_pending());
    assert_eq!(host.pulses.get(), 1);
    io.reconnect.signal(Topology::default());
    assert!(poll(running.as_mut()).is_ready());
}
