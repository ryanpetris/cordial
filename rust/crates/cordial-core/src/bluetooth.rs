//! Operations shared by the selected Bluetooth adapters. Vendor callbacks copy
//! into their event queue and wake the application; they do not reenter it.
use crate::{
    devices::Peer,
    hid,
    link::{LinkId, ServiceId, WriteId},
};
use alloc::{boxed::Box, vec::Vec};
pub use cordial_protocol::messages::{ConnectionSecurity, DeviceKind};
use cordial_protocol::{
    errors::ErrorCode as Error, identifiers::Transport, messages::PromptMethod,
};

#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub classic: bool,
    pub ble: bool,
    /// The backend and controller can actively scan while initiating a BLE link.
    pub ble_scan_and_connect: bool,
}
impl Capabilities {
    pub fn supports(self, transport: Transport) -> bool {
        match transport {
            Transport::Classic => self.classic,
            Transport::Ble => self.ble,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportType {
    Input,
    Output,
    Feature,
}
pub struct Descriptor {
    pub service: ServiceId,
    pub bytes: Box<[u8]>,
}
impl Descriptor {
    pub fn from_slice(service: ServiceId, bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > hid::DESCRIPTOR_BYTES {
            return Err(Error::UnsupportedHid);
        }
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(bytes.len())
            .map_err(|_| Error::Capacity)?;
        owned.extend_from_slice(bytes);
        Ok(Self {
            service,
            bytes: owned.into_boxed_slice(),
        })
    }
}
/// Inline data keeps ordinary input free of allocator calls.
pub struct InputReport {
    pub link: LinkId,
    pub service: ServiceId,
    pub report_id: u8,
    length: u16,
    bytes: [u8; hid::REPORT_BYTES],
}
impl InputReport {
    pub fn new(
        link: LinkId,
        service: ServiceId,
        report_id: u8,
        payload: &[u8],
    ) -> Result<Self, Error> {
        if payload.len() > hid::REPORT_BYTES {
            return Err(Error::InputOverflow);
        }
        let mut report = Self {
            link,
            service,
            report_id,
            length: payload.len() as u16,
            bytes: [0; hid::REPORT_BYTES],
        };
        report.bytes[..payload.len()].copy_from_slice(payload);
        Ok(report)
    }
    pub fn payload(&self) -> &[u8] {
        &self.bytes[..self.length as usize]
    }
}
#[allow(clippy::large_enum_variant)] // The bounded callback queue owns input bytes, with no per-report heap allocation.
pub enum Event {
    Ready,
    Failed(Error),
    /// Release all logical links while the backend restarts through its public
    /// lifecycle API. A later Ready resumes ordinary saved-device reconnects.
    Restarting(Error),
    Found {
        scan: u64,
        /// Fresh-pair address when known; identity alone still signals presence.
        address: Option<Peer>,
        peer: Peer,
        connectable: bool,
        kind: DeviceKind,
        name: Box<str>,
        rssi: Option<i16>,
    },
    Incoming {
        attempt: u32,
        peer: Peer,
    },
    Prompt {
        link: LinkId,
        method: PromptMethod,
        value: Option<Box<str>>,
    },
    /// The backend has stored the new bond using its supported native facility.
    /// It waits for adopt() before admitting HID data for an unsaved pairing.
    Bonded {
        link: LinkId,
        identity: Peer,
    },
    /// Security and all report-map setup have completed. Descriptors belong to
    /// their service, and all reports below exclude any prefixed report ID.
    Connected {
        link: LinkId,
        descriptors: Vec<Descriptor>,
        max_output: usize,
    },
    Security {
        link: LinkId,
        security: ConnectionSecurity,
    },
    Disconnected {
        link: LinkId,
        error: Option<Error>,
    },
    Input(InputReport),
    /// Optional service observation, scoped to a live connection generation.
    Information {
        link: LinkId,
        uuid: u16,
        instance: u8,
        success: bool,
        bytes: Box<[u8]>,
    },
    Written {
        id: WriteId,
        result: Result<(), Error>,
    },
    Read {
        id: WriteId,
        report_type: ReportType,
        result: Result<InputReport, Error>,
    },
}

/// Calls occur in the adapter's supported context. An adapter may marshal them
/// to its vendor task. An accepted call owns all borrowed bytes before returning.
/// Its later completion must carry the supplied generation/operation token.
#[allow(async_fn_in_trait)]
pub trait Bluetooth {
    fn refresh_info(&mut self, _link: LinkId) -> Result<(), Error> {
        Ok(())
    }
    fn info_busy(&self, _link: LinkId) -> bool {
        false
    }

    fn gatt_writes(
        &self,
    ) -> Option<alloc::vec::Vec<cordial_protocol::messages::GattWriteDiagnostic>> {
        None
    }
    fn authentication_failure(&self) -> Option<cordial_protocol::messages::AuthenticationFailure> {
        None
    }

    fn capabilities(&self) -> Capabilities;
    /// Native database capacity, including the temporary pairing entry.
    fn bond_capacity(&self, _transport: Transport) -> usize {
        0
    }
    /// Replace the BLE peers eligible for controller accept-list reconnection.
    /// Connections are offered through Incoming and must still be admitted.
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error>;
    fn scan(&mut self, id: u64, classic: bool, ble: bool) -> Result<(), Error>;
    fn connect(&mut self, link: LinkId, peer: Peer, pairing: bool) -> Result<(), Error>;
    fn incoming(&mut self, attempt: u32, accept: Option<LinkId>) -> Result<(), Error>;
    /// Reject further authentication for this generation. Before Disconnected,
    /// finish native cleanup for a pairing that has not been adopted. Do not
    /// publish later callbacks under a reused slot or remove another live bond.
    fn disconnect(&mut self, link: LinkId);
    fn adopt(&mut self, link: LinkId) -> Result<(), Error>;
    fn pair_reply(
        &mut self,
        link: LinkId,
        method: PromptMethod,
        accept: bool,
        value: Option<&str>,
    ) -> Result<(), Error>;
    /// Reserve transport capacity before asking Link for the next write. Only
    /// this owner submits requests, so capacity cannot be consumed by a callback.
    fn can_write(&self, link: LinkId) -> bool;
    fn write(
        &mut self,
        id: WriteId,
        service: ServiceId,
        kind: ReportType,
        report_id: Option<u8>,
        payload: &[u8],
    ) -> Result<(), Error>;
    fn read(
        &mut self,
        id: WriteId,
        service: ServiceId,
        kind: ReportType,
        report_id: Option<u8>,
    ) -> Result<(), Error>;
    /// Import/export the portable active database. These operations never change
    /// the committed application store.
    async fn import_bond(&mut self, _bond: &crate::bonds::Bond) -> Result<(), Error> {
        Err(Error::UnsupportedTransport)
    }
    async fn export_bond(&mut self, _peer: Peer) -> Result<crate::bonds::Bond, Error> {
        Err(Error::AuthenticationFailed)
    }
    async fn bonds(&mut self) -> Result<Vec<Peer>, Error>;
    async fn forget(&mut self, peer: Peer) -> Result<(), Error>;
}

/// Application-side event pump. Concrete adapters decide whether vendor work
/// runs in this owner or on a native task; the application loop is unchanged.
#[allow(async_fn_in_trait)]
pub trait EventSource: Bluetooth {
    async fn poll(&mut self);
    fn next_event(&mut self) -> Option<Event>;
    async fn changed(&self);
}

/// Bluetooth SIG Appearance values and Peripheral Class of Device bits.
/// These are discovery hints, not proof of HID support or pairing capability.
pub fn discovery_kind(transport: Transport, value: u32) -> DeviceKind {
    match transport {
        Transport::Ble => match value {
            0x03c1 => DeviceKind::Keyboard,
            0x03c2 => DeviceKind::Mouse,
            _ => DeviceKind::Unknown,
        },
        Transport::Classic if value & 0x1f00 == 0x0500 && value & 3 == 0 => match value & 0xc0 {
            0x40 => DeviceKind::Keyboard,
            0x80 => DeviceKind::Mouse,
            0xc0 => DeviceKind::KeyboardMouse,
            _ => DeviceKind::Unknown,
        },
        _ => DeviceKind::Unknown,
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn advertised_input_kinds_require_the_correct_category() {
        for (transport, value, expected) in [
            (Transport::Ble, 0x03c1, DeviceKind::Keyboard),
            (Transport::Ble, 0x03c2, DeviceKind::Mouse),
            (Transport::Ble, 0x03c0, DeviceKind::Unknown),
            (Transport::Ble, 0x03c4, DeviceKind::Unknown),
            (Transport::Ble, 0, DeviceKind::Unknown),
            (Transport::Classic, 0x0540, DeviceKind::Keyboard),
            (Transport::Classic, 0x2580, DeviceKind::Mouse),
            (Transport::Classic, 0x05c0, DeviceKind::KeyboardMouse),
            (Transport::Classic, 0x05c4, DeviceKind::KeyboardMouse),
            (Transport::Classic, 0x0140, DeviceKind::Unknown),
            (Transport::Classic, 0x0541, DeviceKind::Unknown),
            (Transport::Classic, 0, DeviceKind::Unknown),
        ] {
            assert_eq!(discovery_kind(transport, value), expected);
        }
    }
}
