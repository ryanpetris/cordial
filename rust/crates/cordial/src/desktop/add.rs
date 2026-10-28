//! Add Device: scans with one adapter while the dialog is open, pairs a nearby device, answers
//! its prompts, and shows the adapter's pairing result. Choosing a device and pairing it are
//! separate dialogs, so each keeps a size of its own. Closing either stops the scan and cancels a
//! pairing that hasn't saved its bond.
use super::{
    dialogs::{Form, Modal},
    shell::{Ctx, Ev},
    widgets::{
        Btn, Look, device_icon, note, para, set_label, set_look, set_subtitle, set_title, spin,
        spinner, when,
    },
};
use crate::{
    model::{self, Prompt},
    ui::app::{
        Action, App, Live,
        add::{AddDevice, Phase},
        words::{self, DisplayKind, Scope},
    },
};
use adw::prelude::*;
use cordial_protocol::{self as p, CodeKind};
use gtk::glib;
use std::rc::Rc;

/// The signal icon for an RSSI, with its name for assistive technologies.
fn signal(rssi: Option<i32>) -> Option<(&'static str, &'static str)> {
    Some(match rssi? {
        r if r >= -55 => (
            "network-cellular-signal-excellent-symbolic",
            "Excellent Signal",
        ),
        r if r >= -65 => ("network-cellular-signal-good-symbolic", "Good Signal"),
        r if r >= -75 => ("network-cellular-signal-ok-symbolic", "OK Signal"),
        _ => ("network-cellular-signal-weak-symbolic", "Weak Signal"),
    })
}

struct Candidate {
    row: adw::ActionRow,
    signal: gtk::Image,
    pair: Btn,
    why: gtk::Label,
}

/// The size of the list of nearby devices, which fill in after the dialog opens.
const SCAN_SIZE: (i32, i32) = (520, 560);
/// The pairing dialog's size, which holds every step and its answers without resizing between
/// them. Content that doesn't fit, such as a long reason, scrolls above the answers.
const PAIR_SIZE: (i32, i32) = (420, 372);
/// How far a step sits above the center of the space between the header and the answers. Half
/// the header's height centers it between the dialog's top edge and the answers.
const PAIR_RAISE: i32 = 23;
/// The most characters of a device's name a pairing step shows.
const PAIR_NAME: usize = 32;

/// The device's name cut to `PAIR_NAME` characters, so a long one doesn't fill the step.
fn short_name(name: &str) -> std::borrow::Cow<'_, str> {
    if name.chars().count() <= PAIR_NAME {
        return name.into();
    }
    let kept: String = name.chars().take(PAIR_NAME - 1).collect();
    format!("{}…", kept.trim_end()).into()
}

/// Lists nearby devices to pair.
pub struct AddDialog {
    form: Form,
    searching: gtk::Spinner,
    status_box: gtk::Box,
    status: gtk::Label,
    scan: Btn,
    candidates: adw::PreferencesGroup,
    rows: Vec<Candidate>,
    empty: gtk::Label,
    unnamed: adw::SwitchRow,
    cancel: Btn,
    ctx: Rc<Ctx>,
}

impl AddDialog {
    pub fn new(ctx: &Rc<Ctx>) -> Self {
        let form = Form::new("Add Device");
        form.dialog.set_content_width(SCAN_SIZE.0);
        form.dialog.set_content_height(SCAN_SIZE.1);
        let searching = spinner();
        searching.set_valign(gtk::Align::Center);
        searching.update_property(&[gtk::accessible::Property::Label("Searching")]);
        let search_slot = gtk::CenterBox::builder()
            .width_request(34)
            .height_request(34)
            .valign(gtk::Align::Center)
            .center_widget(&searching)
            .build();
        searching
            .bind_property("visible", &search_slot, "visible")
            .sync_create()
            .build();
        form.header.pack_start(&search_slot);
        let status = para();
        status.set_hexpand(true);
        let scan = Btn::new(ctx, "Search Again");
        let status_box = gtk::Box::builder().spacing(12).build();
        status_box.append(&status);
        status_box.append(&scan.widget);

        let candidates = adw::PreferencesGroup::new();
        let empty = gtk::Label::builder()
            .css_classes(["dim-label"])
            .margin_top(12)
            .margin_bottom(12)
            .build();
        let unnamed = adw::SwitchRow::builder()
            .title("Show Unnamed Devices")
            .build();
        unnamed.connect_active_notify(glib::clone!(
            #[weak]
            ctx,
            move |r| ctx.act(Action::ShowUnnamed(r.is_active()))
        ));
        let unnamed_group = adw::PreferencesGroup::new();
        unnamed_group.add(&unnamed);
        form.body.append(&status_box);
        form.body.append(&candidates);
        form.body.append(&empty);
        form.body.append(&unnamed_group);
        let cancel = Btn::new(ctx, "Cancel");
        form.button(&cancel);
        Self {
            form,
            searching,
            status_box,
            status,
            scan,
            candidates,
            rows: Vec::new(),
            empty,
            unnamed,
            cancel,
            ctx: ctx.clone(),
        }
    }

