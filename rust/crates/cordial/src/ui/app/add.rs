//! Add Device: scans with one adapter while the dialog is open, pairs a nearby device, answers
//! its prompts, and reports the adapter's pairing result. Closing the dialog stops
//! the scan and cancels a pairing that hasn't saved its bond. A pairing outlives its adapter's
//! session while the adapter is held, so its confirmed result survives an announced USB reconnect.
use super::{
    Action, App, Dialog, Job, Kind, Menu, Page, SCAN_SECONDS,
    fleet::{Fleet, Slot},
    words::{self, Scope},
};
use crate::{
    commands,
    controller::{Command, Outcome, Target},
    error::Error,
    model::{self, Prompt},
};
use cordial_protocol::{self as p, CodeKind, ErrorCode, ErrorReason};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Pairing,
    Paired,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Pairing {
    job: (Slot, crate::controller::Ticket),
    pending_result: Option<(Job, Result<Outcome, Error>)>,
    pub adapter: String,
    pub slot: Slot,
    pub candidate: u32,
    pub name: String,
    pub phase: Phase,
    pub message: Option<String>,
    /// The saved device, once the bond is saved.
    pub device: Option<u32>,
    pub prompt_error: Option<String>,
    /// The prompt the code field was set up for.
    pub prompt_key: String,
}

