//! Raw HID configuration interfaces and the USB serial marker that follows them.
use crate::{Interface, Io, RawPacket};
use embassy_futures::select::select;
use embassy_usb::{
    class::hid,
    control::{Recipient, Request, RequestType},
    driver::{Direction, Driver, EndpointError},
};

/// Raw HID supports report transfers only; idle retransmission would duplicate editor replies.
pub(crate) fn valid_request(io: &Io, req: Request) -> bool {
    if req.request_type != RequestType::Class
        || req.recipient != Recipient::Interface
        || !io.raw.iter().any(|queue| {
            let number = queue
                .interface_number
                .load(core::sync::atomic::Ordering::Relaxed);
            number != u8::MAX && req.index == u16::from(number)
        })
    {
        return true;
    }
    io.status().configured
        && match req.request {
            1 => req.direction == Direction::In && req.value == 0x0100 && req.length != 0,
            9 => req.direction == Direction::Out && req.value == 0x0200 && req.length == 32,
            _ => false,
        }
}

/// Vial discovers adapters whose USB serial ends with this marker.
pub const VIAL_SERIAL_SUFFIX: &[u8; 14] = b"-vial:f64c2b3c";
/// Embassy reserves string index 3 for its static serial. The control pipe routes
/// that request to this custom string so reconnects update the Vial marker.
pub const SERIAL_INDEX: u8 = 4;
pub struct SerialNumber<'a> {
    pub io: &'a Io,
    pub bytes: [u8; 30],
}
impl embassy_usb::Handler for SerialNumber<'_> {
    fn get_string(&mut self, index: embassy_usb::types::StringIndex, _: u16) -> Option<&str> {
        if u8::from(index) != SERIAL_INDEX {
            return None;
        }
        let length = if self.io.interfaces() & Interface::Vial.bit() != 0 {
            30
        } else {
            16
        };
        core::str::from_utf8(&self.bytes[..length]).ok()
    }
}

/// Serves the last completed input report and accepts control output reports, including hidapi.
pub struct ReportHandler<'a> {
    pub io: &'a Io,
    pub slot: usize,
}
impl hid::RequestHandler for ReportHandler<'_> {
    fn get_report(&mut self, id: hid::ReportId, buf: &mut [u8]) -> Option<usize> {
        let generation = self.io.status().generation;
        let interface = current_request(self.io, generation, self.slot)?;
        if id != hid::ReportId::In(0) {
            return None;
        }
        let length = buf.len().min(32);
        buf[..length].fill(0);
        self.io.raw[self.slot].input.lock(|input| {
            if let Some(input) = input.borrow().as_ref()
                && input.interface == interface
            {
                buf[..length].copy_from_slice(&input.bytes[..length]);
            }
        });
        Some(length)
    }
    fn set_report(&mut self, id: hid::ReportId, data: &[u8]) -> embassy_usb::control::OutResponse {
        use embassy_usb::control::OutResponse;
        let generation = self.io.status().generation;
        let Some(interface) = current_request(self.io, generation, self.slot) else {
            return OutResponse::Rejected;
        };
        if id != hid::ReportId::Out(0) {
            return OutResponse::Rejected;
        }
        let Ok(bytes) = data.try_into() else {
            return OutResponse::Rejected;
        };
        let packet = RawPacket {
            generation,
            interface,
            bytes,
        };
        if self.io.raw[self.slot].rx.try_send(packet).is_err() {
            return OutResponse::Rejected;
        }
        self.io.changed.signal(());
        OutResponse::Accepted
    }
}

/// The interface a request read on `slot` during `generation` belongs to, if that enumeration is
/// still current and exposes the slot.
fn current_request(io: &Io, generation: u64, slot: usize) -> Option<Interface> {
    let status = io.status();
    if status.configured && status.generation == generation {
        io.slot(slot)
    } else {
        None
    }
}

/// VIA's vendor usage page FF60, usage 61, with 32-byte input and output reports. Vial uses the
/// same interface.
pub const REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0x60, 0xff, 0x09, 0x61, 0xa1, 0x01, 0x09, 0x62, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08,
    0x95, 0x20, 0x81, 0x02, 0x09, 0x63, 0x95, 0x20, 0x91, 0x02, 0xc0,
];
/// Moves `slot`'s reports between its Raw HID endpoints and the application owner.
pub async fn run<'d, D: Driver<'d>>(
    hid: hid::HidReaderWriter<'d, D, 32, 32>,
    io: &Io,
    slot: usize,
) {
    let (mut reader, mut writer) = hid.split();
    let mut status = io.status.receiver().unwrap();
    loop {
        let generation = status.get_and(|s| s.configured).await.generation;
        let receive = async {
            loop {
                let mut bytes = [0; 32];
                reader.ready().await;
                if let Ok(32) = reader.read(&mut bytes).await
                    && let Some(interface) = current_request(io, generation, slot)
                {
                    io.raw[slot]
                        .rx
                        .send(RawPacket {
                            generation,
                            interface,
                            bytes,
                        })
                        .await;
                    io.changed.signal(());
                }
            }
        };
        let transmit = async {
            loop {
                let response = io.raw[slot].tx.receive().await;
                io.changed.signal(());
                while current_request(io, response.generation, slot) == Some(response.interface) {
                    writer.ready().await;
                    if current_request(io, response.generation, slot) != Some(response.interface) {
                        break;
                    }
                    match writer.write(&response.bytes).await {
                        Ok(()) => {
                            if current_request(io, response.generation, slot)
                                == Some(response.interface)
                            {
                                io.raw[slot]
                                    .input
                                    .lock(|input| *input.borrow_mut() = Some(response));
                            }
                            break;
                        }
                        Err(EndpointError::Disabled) => {}
                        Err(_) => break,
                    }
                }
            }
        };
        // Consume buffered OUT reports under their original generation before cancelling reads.
        select(
            embassy_futures::join::join(receive, transmit),
            status.changed_and(|s| s.generation != generation),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_interrupt_from_before_retarget_is_rejected_through_reconnect() {
        let io = Io::new();
        io.set_interfaces(Interface::Via.bit());
        io.reset(true);
        let reading_generation = io.status().generation;
        assert_eq!(
            current_request(&io, reading_generation, 0),
            Some(Interface::Via)
        );
        io.reset(false);
        assert_eq!(current_request(&io, reading_generation, 0), None);
        assert_eq!(current_request(&io, io.status().generation, 0), None);
        io.set_interfaces(Interface::Vial.bit());
        io.reset(true);
        assert_eq!(current_request(&io, reading_generation, 0), None);
        assert_eq!(
            current_request(&io, io.status().generation, 0),
            Some(Interface::Vial)
        );
        io.set_interfaces(0);
        assert_eq!(current_request(&io, io.status().generation, 0), None);
    }
}
