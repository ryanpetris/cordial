//! Add Device: scans with one adapter while the dialog is open, pairs a nearby device, answers
//! its prompts, and follows the new device until it connects or can't. Closing the dialog stops
//! the scan and cancels a pairing that hasn't saved its bond.
use super::{
    Action, Dialog, Job, Kind, Menu, Model, Page, SCAN_SECONDS,
    fleet::{Fleet, Slot},
    layout::{self, Layout, Tone, dim, err, ok, span, styled},
    render::{DialogView, busy, button_if},
    words,
};
use crate::{
    commands,
    controller::{Command, Outcome, Target},
    error::Error,
    model::{self, Prompt},
};
use cordial_protocol::{
    self as p, CapacityReason, CodeKind, DeviceState, ErrorCode, InactiveReason,
};
use ratatui::{style::Style, text::Line};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Pairing,
    Connecting,
    Connected,
    Saved,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Pairing {
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
    /// The error came from starting the scan, so trying again is a retry.
    pub scan_failed: bool,
    /// A scan start is waiting for its answer, and its job.
    pub starting: bool,
    pub starting_job: Option<(Slot, crate::controller::Ticket)>,
    pub scan: Option<Scan>,
    pub pairing: Option<Pairing>,
}

impl AddDevice {
    pub fn session_ended(&mut self, slot: Slot) {
        if self.scan.as_ref().is_some_and(|s| s.slot == slot) {
            self.scan = None;
            self.starting = false;
        }
        if self.pairing.as_ref().is_some_and(|p| p.slot == slot) {
            self.pairing = None;
        }
    }
}

/// A typed pairing code the prompt accepts.
fn code_ok(kind: CodeKind, value: &str) -> bool {
    commands::check_code(kind, value).is_ok()
}

/// Signal bars for an RSSI, with the unlit ones dim.
fn signal(rssi: Option<i32>) -> Line<'static> {
    let Some(rssi) = rssi else {
        return Line::default();
    };
    let n = match rssi {
        r if r >= -55 => 4,
        r if r >= -65 => 3,
        r if r >= -75 => 2,
        _ => 1,
    };
    let bars: Vec<char> = "▂▄▆█".chars().collect();
    Line::from(vec![
        span(bars[..n].iter().collect::<String>(), Style::new()),
        span(bars[n..].iter().collect::<String>(), dim()),
    ])
}

impl<F: Fleet> Model<F> {
    /// Opens Add Device with the first ready adapter that has room, else the first ready one.
    pub(super) fn open_add(&mut self) {
        let ready = self.ready_adapters();
        let Some(first) = ready
            .iter()
            .find(|a| !a.status.as_ref().is_some_and(model::storage_full))
            .or(ready.first())
            .map(|a| a.id.clone())
        else {
            return;
        };
        self.menu = None;
        self.add = Some(AddDevice {
            adapter: Some(first.clone()),
            ..AddDevice::default()
        });
        self.dialog = Some(Dialog::AddDevice);
        self.dialog_scroll = 0;
        self.form_focused = false;
        self.start_scan(&first);
    }

