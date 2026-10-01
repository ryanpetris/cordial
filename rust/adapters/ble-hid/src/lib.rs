#![no_std]
extern crate alloc;

mod information;
pub mod native;
use alloc::{boxed::Box, collections::VecDeque, vec::Vec};
use cordial_core::model::{errors::ErrorCode as Error, identifiers::Transport, link::PromptMethod};
use cordial_core::{
    bluetooth::{Bluetooth, Capabilities, Descriptor, Event, InputReport, ReportType},
    devices::Peer,
    hid,
    link::{LinkId, ServiceId, WriteId},
};
use native::{Event as NativeEvent, Host};

const EVENTS: usize = 8;
const SERVICES: usize = 3;
const CHARACTERISTICS: usize = 32;
const SETUP_TIMEOUT: u64 = 120_000;
#[derive(Clone, Copy)]
struct Service {
    start: u16,
    end: u16,
}
#[derive(Clone, Copy)]
struct Characteristic {
    value: u16,
    end: u16,
    properties: u8,
    uuid: u16,
}
#[derive(Clone, Copy)]
struct Report {
    service: ServiceId,
    value: u16,
    properties: u8,
    id: u8,
    kind: ReportType,
}
#[derive(Clone, Copy)]
enum Stage {
    Security,
    Services,
    Characteristics,
    Map,
    Protocol,
    Descriptors,
    Reference,
    Subscribe,
    Ready,
}
#[derive(Clone, Copy)]
enum Operation {
    Information,
    Setup,
    Write(WriteId),
    Read { id: WriteId, report: Report },
}
struct Pending {
    request: u32,
    operation: Operation,
    data: Vec<u8>,
}
struct Link {
    id: LinkId,
    token: u32,
    peer: Peer,
    pairing: bool,
    adopted: bool,
    secured: bool,
    bonded: bool,
    closing: bool,
    error: Option<Error>,
    deadline: u64,
    max_output: usize,
    existing_bonds: Vec<Peer>,
    stage: Stage,
    services: Vec<Service>,
    service: usize,
    characteristics: Vec<Characteristic>,
    characteristic: usize,
    reference: u16,
    cccd: u16,
    descriptors: Vec<Descriptor>,
    reports: Vec<Report>,
    pending: Option<Pending>,
    info: information::Information,
}

