//! Platform hooks for transfer completion, endpoint halt and USB remote wake.
use core::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};
use embassy_time::Timer;
use embassy_usb::driver::{
    Bus, Driver, Endpoint, EndpointAddress, EndpointAllocError, EndpointError, EndpointIn,
    EndpointInfo, EndpointOut, EndpointType, Event, Unsupported,
};

/// Platform adapter observes completion without taking ownership of the USB IRQ.
/// Return Disabled if reset/disable invalidates the submitted transfer.
pub trait Completion: Copy {
    /// Controls the physical pull-up or soft-disconnect bit.
    fn disconnect(&self, disconnected: bool);
    async fn wait(&self, endpoint: EndpointAddress) -> Result<(), EndpointError>;
    /// Changes when reconfiguration cancels transfers, even if immediately re-enabled.
    /// The platform must also wake pending driver operations when cancelling them.
    fn epoch(&self, _: EndpointAddress) -> u32 {
        0
    }
    /// Reports a reset interrupt that the bus task has not processed yet.
    fn reset_pending(&self) -> bool {
        false
    }
    /// Keeps an OUT endpoint from accepting traffic after reset cancels a read.
    fn cancel_out_read(&self, _: EndpointAddress) {}
    /// Reinitialize selected endpoints before enabling traffic on them.
    fn endpoint_set_enabled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, enabled: bool) {
        bus.endpoint_set_enabled(address, enabled);
    }
    /// USB 2.0 section 9.4.1 resets DATA0 on clear, including an unhalted endpoint.
    /// Preserve queued transfers while changing the hardware sequence.
    fn endpoint_set_stalled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, stalled: bool) {
        bus.endpoint_set_stalled(address, stalled);
    }
    /// Clears platform state retained across endpoint operations.
    fn reset(&self) {}
    /// Finish the resume pulse before returning. Cancellation requires bus disable.
    async fn remote_wakeup<B: Bus>(&self, bus: &mut B) -> Result<(), Unsupported> {
        bus.remote_wakeup().await
    }
    /// Wait for host frames after the pulse, without taking ownership of the IRQ.
    async fn wait_resume(&self) {
        core::future::pending().await
    }
    /// Discard frame observations from before this suspension.
    fn suspended(&self) {}
    /// Prevent driver submission/rearm from overwriting a halted endpoint.
    fn stalled(&self, _: EndpointAddress) -> bool {
        false
    }
    /// Quiesces a reconfigured OUT endpoint before discarding its unread packet.
    /// A true return keeps traffic blocked until `finish_out_drain`, in the same poll.
    fn begin_out_drain(&self, _: EndpointAddress) -> bool {
        false
    }
    /// Re-primes an empty endpoint or keeps a failed drain quarantined. Always
    /// releases any global traffic gate acquired by `begin_out_drain`.
    fn finish_out_drain(
        &self,
        _: &EndpointInfo,
        result: Result<(), EndpointError>,
    ) -> Result<(), EndpointError> {
        result
    }
}

fn drain_out(endpoint: &mut impl EndpointOut, cx: &mut Context<'_>) -> Result<(), EndpointError> {
    // The largest USB packet fits here. Keep the scratch space outside the read
    // future so endpoints do not retain it while waiting for host traffic.
    let mut scratch = [0; 1024];
    if usize::from(endpoint.info().max_packet_size) > scratch.len() {
        return Err(EndpointError::BufferOverflow);
    }
    match pin!(endpoint.read(&mut scratch)).poll(cx) {
        Poll::Ready(Err(error)) => Err(error),
        Poll::Ready(Ok(_)) | Poll::Pending => Ok(()),
    }
}

/// Tracks whether halting an OUT endpoint stopped an active hardware receive.
pub struct OutHalt(AtomicBool);
impl OutHalt {
    /// Creates idle OUT halt state.
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Forgets a stopped receive after reset or endpoint reconfiguration.
    pub fn reset(&self) {
        self.0.store(false, Ordering::Relaxed);
    }
}
impl Default for OutHalt {
    fn default() -> Self {
        Self::new()
    }
}