    fn show(&mut self, app: &App<Live>, add: &AddDevice) {
        self.cancel.set(Some(Action::Cancel));
        let adapter = add.adapter.clone().unwrap_or_default();

        let scan = add.scan.as_ref().filter(|s| s.adapter == adapter);
        let st = scan.and_then(|s| app.states.get(&s.slot));
        let scanning = add.starting
            || app.scan_waiting(&adapter)
            || st.is_some_and(|st| st.scanning.is_some());
        // A scan that ended in a failure keeps the candidates it found.
        let ended = st
            .filter(|st| st.scanning.is_none())
            .and_then(|st| st.last_scan.as_ref()?.error.as_ref())
            .map(|e| words::error_text(e, Scope::Read));
        let error = add
            .error
            .clone()
            .or_else(|| scan.and_then(|s| s.error.clone()))
            .or(ended);
        spin(&self.searching, scanning);
        self.status_box.set_visible(!scanning);
        set_label(&self.status, error.as_deref().unwrap_or(""));
        set_look(&self.status, Look::Error);
        self.scan.set(when(!scanning, Action::ScanAgain));

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
        self.empty.set_visible(shown.is_empty());
        set_label(
            &self.empty,
            if scanning {
                "Looking for devices in pairing mode…"
            } else {
                "No Devices Found"
            },
        );
        while self.rows.len() > shown.len() {
            if let Some(r) = self.rows.pop() {
                self.candidates.remove(&r.row);
            }
        }
        while self.rows.len() < shown.len() {
            let row = adw::ActionRow::new();
            let signal = gtk::Image::new();
            row.add_prefix(&signal);
            let why = gtk::Label::builder().css_classes(["dim-label"]).build();
            let pair = Btn::new(&self.ctx, "Pair");
            pair.widget.set_valign(gtk::Align::Center);
            // The visible label is Pair, and the accessible name says which device it pairs.
            pair.widget
                .reset_relation(gtk::AccessibleRelation::LabelledBy);
            row.add_suffix(&why);
            row.add_suffix(&pair.widget);
            self.candidates.add(&row);
            self.rows.push(Candidate {
                row,
                signal,
                pair,
                why,
            });
        }
        for (r, c) in self.rows.iter().zip(&shown) {
            let t = c.transport();
            let name = words::candidate_name(c);
            if r.row.title() != name {
                r.pair
                    .widget
                    .update_property(&[gtk::accessible::Property::Label(&format!("Pair {name}"))]);
            }
            set_title(&r.row, &name);
            set_subtitle(&r.row, words::transport_name(t));
            let signal = signal(c.rssi);
            let icon = signal.map(|(icon, _)| icon);
            if r.signal.icon_name().as_deref() != icon
                && let Some((icon, label)) = signal
            {
                r.signal.set_icon_name(Some(icon));
                r.signal
                    .update_property(&[gtk::accessible::Property::Label(label)]);
            }
            r.signal.set_visible(signal.is_some());
            let why = if model::storage_full(&status) {
                Some(words::STORAGE_FULL.to_owned())
            } else if model::transport_disabled(&status, t) {
                Some(words::transport_disabled_text(t))
            } else {
                None
            };
            note(&r.why, why.as_deref(), Look::Dim);
            r.pair
                .offer(why.is_none(), when(!add.starting, Action::Pair(c.id)));
        }
        self.candidates.set_visible(!shown.is_empty());
        let hidden = all.len().saturating_sub(shown.len());
        if self.unnamed.is_active() != add.show_unnamed {
            self.unnamed.set_active(add.show_unnamed);
        }
        set_subtitle(
            &self.unnamed,
            &if !add.show_unnamed && hidden > 0 {
                format!("{hidden} hidden")
            } else {
                String::new()
            },
        );
    }
}

