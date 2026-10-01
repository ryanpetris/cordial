//! Native GAP/GATT callbacks copy into a bounded queue. Only the application
//! owner runs the common BLE HID profile; native tasks retain their own bonds.
use cordial_ble_hid::native::{Data, Event, Host};
use cordial_core::devices::{Peer, display_name};
use cordial_core::model::{
    errors::ErrorCode as Error, identifiers::Transport, link::PromptMethod,
};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex as Raw},
    channel::Channel,
    signal::Signal,
};
use esp_idf_sys::platform as ffi;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};

static EVENTS: Channel<Raw, Event, 40> = Channel::new();
static FAULT: AtomicBool = AtomicBool::new(false);
static TAKEN: AtomicBool = AtomicBool::new(false);
static BOND_RESULT: Signal<Raw, (u32, Result<Vec<u8>, Error>)> = Signal::new();
static FORGOTTEN: Signal<Raw, (u32, Result<(), Error>)> = Signal::new();
static CHANGED: Signal<Raw, ()> = Signal::new();
// Identities only, not a mirror of opaque keys. Updated by native store owners
// before Ready, after bonding and after explicit removal.
type BondIdentities = ([ffi::cordial_ble_peer; 8], usize);
static BONDS: Mutex<Raw, RefCell<Option<BondIdentities>>> = Mutex::new(RefCell::new(None));

