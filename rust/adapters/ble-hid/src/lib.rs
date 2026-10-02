#![no_std]
extern crate alloc;

mod information;
pub mod native;
use alloc::{boxed::Box, collections::VecDeque, vec::Vec};
use cordial_core::model::{errors::ErrorCode as Error, identifiers::Transport, link::PromptMethod};
use cordial_core::{
    bluetooth::{
        Bluetooth, Capabilities, DatabaseHash, Descriptor, Event, InputReport, Layout,
        LayoutReport, ReportMap, ReportType,
    },
    devices::Peer,
    hid,
    link::{LinkId, ServiceId, WriteId},
};
use native::{Event as NativeEvent, Host};

const EVENTS: usize = 8;
const SERVICES: usize = 3;
const CHARACTERISTICS: usize = 32;
/// Most notifications a link holds until it can route them.
const EARLY: usize = 8;
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
#[derive(Clone, Copy, PartialEq)]
struct Report {
    service: ServiceId,
    value: u16,
    properties: u8,
    id: u8,
    kind: ReportType,
    cccd: u16,
}
impl Report {
    /// Input reports, and Feature reports that notify or indicate, deliver
    /// data through their Client Characteristic Configuration descriptor.
    fn notifying(kind: ReportType, properties: u8) -> bool {
        kind == ReportType::Input || (kind == ReportType::Feature && properties & 0x30 != 0)
    }
    fn layout(&self) -> LayoutReport {
        LayoutReport {
            service: self.service.0,
            kind: self.kind,
            id: self.id,
            value: self.value,
            properties: self.properties,
            cccd: self.cccd,
        }
    }
}
/// Admission phase of a link. Discovery of a supplied layout runs while the
/// link is Ready.
#[derive(Clone, Copy)]
enum Stage {
    Security,
    Setup,
    Ready,
}
/// One ATT procedure of HID service discovery.
#[derive(Clone, Copy, PartialEq)]
enum Step {
    /// Reads the GATT Database Hash, if the device has one.
    Hash,
    Services,
    Characteristics,
    Map,
    Protocol,
    Descriptors,
    Reference,
    Subscribe,
}
#[derive(Clone, Copy)]
enum Operation {
    Information,
    Discover(Step),
    /// Rewrites the CCCD of the indexed report of a supplied layout.
    Subscribe(usize),
    Write(WriteId),
    Read {
        id: WriteId,
        report: Report,
    },
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
    /// The host has not yet accepted this closing link's disconnect request.
    unsent: bool,
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
    /// Raw report maps read by the current discovery.
    maps: Vec<ReportMap>,
    /// Reports found by the current discovery, in discovery order.
    found: Vec<Report>,
    /// The report table that routes notifications and HID requests.
    reports: Vec<Report>,
    /// The supplied layout while it awaits its hash check or verification.
    saved: Option<Saved>,
    /// The Database Hash read by the current discovery.
    hash: Option<DatabaseHash>,
    /// The next verification step, started when the link is idle.
    verify: Option<Step>,
    /// Lets one poll pass between verification steps so HID requests can run.
    verify_yield: bool,
    /// A CCCD write of the supplied layout failed. Verification rewrites every
    /// CCCD; until it finishes, the link cannot rely on its subscriptions.
    cccd_failed: bool,
    /// Notifications not yet routed, oldest first: those received before the
    /// link is Ready, and those that arrive while the event queue is full.
    early: VecDeque<(u16, Box<[u8]>)>,
    pending: Option<Pending>,
    info: information::Information,
}
/// The parts of a supplied layout that its report table does not hold.
struct Saved {
    maps: Vec<ReportMap>,
    hash: Option<DatabaseHash>,
}
impl Link {
    fn clear_discovery(&mut self) {
        self.hash = None;
        self.services = Vec::new();
        self.characteristics = Vec::new();
        self.descriptors = Vec::new();
        self.maps = Vec::new();
        self.found = Vec::new();
    }
    /// Verification failed without learning the device's layout. The supplied
    /// layout remains in use.
    fn abandon_verify(&mut self) {
        self.saved = None;
        self.verify = None;
        self.clear_discovery();
    }
}

