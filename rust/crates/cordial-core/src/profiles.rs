//! Input profiles: named sets of rules on HID usages. A profile is loaded only while a connected
//! device's layers or a configuration interface use it, and loaded profiles share a memory budget.
use crate::{
    devices::Roles,
    hid::{Error, Held, Input, ROTATION_STATE, SYSTEM_USAGES},
    storage::{self, RecordStore, record_key},
};
use alloc::{
    rc::{Rc, Weak},
    string::String,
    vec::Vec,
};
use core::cell::RefCell;
use serde::{Deserialize, Serialize};

/// A HID usage as `(usage page << 16) | usage`. Zero names nothing.
pub type Usage = u32;
pub const fn usage(page: u16, id: u16) -> Usage {
    ((page as u32) << 16) | id as u32
}
pub const fn page(usage: Usage) -> u16 {
    (usage >> 16) as u16
}
pub const fn id(usage: Usage) -> u16 {
    usage as u16
}

pub const KEYBOARD_PAGE: u16 = 0x07;
pub const BUTTON_PAGE: u16 = 0x09;
pub const CONSUMER_PAGE: u16 = 0x0c;
pub const DESKTOP_PAGE: u16 = 0x01;
/// The application collections of the adapter's USB reports.
pub const KEYBOARD: Usage = usage(DESKTOP_PAGE, 0x06);
pub const MOUSE: Usage = usage(DESKTOP_PAGE, 0x02);
pub const CONSUMER: Usage = usage(CONSUMER_PAGE, 0x01);
pub const SYSTEM: Usage = usage(DESKTOP_PAGE, 0x80);
/// Consumer AC Pan, the horizontal wheel a mouse reports.
pub const PAN: Usage = usage(CONSUMER_PAGE, 0x238);
/// The relative values in `Input::motion`, in order: X, Y, wheel and pan.
pub const AXES: [Usage; 4] = [
    usage(DESKTOP_PAGE, 0x30),
    usage(DESKTOP_PAGE, 0x31),
    usage(DESKTOP_PAGE, 0x38),
    PAN,
];

/// The most outputs one remap holds, and one held input produces after every layer.
pub const MAX_REMAP_OUTPUTS: usize = 8;
/// The most profiles one device's layers list.
pub const MAX_LAYERS: usize = 8;
/// Profiles returned by one listing page.
pub const PAGE_SIZE: usize = 16;

/// A contiguous range of usages on one page. `collection` names the application collection of the
/// adapter's USB report that carries an output range, and is 0 for input ranges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Range {
    pub collection: Usage,
    pub page: u16,
    pub min: u16,
    pub max: u16,
}
impl Range {
    const fn new(collection: Usage, page: u16, min: u16, max: u16) -> Self {
        Self {
            collection,
            page,
            min,
            max,
        }
    }
    pub fn contains(&self, usage: Usage) -> bool {
        page(usage) == self.page && (self.min..=self.max).contains(&id(usage))
    }
}
/// On/off controls a remap can name: keys, held Consumer controls, mouse buttons and the System
/// Control buttons the adapter's System Control report carries. The display rotation lock slider
/// (`01:ca`) reports a switch position rather than a press, so rules do not name it.
pub const REMAP_INPUTS: [Range; 9] = [
    Range::new(0, KEYBOARD_PAGE, 0x04, 0xff),
    Range::new(0, CONSUMER_PAGE, 0x0001, 0x0237),
    Range::new(0, CONSUMER_PAGE, 0x0239, 0xffff),
    Range::new(0, BUTTON_PAGE, 1, 16),
    Range::new(0, DESKTOP_PAGE, 0x81, 0x8f),
    Range::new(0, DESKTOP_PAGE, 0x9b, 0x9b),
    Range::new(0, DESKTOP_PAGE, 0xa0, 0xaa),
    Range::new(0, DESKTOP_PAGE, 0xb0, 0xb7),
    Range::new(0, DESKTOP_PAGE, 0xc9, 0xc9),
];
/// Relative values a scale can name.
pub const SCALE_INPUTS: [Range; 3] = [
    Range::new(0, DESKTOP_PAGE, 0x30, 0x31),
    Range::new(0, DESKTOP_PAGE, 0x38, 0x38),
    Range::new(0, CONSUMER_PAGE, 0x0238, 0x0238),
];
/// What the adapter's USB reports carry, by the application collection of each report.
pub const REMAP_OUTPUTS: [Range; 9] = [
    Range::new(KEYBOARD, KEYBOARD_PAGE, 0x04, 0xff),
    Range::new(CONSUMER, CONSUMER_PAGE, 0x0001, 0x0237),
    Range::new(CONSUMER, CONSUMER_PAGE, 0x0239, 0xffff),
    Range::new(MOUSE, BUTTON_PAGE, 1, 16),
    Range::new(SYSTEM, DESKTOP_PAGE, 0x81, 0x8f),
    Range::new(SYSTEM, DESKTOP_PAGE, 0x9b, 0x9b),
    Range::new(SYSTEM, DESKTOP_PAGE, 0xa0, 0xaa),
    Range::new(SYSTEM, DESKTOP_PAGE, 0xb0, 0xb7),
    Range::new(SYSTEM, DESKTOP_PAGE, 0xc9, 0xc9),
];
/// The `Held::system` bits, indexed by `SYSTEM_USAGES`, of the System Control buttons rules name.
/// Other bits carry the rotation lock slider and pass through unchanged.
const SYSTEM_BUTTONS: u64 = ((1 << SYSTEM_USAGES.len()) - 1) & !ROTATION_STATE;
/// The `Held::system` bit of the System Control button `id`, if rules name it.
fn system_bit(id: u16) -> Option<u64> {
    SYSTEM_USAGES
        .iter()
        .position(|&u| u == id)
        .map(|i| 1 << i)
        .filter(|bit| bit & SYSTEM_BUTTONS != 0)
}