pub struct Backend<H> {
    host: H,
    now: fn() -> u64,
    ready: bool,
    sequence: u32,
    links: [Option<Link>; 4],
    events: VecDeque<Event>,
    incoming: Option<(u32, Peer)>,
    reconnect: Vec<Peer>,
}
fn push<T>(values: &mut Vec<T>, value: T, limit: usize) -> Result<(), Error> {
    if values.len() == limit {
        return Err(Error::Capacity);
    }
    values.try_reserve(1).map_err(|_| Error::Capacity)?;
    values.push(value);
    Ok(())
}
impl<H: Host> Backend<H> {
    pub fn new(host: H, now: fn() -> u64) -> Result<Self, Error> {
        let mut events = VecDeque::new();
        events
            .try_reserve_exact(EVENTS)
            .map_err(|_| Error::Capacity)?;
        Ok(Self {
            host,
            now,
            ready: false,
            sequence: 0,
            links: core::array::from_fn(|_| None),
            events,
            incoming: None,
            reconnect: Vec::new(),
        })
    }
    fn open(
        &mut self,
        id: LinkId,
        peer: Peer,
        pairing: bool,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        if !self.ready {
            return Err(Error::RadioUnavailable);
        }
        if peer.transport != Transport::Ble {
            return Err(Error::UnsupportedTransport);
        }
        let slot = usize::from(id.slot);
        if id.generation == 0 || slot >= 4 {
            return Err(Error::InvalidArgs);
        }
        if self.links[slot].is_some()
            || self
                .links
                .iter()
                .flatten()
                .any(|l| !matches!(l.stage, Stage::Ready) || (!l.closing && l.peer == peer))
        {
            return Err(Error::Busy);
        }
        let bonds = self.host.bonds()?;
        if !pairing && !bonds.contains(&peer) {
            return Err(Error::AuthenticationFailed);
        }
        let token = self.sequence()?;
        if let Some(attempt) = attempt {
            self.host.incoming(attempt, Some(token))?;
        } else {
            self.host.connect(token, peer, pairing)?;
        }
        self.links[slot] = Some(Link {
            id,
            token,
            peer,
            pairing,
            adopted: !pairing,
            secured: false,
            bonded: false,
            closing: false,
            error: None,
            deadline: (self.now)().saturating_add(SETUP_TIMEOUT),
            max_output: 20,
            existing_bonds: if pairing { bonds } else { Vec::new() },
            stage: Stage::Security,
            services: Vec::new(),
            service: 0,
            characteristics: Vec::new(),
            characteristic: 0,
            reference: 0,
            cccd: 0,
            descriptors: Vec::new(),
            reports: Vec::new(),
            pending: None,
            info: information::Information::default(),
        });
        Ok(())
    }
    pub fn start(&mut self) -> Result<(), Error> {
        self.host.start()
    }
    pub fn next_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
    fn sequence(&mut self) -> Result<u32, Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        Ok(self.sequence)
    }
    fn slot(&self, id: LinkId) -> Result<usize, Error> {
        let slot = usize::from(id.slot);
        self.links
            .get(slot)
            .and_then(Option::as_ref)
            .filter(|l| l.id == id)
            .map(|_| slot)
            .ok_or(Error::NotConnected)
    }
    fn close(&mut self, link: &mut Link, error: Error) {
        if !link.closing {
            link.error = Some(error);
            link.closing = true;
            link.pending = None;
            self.host.disconnect(link.token);
        }
    }
    pub async fn poll(&mut self) {
        // Pairing security can emit both an observation and a newly saved bond.
        while self.incoming.is_none() && self.events.len() + 2 <= EVENTS {
            let Some(event) = self.host.next_event() else {
                break;
            };
            self.event(event).await;
        }
        for slot in 0..self.links.len() {
            if let Some(mut link) = self.links[slot].take() {
                if !link.closing
                    && !matches!(link.stage, Stage::Ready)
                    && (self.now)() >= link.deadline
                {
                    self.close(&mut link, Error::Timeout);
                }
                if self.events.len() + 2 <= EVENTS {
                    self.information_poll(&mut link);
                }
                self.links[slot] = Some(link);
            }
        }
    }
    async fn event(&mut self, event: NativeEvent) {
        let (token, request) = match &event {
            NativeEvent::Incoming { attempt, peer } => {
                self.incoming = Some((*attempt, *peer));
                self.events.push_back(Event::Incoming {
                    attempt: *attempt,
                    peer: *peer,
                });
                return;
            }
            NativeEvent::Ready => {
                self.reconnect.clear();
                self.ready = true;
                self.events.push_back(Event::Ready);
                return;
            }
            NativeEvent::Restarting(error) => {
                self.ready = false;
                // A submitted CONNECT may still be waiting on the host's
                // queue when reset retires its own links. Cancel it too.
                for slot in 0..4 {
                    if let Some(mut link) = self.links[slot].take() {
                        self.close(&mut link, *error);
                        self.links[slot] = Some(link);
                    }
                }
                self.events.push_back(Event::Restarting(*error));
                return;
            }
            NativeEvent::Failed(error) => {
                self.ready = false;
                for slot in 0..4 {
                    if let Some(mut link) = self.links[slot].take() {
                        self.close(&mut link, *error);
                        self.links[slot] = Some(link);
                    }
                }
                self.events.push_back(Event::Failed(*error));
                return;
            }
            NativeEvent::Found {
                scan,
                address,
                peer,
                connectable,
                kind,
                name,
                rssi,
            } => {
                self.events.push_back(Event::Found {
                    scan: *scan,
                    address: Some(*address),
                    peer: *peer,
                    connectable: *connectable,
                    kind: *kind,
                    name: name.clone(),
                    rssi: Some(*rssi),
                });
                return;
            }
            NativeEvent::Connected { token, .. }
            | NativeEvent::Security { token, .. }
            | NativeEvent::Prompt { token, .. }
            | NativeEvent::Disconnected { token, .. }
            | NativeEvent::Notification { token, .. } => (Some(*token), None),
            NativeEvent::Service { request, .. }
            | NativeEvent::Characteristic { request, .. }
            | NativeEvent::Descriptor { request, .. }
            | NativeEvent::Data { request, .. }
            | NativeEvent::Complete { request, .. } => (None, Some(*request)),
        };
        let Some(slot) = self.links.iter().position(|l| {
            l.as_ref().is_some_and(|l| {
                token == Some(l.token)
                    || l.pending
                        .as_ref()
                        .is_some_and(|p| Some(p.request) == request)
            })
        }) else {
            return;
        };
        let mut link = self.links[slot].take().unwrap();
        if let NativeEvent::Disconnected { error, .. } = event {
            let mut error = link.error.or(error);
            if link.pairing && !link.adopted {
                // One pairing procedure is admitted at a time. The native host
                // publishes its current identities before Disconnected, even
                // if cancellation preceded the security/MTU completion. Remove
                // only bonds created by this attempt, not its temporary OTA
                // address and never a bond that existed before it started.
                match self.host.bonds() {
                    Ok(bonds) => {
                        for peer in bonds {
                            if !link.existing_bonds.contains(&peer)
                                && self.host.forget(peer).await.is_err()
                            {
                                error = Some(Error::StorageFailed);
                            }
                        }
                    }
                    Err(_) => error = Some(Error::StorageFailed),
                }
            }
            self.events.push_back(Event::Disconnected {
                link: link.id,
                error,
            });
            return;
        }
        if !link.closing
            && let Err(error) = self.link_event(&mut link, event)
        {
            self.close(&mut link, error);
        }
        self.links[slot] = Some(link);
    }
    fn link_event(&mut self, link: &mut Link, event: NativeEvent) -> Result<(), Error> {
        if link
            .pending
            .as_ref()
            .is_some_and(|p| matches!(p.operation, Operation::Information))
            && matches!(
                &event,
                NativeEvent::Service { .. }
                    | NativeEvent::Characteristic { .. }
                    | NativeEvent::Descriptor { .. }
                    | NativeEvent::Data { .. }
                    | NativeEvent::Complete { .. }
            )
        {
            self.information_event(link, event);
            return Ok(());
        }
        match event {
            NativeEvent::Connected { max_output, .. } => {
                link.max_output = usize::from(max_output).min(hid::REPORT_BYTES);
            }
            NativeEvent::Security {
                identity, security, ..
            } => {
                if security.encrypted != Some(true) || security.bonded != Some(true) {
                    return Err(Error::AuthenticationFailed);
                }
                self.events.push_back(Event::Security {
                    link: link.id,
                    security,
                });
                link.peer = identity;
                link.secured = true;
                if link.pairing && !link.bonded {
                    link.bonded = true;
                    self.events.push_back(Event::Bonded {
                        link: link.id,
                        identity,
                    });
                } else if link.adopted && matches!(link.stage, Stage::Security) {
                    self.setup(link, Stage::Services)?;
                }
            }
            NativeEvent::Prompt { method, value, .. } => {
                if !link.pairing || link.adopted {
                    return Err(Error::AuthenticationFailed);
                }
                self.events.push_back(Event::Prompt {
                    link: link.id,
                    method,
                    value,
                });
            }
            NativeEvent::Service { start, end, .. } => {
                if !matches!(link.stage, Stage::Services)
                    || start == 0
                    || end < start
                    || link
                        .services
                        .iter()
                        .any(|s| start <= s.end && end >= s.start)
                {
                    return Err(Error::UnsupportedHid);
                }
                push(&mut link.services, Service { start, end }, SERVICES)?;
            }
            NativeEvent::Characteristic {
                declaration,
                value,
                properties,
                uuid,
                ..
            } => {
                let service = link.services[link.service];
                // Some public cache APIs expose only the value handle. The
                // descriptor enumeration remains scoped to this characteristic.
                let boundary = declaration.unwrap_or(value);
                if !matches!(link.stage, Stage::Characteristics)
                    || boundary < service.start
                    || value < boundary
                    || value > service.end
                    || declaration.is_some_and(|d| d >= value)
                    || link
                        .characteristics
                        .last()
                        .is_some_and(|c| c.value >= boundary)
                {
                    return Err(Error::UnsupportedHid);
                }
                if let Some(last) = link.characteristics.last_mut() {
                    last.end = boundary - 1;
                }
                push(
                    &mut link.characteristics,
                    Characteristic {
                        value,
                        end: service.end,
                        properties,
                        uuid,
                    },
                    CHARACTERISTICS,
                )?;
            }
            NativeEvent::Descriptor { handle, uuid, .. } => {
                let c = link.characteristics[link.characteristic];
                if !matches!(link.stage, Stage::Descriptors) || handle <= c.value || handle > c.end
                {
                    return Err(Error::UnsupportedHid);
                }
                match uuid {
                    0x2908 if link.reference == 0 => link.reference = handle,
                    0x2902 if link.cccd == 0 => link.cccd = handle,
                    0x2908 | 0x2902 => return Err(Error::UnsupportedHid),
                    _ => {}
                }
            }
            NativeEvent::Data { offset, data, .. } => {
                let pending = link.pending.as_mut().unwrap();
                let bytes = data.bytes();
                let limit = if matches!(pending.operation, Operation::Setup)
                    && matches!(link.stage, Stage::Map)
                {
                    hid::DESCRIPTOR_BYTES
                } else {
                    hid::REPORT_BYTES
                };
                if usize::from(offset) != pending.data.len()
                    || pending.data.len() + bytes.len() > limit
                {
                    return Err(Error::InputOverflow);
                }
                pending
                    .data
                    .try_reserve(bytes.len())
                    .map_err(|_| Error::Capacity)?;
                pending.data.extend_from_slice(bytes);
            }
            NativeEvent::Complete { result, .. } => {
                let pending = link.pending.take().unwrap();
                match pending.operation {
                    Operation::Information => unreachable!(),
                    Operation::Write(id) => self.events.push_back(Event::Written { id, result }),
                    Operation::Read { id, report } => {
                        let result = result.and_then(|()| {
                            InputReport::new(link.id, report.service, report.id, &pending.data)
                        });
                        self.events.push_back(Event::Read {
                            id,
                            report_type: report.kind,
                            result,
                        });
                    }
                    Operation::Setup => {
                        result?;
                        self.setup_complete(link, pending.data)?;
                    }
                }
            }
            NativeEvent::Notification { handle, data, .. }
                if matches!(link.stage, Stage::Ready) && link.adopted && link.secured =>
            {
                if self.information_notification(link, handle, data.bytes()) {
                    return Ok(());
                }
                if let Some(report) = link
                    .reports
                    .iter()
                    .find(|r| r.value == handle && r.kind != ReportType::Output)
                {
                    self.events.push_back(Event::Input(InputReport::new(
                        link.id,
                        report.service,
                        report.id,
                        data.bytes(),
                    )?));
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn setup(&mut self, link: &mut Link, stage: Stage) -> Result<(), Error> {
        let request = self.sequence()?;
        let token = link.token;
        link.stage = stage;
        link.pending = Some(Pending {
            request,
            operation: Operation::Setup,
            data: Vec::new(),
        });
        match stage {
            Stage::Services => self.host.services(token, request, 0x1812),
            Stage::Characteristics => {
                link.characteristics.clear();
                let service = link.services[link.service];
                self.host
                    .characteristics(token, request, service.start, service.end)
            }
            Stage::Map => {
                let c = link
                    .characteristics
                    .iter()
                    .find(|c| c.uuid == 0x2a4b)
                    .ok_or(Error::UnsupportedHid)?;
                self.host.read(token, request, c.value, false)
            }
            Stage::Protocol => {
                let c = link
                    .characteristics
                    .iter()
                    .find(|c| c.uuid == 0x2a4e)
                    .ok_or(Error::UnsupportedHid)?;
                if c.properties & 0x0c == 0 {
                    return Err(Error::UnsupportedHid);
                }
                self.host
                    .write(token, request, c.value, &[1], c.properties & 0x04 == 0)
            }
            Stage::Descriptors => {
                link.reference = 0;
                link.cccd = 0;
                let c = link.characteristics[link.characteristic];
                if c.value >= c.end {
                    return Err(Error::UnsupportedHid);
                }
                self.host.descriptors(token, request, c.value + 1, c.end)
            }
            Stage::Reference => {
                if link.reference == 0 {
                    return Err(Error::UnsupportedHid);
                }
                self.host.read(token, request, link.reference, true)
            }
            Stage::Subscribe => {
                let c = link.characteristics[link.characteristic];
                if link.cccd == 0 || c.properties & 0x30 == 0 {
                    return Err(Error::UnsupportedHid);
                }
                self.host
                    .subscribe(token, request, c.value, link.cccd, c.properties & 0x10 == 0)
            }
            _ => Err(Error::InternalError),
        }
    }
    fn setup_complete(&mut self, link: &mut Link, data: Vec<u8>) -> Result<(), Error> {
        match link.stage {
            Stage::Services => {
                if link.services.is_empty() {
                    return Err(Error::UnsupportedHid);
                }
                self.setup(link, Stage::Characteristics)
            }
            Stage::Characteristics => self.setup(link, Stage::Map),
            Stage::Map => {
                if data.is_empty() {
                    return Err(Error::UnsupportedHid);
                }
                push(
                    &mut link.descriptors,
                    Descriptor::from_owned(ServiceId(link.service as u16), data)?,
                    SERVICES,
                )?;
                if link.characteristics.iter().any(|c| c.uuid == 0x2a4e) {
                    self.setup(link, Stage::Protocol)
                } else {
                    self.next_report(link, 0)
                }
            }
            Stage::Protocol => self.next_report(link, 0),
            Stage::Descriptors => self.setup(link, Stage::Reference),
            Stage::Reference => {
                if data.len() != 2 {
                    return Err(Error::UnsupportedHid);
                }
                let kind = match data[1] {
                    1 => ReportType::Input,
                    2 => ReportType::Output,
                    3 => ReportType::Feature,
                    _ => return Err(Error::UnsupportedHid),
                };
                let service = ServiceId(link.service as u16);
                if link
                    .reports
                    .iter()
                    .any(|r| r.service == service && r.kind == kind && r.id == data[0])
                {
                    return Err(Error::UnsupportedHid);
                }
                let c = link.characteristics[link.characteristic];
                push(
                    &mut link.reports,
                    Report {
                        service,
                        value: c.value,
                        properties: c.properties,
                        id: data[0],
                        kind,
                    },
                    CHARACTERISTICS,
                )?;
                if kind == ReportType::Input
                    || (kind == ReportType::Feature && c.properties & 0x30 != 0)
                {
                    self.setup(link, Stage::Subscribe)
                } else {
                    self.next_report(link, link.characteristic + 1)
                }
            }
            Stage::Subscribe => self.next_report(link, link.characteristic + 1),
            _ => Err(Error::InternalError),
        }
    }
    fn next_report(&mut self, link: &mut Link, start: usize) -> Result<(), Error> {
        if let Some(index) =
            (start..link.characteristics.len()).find(|&i| link.characteristics[i].uuid == 0x2a4d)
        {
            link.characteristic = index;
            self.setup(link, Stage::Descriptors)
        } else {
            link.service += 1;
            if link.service < link.services.len() {
                return self.setup(link, Stage::Characteristics);
            }
            if !link.reports.iter().any(|r| r.kind == ReportType::Input) {
                return Err(Error::UnsupportedHid);
            }
            link.stage = Stage::Ready;
            link.characteristics = Vec::new();
            link.services = Vec::new();
            link.existing_bonds = Vec::new();
            self.events.push_back(Event::Connected {
                link: link.id,
                descriptors: core::mem::take(&mut link.descriptors),
                max_output: link.max_output,
            });
            Ok(())
        }
    }
    fn report(
        &self,
        id: WriteId,
        service: ServiceId,
        kind: ReportType,
        report_id: Option<u8>,
    ) -> Result<(usize, Report), Error> {
        let slot = self.slot(id.link)?;
        let link = self.links[slot].as_ref().unwrap();
        if link.closing || !matches!(link.stage, Stage::Ready) {
            return Err(Error::NotConnected);
        }
        if link.pending.is_some() {
            return Err(Error::Busy);
        }
        let report = *link
            .reports
            .iter()
            .find(|r| r.service == service && r.kind == kind && r.id == report_id.unwrap_or(0))
            .ok_or(Error::UnsupportedHid)?;
        Ok((slot, report))
    }
}
impl<H: Host> Bluetooth for Backend<H> {
    fn refresh_info(&mut self, id: LinkId) -> Result<(), Error> {
        let slot = self.slot(id)?;
        let link = self.links[slot].as_mut().unwrap();
        link.info.refresh();
        Ok(())
    }
    fn info_busy(&self, id: LinkId) -> bool {
        self.slot(id)
            .ok()
            .is_some_and(|s| self.links[s].as_ref().unwrap().info.busy())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            classic: false,
            ble: true,
            ble_scan_and_connect: self.host.scan_and_connect(),
        }
    }
    fn bond_capacity(&self, transport: Transport) -> usize {
        if transport == Transport::Ble {
            self.host.bond_capacity()
        } else {
            0
        }
    }
    fn scan(&mut self, id: u64, classic: bool, ble: bool) -> Result<(), Error> {
        if classic {
            return Err(Error::UnsupportedTransport);
        }
        if !self.ready {
            return Err(Error::RadioUnavailable);
        }
        self.host.scan(id, ble)
    }
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error> {
        if peers == self.reconnect {
            return Ok(());
        }
        self.host.reconnect(peers)?;
        self.reconnect = peers.to_vec();
        Ok(())
    }
    fn connect(&mut self, id: LinkId, peer: Peer, pairing: bool) -> Result<(), Error> {
        if let Some((attempt, _)) = self.incoming.take() {
            self.host.incoming(attempt, None)?;
        }
        self.open(id, peer, pairing, None)
    }
    fn incoming(&mut self, attempt: u32, id: Option<LinkId>) -> Result<(), Error> {
        let Some((pending, peer)) = self.incoming else {
            return Err(Error::NotConnected);
        };
        if pending != attempt {
            return Err(Error::NotConnected);
        }
        self.incoming = None;
        if let Some(id) = id {
            if let Err(error) = self.open(id, peer, false, Some(attempt)) {
                self.host.incoming(attempt, None)?;
                return Err(error);
            }
            Ok(())
        } else {
            self.host.incoming(attempt, None)
        }
    }
    fn disconnect(&mut self, id: LinkId) {
        if let Ok(slot) = self.slot(id) {
            let mut link = self.links[slot].take().unwrap();
            link.closing = true;
            link.pending = None;
            self.host.disconnect(link.token);
            self.links[slot] = Some(link);
        }
    }
    fn adopt(&mut self, id: LinkId) -> Result<(), Error> {
        let slot = self.slot(id)?;
        let mut link = self.links[slot].take().unwrap();
        let result = if link.closing || !link.bonded || !link.secured {
            Err(Error::NotConnected)
        } else {
            if let Err(error) = self.host.adopt(link.token) {
                self.links[slot] = Some(link);
                return Err(error);
            }
            self.setup(&mut link, Stage::Services).map(|()| {
                link.adopted = true;
                link.pairing = false;
            })
        };
        self.links[slot] = Some(link);
        result
    }
    fn pair_reply(
        &mut self,
        id: LinkId,
        method: PromptMethod,
        accept: bool,
        value: Option<&str>,
    ) -> Result<(), Error> {
        let link = self.links[self.slot(id)?].as_ref().unwrap();
        if !link.pairing || link.adopted || link.closing {
            return Err(Error::StalePrompt);
        }
        self.host.pair_reply(link.token, method, accept, value)
    }
    fn can_write(&self, id: LinkId) -> bool {
        self.slot(id).ok().is_some_and(|s| {
            self.links[s].as_ref().is_some_and(|l| {
                !l.closing && matches!(l.stage, Stage::Ready) && l.pending.is_none()
            })
        })
    }
    fn write(
        &mut self,
        id: WriteId,
        service: ServiceId,
        kind: ReportType,
        report_id: Option<u8>,
        payload: &[u8],
    ) -> Result<(), Error> {
        if payload.len() > hid::REPORT_BYTES {
            return Err(Error::InputOverflow);
        }
        let (slot, report) = self.report(id, service, kind, report_id)?;
        if report.properties & 0x0c == 0 {
            return Err(Error::UnsupportedHid);
        }
        let request = self.sequence()?;
        let link = self.links[slot].as_mut().unwrap();
        if report.properties & 0x08 == 0 && payload.len() > link.max_output {
            return Err(Error::HidReportTooLarge);
        }
        self.host.write(
            link.token,
            request,
            report.value,
            payload,
            // Output reports use the HID Data Output path when advertised.
            // HID++ still validates its own response and setting readback.
            report.properties & 0x08 != 0
                && (kind != ReportType::Output
                    || report.properties & 0x04 == 0
                    || payload.len() > link.max_output),
        )?;
        link.pending = Some(Pending {
            request,
            operation: Operation::Write(id),
            data: Vec::new(),
        });
        Ok(())
    }
    fn read(
        &mut self,
        id: WriteId,
        service: ServiceId,
        kind: ReportType,
        report_id: Option<u8>,
    ) -> Result<(), Error> {
        let (slot, report) = self.report(id, service, kind, report_id)?;
        if report.properties & 0x02 == 0 {
            return Err(Error::UnsupportedHid);
        }
        let request = self.sequence()?;
        let link = self.links[slot].as_mut().unwrap();
        self.host.read(link.token, request, report.value, false)?;
        link.pending = Some(Pending {
            request,
            operation: Operation::Read { id, report },
            data: Vec::new(),
        });
        Ok(())
    }
    async fn import_bond(&mut self, bond: &cordial_core::bonds::Bond) -> Result<(), Error> {
        self.host.import_bond(bond).await
    }
    async fn export_bond(&mut self, peer: Peer) -> Result<cordial_core::bonds::Bond, Error> {
        self.host.export_bond(peer).await
    }
    async fn bonds(&mut self) -> Result<Vec<Peer>, Error> {
        self.host.bonds()
    }
    async fn forget(&mut self, peer: Peer) -> Result<(), Error> {
        if self.links.iter().flatten().any(|l| l.peer == peer) {
            return Err(Error::Busy);
        }
        self.host.forget(peer).await
    }
}

impl<H: Host> cordial_core::bluetooth::EventSource for Backend<H> {
    async fn poll(&mut self) {
        Backend::poll(self).await;
    }
    fn next_event(&mut self) -> Option<Event> {
        Backend::next_event(self)
    }
    async fn changed(&self) {
        self.host.changed().await;
    }
}
