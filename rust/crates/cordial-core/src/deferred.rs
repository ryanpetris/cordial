//! Records saved once keyboard and mouse input pauses.
//!
//! A record whose newest contents are in RAM, and that nothing needs on flash at once, is marked
//! dirty instead of being written. It is written once, after input has been quiet for
//! [`QUIET_MS`], and never later than [`MAX_DELAY_MS`] after it first became dirty, so changes
//! repeated meanwhile, such as a keymap editor's stream of edits, cost one write. Until then,
//! writes also wait while a connection waits for its first input. A write that fails keeps its
//! record dirty and is tried again after its own backoff, from which its longest wait counts
//! again. A record waiting for its backoff does not hold up other records, nor hurry them.
//!
//! Background work that writes, such as saving a discovered layout, follows the same timing
//! without being listed here: it asks [`Dirty::wait`] before each write.
use crate::{
    devices::{Backoff, Roles},
    profiles, storage,
};
use alloc::vec::Vec;

/// How long keyboard and mouse input stays quiet before dirty records are written.
pub const QUIET_MS: u64 = 250;
/// The longest a record stays dirty while input keeps arriving.
pub const MAX_DELAY_MS: u64 = 2000;
/// The longest a rules file that cannot be written waits before it is tried again: it holds edits
/// an editor has been told are made.
pub const RULES_RETRY_MAX_MS: u32 = 10_000;

/// A record whose saved copy is behind RAM.
#[derive(Clone)]
pub enum Record {
    /// A profile's rules file, written from its table, which stays loaded until it is saved.
    Rules(u64, profiles::Map),
    /// The roles summary in a profile's record, which follows its saved rules.
    Roles(u64, Roles),
    /// The roles a device's descriptor reported, for its saved record.
    DeviceRoles(u64, Roles),
}
impl Record {
    /// Whether both are the same file.
    fn same(&self, other: &Record) -> bool {
        match (self, other) {
            (Self::Rules(a, _), Self::Rules(b, _))
            | (Self::Roles(a, _), Self::Roles(b, _))
            | (Self::DeviceRoles(a, _), Self::DeviceRoles(b, _)) => a == b,
            _ => false,
        }
    }
    fn is_rules(&self) -> bool {
        matches!(self, Self::Rules(..))
    }
}

/// A dirty record with the backoff of its failed writes.
struct Entry {
    record: Record,
    retry: Backoff,
    /// How its last write failed.
    failure: Option<storage::Error>,
    /// When it first became dirty.
    since: u64,
    /// Whether the flush in progress has tried it.
    flushed: bool,
}
impl Entry {
    /// When its longest wait started: when it became dirty, or when its backoff ended.
    fn waiting_from(&self) -> u64 {
        self.since.max(self.retry.at())
    }
}

/// Whether a write that has waited since `since` may run at `now`: input has been quiet long
/// enough and no connection waits for its first input, or it has waited the longest it may.
pub fn allowed(since: u64, now: u64, last_input: Option<u64>, starting: bool) -> bool {
    let quiet = last_input.is_none_or(|input| now.saturating_sub(input) >= QUIET_MS);
    (quiet && !starting) || now.saturating_sub(since) >= MAX_DELAY_MS
}