/// The application collection device input of `usage` arrives in. The HID decoder groups held
/// input by role, so each usage page reaches one collection.
pub fn input_collection(usage: Usage) -> Usage {
    match page(usage) {
        KEYBOARD_PAGE => KEYBOARD,
        BUTTON_PAGE => MOUSE,
        CONSUMER_PAGE if usage == PAN => MOUSE,
        CONSUMER_PAGE => CONSUMER,
        DESKTOP_PAGE if AXES.contains(&usage) => MOUSE,
        DESKTOP_PAGE => SYSTEM,
        _ => 0,
    }
}
/// The collection of the only report that carries `usage`, if exactly one does.
pub fn output_collection(usage: Usage) -> Option<Usage> {
    let mut found = REMAP_OUTPUTS.iter().filter(|r| r.contains(usage));
    let first = found.next()?.collection;
    found.all(|r| r.collection == first).then_some(first)
}

/// One output a remap holds: a usage and the collection of the report that carries it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Output {
    pub usage: Usage,
    pub collection: Usage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    Remap(Vec<Output>),
    Scale(i32, u32),
}

/// One rule as clients see it. A profile has at most one rule for each input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    pub input: Usage,
    pub effect: Effect,
}

/// Why a rule cannot be saved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Invalid {
    Input,
    Output,
    Scale,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Rule {
    /// Checks a rule against what the firmware supports and puts it in its saved form: output
    /// collections resolved, outputs sorted without duplicates and ratios in lowest terms.
    pub fn normalized(mut self) -> Result<Self, Invalid> {
        let ranges: &[Range] = match self.effect {
            Effect::Remap(_) => &REMAP_INPUTS,
            Effect::Scale(..) => &SCALE_INPUTS,
        };
        if !ranges.iter().any(|r| r.contains(self.input)) {
            return Err(Invalid::Input);
        }
        match &mut self.effect {
            Effect::Remap(outputs) => {
                let mut resolved: Vec<Output> = Vec::new();
                for output in outputs.iter() {
                    let collection = match output.collection {
                        0 => output_collection(output.usage).ok_or(Invalid::Output)?,
                        c => c,
                    };
                    if !REMAP_OUTPUTS
                        .iter()
                        .any(|r| r.collection == collection && r.contains(output.usage))
                    {
                        return Err(Invalid::Output);
                    }
                    let output = Output {
                        usage: output.usage,
                        collection,
                    };
                    if !resolved.contains(&output) {
                        resolved.push(output);
                    }
                }
                if resolved.len() > MAX_REMAP_OUTPUTS {
                    return Err(Invalid::Output);
                }
                // Outputs are held together, so their order means nothing; keep one form.
                resolved.sort();
                *outputs = resolved;
            }
            Effect::Scale(numerator, denominator) => {
                if *numerator == 0 || *denominator == 0 {
                    return Err(Invalid::Scale);
                }
                // In i64, since the divisor of i32::MIN and 2^31 is 2^31, which i32 cannot hold.
                let (n, d) = (i64::from(*numerator), i64::from(*denominator));
                let divisor = gcd(n.unsigned_abs(), d.unsigned_abs()) as i64;
                // Dividing by a positive divisor keeps the sign and never grows the magnitude.
                *numerator = (n / divisor) as i32;
                *denominator = (d / divisor) as u32;
            }
        }
        Ok(self)
    }
    /// A rule that changes nothing: a remap of the input to only itself in the collection it
    /// arrives in, or a scale by 1.
    pub fn identity(&self) -> bool {
        match &self.effect {
            Effect::Remap(outputs) => {
                outputs.as_slice()
                    == [Output {
                        usage: self.input,
                        collection: input_collection(self.input),
                    }]
            }
            Effect::Scale(n, d) => *n > 0 && n.unsigned_abs() == *d,
        }
    }
    /// The role of the input this rule changes, as a `hid` role bit.
    fn role(&self) -> u8 {
        match input_collection(self.input) {
            KEYBOARD => crate::hid::KEYBOARD,
            MOUSE => crate::hid::MOUSE,
            CONSUMER => crate::hid::CONSUMER,
            SYSTEM => crate::hid::SYSTEM,
            _ => 0,
        }
    }
}

/// One change in a `SetProfileRules` request.
pub enum Change {
    Set(Rule),
    /// Forgets the rule for this input.
    Forget(Usage),
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    input: Usage,
    /// Index of the first output, or of the scale.
    first: u16,
    /// Outputs of a remap; `SCALE` for a scale.
    count: u8,
}
const SCALE: u8 = u8::MAX;

enum Found<'a> {
    Remap(&'a [Output]),
    Scale(i32, u32),
}

