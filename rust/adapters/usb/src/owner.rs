//! The application's two loops. They share the application, its storage and the Bluetooth
//! backend through one lock on one executor, so only one of them touches them at a time.
//!
//! The priority loop does what key presses and mouse movement need right now: it polls the radio
//! and handles every radio event, including connection setup and the profile loads on connect,
//! runs links' timers and output to devices, follows the USB status and hands HID input reports
//! to USB.
//!
//! The secondary loop does everything that can wait: the serial session, its requests, events
//! and output, configuration editor packets, background storage work and deciding when USB
//! enumerates again. Before each step it waits for a pass of the priority loop that left nothing
//! waiting, or, under continuous input, for a pass `SECONDARY_WAIT_MS` after its last step ended.
//! Most steps read or write about one record. A step that needs everything waiting on flash
//! first writes all of it: before USB enumerates again, before the bootloader, when a
//! configuration editor's profile is released and before free space is counted to admit new
//! data. Input waits for one step at a time. A step that wrote flash is followed by a rest at least
//! as long as the step took, so input keeps at least half the time while writes continue.
use crate::{HidTx, Io, RawPacket, SerialRx, SerialTx, Status};
use cordial_core::{
    application::Application, bluetooth::EventSource, interfaces, storage::RecordStore,
};
use cordial_protocol::{Request, frame::DELIMITER};
use core::cell::Cell;
use embassy_futures::select::select;
use embassy_sync::{
    blocking_mutex::{self, raw::NoopRawMutex},
    mutex::{Mutex, MutexGuard},
    signal::Signal,
};

/// How long a report USB has not yet taken holds up the secondary loop. A host polls the input
/// endpoint every millisecond; one that has not taken a report for this long is not reading it
/// promptly, and the serial port stays usable meanwhile.
const HID_WAIT_MS: u64 = 8;

/// How long after its last step ended the secondary loop takes another while every priority pass
/// leaves input waiting. Continuous input then still lets serial requests, configuration editors
/// and background work make progress, one step at a time, and gets at least this long between
/// such steps.
const SECONDARY_WAIT_MS: u64 = 5;

/// What both loops use.
pub struct Shared<S, B> {
    pub app: Application,
    pub store: S,
    pub radio: B,
}

pub struct Owner<'a, S, B> {
    io: &'a Io,
    shared: Mutex<NoopRawMutex, Shared<S, B>>,
    /// Signalled after each priority pass that left nothing waiting, and after each pass
    /// `SECONDARY_WAIT_MS` after the secondary loop's last step ended.
    turn: Signal<NoopRawMutex, ()>,
    /// When the secondary loop's last step ended.
    stepped: blocking_mutex::Mutex<NoopRawMutex, Cell<u64>>,
    /// When the secondary loop may take its next step: after a step that wrote flash, as long
    /// after it ended as it took.
    rested: blocking_mutex::Mutex<NoopRawMutex, Cell<u64>>,
}
impl<'a, S: RecordStore, B: EventSource> Owner<'a, S, B> {
    pub fn new(io: &'a Io, app: Application, store: S, radio: B) -> Self {
        Self {
            io,
            shared: Mutex::new(Shared { app, store, radio }),
            turn: Signal::new(),
            stepped: blocking_mutex::Mutex::new(Cell::new(0)),
            rested: blocking_mutex::Mutex::new(Cell::new(0)),
        }
    }
    /// The shared state, once neither loop is using it.
    pub async fn lock(&self) -> MutexGuard<'_, NoopRawMutex, Shared<S, B>> {
        self.shared.lock().await
    }
    /// Runs the priority loop. `pass` runs at the start of each pass, with the shared state, for
    /// platform start-up and indicators. Between passes the loop waits for a USB change or for
    /// `wake`, which returns on radio activity and at least every millisecond, since protocol and
    /// application deadlines have millisecond resolution.
    pub async fn priority(
        &self,
        mut pass: impl AsyncFnMut(&mut Shared<S, B>),
        wake: impl AsyncFn(),
        now: fn() -> u64,
    ) -> ! {
        let mut priority = Priority::new(self.io);
        loop {
            let (idle, at) = {
                let mut shared = self.shared.lock().await;
                pass(&mut shared).await;
                let at = now();
                (priority.pass(&mut shared, at).await, at)
            };
            if at >= self.rested.lock(Cell::get)
                && (idle || at.saturating_sub(self.stepped.lock(Cell::get)) >= SECONDARY_WAIT_MS)
            {
                self.turn.signal(());
            }
            select(self.io.changed(), wake()).await;
        }
    }
    /// Runs the secondary loop. `interfaces` is the configuration interface set USB started with.
    pub async fn secondary(&self, interfaces: u8, now: fn() -> u64) -> ! {
        let mut secondary = Secondary::new(self.io, interfaces);
        loop {
            self.turn.wait().await;
            let mut shared = self.shared.lock().await;
            let start = now();
            let generation = shared.store.generation().await;
            secondary.step(&mut shared, start).await;
            let end = now();
            self.stepped.lock(|stepped| stepped.set(end));
            // The store's generation changes with every write.
            if shared.store.generation().await != generation {
                let rest = end.saturating_add(end.saturating_sub(start));
                self.rested.lock(|rested| rested.set(rest));
            }
        }
    }
}

