//! Add Device: scans with one adapter while the dialog is open, pairs a nearby device, answers
//! its prompts, and follows the new device until it connects or can't. Closing the dialog stops
//! the scan and cancels a pairing that hasn't saved its bond.
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
        add::{AddDevice, Pairing, Phase},
        words::{self, Scope},
    },
};
use adw::prelude::*;
use cordial_protocol::{self as p, CodeKind};
use gtk::glib;
use std::{cell::Cell, rc::Rc};

/// The signal icon for an RSSI.
fn signal(rssi: Option<i32>) -> Option<&'static str> {
    Some(match rssi? {
        r if r >= -55 => "network-cellular-signal-excellent-symbolic",
        r if r >= -65 => "network-cellular-signal-good-symbolic",
        r if r >= -75 => "network-cellular-signal-ok-symbolic",
        _ => "network-cellular-signal-weak-symbolic",
    })
}

struct Candidate {
    row: adw::ActionRow,
    signal: gtk::Image,
    pair: Btn,
    why: gtk::Label,
}

/// The dialog's size while it lists nearby devices; pairing fits the dialog to its content.
const SCAN_SIZE: (i32, i32) = (520, 560);
const PAIR_WIDTH: i32 = 400;

pub struct AddDialog {
    form: Form,
    stack: gtk::Stack,
    /// The pairing step's answers, side by side under it.
    actions: gtk::Box,
    searching: gtk::Spinner,
    status_box: gtk::Box,
    status: gtk::Label,
    scan: Btn,
    candidates: adw::PreferencesGroup,
    rows: Vec<Candidate>,
    empty: gtk::Label,
    unnamed: adw::SwitchRow,
    device_icon: gtk::Image,
    badge: gtk::Image,
    busy: gtk::Spinner,
    heading: gtk::Label,
    body: gtk::Label,
    reason: gtk::Label,
    code: gtk::Label,
    entry: gtk::Entry,
    prompt_error: gtk::Label,
    cancel: Btn,
    differ: Btn,
    matches: Btn,
    reject: Btn,
    accept: Btn,
    cancel_pairing: Btn,
    another: Btn,
    dismiss: Btn,
    again: Btn,
    close: Btn,
    countdown: Countdown,
    ctx: Rc<Ctx>,
}

/// How a pairing step marks the device's icon.
#[derive(Clone, Copy, PartialEq)]
enum Badge {
    None,
    Busy,
    Success,
    Warning,
    Error,
}

/// How long a connected result stays up, in microseconds of frame time, before the dialog
/// finishes as Dismiss does.
const DISMISS_AFTER: i64 = 3_000_000;

/// The connected result's countdown, drawn as a thin bar along the dialog's bottom edge. Pointer
/// movement over the dialog pauses it until the pointer leaves, and a key press, click or tap
/// pauses it for good.
struct Countdown {
    bar: gtk::ProgressBar,
    tick: Option<gtk::TickCallbackId>,
    hovered: Rc<Cell<bool>>,
    held: Rc<Cell<bool>>,
}

impl Countdown {
    /// Lays the bar over the dialog's content and watches the dialog for interaction.
    fn new(dialog: &adw::Dialog) -> Self {
        let bar = gtk::ProgressBar::builder()
            .accessible_role(gtk::AccessibleRole::Presentation)
            .valign(gtk::Align::End)
            .visible(false)
            .build();
        // Replacing the class list would drop the orientation class that the theme sizes the
        // trough by.
        bar.add_css_class("countdown");
        let frame = gtk::Overlay::new();
        let content = dialog.child();
        dialog.set_child(Some(&frame));
        frame.set_child(content.as_ref());
        frame.add_overlay(&bar);
        let hovered = Rc::new(Cell::new(false));
        let held = Rc::new(Cell::new(false));
        // GTK can report motion without the pointer moving, such as after the layout under it
        // changes, so only motion to a new spot on the window counts as the user moving it.
        let at = Cell::new(None);
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[strong]
            hovered,
            move |c, _, _| {
                let now = c.current_event().and_then(|e| e.position());
                if at.replace(now).is_some_and(|was| Some(was) != now) {
                    hovered.set(true);
                }
            }
        ));
        motion.connect_leave(glib::clone!(
            #[strong]
            hovered,
            move |_| hovered.set(false)
        ));
        frame.add_controller(motion);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[strong]
            held,
            move |_, _, _, _| {
                held.set(true);
                glib::Propagation::Proceed
            }
        ));
        frame.add_controller(keys);
        let press = gtk::GestureClick::builder()
            .button(0)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        press.connect_pressed(glib::clone!(
            #[strong]
            held,
            move |_, _, _, _| held.set(true)
        ));
        frame.add_controller(press);
        Self {
            bar,
            tick: None,
            hovered,
            held,
        }
    }

    /// Starts counting from the first frame drawn, unless a count is already running or has
    /// finished. A pointer resting where the user clicked doesn't pause it until it moves.
    fn run(&mut self, ctx: &Rc<Ctx>) {
        if self.tick.is_some() {
            return;
        }
        self.hovered.set(false);
        self.held.set(false);
        self.bar.set_fraction(1.0);
        self.bar.set_visible(true);
        let hovered = self.hovered.clone();
        let held = self.held.clone();
        let ctx = Rc::downgrade(ctx);
        let last = Cell::new(None);
        let left = Cell::new(DISMISS_AFTER);
        self.tick = Some(self.bar.add_tick_callback(move |bar, clock| {
            let now = clock.frame_time();
            // A frame counts for at most a tenth of a second, so a hidden window, which draws no
            // frames, keeps its time when it returns.
            let step = (now - last.replace(Some(now)).unwrap_or(now)).clamp(0, 100_000);
            if !hovered.get() && !held.get() {
                left.set(left.get() - step);
            }
            bar.set_fraction(left.get().max(0) as f64 / DISMISS_AFTER as f64);
            if left.get() > 0 {
                return glib::ControlFlow::Continue;
            }
            if let Some(ctx) = ctx.upgrade() {
                ctx.act(Action::Done);
            }
            glib::ControlFlow::Break
        }));
    }

    fn stop(&mut self) {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
        self.bar.set_visible(false);
    }
}

