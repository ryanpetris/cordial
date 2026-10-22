//! Independent, bounded workers for the enumerated gamepad slots.
use crate::{Io, gamepad, xinput};
use cordial_core::{
    outputs::{Enumeration, Plan, SlotSnapshot, Snapshots, Support},
    storage::{self, RecordStore},
};
use cordial_protocol::{OutputMode, Role};
use core::{
    cell::{Cell, RefCell},
    future::pending,
};
use embassy_futures::{
    join::{join, join_array},
    select::{Either, Either3, select, select3},
};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex as CS},
    signal::Signal,
    watch::Watch,
};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::driver::{Driver, Endpoint, EndpointOut};

pub const SUPPORT: &[Support] = &[Support {
    role: Role::Gamepad,
    modes: &[OutputMode::Disabled, OutputMode::Hid, OutputMode::Xinput],
}];

/// Scan all saved pairs using the core's bond validation and first-peer-wins rule.
/// A failed scan exposes no gamepad endpoints until the core can load storage.
pub async fn saved_plan<S: RecordStore>(store: &mut S) -> Plan {
    scan(store).await.unwrap_or_default()
}

async fn scan<S: RecordStore>(store: &mut S) -> Result<Plan, storage::Error> {
    let mut peers = alloc::vec::Vec::new();
    let mut plan = Plan::default();
    let mut after = 0;
    loop {
        let ids = store.record_ids(2, after, 8).await?;
        for &id in &ids {
            after = id;
            let Some(bytes) = store.load_owned(storage::record_key(2, id)).await? else {
                continue;
            };
            let Some((policy, _)) = cordial_core::bonds::decode(id, &bytes) else {
                continue;
            };
            if peers.contains(&policy.peer) {
                continue;
            }
            peers.try_reserve(1).map_err(|_| storage::Error::Full)?;
            peers.push(policy.peer);
            match cordial_core::outputs::gamepad(&policy.output_preferences) {
                OutputMode::Hid => plan.hid_gamepads = (plan.hid_gamepads + 1).min(4),
                OutputMode::Xinput => plan.xinput_gamepads = (plan.xinput_gamepads + 1).min(4),
                OutputMode::Disabled => {}
            }
        }
        if ids.len() < 8 {
            return Ok(plan);
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Frame {
    generation: u64,
    slot: SlotSnapshot,
}

/// Digital history is bounded independently of coalesced axes. No report allocates.
const TRANSITIONS: usize = 16;

#[derive(Clone, Copy, Default, PartialEq)]
struct Digital {
    buttons: u32,
    hat: Option<u8>,
    dpad: u8,
    start: bool,
    select: bool,
}
impl Digital {
    fn from(g: cordial_core::hid::Gamepad) -> Self {
        Self {
            buttons: g.buttons,
            hat: g.hat,
            dpad: g.dpad,
            start: g.start,
            select: g.select,
        }
    }
    fn apply(self, g: &mut cordial_core::hid::Gamepad) {
        g.buttons = self.buttons;
        g.hat = self.hat;
        g.dpad = self.dpad;
        g.start = self.start;
        g.select = self.select;
    }
}

struct History {
    entries: [Digital; TRANSITIONS],
    head: usize,
    len: usize,
    epoch: u64,
}
impl History {
    const fn new() -> Self {
        Self {
            entries: [Digital {
                buttons: 0,
                hat: None,
                dpad: 0,
                start: false,
                select: false,
            }; TRANSITIONS],
            head: 0,
            len: 0,
            epoch: 0,
        }
    }
    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.epoch = self.epoch.wrapping_add(1);
    }
    fn push(&mut self, digital: Digital) {
        if self.len == TRANSITIONS {
            // A stalled host cannot hold up radio or keyboard input. Release all
            // digital controls before restoring the current state on overflow.
            self.clear();
            self.push(Digital::default());
        }
        self.entries[(self.head + self.len) % TRANSITIONS] = digital;
        self.len += 1;
    }
}

struct Slot {
    latest: Watch<CS, Frame, 1>,
    history: Mutex<CS, RefCell<History>>,
    reports: gamepad::Reports,
}
impl Slot {
    const fn new() -> Self {
        Self {
            latest: Watch::new(),
            history: Mutex::new(RefCell::new(History::new())),
            reports: gamepad::Reports::new(),
        }
    }
    fn publish(&self, generation: u64, slot: SlotSnapshot) {
        let frame = Frame { generation, slot };
        let previous = self.latest.try_get();
        if previous != Some(frame) {
            self.history.lock(|history| {
                let mut history = history.borrow_mut();
                let replaced = previous
                    .is_none_or(|p| p.generation != generation || !same_owner(p.slot, slot));
                if replaced {
                    history.clear();
                }
                let digital = Digital::from(slot.gamepad);
                if replaced || previous.is_none_or(|p| Digital::from(p.slot.gamepad) != digital) {
                    history.push(digital);
                }
            });
            self.reports.set(hid_report(slot));
            self.latest.sender().send(frame);
        }
    }
    fn next(&self, mut frame: Frame) -> (Frame, u64, bool) {
        self.history.lock(|history| {
            let history = history.borrow();
            if history.len != 0 {
                history.entries[history.head].apply(&mut frame.slot.gamepad);
            }
            (frame, history.epoch, history.len != 0)
        })
    }
    fn complete(&self, epoch: u64) {
        self.history.lock(|history| {
            let mut history = history.borrow_mut();
            // An in-flight write from before overflow must not consume the recovery release.
            if history.epoch == epoch && history.len != 0 {
                history.head = (history.head + 1) % TRANSITIONS;
                history.len -= 1;
            }
        });
    }
}

pub(crate) struct Outputs {
    enumeration: Mutex<CS, Cell<Enumeration>>,
    hid: [Slot; 4],
    xinput: [Slot; 4],
}
impl Outputs {
    pub const fn new() -> Self {
        Self {
            enumeration: Mutex::new(Cell::new(Enumeration {
                plan: Plan {
                    hid_gamepads: 0,
                    xinput_gamepads: 0,
                },
                generation: 0,
                ready: false,
            })),
            hid: [const { Slot::new() }; 4],
            xinput: [const { Slot::new() }; 4],
        }
    }
    pub fn plan(&self) -> Plan {
        self.enumeration(false).plan
    }
    pub fn enumeration(&self, ready: bool) -> Enumeration {
        Enumeration {
            ready,
            ..self.enumeration.lock(Cell::get)
        }
    }
    pub fn enumerate(&self, plan: Plan) {
        self.enumeration.lock(|state| {
            state.set(Enumeration {
                plan,
                generation: state.get().generation.wrapping_add(1),
                ready: false,
            })
        });
    }
    pub fn publish(&self, generation: u64, snapshots: Snapshots) {
        for (slot, snapshot) in self
            .hid
            .iter()
            .zip(snapshots.hid)
            .chain(self.xinput.iter().zip(snapshots.xinput))
        {
            slot.publish(generation, snapshot);
        }
    }
    pub fn reports(&self, index: usize) -> &gamepad::Reports {
        &self.hid[index].reports
    }
    pub fn reset(&self) {
        for slot in &self.hid {
            slot.reports.reset();
        }
    }
}

fn same_owner(a: SlotSnapshot, b: SlotSnapshot) -> bool {
    (a.occupant, a.generation) == (b.occupant, b.generation)
}
fn hid_report(slot: SlotSnapshot) -> gamepad::Report {
    if slot.occupant.is_none() {
        return gamepad::Report::NEUTRAL;
    }
    let g = slot.gamepad;
    // The hat follows mapped directions. Opposing directions cancel on each axis.
    let horizontal = i8::from(g.dpad & 4 != 0) - i8::from(g.dpad & 8 != 0);
    let vertical = i8::from(g.dpad & 2 != 0) - i8::from(g.dpad & 1 != 0);
    let hat = match (horizontal, vertical) {
        (0, -1) => Some(0),
        (1, -1) => Some(1),
        (1, 0) => Some(2),
        (1, 1) => Some(3),
        (0, 1) => Some(4),
        (-1, 1) => Some(5),
        (-1, 0) => Some(6),
        (-1, -1) => Some(7),
        _ => None,
    };
    gamepad::Report::encode(g.buttons, &g.axes, g.axes_valid, hat)
        .with_controls(g.dpad, g.start, g.select)
}
fn xinput_report(slot: SlotSnapshot) -> xinput::Input {
    let g = slot.gamepad;
    xinput::Input::encode(&g).with_controls(g.dpad, g.start, g.select)
}

async fn idle(deadline: Option<Instant>) {
    match deadline {
        Some(at) => Timer::at(at).await,
        None => pending().await,
    }
}

async fn hid_worker<'d, D: Driver<'d>>(mut hid: gamepad::Gamepad<'d, D>, io: &Io, index: usize) {
    let slot = &io.outputs.hid[index];
    let mut changes = slot.latest.receiver().unwrap();
    let mut status = io.status.receiver().unwrap();
    let mut owner = SlotSnapshot::default();
    let mut generation = None;
    let mut neutral = true;
    let mut sent = None;
    let mut sent_at = Instant::now();
    loop {
        let bus = status.get_and(|s| s.input_ready()).await;
        let frame = match select(
            status.changed_and(|s| s.generation != bus.generation),
            changes.get_and(|f| f.generation == bus.generation),
        )
        .await
        {
            Either::First(_) => continue,
            Either::Second(frame) => frame,
        };
        let (frame, epoch, queued) = slot.next(frame);
        if generation != Some(bus.generation) || !same_owner(owner, frame.slot) {
            neutral = true;
            sent = None;
            generation = Some(bus.generation);
            owner = frame.slot;
        }
        let report = if neutral {
            gamepad::Report::NEUTRAL
        } else {
            hid_report(frame.slot)
        };
        let duration = slot.reports.idle_ms();
        let deadline =
            (duration != u32::MAX).then(|| sent_at + Duration::from_millis(duration.into()));
        if sent != Some(report) || deadline.is_some_and(|at| at <= Instant::now()) {
            // Discard queued controls when ownership changes. CompleteDriver
            // settles any submitted transfer before the following neutral write.
            match select3(
                status.changed_and(|s| s.generation != bus.generation),
                changes.changed_and(|f| !same_owner(f.slot, frame.slot)),
                hid.write(report),
            )
            .await
            {
                Either3::Third(Ok(())) => {
                    if !neutral && queued {
                        slot.complete(epoch);
                    }
                    sent_at = Instant::now();
                    sent = if neutral { None } else { Some(report) };
                    neutral = false;
                }
                Either3::Third(Err(_)) => {
                    status.changed().await;
                }
                _ => {}
            }
            continue;
        }
        if queued {
            slot.complete(epoch);
            continue;
        }
        select3(
            status.changed(),
            changes.changed(),
            select(slot.reports.changed(), idle(deadline)),
        )
        .await;
    }
}

#[derive(Default)]
struct PendingCommands {
    frame: Option<Frame>,
    status: bool,
    acknowledgements: u8,
}
struct Commands {
    pending: Mutex<CS, RefCell<PendingCommands>>,
    changed: Signal<CS, ()>,
}
impl Commands {
    fn new() -> Self {
        Self {
            pending: Mutex::new(RefCell::new(PendingCommands::default())),
            changed: Signal::new(),
        }
    }
    fn receive(&self, frame: Frame, bytes: &[u8]) {
        let status = matches!(bytes, [8, 0, 0x0f, 0xc0, ..]);
        let ack = matches!(bytes, [0, 0, 0, 0x40, ..]);
        if !status && !ack {
            return;
        }
        self.pending.lock(|pending| {
            let mut pending = pending.borrow_mut();
            if pending
                .frame
                .is_none_or(|p| p.generation != frame.generation || !same_owner(p.slot, frame.slot))
            {
                *pending = PendingCommands {
                    frame: Some(frame),
                    ..Default::default()
                };
            }
            if status {
                pending.status = true;
                pending.acknowledgements = 0;
            } else {
                // The receiver exchange has two ACK phases. Repeated ACKs do not
                // need unbounded storage while the host leaves IN unread.
                pending.acknowledgements = (pending.acknowledgements + 1).min(2);
            }
        });
        self.changed.signal(());
    }
    fn take(&self, frame: Frame, ack_ready: bool) -> Option<[u8; 12]> {
        self.pending.lock(|pending| {
            let mut pending = pending.borrow_mut();
            if pending
                .frame
                .is_none_or(|p| p.generation != frame.generation || !same_owner(p.slot, frame.slot))
            {
                *pending = PendingCommands::default();
                return None;
            }
            let mut bytes = [0; 12];
            if pending.status {
                pending.status = false;
                bytes[..4].copy_from_slice(&[8, 0, 0x0f, 0xc0]);
            } else if ack_ready && pending.acknowledgements != 0 {
                pending.acknowledgements -= 1;
                bytes[3] = 0x40;
            } else {
                return None;
            }
            Some(bytes)
        })
    }
}

async fn xinput_worker<'d, D: Driver<'d>>(channel: xinput::Channel<'d, D>, io: &Io, index: usize) {
    let xinput::Channel {
        mut input,
        mut output,
        ..
    } = channel;
    let slot = &io.outputs.xinput[index];
    let commands = Commands::new();
    let receive = async {
        let mut status = io.status.receiver().unwrap();
        loop {
            let bus = status.get_and(|s| s.input_ready()).await;
            let mut bytes = [0; 32];
            match select(
                status.changed_and(|s| s.generation != bus.generation),
                output.read(&mut bytes),
            )
            .await
            {
                Either::Second(Ok(12)) => {
                    // Commands are coalesced without waiting for the IN endpoint.
                    // LED and rumble are consumed without reaching Bluetooth.
                    if let Some(frame) = slot
                        .latest
                        .try_get()
                        .filter(|f| f.generation == bus.generation)
                    {
                        commands.receive(frame, &bytes[..12]);
                    }
                }
                Either::Second(Err(_)) => {
                    select(status.changed(), output.wait_enabled()).await;
                }
                _ => {}
            }
        }
    };
    let transmit = async {
        let mut status = io.status.receiver().unwrap();
        let mut changes = slot.latest.receiver().unwrap();
        let mut protocol = xinput::Protocol::new([0, 0, 0, 0, 0, index as u8 + 1]);
        let mut owner = SlotSnapshot::default();
        let mut generation = None;
        let mut current = None;
        let mut ack_ready = false;
        let mut deadline = None;
        loop {
            let bus = status.get_and(|s| s.input_ready()).await;
            let frame = match select(
                status.changed_and(|s| s.generation != bus.generation),
                changes.get_and(|f| f.generation == bus.generation),
            )
            .await
            {
                Either::First(_) => continue,
                Either::Second(frame) => frame,
            };
            let (frame, epoch, queued) = slot.next(frame);
            if generation != Some(bus.generation) {
                protocol.reset();
                ack_ready = false;
                generation = Some(bus.generation);
                current = None;
                deadline = None;
            }
            let replacing = !same_owner(owner, frame.slot) && owner.occupant.is_some();
            if replacing {
                protocol.assign(false);
                ack_ready = false;
            } else if !same_owner(owner, frame.slot) {
                owner = frame.slot;
                protocol.assign(owner.occupant.is_some());
                ack_ready = false;
                current = None;
            }
            if !replacing {
                let report = xinput_report(frame.slot);
                if current != Some(report) {
                    protocol.update(report);
                    current = Some(report);
                    deadline = None;
                }
                if let Some(command) = commands.take(frame, ack_ready) {
                    protocol.receive(&command);
                    ack_ready = false;
                }
            }
            if protocol.pending() {
                match select3(
                    status.changed_and(|s| s.generation != bus.generation),
                    changes.changed_and(|f| !same_owner(f.slot, frame.slot)),
                    protocol.write(&mut input),
                )
                .await
                {
                    Either3::Third(Ok(sent)) => {
                        if replacing {
                            owner = SlotSnapshot::default();
                            current = None;
                        }
                        if matches!(
                            sent,
                            Some(xinput::Sent::Presence | xinput::Sent::Information)
                        ) {
                            ack_ready = !replacing && owner.occupant.is_some();
                        }
                        if sent == Some(xinput::Sent::Input) {
                            if queued {
                                slot.complete(epoch);
                            }
                            deadline = Some(Instant::now() + Duration::from_millis(11));
                        }
                    }
                    Either3::Third(Err(_)) => {
                        status.changed().await;
                    }
                    _ => {}
                }
                continue;
            }
            if queued {
                slot.complete(epoch);
                continue;
            }
            if let Either3::Third(Either::Second(())) = select3(
                status.changed(),
                changes.changed(),
                select(commands.changed.wait(), idle(deadline)),
            )
            .await
            {
                protocol.idle();
                deadline = None;
            }
        }
    };
    join(receive, transmit).await;
}

pub(crate) async fn run<'d, D: Driver<'d>>(
    hid: [Option<gamepad::Gamepad<'d, D>>; 4],
    xinput: [Option<xinput::Channel<'d, D>>; 4],
    io: &Io,
) {
    let mut hi = 0;
    let mut xi = 0;
    join(
        join_array(hid.map(|pad| {
            let index = hi;
            hi += 1;
            async move {
                if let Some(pad) = pad {
                    hid_worker(pad, io, index).await;
                }
            }
        })),
        join_array(xinput.map(|pad| {
            let index = xi;
            xi += 1;
            async move {
                if let Some(pad) = pad {
                    xinput_worker(pad, io, index).await;
                }
            }
        })),
    )
    .await;
    pending::<()>().await;
}

#[cfg(test)]
mod mapping_tests {
    use super::*;
    use cordial_core::hid::Gamepad;

    #[test]
    fn digital_history_keeps_mapped_axes_through_release_and_reassignment() {
        for neutral in [
            Gamepad::default(),
            Gamepad {
                axes: [32768, 32768, 0, 65535, 0, 0, 65535, 0],
                axes_valid: 0xdb,
                ..Default::default()
            },
        ] {
            let slot = Slot::new();
            let owner = cordial_core::link::LinkId {
                slot: 0,
                generation: 1,
            };
            for buttons in [0, 1 << 4, 0] {
                slot.publish(
                    1,
                    SlotSnapshot {
                        occupant: Some(owner),
                        generation: 1,
                        gamepad: cordial_core::hid::Gamepad { buttons, ..neutral },
                    },
                );
            }
            let frame = slot.latest.try_get().unwrap();
            for buttons in [0, 1 << 4, 0] {
                let (next, epoch, pending) = slot.next(frame);
                assert!(pending);
                assert_eq!(next.slot.gamepad, Gamepad { buttons, ..neutral });
                let expected =
                    xinput::Input::encode(&cordial_core::hid::Gamepad { buttons, ..neutral });
                assert_eq!(xinput_report(next.slot), expected);
                slot.complete(epoch);
            }
            slot.publish(1, SlotSnapshot::default());
            let (released, _, _) = slot.next(slot.latest.try_get().unwrap());
            assert_eq!(released.slot.gamepad, Default::default());
        }
    }
}