/// The priority loop's own state.
pub struct Priority<'a> {
    io: &'a Io,
    /// The USB status the loop last followed.
    seen: Status,
    /// The report USB is writing.
    hid_pending: Option<(u64, u64)>,
    hid_sequence: u64,
    /// Since when USB has had a report it has not yet taken. A failed write is retried, and the
    /// wait goes on.
    hid_since: Option<u64>,
}
impl<'a> Priority<'a> {
    pub fn new(io: &'a Io) -> Self {
        Self {
            io,
            seen: Status::default(),
            hid_pending: None,
            hid_sequence: 0,
            hid_since: None,
        }
    }
    /// One pass: polls the radio, handles every radio event, runs the application's link work and
    /// hands the next input report to USB. Returns whether nothing is left waiting for input: no
    /// report the host is reading promptly is waiting to be sent.
    pub async fn pass<S: RecordStore, B: EventSource>(
        &mut self,
        shared: &mut Shared<S, B>,
        now: u64,
    ) -> bool {
        let Shared { app, store, radio } = shared;
        EventSource::poll(radio).await;
        while let Some(event) = radio.next_event() {
            app.event(event, store, radio, now).await;
        }
        let status = self.io.status();
        if status.generation != self.seen.generation {
            self.hid_pending = None;
            self.hid_since = None;
            app.manager.forward.resync();
        }
        app.manager.forward.enable(status.input_ready());
        self.seen = status;
        while let Ok(done) = self.io.hid_done.try_receive() {
            if self.hid_pending == Some((done.generation, done.sequence)) {
                self.hid_pending = None;
                if done.success {
                    self.hid_since = None;
                    app.manager.forward.complete();
                }
            }
        }
        app.operate(store, radio, status.leds, now).await;
        if status.input_ready()
            && self.hid_pending.is_none()
            && !self.io.hid_tx.is_full()
            && let Some(packet) = app.manager.forward.packet()
        {
            self.hid_sequence = self.hid_sequence.wrapping_add(1);
            let mut tx = HidTx {
                generation: status.generation,
                sequence: self.hid_sequence,
                bytes: [0; 69],
                length: 1 + packet.bytes().len(),
            };
            tx.bytes[0] = packet.id;
            tx.bytes[1..tx.length].copy_from_slice(packet.bytes());
            if self.io.hid_tx.try_send(tx).is_ok() {
                self.hid_pending = Some((status.generation, self.hid_sequence));
                self.hid_since.get_or_insert(now);
            }
        }
        app.manager.forward.pending() == 0
            || self
                .hid_since
                .is_some_and(|since| now.saturating_sub(since) >= HID_WAIT_MS)
    }
}