/// A profile's rules, sorted by input for lookup.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    entries: Vec<Entry>,
    outputs: Vec<Output>,
    scales: Vec<(i32, u32)>,
}
impl PartialEq for Rules {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl Rules {
    /// Builds the table from saved rules. Each input appears at most once.
    pub fn new(mut rules: Vec<Rule>) -> Result<Self, Error> {
        rules.sort_by_key(|r| r.input);
        if rules.windows(2).any(|w| w[0].input == w[1].input) {
            return Err(Error::Invalid);
        }
        let mut table = Self::default();
        let outputs: usize = rules
            .iter()
            .map(|r| match &r.effect {
                Effect::Remap(o) => o.len(),
                Effect::Scale(..) => 0,
            })
            .sum();
        let scales = rules.len().saturating_sub(
            rules
                .iter()
                .filter(|r| matches!(r.effect, Effect::Remap(_)))
                .count(),
        );
        table
            .entries
            .try_reserve_exact(rules.len())
            .map_err(|_| Error::Capacity)?;
        table
            .outputs
            .try_reserve_exact(outputs)
            .map_err(|_| Error::Capacity)?;
        table
            .scales
            .try_reserve_exact(scales)
            .map_err(|_| Error::Capacity)?;
        for rule in rules {
            let (first, count) = match rule.effect {
                Effect::Remap(outputs) => {
                    let first = table.outputs.len();
                    table.outputs.extend(outputs);
                    (first, (table.outputs.len() - first) as u8)
                }
                Effect::Scale(n, d) => {
                    table.scales.push((n, d));
                    (table.scales.len() - 1, SCALE)
                }
            };
            table.entries.push(Entry {
                input: rule.input,
                first: u16::try_from(first).map_err(|_| Error::Limit)?,
                count,
            });
        }
        Ok(table)
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    fn found(&self, entry: &Entry) -> Found<'_> {
        let first = usize::from(entry.first);
        if entry.count == SCALE {
            let (n, d) = self.scales[first];
            Found::Scale(n, d)
        } else {
            Found::Remap(&self.outputs[first..first + usize::from(entry.count)])
        }
    }
    fn rule(&self, entry: &Entry) -> Rule {
        Rule {
            input: entry.input,
            effect: match self.found(entry) {
                Found::Remap(outputs) => Effect::Remap(outputs.to_vec()),
                Found::Scale(n, d) => Effect::Scale(n, d),
            },
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = Rule> + '_ {
        self.entries.iter().map(|e| self.rule(e))
    }
    /// The rule for `input`.
    fn lookup(&self, input: Usage) -> Option<Found<'_>> {
        let i = self
            .entries
            .binary_search_by_key(&input, |e| e.input)
            .ok()?;
        Some(self.found(&self.entries[i]))
    }
    /// Bytes the table occupies while loaded.
    pub fn memory(&self) -> usize {
        64 + self.entries.capacity() * core::mem::size_of::<Entry>()
            + self.outputs.capacity() * core::mem::size_of::<Output>()
            + self.scales.capacity() * core::mem::size_of::<(i32, u32)>()
    }
    /// The roles of the input the rules change, as `hid` role bits.
    pub fn roles(&self) -> u8 {
        self.iter().fold(0, |roles, rule| roles | rule.role())
    }
    /// The table after `changes`, applied in order. A rule that changes nothing forgets the rule
    /// it would replace.
    pub fn changed(&self, changes: Vec<Change>) -> Result<Self, Error> {
        let mut rules: Vec<Rule> = self.iter().collect();
        for change in changes {
            let input = match &change {
                Change::Set(rule) => rule.input,
                Change::Forget(input) => *input,
            };
            rules.retain(|r| r.input != input);
            if let Change::Set(rule) = change
                && !rule.identity()
            {
                rules.try_reserve(1).map_err(|_| Error::Capacity)?;
                rules.push(rule);
            }
        }
        Self::new(rules)
    }
}

/// The inputs whose rule differs between `before` and `after`, including those only one of them
/// has a rule for, in ascending order.
pub fn differences(before: &Rules, after: &Rules) -> Vec<Usage> {
    let (mut old, mut new) = (before.iter().peekable(), after.iter().peekable());
    let mut inputs = Vec::new();
    loop {
        match (old.peek().map(|r| r.input), new.peek().map(|r| r.input)) {
            (None, None) => return inputs,
            (Some(a), Some(b)) if a == b => {
                if old.next() != new.next() {
                    inputs.push(a);
                }
            }
            (Some(a), Some(b)) if a < b => {
                old.next();
                inputs.push(a);
            }
            (Some(a), None) => {
                old.next();
                inputs.push(a);
            }
            (_, Some(b)) => {
                new.next();
                inputs.push(b);
            }
        }
    }
}

/// The saved form of `rules.json`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    rules: Vec<SavedRule>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedRule {
    input: [u16; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remap: Option<Vec<[u16; 4]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scale: Option<(i32, u32)>,
}
fn pair(usage: Usage) -> [u16; 2] {
    [page(usage), id(usage)]
}
fn from_pair([p, u]: [u16; 2]) -> Usage {
    self::usage(p, u)
}
impl Rules {
    fn decode(bytes: &[u8]) -> Result<Self, storage::Error> {
        let file: RulesFile = serde_json::from_slice(bytes).map_err(|_| storage::Error::Corrupt)?;
        let mut rules = Vec::new();
        rules
            .try_reserve_exact(file.rules.len())
            .map_err(|_| storage::Error::Unavailable)?;
        for saved in file.rules {
            let effect = match (saved.remap, saved.scale) {
                (Some(outputs), None) => Effect::Remap(
                    outputs
                        .into_iter()
                        .map(|[p, u, cp, cu]| Output {
                            usage: usage(p, u),
                            collection: usage(cp, cu),
                        })
                        .collect(),
                ),
                (None, Some((n, d))) if n != 0 && d != 0 => Effect::Scale(n, d),
                _ => return Err(storage::Error::Corrupt),
            };
            let input = from_pair(saved.input);
            if input == 0 {
                return Err(storage::Error::Corrupt);
            }
            if let Effect::Remap(outputs) = &effect
                && (outputs.len() > usize::from(u8::MAX - 1)
                    || outputs.iter().any(|o| o.usage == 0 || o.collection == 0))
            {
                return Err(storage::Error::Corrupt);
            }
            rules.push(Rule { input, effect });
        }
        let mut table = Self::new(rules).map_err(|e| match e {
            Error::Capacity => storage::Error::Unavailable,
            _ => storage::Error::Corrupt,
        })?;
        table.entries.shrink_to_fit();
        table.outputs.shrink_to_fit();
        table.scales.shrink_to_fit();
        Ok(table)
    }
    fn json(&self) -> Result<Vec<u8>, storage::Error> {
        let rules = self
            .iter()
            .map(|rule| {
                let (remap, scale) = match rule.effect {
                    Effect::Remap(outputs) => (
                        Some(
                            outputs
                                .iter()
                                .map(|o| {
                                    let [p, u] = pair(o.usage);
                                    let [cp, cu] = pair(o.collection);
                                    [p, u, cp, cu]
                                })
                                .collect(),
                        ),
                        None,
                    ),
                    Effect::Scale(n, d) => (None, Some((n, d))),
                };
                SavedRule {
                    input: pair(rule.input),
                    remap,
                    scale,
                }
            })
            .collect();
        storage::json(&RulesFile { rules })
    }
}

/// The saved form of `profile.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "no_roles")]
    pub roles: Roles,
}
fn no_roles(roles: &Roles) -> bool {
    roles.0 == 0
}
pub fn name_valid(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

/// Record kinds of a profile's files.
pub const METADATA: u8 = 8;
pub const RULES: u8 = 9;

/// The profile's record. `Missing` when it does not exist; `Corrupt` when it is undecodable.
pub async fn metadata<S: RecordStore>(store: &mut S, id: u64) -> Result<Metadata, storage::Error> {
    let bytes = store
        .load_owned(record_key(METADATA, id))
        .await?
        .ok_or(storage::Error::Missing)?;
    let meta: Metadata = serde_json::from_slice(&bytes).map_err(|_| storage::Error::Corrupt)?;
    if !name_valid(&meta.name) {
        return Err(storage::Error::Corrupt);
    }
    Ok(meta)
}
/// The profile's rules; none when it has no rules file.
pub async fn rules<S: RecordStore>(store: &mut S, id: u64) -> Result<Rules, storage::Error> {
    match store.load_owned(record_key(RULES, id)).await? {
        Some(bytes) => Rules::decode(&bytes),
        None => Ok(Rules::default()),
    }
}
/// Saves the rules. Forgetting every rule deletes the rules file.
pub async fn save_rules<S: RecordStore>(
    store: &mut S,
    id: u64,
    rules: &Rules,
) -> Result<(), storage::Error> {
    if rules.is_empty() {
        store.remove(record_key(RULES, id)).await
    } else {
        store.save(record_key(RULES, id), &rules.json()?).await
    }
}
/// Saves the profile's record.
pub async fn save_metadata<S: RecordStore>(
    store: &mut S,
    id: u64,
    meta: &Metadata,
) -> Result<(), storage::Error> {
    store
        .save(record_key(METADATA, id), &storage::json(meta)?)
        .await
}
// Until metadata is published, a failed operation cannot have created a visible profile.
fn unpublished_error(error: storage::Error) -> storage::Error {
    if error == storage::Error::Unknown {
        storage::Error::Io
    } else {
        error
    }
}
/// Saves a new profile with `rules`, writing the rules before the record that publishes it.
pub async fn create<S: RecordStore>(
    store: &mut S,
    name: &str,
    rules: &Rules,
) -> Result<(u64, Metadata), storage::Error> {
    let meta = Metadata {
        name: name.into(),
        roles: Roles(rules.roles()),
    };
    let bytes = if rules.is_empty() {
        None
    } else {
        Some(rules.json()?)
    };
    let record = storage::json(&meta)?;
    let size = bytes.as_ref().map_or(0, Vec::len) + record.len();
    if store.available().await?
        < crate::bonds::MAINTENANCE_BYTES
            + crate::bonds::PAIR_BYTES
            + size.div_ceil(4096) * 4096
            + 8192
    {
        return Err(storage::Error::Full);
    }
    let id = storage::allocate(store, true)
        .await
        .map_err(unpublished_error)?;
    if let Some(bytes) = &bytes
        && let Err(error) = store.save(record_key(RULES, id), bytes).await
    {
        // No record has been published, so cleanup is safe even if the rules write landed.
        let _ = store.remove(record_key(RULES, id)).await;
        return Err(unpublished_error(error));
    }
    if let Err(error) = store.save(record_key(METADATA, id), &record).await {
        if error != storage::Error::Unknown {
            let _ = store.remove(record_key(RULES, id)).await;
        }
        return Err(error);
    }
    Ok((id, meta))
}
/// Deletes a profile: removing its record commits the deletion, and the rules follow.
pub async fn remove<S: RecordStore>(store: &mut S, id: u64) -> Result<(), storage::Error> {
    store.remove(record_key(METADATA, id)).await?;
    let _ = store.remove(record_key(RULES, id)).await;
    Ok(())
}

/// Shared rules of one loaded profile. Live edits replace its contents for every user.
pub type Map = Rc<RefCell<Rules>>;

/// Why a device's or interface's profiles were not loaded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadError {
    /// They do not fit in what is left of the memory budget.
    Capacity,
    /// A profile could not be read.
    Storage,
    /// A profile's saved files are undecodable; it is lost.
    Lost(u64),
}

