//! Profiles: an adapter's profile memory, its configuration interfaces, each with a switch and a
//! profile staged until Save, and the listed profiles a page at a time, each labelled with its
//! roles, with creating, copying and deleting them at once. The picker of a profile for an
//! interface or a device's layers lists and pages on its own. Profile rules are never read or
//! changed here.
use super::{
    Action, App, Dialog, Job, Kind, Menu, PickFor, Spot,
    fleet::{Fleet, Slot},
    words,
    world::AdapterView,
};
use crate::{
    controller::{Command, Outcome, Progress, Target, Ticket},
    error::Error,
    profiles::{self, Deletion},
};
use cordial_client::order::profile_entry_id;
use cordial_protocol::{self as p, event, profile_list_entry};
use std::ops::Range;

/// What a deletion's confirmation says before listing the changes it makes.
pub const DELETION_INTRO: &str = "Deleting this profile makes these changes, in order:";
/// Shown below a running deletion's changes while the adapter restarts its USB configuration.
pub const DELETION_WAITING: &str = "Waiting for the adapter to reconnect.";

/// How one of the changes a deletion's confirmation lists stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mark {
    Pending,
    /// Being made.
    Active,
    /// The adapter confirmed it.
    Done,
    Failed,
    /// It may have been made, but the adapter's answer never arrived.
    Unconfirmed,
}

/// Where a deletion's confirmation stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    /// It asks to delete the profile, before a deletion runs or after one stopped short.
    Ask,
    Running,
    Deleted,
}

/// A deletion's confirmation as it shows now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletionView {
    pub stage: Stage,
    /// The profile's name.
    pub name: String,
    /// The changes, in order, each with its mark.
    pub steps: Vec<String>,
    pub marks: Vec<Mark>,
    /// The next change waits for the adapter to reconnect.
    pub waiting: bool,
    /// The dialog's note says the deletion's outcome is unknown, not that it failed.
    pub uncertain: bool,
}

/// A deletion the user confirmed, with the changes listed when it was confirmed.
#[derive(Clone, Debug)]
pub(crate) struct Deleting {
    adapter: String,
    job: (Slot, Ticket),
    name: String,
    steps: Vec<String>,
    /// The plan being made; its first change is `steps[base]`.
    plan: Deletion,
    /// Changes an earlier attempt made.
    base: usize,
    progress: Progress,
    end: Option<End>,
}

#[derive(Clone, Debug)]
enum End {
    Deleted,
    Failed,
    /// The note that says so.
    Unconfirmed(String),
}

/// The changes of `plan` being made once `done` of them are confirmed: the next device, every
/// interface, which change in one request, or the deletion itself.
fn active(plan: &Deletion, done: usize) -> Range<usize> {
    let (devices, interfaces) = (plan.devices.len(), plan.interfaces.len());
    if done < devices {
        done..done + 1
    } else if done == devices && interfaces > 0 {
        devices..devices + interfaces
    } else {
        devices + interfaces..devices + interfaces + 1
    }
}

/// Profiles shown on one page of the Profiles tab or the picker.
pub(crate) const PER_PAGE: usize = 10;

/// The adapter's profiles, as the Profiles tab or the picker shows them a page at a time.
#[derive(Clone, Debug, Default)]
pub struct ProfilePage {
    /// Every listed profile, readable or not, in ID order, kept current by profile events.
    pub entries: Vec<p::ProfileListEntry>,
    /// The shown page, kept within the listing as it shrinks.
    pub index: usize,
    /// The profiles have been listed.
    pub listed: bool,
    pub reading: bool,
    pub error: Option<String>,
}

impl ProfilePage {
    pub(crate) fn pages(&self) -> usize {
        self.entries.len().div_ceil(PER_PAGE).max(1)
    }

    /// Moves back to the last page when the listing no longer reaches the shown one.
    pub(crate) fn clamp(&mut self) {
        self.index = self.index.min(self.pages() - 1);
    }

    pub fn previous(&self) -> bool {
        self.index > 0
    }

    pub fn next(&self) -> bool {
        self.index + 1 < self.pages()
    }

    /// The shown page's readable profiles, and the read errors of those that can't be read.
    pub fn shown(&self) -> (Vec<&p::Profile>, Vec<&p::RecordError>) {
        split(
            self.entries
                .iter()
                .skip(self.index * PER_PAGE)
                .take(PER_PAGE),
        )
    }