/// The secondary loop's own state.
pub struct Secondary<'a> {
    io: &'a Io,
    /// The USB status whose serial session the application follows.
    seen: Status,
    serial_pending: bool,
    separator: bool,
    input: Option<SerialRx>,
    offset: usize,
    /// The configuration interface set last requested from USB.
    interfaces: u8,
}
impl<'a> Secondary<'a> {
    /// `interfaces` is the configuration interface set USB started with.
    pub fn new(io: &'a Io, interfaces: u8) -> Self {
        Self {
            io,
            seen: Status::default(),
            serial_pending: false,
            separator: false,
            input: None,
            offset: 0,
            interfaces,
        }
    }
    /// Follows the serial port opening and closing. A command in progress belongs to the session
    /// that sent it: it finishes first, and its response is then discarded with the rest of that
    /// session's output and input.
    fn sync<S, B: EventSource>(&mut self, shared: &mut Shared<S, B>) {
        let status = self.io.status();
        if status.session == self.seen.session || shared.app.command_pending() {
            return;
        }
        shared.app.session(status.serial_open(), &mut shared.radio);
        self.input = None;
        self.offset = 0;
        self.separator = status.serial_open();
        self.serial_pending = false;
        self.seen = status;
    }
    /// One step: asks USB to enumerate again when it should, saving everything waiting first, or
    /// else continues the command in progress, or writes a deferred save that is due, or reads and
    /// starts the next request, or answers one configuration editor packet, or does the
    /// application's other work. Then it hands serial output to USB.
    pub async fn step<S: RecordStore, B: EventSource>(
        &mut self,
        shared: &mut Shared<S, B>,
        now: u64,
    ) {
        self.sync(shared);
        let Shared { app, store, radio } = shared;
        let session = self.seen.session;
        while let Ok(done) = self.io.serial_done.try_receive() {
            if done.session == session {
                self.serial_pending = false;
                if let Some(token) = done.token {
                    app.serial.output_complete(token, done.length);
                } else if done.length != 0 {
                    self.separator = false;
                }
            }
        }
        // Enumerating again goes before anything that could queue more output.
        if self.reconnect_due(app) {
            app.save_all(store, now).await;
            self.reconnect(app);
            return;
        }
        if app.command_pending() {
            app.proceed(store, radio, now).await;
        } else if app.save(store, now).await {
            // The step wrote a deferred save.
        } else if let Some(request) = self.read(app) {
            app.begin(request, store, radio, now).await;
        } else if !self.edit(app, store, now).await {
            app.work(store, radio, now).await;
        }
        self.output(app);
    }
    /// Reads from at most one USB packet per step until a request is complete.
    fn read(&mut self, app: &mut Application) -> Option<Request> {
        if self.input.is_none() {
            self.input = self.io.serial_rx.try_receive().ok();
        }
        if let Some(packet) = &self.input
            && (packet.session != self.seen.session || !app.serial.active())
        {
            self.input = None;
            self.offset = 0;
        }
        let packet = self.input.as_ref()?;
        let (read, request) = app.serial.feed(&packet.bytes[self.offset..packet.length]);
        self.offset += read;
        if self.offset == packet.length {
            self.input = None;
            self.offset = 0;
        }
        request
    }
    /// The configuration interfaces the saved preference enables.
    fn wanted(app: &Application) -> u8 {
        if app.manager.profiles_supported() {
            interfaces::enabled(&app.manager.preference.configuration_interfaces)
        } else {
            0
        }
    }
    /// Answers one configuration editor packet. Returns whether there was one.
    async fn edit<S: RecordStore>(
        &mut self,
        app: &mut Application,
        store: &mut S,
        now: u64,
    ) -> bool {
        let status = self.io.status();
        for (slot, queue) in self.io.raw.iter().enumerate() {
            if queue.tx.is_full() {
                continue;
            }
            let Ok(packet) = queue.rx.try_receive() else {
                continue;
            };
            let bytes = if packet.generation == status.generation
                && !app.usb_reconnect
                && Self::wanted(app) == self.interfaces
                && self.io.interfaces() == self.interfaces
                && self.io.slot(slot) == Some(packet.interface)
            {
                app.configure(packet.interface, packet.bytes, store, now)
                    .await
            } else {
                let mut bytes = packet.bytes;
                bytes[0] = 0xff;
                bytes
            };
            let _ = queue.tx.try_send(RawPacket {
                generation: packet.generation,
                interface: packet.interface,
                bytes,
            });
            return true;
        }
        false
    }
    /// Saved interfaces are known once storage is ready. USB enumerates again only when they
    /// differ from the exposed set or the application requests it, once no command is in progress
    /// and its output has been sent.
    fn reconnect_due(&self, app: &Application) -> bool {
        app.manager.storage_ready
            && (Self::wanted(app) != self.interfaces || app.usb_reconnect)
            && !app.command_pending()
            && app.serial.queued() == 0
            && !self.serial_pending
    }
    /// Asks USB to enumerate again with the interfaces the saved preference enables.
    fn reconnect(&mut self, app: &mut Application) {
        let wanted = Self::wanted(app);
        self.interfaces = wanted;
        app.usb_reconnect = false;
        self.io.reset(false);
        self.io.reconnect.signal(wanted);
    }
    /// Hands the next chunk of serial output to USB, while the application follows the session
    /// USB has open.
    fn output(&mut self, app: &mut Application) {
        let status = self.io.status();
        if !status.serial_open()
            || status.session != self.seen.session
            || self.serial_pending
            || self.io.serial_tx.is_full()
        {
            return;
        }
        let mut tx = SerialTx {
            session: status.session,
            token: None,
            bytes: [0; 63],
            length: 0,
        };
        if self.separator {
            tx.bytes[0] = DELIMITER;
            tx.length = 1;
        } else if let Some((token, bytes)) = app.serial.output(63) {
            tx.token = Some(token);
            tx.length = bytes.len();
            tx.bytes[..tx.length].copy_from_slice(bytes);
        }
        if tx.length != 0 {
            // Single producer, capacity checked above; no await in between.
            self.io.serial_tx.try_send(tx).ok().unwrap();
            self.serial_pending = true;
        }
    }
}