pub struct Backend<H> {
    host: H,
    now: fn() -> u64,
    ready: bool,
    sequence: u32,
    links: [Option<Link>; 4],
    events: VecDeque<Event>,
    incoming: Option<(u32, Peer)>,
    /// The accept list the host currently holds.
    reconnect: Vec<Peer>,
    /// The accept list the application last supplied.
    wanted: Vec<Peer>,
    /// BLE is enabled. The value survives host restarts.
    ble: bool,
    /// The host may be scanning at the application's request.
    scanning: bool,
    /// An incoming offer the host has not yet been told to decline.
    decline: Option<u32>,
}
fn push<T>(values: &mut Vec<T>, value: T, limit: usize) -> Result<(), Error> {
    if values.len() == limit {
        return Err(Error::Capacity);
    }
    values.try_reserve(1).map_err(|_| Error::Capacity)?;
    values.push(value);
    Ok(())
}
/// The report table of a layout this profile can admit without discovery.
fn saved_reports(layout: &Layout) -> Option<Vec<Report>> {
    if !layout.valid(Transport::Ble)
        || layout.maps.len() > SERVICES
        || layout.reports.len() > CHARACTERISTICS
        || layout.reports.iter().any(|r| {
            (r.kind == ReportType::Input && r.properties & 0x30 == 0)
                || (Report::notifying(r.kind, r.properties) && r.cccd == 0)
        })
    {
        return None;
    }
    let mut reports = Vec::new();
    reports.try_reserve_exact(layout.reports.len()).ok()?;
    reports.extend(layout.reports.iter().map(|r| Report {
        service: ServiceId(r.service),
        value: r.value,
        properties: r.properties,
        id: r.id,
        kind: r.kind,
        cccd: r.cccd,
    }));
    Some(reports)
}
fn compile(maps: &[ReportMap]) -> Result<Vec<Descriptor>, Error> {
    let mut descriptors = Vec::new();
    descriptors
        .try_reserve_exact(maps.len())
        .map_err(|_| Error::Capacity)?;
    for (i, map) in maps.iter().enumerate() {
        descriptors.push(Descriptor::from_slice(ServiceId(i as u16), &map.0)?);
    }
    Ok(descriptors)
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
            wanted: Vec::new(),
            ble: true,
            scanning: false,
            decline: None,
        })
    }
    fn open(
        &mut self,
        id: LinkId,
        peer: Peer,
        pairing: bool,
        attempt: Option<u32>,
        layout: Option<&Layout>,
    ) -> Result<(), Error> {
        if !self.ready {
            return Err(Error::RadioUnavailable);
        }
        if peer.transport != Transport::Ble || !self.ble {
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
        // A layout the profile cannot use is discovered again instead.
        let (reports, saved) = match layout.filter(|_| !pairing).and_then(|l| {
            let reports = saved_reports(l)?;
            let mut maps = Vec::new();
            maps.try_reserve_exact(l.maps.len()).ok()?;
            maps.extend(l.maps.iter().cloned());
            Some((reports, maps))
        }) {
            Some((reports, maps)) => (
                reports,
                Some(Saved {
                    maps,
                    hash: layout.and_then(|l| l.hash),
                }),
            ),
            None => (Vec::new(), None),
        };
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
            unsent: false,
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
            maps: Vec::new(),
            found: Vec::new(),
            reports,
            saved,
            hash: None,
            verify: None,
            verify_yield: false,
            cccd_failed: false,
            early: VecDeque::new(),
            pending: None,
            info: information::Information::default(),
        });
        Ok(())
    }
    /// Gives the host the application's accept list while BLE is enabled, and
    /// an empty one while it is disabled.
    fn apply_reconnect(&mut self) -> Result<(), Error> {
        let peers: &[Peer] = if self.ble { &self.wanted } else { &[] };
        if peers == self.reconnect.as_slice() {
            return Ok(());
        }
        self.host.reconnect(peers)?;
        self.reconnect = peers.to_vec();
        Ok(())
    }
    /// Brings the host in line with the requested transport state: the
    /// application's accept list while BLE is enabled, and while it is
    /// disabled an empty accept list, no scan, no offer and no open link.
    /// Every step is attempted; the first failure is returned and the step is
    /// retried by the next call.
    fn reconcile(&mut self) -> Result<(), Error> {
        let mut result = self.apply_reconnect();
        if self.ble {
            return result;
        }
        if let Some((attempt, _)) = self.incoming.take() {
            self.decline = Some(attempt);
        }
        if let Some(attempt) = self.decline {
            match self.host.incoming(attempt, None) {
                Ok(()) => self.decline = None,
                Err(error) => result = result.and(Err(error)),
            }
        }
        if self.scanning {
            match self.host.scan(0, false) {
                Ok(()) => self.scanning = false,
                Err(error) => result = result.and(Err(error)),
            }
        }
        for slot in 0..self.links.len() {
            if let Some(id) = self.links[slot]
                .as_ref()
                .filter(|l| !l.closing)
                .map(|l| l.id)
            {
                Bluetooth::disconnect(self, id);
            }
        }
        result
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
            link.early = VecDeque::new();
            self.send_disconnect(link);
        }
    }
    /// A request the host cannot queue now is retried from `poll`. The link
    /// stays closing meanwhile, so no later event admits it.
    fn send_disconnect(&mut self, link: &mut Link) {
        link.unsent = self.host.disconnect(link.token).is_err();
    }
    pub async fn poll(&mut self) {
        if self.ready {
            // Failures are retried on the next poll.
            let _ = self.reconcile();
        }
        // Pairing security can emit both an observation and a newly saved bond.
        while self.incoming.is_none() && self.events.len() + 2 <= EVENTS {
            let Some(event) = self.host.next_event() else {
                break;
            };
            self.event(event).await;
        }
        for slot in 0..self.links.len() {
            if let Some(mut link) = self.links[slot].take() {
                if link.closing && link.unsent {
                    self.send_disconnect(&mut link);
                }
                if !link.closing
                    && !matches!(link.stage, Stage::Ready)
                    && (self.now)() >= link.deadline
                {
                    self.close(&mut link, Error::Timeout);
                }
                if !link.closing
                    && let Err(error) = self.flush(&mut link)
                {
                    self.close(&mut link, error);
                }
                if self.events.len() + 2 <= EVENTS {
                    self.information_poll(&mut link);
                }
                self.verify_poll(&mut link);
                self.links[slot] = Some(link);
            }
        }
    }
    async fn event(&mut self, event: NativeEvent) {
        let (token, request) = match &event {
            NativeEvent::Incoming { attempt, peer } => {
                if !self.ble {
                    self.decline = Some(*attempt);
                    // A failed decline is retried on the next poll.
                    let _ = self.reconcile();
                    return;
                }
                self.incoming = Some((*attempt, *peer));
                self.events.push_back(Event::Incoming {
                    attempt: *attempt,
                    peer: *peer,
                });
                return;
            }
            NativeEvent::Ready => {
                // The restarted host holds no accept list, scan or offer.
                self.reconnect.clear();
                self.scanning = false;
                self.decline = None;
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
        let attribute = matches!(
            &event,
            NativeEvent::Service { .. }
                | NativeEvent::Characteristic { .. }
                | NativeEvent::Descriptor { .. }
                | NativeEvent::Data { .. }
                | NativeEvent::Complete { .. }
        );
        match link.pending.as_ref().map(|p| p.operation) {
            Some(Operation::Information) if attribute => {
                self.information_event(link, event);
                return Ok(());
            }
            Some(Operation::Discover(step)) if attribute => {
                return self.discover_event(link, step, event);
            }
            _ => {}
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
                    // A saved layout with a hash waits for the device's current
                    // hash before it is used.
                    let check = link.saved.as_ref().is_some_and(|s| s.hash.is_some());
                    if check || !self.admit(link) {
                        self.setup(link, Step::Hash)?;
                    }
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
            NativeEvent::Service { .. }
            | NativeEvent::Characteristic { .. }
            | NativeEvent::Descriptor { .. } => return Err(Error::UnsupportedHid),
            NativeEvent::Data { offset, data, .. } => {
                let pending = link.pending.as_mut().unwrap();
                let bytes = data.bytes();
                if usize::from(offset) != pending.data.len()
                    || pending.data.len() + bytes.len() > hid::REPORT_BYTES
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
                    Operation::Information | Operation::Discover(_) => unreachable!(),
                    Operation::Subscribe(index) => {
                        link.cccd_failed |= result.is_err();
                        self.subscribe_saved(link, index + 1);
                    }
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
                }
            }
            NativeEvent::Notification { handle, data, .. } if link.adopted => {
                self.notification(link, handle, data.bytes())?;
            }
            _ => {}
        }
        Ok(())
    }
    /// Notifications are routed in arrival order. Until the link is Ready, or
    /// while the event queue is full, they wait in a small buffer that drops
    /// its oldest entry when full.
    fn notification(&mut self, link: &mut Link, handle: u16, bytes: &[u8]) -> Result<(), Error> {
        if matches!(link.stage, Stage::Ready) {
            self.flush(link)?;
            if link.early.is_empty() && self.events.len() < EVENTS {
                return self.route(link, handle, bytes);
            }
        }
        if link.early.len() == EARLY {
            link.early.pop_front();
        }
        let mut copy = Vec::new();
        if link.early.try_reserve(1).is_ok() && copy.try_reserve_exact(bytes.len()).is_ok() {
            copy.extend_from_slice(bytes);
            link.early.push_back((handle, copy.into_boxed_slice()));
        }
        Ok(())
    }
    fn flush(&mut self, link: &mut Link) -> Result<(), Error> {
        while matches!(link.stage, Stage::Ready) && self.events.len() < EVENTS {
            let Some((handle, bytes)) = link.early.pop_front() else {
                break;
            };
            self.route(link, handle, &bytes)?;
        }
        Ok(())
    }
    fn route(&mut self, link: &Link, handle: u16, bytes: &[u8]) -> Result<(), Error> {
        if self.information_notification(link, handle, bytes) {
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
                bytes,
            )?));
        }
        Ok(())
    }
    /// Admits a link through its supplied layout, then rewrites its CCCDs and
    /// queues background verification. Returns false when discovery must run.
    fn admit(&mut self, link: &mut Link) -> bool {
        let Some(saved) = &link.saved else {
            return false;
        };
        let descriptors = match compile(&saved.maps) {
            Ok(descriptors) => descriptors,
            Err(_) => {
                link.saved = None;
                link.reports = Vec::new();
                return false;
            }
        };
        link.stage = Stage::Ready;
        link.existing_bonds = Vec::new();
        link.verify = Some(Step::Hash);
        self.events.push_back(Event::Connected {
            link: link.id,
            descriptors,
            max_output: link.max_output,
            layout: None,
        });
        self.subscribe_saved(link, 0);
        true
    }
    /// Writes the CCCD of the next notifying report at or after `start`. After
    /// the last one, verification starts at once if any write failed.
    fn subscribe_saved(&mut self, link: &mut Link, start: usize) {
        for index in start..link.reports.len() {
            let r = link.reports[index];
            if !Report::notifying(r.kind, r.properties) {
                continue;
            }
            let Ok(request) = self.sequence() else {
                link.cccd_failed = true;
                break;
            };
            link.pending = Some(Pending {
                request,
                operation: Operation::Subscribe(index),
                data: Vec::new(),
            });
            if self
                .host
                .subscribe(
                    link.token,
                    request,
                    r.value,
                    r.cccd,
                    r.properties & 0x10 == 0,
                )
                .is_ok()
            {
                return;
            }
            link.pending = None;
            link.cccd_failed = true;
        }
        if link.cccd_failed {
            self.verify_step(link, Step::Hash);
        }
    }
    /// Verification is a low-priority job. It starts after the supplied CCCDs
    /// are written and the initial information pass has finished, and each
    /// step waits until no other ATT operation is pending on the link. A
    /// device layout the profile cannot use closes the link with the error a
    /// new connection would report. An error response, or a request the host
    /// cannot submit, leaves the supplied layout in use.
    fn verify_poll(&mut self, link: &mut Link) {
        let Some(step) = link.verify else {
            return;
        };
        if !matches!(link.stage, Stage::Ready) || link.closing || link.pending.is_some() {
            return;
        }
        if core::mem::take(&mut link.verify_yield) {
            return;
        }
        if step == Step::Hash && !link.info.settled() {
            return;
        }
        self.verify_step(link, step);
    }
    /// Verification failed without learning the device's layout, so the
    /// supplied layout stays in use. A link with a failed CCCD write would
    /// miss input, so it closes instead and the device reconnects.
    fn abandon(&mut self, link: &mut Link) {
        if link.cccd_failed {
            self.close(link, Error::ConnectionFailed);
        } else {
            link.abandon_verify();
        }
    }
    fn verify_step(&mut self, link: &mut Link, step: Step) {
        if link.verify.take().is_none() {
            return;
        }
        if let Err(error) = self.setup(link, step) {
            self.close(link, error);
        }
    }
    /// Discovery before admission chains its steps. Verification queues each
    /// next step for `verify_poll`.
    fn advance(&mut self, link: &mut Link, step: Step) -> Result<(), Error> {
        if matches!(link.stage, Stage::Ready) {
            link.verify = Some(step);
            link.verify_yield = true;
            Ok(())
        } else {
            self.setup(link, step)
        }
    }
    /// Starts a discovery step. Errors describe a layout the profile cannot
    /// use, except that a request the host cannot submit during verification
    /// abandons the verification instead.
    fn setup(&mut self, link: &mut Link, step: Step) -> Result<(), Error> {
        let request = self.sequence()?;
        let token = link.token;
        if !matches!(link.stage, Stage::Ready) {
            link.stage = Stage::Setup;
        }
        link.pending = Some(Pending {
            request,
            operation: Operation::Discover(step),
            data: Vec::new(),
        });
        let submitted = match step {
            Step::Hash => {
                link.clear_discovery();
                self.host.read_by_uuid(token, request, 0x2b2a)
            }
            Step::Services => {
                link.service = 0;
                self.host.services(token, request, 0x1812)
            }
            Step::Characteristics => {
                link.characteristics.clear();
                let service = link.services[link.service];
                self.host
                    .characteristics(token, request, service.start, service.end)
            }
            Step::Map => {
                let c = link
                    .characteristics
                    .iter()
                    .find(|c| c.uuid == 0x2a4b)
                    .ok_or(Error::UnsupportedHid)?;
                self.host.read(token, request, c.value, false)
            }
            Step::Protocol => {
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
            Step::Descriptors => {
                link.reference = 0;
                link.cccd = 0;
                let c = link.characteristics[link.characteristic];
                if c.value >= c.end {
                    return Err(Error::UnsupportedHid);
                }
                self.host.descriptors(token, request, c.value + 1, c.end)
            }
            Step::Reference => {
                if link.reference == 0 {
                    return Err(Error::UnsupportedHid);
                }
                self.host.read(token, request, link.reference, true)
            }
            Step::Subscribe => {
                let c = link.characteristics[link.characteristic];
                if link.cccd == 0 || c.properties & 0x30 == 0 {
                    return Err(Error::UnsupportedHid);
                }
                self.host
                    .subscribe(token, request, c.value, link.cccd, c.properties & 0x10 == 0)
            }
        };
        if submitted.is_err() && step == Step::Subscribe {
            return Err(Error::ConnectionFailed);
        }
        if submitted.is_err() && matches!(link.stage, Stage::Ready) {
            link.pending = None;
            self.abandon(link);
            return Ok(());
        }
        submitted
    }
    fn discover_event(
        &mut self,
        link: &mut Link,
        step: Step,
        event: NativeEvent,
    ) -> Result<(), Error> {
        match event {
            NativeEvent::Service { start, end, .. } => {
                if step != Step::Services
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
                if step != Step::Characteristics {
                    return Err(Error::UnsupportedHid);
                }
                let service = link.services[link.service];
                // Some public cache APIs expose only the value handle. The
                // descriptor enumeration remains scoped to this characteristic.
                let boundary = declaration.unwrap_or(value);
                if boundary < service.start
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
                if step != Step::Descriptors {
                    return Err(Error::UnsupportedHid);
                }
                let c = link.characteristics[link.characteristic];
                if handle <= c.value || handle > c.end {
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
                let limit = if step == Step::Map {
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
                let result = if step == Step::Hash {
                    // The host reports an ATT error response as UnsupportedHid:
                    // the device offers no readable hash, as it does when its
                    // value is not 16 bytes. Any other failure reveals nothing.
                    match result {
                        Ok(()) => {
                            link.hash = <[u8; 16]>::try_from(pending.data.as_slice())
                                .ok()
                                .map(DatabaseHash);
                            Ok(())
                        }
                        Err(Error::UnsupportedHid) => Ok(()),
                        Err(error) => Err(error),
                    }
                } else {
                    result
                };
                // Without a CCCD on a discovered handle the report's input would
                // never arrive; the device reconnects instead.
                if step == Step::Subscribe && result.is_err() {
                    return Err(Error::ConnectionFailed);
                }
                if let Err(error) = result {
                    // A failed procedure says nothing about the device's layout.
                    if matches!(link.stage, Stage::Ready) {
                        self.abandon(link);
                        return Ok(());
                    }
                    return Err(error);
                }
                self.setup_complete(link, step, pending.data)?;
            }
            _ => {}
        }
        Ok(())
    }
    /// Before admission, a supplied layout is used only when the device's
    /// hash matches its saved one. Otherwise the device is discovered.
    fn hashed(&mut self, link: &mut Link) -> Result<(), Error> {
        if !matches!(link.stage, Stage::Ready)
            && let Some(saved) = &link.saved
        {
            if saved.hash.is_some() && saved.hash == link.hash && self.admit(link) {
                return Ok(());
            }
            link.saved = None;
            link.reports = Vec::new();
        }
        self.advance(link, Step::Services)
    }
    fn setup_complete(&mut self, link: &mut Link, step: Step, data: Vec<u8>) -> Result<(), Error> {
        match step {
            Step::Hash => self.hashed(link),
            Step::Services => {
                if link.services.is_empty() {
                    return Err(Error::UnsupportedHid);
                }
                self.advance(link, Step::Characteristics)
            }
            Step::Characteristics => self.advance(link, Step::Map),
            Step::Map => {
                if data.is_empty() {
                    return Err(Error::UnsupportedHid);
                }
                let mut data = data;
                data.shrink_to_fit();
                // Verification compiles maps only when the layout changed.
                if !matches!(link.stage, Stage::Ready) {
                    push(
                        &mut link.descriptors,
                        Descriptor::from_slice(ServiceId(link.service as u16), &data)?,
                        SERVICES,
                    )?;
                }
                push(&mut link.maps, ReportMap(data), SERVICES)?;
                if link.characteristics.iter().any(|c| c.uuid == 0x2a4e) {
                    self.advance(link, Step::Protocol)
                } else {
                    self.next_report(link, 0)
                }
            }
            Step::Protocol => self.next_report(link, 0),
            Step::Descriptors => self.advance(link, Step::Reference),
            Step::Reference => {
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
                    .found
                    .iter()
                    .any(|r| r.service == service && r.kind == kind && r.id == data[0])
                {
                    return Err(Error::UnsupportedHid);
                }
                let c = link.characteristics[link.characteristic];
                push(
                    &mut link.found,
                    Report {
                        service,
                        value: c.value,
                        properties: c.properties,
                        id: data[0],
                        kind,
                        cccd: link.cccd,
                    },
                    CHARACTERISTICS,
                )?;
                if Report::notifying(kind, c.properties) {
                    self.advance(link, Step::Subscribe)
                } else {
                    self.next_report(link, link.characteristic + 1)
                }
            }
            Step::Subscribe => self.next_report(link, link.characteristic + 1),
        }
    }
    fn next_report(&mut self, link: &mut Link, start: usize) -> Result<(), Error> {
        if let Some(index) =
            (start..link.characteristics.len()).find(|&i| link.characteristics[i].uuid == 0x2a4d)
        {
            link.characteristic = index;
            return self.advance(link, Step::Descriptors);
        }
        link.service += 1;
        if link.service < link.services.len() {
            return self.advance(link, Step::Characteristics);
        }
        if !link.found.iter().any(|r| r.kind == ReportType::Input) {
            return Err(Error::UnsupportedHid);
        }
        self.discovered(link)
    }
    /// Admits a newly discovered layout, or finishes verification of a
    /// supplied one by switching to the discovered layout when it differs.
    fn discovered(&mut self, link: &mut Link) -> Result<(), Error> {
        link.characteristics = Vec::new();
        link.services = Vec::new();
        let maps = core::mem::take(&mut link.maps);
        let found = core::mem::take(&mut link.found);
        let verifying = matches!(link.stage, Stage::Ready);
        // Verification has written every CCCD of the device's layout.
        link.cccd_failed = false;
        let hash = link.hash.take();
        if verifying
            && link
                .saved
                .take()
                .is_some_and(|s| s.maps == maps && s.hash == hash)
            && found == link.reports
        {
            return Ok(());
        }
        let mut reports = Vec::new();
        reports
            .try_reserve_exact(found.len())
            .map_err(|_| Error::Capacity)?;
        reports.extend(found.iter().map(Report::layout));
        if verifying {
            let descriptors = compile(&maps)?;
            // Notifications routed before this point precede the event in the
            // queue; every later one is routed by the new table.
            link.reports = found;
            self.events.push_back(Event::Layout {
                link: link.id,
                descriptors,
                layout: Layout {
                    maps,
                    reports,
                    hash,
                },
            });
        } else {
            link.stage = Stage::Ready;
            link.existing_bonds = Vec::new();
            link.reports = found;
            self.events.push_back(Event::Connected {
                link: link.id,
                descriptors: core::mem::take(&mut link.descriptors),
                max_output: link.max_output,
                layout: Some(Layout {
                    maps,
                    reports,
                    hash,
                }),
            });
        }
        Ok(())
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
        if classic || (ble && !self.ble) {
            return Err(Error::UnsupportedTransport);
        }
        if !self.ready {
            return Err(Error::RadioUnavailable);
        }
        self.host.scan(id, ble)?;
        self.scanning = ble;
        Ok(())
    }
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error> {
        if peers != self.wanted {
            self.wanted = peers.to_vec();
        }
        self.apply_reconnect()
    }
    fn set_transport(&mut self, transport: Transport, enabled: bool) -> Result<(), Error> {
        if transport != Transport::Ble {
            return Ok(());
        }
        self.ble = enabled;
        match self.reconcile() {
            // The host's command queue is full; poll retries.
            Err(Error::Busy | Error::Capacity) => Ok(()),
            result => result,
        }
    }
    fn connect(
        &mut self,
        id: LinkId,
        peer: Peer,
        pairing: bool,
        layout: Option<&Layout>,
    ) -> Result<(), Error> {
        if let Some((attempt, _)) = self.incoming.take() {
            self.host.incoming(attempt, None)?;
        }
        self.open(id, peer, pairing, None, layout)
    }
    fn incoming(
        &mut self,
        attempt: u32,
        id: Option<LinkId>,
        layout: Option<&Layout>,
    ) -> Result<(), Error> {
        let Some((pending, peer)) = self.incoming else {
            return Err(Error::NotConnected);
        };
        if pending != attempt {
            return Err(Error::NotConnected);
        }
        self.incoming = None;
        if let Some(id) = id {
            if let Err(error) = self.open(id, peer, false, Some(attempt), layout) {
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
            self.send_disconnect(&mut link);
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
            self.setup(&mut link, Step::Hash).map(|()| {
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
