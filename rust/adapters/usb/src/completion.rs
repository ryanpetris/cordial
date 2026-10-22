//! Add hardware transfer completion to drivers whose write only fills a FIFO.
use embassy_usb::driver::{
    Bus, Driver, Endpoint, EndpointAddress, EndpointAllocError, EndpointError, EndpointIn,
    EndpointInfo, EndpointType, Event, Unsupported,
};

/// Platform adapter observes completion without taking ownership of the USB IRQ.
/// Return Disabled if reset/disable invalidates the submitted transfer.
pub trait Completion: Copy {
    /// Controls the physical pull-up or soft-disconnect bit.
    fn disconnect(&self, disconnected: bool);
    async fn wait(&self, endpoint: EndpointAddress) -> Result<(), EndpointError>;
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
    pending: bool,
}
impl<'d, D: Driver<'d>, C: Completion + 'd> Driver<'d> for CompleteDriver<D, C> {
    type EndpointOut = D::EndpointOut;
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
        Ok(CompleteEndpoint {
            endpoint: self
                .driver
                .alloc_endpoint_in(kind, address, size, interval)?,
            completion: self.completion,
            pending: false,
        })
    }
    fn start(self, control_max_packet_size: u16) -> (Self::Bus, Self::ControlPipe) {
        let (bus, pipe) = self.driver.start(control_max_packet_size);
        (
            CompleteBus {
                bus,
                completion: self.completion,
                restart: false,
            },
            pipe,
        )
    }
}
impl<E: Endpoint, C> Endpoint for CompleteEndpoint<E, C> {
    fn info(&self) -> &EndpointInfo {
        self.endpoint.info()
    }
    async fn wait_enabled(&mut self) {
        self.endpoint.wait_enabled().await;
    }
}
impl<E: EndpointIn, C: Completion> EndpointIn for CompleteEndpoint<E, C> {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), EndpointError> {
        // A cancelled write can still own the hardware buffer. Settle it before
        // submitting a neutral report or another controller's input.
        if self.pending {
            let _ = self.completion.wait(self.endpoint.info().addr).await;
            self.pending = false;
        }
        self.endpoint.write(bytes).await?;
        self.pending = true;
        let result = self.completion.wait(self.endpoint.info().addr).await;
        self.pending = false;
        result
    }
}

/// Embassy's platform buses keep their peripheral allocated across explicit reconnects.
pub struct CompleteBus<B, C> {
    bus: B,
    completion: C,
    restart: bool,
}
impl<B: Bus, C: Completion> Bus for CompleteBus<B, C> {
    async fn enable(&mut self) {
        self.bus.enable().await;
        self.completion.disconnect(false);
    }
    async fn disable(&mut self) {
        self.completion.disconnect(true);
        self.bus.disable().await;
        self.restart = true;
    }
    async fn poll(&mut self) -> Event {
        if core::mem::take(&mut self.restart) {
            Event::PowerDetected
        } else {
            self.bus.poll().await
        }
    }
    fn endpoint_set_enabled(&mut self, address: EndpointAddress, enabled: bool) {
        self.bus.endpoint_set_enabled(address, enabled);
    }
    fn endpoint_set_stalled(&mut self, address: EndpointAddress, stalled: bool) {
        self.bus.endpoint_set_stalled(address, stalled);
    }
    fn endpoint_is_stalled(&mut self, address: EndpointAddress) -> bool {
        self.bus.endpoint_is_stalled(address)
    }
    async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
        self.bus.remote_wakeup().await
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
    struct IdleBus;
    impl Bus for IdleBus {
        async fn enable(&mut self) {}
        async fn disable(&mut self) {}
        async fn poll(&mut self) -> Event {
            Event::Suspend
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
    struct Physical<'a>(&'a Cell<bool>);
    impl Completion for Physical<'_> {
        fn disconnect(&self, disconnected: bool) {
            self.0.set(disconnected);
        }
        async fn wait(&self, _: EndpointAddress) -> Result<(), EndpointError> {
            Ok(())
        }
    }
    #[test]
    fn reconnect_physically_detaches_and_restarts_the_usb_stack() {
        block_on(async {
            let disconnected = Cell::new(false);
            let mut bus = CompleteBus {
                bus: IdleBus,
                completion: Physical(&disconnected),
                restart: false,
            };
            bus.disable().await;
            assert!(disconnected.get());
            assert!(matches!(bus.poll().await, Event::PowerDetected));
            assert!(disconnected.get());
            bus.enable().await;
            assert!(!disconnected.get());
            assert!(matches!(bus.poll().await, Event::Suspend));
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
            pending: false,
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
