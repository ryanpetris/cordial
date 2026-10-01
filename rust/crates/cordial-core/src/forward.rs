use crate::hid::{
    CONSUMER, CONSUMER_AXES, CONSUMER_SWITCHES, Error, Held, Input, KEYBOARD, MOTION_LIMIT, MOUSE,
    QUEUE, SOURCES,
};

pub const REPORT_KEYBOARD: u8 = 1;
pub const REPORT_MOUSE: u8 = 2;
pub const REPORT_CONSUMER: u8 = 3;
pub const REPORT_CONSUMER_MOTION: u8 = 4;
pub const REPORT_CONSUMER_TOGGLE: u8 = 5;
pub const REPORT_CONSUMER_ON_OFF: u8 = 6;
pub const REPORT_POINTER_POSITION: u8 = 7;
pub const REPORT_CONSUMER_VALUES: u8 = 8;

// A 512-byte report contains at most 128 32-bit values. Their sum fits 40 bits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ConsumerDelta([u8; 5]);
impl ConsumerDelta {
    fn new(value: i64) -> Self {
        Self(value.to_le_bytes()[..5].try_into().unwrap())
    }
    fn value(self) -> i64 {
        let mut bytes = [if self.0[4] & 0x80 != 0 { 0xff } else { 0 }; 8];
        bytes[..5].copy_from_slice(&self.0);
        i64::from_le_bytes(bytes)
    }
}
#[derive(Clone, Copy, Debug)]
struct Queued {
    held: Held,
    motion: [i32; 4],
    consumer_motion: [ConsumerDelta; CONSUMER_AXES.len()],
    pulses: Held,
    pulse_break: bool,
    // Each pair stores neutral 0, explicit On 1, toggle 2, or explicit Off 3.
    consumer_switches: [u8; CONSUMER_SWITCHES.len().div_ceil(4)],
    position: [u16; 3],
    consumer_values: [u16; CONSUMER_AXES.len()],
    source: u8,
}

impl Queued {
    fn switches(self) -> [i8; CONSUMER_SWITCHES.len()] {
        core::array::from_fn(
            |i| match (self.consumer_switches[i / 4] >> (2 * (i % 4))) & 3 {
                3 => -1,
                1 | 2 => 1,
                _ => 0,
            },
        )
    }
    fn explicit(self, i: usize) -> bool {
        self.consumer_switches[i / 4] & (1 << (2 * (i % 4))) != 0
    }
}