    /// Every listed readable profile, and the read errors of those that can't be read.
    pub fn all(&self) -> (Vec<&p::Profile>, Vec<&p::RecordError>) {
        split(&self.entries)
    }

    pub(crate) fn step(&mut self, forward: bool) {
        self.index = if forward {
            self.index + 1
        } else {
            self.index.saturating_sub(1)
        };
        self.clamp();
    }

    pub(crate) fn put(&mut self, profile: &p::Profile) {
        let entry = p::ProfileListEntry {
            entry: Some(profile_list_entry::Entry::Profile(profile.clone())),
        };
        match self
            .entries
            .binary_search_by_key(&profile.id, profile_entry_id)
        {
            Ok(i) => self.entries[i] = entry,
            Err(i) => self.entries.insert(i, entry),
        }
    }
}

/// Readable profiles, and the read errors of those that can't be read.
fn split<'a>(
    entries: impl IntoIterator<Item = &'a p::ProfileListEntry>,
) -> (Vec<&'a p::Profile>, Vec<&'a p::RecordError>) {
    let mut profiles = Vec::new();
    let mut unreadable = Vec::new();
    for e in entries {
        match &e.entry {
            Some(profile_list_entry::Entry::Profile(profile)) => profiles.push(profile),
            Some(profile_list_entry::Entry::Error(r)) => unreadable.push(r),
            None => {}
        }
    }
    (profiles, unreadable)
}