/// The dirty records, and when background work started waiting to write.
#[derive(Default)]
pub struct Dirty {
    records: Vec<Entry>,
    /// When background work, which is not listed, started waiting to write.
    since: Option<u64>,
    /// Background work asked to write since the current pass of it started.
    waiting: bool,
}
impl Dirty {
    /// Marks `record` dirty since `at`, replacing what an earlier mark of the same file holds.
    /// Returns `false`, marking nothing, when there is no memory to list it.
    #[must_use]
    pub fn mark(&mut self, record: Record, at: u64) -> bool {
        if let Some(entry) = self.records.iter_mut().find(|e| e.record.same(&record)) {
            entry.record = record;
            entry.since = entry.since.min(at);
            return true;
        }
        if self.records.try_reserve(1).is_err() {
            return false;
        }
        self.records.push(Entry {
            record,
            retry: Backoff::default(),
            failure: None,
            since: at,
            flushed: false,
        });
        true
    }
    /// Makes room now, so `count` more records can then be marked without allocating.
    pub fn reserve(&mut self, count: usize) -> bool {
        self.records.try_reserve(count).is_ok()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    /// How the last write of `record`'s file failed.
    pub fn failure(&self, record: &Record) -> Option<storage::Error> {
        self.records
            .iter()
            .find(|e| e.record.same(record))
            .and_then(|e| e.failure)
    }
    /// Whether `record`'s file is listed.
    pub fn lists(&self, record: &Record) -> bool {
        self.records.iter().any(|e| e.record.same(record))
    }
    /// Notes how an earlier write of listed `record`'s file, made apart from its entry, failed.
    pub fn set_failure(&mut self, record: &Record, error: storage::Error) {
        if let Some(entry) = self.records.iter_mut().find(|e| e.record.same(record)) {
            entry.failure = Some(error);
        }
    }
    /// Whether the last write of a dirty rules file failed with `error`.
    pub fn rules_failed(&self, error: storage::Error) -> bool {
        self.records
            .iter()
            .any(|e| e.record.is_rules() && e.failure == Some(error))
    }
    /// For background work with a write to make: notes that it waits from `now` and says whether
    /// it may write. Dirty records do not change its timing.
    pub fn wait(&mut self, now: u64, last_input: Option<u64>, starting: bool) -> bool {
        let since = *self.since.get_or_insert(now);
        self.waiting = true;
        allowed(since, now, last_input, starting)
    }
    /// Starts a pass of background work.
    pub fn start_pass(&mut self) {
        self.waiting = false;
    }
    /// Ends a pass of background work that went through all of it without doing anything. With
    /// nothing of it left waiting, its next write waits from when it is asked for.
    pub fn end_pass(&mut self) {
        if !self.waiting {
            self.since = None;
        }
    }
    /// The next record to write at `now`: the first whose backoff has ended and whose own wait
    /// allows it, rules files first, since a profile's roles summary follows its saved rules.
    pub fn next(&self, now: u64, last_input: Option<u64>, starting: bool) -> Option<Record> {
        let due = |e: &&Entry| {
            e.retry.due(now)
                && !self.blocked(&e.record)
                && allowed(e.waiting_from(), now, last_input, starting)
        };
        self.records
            .iter()
            .filter(|e| e.record.is_rules())
            .find(due)
            .or_else(|| self.records.iter().find(due))
            .map(|e| e.record.clone())
    }
    /// Whether `record` waits for another to be written first: a profile's roles summary waits
    /// for its rules.
    fn blocked(&self, record: &Record) -> bool {
        let Record::Roles(id, _) = record else {
            return false;
        };
        self.records
            .iter()
            .any(|e| matches!(e.record, Record::Rules(rules, _) if rules == *id))
    }
    /// Starts a flush, which tries every record once, whatever its backoff.
    pub fn start_flush(&mut self) {
        for entry in &mut self.records {
            entry.flushed = false;
        }
    }
    /// The next record the flush in progress writes: one it has not tried and that waits for no
    /// other, rules files first.
    pub fn next_flushed(&mut self) -> Option<Record> {
        let open = |e: &Entry| !e.flushed && !self.blocked(&e.record);
        let index = self
            .records
            .iter()
            .position(|e| e.record.is_rules() && open(e))
            .or_else(|| self.records.iter().position(open))?;
        let entry = &mut self.records[index];
        entry.flushed = true;
        Some(entry.record.clone())
    }
    /// `record`'s file has been written.
    pub fn saved(&mut self, record: &Record) {
        self.records.retain(|e| !e.record.same(record));
    }
    /// Profile `id`'s rules file has been written with rules whose roles are `roles`: a roles
    /// summary waiting to follow it takes them.
    pub fn follow_rules(&mut self, id: u64, roles: Roles) {
        for entry in &mut self.records {
            if let Record::Roles(p, summary) = &mut entry.record
                && *p == id
            {
                *summary = roles;
            }
        }
    }
    /// The table waiting to be saved as profile `id`'s rules.
    pub fn map(&self, id: u64) -> Option<&profiles::Map> {
        self.maps().find(|(p, _)| *p == id).map(|(_, map)| map)
    }
    /// The roles summary waiting to be saved for profile `id`.
    pub fn roles(&self, id: u64) -> Option<Roles> {
        self.records.iter().find_map(|e| match e.record {
            Record::Roles(p, roles) if p == id => Some(roles),
            _ => None,
        })
    }
    /// Writing `record` failed at `now` with `error`; it stays dirty and backs off.
    pub fn failed(&mut self, record: &Record, error: storage::Error, now: u64) {
        if let Some(entry) = self.records.iter_mut().find(|e| e.record.same(record)) {
            entry.failure = Some(error);
            match entry.record {
                Record::Rules(..) => entry.retry.failed_up_to(now, RULES_RETRY_MAX_MS),
                _ => entry.retry.failed(now),
            }
        }
    }
    /// Profile `id`'s rules file has been written by other means.
    pub fn rules_saved(&mut self, id: u64) {
        self.records
            .retain(|e| !matches!(e.record, Record::Rules(rules, _) if rules == id));
    }
    /// Profile `id` no longer exists, so nothing of it is written.
    pub fn forget_profile(&mut self, id: u64) {
        self.records
            .retain(|e| !matches!(e.record, Record::Rules(p, _) | Record::Roles(p, _) if p == id));
    }
    /// The profile tables waiting to be saved.
    pub fn maps(&self) -> impl Iterator<Item = (u64, &profiles::Map)> {
        self.records.iter().filter_map(|e| match &e.record {
            Record::Rules(id, map) => Some((*id, map)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(id: u64) -> Record {
        Record::DeviceRoles(id, Roles::default())
    }

    #[test]
    fn a_record_waits_for_quiet_input_up_to_its_longest_wait() {
        let mut dirty = Dirty::default();
        assert!(dirty.mark(roles(1), 1000));
        // Input keeps arriving.
        assert!(dirty.next(1500, Some(1490), false).is_none());
        assert!(dirty.next(1500, Some(1200), false).is_some());
        // A connection waiting for its first input holds it up, until the longest wait.
        assert!(dirty.next(1500, None, true).is_none());
        assert!(dirty.next(1000 + MAX_DELAY_MS, Some(2990), true).is_some());
    }

    #[test]
    fn a_record_backing_off_neither_holds_up_nor_hurries_others() {
        let mut dirty = Dirty::default();
        assert!(dirty.mark(roles(1), 0));
        dirty.failed(&roles(1), storage::Error::Io, 100);
        // Long after the first record became dirty, a new one still waits for input to pause.
        let now = 100 + u64::from(crate::devices::RETRY_DELAY_MS) - 10;
        assert!(dirty.mark(roles(2), now - 100));
        assert!(dirty.next(now, Some(now - 1), false).is_none());
        assert!(matches!(
            dirty.next(now, Some(now - QUIET_MS), false),
            Some(Record::DeviceRoles(2, _))
        ));
        // Background work, too.
        dirty.start_pass();
        assert!(!dirty.wait(now, Some(now - 1), false));
        assert!(!dirty.wait(now, None, true));
        // Once its backoff ends, the failed record waits for input to pause again, up to its
        // longest wait from then.
        dirty.saved(&roles(2));
        let retry = 100 + u64::from(crate::devices::RETRY_DELAY_MS);
        assert!(dirty.next(retry, Some(retry - 1), false).is_none());
        assert!(dirty.next(retry + 10, Some(retry), false).is_none());
        assert!(
            dirty
                .next(retry + MAX_DELAY_MS, Some(retry + MAX_DELAY_MS), false)
                .is_some()
        );
    }

    #[test]
    fn a_flush_tries_every_record_once_rules_first() {
        let mut dirty = Dirty::default();
        let map = profiles::Map::default();
        assert!(dirty.mark(roles(1), 0));
        assert!(dirty.mark(Record::Roles(7, Roles::default()), 0));
        assert!(dirty.mark(Record::Rules(7, map), 0));
        dirty.failed(&roles(1), storage::Error::Io, 0);
        dirty.start_flush();
        assert!(matches!(dirty.next_flushed(), Some(Record::Rules(7, _))));
        // The roles summary waits for its rules.
        assert!(matches!(
            dirty.next_flushed(),
            Some(Record::DeviceRoles(1, _))
        ));
        assert!(dirty.next_flushed().is_none());
        dirty.rules_saved(7);
        assert!(matches!(dirty.next_flushed(), Some(Record::Roles(7, _))));
        assert!(dirty.next_flushed().is_none());
    }
}