impl Modal for AddDialog {
    fn dialog(&self) -> adw::Dialog {
        self.form.dialog.clone()
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) {
        if let Some(add) = app.add().filter(|a| a.pairing.is_none()).cloned() {
            self.show(app, &add);
        }
    }
}

/// How a pairing step marks the device's icon.
#[derive(Clone, Copy, PartialEq)]
enum Badge {
    None,
    Busy,
    Success,
    Error,
}

/// What a pairing step shows.
struct Step<'a> {
    name: &'a str,
    phase: Phase,
    prompt: Option<Prompt>,
    message: Option<&'a str>,
    prompt_error: Option<&'a str>,
    kind: DisplayKind,
    /// The typed code, which may be `limit` long and is `valid` to send.
    code: &'a str,
    limit: usize,
    valid: bool,
}

/// Pairs the chosen device, answers its prompts, and shows the result. The header keeps only its
/// close button, which cancels.
pub struct PairDialog {
    dialog: adw::Dialog,
    /// The step's answers, side by side below it.
    actions: gtk::Box,
    device_icon: gtk::Image,
    badge: gtk::Image,
    busy: gtk::Spinner,
    heading: gtk::Label,
    body: gtk::Label,
    reason: gtk::Label,
    code: gtk::Label,
    entry: gtk::Entry,
    prompt_error: gtk::Label,
    differ: Btn,
    matches: Btn,
    reject: Btn,
    accept: Btn,
    cancel_pairing: Btn,
    another: Btn,
    dismiss: Btn,
    again: Btn,
    close: Btn,
}