/// The scan the dialog follows.
#[derive(Clone, Debug)]
pub struct Scan {
    pub adapter: String,
    pub slot: Slot,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AddDevice {
    /// The chosen adapter, kept while the dialog is open.
    pub adapter: Option<String>,
    pub show_unnamed: bool,
    pub error: Option<String>,
    /// A scan start is waiting for its answer, and its job.
    pub starting: bool,
    pub starting_job: Option<(Slot, crate::controller::Ticket)>,
    pub scan: Option<Scan>,
    pub pairing: Option<Pairing>,
}

impl AddDevice {
    /// Ends the session's scan, and its adapter's pairing unless the adapter is held.
    pub fn session_ended(&mut self, id: &str, slot: Slot, held: bool) {
        if self.scan.as_ref().is_some_and(|s| s.slot == slot) {
            self.scan = None;
            self.starting = false;
        }
        if !held && self.pairing.as_ref().is_some_and(|p| p.adapter == id) {
            self.pairing = None;
        }
    }
}

/// A typed pairing code the prompt accepts.
pub(crate) fn code_ok(kind: CodeKind, value: &str) -> bool {
    commands::check_code(kind, value).is_ok()
}

impl<F: Fleet> App<F> {
    /// Opens Add Device with the first ready adapter that has room, else the first ready one.
    pub(crate) fn open_add(&mut self) {
        let ready = self.ready_adapters();
        let Some(first) = ready
            .iter()
            .find(|a| !a.status.as_ref().is_some_and(model::storage_full))
            .or(ready.first())
            .map(|a| a.id.clone())
        else {
            return;
        };
        self.open_add_to(&first);
    }
    /// Opens Add Device with this adapter, when it is ready. An open dialog keeps its request, so
    /// a shortcut or the tray can't replace a scan or pairing in progress.
    pub(crate) fn open_add_to(&mut self, adapter: &str) {
        if !self.can_add_to(adapter) || self.dialog.is_some() || self.add.is_some() {
            return;
        }
        self.menu = None;
        self.add = Some(AddDevice {
            adapter: Some(adapter.to_owned()),
            ..AddDevice::default()
        });
        self.dialog = Some(Dialog::AddDevice);
        self.start_scan(adapter);
    }
    /// Whether the pairing is running in a session that can still cancel it.
    fn pairing_live(&self, pairing: &Pairing) -> bool {
        pairing.phase == Phase::Pairing
            && self.session_of(&pairing.adapter).map(|s| s.slot) == Some(pairing.slot)
    }
    /// Whether Add Device can open with this adapter.
    pub fn can_add_to(&self, adapter: &str) -> bool {
        self.ready_adapters().iter().any(|a| a.id == adapter)
    }
    /// Closes Add Device, stopping its scan and cancelling a pairing that is still running. A
    /// pairing's result that arrives later is ignored.
    pub(crate) fn close_add(&mut self) {
        if let Some(add) = self.add.take() {
            // A reopened dialog waits for the current session's pairing to finish before scanning.
            self.jobs.retain(|_, j| {
                !matches!(j.kind, Kind::Pair) || self.sessions.iter().any(|s| s.slot == j.slot)
            });
            if let Some(pairing) = &add.pairing
                && self.pairing_live(pairing)
            {
                let adapter = pairing.adapter.clone();
                self.run(&adapter, Kind::CancelPairing, Command::CancelPairing);
            }
            if let Some(job) = add.starting_job.filter(|_| add.starting) {
                // The scan stops once its start is answered.
                self.scan_stops.insert(job);
            } else if let Some(scan) = &add.scan
                && self
                    .states
                    .get(&scan.slot)
                    .is_some_and(|st| st.scanning.is_some())
            {
                let adapter = scan.adapter.clone();
                self.run(&adapter, Kind::Quiet, Command::ScanStop);
            }
        }
        if self.dialog == Some(Dialog::AddDevice) {
            self.dialog = None;
        }
    }
    /// Whether a pairing command must finish before this adapter can scan again.
    pub fn scan_waiting(&self, adapter: &str) -> bool {
        self.running(adapter, |k| {
            matches!(k, Kind::Pair | Kind::Answer { .. } | Kind::CancelPairing)
        })
    }
    pub(crate) fn start_scan(&mut self, adapter: &str) {
        if self.scan_waiting(adapter) {
            return;
        }
        let Some(add) = &mut self.add else { return };
        if add.starting {
            return;
        }
        add.error = None;
        let Some(status) = self.state_of(adapter).map(|st| st.status.clone()) else {
            if let Some(add) = &mut self.add {
                add.error = Some(words::ADAPTER_GONE.into());
            }
            return;
        };
        let supported = model::transports(&status);
        let problem = if supported.is_empty() {
            Some("This adapter can't search for devices.".to_owned())
        } else if model::enabled_transports(&status).is_empty() {
            supported.last().map(|t| words::transport_disabled_text(*t))
        } else {
            None
        };
        if let Some(problem) = problem {
            if let Some(add) = &mut self.add {
                add.error = Some(problem);
                add.scan = None;
            }
            return;
        }
        // Another adapter's scan stops first.
        let old = self
            .add
            .as_ref()
            .and_then(|a| a.scan.clone())
            .filter(|s| s.adapter != adapter);
        if let Some(old) = old
            && self
                .states
                .get(&old.slot)
                .is_some_and(|st| st.scanning.is_some())
        {
            self.run(&old.adapter, Kind::Quiet, Command::ScanStop);
        }
        let job = self.run_job(
            adapter,
            Kind::Scan,
            Command::Scan {
                transports: Vec::new(),
                seconds: SCAN_SECONDS,
            },
        );
        if let Some(add) = &mut self.add {
            add.starting = job.is_some();
            add.starting_job = job;
        }
    }
    /// Follows the dialog: closes it when its adapter can't add a device, starts a scan when
    /// none belongs to the chosen adapter, and follows the pairing. A pairing
    /// keeps the dialog open while its adapter has a session or is held.
    pub(crate) fn sync_add(&mut self) {
        if self.dialog != Some(Dialog::AddDevice) {
            if self.add.is_some() {
                self.close_add();
            }
            return;
        }
        let Some(add) = self.add.clone() else {
            self.dialog = None;
            return;
        };
        let Some(adapter) = add.adapter.clone() else {
            return;
        };
        let ready = self.ready_adapters().iter().any(|a| a.id == adapter);
        let pairing_here = add.pairing.as_ref().is_some_and(|p| p.adapter == adapter)
            && (self.session_of(&adapter).is_some() || self.held.contains_key(&adapter));
        if !ready && !pairing_here {
            self.close_add();
            return;
        }
        if let Some(pairing) = add.pairing.clone() {
            self.follow(pairing);
        } else if ready
            && !add.starting
            && add.scan.as_ref().is_none_or(|s| s.adapter != adapter)
            && add.error.is_none()
        {
            self.start_scan(&adapter);
        }
    }
    /// Follows the pairing's prompt in its own session.
    pub(crate) fn follow(&mut self, mut pairing: Pairing) {
        if pairing.phase == Phase::Pairing {
            let prompt = self
                .states
                .get(&pairing.slot)
                .and_then(|st| st.pairing.as_ref())
                .filter(|p| p.candidate == pairing.candidate && model::pairing_running(p))
                .and_then(model::prompt);
            let key = prompt
                .as_ref()
                .map_or_else(String::new, |p| format!("{p:?}"));
            if key != pairing.prompt_key {
                pairing.prompt_key = key;
                if prompt.is_some() {
                    pairing.prompt_error = None;
                }
                self.form.reset();
                if let Some(Prompt::EnterCode(kind)) = prompt {
                    self.form.limit = if kind == CodeKind::Pin { 16 } else { 6 };
                }
            }
        }
        if let Some(add) = &mut self.add {
            add.pairing = Some(pairing);
        }
    }
    pub(crate) fn add_form_changed(&mut self) {
        if let Some(p) = self.add.as_mut().and_then(|a| a.pairing.as_mut()) {
            p.prompt_error = None;
        }
    }
    /// Handles an Add Device action; false when `action` isn't one.
    pub(crate) fn add_action(&mut self, action: &Action) -> bool {
        let Some(add) = self.add.clone() else {
            return false;
        };
        let adapter = add.adapter.clone().unwrap_or_default();
        match action {
            Action::AddAdapter => {
                let items = self
                    .ready_adapters()
                    .iter()
                    .map(|a| (a.name.clone(), Some(Action::ChooseAdapter(a.id.clone()))))
                    .collect();
                self.menu = Some(Menu { items });
            }
            Action::ChooseAdapter(id) => {
                self.menu = None;
                if add.pairing.is_none()
                    && let Some(add) = &mut self.add
                {
                    add.adapter = Some(id.clone());
                    add.error = None;
                }
            }
            Action::ScanAgain | Action::AddAnother => {
                if let Some(add) = &mut self.add {
                    add.pairing = None;
                    add.scan = None;
                    add.error = None;
                }
                self.start_scan(&adapter);
            }
            Action::ShowUnnamed(on) => {
                if let Some(add) = &mut self.add {
                    add.show_unnamed = *on;
                }
            }
            Action::Pair(candidate) => self.start_pairing(&adapter, *candidate),
            Action::CancelPairing => self.close_add(),
            Action::Accept => self.answer(true),
            Action::Reject => self.answer(false),
            Action::Done => {
                let page = add
                    .pairing
                    .as_ref()
                    .and_then(|p| p.device.map(|d| Page::Device(p.adapter.clone(), d)));
                self.close_add();
                if let Some(page) = page {
                    self.open(page);
                }
            }
            Action::Cancel => self.close_add(),
            _ => return false,
        }
        true
    }
    pub(crate) fn start_pairing(&mut self, adapter: &str, candidate: u32) {
        let Some(add) = &self.add else { return };
        if add
            .pairing
            .as_ref()
            .is_some_and(|p| p.phase == Phase::Pairing)
        {
            if let Some(add) = &mut self.add {
                add.error = Some("Another device is being added.".into());
            }
            return;
        }
        if self.scan_waiting(adapter) {
            return;
        }
        let Some((slot, st)) = self
            .session_of(adapter)
            .and_then(|s| Some((s.slot, self.states.get(&s.slot)?)))
        else {
            if let Some(add) = &mut self.add {
                add.error = Some(words::ADAPTER_GONE.into());
            }
            return;
        };
        let Some(c) = st.candidate(candidate).cloned() else {
            return;
        };
        let problem = if model::storage_full(&st.status) {
            Some(words::error_text(
                &p::Error {
                    code: ErrorCode::NoCapacity as i32,
                    reason: ErrorReason::Storage as i32,
                    ..Default::default()
                },
                Scope::Save,
            ))
        } else if model::transport_disabled(&st.status, c.transport()) {
            Some(words::transport_disabled_text(c.transport()))
        } else {
            None
        };
        if let Some(problem) = problem {
            if let Some(add) = &mut self.add {
                add.error = Some(problem);
            }
            return;
        }
        let scanning = st.scanning.is_some();
        if scanning {
            self.run(adapter, Kind::Quiet, Command::ScanStop);
        }
        let name = words::clean(&c.name);
        let Some(job) = self.run_job(adapter, Kind::Pair, Command::Pair(Target::Id(candidate)))
        else {
            return;
        };
        if let Some(add) = &mut self.add {
            add.error = None;
            add.pairing = Some(Pairing {
                job,
                pending_result: None,
                adapter: adapter.to_owned(),
                slot,
                candidate,
                name: if name.is_empty() {
                    "the device".into()
                } else {
                    name
                },
                phase: Phase::Pairing,
                message: None,
                device: None,
                prompt_error: None,
                prompt_key: String::new(),
            });
        }
    }
    pub(crate) fn answer(&mut self, accept: bool) {
        let Some(pairing) = self.add.as_ref().and_then(|a| a.pairing.clone()) else {
            return;
        };
        if !self.pairing_live(&pairing) {
            if let Some(p) = self.add.as_mut().and_then(|a| a.pairing.as_mut()) {
                p.prompt_error = Some(words::ADAPTER_GONE.into());
            }
            return;
        }
        if self
            .jobs
            .values()
            .any(|j| matches!(j.kind, Kind::Answer { pairing: job, .. } if job == pairing.job))
        {
            return;
        }
        let prompt = self
            .states
            .get(&pairing.slot)
            .and_then(|st| st.pairing.as_ref())
            .filter(|p| p.candidate == pairing.candidate && model::answerable(p))
            .and_then(model::prompt);
        let Some(prompt) = prompt else {
            if let Some(p) = self.add.as_mut().and_then(|a| a.pairing.as_mut()) {
                p.prompt_error = Some("No pairing prompt is waiting.".into());
            }
            return;
        };
        let command = match (accept, &prompt) {
            (false, _) => Command::Reject,
            (true, Prompt::EnterCode(kind)) => {
                let value = self.form.value();
                if !code_ok(*kind, &value) {
                    return;
                }
                Command::Accept(Some(value))
            }
            (true, _) => Command::Accept(None),
        };
        self.run(
            &pairing.adapter,
            Kind::Answer {
                pairing: pairing.job,
                dismiss: !accept && matches!(prompt, Prompt::EnterCode(_)),
            },
            command,
        );
    }
    pub(crate) fn add_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let status = self.state_of(&job.adapter).map(|st| st.status.clone());
        // An error without the adapter's answer, after the pairing's session closed, leaves the
        // pairing's outcome unknown.
        let unknown = matches!(result, Err(e) if e.dongle.is_none())
            && !self.states.get(&job.slot).is_some_and(|st| st.available);
        let mut close = false;
        let mut deferred = None;
        let Some(add) = &mut self.add else { return };
        match (&job.kind, result) {
            (Kind::Scan, Ok(_)) => {
                add.starting = false;
                add.scan = Some(Scan {
                    adapter: job.adapter.clone(),
                    slot: job.slot,
                    error: None,
                });
            }
            (Kind::Scan, Err(e)) => {
                add.starting = false;
                let text = match &status {
                    Some(status) => {
                        let t = model::enabled_transports(status);
                        match t.first() {
                            Some(t) => words::transport_failure(e, status, *t, Scope::Save),
                            None => words::failure(e),
                        }
                    }
                    None => words::failure(e),
                };
                add.scan = Some(Scan {
                    adapter: job.adapter.clone(),
                    slot: job.slot,
                    error: Some(text),
                });
            }
            (Kind::Pair, result) => {
                let Some(pairing) = add.pairing.as_mut().filter(|p| {
                    p.slot == job.slot
                        && p.phase == Phase::Pairing
                        && matches!(job.command, Command::Pair(Target::Id(c)) if c == p.candidate)
                }) else {
                    return;
                };
                // The Pair result can arrive before Reject is acknowledged. Keep it until the
                // answer confirms dismissal or fails and the actual pairing result is needed.
                if self.jobs.values().any(|j| {
                    matches!(j.kind, Kind::Answer { pairing: key, dismiss: true } if key == pairing.job)
                }) {
                    pairing.pending_result = Some((job.clone(), result.clone()));
                    return;
                }
                match result {
                    Err(_) if unknown => close = true,
                    Ok(Outcome::PairedWithoutDevice { .. }) => {
                        pairing.device = None;
                        pairing.phase = Phase::Paired;
                    }
                    Ok(Outcome::Paired { subject, device }) => {
                        pairing.device = Some(subject.id);
                        pairing.phase = Phase::Paired;
                        if let Some(d) = device {
                            pairing.name = words::device_name(d);
                        }
                    }
                    Ok(_) => {}
                    // The short wording applies only while the error has nothing more to report.
                    Err(e)
                        if e.dongle.as_ref().is_some_and(|w| {
                            w.code() == ErrorCode::Cancelled && words::plain(w)
                        }) =>
                    {
                        pairing.phase = Phase::Cancelled;
                        pairing.message = Some("Pairing was cancelled.".into());
                    }
                    Err(e) => {
                        pairing.phase = Phase::Failed;
                        let transport = status.as_ref().zip(
                            self.states
                                .get(&job.slot)
                                .and_then(|st| st.candidate(pairing.candidate))
                                .map(p::Candidate::transport),
                        );
                        pairing.message = Some(match transport {
                            Some((status, t)) => {
                                words::transport_failure(e, status, t, Scope::Pair)
                            }
                            None => words::failure_in(e, Scope::Pair),
                        });
                    }
                }
            }
            (
                Kind::Answer {
                    pairing: key,
                    dismiss,
                },
                result,
            ) => {
                let Some(pairing) = add.pairing.as_mut().filter(|p| p.job == *key) else {
                    return;
                };
                if *dismiss && matches!(result, Ok(Outcome::Answered)) {
                    add.pairing = None;
                    add.scan = None;
                    add.error = None;
                    self.form.reset();
                } else if let Err(e) = result {
                    deferred = pairing.pending_result.take();
                    pairing.prompt_error = Some(words::failure(e));
                }
            }
            _ => {}
        }
        if close {
            self.close_add();
        }
        if let Some((job, result)) = deferred {
            self.add_result(&job, &result);
        }
    }
}
