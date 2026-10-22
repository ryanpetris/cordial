//! Wireless XInput receiver channels in a single vendor function.
//!
//! Wire formats follow the receiver implementation at
//! <https://github.com/9names/xinput-device/blob/main/xinput-device/src/xinput.rs>
//! and Linux's `xpad360w_process_packet`. The transport is implemented locally;
//! controller and receiver identities are supplied by the caller.

use cordial_core::hid::Gamepad;

use embassy_usb::{
    Builder, Handler,
    control::{InResponse, Recipient, Request, RequestType},
    driver::{Driver, Endpoint, EndpointError, EndpointIn},
    msos::{CompatibleIdFeatureDescriptor, windows_version},
};

pub const MAX_CHANNELS: usize = 4;
/// Vendor request used by the MS OS 2.0 BOS capability when no header exists yet.
pub const MSOS_VENDOR_CODE: u8 = 0x20;

/// Receiver identity control request, separate from USB manufacturer and product IDs.
pub struct Control {
    receiver_id: [u8; 7],
}

impl Control {
    pub const fn new(receiver_id: [u8; 7]) -> Self {
        Self { receiver_id }
    }
}

impl Handler for Control {
    fn control_in<'a>(&'a mut self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if (
            req.request_type,
            req.recipient,
            req.request,
            req.value,
            req.index,
        ) != (RequestType::Vendor, Recipient::Device, 1, 1, 0)
        {
            return None;
        }
        let len = self
            .receiver_id
            .len()
            .min(req.length as usize)
            .min(buf.len());
        buf[..len].copy_from_slice(&self.receiver_id[..len]);
        Some(InResponse::Accepted(&buf[..len]))
    }
}

/// Independently owned endpoints let OUT reception continue while IN is unread.
pub struct Channel<'d, D: Driver<'d>> {
    pub input: D::EndpointIn,
    pub output: D::EndpointOut,
    pub interface: u8,
}

/// Allocate zero to four contiguous receiver channels, with no headset interfaces.
///
/// Adds one function-level XUSB20 compatible ID, retaining the builder's VID/PID
/// and composite parent. The builder must use IADs and have 64 bytes of MS OS
/// descriptor storage. Manual Windows testing confirms XInput operation in
/// MultiPadTester. Multiple simultaneous channels remain unverified on hardware.
pub fn new<'d, D: Driver<'d>>(
    builder: &mut Builder<'d, D>,
    control: &'d mut Control,
    count: usize,
) -> [Option<Channel<'d, D>>; MAX_CHANNELS] {
    assert!(count <= MAX_CHANNELS);
    let mut channels = core::array::from_fn(|_| None);
    if count == 0 {
        return channels;
    }
    if builder.msos_writer().is_empty() {
        builder.msos_descriptor(windows_version::WIN8_1, MSOS_VENDOR_CODE);
    }
    let mut function = builder.function(0xff, 0x5d, 0x81);
    function.msos_feature(CompatibleIdFeatureDescriptor::new("XUSB20", ""));
    for channel in channels.iter_mut().take(count) {
        let mut interface = function.interface();
        let number = interface.interface_number().0;
        let mut alt = interface.alt_setting(0xff, 0x5d, 0x81, None);
        let input = alt.endpoint_interrupt_in(None, 32, 1);
        let output = alt.endpoint_interrupt_out(None, 32, 8);
        // Receiver capability records name the actual allocated endpoints.
        alt.descriptor(
            0x22,
            &[
                0,
                1,
                0x13,
                input.info().addr.into(),
                0x1d,
                0,
                0x17,
                1,
                2,
                8,
                0x13,
                output.info().addr.into(),
                0x0c,
                0,
                0x0c,
                1,
                2,
                8,
            ],
        );
        *channel = Some(Channel {
            input,
            output,
            interface: number,
        });
    }
    drop(function);
    builder.handler(control);
    channels
}

/// Twelve bytes of controller state within a 29-byte receiver input packet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Input([u8; 12]);

impl Input {
    /// Encode mapped X/Y and Rx/Ry sticks with Brake/Accelerator triggers.
    /// Numbered buttons retain the USB HID Game Pad usage positions.
    pub fn encode(gamepad: &Gamepad) -> Self {
        let Gamepad {
            buttons,
            axes,
            axes_valid,
            hat,
            ..
        } = *gamepad;
        let mut data = [0; 12];
        // HID usages for A/B/X/Y/LB/RB/Back/Start/LS/RS/Guide match Linux's
        // BTN_SOUTH/EAST/WEST/NORTH/TL/TR/SELECT/START/THUMBL/THUMBR/MODE.
        let sources = [1, 2, 5, 4, 7, 8, 11, 12, 14, 15, 13];
        let mut bits = 0u16;
        for (source, target) in sources
            .into_iter()
            .zip([12, 13, 14, 15, 8, 9, 5, 4, 6, 7, 10])
        {
            if buttons & (1 << (source - 1)) != 0 {
                bits |= 1 << target;
            }
        }
        const DPAD: [u16; 8] = [1, 9, 8, 10, 2, 6, 4, 5];
        if let Some(hat) = hat.and_then(|h| DPAD.get(h as usize)) {
            bits |= hat;
        }
        data[..2].copy_from_slice(&bits.to_le_bytes());
        for (offset, i) in [(2, 6), (3, 7)] {
            if axes_valid & (1 << i) != 0 {
                data[offset] = (axes[i] / 257) as u8;
            }
        }
        for (offset, i) in [(4, 0), (6, 1), (8, 3), (10, 4)] {
            let mut axis = if axes_valid & (1 << i) != 0 {
                (i32::from(axes[i]) - 32768) as i16
            } else {
                0
            };
            if offset == 6 || offset == 10 {
                axis = axis.saturating_neg();
            }
            data[offset..offset + 2].copy_from_slice(&axis.to_le_bytes());
        }
        Self(data)
    }