impl PairDialog {
    pub fn new(ctx: &Rc<Ctx>) -> Self {
        // The device's icon on a tinted disc, with the step's result or progress on a badge at
        // its corner. Equal margins around the disc leave room for the badge to overhang it while
        // the disc stays centered.
        let device_icon = gtk::Image::builder()
            .pixel_size(38)
            .width_request(72)
            .height_request(72)
            .css_classes(["pairing-icon", "accent"])
            .margin_start(4)
            .margin_end(4)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        let badge = gtk::Image::builder()
            .pixel_size(14)
            .width_request(26)
            .height_request(26)
            .css_classes(["pairing-badge"])
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .build();
        let busy = spinner();
        busy.set_halign(gtk::Align::Center);
        busy.set_valign(gtk::Align::Center);
        busy.update_property(&[gtk::accessible::Property::Label("Busy")]);
        let busy_badge = gtk::CenterBox::builder()
            .width_request(26)
            .height_request(26)
            .css_classes(["pairing-badge", "accent"])
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .center_widget(&busy)
            .build();
        busy.bind_property("visible", &busy_badge, "visible")
            .sync_create()
            .build();
        let icon = gtk::Overlay::builder()
            .child(&device_icon)
            .halign(gtk::Align::Center)
            .build();
        icon.add_overlay(&badge);
        icon.add_overlay(&busy_badge);
        let centered = |l: gtk::Label| {
            l.set_xalign(0.5);
            l.set_justify(gtk::Justification::Center);
            l
        };
        let heading = centered(para());
        heading.add_css_class("title-2");
        heading.set_margin_top(6);
        let body = centered(para());
        body.add_css_class("dim-label");
        let reason = centered(para());
        reason.add_css_class("dim-label");
        let code = gtk::Label::builder()
            .css_classes(["title-1", "numeric"])
            .selectable(true)
            .margin_top(8)
            .build();
        let entry = gtk::Entry::builder()
            .visible(false)
            .activates_default(true)
            .xalign(0.5)
            .width_request(200)
            .halign(gtk::Align::Center)
            .css_classes(["title-2", "numeric"])
            .margin_top(12)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label("Code")]);
        // Typing and pasting insert into the entry's text, which keeps only the digits of a
        // passkey, so "123 456" enters as 123456 before the length limit applies.
        if let Some(text) = entry.delegate().and_downcast::<gtk::Text>() {
            text.connect_insert_text(|text, new, at| {
                if text.input_purpose() == gtk::InputPurpose::Digits
                    && !new.bytes().all(|b| b.is_ascii_digit())
                {
                    let digits: String = new.chars().filter(char::is_ascii_digit).collect();
                    if !digits.is_empty() {
                        text.insert_text(&digits, at);
                    }
                    text.stop_signal_emission_by_name("insert-text");
                }
            });
        }
        entry.connect_changed(glib::clone!(
            #[weak]
            ctx,
            move |e| ctx.send(Ev::Form(e.text().into()))
        ));
        let prompt_error = centered(
            gtk::Label::builder()
                .accessible_role(gtk::AccessibleRole::Alert)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(["error"])
                .build(),
        );
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .vexpand(true)
            .valign(gtk::Align::Center)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(18)
            .margin_end(18)
            .build();
        for w in [
            icon.upcast_ref::<gtk::Widget>(),
            heading.upcast_ref(),
            body.upcast_ref(),
            reason.upcast_ref(),
            code.upcast_ref(),
            entry.upcast_ref(),
            prompt_error.upcast_ref(),
        ] {
            page.append(w);
        }
        let actions = gtk::Box::builder()
            .spacing(12)
            .homogeneous(true)
            .margin_top(6)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        // The step is centered above a spacer that wants twice `PAIR_RAISE` but needs none, so the
        // step sits higher by `PAIR_RAISE` and the spacer gives up its height before the step
        // has to scroll.
        let raised = gtk::Box::new(gtk::Orientation::Vertical, 0);
        raised.append(&page);
        raised.append(
            &gtk::Picture::builder()
                .paintable(&gtk::gdk::Paintable::new_empty(0, 2 * PAIR_RAISE))
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build(),
        );
        let toolbar = adw::ToolbarView::builder()
            .content(
                &gtk::ScrolledWindow::builder()
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .vexpand(true)
                    .child(&raised)
                    .build(),
            )
            .build();
        toolbar.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
        toolbar.add_bottom_bar(&actions);
        let dialog = adw::Dialog::builder()
            .title("Add Device")
            .content_width(PAIR_SIZE.0)
            .content_height(PAIR_SIZE.1)
            .child(&toolbar)
            .build();
        let d = Self {
            differ: Btn::new(ctx, "Codes Differ"),
            matches: Btn::suggested(ctx, "Codes Match"),
            reject: Btn::new(ctx, "Reject"),
            accept: Btn::suggested(ctx, "Pair"),
            cancel_pairing: Btn::new(ctx, "Cancel Pairing"),
            another: Btn::new(ctx, "Add Another"),
            dismiss: Btn::suggested(ctx, "Dismiss"),
            again: Btn::suggested(ctx, "Search Again"),
            close: Btn::new(ctx, "Close"),
            dialog,
            actions,
            device_icon,
            badge,
            busy,
            heading,
            body,
            reason,
            code,
            entry,
            prompt_error,
        };
        // Each step shows its own answers. Hidden until then, they don't widen the dialog as it
        // opens.
        for b in d.answers() {
            b.hide();
            d.actions.append(&b.widget);
        }
        d
    }

    fn answers(&self) -> [&Btn; 9] {
        [
            &self.cancel_pairing,
            &self.differ,
            &self.matches,
            &self.reject,
            &self.accept,
            &self.another,
            &self.dismiss,
            &self.again,
            &self.close,
        ]
    }

    /// Shows the step's answers left to right, the last being the main one that Enter activates.
    fn arrange(&self, shown: &[&Btn], default: bool) {
        for b in self.answers() {
            if !shown.iter().any(|s| s.widget == b.widget) {
                b.hide();
            }
        }
        let mut previous: Option<&gtk::Button> = None;
        for b in shown.iter().map(|b| &b.widget) {
            if b.prev_sibling().as_ref() != previous.map(|p| p.upcast_ref::<gtk::Widget>()) {
                self.actions.reorder_child_after(b, previous);
            }
            previous = Some(b);
        }
        let b = previous
            .filter(|_| default)
            .map(|b| b.upcast_ref::<gtk::Widget>());
        if self.dialog.default_widget().as_ref() != b {
            self.dialog.set_default_widget(b);
        }
    }

    /// Shows the pairing's step: its prompt while it runs, or its result.
    fn show(&self, step: Step) {
        let name = short_name(step.name);
        let heading;
        let body;
        let mut badge = Badge::None;
        let mut reason = None;
        let mut code = None;
        let mut entry = None;
        let mut prompt_error = None;
        match step.phase {
            Phase::Pairing => {
                prompt_error = step.prompt_error;
                self.cancel_pairing.set(Some(Action::CancelPairing));
                match step.prompt {
                    None => {
                        badge = Badge::Busy;
                        heading = "Pairing".to_owned();
                        body = format!("Pairing with {name}…");
                        self.arrange(&[&self.cancel_pairing], false);
                    }
                    Some(Prompt::ShowCode(kind, value)) => {
                        let (title, what) = if kind == CodeKind::Pin {
                            ("Type This PIN", "PIN")
                        } else {
                            ("Type This Code", "code")
                        };
                        heading = title.to_owned();
                        body = format!("Type this {what} on {name}, then press Enter.");
                        code = Some(words::spaced(&value));
                        self.arrange(&[&self.cancel_pairing], false);
                    }
                    Some(Prompt::ConfirmCode(value)) => {
                        heading = "Compare Codes".to_owned();
                        body = format!("Does {name} show this code?");
                        code = Some(words::spaced(&value));
                        self.arrange(&[&self.differ, &self.matches], true);
                        self.differ.set(Some(Action::Reject));
                        self.matches.set(Some(Action::Accept));
                    }
                    Some(Prompt::EnterCode(kind)) => {
                        (heading, body) = if kind == CodeKind::Pin {
                            ("Enter PIN".to_owned(), format!("Enter the PIN for {name}."))
                        } else {
                            (
                                "Enter Passkey".to_owned(),
                                format!("Enter the six-digit passkey shown on {name}."),
                            )
                        };
                        entry = Some(kind);
                        self.arrange(&[&self.reject, &self.accept], true);
                        self.reject.set(Some(Action::Reject));
                        self.accept.set(when(step.valid, Action::Accept));
                    }
                }
            }
            Phase::Paired => {
                badge = Badge::Success;
                heading = "Paired".to_owned();
                body = format!("{name} is paired.");
                self.arrange(&[&self.another, &self.dismiss], true);
                self.another.set(Some(Action::AddAnother));
                self.dismiss.set(Some(Action::Done));
            }
            Phase::Failed => {
                badge = Badge::Error;
                heading = "Pairing Failed".to_owned();
                body = format!("Cordial couldn't pair with {name}.");
                reason = step.message;
                self.arrange(&[&self.close, &self.again], true);
                self.again.set(Some(Action::ScanAgain));
                self.close.set(Some(Action::Cancel));
            }
            Phase::Cancelled => {
                heading = "Pairing Cancelled".to_owned();
                body = format!("Pairing with {name} was cancelled.");
                self.arrange(&[&self.close, &self.again], true);
                self.again.set(Some(Action::ScanAgain));
                self.close.set(Some(Action::Cancel));
            }
        }
        let icon = device_icon(step.kind);
        if self.device_icon.icon_name().as_deref() != Some(icon) {
            self.device_icon.set_icon_name(Some(icon));
        }
        spin(&self.busy, badge == Badge::Busy);
        let (badge_icon, badge_look) = match badge {
            Badge::Success => (Some("object-select-symbolic"), Look::Success),
            Badge::Error => (Some("window-close-symbolic"), Look::Error),
            Badge::None | Badge::Busy => (None, Look::Plain),
        };
        self.badge.set_visible(badge_icon.is_some());
        if self.badge.icon_name().as_deref() != badge_icon {
            self.badge.set_icon_name(badge_icon);
        }
        set_look(&self.badge, badge_look);
        set_label(&self.heading, &heading);
        set_label(&self.body, &body);
        note(&self.reason, reason, Look::Dim);
        note(&self.code, code.as_deref(), Look::Plain);
        let shown = self.entry.get_visible();
        self.entry.set_visible(entry.is_some());
        if let Some(kind) = entry {
            let purpose = if kind == CodeKind::Pin {
                gtk::InputPurpose::FreeForm
            } else {
                gtk::InputPurpose::Digits
            };
            if self.entry.input_purpose() != purpose {
                self.entry.set_input_purpose(purpose);
            }
            let limit = step.limit as i32;
            if self.entry.max_length() != limit {
                self.entry.set_max_length(limit);
            }
            let placeholder = (kind != CodeKind::Pin).then_some("000000");
            if self.entry.placeholder_text().as_deref() != placeholder {
                self.entry.set_placeholder_text(placeholder);
            }
            if self.entry.text() != step.code {
                self.entry.set_text(step.code);
                self.entry.set_position(-1);
            }
            if !shown {
                self.entry.grab_focus();
            }
        }
        note(&self.prompt_error, prompt_error, Look::Error);
    }
}

