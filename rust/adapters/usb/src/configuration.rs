//! Raw HID configuration interfaces and the USB serial marker that follows them.
use crate::{Interface, Io, RawPacket, SLOTS};
use embassy_futures::select::{Either, select};
use embassy_usb::{class::hid, driver::Driver};

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

/// Accepts output reports sent through the control endpoint, including by hidapi.
pub struct ReportHandler<'a> {
    pub io: &'a Io,
    pub slot: usize,
}
impl hid::RequestHandler for ReportHandler<'_> {
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
/// IAD, interface, HID descriptor and two endpoint descriptors.
pub const INTERFACE_BYTES: usize = 8 + 9 + 9 + 7 + 7;

/// Trims unused trailing Raw HID slots from configuration descriptor transfers. Endpoints remain
/// allocated to avoid rebuilding the peripheral driver at run time.
#[derive(Default)]
pub struct DescriptorFilter {
    /// The number of trailing Raw HID interfaces to omit from the current transfer.
    pub hide: usize,
    remaining: usize,
    ended: bool,
}
impl DescriptorFilter {
    /// The number of slots to omit while `set` is enabled.
    pub fn hidden(set: u8) -> usize {
        SLOTS - crate::used_slots(set)
    }
    pub fn packet(&mut self, bytes: &mut [u8], first: bool, last: bool) -> Option<(usize, bool)> {
        if self.hide == 0 {
            return Some((bytes.len(), last));
        }
        if first {
            if bytes.len() < 9 {
                return Some((bytes.len(), last));
            }
            self.remaining =
                usize::from(u16::from_le_bytes([bytes[2], bytes[3]])) - self.hide * INTERFACE_BYTES;
            bytes[2..4].copy_from_slice(&(self.remaining as u16).to_le_bytes());
            bytes[4] -= self.hide as u8;
            self.ended = false;
        }
        if self.ended {
            return None;
        }
        let size = bytes.len().min(self.remaining);
        self.remaining -= size;
        // A full packet at the shortened boundary needs a following zero-length packet
        // when the Host requested more. The omitted interface supplies that next call.
        let last = last || (self.remaining == 0 && size < 64);
        self.ended = last;
        Some((size, last))
    }
}
/// Moves `slot`'s reports between its Raw HID endpoints and the application owner.
pub async fn run<'d, D: Driver<'d>>(
    hid: hid::HidReaderWriter<'d, D, 32, 32>,
    io: &Io,
    slot: usize,
) {
    let (mut reader, mut writer) = hid.split();
    let mut read_status = io.status.receiver().unwrap();
    let receive = async {
        loop {
            let mut bytes = [0; 32];
            let generation = read_status.get_and(|s| s.configured).await.generation;
            if let Either::First(Ok(32)) = select(
                reader.read(&mut bytes),
                read_status.changed_and(|s| s.generation != generation),
            )
            .await
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
            writer.ready().await;
            if current_request(io, response.generation, slot) == Some(response.interface) {
                let _ = writer.write(&response.bytes).await;
            }
        }
    };
    embassy_futures::join::join(receive, transmit).await;
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
