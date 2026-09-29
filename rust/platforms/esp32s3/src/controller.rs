//! Public ESP-IDF VHCI boundary. Controller callbacks own their bytes before
//! returning, and all BTstack processing stays on the application owner task.
use cordial_btstack::transport::{Io, PACKET_BYTES, Packet};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex as Raw},
    signal::Signal,
    zerocopy_channel::{Channel, Receiver, Sender},
};
use esp_idf_sys::{self as sys, platform};
use std::{cell::RefCell, sync::OnceLock};

static IO: OnceLock<&'static Io> = OnceLock::new();
// Ordinary HID input never allocates. Capacity failure is explicit; it cannot
// silently discard controller lifecycle events.
static mut PACKETS: [Packet; 8] = [const { Packet::empty() }; 8];
static INBOUND: Mutex<Raw, RefCell<Option<Sender<'static, Raw, Packet>>>> =
    Mutex::new(RefCell::new(None));
static RECEIVE: Signal<Raw, Receiver<'static, Raw, Packet>> = Signal::new();

unsafe extern "C" fn sent() {
    IO.get().unwrap().finish_outbound(true);
}
unsafe extern "C" fn received(data: *mut u8, len: u16) -> i32 {
    let io = *IO.get().unwrap();
    if data.is_null() || len < 3 || usize::from(len) > PACKET_BYTES + 1 {
        io.fail();
        return -1;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, usize::from(len)) };
    let valid = match bytes[0] {
        4 => usize::from(bytes[2]) + 3 == bytes.len(),
        2 if bytes.len() >= 5 => {
            usize::from(u16::from_le_bytes([bytes[3], bytes[4]])) + 5 == bytes.len()
        }
        _ => false,
    };
    if !valid {
        io.fail();
        return -1;
    }
    let advertising =
        bytes[0] == 4 && bytes.get(1) == Some(&0x3e) && matches!(bytes.get(3), Some(0x02 | 0x0d));
    let queued = INBOUND.lock(|sender| {
        let mut sender = sender.borrow_mut();
        let sender = sender.as_mut().unwrap();
        let Some(packet) = sender.try_send() else {
            return false;
        };
        packet.kind = bytes[0];
        packet.len = (bytes.len() - 1) as u16;
        packet.data_mut().copy_from_slice(&bytes[1..]);
        sender.send_done();
        true
    });
    if !queued {
        // Discovery updates are optional; do not sacrifice active input links
        // for an advertising burst. Mandatory packet loss remains terminal.
        if advertising {
            return 0;
        }
        io.fail();
        return -1;
    }
    0
}

pub fn initialize(io: &'static Io) -> Result<(), sys::esp_err_t> {
    IO.set(io).map_err(|_| sys::ESP_ERR_INVALID_STATE)?;
    // IO's one-time initialization guards the sole mutable borrow of these
    // static slots. The callback fills a slot in place on its small native stack.
    let packets = unsafe { &mut *std::ptr::addr_of_mut!(PACKETS) };
    let channel = Box::leak(Box::new(Channel::<Raw, Packet>::new(packets)));
    let (sender, receiver) = channel.split();
    INBOUND.lock(|slot| *slot.borrow_mut() = Some(sender));
    RECEIVE.signal(receiver);
    let error = unsafe { platform::cordial_esp_controller_start(Some(sent), Some(received)) };
    if error == sys::ESP_OK {
        Ok(())
    } else {
        Err(error)
    }
}

pub async fn run(io: &'static Io) -> ! {
    let incoming = async {
        let mut receiver = RECEIVE.wait().await;
        loop {
            let value = receiver.receive().await;
            let mut packet = Packet::empty();
            packet.kind = value.kind;
            packet.len = value.len;
            packet.data_mut().copy_from_slice(value.data());
            io.received(packet).await;
            receiver.receive_done();
        }
    };
    let outgoing = async {
        loop {
            let packet = io.next_outbound().await;
            let mut bytes = [0; PACKET_BYTES + 1];
            bytes[0] = packet.kind;
            let length = usize::from(packet.len) + 1;
            bytes[1..length].copy_from_slice(packet.data());
            // Native submission copies the bytes. The worker reports buffer
            // release after the public controller call returns.
            if unsafe { platform::cordial_esp_controller_send(bytes.as_ptr(), length as u16) }
                != sys::ESP_OK
            {
                io.finish_outbound(false);
            }
        }
    };
    embassy_futures::join::join(incoming, outgoing).await;
    unreachable!()
}