impl AddDialog {
    pub fn new(ctx: &Rc<Ctx>) -> Self {
        let form = Form::new("Add Device");
        // Nearby devices fill in after the dialog opens.
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
        let scan = Btn::new(ctx, "Refresh");
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
        let scan_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        scan_page.append(&status_box);
        scan_page.append(&candidates);
        scan_page.append(&empty);
        scan_page.append(&unnamed_group);

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
            .activates_default(true)
            .input_purpose(gtk::InputPurpose::Digits)
            .xalign(0.5)
            .width_request(200)
            .halign(gtk::Align::Center)
            .css_classes(["title-2", "numeric"])
            .margin_top(12)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label("Code")]);
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
        let actions = gtk::Box::builder()
            .spacing(12)
            .homogeneous(true)
            .margin_top(18)
            .build();
        let pair_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_bottom(12)
            .build();
        for w in [
            icon.upcast_ref::<gtk::Widget>(),
            heading.upcast_ref(),
            body.upcast_ref(),
            reason.upcast_ref(),
            code.upcast_ref(),
            entry.upcast_ref(),
            prompt_error.upcast_ref(),
            actions.upcast_ref(),
        ] {
            pair_page.append(w);
        }

        let stack = gtk::Stack::builder().vhomogeneous(false).build();
        stack.add_named(&scan_page, Some("scan"));
        stack.add_named(&pair_page, Some("pair"));
        form.body.append(&stack);
        let countdown = Countdown::new(&form.dialog);

        let d = Self {
            cancel: Btn::new(ctx, "Cancel"),
            differ: Btn::new(ctx, "Codes Differ"),
            matches: Btn::suggested(ctx, "Codes Match"),
            reject: Btn::new(ctx, "Reject"),
            accept: Btn::suggested(ctx, "Pair"),
            cancel_pairing: Btn::new(ctx, "Cancel Pairing"),
            another: Btn::new(ctx, "Add Another"),
            dismiss: Btn::suggested(ctx, "Dismiss"),
            again: Btn::suggested(ctx, "Retry"),
            close: Btn::new(ctx, "Close"),
            form,
            stack,
            actions,
            searching,
            status_box,
            status,
            scan,
            candidates,
            rows: Vec::new(),
            empty,
            unnamed,
            device_icon,
            badge,
            busy,
            heading,
            body,
            reason,
            code,
            entry,
            prompt_error,
            countdown,
            ctx: ctx.clone(),
        };
        for b in d.pair_buttons() {
            d.actions.append(&b.widget);
        }
        d.form.button(&d.cancel);
        d
    }

    fn pair_buttons(&self) -> [&Btn; 9] {
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

    /// Sizes the dialog for listing nearby devices or for pairing. Pairing drops the header's
    /// title, leaving its close button, which cancels.
    fn fit(&self, pairing: bool) {
        let (width, height) = if pairing { (PAIR_WIDTH, -1) } else { SCAN_SIZE };
        let dialog = &self.form.dialog;
        if dialog.content_width() != width {
            dialog.set_content_width(width);
        }
        if dialog.content_height() != height {
            dialog.set_content_height(height);
        }
        if self.form.header.shows_title() == pairing {
            self.form.header.set_show_title(!pairing);
        }
        self.form.show_buttons(!pairing);
    }

    /// Shows the pairing step's answers left to right, the last being the main one that Enter
    /// activates.
    fn arrange(&self, shown: &[&Btn], default: bool) {
        self.buttons(shown);
        self.actions.set_visible(!shown.is_empty());
        let mut previous: Option<&gtk::Button> = None;
        for b in shown.iter().map(|b| &b.widget) {
            if b.prev_sibling().as_ref() != previous.map(|p| p.upcast_ref::<gtk::Widget>()) {
                self.actions.reorder_child_after(b, previous);
            }
            previous = Some(b);
        }
        self.set_default(previous.filter(|_| default));
    }

    fn set_default(&self, b: Option<&gtk::Button>) {
        let dialog = &self.form.dialog;
        let b = b.map(|b| b.upcast_ref::<gtk::Widget>());
        if dialog.default_widget().as_ref() != b {
            dialog.set_default_widget(b);
        }
    }

    fn buttons(&self, shown: &[&Btn]) {
        for b in [
            &self.cancel,
            &self.differ,
            &self.matches,
            &self.reject,
            &self.accept,
            &self.cancel_pairing,
            &self.another,
            &self.dismiss,
            &self.again,
            &self.close,
        ] {
            if !shown.iter().any(|s| s.widget == b.widget) {
                b.hide();
            }
        }
    }

    fn update_scan(&mut self, app: &App<Live>, add: &AddDevice) {
        self.stack.set_visible_child_name("scan");
        self.fit(false);
        self.countdown.stop();
        self.buttons(&[&self.cancel]);
        self.set_default(None);
        self.cancel.set(Some(Action::Cancel));
        let adapter = add.adapter.clone().unwrap_or_default();

        let scan = add.scan.as_ref().filter(|s| s.adapter == adapter);
        let st = scan.and_then(|s| app.states.get(&s.slot));
        let scanning = add.starting || st.is_some_and(|st| st.scanning.is_some());
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
        let retry = error.is_some() && (scan.is_some_and(|s| s.error.is_some()) || add.scan_failed);
        set_label(&self.status, error.as_deref().unwrap_or(""));
        set_look(&self.status, Look::Error);
        self.scan.set_label(if retry { "Retry" } else { "Refresh" });
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
            let pair = Btn::suggested(&self.ctx, "Pair");
            pair.widget.set_valign(gtk::Align::Center);
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
            set_title(&r.row, &words::candidate_name(c));
            set_subtitle(&r.row, words::transport_name(t));
            r.signal.set_icon_name(signal(c.rssi));
            r.signal.set_visible(c.rssi.is_some());
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

    fn update_pairing(&mut self, app: &App<Live>, pairing: &Pairing) {
        self.stack.set_visible_child_name("pair");
        self.fit(true);
        spin(&self.searching, false);
        let name = &pairing.name;
        let heading;
        let body;
        let mut badge = Badge::None;
        let mut reason = None;
        let mut code = None;
        let mut entry = None;
        let mut prompt_error = None;
        let mut counting = false;
        match pairing.phase {
            Phase::Pairing => {
                self.cancel_pairing.set(Some(Action::CancelPairing));
                match app.pairing_prompt() {
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
                        prompt_error = pairing.prompt_error.clone();
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
                        prompt_error = pairing.prompt_error.clone();
                        self.arrange(&[&self.reject, &self.accept], true);
                        self.reject.set(Some(Action::Reject));
                        self.accept.set(when(app.form_valid(), Action::Accept));
                    }
                }
            }
            Phase::Connecting => {
                badge = Badge::Busy;
                heading = "Connecting".to_owned();
                body = format!("Paired. Connecting to {name}…");
                self.arrange(&[], false);
            }
            Phase::Connected | Phase::Saved => {
                // Only a connection finishes on its own; a saved bond that can't connect keeps
                // its reason up until the user answers.
                counting = pairing.phase == Phase::Connected;
                if counting {
                    badge = Badge::Success;
                    heading = "Connected".to_owned();
                    body = format!("{name} is paired and connected.");
                } else {
                    badge = Badge::Warning;
                    heading = "Paired".to_owned();
                    body = format!("{name} is paired.");
                    reason = pairing.message.clone();
                }
                self.arrange(&[&self.another, &self.dismiss], true);
                self.another.set(Some(Action::AddAnother));
                self.dismiss.set(Some(Action::Done));
            }
            Phase::Failed => {
                badge = Badge::Error;
                heading = "Pairing Failed".to_owned();
                body = format!("Cordial couldn't pair with {name}.");
                reason = pairing.message.clone();
                self.arrange(&[&self.close, &self.again], true);
                self.again.set_label("Retry");
                self.again.set(Some(Action::ScanAgain));
                self.close.set(Some(Action::Cancel));
            }
            Phase::Cancelled => {
                heading = "Pairing Cancelled".to_owned();
                body = format!("Pairing with {name} was cancelled.");
                self.arrange(&[&self.close, &self.again], true);
                self.again.set_label("Refresh");
                self.again.set(Some(Action::ScanAgain));
                self.close.set(Some(Action::Cancel));
            }
        }
        if counting {
            self.countdown.run(&self.ctx);
        } else {
            self.countdown.stop();
        }
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
            .unwrap_or(words::DisplayKind::Other);
        let icon = device_icon(kind);
        if self.device_icon.icon_name().as_deref() != Some(icon) {
            self.device_icon.set_icon_name(Some(icon));
        }
        spin(&self.busy, badge == Badge::Busy);
        let (badge_icon, badge_look) = match badge {
            Badge::Success => (Some("object-select-symbolic"), Look::Success),
            Badge::Warning => (Some("dialog-warning-symbolic"), Look::Warning),
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
        note(&self.reason, reason.as_deref(), Look::Dim);
        note(&self.code, code.as_deref(), Look::Plain);
        self.entry.set_visible(entry.is_some());
        if let Some(kind) = entry {
            let limit = app.form_limit() as i32;
            if self.entry.max_length() != limit {
                self.entry.set_max_length(limit);
            }
            let placeholder = (kind != CodeKind::Pin).then_some("000000");
            if self.entry.placeholder_text().as_deref() != placeholder {
                self.entry.set_placeholder_text(placeholder);
            }
            if self.entry.text() != app.form() {
                self.entry.set_text(app.form());
                self.entry.set_position(-1);
            }
        }
        note(&self.prompt_error, prompt_error.as_deref(), Look::Error);
    }
}

impl Drop for AddDialog {
    /// Stops the countdown, since a replaced dialog animates out before it goes and must not
    /// finish the dialog that follows it.
    fn drop(&mut self) {
        self.countdown.stop();
    }
}

impl Modal for AddDialog {
    fn dialog(&self) -> adw::Dialog {
        self.form.dialog.clone()
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) {
        let Some(add) = app.add().cloned() else {
            return;
        };
        match &add.pairing {
            Some(pairing) => self.update_pairing(app, pairing),
            None => self.update_scan(app, &add),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn part(w: &gtk::Widget, name: &str) -> gtk::Widget {
        let mut child = w.first_child();
        while let Some(c) = child {
            if c.css_name() == name {
                return c;
            }
            child = c.next_sibling();
        }
        panic!("{} has no {name} node", w.css_name());
    }

    #[test]
    #[ignore = "requires a display"]
    fn countdown_draws_along_the_bottom_edge() {
        adw::init().expect("GTK needs a display");
        crate::desktop::widgets::add_style();
        let dialog = adw::Dialog::builder()
            .content_width(400)
            .child(&gtk::Label::new(Some("Connected")))
            .build();
        let countdown = Countdown::new(&dialog);
        countdown.bar.set_fraction(0.5);
        countdown.bar.set_visible(true);
        let window = adw::Window::builder()
            .default_width(600)
            .default_height(400)
            .build();
        window.present();
        dialog.present(Some(&window));
        let bar = countdown.bar.upcast_ref::<gtk::Widget>();
        let trough = part(bar, "trough");
        let progress = part(&trough, "progress");
        // Layout runs on the frame clock's timers, so a timer wakes the loop until the bar has
        // its size.
        let tick = || glib::ControlFlow::Continue;
        let wake = glib::timeout_add_local(Duration::from_millis(10), tick);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !(bar.is_mapped() && progress.height() > 0) {
            glib::MainContext::default().iteration(true);
        }
        wake.remove();

        assert_eq!(
            (bar.height(), trough.height(), progress.height()),
            (3, 3, 3)
        );
        let frame = bar.parent().unwrap();
        let edge = bar.compute_bounds(&frame).unwrap();
        assert_eq!(
            (edge.x(), edge.width()),
            (0.0, frame.width() as f32),
            "the bar spans the dialog"
        );
        assert_eq!(
            edge.y() + edge.height(),
            frame.height() as f32,
            "the bar sits on the bottom edge"
        );
        assert!(
            (progress.width() - trough.width() / 2).abs() <= 1,
            "the fill shows the time left"
        );
    }
}
