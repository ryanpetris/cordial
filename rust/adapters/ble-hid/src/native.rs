//! Native hosts own GAP, security, ATT transactions and bond persistence. This
//! boundary carries owned results to the common HID-over-GATT profile owner.
use alloc::{boxed::Box, vec::Vec};
use cordial_core::devices::Peer;
use cordial_core::model::{errors::ErrorCode as Error, link::PromptMethod};

pub struct Data {
    pub length: u16,
    pub bytes: [u8; 512],
}
impl Data {
    pub fn new(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > 512 {
            return Err(Error::InputOverflow);
        }
        let mut value = Self {
            length: bytes.len() as u16,
            bytes: [0; 512],
        };
        value.bytes[..bytes.len()].copy_from_slice(bytes);
        Ok(value)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
}
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Ready,
    Failed(Error),
    /// The host has retired its connections and will publish Ready on resync.
    Restarting(Error),
    Found {
        scan: u64,
        /// Address used to establish this advertised connection.
        address: Peer,
        peer: Peer,
        connectable: bool,
        kind: cordial_core::bluetooth::DeviceKind,
        name: Box<str>,
        rssi: i16,
    },
    Incoming {
        attempt: u32,
        peer: Peer,
    },
    Connected {
        token: u32,
        max_output: u16,
    },
    Security {
        token: u32,
        identity: Peer,
        security: cordial_core::bluetooth::ConnectionSecurity,
    },
    Prompt {
        token: u32,
        method: PromptMethod,
        value: Option<Box<str>>,
    },
    Disconnected {
        token: u32,
        error: Option<Error>,
    },
    Service {
        request: u32,
        start: u16,
        end: u16,
    },
    Characteristic {
        request: u32,
        declaration: Option<u16>,
        value: u16,
        properties: u8,
        uuid: u16,
    },
    Descriptor {
        request: u32,
        handle: u16,
        uuid: u16,
    },
    Data {
        request: u32,
        offset: u16,
        data: Data,
    },
    Complete {
        request: u32,
        result: Result<(), Error>,
    },
    Notification {
        token: u32,
        handle: u16,
        data: Data,
    },
}

#[allow(async_fn_in_trait)]
pub trait Host {
    /// Whether the host and controller can scan while initiating a BLE link.
    fn scan_and_connect(&self) -> bool;
    fn bond_capacity(&self) -> usize {
        0
    }
    /// Callbacks never reenter the profile owner. Tokens are copied, retained
    /// through completion, and never inferred from a reused connection slot.
    /// Notifications are published only from an encrypted, bonded link.
    fn start(&mut self) -> Result<(), Error>;
    fn next_event(&mut self) -> Option<Event>;
    async fn changed(&self);
    fn scan(&mut self, id: u64, enabled: bool) -> Result<(), Error>;
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error>;
    fn incoming(&mut self, attempt: u32, token: Option<u32>) -> Result<(), Error>;
    fn connect(&mut self, token: u32, peer: Peer, pairing: bool) -> Result<(), Error>;
    fn adopt(&mut self, token: u32) -> Result<(), Error>;
    /// An error means the host could not accept the request now; the link
    /// is unchanged and the profile repeats the request later.
    fn disconnect(&mut self, token: u32) -> Result<(), Error>;
    fn pair_reply(
        &mut self,
        token: u32,
        method: PromptMethod,
        accept: bool,
        value: Option<&str>,
    ) -> Result<(), Error>;
    fn services(&mut self, token: u32, request: u32, uuid: u16) -> Result<(), Error>;
    fn characteristics(
        &mut self,
        token: u32,
        request: u32,
        start: u16,
        end: u16,
    ) -> Result<(), Error>;
    fn descriptors(&mut self, token: u32, request: u32, start: u16, end: u16) -> Result<(), Error>;
    fn read(
        &mut self,
        token: u32,
        request: u32,
        handle: u16,
        descriptor: bool,
    ) -> Result<(), Error>;
    /// Reads the value of the first attribute of type `uuid` in the whole
    /// database, delivering Data and Complete like `read`. Any ATT error
    /// response from the device completes with `UnsupportedHid`; no other
    /// failure does.
    fn read_by_uuid(&mut self, token: u32, request: u32, uuid: u16) -> Result<(), Error>;
    fn subscribe(
        &mut self,
        token: u32,
        request: u32,
        value: u16,
        cccd: u16,
        indications: bool,
    ) -> Result<(), Error>;
    /// The host copies all borrowed bytes before returning. Complete means the
    /// acknowledged write finished, or the host accepted a write command.
    fn write(
        &mut self,
        token: u32,
        request: u32,
        handle: u16,
        bytes: &[u8],
        response: bool,
    ) -> Result<(), Error>;
    /// The host refreshes these identities before publishing Disconnected,
    /// including cancellation before security or MTU setup has completed.
    async fn import_bond(&mut self, _bond: &cordial_core::bonds::Bond) -> Result<(), Error> {
        Err(Error::UnsupportedTransport)
    }
    async fn export_bond(&mut self, _peer: Peer) -> Result<cordial_core::bonds::Bond, Error> {
        Err(Error::AuthenticationFailed)
    }
    fn bonds(&mut self) -> Result<Vec<Peer>, Error>;
    async fn forget(&mut self, peer: Peer) -> Result<(), Error>;
}