impl Modal for PairDialog {
    fn dialog(&self) -> adw::Dialog {
        self.dialog.clone()
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) {
        let Some(pairing) = app.add().and_then(|a| a.pairing.as_ref()) else {
            return;
        };
        let kind = app
            .states
            .get(&pairing.slot)
            .and_then(|st| st.candidate(pairing.candidate))
            .map(|c| words::display_kind(&c.kinds, &[]))
            // The pairing's session may have ended across a reconnect; the saved device remains.
            .or_else(|| {
                app.devices()
                    .iter()
                    .find(|d| d.adapter == pairing.adapter && Some(d.d.id) == pairing.device)
                    .map(|d| d.kind)
            })
            .unwrap_or(DisplayKind::Other);
        self.show(Step {
            name: &pairing.name,
            phase: pairing.phase,
            prompt: app.pairing_prompt(),
            message: pairing.message.as_deref(),
            prompt_error: pairing.prompt_error.as_deref(),
            kind,
            code: app.form(),
            limit: app.form_limit(),
            valid: app.form_valid(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Iterates the main loop until two frames have begun, which lays out the latest change.
    fn frames(widget: &impl IsA<gtk::Widget>) {
        let count = Rc::new(std::cell::Cell::new(0));
        let seen = count.clone();
        widget.add_tick_callback(move |_, _| {
            seen.set(seen.get() + 1);
            if seen.get() < 2 {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while count.get() < 2 {
            assert!(Instant::now() < deadline, "no frame was painted");
            glib::MainContext::default().iteration(false);
        }
    }

    /// Every step, named, for a device called `name`, with `reason` as each message and `error` as
    /// each prompt's error.
    fn steps<'a>(name: &'a str, reason: &'a str, error: &'a str) -> Vec<(&'static str, Step<'a>)> {
        let step = |phase, prompt, message, prompt_error| Step {
            name,
            phase,
            prompt,
            message,
            prompt_error,
            kind: DisplayKind::Keyboard,
            code: "12",
            limit: 6,
            valid: false,
        };
        let code = || "123456".to_owned();
        vec![
            ("pairing", step(Phase::Pairing, None, None, None)),
            (
                "pairing error",
                step(Phase::Pairing, None, None, Some(error)),
            ),
            (
                "show passkey",
                step(
                    Phase::Pairing,
                    Some(Prompt::ShowCode(CodeKind::Passkey, code())),
                    None,
                    None,
                ),
            ),
            (
                "show PIN",
                step(
                    Phase::Pairing,
                    Some(Prompt::ShowCode(CodeKind::Pin, "0000".into())),
                    None,
                    None,
                ),
            ),
            (
                "compare",
                step(
                    Phase::Pairing,
                    Some(Prompt::ConfirmCode(code())),
                    None,
                    Some(error),
                ),
            ),
            (
                "enter passkey",
                step(
                    Phase::Pairing,
                    Some(Prompt::EnterCode(CodeKind::Passkey)),
                    None,
                    Some(error),
                ),
            ),
            (
                "enter PIN",
                step(
                    Phase::Pairing,
                    Some(Prompt::EnterCode(CodeKind::Pin)),
                    None,
                    None,
                ),
            ),
            ("paired", step(Phase::Paired, None, None, None)),
            ("failed", step(Phase::Failed, None, Some(reason), None)),
            ("cancelled", step(Phase::Cancelled, None, None, None)),
        ]
    }

    #[test]
    fn signal_names_each_strength() {
        assert_eq!(signal(None), None);
        for (rssi, label) in [
            (-40, "Excellent Signal"),
            (-60, "Good Signal"),
            (-70, "OK Signal"),
            (-90, "Weak Signal"),
        ] {
            assert_eq!(signal(Some(rssi)).unwrap().1, label);
        }
    }

    #[test]
    #[ignore = "requires a display"]
    fn code_entry_takes_passkey_digits_and_pin_text() {
        adw::init().expect("GTK needs a display");
        let (ctx, _requests) = Ctx::test();
        let window = adw::Window::builder()
            .default_width(640)
            .default_height(720)
            .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
            .build();
        window.present();
        let d = PairDialog::new(&ctx);
        d.dialog.present(Some(&window));
        let step = |prompt, code, limit| Step {
            name: "Example Keys",
            phase: Phase::Pairing,
            prompt,
            message: None,
            prompt_error: None,
            kind: DisplayKind::Keyboard,
            code,
            limit,
            valid: false,
        };
        let passkey = || step(Some(Prompt::EnterCode(CodeKind::Passkey)), "", 6);
        let text = d.entry.delegate().and_downcast::<gtk::Text>().unwrap();
        let focused = |w: &gtk::Widget| {
            gtk::prelude::RootExt::focus(&window).is_some_and(|f| f == *w || f.is_ancestor(w))
        };
        let typed = |s: &str| {
            let mut at = text.text().chars().count() as i32;
            text.insert_text(s, &mut at);
        };
        let pasted = |s: &str| {
            d.entry.set_text("");
            d.entry.clipboard().set_text(s);
            text.emit_paste_clipboard();
            let deadline = Instant::now() + Duration::from_secs(5);
            while d.entry.text().is_empty() && Instant::now() < deadline {
                glib::MainContext::default().iteration(false);
            }
            d.entry.text()
        };

        d.show(step(None, "", 0));
        frames(&d.dialog);
        d.show(passkey());
        frames(&d.dialog);
        assert!(
            focused(d.entry.upcast_ref()),
            "the first code prompt focuses the entry"
        );
        assert_eq!(d.entry.input_purpose(), gtk::InputPurpose::Digits);
        assert_eq!(d.entry.max_length(), 6);
        typed("0a1 2");
        assert_eq!(d.entry.text(), "012");
        typed("3456789");
        assert_eq!(d.entry.text(), "012345");
        assert_eq!(pasted("123 456"), "123456");
        assert_eq!(pasted("Code: 0123-4567"), "012345");

        // A redraw of the same prompt leaves focus where the user moved it.
        d.reject.widget.grab_focus();
        d.show(step(
            Some(Prompt::EnterCode(CodeKind::Passkey)),
            "012345",
            6,
        ));
        frames(&d.dialog);
        assert!(focused(d.reject.widget.upcast_ref()));
        assert_eq!(d.entry.text(), "012345");

        d.show(step(Some(Prompt::EnterCode(CodeKind::Pin)), "", 16));
        assert_eq!(d.entry.input_purpose(), gtk::InputPurpose::FreeForm);
        assert_eq!(d.entry.max_length(), 16);
        assert_eq!(d.entry.text(), "");
        assert_eq!(pasted("0000 ab!~"), "0000 ab!~");
        typed("x".repeat(20).as_str());
        assert_eq!(d.entry.text().len(), 16);

        d.show(passkey());
        assert_eq!(d.entry.input_purpose(), gtk::InputPurpose::Digits);
        assert_eq!(d.entry.text(), "");
        d.dialog.force_close();
        let direct = PairDialog::new(&ctx);
        direct.dialog.present(Some(&window));
        direct.show(passkey());
        frames(&direct.dialog);
        assert!(
            focused(direct.entry.upcast_ref()),
            "opening directly on a code prompt focuses the entry"
        );
        direct.dialog.force_close();
        window.close();
    }

    #[test]
    fn short_name_cuts_long_names() {
        assert_eq!(short_name("Example Keys Mini"), "Example Keys Mini");
        let exact = "x".repeat(PAIR_NAME);
        assert_eq!(short_name(&exact), exact);
        assert_eq!(
            short_name("Example Keyboard With A Very Long Name"),
            "Example Keyboard With A Very Lo…"
        );
        // The cut doesn't leave a space before the ellipsis.
        assert_eq!(
            short_name("Example Keyboard With Wide Key Caps"),
            "Example Keyboard With Wide Key…"
        );
    }

    #[test]
    #[ignore = "requires a display"]
    fn pairing_centers_every_step_at_one_size() {
        adw::init().expect("GTK needs a display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let (ctx, _requests) = Ctx::test();
        let reason = "The adapter is busy. Try again when the current operation finishes.";
        let error = words::ADAPTER_GONE;
        let long = "Example Keyboard With A Name Long Enough To Wrap Across Several Lines";
        let overflow = reason.repeat(10);
        for (width, height) in [(640, 480), (480, 480), (480, 320)] {
            let short = height < PAIR_SIZE.1;
            let window = adw::Window::builder()
                .default_width(width)
                .default_height(height)
                .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
                .build();
            window.present();
            let d = PairDialog::new(&ctx);
            d.dialog.present(Some(&window));
            let page = d.heading.parent().unwrap();
            let scrolled = page
                .ancestor(gtk::ScrolledWindow::static_type())
                .and_downcast::<gtk::ScrolledWindow>()
                .unwrap();
            let mut sizes = Vec::new();
            let mut answers = Vec::new();
            for (name, reason, error, fits) in [
                ("Example Keys Mini", reason, error, true),
                (long, reason, error, true),
                (long, &overflow, &overflow, false),
            ] {
                for (label, step) in steps(name, reason, error) {
                    let error = step.prompt_error;
                    d.show(step);
                    frames(&d.dialog);
                    let at = format!("{label} at {width}x{height}");
                    assert_eq!(d.prompt_error.is_visible(), error.is_some(), "{at}");
                    if let Some(error) = error {
                        assert_eq!(d.prompt_error.text(), error, "{at}");
                    }
                    assert!(d.body.text().contains(&*short_name(name)), "{at}");
                    let content = d.dialog.child().unwrap();
                    let size = (content.width(), content.height());
                    assert!(
                        size.0 <= PAIR_SIZE.0 && size.0 < width && size.1 < height,
                        "{at}: {size:?} doesn't fit the window"
                    );
                    if !short {
                        assert_eq!(size.1, PAIR_SIZE.1, "{at}");
                    }
                    let view = scrolled.compute_bounds(&content).unwrap();
                    let bar = d.actions.compute_bounds(&content).unwrap();
                    assert!(
                        view.y() + view.height() <= bar.y(),
                        "{at}: the step overlaps the answers"
                    );
                    let v = scrolled.vadjustment();
                    let top = page.compute_bounds(&scrolled).unwrap().y();
                    let room = scrolled.height() - page.height();
                    if fits && !short {
                        assert!(v.upper() <= v.page_size(), "{at} scrolls");
                        let centered = (room / 2 - PAIR_RAISE).max(12) as f32;
                        assert!(
                            (top - centered).abs() <= 1.0,
                            "{at}: {top} isn't {centered}"
                        );
                    } else {
                        assert!(top >= 12.0, "{at}: the step's top is cut off at {top}");
                    }
                    if !fits && label == "failed" {
                        assert!(
                            v.upper() > v.page_size(),
                            "{at}: an overlong reason scrolls"
                        );
                    }
                    sizes.push(size);
                    if d.answers().iter().any(|b| b.widget.is_visible()) {
                        answers.push((bar.y(), bar.height()));
                    }
                }
            }
            sizes.dedup();
            // A window shorter than the dialog shrinks it, but no step resizes it.
            assert_eq!(sizes.len(), 1, "{width}x{height}: {sizes:?}");
            answers.dedup();
            assert_eq!(answers.len(), 1, "{width}x{height}: {answers:?}");
            d.dialog.force_close();
            window.close();
        }
    }
}