/// Applies an OUT halt transition and re-primes only a receive stopped by HALT.
pub fn set_out_halt<B: Bus>(
    state: &OutHalt,
    bus: &mut B,
    address: EndpointAddress,
    stalled: bool,
    halt: impl FnOnce() -> bool,
    clear: impl FnOnce(),
) {
    if stalled {
        if !state.0.load(Ordering::Relaxed) && halt() {
            state.0.store(true, Ordering::Relaxed);
        }
    } else {
        clear();
        if state.0.load(Ordering::Relaxed) {
            state.reset();
            bus.endpoint_set_enabled(address, true);
        }
    }
}

async fn cancellable<C: Completion, T>(
    completion: C,
    address: EndpointAddress,
    epoch: u32,
    future: impl Future<Output = Result<T, EndpointError>>,
) -> Result<T, EndpointError> {
    let mut future = pin!(future);
    poll_fn(|cx| {
        if completion.reset_pending() || completion.epoch(address) != epoch {
            Poll::Ready(Err(EndpointError::Disabled))
        } else {
            let result = future.as_mut().poll(cx);
            if completion.reset_pending() || completion.epoch(address) != epoch {
                Poll::Ready(Err(EndpointError::Disabled))
            } else {
                result
            }
        }
    })
    .await
}

async fn wait_enabled<E: Endpoint, C: Completion>(endpoint: &mut E, completion: C) {
    loop {
        endpoint.wait_enabled().await;
        if !completion.reset_pending() {
            return;
        }
        Timer::after_micros(125).await;
    }
}

async fn transfer<C: Completion, F: Future>(
    completion: C,
    address: EndpointAddress,
    future: F,
) -> F::Output {
    let mut future = pin!(future);
    let mut retry = None;
    poll_fn(|cx| {
        if completion.stalled(address) {
            let timer = retry.get_or_insert_with(|| Timer::after_micros(125));
            if Pin::new(timer).poll(cx).is_ready() {
                retry = None;
                cx.waker().wake_by_ref();
            }
            Poll::Pending
        } else {
            retry = None;
            future.as_mut().poll(cx)
        }
    })
    .await
}

/// RP's queued buffers contain the PID to transmit/receive. For completed or
/// idle buffers Embassy flips this field when it submits/rearms the next packet.
pub fn rp_data0_pid(available: bool) -> bool {
    !available
}

/// Stop active transfers, then reset HALT and DATA0 before OUT priming.
/// `quiesce` must wait for hardware cancellation before the driver flushes IN.
pub fn reconfigure_endpoint<B: Bus>(
    bus: &mut B,
    address: EndpointAddress,
    enabled: bool,
    quiesce: impl FnOnce(),
    reset: impl FnOnce(&mut B),
) {
    quiesce();
    bus.endpoint_set_enabled(address, false);
    if enabled {
        reset(bus);
        bus.endpoint_set_enabled(address, true);
    }
}

/// Poll quickly through eight normal 1 ms host intervals, then limit wakeups for
/// an unopened endpoint. A new transfer starts with the fast interval again.
#[derive(Default)]
pub struct PollDelay(u8);
impl PollDelay {
    pub fn micros(&mut self) -> u64 {
        if self.0 < 64 {
            self.0 += 1;
            125
        } else {
            8_000
        }
    }
}

pub struct CompleteDriver<D, C> {
    pub driver: D,
    pub completion: C,
}
pub struct CompleteEndpoint<E, C> {
    endpoint: E,
    completion: C,
    pending: Option<u32>,
}
pub struct CompleteOut<E, C> {
    endpoint: E,
    completion: C,
}
impl<'d, D: Driver<'d>, C: Completion + 'd> Driver<'d> for CompleteDriver<D, C> {
    type EndpointOut = CompleteOut<D::EndpointOut, C>;
    type EndpointIn = CompleteEndpoint<D::EndpointIn, C>;
    type ControlPipe = D::ControlPipe;
    type Bus = CompleteBus<D::Bus, C>;