/// Loaded profiles by ID. Weak entries identify shared tables without keeping unused ones loaded.
#[derive(Default)]
pub struct Cache(Vec<(u64, Weak<RefCell<Rules>>)>);
impl Cache {
    pub fn get(&self, id: u64) -> Option<Map> {
        self.0
            .iter()
            .find(|(key, _)| *key == id)
            .and_then(|(_, map)| map.upgrade())
    }
    /// Bytes the loaded profiles occupy.
    pub fn used(&self) -> usize {
        self.0
            .iter()
            .filter_map(|(_, map)| map.upgrade())
            .map(|map| map.borrow().memory())
            .sum()
    }
    /// Loads every profile in `ids`, in order, sharing those already loaded. `releasing` lists
    /// what the caller holds now and lets go of once the new profiles are in use; profiles only it
    /// holds count as released. Loads all or none: when they do not fit in `budget`, nothing new
    /// stays loaded. Only each profile's rules file is read, so a profile without one, including a
    /// profile that no longer exists, loads with no rules and passes input through.
    pub async fn load<S: RecordStore>(
        &mut self,
        store: &mut S,
        ids: &[u64],
        budget: usize,
        releasing: &[Map],
    ) -> Result<Vec<Map>, LoadError> {
        self.0.retain(|(_, map)| map.strong_count() != 0);
        let used = self.used();
        let mut found: Vec<(u64, Map)> = Vec::new();
        found
            .try_reserve_exact(ids.len())
            .map_err(|_| LoadError::Capacity)?;
        let mut maps: Vec<Map> = Vec::new();
        maps.try_reserve_exact(ids.len())
            .map_err(|_| LoadError::Capacity)?;
        // A profile only `releasing` holds is released once the new profiles are in use.
        let released_only = |map: &Map| {
            Rc::strong_count(map) == releasing.iter().filter(|m| Rc::ptr_eq(m, map)).count()
        };
        // The most `releasing` can free, so a load that cannot fit stops before reading the rest.
        let most_released: usize = releasing
            .iter()
            .enumerate()
            .filter(|(i, map)| {
                !releasing[..*i].iter().any(|m| Rc::ptr_eq(m, map)) && released_only(map)
            })
            .map(|(_, map)| map.borrow().memory())
            .sum();
        let mut added = 0usize;
        for &id in ids {
            if let Some((_, map)) = found.iter().find(|(key, _)| *key == id) {
                maps.push(map.clone());
                continue;
            }
            let map = match self.get(id) {
                Some(map) => map,
                None => {
                    let table = match rules(store, id).await {
                        Ok(table) => table,
                        Err(storage::Error::Corrupt) => return Err(LoadError::Lost(id)),
                        Err(storage::Error::Unavailable) => return Err(LoadError::Capacity),
                        Err(_) => return Err(LoadError::Storage),
                    };
                    added += table.memory();
                    if used.saturating_sub(most_released) + added > budget {
                        return Err(LoadError::Capacity);
                    }
                    Rc::new(RefCell::new(table))
                }
            };
            found.push((id, map.clone()));
            maps.push(map);
        }
        let released: usize = releasing
            .iter()
            .enumerate()
            .filter(|(i, map)| {
                !releasing[..*i].iter().any(|m| Rc::ptr_eq(m, map))
                    && !maps.iter().any(|m| Rc::ptr_eq(m, map))
                    && released_only(map)
            })
            .map(|(_, map)| map.borrow().memory())
            .sum();
        if used.saturating_sub(released) + added > budget {
            return Err(LoadError::Capacity);
        }
        self.0
            .try_reserve(found.len())
            .map_err(|_| LoadError::Capacity)?;
        for (id, map) in found {
            if !self.0.iter().any(|(key, _)| *key == id) {
                self.0.push((id, Rc::downgrade(&map)));
            }
        }
        Ok(maps)
    }
    /// Whether replacing loaded profile `id` with a table of `memory` bytes keeps the loaded
    /// profiles within `budget`. A profile that is not loaded always fits.
    pub fn fits(&self, id: u64, memory: usize, budget: usize) -> bool {
        match self.get(id) {
            Some(map) => self.used() - map.borrow().memory() + memory <= budget,
            None => true,
        }
    }
    /// Replaces the rules of loaded profile `id` for every user.
    pub fn publish(&self, id: u64, rules: &Rules) {
        if let Some(map) = self.get(id) {
            *map.borrow_mut() = rules.clone();
        }
    }
    /// Unloads profile `id`'s entry so no later load shares a table that no longer exists.
    pub fn forget(&mut self, id: u64) {
        self.0.retain(|(key, _)| *key != id);
    }
}

