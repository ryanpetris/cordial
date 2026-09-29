//! Add hardware transfer completion to drivers whose write only fills a FIFO.
use embassy_usb::driver::{
    Driver, Endpoint, EndpointAddress, EndpointAllocError, EndpointError, EndpointIn, EndpointInfo,
    EndpointType,
};

/// Platform adapter observes completion without taking ownership of the USB IRQ.
/// Return Disabled if reset/disable invalidates the submitted transfer.
pub trait Completion: Copy {
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
    type Bus = D::Bus;

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
        self.driver.start(control_max_packet_size)
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
