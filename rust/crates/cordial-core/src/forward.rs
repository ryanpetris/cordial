use crate::hid::{CONSUMER, Error, Held, Input, KEYBOARD, MOTION_LIMIT, MOUSE, QUEUE, SOURCES};

pub const REPORT_KEYBOARD: u8 = 1;
pub const REPORT_MOUSE: u8 = 2;
pub const REPORT_CONSUMER: u8 = 3;

#[derive(Clone, Copy, Debug, Default)]
struct Queued {
    held: Held,
    motion: [i32; 4],
    source: u8,
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub id: u8,
    bytes: [u8; 32],
    length: usize,
    motion: [i16; 4],
    source: Option<usize>,
}
impl Packet {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

/// Queues source transitions without allocating in the input or USB paths.
pub struct Forwarder {
    received: [Held; SOURCES],
    applied: [Held; SOURCES],
    sent: Held,
    queue: [Queued; QUEUE],
    head: usize,
    count: usize,
    active: Option<Queued>,
    packet: Option<Packet>,
    enabled: bool,
    reconcile: bool,
    dirty: u8,
}
impl Default for Forwarder {
    fn default() -> Self {
        Self {
            received: [Held::default(); SOURCES],
            applied: [Held::default(); SOURCES],
            sent: Held::default(),
            queue: [Queued::default(); QUEUE],
            head: 0,
            count: 0,
            active: None,
            packet: None,
            enabled: true,
            reconcile: true,
            dirty: KEYBOARD | CONSUMER | MOUSE,
        }
    }
}
fn combined(sources: &[Held; SOURCES]) -> Result<Held, Error> {
    sources
        .iter()
        .try_fold(Held::default(), |all, held| all.union(held))
}
impl Forwarder {
    pub fn input(&mut self, source: usize, input: Input) -> Result<(), Error> {
        if source >= SOURCES {
            return Err(Error::Invalid);
        }
        let mut next = self.received;
        next[source] = input.held;
        combined(&next)?;
        if !self.enabled {
            self.received = next;
            return Ok(());
        }
        if input
            .motion
            .iter()
            .any(|v| !(-MOTION_LIMIT..=MOTION_LIMIT).contains(v))
        {
            return Err(Error::Overflow);
        }
        if self.received[source] == input.held && input.motion == [0; 4] {
            return Ok(());
        }
        if self.count != 0 {
            let last = &mut self.queue[(self.head + self.count - 1) % QUEUE];
            if last.source as usize == source && last.held == input.held {
                let mut motion = [0; 4];
                for (i, v) in motion.iter_mut().enumerate() {
                    *v = i64::from(last.motion[i]) + input.motion[i];
                }
                if motion
                    .iter()
                    .all(|v| (-MOTION_LIMIT..=MOTION_LIMIT).contains(v))
                {
                    last.motion = motion.map(|v| v as i32);
                    self.received = next;
                    return Ok(());
                }
            }
        }
        if self.count == QUEUE {
            return Err(Error::Overflow);
        }
        self.queue[(self.head + self.count) % QUEUE] = Queued {
            held: input.held,
            motion: input.motion.map(|v| v as i32),
            source: source as u8,
        };
        self.count += 1;
        self.received = next;
        Ok(())
    }
    pub fn remove(&mut self, source: usize) {
        if source >= SOURCES {
            return;
        }
        self.received[source] = Held::default();
        self.applied[source] = Held::default();
        let mut keep = 0;
        for i in 0..self.count {
            let item = self.queue[(self.head + i) % QUEUE];
            if item.source as usize != source {
                self.queue[(self.head + keep) % QUEUE] = item;
                keep += 1;
            }
        }
        self.count = keep;
        if self.active.is_some_and(|q| q.source as usize == source) {
            self.active = None;
        }
        // An already submitted packet must finish before its release is sent.
        self.reconcile = true;
    }
    /// Call when the bus invalidates pending transfers, or forwarding is suspended.
    pub fn resync(&mut self) {
        self.head = 0;
        self.count = 0;
        self.active = None;
        self.packet = None;
        self.applied = self.received;
        self.dirty = KEYBOARD | MOUSE | CONSUMER;
        self.reconcile = true;
    }
    pub fn enable(&mut self, enabled: bool) {
        if enabled != self.enabled {
            self.enabled = enabled;
            self.resync();
        }
    }
    fn prepare(&mut self, motion: [i32; 4], source: Option<usize>) -> bool {
        let held = combined(&self.applied).expect("queued input exceeded consumer capacity");
        let mut packet = Packet {
            id: 0,
            bytes: [0; 32],
            length: 0,
            motion: [0; 4],
            source,
        };
        let dirty = if self.dirty & KEYBOARD != 0 || held.keys != self.sent.keys {
            packet.id = REPORT_KEYBOARD;
            packet.bytes = held.keys;
            packet.length = 32;
            KEYBOARD
        } else if self.dirty & CONSUMER != 0 || held.consumers != self.sent.consumers {
            packet.id = REPORT_CONSUMER;
            packet.length = 16;
            for (i, value) in held.consumers.iter().enumerate() {
                packet.bytes[2 * i..2 * i + 2].copy_from_slice(&value.to_le_bytes());
            }
            CONSUMER
        } else if self.dirty & MOUSE != 0 || held.buttons != self.sent.buttons || motion != [0; 4] {
            packet.id = REPORT_MOUSE;
            packet.length = 10;
            packet.bytes[..2].copy_from_slice(&held.buttons.to_le_bytes());
            for (i, value) in motion.iter().enumerate() {
                let value = (*value).clamp(i16::MIN.into(), i16::MAX.into()) as i16;
                packet.motion[i] = value;
                packet.bytes[2 + 2 * i..4 + 2 * i].copy_from_slice(&value.to_le_bytes());
            }
            MOUSE
        } else {
            return false;
        };
        self.dirty &= !dirty;
        self.packet = Some(packet);
        true
    }
    /// Repeated calls return the same bytes until USB confirms completion.
    pub fn packet(&mut self) -> Option<&Packet> {
        if !self.enabled {
            return None;
        }
        if self.packet.is_some() {
            return self.packet.as_ref();
        }
        if self.reconcile {
            if self.prepare([0; 4], None) {
                return self.packet.as_ref();
            }
            self.reconcile = false;
        }
        loop {
            if self.active.is_none() && self.count > 0 {
                let item = self.queue[self.head];
                self.head = (self.head + 1) % QUEUE;
                self.count -= 1;
                self.applied[item.source as usize] = item.held;
                self.active = Some(item);
            }
            let active = self.active?;
            if self.prepare(active.motion, Some(active.source as usize)) {
                return self.packet.as_ref();
            }
            self.active = None;
        }
    }
    /// Complete only the packet returned by `packet`; a failed write leaves it pending.
    pub fn complete(&mut self) {
        let Some(packet) = self.packet.take() else {
            return;
        };
        match packet.id {
            REPORT_KEYBOARD => self.sent.keys = packet.bytes,
            REPORT_CONSUMER => {
                for (i, value) in self.sent.consumers.iter_mut().enumerate() {
                    *value = u16::from_le_bytes([packet.bytes[2 * i], packet.bytes[2 * i + 1]]);
                }
            }
            REPORT_MOUSE => {
                self.sent.buttons = u16::from_le_bytes([packet.bytes[0], packet.bytes[1]]);
                if let Some(active) = &mut self.active
                    && packet.source == Some(active.source as usize)
                {
                    for (remaining, sent) in active.motion.iter_mut().zip(packet.motion) {
                        *remaining -= i32::from(sent);
                    }
                }
            }
            _ => unreachable!(),
        }
    }
    pub fn pending(&self) -> usize {
        if !self.enabled {
            return 0;
        }
        self.count
            + usize::from(self.active.is_some())
            + usize::from(self.packet.is_some())
            + usize::from(self.reconcile)
    }
}