/// Outputs of one input, at most `MAX_REMAP_OUTPUTS`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Outputs {
    items: [Output; MAX_REMAP_OUTPUTS],
    len: u8,
}
impl Outputs {
    fn one(output: Output) -> Self {
        let mut outputs = Self::default();
        outputs.push(output);
        outputs
    }
    fn as_slice(&self) -> &[Output] {
        &self.items[..usize::from(self.len)]
    }
    fn push(&mut self, output: Output) {
        if usize::from(self.len) < MAX_REMAP_OUTPUTS && !self.as_slice().contains(&output) {
            self.items[usize::from(self.len)] = output;
            self.len += 1;
        }
    }
}

/// Applies a device's layers to its input. Held inputs keep the outputs they were pressed with
/// until they are released.
pub struct Mapper {
    pub layers: Vec<Map>,
    held: Vec<(Usage, Outputs)>,
    remainder: [i64; 4],
    ratio: [(i64, i64); 4],
}
impl Default for Mapper {
    fn default() -> Self {
        Self::new()
    }
}
impl Mapper {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            held: Vec::new(),
            remainder: [0; 4],
            ratio: [(1, 1); 4],
        }
    }
    /// The outputs of `input` after every layer.
    fn resolve(&self, input: Usage) -> Outputs {
        let mut current = Outputs::one(Output {
            usage: input,
            collection: input_collection(input),
        });
        for layer in &self.layers {
            let layer = layer.borrow();
            let mut next = Outputs::default();
            for output in current.as_slice() {
                match layer.lookup(output.usage) {
                    Some(Found::Remap(outputs)) => {
                        for o in outputs {
                            next.push(*o);
                        }
                    }
                    _ => next.push(*output),
                }
            }
            current = next;
            if current.len == 0 {
                break;
            }
        }
        current
    }
    fn pressed(input: &Held) -> impl Iterator<Item = Usage> + '_ {
        let keys = (4..256u16)
            .filter(|k| input.keys[usize::from(k / 8)] & (1 << (k % 8)) != 0)
            .map(|k| usage(KEYBOARD_PAGE, k));
        let consumers = input
            .consumers
            .iter()
            .filter(|u| **u != 0)
            .map(|u| usage(CONSUMER_PAGE, *u));
        let buttons = (0..16u16)
            .filter(|b| input.buttons & (1 << b) != 0)
            .map(|b| usage(BUTTON_PAGE, b + 1));
        let system = SYSTEM_USAGES
            .iter()
            .enumerate()
            .filter(|(i, _)| input.system & SYSTEM_BUTTONS & (1 << i) != 0)
            .map(|(_, u)| usage(DESKTOP_PAGE, *u));
        keys.chain(consumers).chain(buttons).chain(system)
    }
    pub fn held(&mut self, input: Held) -> Result<Held, Error> {
        let mut output = Held {
            system: input.system & !SYSTEM_BUTTONS,
            radio: input.radio,
            ..Held::default()
        };
        let count = Self::pressed(&input).count();
        // Validate and reserve before mutating press-time state. Retain the allocation between
        // reports; only a larger simultaneous chord needs another allocation.
        self.held
            .try_reserve(count.saturating_sub(self.held.len()))
            .map_err(|_| Error::Capacity)?;
        self.held
            .retain(|(usage, _)| Self::pressed(&input).any(|u| u == *usage));
        for input in Self::pressed(&input) {
            let outputs = match self.held.iter().find(|(u, _)| *u == input) {
                Some((_, outputs)) => *outputs,
                None => {
                    let outputs = self.resolve(input);
                    self.held.push((input, outputs));
                    outputs
                }
            };
            for o in outputs.as_slice() {
                emit(&mut output, *o)?;
            }
        }
        Ok(output)
    }
    /// The combined ratio every layer applies to `axis`, in lowest terms.
    fn ratio(&self, axis: Usage) -> (i64, i64) {
        let limit = i64::from(i32::MAX);
        let mut ratio = (1i64, 1i64);
        for layer in &self.layers {
            if let Some(Found::Scale(n, d)) = layer.borrow().lookup(axis) {
                let numerator = ratio.0.saturating_mul(n.into());
                let denominator = ratio.1.saturating_mul(d.into());
                let divisor = gcd(numerator.unsigned_abs(), denominator.unsigned_abs()).max(1);
                ratio = (
                    (numerator / divisor as i64).clamp(-limit, limit),
                    (denominator / divisor as i64).clamp(1, limit),
                );
            }
        }
        ratio
    }
    pub fn motion(&mut self, input: &mut Input) {
        for (i, axis) in AXES.into_iter().enumerate() {
            let ratio = self.ratio(axis);
            if ratio != self.ratio[i] {
                self.ratio[i] = ratio;
                self.remainder[i] = 0;
            }
            if ratio == (1, 1) {
                continue;
            }
            let scaled = input.motion[i]
                .saturating_mul(ratio.0)
                .saturating_add(self.remainder[i]);
            input.motion[i] =
                (scaled / ratio.1).clamp(-crate::hid::MOTION_LIMIT, crate::hid::MOTION_LIMIT);
            self.remainder[i] = scaled % ratio.1;
        }
    }
}
fn emit(output: &mut Held, o: Output) -> Result<(), Error> {
    match (o.collection, page(o.usage)) {
        (KEYBOARD, KEYBOARD_PAGE) => {
            let key = usize::from(id(o.usage));
            output.keys[key / 8] |= 1 << (key % 8);
        }
        (CONSUMER, CONSUMER_PAGE) => output.consumer(id(o.usage))?,
        (MOUSE, BUTTON_PAGE) if (1..=16).contains(&id(o.usage)) => {
            output.buttons |= 1 << (id(o.usage) - 1)
        }
        (SYSTEM, DESKTOP_PAGE) => {
            if let Some(bit) = system_bit(id(o.usage)) {
                output.system |= bit;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(k: u16) -> Usage {
        usage(KEYBOARD_PAGE, k)
    }
    fn out(k: u16) -> Output {
        Output {
            usage: key(k),
            collection: KEYBOARD,
        }
    }
    fn remap(input: Usage, outputs: &[Output]) -> Rule {
        Rule {
            input,
            effect: Effect::Remap(outputs.to_vec()),
        }
    }
    fn map(rules: Vec<Rule>) -> Map {
        Rc::new(RefCell::new(Rules::new(rules).unwrap()))
    }
    fn keys(usages: &[u8]) -> Held {
        let mut held = Held::default();
        for &key in usages {
            held.keys[usize::from(key / 8)] |= 1 << (key % 8);
        }
        held
    }
    fn system(id: u16) -> Output {
        Output {
            usage: usage(DESKTOP_PAGE, id),
            collection: SYSTEM,
        }
    }
    fn system_held(ids: &[u16]) -> Held {
        Held {
            system: ids.iter().map(|&id| system_bit(id).unwrap()).sum(),
            ..Held::default()
        }
    }
    #[test]
    fn system_ranges_name_every_system_button() {
        for (i, &id) in SYSTEM_USAGES.iter().enumerate() {
            let button = SYSTEM_BUTTONS & (1 << i) != 0;
            let input = usage(DESKTOP_PAGE, id);
            assert_eq!(REMAP_INPUTS.iter().any(|r| r.contains(input)), button);
            assert_eq!(output_collection(input), button.then_some(SYSTEM));
            assert_eq!(system_bit(id), button.then_some(1 << i));
        }
        assert_eq!(system_bit(0xca), None);
        for range in SCALE_INPUTS {
            for id in range.min..=range.max {
                let input = usage(range.page, id);
                assert!(!REMAP_INPUTS.iter().any(|r| r.contains(input)));
                assert!(!REMAP_OUTPUTS.iter().any(|r| r.contains(input)));
            }
        }
    }
    #[test]
    fn system_controls_remap_to_and_from_keys_through_layers() {
        // Sleep sends F12, Caps Lock sends Power, and a later layer turns Power into Wake.
        let first = map(alloc::vec![
            remap(usage(DESKTOP_PAGE, 0x82), &[out(0x45)]),
            remap(key(0x39), &[system(0x81)]),
        ]);
        let second = map(alloc::vec![remap(
            usage(DESKTOP_PAGE, 0x81),
            &[system(0x83), out(0xe0)]
        )]);
        let mut mapper = Mapper::new();
        mapper.layers = alloc::vec![first.clone()];
        assert_eq!(mapper.held(system_held(&[0x82])).unwrap(), keys(&[0x45]));
        assert_eq!(mapper.held(keys(&[0x39])).unwrap(), system_held(&[0x81]));
        assert_eq!(mapper.held(Held::default()).unwrap(), Held::default());
        // A system control without a rule passes through.
        assert_eq!(
            mapper.held(system_held(&[0x83, 0xc9])).unwrap(),
            system_held(&[0x83, 0xc9])
        );
        mapper.layers = alloc::vec![first, second];
        let mut expected = system_held(&[0x83]);
        expected.keys = keys(&[0xe0]).keys;
        assert_eq!(mapper.held(keys(&[0x39])).unwrap(), expected);
        assert_eq!(mapper.held(Held::default()).unwrap(), Held::default());
        assert_eq!(mapper.held(system_held(&[0x81])).unwrap(), expected);
    }
    #[test]
    fn remapping_keeps_rotation_lock_and_radio_state() {
        let layer = map(alloc::vec![
            remap(usage(DESKTOP_PAGE, 0x81), &[]),
            remap(key(0x04), &[system(0x82)]),
        ]);
        let mut mapper = Mapper::new();
        mapper.layers = alloc::vec![layer];
        let state = crate::hid::ROTATION_KNOWN | ROTATION_STATE;
        let mut input = system_held(&[0x81]);
        input.system |= state;
        input.keys = keys(&[0x04]).keys;
        input.radio = 7;
        let output = mapper.held(input).unwrap();
        assert_eq!(output.system, state | system_bit(0x82).unwrap());
        assert_eq!(output.radio, 7);
        assert_eq!(output.keys, [0; 32]);
    }
    #[test]
    fn system_rules_normalize_and_report_the_system_role() {
        let power = usage(DESKTOP_PAGE, 0x81);
        let rule = remap(
            key(0x39),
            &[Output {
                usage: power,
                collection: 0,
            }],
        )
        .normalized()
        .unwrap();
        assert_eq!(rule.effect, Effect::Remap(alloc::vec![system(0x81)]));
        assert_eq!(
            remap(power, &[out(0x45)]).normalized().unwrap().role(),
            crate::hid::SYSTEM
        );
        let rotation = usage(DESKTOP_PAGE, 0xc9);
        assert_eq!(
            remap(rotation, &[]).normalized().unwrap().role(),
            crate::hid::SYSTEM
        );
        assert!(
            remap(power, &[system(0x81)])
                .normalized()
                .unwrap()
                .identity()
        );
        let slider = usage(DESKTOP_PAGE, 0xca);
        assert_eq!(remap(slider, &[]).normalized(), Err(Invalid::Input));
        assert_eq!(
            remap(
                key(0x39),
                &[Output {
                    usage: slider,
                    collection: 0
                }]
            )
            .normalized(),
            Err(Invalid::Output)
        );
        assert_eq!(
            remap(
                key(0x39),
                &[Output {
                    usage: power,
                    collection: KEYBOARD
                }]
            )
            .normalized(),
            Err(Invalid::Output)
        );
    }
    #[test]
    fn layers_chain_and_fan_out() {
        // A -> Ctrl+C, then C -> Alt+D: A sends Ctrl+Alt+D, and C alone sends Alt+D.
        let first = map(alloc::vec![remap(key(0x04), &[out(0xe0), out(0x06)])]);
        let second = map(alloc::vec![remap(key(0x06), &[out(0xe2), out(0x07)])]);
        let mut mapper = Mapper::new();
        mapper.layers = alloc::vec![first, second];
        assert_eq!(
            mapper.held(keys(&[0x04])).unwrap(),
            keys(&[0xe0, 0xe2, 0x07])
        );
        assert_eq!(mapper.held(keys(&[])).unwrap(), Held::default());
        assert_eq!(mapper.held(keys(&[0x06])).unwrap(), keys(&[0xe2, 0x07]));
    }
    #[test]
    fn held_inputs_keep_press_time_outputs_across_live_edits() {
        let layer = map(alloc::vec![remap(key(0x04), &[out(0x1c)])]);
        let mut mapper = Mapper::new();
        mapper.layers = alloc::vec![layer.clone()];
        assert_eq!(mapper.held(keys(&[0x04])).unwrap(), keys(&[0x1c]));
        *layer.borrow_mut() = Rules::new(alloc::vec![remap(key(0x04), &[out(0x29)])]).unwrap();
        assert_eq!(mapper.held(keys(&[0x04])).unwrap(), keys(&[0x1c]));
        assert_eq!(mapper.held(keys(&[])).unwrap(), Held::default());
        assert_eq!(mapper.held(keys(&[0x04])).unwrap(), keys(&[0x29]));
    }
    #[test]
    fn disabling_stops_the_chain() {
        let button = usage(BUTTON_PAGE, 1);
        let layer = map(alloc::vec![
            remap(key(0x04), &[]),
            remap(button, &[out(0x06)])
        ]);
        let mut mapper = Mapper::new();
        mapper.layers = alloc::vec![layer];
        assert_eq!(mapper.held(keys(&[0x04])).unwrap(), Held::default());
        let input = Held {
            buttons: 1,
            ..Held::default()
        };
        assert_eq!(mapper.held(input).unwrap(), keys(&[0x06]));
    }
    #[test]
    fn scales_multiply_across_layers_with_one_remainder() {
        let x = AXES[0];
        let wheel = AXES[2];
        let scale = |input, n, d| Rule {
            input,
            effect: Effect::Scale(n, d),
        };
        let mut mapper = Mapper::new();
        mapper.layers = alloc::vec![
            map(alloc::vec![scale(x, 1, 2), scale(wheel, -1, 1)]),
            map(alloc::vec![scale(x, 2, 3)]),
        ];
        let mut input = Input {
            motion: [1, 1, 2, 3],
            ..Input::default()
        };
        mapper.motion(&mut input);
        assert_eq!(input.motion, [0, 1, -2, 3]);
        input.motion = [2, 0, 0, 0];
        mapper.motion(&mut input);
        assert_eq!(input.motion[0], 1);
    }
    #[test]
    fn rules_normalize_and_identity_forgets() {
        let rule = remap(
            key(0x04),
            &[
                Output {
                    usage: key(0x05),
                    collection: 0,
                },
                out(0x05),
            ],
        )
        .normalized()
        .unwrap();
        assert_eq!(rule.effect, Effect::Remap(alloc::vec![out(0x05)]));
        assert_eq!(
            remap(key(0x02), &[out(0x05)]).normalized(),
            Err(Invalid::Input)
        );
        assert_eq!(
            remap(key(0x04), &[out(0x02)]).normalized(),
            Err(Invalid::Output)
        );
        let identity = remap(key(0x04), &[out(0x04)]).normalized().unwrap();
        assert!(identity.identity());
        let table = Rules::new(alloc::vec![remap(key(0x04), &[out(0x05)])]).unwrap();
        let table = table.changed(alloc::vec![Change::Set(identity)]).unwrap();
        assert!(table.is_empty());
        // Rules are identified by their input alone.
        let table = Rules::new(alloc::vec![
            remap(key(0x04), &[out(0x05)]),
            remap(key(0x06), &[out(0x07)]),
        ])
        .unwrap();
        let table = table
            .changed(alloc::vec![
                Change::Forget(key(0x04)),
                Change::Set(remap(key(0x06), &[out(0x08)])),
            ])
            .unwrap();
        assert!(table.iter().eq([remap(key(0x06), &[out(0x08)])]));
        assert!(
            Rules::new(alloc::vec![
                remap(key(0x04), &[]),
                remap(key(0x04), &[out(0x05)])
            ])
            .is_err()
        );
        let scaled = Rule {
            input: AXES[0],
            effect: Effect::Scale(4, 4),
        }
        .normalized()
        .unwrap();
        assert_eq!(scaled.effect, Effect::Scale(1, 1));
        assert!(scaled.identity());
        // The divisor of i32::MIN and 2^31 does not fit in i32; the inversion stays one.
        let inverted = Rule {
            input: AXES[2],
            effect: Effect::Scale(i32::MIN, 1 << 31),
        }
        .normalized()
        .unwrap();
        assert_eq!(inverted.effect, Effect::Scale(-1, 1));
        assert!(!inverted.identity());
    }
    #[test]
    fn rules_files_roundtrip_and_report_roles() {
        let table = Rules::new(alloc::vec![
            remap(key(0x39), &[out(0xe0)]),
            remap(
                usage(CONSUMER_PAGE, 0xcd),
                &[Output {
                    usage: usage(BUTTON_PAGE, 1),
                    collection: MOUSE,
                }],
            ),
            Rule {
                input: AXES[2],
                effect: Effect::Scale(-1, 1),
            },
        ])
        .unwrap();
        let decoded = Rules::decode(&table.json().unwrap()).unwrap();
        assert_eq!(decoded, table);
        assert_eq!(
            table.roles(),
            crate::hid::KEYBOARD | crate::hid::CONSUMER | crate::hid::MOUSE
        );
        assert!(Rules::decode(br#"{"rules":[{"input":[7,4]}]}"#).is_err());
    }
}
