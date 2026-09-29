use alloc::{boxed::Box, vec::Vec};

pub const DESCRIPTOR_BYTES: usize = 2048;
pub const REPORT_BYTES: usize = 512;
pub const REPORTS: usize = 16;
pub const FIELDS: usize = 96;
pub const USAGE_SPANS: usize = 128;
pub const SOURCES: usize = 4;
pub const QUEUE: usize = 64;
pub const CONSUMERS: usize = 8;
pub const MOTION_LIMIT: i64 = 1_048_576;
pub const KEYBOARD: u8 = 1;
pub const MOUSE: u8 = 2;
pub const CONSUMER: u8 = 4;
pub const HIDPP_SHORT: u8 = 1;
pub const HIDPP_LONG: u8 = 2;
const HIDPP_APPLICATION: u8 = 8;
const HIDPP_BLUETOOTH_APPLICATION: u8 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Invalid,
    Limit,
    Capacity,
    Unsupported,
    Rollover,
    Overflow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Held {
    pub keys: [u8; 32],
    pub consumers: [u16; CONSUMERS],
    pub buttons: u16,
}
impl Held {
    pub fn consumer(&mut self, usage: u16) -> Result<(), Error> {
        if usage == 0 || usage > 1023 {
            return Err(Error::Unsupported);
        }
        if self.consumers.contains(&usage) {
            return Ok(());
        }
        let index = self
            .consumers
            .iter()
            .position(|&u| u == 0 || u > usage)
            .ok_or(Error::Overflow)?;
        if self.consumers[CONSUMERS - 1] != 0 {
            return Err(Error::Overflow);
        }
        self.consumers.copy_within(index..CONSUMERS - 1, index + 1);
        self.consumers[index] = usage;
        Ok(())
    }
    pub fn union(self, other: &Self) -> Result<Self, Error> {
        let mut combined = self;
        for (a, b) in combined.keys.iter_mut().zip(other.keys) {
            *a |= b;
        }
        combined.buttons |= other.buttons;
        for &usage in &other.consumers {
            if usage != 0 {
                combined.consumer(usage)?;
            }
        }
        Ok(combined)
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Input {
    pub held: Held,
    pub motion: [i64; 4],
}
#[derive(Clone, Debug)]
pub struct State {
    reports: Box<[Held]>,
}
impl State {
    pub fn clear(&mut self) {
        self.reports.fill(Held::default());
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Span {
    page: u16,
    first: u16,
    last: u16,
}
impl Span {
    fn first(self) -> u32 {
        (u32::from(self.page) << 16) | u32::from(self.first)
    }
    fn last(self) -> u32 {
        (u32::from(self.page) << 16) | u32::from(self.last)
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Field {
    // Naturally aligned word: 12-bit offset, 12-bit count-minus-one,
    // 5-bit size-minus-one, 3-bit application. Bounds are checked at compile.
    location: u32,
    span: u8,
    spans: u8,
    report: u8,
    // Constant fields are discarded. Bit 0 marks output on retained fields;
    // the remaining bits keep the descriptor's variable/relative/null flags.
    flags: u8,
    minimum: i32,
    maximum: u32,
}
impl Field {
    fn bit(self) -> usize {
        (self.location & 0xfff) as usize
    }
    fn count(self) -> usize {
        ((self.location >> 12) & 0xfff) as usize + 1
    }
    fn size(self) -> usize {
        ((self.location >> 24) & 31) as usize + 1
    }
    fn app(self) -> u8 {
        (self.location >> 29) as u8
    }
}
const _: () = assert!(core::mem::size_of::<Field>() == 16);
#[derive(Clone, Copy, Debug, Default)]
pub struct Layout {
    pub bits: [u16; 3],
    pub id: u8,
    pub leds: bool,
    pub output_other: bool,
    hidpp_fields: u8,
    hidpp_other: u8,
}
#[derive(Clone, Copy, Debug)]
struct BatteryField {
    instance: u8,
    usage: u32,
    last_usage: u32,
    array: bool,
    minimum: i32,
    maximum: i64,
    bit: u16,
    size: u8,
    report: u8,
    kind: u8,
}
#[derive(Clone, Debug, Default)]
pub struct Map {
    fields: Box<[Field]>,
    battery: Box<[BatteryField]>,
    usages: Box<[Span]>,
    reports: Box<[Layout]>,
    pub roles: u8,
    pub hidpp_reports: u8,
    pub numbered: bool,
    pub ignored_fields: bool,
}
#[derive(Clone, Copy, Default)]
struct Global {
    page: u32,
    count: u32,
    size: u32,
    max: u32,
    min: i32,
    id: u8,
    max_size: usize,
}

fn signed(value: u32, bytes: usize) -> i32 {
    if bytes > 0 && bytes < 4 {
        ((value << (32 - bytes * 8)) as i32) >> (32 - bytes * 8)
    } else {
        value as i32
    }
}
fn usage_at(spans: &[Span], mut index: u32, repeat: bool) -> u32 {
    for span in spans {
        let length = span.last() - span.first() + 1;
        if index < length {
            return span.first() + index;
        }
        index -= length;
    }
    if repeat {
        spans.last().map_or(0, |s| s.last())
    } else {
        0
    }
}
fn usage_kind(usage: u32, app: u8, output: bool, relative: bool) -> u8 {
    let page = usage >> 16;
    let code = usage & 0xffff;
    if app == 0 || app == HIDPP_APPLICATION || app == HIDPP_BLUETOOTH_APPLICATION {
        return 0;
    }
    if output {
        return if page == 8 && (1..=5).contains(&code) {
            KEYBOARD
        } else {
            0
        };
    }
    if page == 7 && (1..=255).contains(&code) {
        return KEYBOARD;
    }
    if page == 9 && (1..=16).contains(&code) && app == MOUSE {
        return MOUSE;
    }
    if app == MOUSE && matches!(usage, 0x10030 | 0x10031 | 0x10038) {
        return MOUSE;
    }
    if usage == 0xc0238 && relative {
        return MOUSE;
    }
    if page == 12 && (1..=1023).contains(&code) {
        return CONSUMER;
    }
    0
}
// Admission scratch lives on the owner stack. Only the exact retained map is
// copied to the heap, without interleaving vector growth with raw descriptors.
#[derive(Default)]
struct Compiler {
    fields: heapless::Vec<Field, FIELDS>,
    battery: heapless::Vec<BatteryField, 16>,
    usages: heapless::Vec<Span, USAGE_SPANS>,
    reports: heapless::Vec<Layout, REPORTS>,
    roles: u8,
    hidpp_reports: u8,
    numbered: bool,
    ignored_fields: bool,
}
fn copy<T: Copy>(slice: &[T]) -> Result<Box<[T]>, Error> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(slice.len())
        .map_err(|_| Error::Capacity)?;
    values.extend_from_slice(slice);
    Ok(values.into_boxed_slice())
}
fn push<T, const N: usize>(values: &mut heapless::Vec<T, N>, value: T) -> Result<(), Error> {
    values.push(value).map_err(|_| Error::Limit)
}
impl Map {
    /// Allocate once at admission; decoding and clearing never allocate.
    pub fn state(&self) -> Result<State, Error> {
        let mut reports = Vec::new();
        reports
            .try_reserve_exact(self.reports.len())
            .map_err(|_| Error::Capacity)?;
        reports.resize(self.reports.len(), Held::default());
        Ok(State {
            reports: reports.into_boxed_slice(),
        })
    }
    pub fn battery_reports(&self) -> impl Iterator<Item = (u8, crate::bluetooth::ReportType)> + '_ {
        self.reports.iter().enumerate().flat_map(move |(i, r)| {
            [0, 2].into_iter().filter_map(move |kind| {
                self.battery
                    .iter()
                    .any(|f| f.report as usize == i && f.kind == kind)
                    .then_some((
                        r.id,
                        if kind == 0 {
                            crate::bluetooth::ReportType::Input
                        } else {
                            crate::bluetooth::ReportType::Feature
                        },
                    ))
            })
        })
    }
    pub fn battery(
        &self,
        report: u8,
        kind: crate::bluetooth::ReportType,
        bytes: &[u8],
        mut emit: impl FnMut(
            u8,
            cordial_protocol::info::InfoKey,
            cordial_protocol::settings::SettingValue,
        ),
    ) {
        use cordial_protocol::{info::InfoKey as K, settings::SettingValue as V};
        let kind = match kind {
            crate::bluetooth::ReportType::Input => 0,
            crate::bluetooth::ReportType::Feature => 2,
            _ => return,
        };
        for instance in 0..5 {
            let mut has_charging = false;
            let mut charging = None;
            let mut explicit_charging = None;
            for f in self.battery.iter().filter(|f| {
                self.reports[f.report as usize].id == report
                    && f.kind == kind
                    && f.instance == instance
            }) {
                let key = if matches!(f.usage, 0x60020 | 0x850064) {
                    K::BatteryPercent
                } else {
                    K::BatteryCharging
                };
                if key == K::BatteryCharging {
                    has_charging = true;
                }
                if bytes.len() * 8 < f.bit as usize + f.size as usize {
                    if key == K::BatteryPercent {
                        emit(instance, key, V::Null);
                    }
                    continue;
                }
                let mut raw = 0u32;
                for i in 0..f.size as usize {
                    let bit = f.bit as usize + i;
                    raw |= u32::from((bytes[bit / 8] >> (bit % 8)) & 1) << i;
                }
                let value = if f.minimum < 0 {
                    signed(raw, 4) as i64
                        - if f.size < 32 && raw & (1 << (f.size - 1)) != 0 {
                            1i64 << f.size
                        } else {
                            0
                        }
                } else {
                    raw as i64
                };
                let valid = value >= i64::from(f.minimum) && value <= f.maximum;
                if key == K::BatteryCharging {
                    if valid {
                        let usage = if f.array {
                            (i64::from(f.usage) + value - i64::from(f.minimum)) as u32
                        } else {
                            f.usage
                        };
                        if usage <= f.last_usage {
                            match usage {
                                0x850044 => explicit_charging = Some(f.array || value != 0),
                                0x850045 | 0x850046 if f.array || value != 0 => {
                                    charging = Some(false)
                                }
                                _ => {}
                            }
                        }
                    }
                    continue;
                }
                let value = if !valid {
                    V::Null
                } else if key == K::BatteryPercent {
                    if f.usage == 0x850064 {
                        if (0..=100).contains(&value) {
                            V::Integer(value)
                        } else {
                            V::Null
                        }
                    } else {
                        let span = f.maximum - i64::from(f.minimum);
                        if span > 0 {
                            V::Integer(((value - i64::from(f.minimum)) * 100 + span / 2) / span)
                        } else {
                            V::Null
                        }
                    }
                } else if f.usage == 0x850044 {
                    V::Bool(value != 0)
                } else if value != 0 {
                    V::Bool(false)
                } else {
                    continue;
                };
                emit(instance, key, value);
            }
            if has_charging {
                emit(
                    instance,
                    K::BatteryCharging,
                    explicit_charging.or(charging).map_or(V::Null, V::Bool),
                );
            }
        }
    }
    pub fn reports(&self) -> &[Layout] {
        &self.reports
    }
    pub fn compile(descriptor: &[u8]) -> Result<Self, Error> {
        Compiler::compile(descriptor)
    }
}
impl Compiler {
    fn compile(descriptor: &[u8]) -> Result<Map, Error> {
        if descriptor.is_empty() {
            return Err(Error::Invalid);
        }
        if descriptor.len() > DESCRIPTOR_BYTES {
            return Err(Error::Limit);
        }
        let mut map = Self::default();
        let mut g = Global::default();
        let mut globals = [Global::default(); 8];
        let mut depth = 0;
        let mut local = heapless::Vec::<Span, USAGE_SPANS>::new();
        let mut pending = false;
        let mut apps = [0; 16];
        let mut battery_instances = [0; 16];
        let mut battery_instance = 0;
        let mut next_battery = 1u8;
        let mut collection = 0;
        let mut app = 0;
        let mut pos = 0;
        while pos < descriptor.len() {
            let prefix = descriptor[pos];
            pos += 1;
            if prefix == 0xfe {
                return Err(Error::Unsupported);
            }
            let bytes = match prefix & 3 {
                3 => 4,
                n => n as usize,
            };
            if descriptor.len() - pos < bytes {
                return Err(Error::Invalid);
            }
            let mut value = 0u32;
            for i in 0..bytes {
                value |= u32::from(descriptor[pos + i]) << (8 * i);
            }
            pos += bytes;
            let kind = (prefix >> 2) & 3;
            let tag = prefix >> 4;
            match kind {
                1 => match tag {
                    0 => {
                        if value > 0xffff {
                            return Err(Error::Invalid);
                        }
                        g.page = value;
                    }
                    1 => g.min = signed(value, bytes),
                    2 => {
                        g.max = value;
                        g.max_size = bytes;
                    }
                    3..=6 => {}
                    7 => g.size = value,
                    8 => {
                        if !(1..=255).contains(&value) {
                            return Err(Error::Invalid);
                        }
                        g.id = value as u8;
                        map.numbered = true;
                    }
                    9 => g.count = value,
                    10 => {
                        if bytes != 0 || depth == globals.len() {
                            return Err(Error::Limit);
                        }
                        globals[depth] = g;
                        depth += 1;
                    }
                    11 => {
                        if bytes != 0 || depth == 0 {
                            return Err(Error::Invalid);
                        }
                        depth -= 1;
                        g = globals[depth];
                    }
                    _ => return Err(Error::Unsupported),
                },
                2 => {
                    let usage = if bytes == 4 {
                        value
                    } else {
                        (g.page << 16) | value
                    };
                    match tag {
                        0 | 1 => {
                            if pending {
                                return Err(Error::Invalid);
                            }
                            push(
                                &mut local,
                                Span {
                                    page: (usage >> 16) as u16,
                                    first: usage as u16,
                                    last: usage as u16,
                                },
                            )?;
                            pending = tag == 1;
                        }
                        2 => {
                            if !pending {
                                return Err(Error::Invalid);
                            }
                            let span = local.last_mut().ok_or(Error::Invalid)?;
                            if usage < span.first() || usage >> 16 != span.first() >> 16 {
                                return Err(Error::Invalid);
                            }
                            span.last = usage as u16;
                            pending = false;
                        }
                        3..=5 | 7..=9 => {}
                        _ => return Err(Error::Unsupported),
                    }
                }
                0 => {
                    if pending {
                        return Err(Error::Invalid);
                    }
                    match tag {
                        10 => {
                            if bytes > 1 || collection == apps.len() {
                                return Err(Error::Invalid);
                            }
                            apps[collection] = app;
                            battery_instances[collection] = battery_instance;
                            if usage_at(&local, 0, false) == 0x840012 {
                                battery_instance = next_battery;
                                next_battery = next_battery.saturating_add(1);
                            }
                            collection += 1;
                            if value == 1 {
                                let u = usage_at(&local, 0, false);
                                app = match u {
                                    0x10006 | 0x10007 => KEYBOARD,
                                    0x10002 => MOUSE,
                                    0xc0001 => CONSUMER,
                                    0xff430202 => HIDPP_BLUETOOTH_APPLICATION,
                                    _ if u >> 16 == 0xff00 => HIDPP_APPLICATION,
                                    _ => 0,
                                };
                            }
                        }
                        12 => {
                            if bytes != 0 || collection == 0 {
                                return Err(Error::Invalid);
                            }
                            collection -= 1;
                            app = apps[collection];
                            battery_instance = battery_instances[collection];
                        }
                        8 | 9 | 11 => {
                            if collection == 0 {
                                return Err(Error::Invalid);
                            }
                            map.compile_field(
                                &g,
                                &local,
                                app,
                                battery_instance,
                                match tag {
                                    8 => 0,
                                    9 => 1,
                                    _ => 2,
                                },
                                value,
                            )?;
                        }
                        _ => return Err(Error::Unsupported),
                    }
                    local.clear();
                }
                _ => return Err(Error::Unsupported),
            }
        }
        if depth != 0 || collection != 0 || !local.is_empty() {
            return Err(Error::Invalid);
        }
        for report in &map.reports {
            if map.numbered && report.id == 0 {
                return Err(Error::Invalid);
            }
            if report.hidpp_fields == 3 && report.hidpp_other == 0 {
                if report.id == 0x10 && report.bits[0] == 48 && report.bits[1] == 48 {
                    map.hidpp_reports |= HIDPP_SHORT;
                }
                if report.id == 0x11 && report.bits[0] == 152 && report.bits[1] == 152 {
                    map.hidpp_reports |= HIDPP_LONG;
                }
            }
        }
        if map.roles == 0 {
            Err(Error::Unsupported)
        } else {
            Ok(Map {
                fields: copy(&map.fields)?,
                battery: copy(&map.battery)?,
                usages: copy(&map.usages)?,
                reports: copy(&map.reports)?,
                roles: map.roles,
                hidpp_reports: map.hidpp_reports,
                numbered: map.numbered,
                ignored_fields: map.ignored_fields,
            })
        }
    }
    fn compile_field(
        &mut self,
        g: &Global,
        local: &[Span],
        app: u8,
        battery_instance: u8,
        kind: usize,
        flags: u32,
    ) -> Result<(), Error> {
        if g.count == 0 {
            return Ok(());
        }
        if g.size == 0 {
            return Err(Error::Invalid);
        }
        if g.count > (REPORT_BYTES * 8) as u32 || g.size > (REPORT_BYTES * 8) as u32 / g.count {
            return Err(Error::Limit);
        }
        let ri = match self.reports.iter().position(|r| r.id == g.id) {
            Some(i) => i,
            None => {
                push(
                    &mut self.reports,
                    Layout {
                        id: g.id,
                        ..Layout::default()
                    },
                )?;
                self.reports.len() - 1
            }
        };
        let report = &mut self.reports[ri];
        let bits = g.count * g.size;
        let offset = report.bits[kind];
        if u32::from(offset) + bits > (REPORT_BYTES * 8) as u32 {
            return Err(Error::Limit);
        }
        report.bits[kind] = (u32::from(offset) + bits) as u16;
        if kind != 2 {
            let hidpp_app =
                app == HIDPP_APPLICATION || (app == HIDPP_BLUETOOTH_APPLICATION && g.id == 0x11);
            if hidpp_app && g.size == 8 && flags & 1 == 0 {
                report.hidpp_fields |= 1 << kind;
            } else {
                report.hidpp_other |= 1 << kind;
            }
        }
        // Battery fields may be Input or Feature reports, including fields
        // outside the keyboard/mouse application collection.
        if kind != 1 && flags & 5 == 0 && g.size <= 32 && flags & 0x100 == 0 {
            let maximum = if g.min < 0 {
                i64::from(signed(g.max, g.max_size))
            } else {
                i64::from(g.max)
            };
            for j in 0..g.count {
                let array = flags & 2 == 0;
                let usage = if array {
                    local.first().map_or(0, |s| s.first())
                } else {
                    usage_at(local, j, true)
                };
                let last_usage = if array {
                    local.first().map_or(0, |s| s.last())
                } else {
                    usage
                };
                if ((!array
                    && matches!(usage, 0x60020 | 0x850064 | 0x850044 | 0x850045 | 0x850046))
                    || (array && local.len() == 1 && usage >= 0x850040 && last_usage <= 0x850047))
                    && i64::from(g.min) <= maximum
                {
                    let _ = self.battery.push(BatteryField {
                        instance: battery_instance,
                        usage,
                        last_usage,
                        array,
                        minimum: g.min,
                        maximum,
                        bit: offset + (j * g.size) as u16,
                        size: g.size as u8,
                        report: ri as u8,
                        kind: kind as u8,
                    });
                }
            }
        }
        if flags & 1 != 0 || kind == 2 {
            return Ok(());
        }
        let variable = flags & 2 != 0;
        let relative = flags & 4 != 0;
        let mut roles = 0;
        let mut other = false;
        let mut unsupported = false;
        for span in local {
            let page = span.first() >> 16;
            let cap = match page {
                7 => 255,
                9 => 16,
                12 => 1023,
                1 => 56,
                8 => 5,
                _ => 0,
            };
            let end = ((page << 16) | cap).min(span.last());
            if kind == 1 && (page != 8 || span.first() < 0x80001 || span.last() > 0x80005) {
                other = true;
            }
            if cap != 0 {
                for usage in span.first()..=end {
                    let role = usage_kind(usage, app, kind == 1, relative);
                    roles |= role;
                    if role == 0 {
                        other = true;
                    }
                    if role != 0 && kind == 0 {
                        let desktop = app == MOUSE && matches!(usage, 0x10030 | 0x10031 | 0x10038);
                        if desktop && (!variable || !relative) {
                            return Err(Error::Unsupported);
                        }
                        let axis = desktop || (usage == 0xc0238 && variable && relative);
                        if !axis && relative {
                            unsupported = true;
                        }
                    }
                }
            }
        }
        if kind == 1 && (other || local.is_empty() || !variable) {
            report.output_other = true;
        }
        if roles == 0 {
            return Ok(());
        }
        if kind == 1 {
            report.leds = true;
        }
        if unsupported || (kind == 1 && (relative || !variable || g.min != 0 || g.max != 1)) {
            self.ignored_fields = true;
            if kind == 1 {
                report.output_other = true;
            }
            return Ok(());
        }
        if g.size > 32 || flags & 0x100 != 0 {
            return Err(Error::Unsupported);
        }
        if g.min < 0 {
            let maximum = signed(g.max, g.max_size);
            if g.min > maximum
                || (g.size < 32
                    && (i64::from(g.min) < -(1i64 << (g.size - 1))
                        || i64::from(maximum) >= (1i64 << (g.size - 1))))
            {
                return Err(Error::Invalid);
            }
        } else if g.min as u32 > g.max || (g.size < 32 && g.max >= (1u32 << g.size)) {
            return Err(Error::Invalid);
        }
        if kind != 1 {
            self.roles |= roles;
        }
        if self.usages.len() + local.len() > USAGE_SPANS {
            return Err(Error::Limit);
        }
        debug_assert!(app < 8);
        let field = Field {
            location: u32::from(offset)
                | ((g.count - 1) << 12)
                | ((g.size - 1) << 24)
                | (u32::from(app) << 29),
            span: self.usages.len() as u8,
            spans: local.len() as u8,
            report: ri as u8,
            flags: flags as u8 | u8::from(kind == 1),
            minimum: g.min,
            maximum: if g.min < 0 {
                signed(g.max, g.max_size) as u32
            } else {
                g.max
            },
        };
        push(&mut self.fields, field)?;
        self.usages
            .extend_from_slice(local)
            .map_err(|_| Error::Limit)?;
        Ok(())
    }
}
impl Map {
    pub fn decode(&self, state: &mut State, report_id: u8, payload: &[u8]) -> Result<Input, Error> {
        if state.reports.len() != self.reports.len() {
            return Err(Error::Invalid);
        }
        let ri = self
            .reports
            .iter()
            .position(|r| r.id == report_id)
            .ok_or(Error::Invalid)?;
        let bits = self.reports[ri].bits[0] as usize;
        if bits == 0 || payload.len() < bits.div_ceil(8) || payload.len() > REPORT_BYTES {
            return Err(Error::Invalid);
        }
        let mut next = Input::default();
        for field in &self.fields {
            if field.report as usize != ri || field.flags & 1 != 0 {
                continue;
            }
            let variable = field.flags & 2 != 0;
            let relative = field.flags & 4 != 0;
            for j in 0..field.count() {
                let bit = field.bit() + j * field.size();
                let mut raw = 0u32;
                for i in 0..field.size() {
                    raw |= u32::from((payload[(bit + i) / 8] >> ((bit + i) % 8)) & 1) << i;
                }
                let mut value = i64::from(raw);
                if field.minimum < 0 && raw & (1 << (field.size() - 1)) != 0 {
                    value -= 1i64 << field.size();
                }
                let maximum = if field.minimum < 0 {
                    i64::from(field.maximum as i32)
                } else {
                    i64::from(field.maximum)
                };
                if value < i64::from(field.minimum) || value > maximum {
                    if !variable || field.flags & 0x40 != 0 {
                        continue;
                    }
                    value = value.clamp(i64::from(field.minimum), maximum);
                }
                let spans = &self.usages[field.span as usize..(field.span + field.spans) as usize];
                let usage = usage_at(
                    spans,
                    if variable {
                        j as u32
                    } else {
                        (value - i64::from(field.minimum)) as u32
                    },
                    variable,
                );
                if usage_kind(usage, field.app(), false, relative) == 0 {
                    continue;
                }
                next.set_usage(usage, if variable { value } else { 1 }, relative)?;
            }
        }
        // This report is valid on its own. Retain its releases even when other
        // report IDs temporarily put the aggregate over the consumer limit.
        state.reports[ri] = next.held;
        for (i, other) in state.reports.iter().enumerate().take(self.reports.len()) {
            if i != ri {
                next.held = next.held.union(other)?;
            }
        }
        Ok(next)
    }
    pub fn led_report(&self, ri: usize, leds: u8, payload: &mut [u8]) -> Option<usize> {
        let report = self.reports.get(ri)?;
        if !report.leds || report.output_other {
            return None;
        }
        let length = (report.bits[1] as usize).div_ceil(8);
        if payload.len() < length {
            return None;
        }
        payload[..length].fill(0);
        for field in &self.fields {
            if field.report as usize != ri || field.flags & 1 == 0 {
                continue;
            }
            let spans = &self.usages[field.span as usize..(field.span + field.spans) as usize];
            for j in 0..field.count() {
                let usage = usage_at(spans, j as u32, true);
                if !(0x80001..=0x80005).contains(&usage) || leds & (1 << (usage - 0x80001)) == 0 {
                    continue;
                }
                let bit = field.bit() + j * field.size();
                payload[bit / 8] |= 1 << (bit % 8);
            }
        }
        Some(length)
    }
}
impl Input {
    fn set_usage(&mut self, usage: u32, value: i64, relative: bool) -> Result<(), Error> {
        if value == 0 {
            return Ok(());
        }
        let page = usage >> 16;
        let code = (usage & 0xffff) as usize;
        if relative {
            let axis = match usage {
                0x10030 => Some(0),
                0x10031 => Some(1),
                0x10038 => Some(2),
                0xc0238 => Some(3),
                _ => None,
            };
            if let Some(axis) = axis {
                self.motion[axis] += value;
            }
        } else if page == 7 && (1..=255).contains(&code) {
            if code <= 3 {
                return Err(Error::Rollover);
            }
            self.held.keys[code / 8] |= 1 << (code % 8);
        } else if page == 9 && (1..=16).contains(&code) {
            self.held.buttons |= 1 << (code - 1);
        } else if page == 12 && (1..=1023).contains(&code) {
            self.held.consumer(code as u16)?;
        }
        Ok(())
    }
}