    fn alloc_endpoint_out(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Self::EndpointOut, EndpointAllocError> {
        Ok(CompleteOut {
            endpoint: self
                .driver
                .alloc_endpoint_out(kind, address, size, interval)?,
            completion: self.completion,
        })
    }
    fn alloc_endpoint_in(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Self::EndpointIn, EndpointAllocError> {
        Ok(CompleteEndpoint {
            endpoint: self
                .driver
                .alloc_endpoint_in(kind, address, size, interval)?,
            completion: self.completion,
            pending: None,
        })
    }
    fn start(self, control_max_packet_size: u16) -> (Self::Bus, Self::ControlPipe) {
        let (bus, pipe) = self.driver.start(control_max_packet_size);
        (
            CompleteBus {
                bus,
                completion: self.completion,
                restart: false,
                deferred: None,
            },
            pipe,
        )
    }
}
impl<E: Endpoint, C: Completion> Endpoint for CompleteEndpoint<E, C> {
    fn info(&self) -> &EndpointInfo {
        self.endpoint.info()
    }
    async fn wait_enabled(&mut self) {
        wait_enabled(&mut self.endpoint, self.completion).await;
    }
}
impl<E: EndpointIn, C: Completion> EndpointIn for CompleteEndpoint<E, C> {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), EndpointError> {
        let address = self.endpoint.info().addr;
        let completion = self.completion;
        let epoch = completion.epoch(address);
        // A cancelled write can still own the hardware buffer. Settle it before
        // submitting a neutral report or another controller's input.
        if let Some(pending) = self.pending {
            let _ = cancellable(completion, address, pending, completion.wait(address)).await;
            self.pending = None;
        }
        let result = cancellable(completion, address, epoch, async {
            transfer(completion, address, self.endpoint.write(bytes)).await?;
            self.pending = Some(epoch);
            completion.wait(address).await
        })
        .await;
        self.pending = None;
        result
    }
}
impl<E: Endpoint, C: Completion> Endpoint for CompleteOut<E, C> {
    fn info(&self) -> &EndpointInfo {
        self.endpoint.info()
    }
    async fn wait_enabled(&mut self) {
        wait_enabled(&mut self.endpoint, self.completion).await;
    }
}
impl<E: EndpointOut, C: Completion> EndpointOut for CompleteOut<E, C> {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, EndpointError> {
        let address = self.endpoint.info().addr;
        let result = cancellable(
            self.completion,
            address,
            self.completion.epoch(address),
            transfer(self.completion, address, async {
                poll_fn(|cx| {
                    let result = if self.completion.begin_out_drain(address) {
                        let result = drain_out(&mut self.endpoint, cx);
                        self.completion
                            .finish_out_drain(self.endpoint.info(), result)
                    } else {
                        Ok(())
                    };
                    Poll::Ready(result)
                })
                .await?;
                self.endpoint.read(bytes).await
            }),
        )
        .await;
        if matches!(result, Err(EndpointError::Disabled)) && self.completion.reset_pending() {
            self.completion.cancel_out_read(address);
        }
        result
    }
}

/// Embassy's platform buses keep their peripheral allocated across explicit reconnects.
pub struct CompleteBus<B, C> {
    bus: B,
    completion: C,
    restart: bool,
    deferred: Option<Event>,
}
impl<B: Bus, C: Completion> Bus for CompleteBus<B, C> {
    async fn enable(&mut self) {
        self.bus.enable().await;
        self.completion.disconnect(false);
    }
    async fn disable(&mut self) {
        self.completion.disconnect(true);
        self.completion.reset();
        self.bus.disable().await;
        self.restart = true;
        self.deferred = None;
    }
    async fn poll(&mut self) -> Event {
        let event = if core::mem::take(&mut self.restart) {
            Event::PowerDetected
        } else if let Some(event) = self.deferred.take() {
            event
        } else {
            self.bus.poll().await
        };
        if matches!(event, Event::Reset | Event::PowerRemoved) {
            self.completion.reset();
        } else if matches!(event, Event::Suspend) {
            self.completion.suspended();
        }
        event
    }
    fn endpoint_set_enabled(&mut self, address: EndpointAddress, enabled: bool) {
        self.completion
            .endpoint_set_enabled(&mut self.bus, address, enabled);
    }
    fn endpoint_set_stalled(&mut self, address: EndpointAddress, stalled: bool) {
        self.completion
            .endpoint_set_stalled(&mut self.bus, address, stalled);
    }
    fn endpoint_is_stalled(&mut self, address: EndpointAddress) -> bool {
        self.bus.endpoint_is_stalled(address)
    }
    async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
        self.completion.remote_wakeup(&mut self.bus).await?;
        loop {
            use embassy_futures::select::{Either, select};
            match select(self.bus.poll(), self.completion.wait_resume()).await {
                Either::Second(()) => return Ok(()),
                // A resume interrupt can precede the end of host signaling.
                Either::First(Event::Resume | Event::Suspend) => {}
                Either::First(event) => {
                    self.deferred = Some(event);
                    return Err(Unsupported);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use embassy_futures::block_on;
    #[test]
    fn unopened_endpoint_has_bounded_wakeups_and_new_transfers_poll_quickly() {
        let mut delay = PollDelay::default();
        let mut elapsed = 0;
        let mut polls = 0;
        while elapsed < 1_000_000 {
            let interval = delay.micros();
            if elapsed < 8_000 {
                assert_eq!(interval, 125);
            }
            assert!(interval <= 8_000);
            elapsed += interval;
            polls += 1;
        }
        assert_eq!(polls, 188);
        assert_eq!(PollDelay::default().micros(), 125);
    }
    struct IdleBus {
        reset: bool,
    }
    impl Bus for IdleBus {
        async fn enable(&mut self) {}
        async fn disable(&mut self) {}
        async fn poll(&mut self) -> Event {
            if self.reset {
                Event::Reset
            } else {
                Event::Suspend
            }
        }
        fn endpoint_set_enabled(&mut self, _: EndpointAddress, _: bool) {}
        fn endpoint_set_stalled(&mut self, _: EndpointAddress, _: bool) {}
        fn endpoint_is_stalled(&mut self, _: EndpointAddress) -> bool {
            false
        }
        async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
            Err(Unsupported)
        }
    }
    #[derive(Clone, Copy)]
    struct Physical<'a> {
        disconnected: &'a Cell<bool>,
        resets: &'a Cell<u8>,
    }
    impl Completion for Physical<'_> {
        fn disconnect(&self, disconnected: bool) {
            self.disconnected.set(disconnected);
        }
        fn reset(&self) {
            self.resets.set(self.resets.get() + 1);
        }
        async fn wait(&self, _: EndpointAddress) -> Result<(), EndpointError> {
            Ok(())
        }
    }
    #[test]
    fn reconnect_physically_detaches_and_restarts_the_usb_stack() {
        block_on(async {
            let disconnected = Cell::new(false);
            let resets = Cell::new(0);
            let mut bus = CompleteBus {
                bus: IdleBus { reset: false },
                completion: Physical {
                    disconnected: &disconnected,
                    resets: &resets,
                },
                restart: false,
                deferred: None,
            };
            bus.disable().await;
            assert!(disconnected.get());
            assert_eq!(resets.get(), 1);
            assert!(matches!(bus.poll().await, Event::PowerDetected));
            assert!(disconnected.get());
            bus.enable().await;
            assert!(!disconnected.get());
            assert!(matches!(bus.poll().await, Event::Suspend));
            assert_eq!(resets.get(), 1);
        });
    }
    #[test]
    fn bus_reset_clears_platform_endpoint_state() {
        block_on(async {
            let disconnected = Cell::new(false);
            let resets = Cell::new(0);
            let mut bus = CompleteBus {
                bus: IdleBus { reset: true },
                completion: Physical {
                    disconnected: &disconnected,
                    resets: &resets,
                },
                restart: false,
                deferred: None,
            };
            assert!(matches!(bus.poll().await, Event::Reset));
            assert_eq!(resets.get(), 1);
        });
    }
}

#[cfg(test)]
mod inflight_tests {
    use super::*;
    use core::{
        cell::{Cell, RefCell},
        future::{Future, poll_fn},
        pin::pin,
        task::{Context, Poll, Waker},
    };