fn error(code: u8) -> Error {
    match u32::from(code) {
        ffi::cordial_ble_error_CORDIAL_BLE_BUSY => Error::Busy,
        ffi::cordial_ble_error_CORDIAL_BLE_CAPACITY => Error::Capacity,
        ffi::cordial_ble_error_CORDIAL_BLE_CONNECTION => Error::ConnectionFailed,
        ffi::cordial_ble_error_CORDIAL_BLE_AUTH => Error::AuthenticationFailed,
        ffi::cordial_ble_error_CORDIAL_BLE_UNSUPPORTED => Error::UnsupportedHid,
        ffi::cordial_ble_error_CORDIAL_BLE_STORAGE => Error::StorageFailed,
        ffi::cordial_ble_error_CORDIAL_BLE_OVERFLOW => Error::InputOverflow,
        ffi::cordial_ble_error_CORDIAL_BLE_TIMEOUT => Error::Timeout,
        ffi::cordial_ble_error_CORDIAL_BLE_REPORT_SIZE => Error::HidReportTooLarge,
        _ => Error::RadioUnavailable,
    }
}
fn result(code: i32) -> Result<(), Error> {
    if code == 0 {
        Ok(())
    } else {
        Err(error(code as u8))
    }
}
fn peer(p: ffi::cordial_ble_peer) -> Peer {
    Peer {
        address: p.address,
        random: p.random != 0,
        transport: Transport::Ble,
    }
}
fn native(p: Peer) -> ffi::cordial_ble_peer {
    ffi::cordial_ble_peer {
        address: p.address,
        random: u8::from(p.random),
    }
}
unsafe extern "C" fn receive(pointer: *const ffi::cordial_ble_event) {
    let event = unsafe { &*pointer };
    if copy_event(event).is_err() {
        FAULT.store(true, Ordering::Release);
    }
    CHANGED.signal(());
}
fn copy_event(e: &ffi::cordial_ble_event) -> Result<(), Error> {
    let bytes = if e.length == 0 {
        &[]
    } else {
        if e.data.is_null() {
            return Err(Error::InputOverflow);
        }
        unsafe { std::slice::from_raw_parts(e.data, usize::from(e.length)) }
    };
    let token = e.token;
    let request = e.request;
    let event = match u32::from(e.kind) {
        ffi::cordial_ble_kind_CORDIAL_BLE_READY => Event::Ready,
        ffi::cordial_ble_kind_CORDIAL_BLE_FAILED => Event::Failed(error(e.code)),
        ffi::cordial_ble_kind_CORDIAL_BLE_RESTARTING => Event::Restarting(error(e.code)),
        ffi::cordial_ble_kind_CORDIAL_BLE_FOUND => {
            if EVENTS.is_full() {
                return Ok(());
            }
            Event::Found {
                scan: e.scan,
                address: peer(e.address),
                peer: peer(e.peer),
                connectable: e.code != 0,
                kind: cordial_core::bluetooth::discovery_kind(Transport::Ble, e.number),
                name: display_name(bytes),
                rssi: e.rssi,
            }
        }
        ffi::cordial_ble_kind_CORDIAL_BLE_INCOMING => Event::Incoming {
            attempt: e.number,
            peer: peer(e.peer),
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_CONNECTED => Event::Connected {
            token,
            max_output: e.number.min(cordial_core::hid::REPORT_BYTES as u32) as u16,
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_SECURITY => Event::Security {
            token,
            identity: peer(e.peer),
            security: cordial_core::bluetooth::ConnectionSecurity {
                bonded: Some(e.bonded != 0),
                encrypted: Some(e.encrypted != 0),
                authenticated: reported_bool(e.authenticated),
                secure_connections: reported_bool(e.secure_connections),
                key_size: (7..=16).contains(&e.key_size).then_some(e.key_size),
            },
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_PROMPT => {
            let method = match u32::from(e.code) {
                ffi::cordial_ble_prompt_CORDIAL_BLE_CONFIRM => PromptMethod::ConfirmPasskey,
                ffi::cordial_ble_prompt_CORDIAL_BLE_ENTER => PromptMethod::EnterPasskey,
                ffi::cordial_ble_prompt_CORDIAL_BLE_DISPLAY => PromptMethod::DisplayPasskey,
                _ => return Err(Error::AuthenticationFailed),
            };
            let value = (method != PromptMethod::EnterPasskey)
                .then(|| format!("{:06}", e.number).into_boxed_str());
            Event::Prompt {
                token,
                method,
                value,
            }
        }
        ffi::cordial_ble_kind_CORDIAL_BLE_DISCONNECTED => Event::Disconnected {
            token,
            error: (e.code != 0).then(|| error(e.code)),
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_SERVICE => Event::Service {
            request,
            start: e.start,
            end: e.end,
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_CHARACTERISTIC => Event::Characteristic {
            request,
            declaration: (e.start != 0).then_some(e.start),
            value: e.handle,
            properties: e.properties,
            uuid: e.uuid,
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_DESCRIPTOR => Event::Descriptor {
            request,
            handle: e.handle,
            uuid: e.uuid,
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_DATA => Event::Data {
            request,
            offset: e.offset,
            data: Data::new(bytes)?,
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_COMPLETE => Event::Complete {
            request,
            result: result(i32::from(e.code)),
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_NOTIFICATION => Event::Notification {
            token,
            handle: e.handle,
            data: Data::new(bytes)?,
        },
        ffi::cordial_ble_kind_CORDIAL_BLE_BONDS => {
            let size = std::mem::size_of::<ffi::cordial_ble_peer>();
            if bytes.len() % size != 0 || bytes.len() / size > 8 {
                return Err(Error::StorageFailed);
            }
            let mut peers = [ffi::cordial_ble_peer {
                address: [0; 6],
                random: 0,
            }; 8];
            for (out, data) in peers.iter_mut().zip(bytes.chunks_exact(size)) {
                *out = unsafe { std::ptr::read_unaligned(data.as_ptr().cast()) };
            }
            BONDS.lock(|b| *b.borrow_mut() = Some((peers, bytes.len() / size)));
            return Ok(());
        }
        ffi::cordial_ble_kind_CORDIAL_BLE_BOND_RESULT => {
            BOND_RESULT.signal((request, result(i32::from(e.code)).map(|()| bytes.to_vec())));
            return Ok(());
        }
        ffi::cordial_ble_kind_CORDIAL_BLE_FORGOTTEN => {
            FORGOTTEN.signal((request, result(i32::from(e.code))));
            return Ok(());
        }
        _ => return Err(Error::RadioUnavailable),
    };
    EVENTS.try_send(event).map_err(|_| Error::Capacity)
}

pub struct Native {
    failed: bool,
    sequence: u32,
}
impl Native {
    async fn bond_command(&mut self, mut c: ffi::cordial_ble_command) -> Result<Vec<u8>, Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        c.request = self.sequence;
        BOND_RESULT.reset();
        Self::submit(&c)?;
        let deadline = embassy_time::Instant::now() + embassy_time::Duration::from_secs(10);
        loop {
            match embassy_time::with_deadline(deadline, BOND_RESULT.wait()).await {
                Ok((request, result)) if request == self.sequence => return result,
                Ok(_) => {}
                Err(_) => return Err(Error::StorageFailed),
            }
        }
    }

    pub fn new() -> Result<Self, Error> {
        if TAKEN.swap(true, Ordering::AcqRel) {
            return Err(Error::Busy);
        }
        Ok(Self {
            failed: false,
            sequence: 0,
        })
    }
    fn command(kind: u32, token: u32, request: u32) -> ffi::cordial_ble_command {
        // A C command consists entirely of integers and an inline byte array.
        let mut command: ffi::cordial_ble_command = unsafe { std::mem::zeroed() };
        command.kind = kind as u8;
        command.token = token;
        command.request = request;
        command
    }
    fn submit(command: &ffi::cordial_ble_command) -> Result<(), Error> {
        result(unsafe { ffi::cordial_ble_submit(command) })
    }
}
impl Host for Native {
    fn scan_and_connect(&self) -> bool {
        false
    }
    fn bond_capacity(&self) -> usize {
        unsafe { ffi::cordial_ble_bond_capacity() as usize }
    }
    fn adopt(&mut self, token: u32) -> Result<(), Error> {
        Self::submit(&Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_ADOPT,
            token,
            0,
        ))
    }
    async fn changed(&self) {
        CHANGED.wait().await;
    }
    fn start(&mut self) -> Result<(), Error> {
        result(unsafe { ffi::cordial_ble_start(Some(receive)) })
    }
    fn next_event(&mut self) -> Option<Event> {
        if FAULT.swap(false, Ordering::AcqRel) && !self.failed {
            self.failed = true;
            while EVENTS.try_receive().is_ok() {}
            return Some(Event::Failed(Error::Capacity));
        }
        if self.failed {
            return None;
        }
        let event = EVENTS.try_receive().ok();
        if matches!(event, Some(Event::Failed(_))) {
            self.failed = true;
        }
        event
    }
    fn scan(&mut self, id: u64, enabled: bool) -> Result<(), Error> {
        let mut c = Self::command(ffi::cordial_ble_command_kind_CORDIAL_BLE_SCAN, 0, 0);
        c.scan = id;
        c.enabled = u8::from(enabled);
        Self::submit(&c)
    }
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error> {
        if peers.len() > self.bond_capacity() {
            return Err(Error::Capacity);
        }
        let mut c = Self::command(ffi::cordial_ble_command_kind_CORDIAL_BLE_RECONNECT, 0, 0);
        for (i, peer) in peers.iter().enumerate() {
            if peer.transport != Transport::Ble {
                return Err(Error::UnsupportedTransport);
            }
            c.data[i * 7..i * 7 + 6].copy_from_slice(&peer.address);
            c.data[i * 7 + 6] = u8::from(peer.random);
        }
        c.length = (peers.len() * 7) as u16;
        Self::submit(&c).map_err(|e| if e == Error::Capacity { Error::Busy } else { e })
    }
    fn incoming(&mut self, attempt: u32, token: Option<u32>) -> Result<(), Error> {
        let mut c = Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_ACCEPT,
            token.unwrap_or(0),
            0,
        );
        c.number = attempt;
        Self::submit(&c)
    }
    fn connect(&mut self, token: u32, peer: Peer, pairing: bool) -> Result<(), Error> {
        let mut c = Self::command(ffi::cordial_ble_command_kind_CORDIAL_BLE_CONNECT, token, 0);
        c.peer = native(peer);
        c.pairing = u8::from(pairing);
        Self::submit(&c)
    }
    fn disconnect(&mut self, token: u32) {
        if Self::submit(&Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_DISCONNECT,
            token,
            0,
        ))
        .is_err()
        {
            FAULT.store(true, Ordering::Release);
        }
    }
    fn pair_reply(
        &mut self,
        token: u32,
        method: PromptMethod,
        accept: bool,
        value: Option<&str>,
    ) -> Result<(), Error> {
        let mut c = Self::command(ffi::cordial_ble_command_kind_CORDIAL_BLE_REPLY, token, 0);
        c.method = match method {
            PromptMethod::ConfirmPasskey => ffi::cordial_ble_prompt_CORDIAL_BLE_CONFIRM,
            PromptMethod::EnterPasskey => ffi::cordial_ble_prompt_CORDIAL_BLE_ENTER,
            PromptMethod::DisplayPasskey => ffi::cordial_ble_prompt_CORDIAL_BLE_DISPLAY,
            _ => return Err(Error::InvalidArgs),
        } as u8;
        c.accept = u8::from(accept);
        if let Some(value) = value {
            if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::InvalidArgs);
            }
            c.number = value.parse().map_err(|_| Error::InvalidArgs)?;
        }
        Self::submit(&c)
    }
    fn services(&mut self, token: u32, request: u32, uuid: u16) -> Result<(), Error> {
        let mut c = Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_SERVICES,
            token,
            request,
        );
        c.number = uuid.into();
        Self::submit(&c)
    }
    fn characteristics(
        &mut self,
        token: u32,
        request: u32,
        start: u16,
        end: u16,
    ) -> Result<(), Error> {
        let mut c = Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_CHARACTERISTICS,
            token,
            request,
        );
        c.start = start;
        c.end = end;
        Self::submit(&c)
    }
    fn descriptors(&mut self, token: u32, request: u32, start: u16, end: u16) -> Result<(), Error> {
        let mut c = Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_DESCRIPTORS,
            token,
            request,
        );
        c.start = start;
        c.end = end;
        Self::submit(&c)
    }
    fn read(
        &mut self,
        token: u32,
        request: u32,
        handle: u16,
        descriptor: bool,
    ) -> Result<(), Error> {
        let mut c = Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_READ,
            token,
            request,
        );
        c.handle = handle;
        c.descriptor = u8::from(descriptor);
        Self::submit(&c)
    }
    fn write(
        &mut self,
        token: u32,
        request: u32,
        handle: u16,
        bytes: &[u8],
        response: bool,
    ) -> Result<(), Error> {
        let mut c = Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_WRITE,
            token,
            request,
        );
        c.handle = handle;
        c.response = u8::from(response);
        if bytes.len() > c.data.len() {
            return Err(Error::InputOverflow);
        }
        c.length = bytes.len() as u16;
        c.data[..bytes.len()].copy_from_slice(bytes);
        Self::submit(&c)
    }
    fn subscribe(
        &mut self,
        token: u32,
        request: u32,
        value: u16,
        cccd: u16,
        indications: bool,
    ) -> Result<(), Error> {
        let mut c = Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_SUBSCRIBE,
            token,
            request,
        );
        c.handle = value;
        c.start = cccd;
        c.enabled = u8::from(indications);
        Self::submit(&c)
    }
    async fn import_bond(&mut self, bond: &cordial_core::bonds::Bond) -> Result<(), Error> {
        let bytes = bond.encode().map_err(|_| Error::StorageFailed)?;
        let mut c = Self::command(ffi::cordial_ble_command_kind_CORDIAL_BLE_IMPORT, 0, 0);
        c.length = bytes.len() as u16;
        c.data[..bytes.len()].copy_from_slice(&bytes);
        self.bond_command(c).await.map(|_| ())
    }
    async fn export_bond(&mut self, peer: Peer) -> Result<cordial_core::bonds::Bond, Error> {
        let mut c = Self::command(ffi::cordial_ble_command_kind_CORDIAL_BLE_EXPORT, 0, 0);
        c.peer = native(peer);
        let bytes = self.bond_command(c).await?;
        cordial_core::bonds::Bond::decode(&bytes).map_err(|_| Error::StorageFailed)
    }
    fn bonds(&mut self) -> Result<Vec<Peer>, Error> {
        let (peers, length) = BONDS
            .lock(|b| b.borrow().as_ref().copied())
            .ok_or(Error::StorageFailed)?;
        let mut out = Vec::new();
        out.try_reserve_exact(length).map_err(|_| Error::Capacity)?;
        out.extend(peers[..length].iter().copied().map(peer));
        Ok(out)
    }
    async fn forget(&mut self, peer: Peer) -> Result<(), Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        let mut c = Self::command(
            ffi::cordial_ble_command_kind_CORDIAL_BLE_FORGET,
            0,
            self.sequence,
        );
        c.peer = native(peer);
        FORGOTTEN.reset();
        Self::submit(&c)?;
        let deadline = embassy_time::Instant::now() + embassy_time::Duration::from_secs(10);
        loop {
            match embassy_time::with_deadline(deadline, FORGOTTEN.wait()).await {
                Ok((request, result)) if request == self.sequence => return result,
                Ok(_) => {}
                Err(_) => return Err(Error::StorageFailed),
            }
        }
    }
}

fn reported_bool(value: u8) -> Option<bool> {
    match value {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

/// Called synchronously on the NimBLE host task with fixed-size valid buffers.
#[unsafe(no_mangle)]
unsafe extern "C" fn cordial_ble_resolve_key(irk: *const u8, rpa: *const u8) -> i32 {
    let irk = unsafe { &*irk.cast::<[u8; 16]>() };
    let mut address = unsafe { *rpa.cast::<[u8; 6]>() };
    address.reverse();
    i32::from(cordial_core::bonds::resolves_rpa(irk, &address))
}