    /// OR explicit Generic Desktop controls with numbered buttons and the hat.
    /// D-pad bits are Up, Down, Right and Left; opposite directions remain held.
    pub fn with_controls(mut self, dpad: u8, start: bool, select: bool) -> Self {
        self.0[0] |= (dpad & 3)
            | ((dpad & 4) << 1)
            | ((dpad & 8) >> 1)
            | (u8::from(start) << 4)
            | (u8::from(select) << 5);
        self
    }

    pub fn as_bytes(&self) -> &[u8; 12] {
        &self.0
    }
}

/// Packet just sent. After input, a worker schedules `Protocol::idle` in 11 ms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sent {
    Presence,
    Information,
    Input,
    Idle,
}

/// Bounded channel protocol state, owned by one worker.
///
/// Assignment changes clear prior input. USB reset retains current input but
/// restarts presence and the ACK exchange. The owner checks assignment and USB
/// generations and settles any in-flight endpoint transfer before slot reuse.
pub struct Protocol {
    controller_id: [u8; 6],
    connected: bool,
    presence_pending: bool,
    info_pending: bool,
    ack_phase: u8,
    input: Input,
    input_pending: bool,
    idle_pending: bool,
}

impl Protocol {
    pub const fn new(controller_id: [u8; 6]) -> Self {
        Self {
            controller_id,
            connected: false,
            presence_pending: true,
            info_pending: false,
            ack_phase: 0,
            input: Input([0; 12]),
            input_pending: false,
            idle_pending: false,
        }
    }

    /// Begin or end an assignment without changing the USB descriptors. Flush
    /// a disconnect's pending presence before assigning the slot again.
    pub fn assign(&mut self, connected: bool) {
        self.connected = connected;
        self.input = Input::default();
        self.reset();
    }

    /// Call on USB reset/reconfiguration or resume when reannouncing presence.
    pub fn reset(&mut self) {
        self.presence_pending = true;
        self.info_pending = false;
        self.ack_phase = 0;
        self.input_pending = self.connected;
        self.idle_pending = false;
    }

    pub fn update(&mut self, input: Input) {
        if self.connected {
            self.input = input;
            self.input_pending = true;
            self.idle_pending = false;
        }
    }

    /// Queue the receiver's idle marker after 11 ms without another input update.
    pub fn idle(&mut self) {
        self.idle_pending = self.connected && !self.input_pending;
    }

    /// Consume one host packet. LED and rumble commands are accepted and discarded.
    /// Returns false for malformed or unknown commands.
    pub fn receive(&mut self, packet: &[u8]) -> bool {
        if packet.len() != 12 {
            return false;
        }
        match packet {
            [8, 0, 0x0f, 0xc0, ..] => self.reset(),
            [0, 0, 0, 0x40, ..] => match self.ack_phase {
                1 => self.info_pending = true,
                2 => self.ack_phase = 0,
                _ => {}
            },
            [0, 0, 8, led, ..] if led & 0x40 != 0 => {}
            [0, 1, 0x0f, 0xc0, 0, _, _, ..] => {}
            _ => return false,
        }
        true
    }

    pub fn pending(&self) -> bool {
        self.presence_pending || self.info_pending || self.input_pending || self.idle_pending
    }

    /// Write one packet, retaining pending work on error or cancellation.
    /// Input is allowed before ACKs because Linux does not use the info exchange.
    pub async fn write<E: EndpointIn>(
        &mut self,
        endpoint: &mut E,
    ) -> Result<Option<Sent>, EndpointError> {
        if self.presence_pending {
            endpoint
                .write(&[8, if self.connected { 0x80 } else { 8 }])
                .await?;
            self.presence_pending = false;
            self.ack_phase = u8::from(self.connected);
            return Ok(Some(Sent::Presence));
        }
        let mut packet = [0; 29];
        packet[3] = 0xf0;
        if self.info_pending {
            packet[1] = 0x0f;
            packet[4] = 0xf0;
            packet[5] = 0xcc;
            packet[6..10].fill(0xff);
            packet[10..16].copy_from_slice(&self.controller_id);
            packet[16..].copy_from_slice(&[
                0x13, 0xa3, 0x20, 0x1d, 0x30, 3, 0x40, 1, 0x50, 1, 0xff, 0xff, 0xff,
            ]);
            endpoint.write(&packet).await?;
            self.info_pending = false;
            self.ack_phase = 2;
            Ok(Some(Sent::Information))
        } else if self.input_pending {
            packet[1] = 1;
            packet[5] = 0x13;
            packet[6..18].copy_from_slice(self.input.as_bytes());
            endpoint.write(&packet).await?;
            self.input_pending = false;
            self.idle_pending = false;
            Ok(Some(Sent::Input))
        } else if self.idle_pending {
            endpoint.write(&packet).await?;
            self.idle_pending = false;
            Ok(Some(Sent::Idle))
        } else {
            Ok(None)
        }
    }
}
