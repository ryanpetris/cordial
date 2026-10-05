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

pub struct CompleteDriver<D, C> {
    pub driver: D,
    pub completion: C,
}
pub struct CompleteEndpoint<E, C> {
    endpoint: E,
    completion: C,
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
        self.endpoint.write(bytes).await?;
        self.completion.wait(self.endpoint.info().addr).await
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