    /// Closes Add Device, stopping its scan and cancelling a pairing that is still running.
    pub(super) fn close_add(&mut self) {
        if let Some(add) = self.add.take() {
            if let Some(pairing) = &add.pairing
                && pairing.phase == Phase::Pairing
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
        self.form_focused = false;
    }

    fn start_scan(&mut self, adapter: &str) {
        let Some(add) = &mut self.add else { return };
        if add.starting {
            return;
        }
        add.error = None;
        add.scan_failed = false;
        let Some(status) = self.state_of(adapter).map(|st| st.status.clone()) else {
            if let Some(add) = &mut self.add {
                add.error = Some(words::ADAPTER_GONE.into());
                add.scan_failed = true;
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
                add.scan_failed = true;
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
    /// none belongs to the chosen adapter, and follows the pairing and the new device.
    pub(super) fn sync_add(&mut self) {
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
        let pairing_here = add.pairing.as_ref().is_some_and(|p| p.adapter == adapter);
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

    /// Follows the pairing's prompt and the new device's connection.
    fn follow(&mut self, mut pairing: Pairing) {
        let st = self.states.get(&pairing.slot);
        match pairing.phase {
            Phase::Pairing => {
                let prompt = st
                    .and_then(|st| st.pairing.as_ref())
                    .filter(|p| p.candidate == pairing.candidate && model::pairing_running(p))
                    .and_then(model::prompt);
                let key = prompt
                    .as_ref()
                    .map_or_else(String::new, |p| format!("{p:?}"));
                if key != pairing.prompt_key {
                    pairing.prompt_key = key;
                    pairing.prompt_error = None;
                    self.form.reset();
                    self.form_focused = false;
                    if let Some(Prompt::EnterCode(kind)) = prompt {
                        self.form.limit = if kind == CodeKind::Pin { 16 } else { 6 };
                        self.form_focused = true;
                    }
                }
            }
            Phase::Connecting => {
                if let Some(d) = pairing
                    .device
                    .and_then(|id| st.and_then(|st| st.device(id)))
                {
                    pairing.name = words::device_name(d);
                    match (d.state(), model::inactive(d), model::last_error(d)) {
                        (DeviceState::Connected, ..) => pairing.phase = Phase::Connected,
                        (_, Some(InactiveReason::Disabled), _) => {
                            pairing.phase = Phase::Saved;
                            pairing.message = Some("The device is paired but turned off in Cordial. Turn on “Use This Device” to connect it.".into());
                        }
                        (_, Some(_), _) => {
                            pairing.phase = Phase::Saved;
                            pairing.message = Some(format!(
                                "The device is paired but can't connect. {}",
                                words::inactive_text(d)
                            ));
                        }
                        (DeviceState::Disconnected, None, Some(code)) => {
                            pairing.phase = Phase::Saved;
                            pairing.message = Some(format!(
                                "The device is paired, but the adapter couldn't connect to it. {}",
                                words::code_text(code)
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if let Some(add) = &mut self.add {
            add.pairing = Some(pairing);
        }
    }

    pub(super) fn add_form_changed(&mut self) {
        if let Some(p) = self.add.as_mut().and_then(|a| a.pairing.as_mut()) {
            p.prompt_error = None;
        }
    }

    /// Handles an Add Device action; false when `action` isn't one.
    pub(super) fn add_action(&mut self, action: &Action) -> bool {
        let Some(add) = self.add.clone() else {
            return false;
        };
        let adapter = add.adapter.clone().unwrap_or_default();
        match action {
            Action::AddAdapter => {
                let (x, y) = self
                    .hits
                    .iter()
                    .find(|h| h.action == *action)
                    .map_or((0, 0), |h| (h.x, h.y + 1));
                let items = self
                    .ready_adapters()
                    .iter()
                    .map(|a| (a.name.clone(), Some(Action::ChooseAdapter(a.id.clone()))))
                    .collect();
                self.menu = Some(Menu { x, y, items });
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
                    add.error = None;
                }
                self.form_focused = false;
                self.start_scan(&adapter);
            }
            Action::ShowUnnamed(on) => {
                if let Some(add) = &mut self.add {
                    add.show_unnamed = *on;
                }
            }
            Action::Pair(candidate) => self.start_pairing(&adapter, *candidate),
            Action::CancelPairing => {
                if let Some(p) = &add.pairing
                    && p.phase == Phase::Pairing
                {
                    let a = p.adapter.clone();
                    self.run(&a, Kind::CancelPairing, Command::CancelPairing);
                }
            }
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

    fn start_pairing(&mut self, adapter: &str, candidate: u32) {
        let Some(add) = &self.add else { return };
        if add
            .pairing
            .as_ref()
            .is_some_and(|p| p.phase == Phase::Pairing)
        {
            if let Some(add) = &mut self.add {
                add.error = Some("Another device is being added.".into());
                add.scan_failed = false;
            }
            return;
        }
        let Some((slot, st)) = self
            .session_of(adapter)
            .and_then(|s| Some((s.slot, self.states.get(&s.slot)?)))
        else {
            if let Some(add) = &mut self.add {
                add.error = Some(words::ADAPTER_GONE.into());
                add.scan_failed = false;
            }
            return;
        };
        let Some(c) = st.candidate(candidate).cloned() else {
            return;
        };
        let problem = if model::storage_full(&st.status) {
            Some(words::error_text(&p::Error {
                code: ErrorCode::NoCapacity as i32,
                reason: CapacityReason::Storage as i32,
                ..Default::default()
            }))
        } else if model::transport_disabled(&st.status, c.transport()) {
            Some(words::transport_disabled_text(c.transport()))
        } else {
            None
        };
        if let Some(problem) = problem {
            if let Some(add) = &mut self.add {
                add.error = Some(problem);
                add.scan_failed = false;
            }
            return;
        }
        let scanning = st.scanning.is_some();
        if scanning {
            self.run(adapter, Kind::Quiet, Command::ScanStop);
        }
        let name = words::clean(&c.name);
        if let Some(add) = &mut self.add {
            add.error = None;
            add.pairing = Some(Pairing {
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
        self.run(adapter, Kind::Pair, Command::Pair(Target::Id(candidate)));
    }

    fn answer(&mut self, accept: bool) {
        let Some(pairing) = self.add.as_ref().and_then(|a| a.pairing.clone()) else {
            return;
        };
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
        self.run(&pairing.adapter, Kind::Answer, command);
    }

    pub(super) fn add_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let status = self.state_of(&job.adapter).map(|st| st.status.clone());
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
                            Some(t) => words::transport_failure(e, status, *t),
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
                let Some(pairing) = add.pairing.as_mut().filter(|p| p.slot == job.slot) else {
                    return;
                };
                match result {
                    Ok(Outcome::Paired { subject, device }) => {
                        pairing.device = Some(subject.id);
                        pairing.phase = Phase::Connecting;
                        if let Some(d) = device {
                            pairing.name = words::device_name(d);
                        }
                    }
                    Ok(_) => {}
                    Err(e) if e.code_of() == Some(ErrorCode::Cancelled) => {
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
                            Some((status, t)) => words::transport_failure(e, status, t),
                            None => words::failure(e),
                        });
                    }
                }
            }
            (Kind::Answer, Err(e)) => {
                if let Some(pairing) = add.pairing.as_mut() {
                    pairing.prompt_error = if e.code_of() == Some(ErrorCode::NoPrompt) {
                        None
                    } else {
                        Some(words::failure(e))
                    };
                }
            }
            _ => {}
        }
    }

    /// The Add Device dialog.
    pub(super) fn add_dialog(&mut self, w: usize) -> DialogView {
        let add = self.add.clone().unwrap_or_default();
        let mut body = Layout::new(w);
        let mut buttons = Layout::new(w);
        let adapter = add.adapter.clone().unwrap_or_default();
        if let Some(pairing) = &add.pairing {
            self.pairing_view(pairing, &mut body, &mut buttons);
            return DialogView {
                title: "Add Device".into(),
                body,
                buttons,
            };
        }
        let ready = self.ready_adapters();
        if ready.len() > 1 {
            let name = self
                .adapter(&adapter)
                .map(|a| a.name.clone())
                .unwrap_or_default();
            let mut r = Layout::new(w);
            r.button(&format!("{name} ▾"), Action::AddAdapter, Tone::Normal);
            body.labelled(styled("Adapter", Style::new()), r);
            body.row();
        }
        let scan = add.scan.as_ref().filter(|s| s.adapter == adapter);
        let st = scan.and_then(|s| self.states.get(&s.slot));
        let scanning = add.starting || st.is_some_and(|st| st.scanning.is_some());
        let error = add
            .error
            .clone()
            .or_else(|| scan.and_then(|s| s.error.clone()));
        if scanning {
            body.line(busy("Searching…"));
        } else if let Some(e) = &error {
            let retry = scan.is_some_and(|s| s.error.is_some()) || add.scan_failed;
            let mut r = Layout::new(w);
            button_if(
                &mut r,
                if retry { "Retry" } else { "Refresh" },
                Action::ScanAgain,
                Tone::Normal,
                !add.starting,
            );
            body.hang(styled("! ", err()), e, err());
            body.add(r);
        } else {
            let mut r = Layout::new(w);
            button_if(
                &mut r,
                "Refresh",
                Action::ScanAgain,
                Tone::Normal,
                !add.starting,
            );
            body.add(r);
        }
        body.row();
        let all: Vec<p::Candidate> = st.map(|st| st.candidates.clone()).unwrap_or_default();
        let shown: Vec<&p::Candidate> = all
            .iter()
            .filter(|c| {
                add.show_unnamed
                    || !words::clean(&c.name).is_empty()
                    || !model::known_kinds(&c.kinds).is_empty()
            })
            .collect();
        let status = st.map(|st| st.status.clone()).unwrap_or_default();
        if shown.is_empty() {
            let text = if scanning {
                "Looking for devices in pairing mode…"
            } else {
                "No Devices Found"
            };
            body.line(styled(text, dim()));
        }
        for c in &shown {
            let t = c.transport();
            let mut r = Layout::new(w);
            r.lines.push(signal(c.rssi));
            if model::storage_full(&status) {
                r.label(words::STORAGE_FULL, dim());
            } else if model::transport_disabled(&status, t) {
                r.label(&words::transport_disabled_text(t), dim());
            } else {
                button_if(
                    &mut r,
                    "Pair",
                    Action::Pair(c.id),
                    Tone::Primary,
                    !add.starting,
                );
            }
            r.width = r.lines.iter().map(layout::line_width).max().unwrap_or(0);
            let first = Line::from(vec![span(words::candidate_name(c), layout::bold())]);
            body.labelled(first, r);
            body.line(styled(format!("  {}", words::transport_name(t)), dim()));
        }
        body.row();
        let hidden = all.len().saturating_sub(shown.len());
        let mut label = Line::from(span("Show Unnamed Devices", Style::new()));
        if !add.show_unnamed && hidden > 0 {
            label.spans.push(span(format!(" ({hidden} hidden)"), dim()));
        }
        body.labelled(
            label,
            layout::switch(
                Some(add.show_unnamed),
                Action::ShowUnnamed(!add.show_unnamed),
                true,
            ),
        );
        buttons.button_right("Cancel", Action::Cancel, Tone::Normal);
        DialogView {
            title: "Add Device".into(),
            body,
            buttons,
        }
    }

    fn pairing_view(&mut self, pairing: &Pairing, body: &mut Layout, buttons: &mut Layout) {
        let name = &pairing.name;
        let w = body.width;
        let mut right = Layout::new(w);
        match pairing.phase {
            Phase::Pairing => {
                let prompt = self
                    .states
                    .get(&pairing.slot)
                    .and_then(|st| st.pairing.as_ref())
                    .filter(|p| p.candidate == pairing.candidate && model::pairing_running(p))
                    .and_then(model::prompt);
                match prompt {
                    None => body.line(busy(&format!("Pairing with {name}…"))),
                    Some(Prompt::ShowCode(kind, code)) => {
                        let what = if kind == CodeKind::Pin { "PIN" } else { "code" };
                        body.para(
                            &format!("Type this {what} on {name}, then press Enter:"),
                            layout::plain(),
                        );
                        body.row();
                        body.line(styled(
                            format!("  {}", words::spaced(&code)),
                            layout::title(),
                        ));
                    }
                    Some(Prompt::ConfirmCode(code)) => {
                        body.para(&format!("Does {name} show this code?"), layout::plain());
                        body.row();
                        body.line(styled(
                            format!("  {}", words::spaced(&code)),
                            layout::title(),
                        ));
                        if let Some(e) = &pairing.prompt_error {
                            body.para(e, err());
                        }
                        right.button("Codes Differ", Action::Reject, Tone::Normal);
                        right.button("Codes Match", Action::Accept, Tone::Primary);
                    }
                    Some(Prompt::EnterCode(kind)) => {
                        let text = if kind == CodeKind::Pin {
                            format!("Enter the PIN for {name}.")
                        } else {
                            format!("Enter the six-digit passkey shown on {name}.")
                        };
                        body.para(&text, layout::plain());
                        body.row();
                        let line = self.field_line(w.min(24));
                        body.control(line, Action::Field);
                        if let Some(e) = &pairing.prompt_error {
                            body.para(e, err());
                        }
                        let valid = code_ok(kind, &self.form.value());
                        right.button("Reject", Action::Reject, Tone::Normal);
                        button_if(&mut right, "Pair", Action::Accept, Tone::Primary, valid);
                    }
                }
                right.button("Cancel Pairing", Action::CancelPairing, Tone::Normal);
            }
            Phase::Connecting => body.line(busy(&format!("Paired. Connecting to {name}…"))),
            Phase::Connected => {
                body.line(Line::from(vec![
                    span("✓ ", ok()),
                    span(format!("{name} is paired and connected."), Style::new()),
                ]));
                right.button("Add Another", Action::AddAnother, Tone::Normal);
                right.button("Done", Action::Done, Tone::Primary);
            }
            Phase::Saved => {
                body.line(Line::from(vec![
                    span("✓ ", ok()),
                    span(format!("{name} is paired."), Style::new()),
                ]));
                if let Some(m) = &pairing.message {
                    body.para(m, dim());
                }
                right.button("Add Another", Action::AddAnother, Tone::Normal);
                right.button("Done", Action::Done, Tone::Primary);
            }
            Phase::Failed => {
                body.line(Line::from(vec![
                    span("! ", err()),
                    span(format!("Cordial couldn't pair with {name}."), Style::new()),
                ]));
                if let Some(m) = &pairing.message {
                    body.para(m, dim());
                }
                right.button("Retry", Action::ScanAgain, Tone::Normal);
                right.button("Close", Action::Cancel, Tone::Primary);
            }
            Phase::Cancelled => {
                if let Some(m) = &pairing.message {
                    body.para(m, dim());
                }
                right.button("Refresh", Action::ScanAgain, Tone::Normal);
                right.button("Close", Action::Cancel, Tone::Primary);
            }
        }
        buttons.align_right(right);
    }
}