impl Default for Queued {
    fn default() -> Self {
        Self {
            held: Held::default(),
            motion: [0; 4],
            consumer_motion: [ConsumerDelta::default(); CONSUMER_AXES.len()],
            pulses: Held::default(),
            pulse_break: false,
            consumer_switches: [0; CONSUMER_SWITCHES.len().div_ceil(4)],
            position: [u16::MAX; 3],
            consumer_values: [u16::MAX; CONSUMER_AXES.len()],
            source: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub id: u8,
    bytes: [u8; 68],
    length: usize,
    motion: [i16; 4],
    consumer_motion: [i32; CONSUMER_AXES.len()],
    switch_reset: bool,
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
    switch_reset: u8,
    received_position: [Option<(u16, u8)>; 3],
    received_values: [Option<(u16, u8)>; CONSUMER_AXES.len()],
    reconcile_position: [Option<u16>; 3],
    reconcile_values: [Option<u16>; CONSUMER_AXES.len()],
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
            switch_reset: 0,
            received_position: [None; 3],
            received_values: [None; CONSUMER_AXES.len()],
            reconcile_position: [None; 3],
            reconcile_values: [None; CONSUMER_AXES.len()],
        }
    }
}
fn combined(sources: &[Held; SOURCES]) -> Result<Held, Error> {
    sources
        .iter()
        .try_fold(Held::default(), |all, held| all.union(held))
}
impl Forwarder {
    fn record_values(&mut self, source: usize, input: &Input) {
        for (state, value) in self.received_position.iter_mut().zip(input.position) {
            if let Some(value) = value {
                *state = Some((value, source as u8));
            }
        }
        for (state, value) in self.received_values.iter_mut().zip(input.consumer_values) {
            if let Some(value) = value {
                *state = Some((value, source as u8));
            }
        }
    }
    pub fn input(&mut self, source: usize, input: Input) -> Result<(), Error> {
        if source >= SOURCES {
            return Err(Error::Invalid);
        }
        if input
            .position
            .iter()
            .chain(input.consumer_values.iter())
            .any(|&v| v == Some(u16::MAX))
        {
            return Err(Error::Invalid);
        }
        if input
            .consumer_switches
            .iter()
            .any(|&v| !(-1..=1).contains(&v))
        {
            return Err(Error::Invalid);
        }
        if input
            .consumer_motion
            .iter()
            .any(|v| !(-(1i64 << 39)..(1i64 << 39)).contains(v))
        {
            return Err(Error::Overflow);
        }
        let repetitions = usize::from(input.pulse_repetition_count);
        if repetitions >= QUEUE
            || self.enabled && repetitions > 0 && self.count + repetitions + 1 > QUEUE
        {
            return Err(Error::Overflow);
        }
        if repetitions > 0 {
            let mut first = input;
            first.pulse_repetition_count = 0;
            self.input(source, first)?;
            if self.enabled {
                for &usage in &input.pulse_repetitions[..repetitions] {
                    let mut repeat = Input {
                        held: input.held,
                        ..Input::default()
                    };
                    repeat.pulse(usage)?;
                    self.input(source, repeat)?;
                }
            }
            return Ok(());
        }
        let mut next = self.received;
        next[source] = input.held;
        combined(&next)?.union(&input.pulses)?;
        if !self.enabled {
            self.received = next;
            self.record_values(source, &input);
            return Ok(());
        }
        if input
            .motion
            .iter()
            .any(|v| !(-MOTION_LIMIT..=MOTION_LIMIT).contains(v))
        {
            return Err(Error::Overflow);
        }
        if self.received[source] == input.held
            && input.motion == [0; 4]
            && input.consumer_motion == [0; CONSUMER_AXES.len()]
            && input.pulses == Held::default()
            && input.consumer_switches == [0; CONSUMER_SWITCHES.len()]
            && input.position.iter().enumerate().all(|(i, v)| {
                v.is_none_or(|v| self.received_position[i] == Some((v, source as u8)))
            })
            && input
                .consumer_values
                .iter()
                .enumerate()
                .all(|(i, v)| v.is_none_or(|v| self.received_values[i] == Some((v, source as u8))))
        {
            return Ok(());
        }
        if self.count != 0 {
            let last = &mut self.queue[(self.head + self.count - 1) % QUEUE];
            // Numeric consumer adjustments retain order at host range boundaries.
            if last.source as usize == source
                && last.held == input.held
                && last.consumer_motion == [ConsumerDelta::default(); CONSUMER_AXES.len()]
                && input.consumer_motion == [0; CONSUMER_AXES.len()]
                && last.pulses == Held::default()
                && input.pulses == Held::default()
                && last.consumer_switches == [0; CONSUMER_SWITCHES.len().div_ceil(4)]
                && input.consumer_switches == [0; CONSUMER_SWITCHES.len()]
            {
                let mut motion = [0; 4];
                for (i, v) in motion.iter_mut().enumerate() {
                    *v = i64::from(last.motion[i]) + input.motion[i];
                }
                if motion
                    .iter()
                    .all(|v| (-MOTION_LIMIT..=MOTION_LIMIT).contains(v))
                {
                    last.motion = motion.map(|v| v as i32);
                    for (last, value) in last.position.iter_mut().zip(input.position) {
                        if let Some(value) = value {
                            *last = value;
                        }
                    }
                    for (last, value) in last.consumer_values.iter_mut().zip(input.consumer_values)
                    {
                        if let Some(value) = value {
                            *last = value;
                        }
                    }
                    self.record_values(source, &input);
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
            consumer_motion: input.consumer_motion.map(ConsumerDelta::new),
            pulses: input.pulses,
            pulse_break: false,
            consumer_switches: core::array::from_fn(|byte| {
                (0..4).fold(0, |bits, j| {
                    let i = 4 * byte + j;
                    let value = input.consumer_switches.get(i).copied().unwrap_or(0);
                    let code = if value == 0 {
                        0
                    } else if input.explicit_switches & (1 << i) != 0 {
                        value as u8 & 3
                    } else {
                        2
                    };
                    bits | (code << (2 * j))
                })
            }),
            position: input.position.map(|v| v.unwrap_or(u16::MAX)),
            consumer_values: input.consumer_values.map(|v| v.unwrap_or(u16::MAX)),
            source: source as u8,
        };
        self.count += 1;
        self.received = next;
        self.record_values(source, &input);
        Ok(())
    }
    pub fn remove(&mut self, source: usize) {
        if source >= SOURCES {
            return;
        }
        self.received[source] = Held::default();
        self.applied[source] = Held::default();
        for (received, snapshot) in self
            .received_position
            .iter_mut()
            .zip(&mut self.reconcile_position)
        {
            if received.is_some_and(|(_, owner)| owner as usize == source) {
                *received = None;
                *snapshot = None;
            }
        }
        for (received, snapshot) in self
            .received_values
            .iter_mut()
            .zip(&mut self.reconcile_values)
        {
            if received.is_some_and(|(_, owner)| owner as usize == source) {
                *received = None;
                *snapshot = None;
            }
        }
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
        if let Some(packet) = &self.packet {
            if packet.id == REPORT_CONSUMER_TOGGLE {
                self.switch_reset |= 1;
            }
            if packet.id == REPORT_CONSUMER_ON_OFF {
                self.switch_reset |= 2;
            }
        }
        self.head = 0;
        self.count = 0;
        self.active = None;
        self.packet = None;
        self.applied = self.received;
        self.reconcile_position = self.received_position.map(|v| v.map(|(value, _)| value));
        self.reconcile_values = self.received_values.map(|v| v.map(|(value, _)| value));
        self.dirty = KEYBOARD | MOUSE | CONSUMER;
        self.reconcile = true;
    }
    pub fn enable(&mut self, enabled: bool) {
        if enabled != self.enabled {
            self.enabled = enabled;
            self.resync();
        }
    }
    fn prepare(&mut self, active: Option<Queued>) -> bool {
        let held = combined(&self.applied)
            .and_then(|held| {
                if active.is_some_and(|q| q.pulse_break) {
                    let pulse = active.unwrap().pulses;
                    let mut held = held;
                    for (value, mask) in held.keys.iter_mut().zip(pulse.keys) {
                        *value &= !mask;
                    }
                    held.buttons &= !pulse.buttons;
                    let mut consumers = Held::default();
                    for &usage in &held.consumers {
                        if usage != 0 && !pulse.consumers.contains(&usage) {
                            consumers.consumer(usage)?;
                        }
                    }
                    held.consumers = consumers.consumers;
                    Ok(held)
                } else {
                    held.union(&active.map_or(Held::default(), |q| q.pulses))
                }
            })
            .expect("queued input exceeded consumer capacity");
        let motion = active.map_or([0; 4], |q| q.motion);
        let consumer_motion = active.map_or([0; CONSUMER_AXES.len()], |q| {
            q.consumer_motion.map(ConsumerDelta::value)
        });
        let mut packet = Packet {
            id: 0,
            bytes: [0; 68],
            length: 0,
            motion: [0; 4],
            consumer_motion: [0; CONSUMER_AXES.len()],
            switch_reset: false,
            source: active.map(|q| q.source as usize).filter(|&s| s < SOURCES),
        };
        let dirty = if self.dirty & KEYBOARD != 0 || held.keys != self.sent.keys {
            packet.id = REPORT_KEYBOARD;
            packet.bytes[..32].copy_from_slice(&held.keys);
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
        } else if self.switch_reset != 0
            || active
                .is_some_and(|q| q.consumer_switches != [0; CONSUMER_SWITCHES.len().div_ceil(4)])
        {
            let active = active.unwrap_or_default();
            let reset = self.switch_reset != 0;
            let explicit = if reset {
                self.switch_reset & 2 != 0
            } else {
                active
                    .switches()
                    .iter()
                    .enumerate()
                    .any(|(i, &v)| v != 0 && active.explicit(i))
            };
            packet.id = if explicit {
                REPORT_CONSUMER_ON_OFF
            } else {
                REPORT_CONSUMER_TOGGLE
            };
            packet.length = (CONSUMER_SWITCHES.len() * if explicit { 2 } else { 1 }).div_ceil(8);
            packet.switch_reset = reset;
            if !reset {
                for (i, &value) in active.switches().iter().enumerate() {
                    if active.explicit(i) == explicit {
                        let size = if explicit { 2 } else { 1 };
                        let bits = if explicit {
                            (value as u8) & 3
                        } else {
                            u8::from(value != 0)
                        };
                        packet.bytes[i * size / 8] |= bits << (i * size % 8);
                    }
                }
            }
            0
        } else if consumer_motion != [0; CONSUMER_AXES.len()] {
            packet.id = REPORT_CONSUMER_MOTION;
            packet.length = 4 * CONSUMER_AXES.len();
            for (i, value) in consumer_motion.iter().enumerate() {
                let value = (*value).clamp(i32::MIN.into(), i32::MAX.into()) as i32;
                packet.consumer_motion[i] = value;
                packet.bytes[4 * i..4 * i + 4].copy_from_slice(&value.to_le_bytes());
            }
            0
        } else if let Some(active) = active.filter(|q| q.position != [u16::MAX; 3]) {
            packet.id = REPORT_POINTER_POSITION;
            packet.length = 6;
            for (i, value) in active.position.iter().enumerate() {
                packet.bytes[2 * i..2 * i + 2].copy_from_slice(&value.to_le_bytes());
            }
            0
        } else if let Some(active) =
            active.filter(|q| q.consumer_values != [u16::MAX; CONSUMER_AXES.len()])
        {
            packet.id = REPORT_CONSUMER_VALUES;
            packet.length = 2 * CONSUMER_AXES.len();
            for (i, value) in active.consumer_values.iter().enumerate() {
                packet.bytes[2 * i..2 * i + 2].copy_from_slice(&value.to_le_bytes());
            }
            0
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
            if self.prepare(Some(Queued {
                position: self.reconcile_position.map(|v| v.unwrap_or(u16::MAX)),
                consumer_values: self.reconcile_values.map(|v| v.unwrap_or(u16::MAX)),
                source: u8::MAX,
                ..Queued::default()
            })) {
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
                let held =
                    combined(&self.applied).expect("queued input exceeded consumer capacity");
                let mut item = item;
                item.pulse_break = held
                    .keys
                    .iter()
                    .zip(item.pulses.keys)
                    .any(|(&a, b)| a & b != 0)
                    || held.buttons & item.pulses.buttons != 0
                    || held
                        .consumers
                        .iter()
                        .any(|&u| u != 0 && item.pulses.consumers.contains(&u));
                self.active = Some(item);
            }
            let active = self.active?;
            if active.pulses != Held::default() {
                let press = Queued {
                    motion: [0; 4],
                    consumer_motion: [ConsumerDelta::default(); CONSUMER_AXES.len()],
                    consumer_switches: [0; CONSUMER_SWITCHES.len().div_ceil(4)],
                    position: [u16::MAX; 3],
                    consumer_values: [u16::MAX; CONSUMER_AXES.len()],
                    ..active
                };
                if self.prepare(Some(press)) {
                    return self.packet.as_ref();
                }
                if active.pulse_break {
                    self.active.as_mut().unwrap().pulse_break = false;
                } else {
                    self.active.as_mut().unwrap().pulses = Held::default();
                }
                continue;
            }
            if self.prepare(Some(active)) {
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
            REPORT_KEYBOARD => self.sent.keys.copy_from_slice(&packet.bytes[..32]),
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
            REPORT_CONSUMER_MOTION => {
                if let Some(active) = &mut self.active
                    && packet.source == Some(active.source as usize)
                {
                    for (remaining, sent) in active
                        .consumer_motion
                        .iter_mut()
                        .zip(packet.consumer_motion)
                    {
                        *remaining = ConsumerDelta::new(remaining.value() - i64::from(sent));
                    }
                }
            }
            REPORT_CONSUMER_TOGGLE | REPORT_CONSUMER_ON_OFF => {
                let explicit = packet.id == REPORT_CONSUMER_ON_OFF;
                let mask = if explicit { 2 } else { 1 };
                if packet.switch_reset {
                    self.switch_reset &= !mask;
                } else {
                    self.switch_reset |= mask;
                }
                if !packet.switch_reset
                    && let Some(active) = &mut self.active
                    && packet.source == Some(active.source as usize)
                {
                    for i in 0..CONSUMER_SWITCHES.len() {
                        if active.explicit(i) == explicit {
                            active.consumer_switches[i / 4] &= !(3u8 << (2 * (i % 4)));
                        }
                    }
                }
            }
            REPORT_POINTER_POSITION | REPORT_CONSUMER_VALUES => {
                if packet.source.is_none() {
                    if packet.id == REPORT_POINTER_POSITION {
                        self.reconcile_position = [None; 3];
                    } else {
                        self.reconcile_values = [None; CONSUMER_AXES.len()];
                    }
                }
                if let Some(active) = &mut self.active
                    && packet.source == Some(active.source as usize)
                {
                    if packet.id == REPORT_POINTER_POSITION {
                        active.position = [u16::MAX; 3];
                    } else {
                        active.consumer_values = [u16::MAX; CONSUMER_AXES.len()];
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
