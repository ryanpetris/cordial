use alloc::{boxed::Box, vec::Vec};

pub const DESCRIPTOR_BYTES: usize = 2048;
pub const REPORT_BYTES: usize = 512;
pub const REPORTS: usize = 16;
pub const FIELDS: usize = 96;
pub const USAGE_SPANS: usize = 128;
pub const SOURCES: usize = 4;
pub const QUEUE: usize = 64;
pub const CONSUMERS: usize = 8;
pub const MAX_WARNINGS: usize = 3 * (FIELDS + 2 * REPORTS * (REPORT_BYTES * 8 + 1));
pub const MOTION_LIMIT: i64 = 1_048_576;
/// Consumer-page linear controls from the USB HID Usage Tables.
pub const CONSUMER_AXES: [u16; 17] = [
    0x71, 0x7b, 0x86, 0xbd, 0xbf, 0xe0, 0xe1, 0xe3, 0xe4, 0x101, 0x103, 0x105, 0x109, 0x170, 0x22f,
    0x235, 0x238,
];
pub const KEYBOARD: u8 = 1;
pub const MOUSE: u8 = 2;
pub const CONSUMER: u8 = 4;
pub const SYSTEM: u8 = 8;
pub const RADIO: u8 = 16;
pub const ROTATION_KNOWN: u64 = 1 << 63;
pub const ROTATION_STATE: u64 = 1 << 36;
const SYSTEM_APPLICATION: u8 = 3;
const RADIO_APPLICATION: u8 = 5;
/// Standard keyboard-related Generic Desktop System usages, in USB bitmap order.
pub const SYSTEM_USAGES: &[u16] = &[
    0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f, 0x9b,
    0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb0, 0xb1, 0xb2, 0xb3, 0xb4,
    0xb5, 0xb6, 0xb7, 0xc9, 0xca,
];
const LOCK_SELECTORS: [u32; 8] = [
    0x80001, 0x80002, 0x80003, 0x80004, 0x80005, 0x70053, 0x70039, 0x70047,
];
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
    pub system: u64,
    /// Bit 0 is the radio button; bit 1 is slider state, valid when bit 2 is set.
    pub radio: u8,
}
impl Held {
    pub fn consumer(&mut self, usage: u16) -> Result<(), Error> {
        if usage == 0 {
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
        combined.system |= other.system;
        combined.radio |= other.radio;
        for &usage in &other.consumers {
            if usage != 0 {
                combined.consumer(usage)?;
            }
        }
        Ok(combined)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Input {
    pub held: Held,
    pub motion: [i64; 4],
    pub consumer_motion: [i64; CONSUMER_AXES.len()],
    pub pulses: Held,
    pub pulse_repetitions: [u32; QUEUE],
    pub pulse_repetition_count: u8,
    pub consumer_switches: [i8; CONSUMER_SWITCHES.len()],
    pub explicit_switches: u64,
    /// Latest rotation-lock and radio slider observations, independent of held buttons.
    pub sliders: [Option<bool>; 2],
    pub position: [Option<u16>; 3],
    pub consumer_values: [Option<u16>; CONSUMER_AXES.len()],
}
impl Default for Input {
    fn default() -> Self {
        Self {
            held: Held::default(),
            motion: [0; 4],
            consumer_motion: [0; CONSUMER_AXES.len()],
            pulses: Held::default(),
            pulse_repetitions: [0; QUEUE],
            pulse_repetition_count: 0,
            consumer_switches: [0; CONSUMER_SWITCHES.len()],
            explicit_switches: 0,
            sliders: [None; 2],
            position: [None; 3],
            consumer_values: [None; CONSUMER_AXES.len()],
        }
    }
}
#[derive(Clone, Debug)]
pub struct State {
    reports: Box<[u8]>,
    arrays: Box<[Held]>,
    previous: Box<[u8]>,
    latches: Box<[u8]>,
}
impl State {
    pub fn clear(&mut self) {
        self.reports.fill(0);
        self.arrays.fill(Held::default());
        self.previous.fill(0);
        self.latches.fill(0);
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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
    // Packs a 12-bit offset, 12-bit count-minus-one, history bit and 3-bit application.
    // Bounds are checked while compiling the descriptor.
    location: u32,
    span: u8,
    spans: u8,
    report: u8,
    // Retains main-item flags for variable, relative and null handling.
    flags: u8,
    minimum: i32,
    maximum: u32,
    state: u16,
    size: u16,
}
#[derive(Clone, Copy, Debug)]
struct OutputField {
    collection: u16,
    scope: u16,
    state: u16,
    retained: u16,
    numeric: u16,
    scale: u16,
    group: u16,
    unit: u16,
    target: u8,
    mode: u8,
    dark: i64,
    kind: u8,
    bit: u16,
    size: u16,
    count: u16,
    span: u8,
    spans: u8,
    report: u8,
    flags: u16,
    minimum: i32,
    maximum: i64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndicatorError {
    ReadRequired,
    StateUnknown,
    ArrayCapacity,
    RelativeArray,
    Range,
    Buffered,
    Mode,
    Nonlinear,
    Scale,
}
pub(crate) struct IndicatorCache {
    values: Box<[u8]>,
    retained: Box<[[i64; 2]]>,
    numeric: Box<[[i128; 2]]>,
    numeric_live: Box<[u8]>,
}
impl IndicatorCache {
    fn get(&self, slot: usize) -> u8 {
        (self.values[slot / 2] >> (4 * (slot % 2))) & 15
    }
    fn put(&mut self, slot: usize, value: u8) {
        let shift = 4 * (slot % 2);
        self.values[slot / 2] = (self.values[slot / 2] & !(15 << shift)) | ((value & 15) << shift);
    }
    pub(crate) fn begin_read(&mut self) {
        for value in &mut self.values {
            *value &= 0x77;
        }
        self.numeric_live.fill(0);
    }
    pub(crate) fn begin_colors(&mut self) {
        for value in &mut self.retained {
            value[1] = i64::MIN;
        }
    }
}
fn report_kind(kind: crate::bluetooth::ReportType) -> u8 {
    match kind {
        crate::bluetooth::ReportType::Input => 0,
        crate::bluetooth::ReportType::Output => 1,
        crate::bluetooth::ReportType::Feature => 2,
    }
}
/// Indicator bits accompanied by a mask of states the device actually reports.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndicatorValue {
    pub bits: u8,
    pub known: u8,
}
impl IndicatorValue {
    pub fn merge(&mut self, other: Self) {
        self.bits = (self.bits & !other.known) | (other.bits & other.known);
        self.known |= other.known;
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndicatorEncoding {
    pub length: usize,
    pub applied: u8,
    pub unknown: u8,
    pub complete: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndicatorFailure {
    pub reason: IndicatorError,
    pub bit_offset: Option<u16>,
    pub usage: Option<u32>,
}
impl Field {
    fn history(self) -> bool {
        self.location & (1 << 24) != 0
    }
    fn bit(self) -> usize {
        (self.location & 0xfff) as usize
    }
    fn count(self) -> usize {
        ((self.location >> 12) & 0xfff) as usize + 1
    }
    fn size(self) -> usize {
        usize::from(self.size)
    }
    fn app(self) -> u8 {
        (self.location >> 29) as u8
    }
}
const _: () = assert!(core::mem::size_of::<Field>() == 20);
#[derive(Clone, Copy, Debug, Default)]
pub struct Layout {
    pub bits: [u16; 3],
    pub id: u8,
    pub leds: bool,
    held_roles: u8,
    key_first: u8,
    key_last: u8,
    hidpp_fields: u8,
    hidpp_other: u8,
    /// Report types, as bits, holding an absolute field left unused, whose bits a write must
    /// copy from a fresh read of the report.
    preserved: u8,
    /// Report types, as bits, holding a relative field left unused. A write could repeat its
    /// last value, so these reports are not written.
    unwritable: u8,
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
#[derive(Clone, Copy, Debug, Default)]
struct Section {
    offset: u16,
    length: u16,
}
#[derive(Clone, Copy, Debug)]
struct IndicatorScale {
    minimum: i64,
    maximum: i64,
    unit: u32,
    exponent: i8,
}
#[derive(Clone, Debug, Default)]
pub struct Map {
    // One aligned allocation holds the immutable compiled sections. Releasing
    // a service returns one contiguous block for its next descriptor/map.
    storage: Box<[core::mem::MaybeUninit<u64>]>,
    sections: [Section; 7],
    pub roles: u8,
    pub hidpp_reports: u8,
    pub numbered: bool,
    state_bits: usize,
    array_fields: usize,
    indicator_units: usize,
    retained_values: usize,
    numeric_values: usize,
}
#[derive(Clone, Copy, Debug)]
pub struct InputLimitation {
    pub code: crate::model::errors::WarningCode,
    /// The report type: 0 input, 1 output, 2 feature.
    pub kind: u8,
    pub report_id: u8,
    pub bit_offset: u16,
    pub usage_page: u16,
    pub usage: u16,
}
const _: () = {
    assert!(core::mem::align_of::<Field>() <= 8);
    assert!(core::mem::align_of::<OutputField>() <= 8);
    assert!(core::mem::align_of::<BatteryField>() <= 8);
    assert!(core::mem::align_of::<Span>() <= 8);
    assert!(core::mem::align_of::<Layout>() <= 8);
    assert!(core::mem::align_of::<InputLimitation>() <= 8);
    assert!(core::mem::align_of::<IndicatorScale>() <= 8);
};
#[derive(Clone, Copy, Default)]
struct Global {
    page: u32,
    count: u32,
    size: u32,
    max: u32,
    min: i32,
    id: u8,
    max_size: usize,
    physical_minimum: i32,
    physical_maximum: u32,
    physical_maximum_size: usize,
    physical_bounds: u8,
    unit: u32,
    exponent: i8,
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
fn validate_scalar(g: &Global) -> Result<(), Error> {
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
    Ok(())
}

fn numeric_selector(usage: u32) -> bool {
    matches!(usage, 0x10030 | 0x10031 | 0x10038 | 0xc0238)
        || (usage >> 16 == 12 && CONSUMER_AXES.contains(&(usage as u16)))
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
    if page == 1 && SYSTEM_USAGES.contains(&(code as u16)) {
        return SYSTEM;
    }
    if page == 1 && matches!(code, 0xc6 | 0xc8) {
        return RADIO;
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
    if page == 12 && (1..=65535).contains(&code) {
        return CONSUMER;
    }
    0
}
/// Consumer-page on/off controls use relative toggle or explicit on/off reports.
pub const CONSUMER_SWITCHES: [u16; 35] = [
    0x30, 0x35, 0x40, 0x60, 0x61, 0x63, 0x72, 0x75, 0x76, 0x77, 0x78, 0x7c, 0x7f, 0x95, 0xb0, 0xb1,
    0xb2, 0xb3, 0xb4, 0xb9, 0xc4, 0xd8, 0xd9, 0xe2, 0xe5, 0xe7, 0xe8, 0x100, 0x102, 0x104, 0x106,
    0x2d0, 0x500, 0x501, 0x502,
];
fn numeric_value(minimum: i32, bytes: &[u8], bit: usize, size: usize) -> i64 {
    let at = |i: usize| (bytes[(bit + i) / 8] >> ((bit + i) % 8)) & 1;
    let mut low = 0u32;
    for i in 0..size.min(32) {
        low |= u32::from(at(i)) << i;
    }
    if size <= 32 {
        if minimum < 0 && at(size - 1) != 0 {
            i64::from(low) - (1i64 << size)
        } else {
            i64::from(low)
        }
    } else {
        let negative = minimum < 0 && at(size - 1) != 0;
        if (32..size).any(|i| (at(i) != 0) != negative) {
            return if negative { i64::MIN } else { i64::MAX };
        }
        if negative {
            if low & (1 << 31) == 0 {
                i64::MIN
            } else {
                i64::from(low as i32)
            }
        } else {
            i64::from(low)
        }
    }
}
fn field_value(field: &Field, bytes: &[u8], index: usize) -> i64 {
    numeric_value(
        field.minimum,
        bytes,
        field.bit() + index * field.size(),
        field.size(),
    )
}
// Admission scratch lives on the owner stack. Only the exact retained map is
// copied to the heap, without interleaving vector growth with raw descriptors.
#[derive(Default)]
struct Compiler {
    fields: heapless::Vec<Field, FIELDS>,
    output_fields: heapless::Vec<OutputField, FIELDS>,
    battery: heapless::Vec<BatteryField, 16>,
    usages: heapless::Vec<Span, USAGE_SPANS>,
    reports: heapless::Vec<Layout, REPORTS>,
    roles: u8,
    hidpp_reports: u8,
    numbered: bool,
    limitations: heapless::Vec<InputLimitation, FIELDS>,
    scales: heapless::Vec<IndicatorScale, FIELDS>,
    state_bits: usize,
    array_fields: usize,
    indicator_units: usize,
    retained_values: usize,
    numeric_values: usize,
}
fn zeros(length: usize) -> Result<Box<[u8]>, Error> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| Error::Capacity)?;
    bytes.resize(length, 0);
    Ok(bytes.into_boxed_slice())
}
fn push<T, const N: usize>(values: &mut heapless::Vec<T, N>, value: T) -> Result<(), Error> {
    values.push(value).map_err(|_| Error::Limit)
}
impl Map {
    fn section<T: Copy>(&self, index: usize) -> &[T] {
        let section = self.sections[index];
        // Compilation copies this section's exact T values at an eight-byte
        // boundary. All section types have alignment at most eight, contain no
        // references, and retain their initialized values for this owner's life.
        unsafe {
            core::slice::from_raw_parts(
                self.storage
                    .as_ptr()
                    .cast::<u8>()
                    .add(usize::from(section.offset))
                    .cast::<T>(),
                usize::from(section.length),
            )
        }
    }
    fn fields(&self) -> &[Field] {
        self.section(0)
    }
    fn output_fields(&self) -> &[OutputField] {
        self.section(1)
    }
    pub(crate) fn indicator_field_count(&self) -> usize {
        self.output_fields().len()
    }
    fn battery_fields(&self) -> &[BatteryField] {
        self.section(2)
    }
    fn usages(&self) -> &[Span] {
        self.section(3)
    }
    pub fn reports(&self) -> &[Layout] {
        self.section(4)
    }
    pub fn limitations(&self) -> &[InputLimitation] {
        self.section(5)
    }
    fn scales(&self) -> &[IndicatorScale] {
        self.section(6)
    }

    /// Allocate once at admission; decoding and clearing never allocate.
    pub fn state(&self) -> Result<State, Error> {
        let reports = zeros(self.reports().iter().map(held_bytes).sum())?;
        let mut arrays = Vec::new();
        arrays
            .try_reserve_exact(self.array_fields)
            .map_err(|_| Error::Capacity)?;
        arrays.resize(self.array_fields, Held::default());
        Ok(State {
            reports,
            arrays: arrays.into_boxed_slice(),
            previous: zeros(self.state_bits.div_ceil(8))?,
            latches: zeros(self.state_bits.div_ceil(8))?,
        })
    }
    pub fn battery_reports(&self) -> impl Iterator<Item = (u8, crate::bluetooth::ReportType)> + '_ {
        self.reports().iter().enumerate().flat_map(move |(i, r)| {
            [0, 2].into_iter().filter_map(move |kind| {
                self.battery_fields()
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
        mut emit: impl FnMut(u8, crate::model::info::InfoKey, crate::model::settings::SettingValue),
    ) {
        use crate::model::{info::InfoKey as K, settings::SettingValue as V};
        let kind = match kind {
            crate::bluetooth::ReportType::Input => 0,
            crate::bluetooth::ReportType::Feature => 2,
            _ => return,
        };
        for instance in 0..5 {
            let mut has_charging = false;
            let mut charging = None;
            let mut explicit_charging = None;
            for f in self.battery_fields().iter().filter(|f| {
                self.reports()[f.report as usize].id == report
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

    pub fn compile(descriptor: &[u8]) -> Result<Self, Error> {
        Compiler::compile(descriptor)
    }
}
#[derive(Clone, Copy)]
struct CollectionContext {
    app: u8,
    instance: u16,
    scope: u16,
    group: u16,
    mode: u8,
    unit: u16,
    indicator: u8,
    battery_instance: u8,
}
fn walk_descriptor(
    descriptor: &[u8],
    mut emit: impl FnMut(&Global, &[Span], CollectionContext, usize, u32) -> Result<(), Error>,
) -> Result<bool, Error> {
    if descriptor.is_empty() {
        return Err(Error::Invalid);
    }
    if descriptor.len() > DESCRIPTOR_BYTES {
        return Err(Error::Limit);
    }
    let mut numbered = false;
    let mut g = Global::default();
    let mut globals = [Global::default(); 8];
    let mut depth = 0;
    let mut local = heapless::Vec::<Span, USAGE_SPANS>::new();
    let mut pending = false;
    let mut apps = [0; 16];
    let mut instances = [0; 16];
    let mut scopes = [0; 16];
    let mut scope = 0u16;
    let mut next_scope = 0u16;
    let mut indicators = [0; 16];
    let mut groups = [0; 16];
    let mut modes = [0; 16];
    let mut units = [0; 16];
    let mut group = 0u16;
    let mut mode = 0u8;
    let mut unit = 0u16;
    let mut instance = 0u16;
    let mut next_instance = 0u16;
    let mut indicator = 0u8;
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
                3 => {
                    g.physical_minimum = signed(value, bytes);
                    g.physical_bounds |= 1;
                }
                4 => {
                    g.physical_maximum = value;
                    g.physical_maximum_size = bytes;
                    g.physical_bounds |= 2;
                }
                5 => g.exponent = ((value as i8) << 4) >> 4,
                6 => g.unit = value,
                7 => g.size = value,
                8 => {
                    if !(1..=255).contains(&value) {
                        return Err(Error::Invalid);
                    }
                    g.id = value as u8;
                    numbered = true;
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
                        instances[collection] = instance;
                        scopes[collection] = scope;
                        next_scope = next_scope.checked_add(1).ok_or(Error::Limit)?;
                        indicators[collection] = indicator;
                        groups[collection] = group;
                        modes[collection] = mode;
                        units[collection] = unit;
                        battery_instances[collection] = battery_instance;
                        if usage_at(&local, 0, false) == 0x840012 {
                            battery_instance = next_battery;
                            next_battery = next_battery.saturating_add(1);
                        }
                        collection += 1;
                        if value == 1 {
                            next_instance = next_instance.checked_add(1).ok_or(Error::Limit)?;
                            instance = next_instance;
                            indicator = 0;
                            scope = 0;
                            group = 0;
                            mode = 0;
                            unit = 0;
                            let u = usage_at(&local, 0, false);
                            app = match u {
                                0x10006 | 0x10007 => KEYBOARD,
                                0x10002 => MOUSE,
                                0xc0001 => CONSUMER,
                                0x10080 => SYSTEM_APPLICATION,
                                0x1000c => RADIO_APPLICATION,
                                0xff430202 => HIDPP_BLUETOOTH_APPLICATION,
                                _ if u >> 16 == 0xff00 => HIDPP_APPLICATION,
                                _ => 0,
                            };
                        }
                        let usage = usage_at(&local, 0, false);
                        if value == 0 || (0x80001..=0x80005).contains(&usage) {
                            scope = next_scope;
                        }
                        if (0x80001..=0x80005).contains(&usage) {
                            indicator = usage as u8;
                        }
                        let modifier = match usage {
                            0x8003c => 1,
                            0x80047 => 2,
                            0x80052 => 3,
                            0x8003a | 0x8003b => 4,
                            _ => 0,
                        };
                        if modifier != 0 {
                            mode = modifier;
                            group = next_scope;
                        }
                        if modifier == 3 {
                            unit = next_scope;
                        }
                        if usage >> 16 == 0xa && usage & 0xffff != 0 && mode != 0 {
                            group = next_scope;
                            unit = next_scope;
                        }
                    }
                    12 => {
                        if bytes != 0 || collection == 0 {
                            return Err(Error::Invalid);
                        }
                        collection -= 1;
                        app = apps[collection];
                        instance = instances[collection];
                        scope = scopes[collection];
                        indicator = indicators[collection];
                        group = groups[collection];
                        mode = modes[collection];
                        unit = units[collection];
                        battery_instance = battery_instances[collection];
                    }
                    8 | 9 | 11 => {
                        if collection == 0 {
                            return Err(Error::Invalid);
                        }
                        emit(
                            &g,
                            &local,
                            CollectionContext {
                                app,
                                instance,
                                scope,
                                group,
                                mode,
                                unit,
                                indicator,
                                battery_instance,
                            },
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
    Ok(numbered)
}
impl Compiler {
    fn compile(descriptor: &[u8]) -> Result<Map, Error> {
        let mut indicator_reports = [0u8; 256];
        let mut numeric_indicators = false;
        walk_descriptor(descriptor, |g, local, context, kind, flags| {
            numeric_indicators |= kind != 0 && context.mode == 3 && flags & 7 == 6;
            if kind != 0
                && flags & 1 == 0
                && g.count != 0
                && g.size != 0
                && (context.indicator != 0
                    || context.mode == 4
                    || local
                        .iter()
                        .any(|span| span.first() <= 0x80005 && span.last() >= 0x80001))
            {
                indicator_reports[usize::from(g.id)] |= 1 << kind;
            }
            Ok(())
        })?;
        let mut map = Self::default();
        map.numbered = walk_descriptor(descriptor, |g, local, context, kind, flags| {
            map.compile_field(
                g,
                local,
                context,
                kind,
                flags,
                &indicator_reports,
                numeric_indicators,
            )
        })?;
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
            let sizes = [
                core::mem::size_of_val(map.fields.as_slice()),
                core::mem::size_of_val(map.output_fields.as_slice()),
                core::mem::size_of_val(map.battery.as_slice()),
                core::mem::size_of_val(map.usages.as_slice()),
                core::mem::size_of_val(map.reports.as_slice()),
                core::mem::size_of_val(map.limitations.as_slice()),
                core::mem::size_of_val(map.scales.as_slice()),
            ];
            let mut storage = Vec::new();
            let words: usize = sizes.iter().map(|bytes| bytes.div_ceil(8)).sum();
            storage
                .try_reserve_exact(words)
                .map_err(|_| Error::Capacity)?;
            storage.resize(words, core::mem::MaybeUninit::<u64>::zeroed());
            let mut sections = [Section::default(); 7];
            let mut offset = 0usize;
            let mut retain =
                |index: usize, source: *const u8, length: usize| -> Result<(), Error> {
                    sections[index] = Section {
                        offset: offset.try_into().map_err(|_| Error::Limit)?,
                        length: length.try_into().map_err(|_| Error::Limit)?,
                    };
                    // Destination is aligned, reserved in full, and disjoint from
                    // the owner's typed admission scratch. The getters use exactly
                    // the source section's type and element count.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            source,
                            storage.as_mut_ptr().cast::<u8>().add(offset),
                            sizes[index],
                        );
                    }
                    offset += sizes[index].div_ceil(8) * 8;
                    Ok(())
                };
            retain(0, map.fields.as_ptr().cast(), map.fields.len())?;
            retain(
                1,
                map.output_fields.as_ptr().cast(),
                map.output_fields.len(),
            )?;
            retain(2, map.battery.as_ptr().cast(), map.battery.len())?;
            retain(3, map.usages.as_ptr().cast(), map.usages.len())?;
            retain(4, map.reports.as_ptr().cast(), map.reports.len())?;
            retain(5, map.limitations.as_ptr().cast(), map.limitations.len())?;
            retain(6, map.scales.as_ptr().cast(), map.scales.len())?;
            Ok(Map {
                storage: storage.into_boxed_slice(),
                sections,
                roles: map.roles,
                hidpp_reports: map.hidpp_reports,
                numbered: map.numbered,
                state_bits: map.state_bits,
                array_fields: map.array_fields,
                indicator_units: map.indicator_units,
                retained_values: map.retained_values,
                numeric_values: map.numeric_values,
            })
        }
    }
    fn store_usages(&mut self, local: &[Span]) -> Result<u8, Error> {
        if local.is_empty() {
            return Ok(0);
        }
        if let Some(index) = self
            .usages
            .windows(local.len())
            .position(|spans| spans == local)
        {
            return Ok(index as u8);
        }
        let index = self.usages.len();
        self.usages
            .extend_from_slice(local)
            .map_err(|_| Error::Limit)?;
        Ok(index as u8)
    }
    #[allow(clippy::too_many_arguments)]
    fn compile_field(
        &mut self,
        g: &Global,
        local: &[Span],
        context: CollectionContext,
        kind: usize,
        flags: u32,
        indicator_reports: &[u8; 256],
        numeric_indicators: bool,
    ) -> Result<(), Error> {
        let CollectionContext {
            app,
            battery_instance,
            ..
        } = context;
        // A 1-bit output or feature field, such as an LED after an 8-bit key array, is on or off
        // even when it inherits a wider logical maximum, as hosts read it. Wider scalar fields
        // must fit their declared range.
        let narrowed;
        let g = if kind != 0 && g.size == 1 && g.min == 0 && g.max > 1 {
            narrowed = Global { max: 1, ..*g };
            &narrowed
        } else {
            g
        };
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
                        key_first: 255,
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
        if flags & 1 != 0 {
            return Ok(());
        }
        let has_leds = local
            .iter()
            .any(|s| s.first() <= 0x80005 && s.last() >= 0x80001);
        let mut stored_spans = None;
        if (kind != 0 && indicator_reports[usize::from(g.id)] & (1 << kind) != 0)
            || has_leds
            || context.indicator != 0
            || context.mode == 4
        {
            if flags & 0x100 == 0 && validate_scalar(g).is_err() {
                if kind == 0 {
                    return Err(Error::Invalid);
                }
                // An output or feature indicator whose range does not fit its field is left
                // unused; the rest of the device works. A write keeps an absolute field's value
                // from a fresh read, which for a relative field would repeat its last change.
                if flags & 4 == 0 {
                    self.reports[ri].preserved |= 1 << kind;
                } else {
                    self.reports[ri].unwritable |= 1 << kind;
                }
                let usage = usage_at(local, 0, true);
                return push(
                    &mut self.limitations,
                    InputLimitation {
                        code: crate::model::errors::WarningCode::IndicatorRangeUnsupported,
                        kind: kind as u8,
                        report_id: g.id,
                        bit_offset: offset,
                        usage_page: (usage >> 16) as u16,
                        usage: usage as u16,
                    },
                );
            }
            report.leds |= kind == 1 && (has_leds || context.indicator != 0 || context.mode == 4);
            let span = self.store_usages(local)?;
            let cached = kind != 0
                && flags & 4 != 0
                && g.min >= 0
                && (has_leds || context.indicator != 0 || context.mode == 4);
            let state = if cached {
                self.indicator_units.try_into().map_err(|_| Error::Limit)?
            } else {
                0
            };
            let retained = self.retained_values.try_into().map_err(|_| Error::Limit)?;
            if kind != 0 && matches!(context.mode, 2 | 3) && !(context.mode == 3 && flags & 7 == 6)
            {
                self.retained_values += g.count as usize;
            }
            let numeric = self.numeric_values.try_into().map_err(|_| Error::Limit)?;
            if kind != 0 && context.mode == 3 && flags & 7 == 6 {
                self.numeric_values += g.count as usize;
            }
            let scale = if numeric_indicators && context.mode == 3 {
                let index = self.scales.len() as u16;
                let minimum = if g.physical_bounds == 3 {
                    i64::from(g.physical_minimum)
                } else {
                    0
                };
                let maximum = if g.physical_bounds != 3 {
                    0
                } else if minimum < 0 {
                    i64::from(signed(g.physical_maximum, g.physical_maximum_size))
                } else {
                    i64::from(g.physical_maximum)
                };
                push(
                    &mut self.scales,
                    IndicatorScale {
                        minimum,
                        maximum,
                        unit: g.unit,
                        exponent: g.exponent,
                    },
                )?;
                index
            } else {
                u16::MAX
            };
            if cached {
                self.indicator_units += if flags & 2 != 0 {
                    g.count as usize
                } else if context.mode == 4 {
                    8
                } else {
                    5
                };
            }
            push(
                &mut self.output_fields,
                OutputField {
                    collection: context.instance,
                    scope: context.scope,
                    state,
                    retained,
                    numeric,
                    scale,
                    group: context.group,
                    unit: context.unit,
                    target: context.indicator,
                    mode: context.mode,
                    dark: dark_value(g),
                    kind: kind as u8,
                    bit: offset,
                    size: g.size as u16,
                    count: g.count as u16,
                    span,
                    spans: local.len() as u8,
                    report: ri as u8,
                    flags: flags as u16 | if cached { 0x8000 } else { 0 },
                    minimum: g.min,
                    maximum: if g.min < 0 {
                        i64::from(signed(g.max, g.max_size))
                    } else {
                        i64::from(g.max)
                    },
                },
            )?;
            stored_spans = Some(span);
            if kind != 0 {
                return Ok(());
            }
        }
        if kind != 0 {
            return Ok(());
        }
        let variable = flags & 2 != 0;
        let relative = flags & 4 != 0;
        let mut roles = 0;
        let mut unsupported = None;
        for span in local {
            let page = span.first() >> 16;
            let cap = match page {
                7 => 255,
                9 => 16,
                12 => 65535,
                1 => 0xca,
                8 => 5,
                _ => 0,
            };
            let end = ((page << 16) | cap).min(span.last());
            if cap != 0 {
                for usage in span.first()..=end {
                    let role = usage_kind(usage, app, kind == 1, relative);
                    roles |= role;
                    if role != 0 && kind == 0 {
                        let desktop = app == MOUSE && matches!(usage, 0x10030 | 0x10031 | 0x10038);
                        if desktop && !variable {
                            unsupported = Some((
                                crate::model::errors::WarningCode::PointerSelectorUnsupported,
                                usage,
                            ));
                        }
                        let axis = desktop || (usage == 0xc0238 && variable && relative);
                        // A usage range that only spans a numeric control, as a keyboard's
                        // Consumer array often does, describes a block of codes; only a numeric
                        // usage the device lists by itself is one it means to select.
                        if !axis
                            && !variable
                            && page == 12
                            && span.first() == span.last()
                            && CONSUMER_AXES.contains(&(usage as u16))
                        {
                            unsupported = Some((
                                crate::model::errors::WarningCode::NumericSelectorUnsupported,
                                usage,
                            ));
                        }
                    }
                }
            }
        }
        if roles == 0 {
            return Ok(());
        }
        self.roles |= roles;
        self.reports[ri].held_roles |= roles;
        if variable && flags & 0x100 != 0 {
            unsupported = Some((
                crate::model::errors::WarningCode::BufferedInputUnsupported,
                usage_at(local, 0, true),
            ));
        }
        if let Some((code, usage)) = unsupported {
            push(
                &mut self.limitations,
                InputLimitation {
                    code,
                    kind: 0,
                    report_id: g.id,
                    bit_offset: offset,
                    usage_page: (usage >> 16) as u16,
                    usage: usage as u16,
                },
            )?;
            if variable {
                return Ok(());
            }
        }
        validate_scalar(g)?;
        self.roles |= roles;
        let span = match stored_spans {
            Some(span) => span,
            None => self.store_usages(local)?,
        };
        debug_assert!(app < 8);
        let history = variable && (relative || flags & 0x40 != 0);
        if roles & KEYBOARD != 0 {
            let report = &mut self.reports[ri];
            if variable {
                for j in 0..g.count {
                    let usage = usage_at(local, j, true);
                    if (0x70001..=0x700ff).contains(&usage) {
                        report.key_first = report.key_first.min(usage as u8);
                        report.key_last = report.key_last.max(usage as u8);
                    }
                }
            } else {
                for span in local
                    .iter()
                    .filter(|s| s.page == 7 && s.last != 0 && s.first <= 255)
                {
                    report.key_first = report.key_first.min(span.first.max(1) as u8);
                    report.key_last = report.key_last.max(span.last.min(255) as u8);
                }
            }
        }
        let field = Field {
            location: u32::from(offset)
                | ((g.count - 1) << 12)
                | (u32::from(history) << 24)
                | (u32::from(app) << 29),
            span,
            spans: local.len() as u8,
            report: ri as u8,
            flags: flags as u8,
            minimum: g.min,
            maximum: if g.min < 0 {
                signed(g.max, g.max_size) as u32
            } else {
                g.max
            },
            state: if history {
                self.state_bits.try_into().map_err(|_| Error::Limit)?
            } else if relative && !variable {
                self.array_fields.try_into().map_err(|_| Error::Limit)?
            } else {
                0
            },
            size: g.size as u16,
        };
        if field.history() {
            self.state_bits += g.count as usize;
        }
        if relative && !variable {
            self.array_fields += 1;
        }
        push(&mut self.fields, field)?;
        Ok(())
    }
}
impl Map {
    /// Whether an input field can represent a standard usage on this connection.
    pub fn supports_usage(&self, usage: u32) -> bool {
        self.fields().iter().any(|f| {
            if f.flags & 1 != 0 || usage_kind(usage, f.app(), false, f.flags & 4 != 0) == 0 {
                return false;
            }
            let spans =
                &self.usages()[usize::from(f.span)..usize::from(f.span) + usize::from(f.spans)];
            if f.flags & 2 != 0 {
                (0..f.count()).any(|i| usage_at(spans, i as u32, true) == usage)
            } else {
                let maximum = if f.minimum < 0 {
                    i64::from(f.maximum as i32)
                } else {
                    i64::from(f.maximum)
                };
                selector_value(spans, usage, f.minimum)
                    .is_some_and(|v| v <= maximum && (f.size >= 32 || v < (1i64 << f.size)))
            }
        })
    }

    pub fn decode(&self, state: &mut State, report_id: u8, payload: &[u8]) -> Result<Input, Error> {
        if state.reports.len() != self.reports().iter().map(held_bytes).sum::<usize>() {
            return Err(Error::Invalid);
        }
        let ri = self
            .reports()
            .iter()
            .position(|r| r.id == report_id)
            .ok_or(Error::Invalid)?;
        let bits = self.reports()[ri].bits[0] as usize;
        if bits == 0 || payload.len() < bits.div_ceil(8) || payload.len() > REPORT_BYTES {
            return Err(Error::Invalid);
        }
        let mut next = Input::default();
        let held_offset: usize = self.reports()[..ri].iter().map(held_bytes).sum();
        let previous_held = load_held(
            &state.reports[held_offset..held_offset + held_bytes(&self.reports()[ri])],
            &self.reports()[ri],
        );
        let slider_known = |usage| match usage {
            0x100c8 => previous_held.radio & 4 != 0,
            0x100ca => previous_held.system & ROTATION_KNOWN != 0,
            _ => true,
        };
        let previous_value = |f: &Field, j: usize| {
            if !f.history() {
                return 0;
            }
            let bit = usize::from(f.state) + j;
            i64::from(state.previous[bit / 8] & (1 << (bit % 8)) != 0)
        };
        for field in self.fields() {
            if field.report as usize != ri || field.flags & 1 != 0 {
                continue;
            }
            let variable = field.flags & 2 != 0;
            let relative = field.flags & 4 != 0;
            let mut selected = Input::default();
            for j in 0..field.count() {
                let mut value = field_value(field, payload, j);
                let maximum = if field.minimum < 0 {
                    i64::from(field.maximum as i32)
                } else {
                    i64::from(field.maximum)
                };
                if value < i64::from(field.minimum) || value > maximum {
                    if !variable {
                        continue;
                    }
                    if field.flags & 0x40 != 0 {
                        let spans = &self.usages()[usize::from(field.span)
                            ..usize::from(field.span) + usize::from(field.spans)];
                        let usage = usage_at(spans, j as u32, true);
                        if !slider_known(usage) {
                            continue;
                        }
                        let sliders = next.sliders;
                        let numeric = matches!(usage, 0x10030 | 0x10031 | 0x10038 | 0xc0238)
                            || (usage >> 16 == 12
                                && (CONSUMER_AXES.contains(&(usage as u16))
                                    || CONSUMER_SWITCHES.contains(&(usage as u16))));
                        if relative
                            && field.minimum < 0
                            && !numeric
                            && usage_kind(usage, field.app(), false, true) != 0
                        {
                            let bit = usize::from(field.state) + j;
                            next.set_usage(
                                usage,
                                i64::from(state.latches[bit / 8] & (1 << (bit % 8)) != 0),
                                false,
                            )?;
                        } else if !relative
                            && !(usage >> 16 == 12 && CONSUMER_AXES.contains(&(usage as u16)))
                            && !matches!(usage, 0x10030 | 0x10031 | 0x10038)
                        {
                            next.set_usage(usage, previous_value(field, j), false)?;
                        }
                        next.sliders = sliders;
                        continue;
                    }
                    value = value.clamp(i64::from(field.minimum), maximum);
                }
                let spans =
                    &self.usages()[field.span as usize..(field.span + field.spans) as usize];
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
                if !variable && numeric_selector(usage) {
                    continue;
                }
                if relative && !variable {
                    if held_usage(&state.arrays[usize::from(field.state)], usage)
                        || held_usage(&selected.held, usage)
                    {
                        continue;
                    }
                    selected.set_usage(usage, 1, false)?;
                    if usage >> 16 == 12
                        && let Some(index) =
                            CONSUMER_SWITCHES.iter().position(|&u| u == usage as u16)
                    {
                        next.switch(index, 1, false);
                    } else {
                        next.pulse(usage)?;
                    }
                    continue;
                }
                let old = previous_value(field, j);
                if !relative && variable && matches!(usage, 0x10030 | 0x10031 | 0x10038) {
                    let axis = match usage {
                        0x10030 => 0,
                        0x10031 => 1,
                        _ => 2,
                    };
                    next.position[axis] =
                        Some(normalized(value, i64::from(field.minimum), maximum));
                } else if !relative
                    && variable
                    && usage >> 16 == 12
                    && let Some(index) = CONSUMER_AXES.iter().position(|&u| u == usage as u16)
                {
                    next.consumer_values[index] =
                        Some(normalized(value, i64::from(field.minimum), maximum));
                } else if relative
                    && usage >> 16 == 12
                    && let Some(index) = CONSUMER_SWITCHES.iter().position(|&u| u == usage as u16)
                {
                    if value != 0 && old == 0 {
                        next.switch(index, if value > 0 { 1 } else { -1 }, field.minimum < 0);
                    }
                } else if relative
                    && !matches!(usage, 0x10030 | 0x10031 | 0x10038 | 0xc0238)
                    && !(usage >> 16 == 12 && CONSUMER_AXES.contains(&(usage as u16)))
                {
                    if field.minimum < 0 {
                        if value == 0 && !slider_known(usage) {
                            continue;
                        }
                        let bit = usize::from(field.state) + j;
                        let sliders = next.sliders;
                        let latched = state.latches[bit / 8] & (1 << (bit % 8)) != 0;
                        next.set_usage(
                            usage,
                            i64::from(if value == 0 { latched } else { value > 0 }),
                            false,
                        )?;
                        if value == 0 {
                            next.sliders = sliders;
                        }
                    } else if value != 0 && old == 0 {
                        next.pulse(usage)?;
                    }
                } else {
                    // Two-bit consumer linear controls describe button edges.
                    let edge =
                        relative && usage >> 16 == 12 && field.minimum == -1 && field.maximum == 1;
                    if !edge || old == 0 {
                        next.set_usage(usage, if variable { value } else { 1 }, relative)?;
                    }
                }
            }
        }
        for f in self
            .fields()
            .iter()
            .filter(|f| usize::from(f.report) == ri && f.flags & 7 == 4)
        {
            let spans =
                &self.usages()[usize::from(f.span)..usize::from(f.span) + usize::from(f.spans)];
            let mut selected = Input::default();
            let maximum = if f.minimum < 0 {
                i64::from(f.maximum as i32)
            } else {
                i64::from(f.maximum)
            };
            for j in 0..f.count() {
                let value = field_value(f, payload, j);
                if value < i64::from(f.minimum) || value > maximum {
                    continue;
                }
                let usage = usage_at(spans, (value - i64::from(f.minimum)) as u32, false);
                if usage_kind(usage, f.app(), false, true) != 0 && !numeric_selector(usage) {
                    selected.set_usage(usage, 1, false)?;
                }
            }
            state.arrays[usize::from(f.state)] = selected.held;
        }
        for f in self
            .fields()
            .iter()
            .filter(|f| usize::from(f.report) == ri && f.flags & 7 == 6 && f.minimum < 0)
        {
            let spans =
                &self.usages()[usize::from(f.span)..usize::from(f.span) + usize::from(f.spans)];
            for j in 0..f.count() {
                let usage = usage_at(spans, j as u32, true);
                if usage_kind(usage, f.app(), false, true) == 0
                    || matches!(usage, 0x10030 | 0x10031 | 0x10038 | 0xc0238)
                    || (usage >> 16 == 12
                        && (CONSUMER_AXES.contains(&(usage as u16))
                            || CONSUMER_SWITCHES.contains(&(usage as u16))))
                {
                    continue;
                }
                let value = field_value(f, payload, j);
                if f.flags & 0x40 != 0
                    && (value < i64::from(f.minimum) || value > i64::from(f.maximum as i32))
                {
                    continue;
                }
                let value = value.clamp(i64::from(f.minimum), i64::from(f.maximum as i32));
                let bit = usize::from(f.state) + j;
                let mask = 1 << (bit % 8);
                if value > 0 {
                    state.latches[bit / 8] |= mask;
                }
                if value < 0 {
                    state.latches[bit / 8] &= !mask;
                }
            }
        }
        for f in self
            .fields()
            .iter()
            .filter(|f| usize::from(f.report) == ri && f.history() && f.flags & 3 == 2)
        {
            let maximum = if f.minimum < 0 {
                i64::from(f.maximum as i32)
            } else {
                i64::from(f.maximum)
            };
            for j in 0..f.count() {
                let value = field_value(f, payload, j);
                if (value < i64::from(f.minimum) || value > maximum) && f.flags & 0x40 != 0 {
                    continue;
                }
                let bit = usize::from(f.state) + j;
                let mask = 1 << (bit % 8);
                if value.clamp(i64::from(f.minimum), maximum) != 0 {
                    state.previous[bit / 8] |= mask;
                } else {
                    state.previous[bit / 8] &= !mask;
                }
            }
        }
        // This report is valid on its own. Retain its releases even when other
        // report IDs temporarily put the aggregate over the consumer limit.
        store_held(
            &mut state.reports[held_offset..held_offset + held_bytes(&self.reports()[ri])],
            &self.reports()[ri],
            &next.held,
        );
        let mut offset = 0;
        for (i, report) in self.reports().iter().enumerate() {
            let length = held_bytes(report);
            if i != ri {
                next.held = next
                    .held
                    .union(&load_held(&state.reports[offset..offset + length], report))?;
            }
            offset += length;
        }
        Ok(next)
    }
    pub fn led_report(&self, ri: usize, leds: u8, payload: &mut [u8]) -> Option<usize> {
        self.indicator_report(ri, leds, IndicatorValue::default(), None, false, payload)
            .ok()
            .flatten()
            .map(|u| u.length)
    }
    /// Encodes indicators while preserving unrelated nonvolatile output values.
    /// A baseline is a fresh GET_REPORT result, not an earlier write buffer.
    pub fn indicator_report(
        &self,
        ri: usize,
        leds: u8,
        confirmed: IndicatorValue,
        baseline: Option<&[u8]>,
        rearm: bool,
        payload: &mut [u8],
    ) -> Result<Option<IndicatorEncoding>, IndicatorFailure> {
        self.encode_indicators(
            ri,
            1,
            leds,
            |_, _, _| confirmed,
            None,
            baseline,
            rearm,
            payload,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn indicator_report_scoped(
        &self,
        ri: usize,
        kind: crate::bluetooth::ReportType,
        leds: u8,
        cache: &IndicatorCache,
        baseline: Option<&[u8]>,
        rearm: bool,
        payload: &mut [u8],
    ) -> Result<Option<IndicatorEncoding>, IndicatorFailure> {
        self.encode_indicators(
            ri,
            report_kind(kind),
            leds,
            |f, j, usage| {
                let slot = usize::from(f.state) + j;
                let value = if f.flags & 0x8000 != 0 {
                    cache.get(slot)
                } else {
                    0
                };
                let mask = 1 << (usage - 0x80001);
                IndicatorValue {
                    bits: if value & 2 != 0 { mask } else { 0 },
                    known: if value & 1 != 0 { mask } else { 0 },
                }
            },
            Some(cache),
            baseline,
            rearm,
            payload,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn encode_indicators(
        &self,
        ri: usize,
        kind: u8,
        leds: u8,
        mut confirmed: impl FnMut(&OutputField, usize, u32) -> IndicatorValue,
        cache: Option<&IndicatorCache>,
        baseline: Option<&[u8]>,
        rearm: bool,
        payload: &mut [u8],
    ) -> Result<Option<IndicatorEncoding>, IndicatorFailure> {
        let Some(report) = self.reports().get(ri).filter(|_| {
            self.output_fields().iter().any(|f| {
                usize::from(f.report) == ri && f.kind == kind && self.field_targets(f) != 0
            })
        }) else {
            return Ok(None);
        };
        let length = usize::from(report.bits[usize::from(kind)]).div_ceil(8);
        if payload.len() < length {
            return Err(IndicatorFailure {
                reason: IndicatorError::Range,
                bit_offset: None,
                usage: None,
            });
        }
        let baseline = baseline.filter(|b| b.len() >= length);
        if report.unwritable & (1 << kind) != 0 {
            return Err(IndicatorFailure {
                reason: IndicatorError::Range,
                bit_offset: None,
                usage: None,
            });
        }
        if report.preserved & (1 << kind) != 0 && baseline.is_none() {
            return Err(IndicatorFailure {
                reason: IndicatorError::ReadRequired,
                bit_offset: None,
                usage: None,
            });
        }
        let mut applied = 0;
        let mut unknown = 0;
        let mut complete = true;
        payload[..length].fill(0);
        if let Some(base) = baseline.filter(|b| b.len() >= length) {
            payload[..length].copy_from_slice(&base[..length]);
        }
        for f in self
            .output_fields()
            .iter()
            .filter(|f| usize::from(f.report) == ri && f.kind == kind)
        {
            let spans =
                &self.usages()[usize::from(f.span)..usize::from(f.span) + usize::from(f.spans)];
            let failure = |reason| IndicatorFailure {
                reason,
                bit_offset: Some(f.bit),
                usage: Some(usage_at(spans, 0, false)),
            };
            let variable = f.flags & 2 != 0;
            let relative = f.flags & 4 != 0;
            if f.target != 0 && matches!(f.mode, 1..=3) && self.field_targets(f) != 0 {
                complete &= self
                    .encode_modified_indicator(
                        f,
                        leds & (1 << (f.target - 1)) != 0,
                        cache,
                        baseline,
                        rearm,
                        payload,
                    )
                    .map_err(failure)?;
                applied |= 1 << (f.target - 1);
                continue;
            }
            if variable && f.flags & 0x100 != 0 {
                if f.flags & 0x84 != 0
                    || spans
                        .iter()
                        .any(|s| s.first() <= 0x80005 && s.last() >= 0x80001)
                {
                    return Err(failure(IndicatorError::Buffered));
                }
                if baseline.is_none() {
                    return Err(failure(IndicatorError::ReadRequired));
                }
                continue;
            }
            if !variable {
                let has_leds = self.field_targets(f) != 0;
                if !has_leds {
                    self.neutral_output(f, baseline, payload).map_err(failure)?;
                    continue;
                }
                let mixed = spans.iter().any(|s| {
                    (s.first()..=s.last())
                        .any(|usage| usage & 0xffff != 0 && lock_usage(f, usage).is_none())
                });
                if mixed && !relative && baseline.is_none() {
                    return Err(failure(IndicatorError::ReadRequired));
                }
                let empty = self.empty_selector(f);
                let mut used = 0usize;
                let mut append = |value: i64| -> Result<(), IndicatorError> {
                    if used == usize::from(f.count) {
                        return Err(IndicatorError::ArrayCapacity);
                    }
                    put_value(
                        payload,
                        usize::from(f.bit) + used * usize::from(f.size),
                        usize::from(f.size),
                        value,
                    );
                    used += 1;
                    Ok(())
                };
                if let Some(base) = baseline.filter(|_| mixed && !relative) {
                    for j in 0..usize::from(f.count) {
                        let value = output_value(f, base, j);
                        if value < i64::from(f.minimum) || value > f.maximum {
                            continue;
                        }
                        let usage = usage_at(spans, (value - i64::from(f.minimum)) as u32, false);
                        if usage & 0xffff != 0 && lock_usage(f, usage).is_none() {
                            append(value).map_err(failure)?;
                        }
                    }
                }
                for (slot, raw) in LOCK_SELECTORS.into_iter().enumerate() {
                    if !self.contains_usage(f, raw) {
                        continue;
                    }
                    let Some(usage) = lock_usage(f, raw) else {
                        continue;
                    };
                    if let Some(value) = selector_value(spans, raw, f.minimum) {
                        let mask = 1 << (usage - 0x80001);
                        let current = confirmed(f, slot, usage);
                        let select = if relative {
                            if current.known & mask == 0 {
                                unknown |= mask;
                                false
                            } else {
                                !rearm && ((leds & mask != 0) != (current.bits & mask != 0))
                            }
                        } else {
                            leds & mask != 0
                        };
                        if !select {
                            continue;
                        }
                        if value > f.maximum {
                            return Err(failure(IndicatorError::Range));
                        }
                        append(value).map_err(failure)?;
                    }
                }
                for raw in LOCK_SELECTORS {
                    let Some(usage) = lock_usage(f, raw) else {
                        continue;
                    };
                    let mask = 1 << (usage - 0x80001);
                    if selector_value(spans, raw, f.minimum).is_some() && unknown & mask == 0 {
                        applied |= mask;
                    }
                }
                while used < usize::from(f.count) {
                    let value = empty.ok_or_else(|| failure(IndicatorError::ArrayCapacity))?;
                    put_value(
                        payload,
                        usize::from(f.bit) + used * usize::from(f.size),
                        usize::from(f.size),
                        value,
                    );
                    used += 1;
                }
                continue;
            }
            for j in 0..usize::from(f.count) {
                let raw_usage = usage_at(spans, j as u32, true);
                let usage = lock_usage(f, raw_usage).unwrap_or(raw_usage);
                let bit = usize::from(f.bit) + j * usize::from(f.size);
                if !(0x80001..=0x80005).contains(&usage) {
                    let one = OutputField {
                        bit: bit as u16,
                        count: 1,
                        ..*f
                    };
                    self.neutral_output(&one, baseline, payload)
                        .map_err(|reason| IndicatorFailure {
                            reason,
                            bit_offset: Some(bit as u16),
                            usage: Some(usage),
                        })?;
                    continue;
                }
                let mask = 1 << (usage - 0x80001);
                let on = leds & mask != 0;
                if relative && (f.minimum > 0 || f.maximum < 1) {
                    return Err(IndicatorFailure {
                        reason: IndicatorError::Range,
                        bit_offset: Some(bit as u16),
                        usage: Some(usage),
                    });
                }
                let confirmed = confirmed(f, j, usage);
                let value = if !relative {
                    if f.minimum > 0 || f.maximum < 1 {
                        return Err(IndicatorFailure {
                            reason: IndicatorError::Range,
                            bit_offset: Some(bit as u16),
                            usage: Some(usage),
                        });
                    }
                    if on { 1 } else { 0 }
                } else if rearm {
                    0
                } else if f.minimum <= -1 && f.maximum >= 1 {
                    if on { 1 } else { -1 }
                } else if f.minimum == 0 && f.maximum >= 1 {
                    if confirmed.known & mask == 0 {
                        unknown |= mask;
                        0
                    } else {
                        i64::from((confirmed.bits & mask != 0) != on)
                    }
                } else {
                    return Err(failure(IndicatorError::Range));
                };
                if unknown & mask == 0 {
                    applied |= mask;
                }
                put_value(payload, bit, usize::from(f.size), value);
            }
        }
        Ok(Some(IndicatorEncoding {
            length,
            applied,
            unknown,
            complete,
        }))
    }
    fn same_indicator(a: &OutputField, b: &OutputField) -> bool {
        a.collection == b.collection && a.scope == b.scope && a.target == b.target
    }
    fn same_group(a: &OutputField, b: &OutputField) -> bool {
        a.collection == b.collection && a.group == b.group && a.target == b.target
    }
    fn group_has_usage(&self, f: &OutputField, usage: u32) -> bool {
        self.output_fields().iter().any(|other| {
            other.kind != 0 && Self::same_group(f, other) && self.contains_usage(other, usage)
        })
    }
    fn has_indicator_gate(&self, f: &OutputField) -> bool {
        self.output_fields().iter().any(|gate| {
            gate.kind != 0
                && gate.mode == 1
                && Self::same_indicator(f, gate)
                && (gate.unit == 0 || gate.unit == f.unit)
                && self.group_has_usage(gate, 0x8003d)
                && self.group_has_usage(gate, 0x80041)
        })
    }
    fn current_indicator(f: &OutputField, baseline: Option<&[u8]>, j: usize) -> Option<i64> {
        baseline
            .map(|bytes| output_value(f, bytes, j))
            .filter(|&value| {
                f.flags & 2 == 0 || value >= i64::from(f.minimum) && value <= f.maximum
            })
    }
    fn retained_indicator(
        f: &OutputField,
        cache: Option<&IndicatorCache>,
        j: usize,
    ) -> Option<i64> {
        cache
            .and_then(|cache| cache.retained.get(usize::from(f.retained) + j))
            .map(|value| value[0])
            .filter(|&value| value != i64::MIN)
    }
    fn color_observation(f: &OutputField, cache: Option<&IndicatorCache>, j: usize) -> Option<i64> {
        cache
            .and_then(|cache| cache.retained.get(usize::from(f.retained) + j))
            .map(|value| value[1])
            .filter(|&value| value != i64::MIN)
    }
    fn numeric_peer(&self, f: &OutputField, j: usize) -> Option<(&OutputField, usize)> {
        let usage = usage_at(self.spans(f), j as u32, true);
        let mut matches = self
            .output_fields()
            .iter()
            .filter(|other| {
                matches!(other.kind, 0 | 2)
                    && other.mode == 3
                    && other.flags & 0x107 == 2
                    && Self::same_group(f, other)
            })
            .flat_map(|other| {
                (0..usize::from(other.count)).filter_map(move |k| {
                    (usage_at(self.spans(other), k as u32, true) == usage).then_some((other, k))
                })
            });
        let peer = matches.next()?;
        matches.next().is_none().then_some(peer)
    }
    // The exact ratio maps absolute logical counts to relative logical counts.
    // State uses the numerator's lattice; each submitted delta advances it by
    // the denominator, without rounding intermediate device values.
    fn numeric_ratio(
        &self,
        f: &OutputField,
        peer: &OutputField,
    ) -> Result<(i128, i128), IndicatorError> {
        let relative = self
            .scales()
            .get(usize::from(f.scale))
            .ok_or(IndicatorError::Range)?;
        let absolute = self
            .scales()
            .get(usize::from(peer.scale))
            .ok_or(IndicatorError::Range)?;
        if f.flags & 0x10 != 0 || peer.flags & 0x10 != 0 {
            return Err(IndicatorError::Nonlinear);
        }
        let step = |field: &OutputField, scale: &IndicatorScale| {
            let logical = i128::from(field.maximum - i64::from(field.minimum));
            let physical = if scale.minimum == 0 && scale.maximum == 0 {
                logical
            } else {
                i128::from(scale.maximum - scale.minimum)
            };
            (physical, logical)
        };
        let (rp, rl) = step(f, relative);
        let (ap, al) = step(peer, absolute);
        if [rp, rl, ap, al].iter().any(|&v| v <= 0) {
            return Err(IndicatorError::Scale);
        }
        let mut numerator = ap * rl;
        let mut denominator = al * rp;
        indicator_unit_ratio(
            absolute.unit,
            relative.unit,
            &mut numerator,
            &mut denominator,
        )?;
        let exponent = i32::from(absolute.exponent) - i32::from(relative.exponent);
        if exponent >= 0 {
            multiply_indicator_ratio(
                &mut numerator,
                &mut denominator,
                10i128.pow(exponent as u32),
                1,
            )?;
        } else {
            multiply_indicator_ratio(
                &mut numerator,
                &mut denominator,
                1,
                10i128.pow((-exponent) as u32),
            )?;
        }
        let mut a = numerator;
        let mut b = denominator;
        while b != 0 {
            (a, b) = (b, a % b);
        }
        let numerator = numerator / a;
        for endpoint in [i64::from(peer.minimum), peer.maximum] {
            let value = i128::from(endpoint)
                .checked_mul(numerator)
                .ok_or(IndicatorError::Scale)?;
            if !(i128::MIN / 2..=i128::MAX / 2).contains(&value) {
                return Err(IndicatorError::Scale);
            }
        }
        Ok((numerator, denominator / a))
    }
    fn numeric_primary(&self, f: &OutputField, j: usize) -> bool {
        let usage = usage_at(self.spans(f), j as u32, true);
        let owners = || {
            self.output_fields()
                .iter()
                .filter(|other| other.kind != 0 && other.mode == 3 && Self::same_group(f, other))
                .flat_map(|other| {
                    (0..usize::from(other.count)).filter_map(move |k| {
                        (usage_at(self.spans(other), k as u32, true) == usage).then_some((other, k))
                    })
                })
        };
        if owners().any(|(other, _)| other.flags & 0x107 == 2) {
            return false;
        }
        owners()
            .find(|(other, _)| other.flags & 0x107 == 6)
            .is_some_and(|(other, k)| core::ptr::eq(other, f) && j == k)
    }
    fn rgb_visible(&self, f: &OutputField, cache: &IndicatorCache, saved: bool) -> bool {
        self.output_fields()
            .iter()
            .filter(|other| other.kind != 0 && other.mode == 3 && Self::same_group(f, other))
            .any(|other| {
                (0..usize::from(other.count)).any(|j| {
                    if !(0x80053..=0x80055).contains(&usage_at(self.spans(other), j as u32, true)) {
                        return false;
                    }
                    if other.flags & 7 == 6 {
                        if !self.numeric_primary(other, j) {
                            return false;
                        }
                        let Some((peer, _)) = self.numeric_peer(other, j) else {
                            return false;
                        };
                        let Ok((numerator, _)) = self.numeric_ratio(other, peer) else {
                            return false;
                        };
                        let value =
                            cache.numeric[usize::from(other.numeric) + j][usize::from(!saved)];
                        value != i128::MIN
                            && (peer.dark == i64::MIN || value != i128::from(peer.dark) * numerator)
                    } else {
                        let value = if saved {
                            Self::retained_indicator(other, Some(cache), j)
                        } else {
                            Self::color_observation(other, Some(cache), j)
                        };
                        value.is_some_and(|v| v != other.dark)
                    }
                })
            })
    }
    fn encode_numeric_indicator(
        &self,
        f: &OutputField,
        on: bool,
        cache: Option<&IndicatorCache>,
        baseline: Option<&[u8]>,
        rearm: bool,
        payload: &mut [u8],
    ) -> Result<bool, IndicatorError> {
        if f.flags & 0x100 != 0 {
            return Err(IndicatorError::Buffered);
        }
        if !(0x80053..=0x80055).all(|usage| self.group_has_usage(f, usage)) {
            return Err(IndicatorError::Mode);
        }
        let mut complete = true;
        let gate = self.has_indicator_gate(f);
        let intensity = self.group_has_usage(f, 0x80056);
        for j in 0..usize::from(f.count) {
            let usage = usage_at(self.spans(f), j as u32, true);
            let bit = usize::from(f.bit) + j * usize::from(f.size);
            if !(0x80053..=0x80056).contains(&usage) {
                self.neutral_output(
                    &OutputField {
                        bit: bit as u16,
                        count: 1,
                        ..*f
                    },
                    baseline,
                    payload,
                )?;
                continue;
            }
            let neutral = if f.minimum <= 0 && f.maximum >= 0 {
                0
            } else if f.flags & 0x40 != 0 {
                invalid_value(f).ok_or(IndicatorError::Range)?
            } else {
                return Err(IndicatorError::Range);
            };
            let peer = self.numeric_peer(f, j);
            let preserve = !on && (gate || usage != 0x80056 && intensity);
            if rearm || preserve || !self.numeric_primary(f, j) {
                put_value(payload, bit, usize::from(f.size), neutral);
                continue;
            }
            let (peer, _) = peer.ok_or(IndicatorError::StateUnknown)?;
            let (numerator, denominator) = self.numeric_ratio(f, peer)?;
            let cache = cache.ok_or(IndicatorError::StateUnknown)?;
            let [saved, current] = cache.numeric[usize::from(f.numeric) + j];
            if current == i128::MIN {
                return Err(IndicatorError::StateUnknown);
            }
            let dark = (peer.dark != i64::MIN).then(|| i128::from(peer.dark) * numerator);
            let target = if !on {
                dark.ok_or(IndicatorError::Range)?
            } else if usage != 0x80056 && self.rgb_visible(f, cache, true) {
                if saved == i128::MIN {
                    return Err(IndicatorError::StateUnknown);
                }
                saved
            } else if usage == 0x80056 && saved != i128::MIN && Some(saved) != dark {
                saved
            } else {
                i128::from(peer.maximum) * numerator
            };
            let difference = target - current;
            if difference % denominator != 0 {
                return Err(IndicatorError::Range);
            }
            let delta = difference / denominator;
            let part = delta.clamp(i128::from(f.minimum), i128::from(f.maximum));
            if part == 0 && delta != 0 || part.signum() != delta.signum() {
                return Err(IndicatorError::Range);
            }
            complete &= part == delta;
            put_value(payload, bit, usize::from(f.size), part as i64);
        }
        Ok(complete)
    }
    /// Accepts only uniquely paired absolute feedback for relative numeric LEDs.
    pub(crate) fn observe_numeric_indicators(
        &self,
        cache: &mut IndicatorCache,
        report_id: u8,
        kind: crate::bluetooth::ReportType,
        bytes: &[u8],
        live: bool,
    ) -> bool {
        let mut changed = false;
        for f in self
            .output_fields()
            .iter()
            .filter(|f| f.kind != 0 && f.mode == 3 && f.flags & 7 == 6)
        {
            for j in 0..usize::from(f.count) {
                let Some((peer, k)) = self.numeric_peer(f, j) else {
                    continue;
                };
                if peer.kind != report_kind(kind)
                    || self.reports()[usize::from(peer.report)].id != report_id
                    || usize::from(peer.bit) + usize::from(peer.count) * usize::from(peer.size)
                        > bytes.len() * 8
                {
                    continue;
                }
                let Ok((numerator, _)) = self.numeric_ratio(f, peer) else {
                    continue;
                };
                let Some(raw) = Self::current_indicator(peer, Some(bytes), k) else {
                    continue;
                };
                let slot = usize::from(f.numeric) + j;
                let mask = 1 << (slot % 8);
                if !live && cache.numeric_live[slot / 8] & mask != 0 {
                    continue;
                }
                let value = i128::from(raw) * numerator;
                changed |= cache.numeric[slot][1] != value;
                cache.numeric[slot][1] = value;
                if live {
                    cache.numeric_live[slot / 8] |= mask;
                }
            }
        }
        changed
    }
    pub(crate) fn begin_numeric_write(
        &self,
        cache: &mut IndicatorCache,
        ri: usize,
        kind: crate::bluetooth::ReportType,
    ) {
        for f in self.output_fields().iter().filter(|f| {
            usize::from(f.report) == ri
                && f.kind == report_kind(kind)
                && f.mode == 3
                && f.flags & 7 == 6
        }) {
            for j in 0..usize::from(f.count) {
                let slot = usize::from(f.numeric) + j;
                cache.numeric_live[slot / 8] &= !(1 << (slot % 8));
            }
        }
    }
    pub(crate) fn numeric_written(
        &self,
        cache: &mut IndicatorCache,
        ri: usize,
        kind: crate::bluetooth::ReportType,
        bytes: &[u8],
        success: bool,
    ) -> bool {
        let mut confirmed = true;
        for f in self.output_fields().iter().filter(|f| {
            usize::from(f.report) == ri
                && f.kind == report_kind(kind)
                && f.mode == 3
                && f.flags & 7 == 6
        }) {
            for j in 0..usize::from(f.count) {
                let slot = usize::from(f.numeric) + j;
                if !success {
                    cache.numeric[slot][1] = i128::MIN;
                    continue;
                }
                if let Some((peer, _)) = self.numeric_peer(f, j) {
                    if peer.kind == 2 {
                        continue;
                    }
                    let delta = output_value(f, bytes, j);
                    if delta == 0 || delta < i64::from(f.minimum) || delta > f.maximum {
                        continue;
                    }
                    // A notification during the write may precede or follow
                    // application of this delta. A fresh read disambiguates it.
                    if cache.numeric_live[slot / 8] & (1 << (slot % 8)) != 0 {
                        cache.numeric[slot][1] = i128::MIN;
                        confirmed = false;
                        continue;
                    }
                    if let Ok((_, denominator)) = self.numeric_ratio(f, peer)
                        && cache.numeric[slot][1] != i128::MIN
                        && delta >= i64::from(f.minimum)
                        && delta <= f.maximum
                    {
                        cache.numeric[slot][1] += i128::from(delta) * denominator;
                    }
                }
            }
        }
        confirmed
    }
    fn encode_modified_indicator(
        &self,
        f: &OutputField,
        on: bool,
        cache: Option<&IndicatorCache>,
        baseline: Option<&[u8]>,
        rearm: bool,
        payload: &mut [u8],
    ) -> Result<bool, IndicatorError> {
        if f.mode == 3 && f.flags & 7 == 6 {
            return self.encode_numeric_indicator(f, on, cache, baseline, rearm, payload);
        }
        if f.flags & 0x104 != 0 {
            return Err(if f.flags & 0x100 != 0 {
                IndicatorError::Buffered
            } else {
                IndicatorError::RelativeArray
            });
        }
        let spans = self.spans(f);
        let variable = f.flags & 2 != 0;
        let current = |j| {
            Self::current_indicator(f, baseline, j).or_else(|| Self::color_observation(f, cache, j))
        };
        let retained = |j| Self::retained_indicator(f, cache, j);
        let put = |payload: &mut [u8], j: usize, value| {
            put_value(
                payload,
                usize::from(f.bit) + j * usize::from(f.size),
                usize::from(f.size),
                value,
            )
        };
        if f.mode == 3 {
            if !variable || !(0x80053..=0x80055).all(|usage| self.group_has_usage(f, usage)) {
                return Err(IndicatorError::Mode);
            }
            let has_intensity = self.group_has_usage(f, 0x80056);
            let gated = self.has_indicator_gate(f);
            // A color is selected as a complete tuple. Zero components in a
            // visible tuple remain zero when the light is restored.
            let visible = |saved: bool| {
                self.output_fields()
                    .iter()
                    .filter(|other| {
                        other.kind != 0 && Self::same_group(f, other) && other.mode == 3
                    })
                    .any(|other| {
                        (0..usize::from(other.count)).any(|j| {
                            let usage = usage_at(self.spans(other), j as u32, true);
                            let value = if saved {
                                Self::retained_indicator(other, cache, j)
                            } else {
                                (other.report == f.report && other.kind == f.kind)
                                    .then(|| Self::current_indicator(other, baseline, j))
                                    .flatten()
                                    .or_else(|| Self::color_observation(other, cache, j))
                            };
                            (0x80053..=0x80055).contains(&usage)
                                && value.is_some_and(|v| v != other.dark)
                        })
                    })
            };
            let current_color = cache.is_none() && visible(false);
            let saved_color = cache.is_some_and(|cache| self.rgb_visible(f, cache, true));
            for j in 0..usize::from(f.count) {
                let usage = usage_at(spans, j as u32, true);
                if !(0x80053..=0x80056).contains(&usage) {
                    self.neutral_output(
                        &OutputField {
                            bit: f.bit + (j * usize::from(f.size)) as u16,
                            count: 1,
                            ..*f
                        },
                        baseline,
                        payload,
                    )?;
                    continue;
                }
                let channel = usage != 0x80056;
                let preserve = !on && (gated || channel && has_intensity);
                if preserve {
                    self.neutral_output(
                        &OutputField {
                            bit: f.bit + (j * usize::from(f.size)) as u16,
                            count: 1,
                            ..*f
                        },
                        baseline,
                        payload,
                    )?;
                    continue;
                }
                if !on && !preserve && f.dark == i64::MIN {
                    return Err(IndicatorError::Range);
                }
                let value = if !on {
                    f.dark
                } else if channel {
                    if current_color {
                        current(j).ok_or(IndicatorError::ReadRequired)?
                    } else if saved_color {
                        retained(j).ok_or(IndicatorError::ReadRequired)?
                    } else {
                        f.maximum
                    }
                } else {
                    current(j)
                        .filter(|&v| v != f.dark)
                        .or_else(|| retained(j).filter(|&v| v != f.dark))
                        .unwrap_or(f.maximum)
                };
                if value < i64::from(f.minimum)
                    || value > f.maximum
                    || on && !channel && value == f.dark
                {
                    return Err(IndicatorError::Range);
                }
                put(payload, j, value);
            }
            return Ok(true);
        }
        let color = |usage| matches!(usage, 0x80048 | 0x80049 | 0x8004a | 0x8004e | 0x8004f);
        let gated = f.mode == 2 && self.has_indicator_gate(f);
        let lit = if f.mode == 1 {
            [0x8003d, 0x8003f, 0x80040]
                .into_iter()
                .find(|&usage| self.group_has_usage(f, usage))
        } else {
            [0x80048, 0x80049, 0x8004a, 0x8004e, 0x8004f]
                .into_iter()
                .find(|&usage| self.group_has_usage(f, usage))
        }
        .ok_or(IndicatorError::Mode)?;
        if !gated && !self.group_has_usage(f, 0x80041) {
            return Err(IndicatorError::Mode);
        }
        let selected = |j, saved| {
            let value = if saved { retained(j) } else { current(j) };
            value.is_some_and(|v| {
                if variable {
                    color(usage_at(spans, j as u32, true)) && v != 0
                } else {
                    color(usage_at(spans, (v - i64::from(f.minimum)) as u32, false))
                }
            })
        };
        let visible_color = |saved: bool| {
            self.output_fields()
                .iter()
                .filter(|other| other.kind != 0 && Self::same_group(f, other) && other.mode == 2)
                .any(|other| {
                    (0..usize::from(other.count)).any(|j| {
                        let value = if saved {
                            Self::retained_indicator(other, cache, j)
                        } else {
                            (other.report == f.report && other.kind == f.kind)
                                .then(|| Self::current_indicator(other, baseline, j))
                                .flatten()
                                .or_else(|| Self::color_observation(other, cache, j))
                        };
                        value.is_some_and(|v| {
                            if other.flags & 2 != 0 {
                                color(usage_at(self.spans(other), j as u32, true)) && v != 0
                            } else {
                                color(usage_at(
                                    self.spans(other),
                                    (v - i64::from(other.minimum)) as u32,
                                    false,
                                ))
                            }
                        })
                    })
                })
        };
        let current_color = f.mode == 2 && cache.is_none() && visible_color(false);
        let saved_color = f.mode == 2 && cache.is_some() && visible_color(true);
        let preserve_color = f.mode == 2 && gated && !on;
        if variable {
            if f.minimum > 0 || f.maximum < 1 {
                return Err(IndicatorError::Range);
            }
            for j in 0..usize::from(f.count) {
                let usage = usage_at(spans, j as u32, true);
                let recognized = if f.mode == 1 {
                    (0x8003d..=0x80041).contains(&usage)
                } else {
                    color(usage) || usage == 0x80041
                };
                if !recognized || preserve_color {
                    self.neutral_output(
                        &OutputField {
                            bit: f.bit + (j * usize::from(f.size)) as u16,
                            count: 1,
                            ..*f
                        },
                        baseline,
                        payload,
                    )?;
                    continue;
                }
                let set = if !on {
                    usage == 0x80041
                } else if f.mode == 2 && current_color {
                    selected(j, false)
                } else if f.mode == 2 && saved_color {
                    selected(j, true)
                } else {
                    usage == lit
                };
                put(payload, j, i64::from(set));
            }
        } else {
            if preserve_color {
                self.neutral_output(f, baseline, payload)?;
                return Ok(true);
            }
            let mut used = 0;
            if on && f.mode == 2 && (current_color || saved_color) {
                for j in 0..usize::from(f.count) {
                    if selected(j, saved_color) {
                        put(
                            payload,
                            used,
                            if saved_color { retained(j) } else { current(j) }.unwrap(),
                        );
                        used += 1;
                    }
                }
            } else if let Some(value) =
                selector_value(spans, if on { lit } else { 0x80041 }, f.minimum)
                    .filter(|&value| value <= f.maximum)
            {
                put(payload, 0, value);
                used = 1;
            }
            let empty = selector_value(spans, 0x80000, f.minimum)
                .filter(|&value| value <= f.maximum)
                .or_else(|| invalid_value(f));
            while used < usize::from(f.count) {
                put(payload, used, empty.ok_or(IndicatorError::ArrayCapacity)?);
                used += 1;
            }
        }
        Ok(true)
    }
    /// Collects writable color facets before the first indicator update.
    pub(crate) fn color_reports(
        &self,
    ) -> impl Iterator<Item = (u8, crate::bluetooth::ReportType)> + '_ {
        self.reports()
            .iter()
            .enumerate()
            .flat_map(move |(ri, report)| {
                [0, 1, 2].into_iter().filter_map(move |kind| {
                    self.output_fields()
                        .iter()
                        .any(|f| {
                            usize::from(f.report) == ri
                                && f.kind == kind
                                && f.flags & 4 == 0
                                && (kind != 0 || self.numeric_values != 0 && f.mode == 3)
                                && matches!(f.mode, 2 | 3)
                                && f.target != 0
                        })
                        .then_some((
                            report.id,
                            match kind {
                                0 => crate::bluetooth::ReportType::Input,
                                1 => crate::bluetooth::ReportType::Output,
                                _ => crate::bluetooth::ReportType::Feature,
                            },
                        ))
                })
            })
    }
    pub(crate) fn remember_indicator_values(
        &self,
        cache: &mut IndicatorCache,
        report_id: u8,
        kind: crate::bluetooth::ReportType,
        bytes: &[u8],
    ) {
        for f in self.output_fields().iter().filter(|f| {
            matches!(f.mode, 2 | 3)
                && f.kind != 0
                && f.kind == report_kind(kind)
                && f.flags & 0x104 == 0
                && self.reports()[usize::from(f.report)].id == report_id
        }) {
            if usize::from(f.bit) + usize::from(f.count) * usize::from(f.size) > bytes.len() * 8 {
                continue;
            }
            for j in 0..usize::from(f.count) {
                if let Some(raw) = Self::current_indicator(f, Some(bytes), j) {
                    cache.retained[usize::from(f.retained) + j][1] = raw;
                }
            }
        }
    }
    pub(crate) fn capture_colors(&self, cache: &mut IndicatorCache, targets: u8) {
        for f in self.output_fields().iter().filter(|f| {
            f.kind != 0
                && f.target != 0
                && targets & (1 << (f.target - 1)) != 0
                && matches!(f.mode, 2 | 3)
                && f.flags & 0x104 == 0
        }) {
            let visible = if f.mode == 3 {
                self.rgb_visible(f, cache, false)
            } else {
                self.output_fields()
                    .iter()
                    .filter(|other| {
                        other.kind != 0 && Self::same_group(f, other) && other.mode == f.mode
                    })
                    .any(|other| {
                        (0..usize::from(other.count)).any(|j| {
                            let Some(raw) = Self::color_observation(other, Some(cache), j) else {
                                return false;
                            };
                            let variable = other.flags & 2 != 0;
                            let usage = usage_at(
                                self.spans(other),
                                if variable {
                                    j as u32
                                } else {
                                    (raw - i64::from(other.minimum)) as u32
                                },
                                variable,
                            );
                            if other.mode == 3 {
                                (0x80053..=0x80055).contains(&usage) && raw != other.dark
                            } else {
                                matches!(usage, 0x80048 | 0x80049 | 0x8004a | 0x8004e | 0x8004f)
                                    && (!variable || raw != 0)
                            }
                        })
                    })
            };
            for j in 0..usize::from(f.count) {
                let Some(raw) = Self::color_observation(f, Some(cache), j) else {
                    continue;
                };
                let usage = usage_at(
                    self.spans(f),
                    if f.flags & 2 != 0 {
                        j as u32
                    } else {
                        (raw - i64::from(f.minimum)) as u32
                    },
                    f.flags & 2 != 0,
                );
                let keep = if f.mode == 3 {
                    (0x80053..=0x80055).contains(&usage) && visible
                        || usage == 0x80056 && raw != f.dark
                } else {
                    visible
                };
                if keep {
                    cache.retained[usize::from(f.retained) + j][0] = raw;
                }
            }
        }
        for f in self.output_fields().iter().filter(|f| {
            f.kind != 0
                && f.mode == 3
                && f.flags & 7 == 6
                && f.target != 0
                && targets & (1 << (f.target - 1)) != 0
        }) {
            let visible = self.rgb_visible(f, cache, false);
            for j in 0..usize::from(f.count) {
                let slot = usize::from(f.numeric) + j;
                let raw = cache.numeric[slot][1];
                if raw == i128::MIN {
                    continue;
                }
                let usage = usage_at(self.spans(f), j as u32, true);
                let keep = if usage == 0x80056 {
                    self.numeric_peer(f, j)
                        .and_then(|(peer, _)| {
                            self.numeric_ratio(f, peer).ok().map(|(numerator, _)| {
                                peer.dark == i64::MIN || raw != i128::from(peer.dark) * numerator
                            })
                        })
                        .unwrap_or(false)
                } else {
                    (0x80053..=0x80055).contains(&usage) && visible
                };
                if keep {
                    cache.numeric[slot][0] = raw;
                }
            }
        }
    }
    pub(crate) fn indicator_cache(&self) -> Result<IndicatorCache, Error> {
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(self.retained_values)
            .map_err(|_| Error::Capacity)?;
        retained.resize(self.retained_values, [i64::MIN; 2]);
        let mut numeric = Vec::new();
        numeric
            .try_reserve_exact(self.numeric_values)
            .map_err(|_| Error::Capacity)?;
        numeric.resize(self.numeric_values, [i128::MIN; 2]);
        Ok(IndicatorCache {
            values: zeros(self.indicator_units.div_ceil(2))?,
            retained: retained.into_boxed_slice(),
            numeric: numeric.into_boxed_slice(),
            numeric_live: zeros(self.numeric_values.div_ceil(8))?,
        })
    }
    fn spans(&self, f: &OutputField) -> &[Span] {
        &self.usages()[usize::from(f.span)..usize::from(f.span) + usize::from(f.spans)]
    }
    fn contains_usage(&self, f: &OutputField, usage: u32) -> bool {
        let spans = self.spans(f);
        if f.flags & 2 != 0 {
            (0..u32::from(f.count)).any(|j| usage_at(spans, j, true) == usage)
        } else {
            selector_value(spans, usage, f.minimum).is_some_and(|value| value <= f.maximum)
        }
    }
    fn indicator_elements(f: &OutputField) -> usize {
        if f.flags & 2 != 0 {
            usize::from(f.count)
        } else if f.mode == 4 {
            8
        } else {
            5
        }
    }
    fn indicator_usage(&self, f: &OutputField, j: usize) -> u32 {
        if f.flags & 2 != 0 {
            usage_at(self.spans(f), j as u32, true)
        } else {
            LOCK_SELECTORS[j]
        }
    }
    fn empty_selector(&self, f: &OutputField) -> Option<i64> {
        self.spans(f)
            .iter()
            .find_map(|span| {
                selector_value(self.spans(f), u32::from(span.page) << 16, f.minimum)
                    .filter(|&value| value <= f.maximum)
            })
            .or_else(|| invalid_value(f))
    }
    fn field_targets(&self, f: &OutputField) -> u8 {
        let spans = self.spans(f);
        let mut mask = (1..=5).fold(0, |mask, code| {
            mask | if self.contains_usage(f, 0x80000 + code) {
                1 << (code - 1)
            } else {
                0
            }
        });
        if f.mode == 4 {
            for (usage, code) in [(0x70053, 1), (0x70039, 2), (0x70047, 3)] {
                if self.contains_usage(f, usage) {
                    mask |= 1 << (code - 1);
                }
            }
        } else if f.target != 0
            && spans.iter().any(|s| {
                [
                    0x8003d, 0x8003e, 0x8003f, 0x80040, 0x80041, 0x80048, 0x80049, 0x8004a,
                    0x8004e, 0x8004f, 0x80053, 0x80054, 0x80055, 0x80056,
                ]
                .iter()
                .any(|u| (s.first()..=s.last()).contains(u))
            })
        {
            mask |= 1 << (f.target - 1);
        }
        mask
    }
    pub(crate) fn indicator_reports(
        &self,
    ) -> impl Iterator<Item = (usize, crate::bluetooth::ReportType)> + '_ {
        self.reports().iter().enumerate().flat_map(move |(ri, _)| {
            [1, 2].into_iter().filter_map(move |kind| {
                self.output_fields()
                    .iter()
                    .any(|f| {
                        usize::from(f.report) == ri && f.kind == kind && self.field_targets(f) != 0
                    })
                    .then_some((
                        ri,
                        if kind == 1 {
                            crate::bluetooth::ReportType::Output
                        } else {
                            crate::bluetooth::ReportType::Feature
                        },
                    ))
            })
        })
    }
    pub(crate) fn confirm_indicators(
        &self,
        cache: &mut IndicatorCache,
        ri: usize,
        kind: crate::bluetooth::ReportType,
        target: u8,
        success: bool,
    ) {
        for f in self.output_fields().iter().filter(|f| {
            usize::from(f.report) == ri && f.kind == report_kind(kind) && f.flags & 0x8000 != 0
        }) {
            let count = Self::indicator_elements(f);
            for j in 0..count {
                let raw = self.indicator_usage(f, j);
                let Some(usage) = lock_usage(f, raw) else {
                    continue;
                };
                if !self.contains_usage(f, raw) {
                    continue;
                }
                let slot = usize::from(f.state) + j;
                if !success {
                    cache.put(slot, 0);
                } else if cache.get(slot) & 1 != 0 {
                    cache.put(
                        slot,
                        5 | if target & (1 << (usage - 0x80001)) != 0 {
                            2
                        } else {
                            0
                        },
                    );
                }
            }
        }
    }
    fn indicator_matches(&self, f: &OutputField, raw: u32, canonical: bool) -> usize {
        (0..Self::indicator_elements(f))
            .filter(|&j| {
                let candidate = self.indicator_usage(f, j);
                self.contains_usage(f, candidate)
                    && if canonical {
                        lock_usage(f, candidate) == lock_usage(f, raw)
                    } else {
                        candidate == raw
                    }
            })
            .count()
    }
    fn owners(&self, f: &OutputField, raw: u32, feedback: bool, canonical: bool) -> usize {
        self.output_fields()
            .iter()
            .filter(|other| {
                other.collection == f.collection
                    && other.scope == f.scope
                    && if feedback {
                        matches!(other.kind, 0 | 2) && other.flags & 0x105 == 0
                    } else {
                        other.kind != 0
                    }
            })
            .map(|other| self.indicator_matches(other, raw, canonical))
            .sum()
    }
    pub(crate) fn observe_indicator_feedback(
        &self,
        cache: &mut IndicatorCache,
        report_id: u8,
        kind: crate::bluetooth::ReportType,
        bytes: &[u8],
        live: bool,
    ) -> bool {
        let mut changed = false;
        for owner in self
            .output_fields()
            .iter()
            .filter(|f| f.flags & 0x8000 != 0)
        {
            let count = Self::indicator_elements(owner);
            for j in 0..count {
                let raw = self.indicator_usage(owner, j);
                let Some(usage) = lock_usage(owner, raw) else {
                    continue;
                };
                if !self.contains_usage(owner, raw) || self.owners(owner, raw, false, false) != 1 {
                    continue;
                }
                let exact = self.owners(owner, raw, true, false);
                let canonical = exact == 0
                    && self.owners(owner, raw, false, true) == 1
                    && self.owners(owner, raw, true, true) == 1;
                if exact != 1 && !canonical {
                    continue;
                }
                let Some(feedback) = self.output_fields().iter().find(|f| {
                    f.collection == owner.collection
                        && f.scope == owner.scope
                        && f.kind == report_kind(kind)
                        && self.reports()[usize::from(f.report)].id == report_id
                        && f.flags & 0x105 == 0
                        && self.indicator_matches(f, raw, canonical) != 0
                }) else {
                    continue;
                };
                if usize::from(feedback.bit)
                    + usize::from(feedback.size) * usize::from(feedback.count)
                    > bytes.len() * 8
                {
                    continue;
                }
                let mut observed = (feedback.flags & 2 == 0).then_some(false);
                for k in 0..usize::from(feedback.count) {
                    let raw = output_value(feedback, bytes, k);
                    if raw < i64::from(feedback.minimum) || raw > feedback.maximum {
                        continue;
                    }
                    if feedback.flags & 2 != 0 {
                        let candidate = usage_at(self.spans(feedback), k as u32, true);
                        if if canonical {
                            lock_usage(feedback, candidate) == Some(usage)
                        } else {
                            candidate == self.indicator_usage(owner, j)
                        } {
                            observed = Some(raw != 0);
                        }
                    } else {
                        let candidate = usage_at(
                            self.spans(feedback),
                            (raw - i64::from(feedback.minimum)) as u32,
                            false,
                        );
                        let selected = if canonical {
                            lock_usage(feedback, candidate) == Some(usage)
                        } else {
                            candidate == self.indicator_usage(owner, j)
                        };
                        observed = Some(observed.unwrap_or(false) || selected);
                    }
                }
                if let Some(on) = observed {
                    let slot = usize::from(owner.state) + j;
                    let previous = cache.get(slot);
                    // Conflicting feedback after a write requires a fresh read:
                    // it may be queued before completion or reflect a device reset.
                    let conflict = live && previous & 4 != 0 && (previous & 2 != 0) != on;
                    let value = if conflict {
                        previous & !9
                    } else if !live && previous & 8 != 0 {
                        previous
                    } else {
                        1 | if on { 2 } else { 0 }
                    };
                    cache.put(slot, value | if live && !conflict { 8 } else { 0 });
                    changed |= (previous & 3) != (value & 3);
                }
            }
        }
        changed
    }
    pub fn relative_indicators(&self, ri: usize) -> bool {
        self.relative_indicators_kind(ri, crate::bluetooth::ReportType::Output)
    }
    pub(crate) fn relative_indicators_kind(
        &self,
        ri: usize,
        kind: crate::bluetooth::ReportType,
    ) -> bool {
        self.output_fields().iter().any(|f| {
            usize::from(f.report) == ri
                && f.kind == report_kind(kind)
                && f.flags & 4 != 0
                && f.mode != 3
                && self.field_targets(f) != 0
        })
    }
    /// Input and Feature reports that expose absolute indicator feedback.
    pub fn indicator_feedback(
        &self,
    ) -> impl Iterator<Item = (u8, crate::bluetooth::ReportType)> + '_ {
        self.reports().iter().enumerate().flat_map(move |(ri, r)| {
            [0, 2].into_iter().filter_map(move |kind| {
                self.output_fields()
                    .iter()
                    .any(|f| usize::from(f.report) == ri && f.kind == kind && f.flags & 0x105 == 0)
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
    /// Reads valid absolute LED values without inventing a state for absent fields.
    pub fn observed_indicators(
        &self,
        report_id: u8,
        kind: crate::bluetooth::ReportType,
        bytes: &[u8],
    ) -> IndicatorValue {
        let kind = match kind {
            crate::bluetooth::ReportType::Input => 0,
            crate::bluetooth::ReportType::Output => 1,
            crate::bluetooth::ReportType::Feature => 2,
        };
        let mut result = IndicatorValue::default();
        for f in self.output_fields().iter().filter(|f| {
            self.reports()[usize::from(f.report)].id == report_id
                && f.kind == kind
                && f.flags & 0x105 == 0
        }) {
            if usize::from(f.bit) + usize::from(f.count) * usize::from(f.size) > bytes.len() * 8 {
                continue;
            }
            let spans =
                &self.usages()[usize::from(f.span)..usize::from(f.span) + usize::from(f.spans)];
            if f.flags & 2 == 0 {
                for code in 1..=5 {
                    if selector_value(spans, 0x80000 + code, f.minimum).is_some() {
                        result.known |= 1 << (code - 1);
                    }
                }
            }
            for j in 0..usize::from(f.count) {
                let value = output_value(f, bytes, j);
                if value < i64::from(f.minimum) || value > f.maximum {
                    continue;
                }
                let usage = usage_at(
                    spans,
                    if f.flags & 2 != 0 {
                        j as u32
                    } else {
                        (value - i64::from(f.minimum)) as u32
                    },
                    f.flags & 2 != 0,
                );
                if !(0x80001..=0x80005).contains(&usage) {
                    continue;
                }
                let mask = 1 << (usage - 0x80001);
                result.known |= mask;
                if f.flags & 2 == 0 || value != 0 {
                    result.bits |= mask;
                }
            }
        }
        result
    }
    pub fn indicator_locations(
        &self,
        ri: usize,
        mask: u8,
    ) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.indicator_locations_kind(ri, crate::bluetooth::ReportType::Output, mask)
    }
    pub(crate) fn indicator_locations_kind(
        &self,
        ri: usize,
        kind: crate::bluetooth::ReportType,
        mask: u8,
    ) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.output_fields()
            .iter()
            .filter(move |f| usize::from(f.report) == ri && f.kind == report_kind(kind))
            .flat_map(move |f| {
                let variable = f.flags & 2 != 0;
                (0..Self::indicator_elements(f)).filter_map(move |j| {
                    let raw = self.indicator_usage(f, j);
                    let usage = lock_usage(f, raw)?;
                    if mask & (1 << (usage - 0x80001)) == 0 || !self.contains_usage(f, raw) {
                        return None;
                    }
                    Some((
                        f.bit
                            + if variable {
                                (j * usize::from(f.size)) as u16
                            } else {
                                0
                            },
                        raw,
                    ))
                })
            })
    }
    pub(crate) fn unknown_indicator_locations<'a>(
        &'a self,
        ri: usize,
        kind: crate::bluetooth::ReportType,
        mask: u8,
        cache: &'a IndicatorCache,
    ) -> impl Iterator<Item = (u16, u32)> + 'a {
        self.output_fields()
            .iter()
            .filter(move |f| {
                usize::from(f.report) == ri && f.kind == report_kind(kind) && f.flags & 0x8000 != 0
            })
            .flat_map(move |f| {
                let variable = f.flags & 2 != 0;
                (0..Self::indicator_elements(f)).filter_map(move |j| {
                    let raw = self.indicator_usage(f, j);
                    let usage = lock_usage(f, raw)?;
                    if mask & (1 << (usage - 0x80001)) == 0
                        || !self.contains_usage(f, raw)
                        || cache.get(usize::from(f.state) + j) & 1 != 0
                    {
                        return None;
                    }
                    Some((
                        f.bit
                            + if variable {
                                (j * usize::from(f.size)) as u16
                            } else {
                                0
                            },
                        raw,
                    ))
                })
            })
    }
    fn neutral_output(
        &self,
        f: &OutputField,
        baseline: Option<&[u8]>,
        payload: &mut [u8],
    ) -> Result<(), IndicatorError> {
        let value = if f.flags & 4 != 0 {
            if f.flags & 2 == 0 {
                Some(
                    self.empty_selector(f)
                        .ok_or(IndicatorError::RelativeArray)?,
                )
            } else {
                if f.minimum <= 0 && f.maximum >= 0 {
                    Some(0)
                } else if f.flags & 0x40 != 0 {
                    Some(invalid_value(f).ok_or(IndicatorError::Range)?)
                } else {
                    return Err(IndicatorError::Range);
                }
            }
        } else if f.flags & 0xc0 != 0 {
            invalid_value(f)
        } else {
            None
        };
        if let Some(value) = value {
            for j in 0..usize::from(f.count) {
                put_value(
                    payload,
                    usize::from(f.bit) + j * usize::from(f.size),
                    usize::from(f.size),
                    value,
                );
            }
        } else if baseline.is_none() {
            return Err(IndicatorError::ReadRequired);
        }
        Ok(())
    }
}
fn indicator_unit_ratio(
    absolute: u32,
    relative: u32,
    numerator: &mut i128,
    denominator: &mut i128,
) -> Result<(), IndicatorError> {
    if absolute == relative {
        return Ok(());
    }
    if absolute >> 4 != relative >> 4 {
        return Err(IndicatorError::Scale);
    }
    let a = absolute & 15;
    let r = relative & 15;
    if absolute >> 4 == 0 {
        return Ok(());
    }
    if !(1..=4).contains(&a) || !(1..=4).contains(&r) {
        return Err(IndicatorError::Scale);
    }
    for (shift, n, d) in [
        (4, 127i128, 50i128),
        (8, 45_359_237i128 * 980_665, 3_048_000_000i128),
        (16, 5i128, 9i128),
    ] {
        let exponent = (((absolute >> shift) as i8) << 4) >> 4;
        if exponent == 0 {
            continue;
        }
        if shift == 4 {
            // Linear length and angular position have different base meanings.
            if a % 2 != r % 2 || a.is_multiple_of(2) && a != r {
                return Err(IndicatorError::Scale);
            }
        }
        if (a >= 3) == (r >= 3) {
            continue;
        }
        let inverse = (a < 3) != (exponent < 0);
        for _ in 0..exponent.unsigned_abs() {
            let (n, d) = if inverse { (d, n) } else { (n, d) };
            multiply_indicator_ratio(numerator, denominator, n, d)?;
        }
    }
    Ok(())
}
fn multiply_indicator_ratio(
    numerator: &mut i128,
    denominator: &mut i128,
    mut n: i128,
    mut d: i128,
) -> Result<(), IndicatorError> {
    let gcd = |mut a: i128, mut b: i128| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let common = gcd(*numerator, d);
    *numerator /= common;
    d /= common;
    let common = gcd(*denominator, n);
    *denominator /= common;
    n /= common;
    *numerator = numerator.checked_mul(n).ok_or(IndicatorError::Scale)?;
    *denominator = denominator.checked_mul(d).ok_or(IndicatorError::Scale)?;
    Ok(())
}
fn dark_value(g: &Global) -> i64 {
    let minimum = i64::from(g.min);
    let maximum = if g.min < 0 {
        i64::from(signed(g.max, g.max_size))
    } else {
        i64::from(g.max)
    };
    let low = i64::from(g.physical_minimum);
    let high = if low < 0 {
        i64::from(signed(g.physical_maximum, g.physical_maximum_size))
    } else {
        i64::from(g.physical_maximum)
    };
    if g.physical_bounds != 3 || low == 0 && high == 0 {
        return if minimum <= 0 && maximum >= 0 {
            0
        } else {
            i64::MIN
        };
    }
    if low > 0 || high < 0 || high <= low {
        return i64::MIN;
    }
    let numerator = -i128::from(low) * i128::from(maximum - minimum);
    let denominator = i128::from(high - low);
    if numerator % denominator != 0 {
        return i64::MIN;
    }
    (i128::from(minimum) + numerator / denominator) as i64
}
fn lock_usage(f: &OutputField, usage: u32) -> Option<u32> {
    if (0x80001..=0x80005).contains(&usage) {
        return Some(usage);
    }
    if f.mode == 4 {
        return match usage {
            0x70053 => Some(0x80001),
            0x70039 => Some(0x80002),
            0x70047 => Some(0x80003),
            _ => None,
        };
    }
    None
}
fn keyboard_bits(report: &Layout) -> usize {
    if report.held_roles & KEYBOARD == 0 || report.key_last < report.key_first {
        0
    } else {
        usize::from(report.key_last) - usize::from(report.key_first) + 1
    }
}
fn held_bytes(report: &Layout) -> usize {
    keyboard_bits(report).div_ceil(8)
        + usize::from(report.held_roles & MOUSE != 0) * 2
        + usize::from(report.held_roles & CONSUMER != 0) * 16
        + usize::from(report.held_roles & SYSTEM != 0) * 8
        + usize::from(report.held_roles & RADIO != 0)
}
fn load_held(bytes: &[u8], report: &Layout) -> Held {
    let mut held = Held::default();
    let bits = keyboard_bits(report);
    for bit in 0..bits {
        let code = usize::from(report.key_first) + bit;
        if bytes[bit / 8] & (1 << (bit % 8)) != 0 {
            held.keys[code / 8] |= 1 << (code % 8);
        }
    }
    let mut offset = bits.div_ceil(8);
    if report.held_roles & MOUSE != 0 {
        held.buttons = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        offset += 2;
    }
    if report.held_roles & CONSUMER != 0 {
        for (value, bytes) in held
            .consumers
            .iter_mut()
            .zip(bytes[offset..].as_chunks::<2>().0)
        {
            *value = u16::from_le_bytes(*bytes);
        }
        offset += 16;
    }
    if report.held_roles & SYSTEM != 0 {
        held.system = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
    }
    if report.held_roles & RADIO != 0 {
        held.radio = bytes[offset];
    }
    held
}
fn store_held(bytes: &mut [u8], report: &Layout, held: &Held) {
    let bits = keyboard_bits(report);
    let mut offset = bits.div_ceil(8);
    bytes[..offset].fill(0);
    for bit in 0..bits {
        let code = usize::from(report.key_first) + bit;
        if held.keys[code / 8] & (1 << (code % 8)) != 0 {
            bytes[bit / 8] |= 1 << (bit % 8);
        }
    }
    if report.held_roles & MOUSE != 0 {
        bytes[offset..offset + 2].copy_from_slice(&held.buttons.to_le_bytes());
        offset += 2;
    }
    if report.held_roles & CONSUMER != 0 {
        for (bytes, value) in bytes[offset..]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(held.consumers)
        {
            bytes.copy_from_slice(&value.to_le_bytes());
        }
        offset += 16;
    }
    if report.held_roles & SYSTEM != 0 {
        bytes[offset..offset + 8].copy_from_slice(&held.system.to_le_bytes());
        offset += 8;
    }
    if report.held_roles & RADIO != 0 {
        bytes[offset] = held.radio;
    }
}
fn held_usage(held: &Held, usage: u32) -> bool {
    let code = usage as u16;
    match usage >> 16 {
        7 if code <= 255 => held.keys[usize::from(code) / 8] & (1 << (code % 8)) != 0,
        9 if (1..=16).contains(&code) => held.buttons & (1 << (code - 1)) != 0,
        12 => held.consumers.contains(&code),
        1 if code == 0xc6 => held.radio & 1 != 0,
        1 if code == 0xc8 => held.radio & 2 != 0,
        1 => SYSTEM_USAGES
            .iter()
            .position(|&u| u == code)
            .is_some_and(|i| held.system & (1 << i) != 0),
        _ => false,
    }
}
fn put_value(bytes: &mut [u8], bit: usize, size: usize, value: i64) {
    for i in 0..size {
        let mask = 1 << ((bit + i) % 8);
        let byte = &mut bytes[(bit + i) / 8];
        let set = if i < 64 {
            (value as u64) & (1 << i) != 0
        } else {
            value < 0
        };
        if !set {
            *byte &= !mask;
        } else {
            *byte |= mask;
        }
    }
}
fn normalized(value: i64, minimum: i64, maximum: i64) -> u16 {
    if minimum == maximum {
        0
    } else {
        ((value - minimum) * 65_534 / (maximum - minimum)) as u16
    }
}
fn output_value(f: &OutputField, bytes: &[u8], j: usize) -> i64 {
    numeric_value(
        f.minimum,
        bytes,
        usize::from(f.bit) + j * usize::from(f.size),
        usize::from(f.size),
    )
}
fn invalid_value(f: &OutputField) -> Option<i64> {
    if f.size > 32 {
        return Some(if f.minimum < 0 {
            i64::from(f.minimum) - 1
        } else {
            f.maximum + 1
        });
    }
    let low = if f.minimum < 0 {
        -(1i64 << (f.size - 1))
    } else {
        0
    };
    let high = if f.minimum < 0 {
        (1i64 << (f.size - 1)) - 1
    } else {
        (1i64 << f.size) - 1
    };
    if i64::from(f.minimum) > low {
        Some(i64::from(f.minimum) - 1)
    } else if f.maximum < high {
        Some(f.maximum + 1)
    } else {
        None
    }
}
fn selector_value(spans: &[Span], usage: u32, minimum: i32) -> Option<i64> {
    let mut offset = 0i64;
    for span in spans {
        if (span.first()..=span.last()).contains(&usage) {
            return Some(i64::from(minimum) + offset + i64::from(usage - span.first()));
        }
        offset += i64::from(span.last() - span.first() + 1);
    }
    None
}
impl Input {
    pub(crate) fn pulse(&mut self, usage: u32) -> Result<(), Error> {
        // Relative slider toggles use the corresponding standard momentary button.
        let usage = match usage {
            0x100c8 => 0x100c6,
            0x100ca => 0x100c9,
            _ => usage,
        };
        let mut one = Input::default();
        one.set_usage(usage, 1, false)?;
        if held_usage(&self.pulses, usage) {
            let index = usize::from(self.pulse_repetition_count);
            if index + 1 >= QUEUE {
                return Err(Error::Overflow);
            }
            self.pulse_repetitions[index] = usage;
            self.pulse_repetition_count += 1;
        } else {
            self.pulses = self.pulses.union(&one.held)?;
        }
        Ok(())
    }

    fn switch(&mut self, index: usize, value: i8, explicit: bool) {
        let current = &mut self.consumer_switches[index];
        if explicit {
            *current = value;
            self.explicit_switches |= 1 << index;
        } else if self.explicit_switches & (1 << index) != 0 {
            *current = -*current;
        } else {
            *current = if *current == 0 { 1 } else { 0 };
        }
    }

    fn set_usage(&mut self, usage: u32, value: i64, relative: bool) -> Result<(), Error> {
        if usage == 0x100ca && !relative {
            self.held.system |= ROTATION_KNOWN;
            self.sliders[0] = Some(value != 0);
        }
        if usage == 0x100c8 && !relative {
            self.held.radio |= 4 | (u8::from(value != 0) << 1);
            self.sliders[1] = Some(value != 0);
        }
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
            } else if page == 12
                && let Some(axis) = CONSUMER_AXES.iter().position(|&u| u as usize == code)
            {
                self.consumer_motion[axis] += value;
            }
        } else if usage == 0x100c6 {
            self.held.radio |= 1;
        } else if page == 1
            && let Some(i) = SYSTEM_USAGES.iter().position(|&u| usize::from(u) == code)
        {
            self.held.system |= 1 << i;
        } else if page == 7 && (1..=255).contains(&code) {
            if code <= 3 {
                return Err(Error::Rollover);
            }
            self.held.keys[code / 8] |= 1 << (code % 8);
        } else if page == 9 && (1..=16).contains(&code) {
            self.held.buttons |= 1 << (code - 1);
        } else if page == 12 && (1..=65535).contains(&code) {
            self.held.consumer(code as u16)?;
        }
        Ok(())
    }
}