    struct Fifo<'a> {
        info: EndpointInfo,
        completed: &'a Cell<bool>,
        packets: &'a RefCell<std::vec::Vec<std::vec::Vec<u8>>>,
    }
    impl Endpoint for Fifo<'_> {
        fn info(&self) -> &EndpointInfo {
            &self.info
        }
        async fn wait_enabled(&mut self) {}
    }
    impl EndpointIn for Fifo<'_> {
        async fn write(&mut self, bytes: &[u8]) -> Result<(), EndpointError> {
            self.packets.borrow_mut().push(bytes.to_vec());
            self.completed.set(false);
            Ok(())
        }
    }
    #[derive(Clone, Copy)]
    struct Witness<'a>(&'a Cell<bool>);
    impl Completion for Witness<'_> {
        fn disconnect(&self, _: bool) {}
        async fn wait(&self, _: EndpointAddress) -> Result<(), EndpointError> {
            poll_fn(|_| {
                if self.0.get() {
                    Poll::Ready(Ok(()))
                } else {
                    Poll::Pending
                }
            })
            .await
        }
    }
    #[test]
    fn cancelled_fifo_write_settles_before_the_next_occupants_neutral_report() {
        let completed = Cell::new(true);
        let packets = RefCell::new(std::vec::Vec::new());
        let mut endpoint = CompleteEndpoint {
            endpoint: Fifo {
                info: EndpointInfo {
                    addr: EndpointAddress::from_parts(1, embassy_usb::driver::Direction::In),
                    ep_type: EndpointType::Interrupt,
                    max_packet_size: 32,
                    interval_ms: 1,
                },
                completed: &completed,
                packets: &packets,
            },
            completion: Witness(&completed),
            pending: None,
        };
        let mut cx = Context::from_waker(Waker::noop());
        {
            let mut write = pin!(endpoint.write(&[1]));
            assert!(write.as_mut().poll(&mut cx).is_pending());
        }
        assert_eq!(*packets.borrow(), [std::vec![1]]);
        let mut neutral = pin!(endpoint.write(&[0]));
        assert!(neutral.as_mut().poll(&mut cx).is_pending());
        assert_eq!(*packets.borrow(), [std::vec![1]]);
        completed.set(true);
        assert!(neutral.as_mut().poll(&mut cx).is_pending());
        assert_eq!(*packets.borrow(), [std::vec![1], std::vec![0]]);
        completed.set(true);
        assert_eq!(neutral.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use core::{
        cell::{Cell, RefCell},
        future::{Future, poll_fn},
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use embassy_time::{Duration, Timer};
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Hardware {
        events: RefCell<VecDeque<Event>>,
        pulse: Cell<bool>,
        frames: Cell<bool>,
        pid: Cell<bool>,
        available: Cell<bool>,
        stalled: Cell<bool>,
        frozen: Cell<bool>,
        payload: RefCell<std::vec::Vec<u8>>,
    }
    struct MockBus<'a>(&'a Hardware);
    impl Bus for MockBus<'_> {
        async fn enable(&mut self) {}
        async fn disable(&mut self) {}
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
        fn endpoint_set_stalled(&mut self, _: EndpointAddress, stalled: bool) {
            assert!(self.0.frozen.get());
            if !stalled {
                assert_eq!(self.0.pid.get(), !self.0.available.get());
            }
            self.0.stalled.set(stalled);
        }
        fn endpoint_is_stalled(&mut self, _: EndpointAddress) -> bool {
            self.0.stalled.get()
        }
        async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
            self.0.pulse.set(true);
            Timer::after_millis(10).await;
            self.0.pulse.set(false);
            Ok(())
        }
    }
    #[derive(Clone, Copy)]
    struct Platform<'a>(&'a Hardware);
    impl Completion for Platform<'_> {
        fn stalled(&self, _: EndpointAddress) -> bool {
            self.0.stalled.get()
        }
        fn disconnect(&self, disconnected: bool) {
            if disconnected {
                self.0.pulse.set(false);
            }
        }
        async fn wait(&self, _: EndpointAddress) -> Result<(), EndpointError> {
            Ok(())
        }
        fn endpoint_set_stalled<B: Bus>(
            &self,
            bus: &mut B,
            address: EndpointAddress,
            stalled: bool,
        ) {
            self.0.frozen.set(true);
            if !stalled {
                self.0.pid.set(rp_data0_pid(self.0.available.get()));
            }
            bus.endpoint_set_stalled(address, stalled);
            self.0.frozen.set(false);
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
    fn bus(hw: &Hardware) -> CompleteBus<MockBus<'_>, Platform<'_>> {
        CompleteBus {
            bus: MockBus(hw),
            completion: Platform(hw),
            restart: false,
            deferred: None,
        }
    }
    fn advance(ms: u64) {
        crate::tests::TICKS.with(|ticks| ticks.set(Duration::from_millis(ms).as_ticks()));
    }
    #[test]
    fn clear_halt_preserves_queued_and_completed_packets_in_both_directions() {
        for direction in [
            embassy_usb::driver::Direction::In,
            embassy_usb::driver::Direction::Out,
        ] {
            for available in [true, false] {
                for stalled in [true, false] {
                    let hw = Hardware::default();
                    hw.available.set(available);
                    hw.stalled.set(stalled);
                    hw.payload.replace(std::vec![4, 5, 6]);
                    bus(&hw).endpoint_set_stalled(EndpointAddress::from_parts(1, direction), false);
                    assert!(!hw.stalled.get());
                    assert!(!hw.frozen.get());
                    assert_eq!(hw.available.get(), available);
                    assert_eq!(*hw.payload.borrow(), [4, 5, 6]);
                    // Pending hardware uses PID directly; an idle IN or unread
                    // OUT passes through Embassy's inversion on write/rearm.
                    let next_pid = if available {
                        hw.pid.get()
                    } else {
                        !hw.pid.get()
                    };
                    assert!(!next_pid);
                }
            }
        }
    }
    #[test]
    fn halt_pauses_an_already_pending_driver_operation_until_clear() {
        let hw = Hardware::default();
        let polls = Cell::new(0);
        let ready = Cell::new(false);
        let mut cx = Context::from_waker(Waker::noop());
        let operation = poll_fn(|_| {
            polls.set(polls.get() + 1);
            if ready.get() {
                Poll::Ready(Ok::<_, EndpointError>(3))
            } else {
                Poll::Pending
            }
        });
        let address = EndpointAddress::from_parts(1, embassy_usb::driver::Direction::Out);
        let mut operation = pin!(transfer(Platform(&hw), address, operation));
        assert!(operation.as_mut().poll(&mut cx).is_pending());
        assert_eq!(polls.get(), 1);
        bus(&hw).endpoint_set_stalled(address, true);
        ready.set(true);
        assert!(operation.as_mut().poll(&mut cx).is_pending());
        assert_eq!(polls.get(), 1);
        bus(&hw).endpoint_set_stalled(address, false);
        assert_eq!(operation.as_mut().poll(&mut cx), Poll::Ready(Ok(3)));
        assert_eq!(polls.get(), 2);
    }
    #[test]
    fn resume_waits_for_pulse_completion_and_host_frames() {
        advance(0);
        let hw = Hardware::default();
        let mut bus = bus(&hw);
        let mut cx = Context::from_waker(Waker::noop());
        let mut wake = pin!(bus.remote_wakeup());
        assert!(wake.as_mut().poll(&mut cx).is_pending());
        assert!(hw.pulse.get());
        hw.events.borrow_mut().push_back(Event::Resume);
        advance(9);
        assert!(wake.as_mut().poll(&mut cx).is_pending());
        assert!(hw.pulse.get());
        advance(10);
        assert!(wake.as_mut().poll(&mut cx).is_pending());
        assert!(!hw.pulse.get());
        hw.frames.set(true);
        assert!(matches!(wake.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
    }
    #[test]
    fn reset_during_pulse_is_delivered_before_resume_and_disable_ends_cancelled_pulse() {
        advance(0);
        let hw = Hardware::default();
        let mut bus = bus(&hw);
        let mut cx = Context::from_waker(Waker::noop());
        {
            let mut wake = pin!(bus.remote_wakeup());
            assert!(wake.as_mut().poll(&mut cx).is_pending());
            hw.events.borrow_mut().push_back(Event::Reset);
            hw.frames.set(true);
            advance(10);
            assert!(matches!(wake.as_mut().poll(&mut cx), Poll::Ready(Err(_))));
        }
        assert!(matches!(
            embassy_futures::block_on(bus.poll()),
            Event::Reset
        ));
        {
            let mut wake = pin!(bus.remote_wakeup());
            assert!(wake.as_mut().poll(&mut cx).is_pending());
            assert!(hw.pulse.get());
        }
        embassy_futures::block_on(bus.disable());
        assert!(!hw.pulse.get());
    }
}