impl<F: Fleet> App<F> {
    pub(crate) fn page_of(&self, adapter: &str, picker: bool) -> Option<&ProfilePage> {
        let s = self.session_of(adapter)?;
        Some(if picker { &s.picker } else { &s.list })
    }
    pub(crate) fn page_mut(&mut self, adapter: &str, picker: bool) -> Option<&mut ProfilePage> {
        let s = self.session_of_mut(adapter)?;
        Some(if picker { &mut s.picker } else { &mut s.list })
    }
    /// The page shown while the adapter is held, else its live page.
    pub fn shown_page(&self, adapter: &str, picker: bool) -> Option<ProfilePage> {
        self.page_of(adapter, picker).cloned().or_else(|| {
            self.held.get(adapter).map(|h| {
                if picker {
                    h.picker.clone()
                } else {
                    h.list.clone()
                }
            })
        })
    }
    /// Lists the profiles for the picker when `picker`, else for the Profiles tab, showing the
    /// first page when `first`.
    pub(crate) fn read_page(&mut self, adapter: &str, picker: bool, first: bool) {
        let Some(page) = self.page_mut(adapter, picker) else {
            return;
        };
        if first {
            page.index = 0;
        }
        page.reading = true;
        page.error = None;
        self.run(adapter, Kind::Page { picker }, Command::Profiles);
    }
    /// Shows the next listed page when `forward`, else the previous one.
    pub(crate) fn step_page(&mut self, adapter: &str, picker: bool, forward: bool) {
        if let Some(page) = self.page_mut(adapter, picker) {
            page.step(forward);
        }
    }
    pub(crate) fn open_picker(&mut self, adapter: &str) {
        self.read_page(adapter, true, true);
    }
    /// Whether the picker's failed listing can be read again: the adapter can be changed, no
    /// profile change is running, and no listing is.
    pub fn picker_retryable(&self, adapter: &str) -> bool {
        self.adapter(adapter)
            .is_some_and(|a| a.live().is_some() && self.adapter_editable(a))
            && !self.profiles_busy(adapter)
            && self
                .page_of(adapter, true)
                .is_some_and(|p| p.error.is_some() && !p.reading)
    }
    pub(crate) fn picker_open(&self, adapter: &str) -> bool {
        matches!(&self.dialog, Some(Dialog::Pick { adapter: a, .. }) if a == adapter)
    }
    /// Reads the names of profiles in use that nothing has named yet, each once per session.
    pub(crate) fn sync_profiles(&mut self) {
        let mut lookups = Vec::new();
        for s in &mut self.sessions {
            let Some(st) = self.states.get(&s.slot).filter(|st| st.ready()) else {
                continue;
            };
            for id in profiles::unnamed(st) {
                if s.looked_up.insert(id) {
                    lookups.push((s.id.clone(), id));
                }
            }
        }
        for (adapter, id) in lookups {
            self.run(&adapter, Kind::Lookup, Command::ProfileLookup(id));
        }
    }
    /// Follows a changed or removed profile in the listed profiles.
    pub(crate) fn profile_event(&mut self, adapter: &str, kind: &event::Kind) {
        for picker in [false, true] {
            let Some(page) = self.page_mut(adapter, picker).filter(|p| p.listed) else {
                continue;
            };
            match kind {
                event::Kind::Profile(profile) => page.put(profile),
                event::Kind::ProfileRemoved(removed) => {
                    page.entries.retain(|e| profile_entry_id(e) != removed.id);
                    page.clamp();
                }
                _ => {}
            }
        }
    }
    /// Fills the pages waiting for a list with a profile list response. It arrives in order with
    /// the events around it, so the profile events that follow it apply on top.
    pub(crate) fn profiles_listed(&mut self, adapter: &str, list: p::ProfileList) {
        let mut entries: Vec<_> = list
            .entries
            .into_iter()
            .filter(|e| e.entry.is_some())
            .collect();
        entries.sort_by_key(profile_entry_id);
        for picker in [false, true] {
            let Some(page) = self.page_mut(adapter, picker).filter(|p| p.reading) else {
                continue;
            };
            page.entries = entries.clone();
            page.listed = true;
            page.clamp();
        }
    }
    pub(crate) fn page_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Kind::Page { picker } = job.kind else {
            return;
        };
        let Some(page) = self.page_mut(&job.adapter, picker) else {
            return;
        };
        page.reading = false;
        if let Err(e) = result {
            page.error = Some(words::read_failure(e));
        }
    }
    pub fn profiles_busy(&self, adapter: &str) -> bool {
        self.running(adapter, |k| matches!(k, Kind::Profile))
    }
    /// Handles a profile action; false when `action` isn't one.
    pub(crate) fn profiles_action(&mut self, adapter: &str, action: &Action) -> bool {
        let Some(a) = self.adapter(adapter).cloned() else {
            return false;
        };
        let ready = a.live().is_some();
        let disabled = self.profiles_busy(adapter) || !self.adapter_editable(&a) || !ready;
        match action {
            Action::ProfilePage(next) => {
                if ready {
                    self.step_page(adapter, false, *next);
                }
            }
            Action::ProfilesRetry if !disabled => self.read_page(adapter, false, false),
            Action::ProfileMenu(id) if !disabled => {
                self.menu = Some(Menu {
                    items: vec![
                        ("Copy".into(), Some(Action::ProfileCopy(*id))),
                        ("Delete".into(), Some(Action::ProfileDelete(*id))),
                    ],
                });
            }
            Action::ProfileNew if !disabled => {
                self.menu = None;
                self.pick_after_new = None;
                self.form.limit = 64;
                self.form.set_value("");
                self.notes.remove(&Spot::Dialog);
                self.dialog = Some(Dialog::ProfileName(adapter.to_owned(), None));
            }
            Action::ProfileCopy(id) if !disabled => {
                self.menu = None;
                self.pick_after_new = None;
                let name = self.profile_text(adapter, *id);
                self.form.limit = 64;
                self.form.set_value(&profiles::copy_name(&name));
                self.notes.remove(&Spot::Dialog);
                self.dialog = Some(Dialog::ProfileName(adapter.to_owned(), Some(*id)));
            }
            Action::ProfileDelete(id) if !disabled => {
                self.menu = None;
                self.notes.remove(&Spot::Dialog);
                self.deleting = None;
                self.dialog = Some(Dialog::ProfileDelete(adapter.to_owned(), *id));
            }
            _ => return false,
        }
        true
    }
    /// Creates or copies the profile the name dialog names.
    pub(crate) fn submit_profile_name(&mut self, adapter: &str, copy: Option<u32>) {
        let available = self.adapter(adapter).and_then(AdapterView::live).is_some();
        if self.profiles_busy(adapter) || !available {
            return;
        }
        let name = self.form.value();
        let trimmed = name.trim();
        if !profiles::valid_name(trimmed) {
            self.notes
                .insert(Spot::Dialog, words::PROFILE_NAME_INVALID.into());
            return;
        }
        let command = match copy {
            Some(id) => Command::ProfileCopy(Target::Id(id), trimmed.to_owned()),
            None => Command::ProfileCreate(trimmed.to_owned()),
        };
        self.notes.remove(&Spot::Dialog);
        if !self.run(adapter, Kind::Profile, command) {
            self.notes.insert(Spot::Dialog, words::ADAPTER_GONE.into());
        }
    }
    /// What deleting a profile changes, as the adapter's view shows its users now.
    pub fn deletion(&self, adapter: &str, id: u32) -> Option<Deletion> {
        Some(Deletion::of(self.state_of(adapter)?, id))
    }
    /// What deleting a profile changes, in order, for its confirmation; empty when nothing uses
    /// the profile.
    pub fn deletion_steps(&self, adapter: &str, id: u32) -> Vec<String> {
        match self.deletion(adapter, id) {
            Some(plan) if !plan.devices.is_empty() || !plan.interfaces.is_empty() => {
                self.plan_steps(adapter, &plan, &self.profile_text(adapter, id))
            }
            _ => Vec::new(),
        }
    }
    /// The changes `plan` makes, in order, ending with the deletion of the profile `name` names.
    fn plan_steps(&self, adapter: &str, plan: &Deletion, name: &str) -> Vec<String> {
        let mut steps: Vec<String> = plan
            .devices
            .iter()
            .map(|d| {
                let name = self
                    .device(adapter, d.device)
                    .map_or_else(|| d.name.clone(), |v| v.name.clone());
                if plan.kept(d).is_empty() {
                    format!("Remove it from {name}")
                } else {
                    format!("Remove it from {name}, keeping the other profiles in order")
                }
            })
            .collect();
        steps.extend(plan.interfaces.iter().map(|u| {
            let label = profiles::interface_label(u.interface);
            if u.enabled {
                format!("Turn off {label} and clear its profile")
            } else {
                format!("Clear {label}'s profile")
            }
        }));
        steps.push(format!("Delete “{name}”"));
        steps
    }
    /// Keeps a deletion's confirmation open once the profile is deleted, until it is closed.
    pub fn keep_deleted(&mut self) {
        self.keep_deleted = true;
    }
    /// A deletion's confirmation as it shows now. A running or finished deletion lists the
    /// changes it was confirmed with. One that stopped short keeps the changes it made, then
    /// lists what deleting the profile still needs; its stopping point is marked while what it
    /// lists there is unchanged. Once confirmed, the profile keeps the name it had then, which
    /// the adapter may no longer know.
    pub fn deletion_view(&self, adapter: &str, id: u32) -> DeletionView {
        let run = self
            .deleting
            .as_ref()
            .filter(|r| r.adapter == adapter && r.plan.profile == id);
        let mut view = DeletionView {
            stage: Stage::Ask,
            name: self.profile_text(adapter, id),
            steps: self.deletion_steps(adapter, id),
            marks: Vec::new(),
            waiting: false,
            uncertain: false,
        };
        if let Some(r) = run {
            let done = r.base + r.progress.done;
            let active = active(&r.plan, r.progress.done);
            let at = r.base + active.start..r.base + active.end;
            let mark = match &r.end {
                None => {
                    view.stage = Stage::Running;
                    view.waiting = r.progress.waiting;
                    Mark::Active
                }
                Some(End::Deleted) => {
                    view.stage = Stage::Deleted;
                    Mark::Done
                }
                Some(End::Failed) => Mark::Failed,
                Some(End::Unconfirmed(note)) => {
                    view.uncertain = self.notes.get(&Spot::Dialog) == Some(note);
                    Mark::Unconfirmed
                }
            };
            view.name = r.name.clone();
            if view.stage != Stage::Ask {
                view.steps = r.steps.clone();
            } else if done > 0 {
                view.steps = r.steps[..done].to_vec();
                if let Some(plan) = self.deletion(adapter, id) {
                    view.steps.extend(self.plan_steps(adapter, &plan, &r.name));
                }
            }
            view.marks = (0..view.steps.len())
                .map(|i| {
                    if i < done || view.stage == Stage::Deleted {
                        Mark::Done
                    } else if at.contains(&i) && r.steps.get(i) == view.steps.get(i) {
                        mark
                    } else {
                        Mark::Pending
                    }
                })
                .collect();
        } else {
            view.marks = vec![Mark::Pending; view.steps.len()];
        }
        view
    }
    /// Follows a running deletion's progress.
    pub(crate) fn deletion_progress(&mut self, job: &Job, ticket: Ticket, progress: Progress) {
        if let Command::ProfileDeletePlanned(plan) = &job.command
            && let Some(r) = self.deleting.as_mut().filter(|r| {
                r.job == (job.slot, ticket)
                    && r.adapter == job.adapter
                    && r.plan == *plan
                    && r.end.is_none()
            })
        {
            r.progress = progress;
        }
    }
    /// Whether deleting profile `id` can start: the adapter is ready, no other profile change or
    /// settings write on it is running or waiting to be sent, and no change kept for it, staged
    /// or unsent, names the profile for a device's layers or a configuration interface. A kept
    /// interface change that leaves the profile unnamed doesn't set it on save, so it doesn't
    /// block.
    pub fn deletion_ready(&self, adapter: &str, id: u32) -> bool {
        self.adapter(adapter).and_then(AdapterView::live).is_some()
            && !self.profiles_busy(adapter)
            && !self.writes_waiting(adapter)
            && !self.drafts_use(adapter, id)
    }
    fn drafts_use(&self, adapter: &str, id: u32) -> bool {
        let layers = self
            .device_drafts
            .iter()
            .any(|((a, _), u)| a == adapter && u.layers.as_ref().is_some_and(|l| l.contains(&id)));
        let interfaces = self
            .adapter_drafts
            .get(adapter)
            .is_some_and(|u| u.interfaces.iter().any(|i| i.profile == Some(id)));
        layers || interfaces
    }
    /// Deletes a profile with the changes its confirmation showed. When the changes needed now
    /// differ, nothing is sent and the confirmation waits for an answer to the new ones.
    pub(crate) fn delete_profile(&mut self, adapter: &str, shown: &Deletion) {
        if !self.deletion_ready(adapter, shown.profile) {
            return;
        }
        let Some(plan) = self.deletion(adapter, shown.profile) else {
            return;
        };
        if plan != *shown {
            let name = self.profile_text(adapter, plan.profile);
            self.notes.insert(
                Spot::Dialog,
                format!(
                    "The changes needed to delete “{name}” changed. Review them and try again."
                ),
            );
            return;
        }
        // The changes it lists stay as they are now, after those an earlier attempt made.
        let view = self.deletion_view(adapter, plan.profile);
        self.notes.remove(&Spot::Dialog);
        if let Some(job) = self.run_job(
            adapter,
            Kind::Profile,
            Command::ProfileDeletePlanned(plan.clone()),
        ) {
            self.deleting = Some(Deleting {
                adapter: adapter.to_owned(),
                job,
                name: view.name,
                base: view.marks.iter().take_while(|m| **m == Mark::Done).count(),
                steps: view.steps,
                plan,
                progress: Progress::default(),
                end: None,
            });
        } else {
            self.notes.insert(Spot::Dialog, words::ADAPTER_GONE.into());
        }
    }
    /// A failed deletion: the cause, said to be incomplete once some of its changes took effect,
    /// or unconfirmed when a change may have been made without its answer arriving.
    fn deletion_failure(&self, adapter: &str, id: u32, e: &Error) -> String {
        let (cause, incomplete) = crate::commands::deletion_failure(e);
        let name = self.profile_text(adapter, id);
        let reason = words::failure(&cause);
        if cause.unconfirmed() {
            crate::ui::text::sentence(&crate::ui::text::deletion_uncertain(e))
        } else if incomplete {
            format!("The adapter couldn't finish deleting “{name}”. {reason} Try again.")
        } else {
            reason
        }
    }
    pub(crate) fn profile_result(
        &mut self,
        job: &Job,
        ticket: Ticket,
        result: &Result<Outcome, Error>,
    ) {
        let deleted = match &job.command {
            Command::ProfileDeletePlanned(plan) => Some(plan),
            _ => None,
        };
        let end = match (deleted, result) {
            (_, Ok(_)) => End::Deleted,
            (Some(plan), Err(e)) if e.unconfirmed() => {
                End::Unconfirmed(self.deletion_failure(&job.adapter, plan.profile, e))
            }
            (_, Err(_)) => End::Failed,
        };
        let run = self.deleting.as_mut().filter(|r| {
            r.job == (job.slot, ticket)
                && r.adapter == job.adapter
                && Some(&r.plan) == deleted
                && r.end.is_none()
        });
        let dialog_of_adapter = match (&self.dialog, deleted) {
            (Some(Dialog::ProfileDelete(a, id)), Some(plan)) => {
                *a == job.adapter && *id == plan.profile && run.is_some()
            }
            (Some(Dialog::ProfileName(a, _)), None) => *a == job.adapter,
            _ => false,
        };
        let kept = run.is_some() && result.is_ok() && self.keep_deleted;
        if let Some(r) = run {
            r.end = Some(end);
        }
        match result {
            Ok(outcome) => {
                if !dialog_of_adapter || kept {
                    return;
                }
                self.dialog = None;
                // A profile created from an empty picker returns to the picker, chosen.
                if let (Some(purpose), Outcome::Profile(created)) =
                    (self.pick_after_new.take(), outcome)
                {
                    self.dialog = Some(Dialog::Pick {
                        adapter: job.adapter.clone(),
                        purpose,
                        chosen: created.id,
                    });
                    self.open_picker(&job.adapter);
                }
            }
            Err(e) => {
                let text = match deleted {
                    Some(plan) => self.deletion_failure(&job.adapter, plan.profile, e),
                    None => words::failure(e),
                };
                if dialog_of_adapter
                    && self.session_of(&job.adapter).map(|s| s.slot) == Some(job.slot)
                {
                    self.notes.insert(Spot::Dialog, text);
                } else if deleted.is_some() {
                    if dialog_of_adapter {
                        self.dialog = None;
                        self.pick_after_new = None;
                        self.menu = None;
                        self.notes.remove(&Spot::Dialog);
                    }
                    self.toast(text);
                }
            }
        }
    }

    /// A running deletion owns a completion job even after loss; confirmed success stays open.
    pub(crate) fn deletion_kept(&self, adapter: &str, slot: Slot) -> bool {
        self.deleting.as_ref().is_some_and(|run| {
            run.adapter == adapter
                && matches!(&self.dialog, Some(Dialog::ProfileDelete(a, id))
                    if a == adapter && *id == run.plan.profile)
                && match run.end {
                    None => run.job.0 == slot && self.jobs.contains_key(&run.job),
                    Some(End::Deleted) => self.keep_deleted,
                    _ => false,
                }
        })
    }

    /// Keeps a completed failure visible when its adapter's loss closes the dialog.
    pub(crate) fn deletion_lost(&mut self, adapter: &str) {
        if matches!(&self.dialog, Some(Dialog::ProfileDelete(a, _)) if a == adapter)
            && self.deleting.as_ref().is_some_and(|run| {
                run.adapter == adapter && matches!(run.end, Some(End::Failed | End::Unconfirmed(_)))
            })
            && let Some(note) = self.notes.remove(&Spot::Dialog)
        {
            self.toast(note);
        }
    }

    /// Opens New Profile from the picker, which returns to it once the profile is created.
    pub(crate) fn pick_new_profile(&mut self) {
        let Some(Dialog::Pick {
            adapter, purpose, ..
        }) = self.dialog.clone()
        else {
            return;
        };
        let editable = self
            .adapter(&adapter)
            .is_some_and(|a| a.live().is_some() && self.adapter_editable(a));
        if !editable || self.profiles_busy(&adapter) {
            return;
        }
        self.form.limit = 64;
        self.form.set_value("");
        self.notes.remove(&Spot::Dialog);
        self.dialog = Some(Dialog::ProfileName(adapter, None));
        self.pick_after_new = Some(purpose);
    }
    /// Chooses a profile in the picker.
    pub(crate) fn choose_profile(&mut self, adapter: &str, purpose: PickFor, chosen: u32) {
        self.dialog = None;
        match purpose {
            PickFor::Interface(i) => self.stage_interface(adapter, i, None, Some(chosen)),
            PickFor::Layer(device) if chosen != 0 => self.add_layer(adapter, device, chosen),
            PickFor::Layer(_) => {}
        }
    }
}
