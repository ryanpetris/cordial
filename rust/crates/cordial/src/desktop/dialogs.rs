//! The model's dialogs over the window. A dialog stays open while its request runs and shows the
//! request's failure; closing it cancels it as Escape does.
use super::{
    add::{AddDialog, PairDialog},
    profiles::Picker,
    shell::{Ctx, Ev},
    widgets::{Btn, Look, note, para, when},
};
use crate::{
    commands, profiles,
    ui::app::{
        Action, App, Dialog, Kind, Live, PickFor, Spot, profiles as app_profiles, words,
        world::AdapterView,
    },
};
use adw::prelude::*;
use gtk::glib;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// Which dialog is shown. A change of key replaces the dialog; anything else updates it.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Rename(String),
    Forget(String, u32),
    ProfileName(String, Option<u32>),
    ProfileDelete(String, u32),
    Pick(String, PickFor),
    /// Add Device while it lists nearby devices.
    Add,
    /// Add Device while it pairs one.
    Pair,
    Bootloader(String),
}

pub fn key(app: &App<Live>) -> Option<Key> {
    Some(match app.dialog()? {
        Dialog::Help => return None,
        Dialog::Rename(a) => Key::Rename(a.clone()),
        Dialog::Forget(a, id) => Key::Forget(a.clone(), *id),
        Dialog::ProfileName(a, copy) => Key::ProfileName(a.clone(), *copy),
        Dialog::ProfileDelete(a, id) => Key::ProfileDelete(a.clone(), *id),
        Dialog::Pick {
            adapter, purpose, ..
        } => Key::Pick(adapter.clone(), *purpose),
        Dialog::AddDevice if app.add().is_some_and(|a| a.pairing.is_some()) => Key::Pair,
        Dialog::AddDevice => Key::Add,
        Dialog::Bootloader(a) => Key::Bootloader(a.clone()),
    })
}

pub trait Modal {
    fn dialog(&self) -> adw::Dialog;
    fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>);
}

/// The dialog on screen.
pub struct Shown {
    key: Key,
    modal: Box<dyn Modal>,
    /// The dialog has closed, or is closing because the model no longer has it.
    closed: Rc<Cell<bool>>,
}

impl Shown {
    pub fn open(ctx: &Rc<Ctx>, app: &App<Live>, key: Key) -> Self {
        let modal: Box<dyn Modal> = match &key {
            Key::Rename(a) => Box::new(Rename::new(ctx, a)),
            Key::Forget(a, id) => Box::new(Confirm::forget(ctx, app, a, *id)),
            Key::ProfileName(a, copy) => Box::new(ProfileName::new(ctx, app, a, *copy)),
            Key::ProfileDelete(a, id) => Box::new(Confirm::delete(ctx, app, a, *id)),
            Key::Pick(a, purpose) => Box::new(Picker::new(ctx, a, *purpose)),
            Key::Add => Box::new(AddDialog::new(ctx)),
            Key::Pair => Box::new(PairDialog::new(ctx)),
            Key::Bootloader(a) => Box::new(Confirm::bootloader(ctx, a)),
        };
        let closed = Rc::new(Cell::new(false));
        modal.dialog().connect_closed(glib::clone!(
            #[weak]
            ctx,
            #[strong]
            closed,
            move |_| {
                if !closed.replace(true) {
                    ctx.act(Action::Cancel);
                }
            }
        ));
        Self { key, modal, closed }
    }

    pub fn key(&self) -> &Key {
        &self.key
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.modal.dialog().present(Some(parent));
    }

    /// Closes the dialog without cancelling anything.
    pub fn dismiss(&self) {
        if !self.closed.replace(true) {
            self.modal.dialog().force_close();
        }
    }

    pub fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>) {
        self.modal.update(ctx, app);
    }
}

/// A dialog with a header, a body, the failure of its request, and buttons at the bottom.
pub struct Form {
    pub dialog: adw::Dialog,
    pub header: adw::HeaderBar,
    pub body: gtk::Box,
    error: gtk::Label,
    /// Buttons and indicators at the start of the button row.
    pub start: gtk::Box,
    end: gtk::Box,
}

impl Form {
    pub fn new(title: &str) -> Self {
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        let error = para();
        error.add_css_class("error");
        error.set_visible(false);
        error.set_accessible_role(gtk::AccessibleRole::Alert);
        let start = gtk::Box::builder().spacing(6).hexpand(true).build();
        let end = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        // The buttons stay below the body as it scrolls.
        let buttons = gtk::Box::builder()
            .spacing(6)
            .margin_top(6)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        buttons.append(&start);
        buttons.append(&end);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(6)
            .margin_start(18)
            .margin_end(18)
            .build();
        content.append(&body);
        content.append(&error);
        let toolbar = adw::ToolbarView::builder()
            .content(
                &gtk::ScrolledWindow::builder()
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .propagate_natural_height(true)
                    .child(&content)
                    .build(),
            )
            .build();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&buttons);
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(440)
            .child(&toolbar)
            .build();
        Self {
            dialog,
            header,
            body,
            error,
            start,
            end,
        }
    }

    pub fn button(&self, b: &Btn) {
        self.end.append(&b.widget);
    }

    /// Shows a failure, or hides it.
    pub fn note(&self, text: Option<&str>) {
        note(&self.error, text, Look::Error);
    }

    /// Lets Escape and the close button close the dialog unless its request is running.
    pub fn busy(&self, busy: bool) {
        self.dialog.set_can_close(!busy);
    }

    pub fn default(&self, b: &Btn) {
        self.dialog.set_default_widget(Some(&b.widget));
    }
}

/// A one-line text field, kept in step with the model's form.
fn text_row(ctx: &Rc<Ctx>, title: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(title)
        .activates_default(true)
        .build();
    row.connect_changed(glib::clone!(
        #[weak]
        ctx,
        move |r| ctx.send(Ev::Form(r.text().into()))
    ));
    row
}

fn show_text(row: &adw::EntryRow, text: &str) {
    if row.text() != text {
        row.set_text(text);
        row.set_position(-1);
    }
}

fn field_group(row: &adw::EntryRow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.add(row);
    group
}

struct Rename {
    form: Form,
    id: String,
    entry: adw::EntryRow,
    reset: Btn,
    cancel: Btn,
    rename: Btn,
}

impl Rename {
    fn new(ctx: &Rc<Ctx>, id: &str) -> Self {
        let form = Form::new("Rename Adapter");
        let entry = text_row(ctx, "Name");
        form.body.append(&field_group(&entry));
        let reset = Btn::new(ctx, "Reset to Default");
        form.start.append(&reset.widget);
        let cancel = Btn::new(ctx, "Cancel");
        let rename = Btn::suggested(ctx, "Rename");
        form.button(&cancel);
        form.button(&rename);
        form.default(&rename);
        Self {
            form,
            id: id.to_owned(),
            entry,
            reset,
            cancel,
            rename,
        }
    }
}

impl Modal for Rename {
    fn dialog(&self) -> adw::Dialog {
        self.form.dialog.clone()
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) {
        show_text(&self.entry, app.form());
        self.form.note(app.note(&Spot::Dialog));
        let busy = app.renaming(&self.id);
        let available = app.adapter(&self.id).and_then(AdapterView::live).is_some();
        let valid = commands::adapter_name(app.form()).is_some();
        self.form.busy(busy);
        self.entry.set_sensitive(!busy);
        self.reset.set(when(!busy && available, Action::ResetName));
        self.cancel.set(when(!busy, Action::Cancel));
        self.rename
            .set(when(!busy && available && valid, Action::Submit));
    }
}

struct ProfileName {
    form: Form,
    adapter: String,
    entry: adw::EntryRow,
    cancel: Btn,
    submit: Btn,
}

impl ProfileName {
    fn new(ctx: &Rc<Ctx>, app: &App<Live>, adapter: &str, copy: Option<u32>) -> Self {
        let title = match copy {
            Some(id) => format!("Copy “{}”", app.profile_text(adapter, id)),
            None => "New Profile".into(),
        };
        let form = Form::new(&title);
        let entry = text_row(ctx, "Name");
        form.body.append(&field_group(&entry));
        let cancel = Btn::new(ctx, "Cancel");
        let submit = Btn::suggested(ctx, if copy.is_some() { "Copy" } else { "Create" });
        form.button(&cancel);
        form.button(&submit);
        form.default(&submit);
        Self {
            form,
            adapter: adapter.to_owned(),
            entry,
            cancel,
            submit,
        }
    }
}

impl Modal for ProfileName {
    fn dialog(&self) -> adw::Dialog {
        self.form.dialog.clone()
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) {
        show_text(&self.entry, app.form());
        self.form.note(app.note(&Spot::Dialog));
        let busy = app.profiles_busy(&self.adapter);
        let available = app
            .adapter(&self.adapter)
            .and_then(AdapterView::live)
            .is_some();
        let valid = profiles::valid_name(app.form().trim());
        self.form.busy(busy);
        self.entry.set_sensitive(!busy);
        self.cancel.set(when(!busy, Action::Cancel));
        self.submit
            .set(when(!busy && available && valid, Action::Submit));
    }
}

/// What a confirmation checks before it can be answered.
enum Asks {
    Forget(String, u32),
    Delete(String, u32),
    Bootloader,
}

const CANCEL: &str = "cancel";
const CONFIRM: &str = "confirm";
/// The only response of a deletion that finished.
const CLOSE: &str = "close";
/// A deletion confirmation's width in sp. It is wider than other alerts so that its steps wrap
/// less.
const DELETION_WIDTH: f64 = 420.0;

/// What a confirmation's destructive response sends, with the changes it lists. A new answer
/// waits until a frame has painted its changes: input that reaches the dialog before then answers
/// what was on screen.
#[derive(Default)]
struct Answer {
    /// The changes on screen, or about to be.
    shown: RefCell<Vec<String>>,
    now: RefCell<Option<Action>>,
    next: RefCell<Option<Action>>,
    /// Frames begun since the changes last changed.
    frames: Cell<u32>,
}

impl Answer {
    /// Answers with `action` once `steps` are painted. True when they differ from the changes
    /// shown, which the caller then shows.
    fn show(
        self: &Rc<Self>,
        widget: &impl IsA<gtk::Widget>,
        steps: &[String],
        action: Action,
    ) -> bool {
        if *self.shown.borrow() == steps {
            match &mut *self.next.borrow_mut() {
                Some(next) => *next = action,
                None => *self.now.borrow_mut() = Some(action),
            }
            return false;
        }
        *self.shown.borrow_mut() = steps.to_vec();
        self.frames.set(0);
        if self.next.replace(Some(action)).is_some() {
            return true;
        }
        let answer = self.clone();
        // A tick comes before its frame's paint, so the frame that paints the changes has begun
        // once a second tick runs.
        widget.add_tick_callback(move |_, _| {
            let frames = answer.frames.get() + 1;
            answer.frames.set(frames);
            if frames < 2 {
                return glib::ControlFlow::Continue;
            }
            *answer.now.borrow_mut() = answer.next.take();
            glib::ControlFlow::Break
        });
        true
    }
}

/// A confirmation: Cancel, the default, or the destructive response. It stays open while its
/// request runs and shows the request's failure; the model closes it.
struct Confirm {
    dialog: adw::AlertDialog,
    /// The changes a deletion makes, in order, below the body.
    steps: gtk::Box,
    /// Each listed change's row and marker.
    rows: RefCell<Vec<(gtk::Box, gtk::Stack)>>,
    error: gtk::Label,
    asks: Asks,
    answer: Rc<Answer>,
}

impl Confirm {
    fn new(ctx: &Rc<Ctx>, heading: &str, body: &str, label: &str, asks: Asks) -> Self {
        let ctx = Rc::downgrade(ctx);
        let act = move |action| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.act(action);
            }
        };
        Self::with(act, heading, body, label, asks)
    }

    fn with(
        act: impl Fn(Action) + Clone + 'static,
        heading: &str,
        body: &str,
        label: &str,
        asks: Asks,
    ) -> Self {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_response(CANCEL, "_Cancel");
        dialog.add_response(CONFIRM, label);
        dialog.set_response_appearance(CONFIRM, adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some(CANCEL));
        dialog.set_close_response(CANCEL);
        let error = para();
        error.add_css_class("error");
        error.set_visible(false);
        error.set_accessible_role(gtk::AccessibleRole::Alert);
        let steps = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .accessible_role(gtk::AccessibleRole::List)
            .visible(false)
            .build();
        let extra = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        extra.append(&steps);
        extra.append(&error);
        dialog.set_extra_child(Some(&extra));
        // Responses go to the model, which closes the dialog. Escape only attempts to close it,
        // and cancels unless the attempt came from a response, which follows it at once.
        dialog.set_can_close(false);
        let answered = Rc::new(Cell::new(false));
        let answer = Rc::new(Answer::default());
        dialog.connect_response(
            None,
            glib::clone!(
                #[strong]
                act,
                #[strong]
                answered,
                #[strong]
                answer,
                move |_, response| {
                    answered.set(true);
                    act(if response == CONFIRM {
                        answer.now.borrow().clone().unwrap_or(Action::Confirm)
                    } else {
                        Action::Cancel
                    });
                }
            ),
        );
        dialog.connect_close_attempt(glib::clone!(
            #[strong]
            answered,
            move |_| {
                answered.set(false);
                glib::idle_add_local_once(glib::clone!(
                    #[strong]
                    act,
                    #[strong]
                    answered,
                    move || {
                        if !answered.get() {
                            act(Action::Cancel);
                        }
                    }
                ));
            }
        ));
        Self {
            dialog,
            steps,
            rows: RefCell::default(),
            error,
            asks,
            answer,
        }
    }

    /// Lists a deletion's changes under its introduction, each wrapping beside its number, and
    /// answers with `action` once they are painted.
    fn show_steps(&self, steps: &[String], action: Action) {
        if !self.answer.show(&self.dialog, steps, action) {
            return;
        }
        self.dialog.set_body(if steps.is_empty() {
            ""
        } else {
            app_profiles::DELETION_INTRO
        });
        while let Some(row) = self.steps.first_child() {
            self.steps.remove(&row);
        }
        // Wide enough for the last number, so the numbers right-align in one column.
        let digits = steps.len().to_string().len() as i32 + 1;
        let mut rows = self.rows.borrow_mut();
        rows.clear();
        for (i, step) in steps.iter().enumerate() {
            let row = gtk::Box::builder()
                .spacing(8)
                .accessible_role(gtk::AccessibleRole::ListItem)
                .build();
            let marker = marker(i + 1, digits);
            let text = para();
            text.set_hexpand(true);
            text.set_label(step);
            row.append(&marker);
            row.append(&text);
            row.update_relation(&[gtk::accessible::Relation::LabelledBy(&[text.upcast_ref()])]);
            self.steps.append(&row);
            rows.push((row, marker));
        }
        self.steps.set_visible(!steps.is_empty());
    }

    /// Shows how each listed change stands. Screen readers hear it once a deletion has run.
    fn show_marks(&self, marks: &[app_profiles::Mark]) {
        use app_profiles::Mark;
        let ran = marks.iter().any(|m| *m != Mark::Pending);
        for ((row, marker), mark) in self.rows.borrow().iter().zip(marks) {
            let (page, state) = match mark {
                Mark::Pending => ("pending", "Not started"),
                Mark::Active => ("active", "In progress"),
                Mark::Done => ("done", "Done"),
                Mark::Failed => ("failed", "Failed"),
                Mark::Unconfirmed => ("unconfirmed", "Unconfirmed"),
            };
            marker.set_visible_child_name(page);
            if let Some(spinner) = marker
                .child_by_name("active")
                .and_downcast::<gtk::Spinner>()
            {
                spinner.set_spinning(*mark == Mark::Active);
            }
            if ran {
                row.update_property(&[gtk::accessible::Property::Description(state)]);
            } else {
                row.reset_property(gtk::AccessibleProperty::Description);
            }
        }
    }

    /// Shows a deletion as the model has it. A finished one offers only Close.
    fn show_deletion(&self, view: &app_profiles::DeletionView, action: Action) {
        use app_profiles::Stage;
        let name = &view.name;
        self.dialog.set_heading(Some(&match view.stage {
            Stage::Ask => format!("Delete “{name}”?"),
            Stage::Running => format!("Deleting “{name}”"),
            Stage::Deleted => format!("Deleted “{name}”"),
        }));
        // Only an asking dialog can answer, so a change found while one runs waits to be painted.
        let action = match view.stage {
            Stage::Ask => action,
            _ => Action::Confirm,
        };
        self.show_steps(&view.steps, action);
        self.show_marks(&view.marks);
        if !view.steps.is_empty() {
            self.dialog.set_body(
                if view
                    .marks
                    .iter()
                    .any(|mark| *mark != app_profiles::Mark::Pending)
                {
                    "Changes, in order:"
                } else {
                    app_profiles::DELETION_INTRO
                },
            );
        }
        if view.stage == Stage::Deleted && !self.dialog.has_response(CLOSE) {
            self.dialog.remove_response(CANCEL);
            self.dialog.remove_response(CONFIRM);
            self.dialog.add_response(CLOSE, "_Close");
            self.dialog.set_default_response(Some(CLOSE));
            self.dialog.set_close_response(CLOSE);
        }
    }

    fn forget(ctx: &Rc<Ctx>, app: &App<Live>, adapter: &str, id: u32) -> Self {
        let name = app
            .device(adapter, id)
            .map_or_else(|| "this device".to_owned(), |d| d.name.clone());
        Self::new(
            ctx,
            &format!("Forget “{name}”?"),
            words::FORGET_TEXT,
            "_Forget",
            Asks::Forget(adapter.to_owned(), id),
        )
    }

    fn delete(ctx: &Rc<Ctx>, app: &App<Live>, adapter: &str, id: u32) -> Self {
        Self::new(
            ctx,
            &format!("Delete “{}”?", app.profile_text(adapter, id)),
            "",
            "_Delete",
            Asks::Delete(adapter.to_owned(), id),
        )
        .widen()
    }

    /// Makes the dialog `DELETION_WIDTH` wide. An alert grows past its usual width only to fit
    /// its content's minimum, so the content asks for that width less the alert's 24px side
    /// padding. The window's minimum width leaves room for it.
    fn widen(self) -> Self {
        let width = adw::LengthUnit::Sp.to_px(DELETION_WIDTH, Some(&self.dialog.settings()));
        self.dialog
            .extra_child()
            .expect("a confirmation has an extra child")
            .set_width_request(width.ceil() as i32 - 48);
        self
    }

    fn bootloader(ctx: &Rc<Ctx>, _: &str) -> Self {
        Self::new(
            ctx,
            "Restart the adapter into USB programming mode?",
            "Keyboard and mouse input stops until the adapter restarts. Saved bonds are kept and no firmware is installed.",
            "_Enter Bootloader",
            Asks::Bootloader,
        )
    }
}

/// A listed change's marker: its number until a deletion starts it, then a spinner while it is
/// made and its result once it is. Every page takes the same space, so the row never moves.
fn marker(number: usize, digits: i32) -> gtk::Stack {
    let marker = gtk::Stack::builder()
        .valign(gtk::Align::Start)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    let label = gtk::Label::builder()
        .label(format!("{number}."))
        .width_chars(digits)
        .xalign(1.0)
        .css_classes(["dim-label", "numeric"])
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    marker.add_named(&label, Some("pending"));
    let spinner = gtk::Spinner::builder()
        .halign(gtk::Align::End)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    marker.add_named(&spinner, Some("active"));
    for (page, icon, look) in [
        ("done", "object-select-symbolic", "success"),
        ("failed", "dialog-error-symbolic", "error"),
        ("unconfirmed", "dialog-warning-symbolic", "warning"),
    ] {
        let image = gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(16)
            .halign(gtk::Align::End)
            .css_classes([look])
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        marker.add_named(&image, Some(page));
    }
    marker
}

impl Modal for Confirm {
    fn dialog(&self) -> adw::Dialog {
        self.dialog.clone().upcast()
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) {
        note(&self.error, app.note(&Spot::Dialog), Look::Error);
        let (busy, ready) = match &self.asks {
            Asks::Forget(a, id) => (
                app.running(a, |k| matches!(k, Kind::Forget(d) if d == id)),
                app.device(a, *id).is_some_and(|d| app.adapter_ready(d)),
            ),
            Asks::Delete(a, id) => {
                let view = app.deletion_view(a, *id);
                let action = app
                    .deletion(a, *id)
                    .map_or(Action::Confirm, Action::ProfileDeleteConfirm);
                self.show_deletion(&view, action);
                if view.waiting {
                    note(&self.error, Some(app_profiles::DELETION_WAITING), Look::Dim);
                } else if view.uncertain {
                    note(&self.error, app.note(&Spot::Dialog), Look::Warning);
                }
                if view.stage == app_profiles::Stage::Deleted {
                    return;
                }
                (app.profiles_busy(a), app.deletion_ready(a, *id))
            }
            Asks::Bootloader => (false, true),
        };
        self.dialog.set_response_enabled(CANCEL, !busy);
        self.dialog.set_response_enabled(CONFIRM, !busy && ready);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{Deletion, Detach};
    use std::time::{Duration, Instant};

    fn deletion(devices: &[u32]) -> Action {
        Action::ProfileDeleteConfirm(Deletion {
            profile: 1,
            devices: devices
                .iter()
                .map(|&device| Detach {
                    device,
                    name: format!("Device {device}"),
                    layers: vec![1],
                })
                .collect(),
            interfaces: Vec::new(),
        })
    }

    /// The changes the dialog lists, as its labels show them, each after its number.
    fn listed(c: &Confirm) -> Vec<String> {
        let mut steps = Vec::new();
        let mut row = c.steps.first_child();
        while let Some(r) = row {
            let number = r
                .first_child()
                .and_downcast::<gtk::Stack>()
                .and_then(|m| m.child_by_name("pending"))
                .and_downcast::<gtk::Label>()
                .unwrap();
            assert_eq!(number.label(), format!("{}.", steps.len() + 1));
            let text = r.last_child().and_downcast::<gtk::Label>().unwrap();
            assert!(gtk::test_accessible_has_relation(
                &r,
                gtk::AccessibleRelation::LabelledBy
            ));
            steps.push(text.label().into());
            row = r.next_sibling();
        }
        steps
    }

    #[test]
    #[ignore = "requires a display"]
    fn a_replaced_dialog_cancels_nothing() {
        adw::init().expect("GTK needs a display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let (ctx, requests) = Ctx::test();
        let app = App::new(Live::new(std::sync::Arc::new(|_| {}), None));
        let window = adw::Window::new();
        window.present();
        let pump = |done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done() {
                assert!(Instant::now() < deadline, "the dialog never closed");
                glib::MainContext::default().iteration(false);
            }
        };

        let add = Shown::open(&ctx, &app, Key::Add);
        add.present(&window);
        let closed = Rc::new(Cell::new(false));
        add.modal.dialog().connect_closed(glib::clone!(
            #[strong]
            closed,
            move |_| closed.set(true)
        ));
        add.dismiss();
        let pair = Shown::open(&ctx, &app, Key::Pair);
        pair.present(&window);
        pump(&|| closed.get());
        assert!(
            requests.try_recv().is_err(),
            "the model's own replacement sends nothing"
        );
        assert!(!add.modal.dialog().is_mapped());
        assert!(pair.modal.dialog().is_mapped());

        pair.modal.dialog().close();
        pump(&|| !requests.is_empty());
        assert!(
            matches!(requests.try_recv(), Ok(Ev::Act(Action::Cancel))),
            "closing the dialog cancels"
        );
    }

    #[test]
    #[ignore = "requires a display"]
    fn a_changed_deletion_is_answered_once_its_changes_are_painted() {
        adw::init().expect("GTK needs a display");
        let sent = Rc::new(RefCell::new(Vec::new()));
        let c = Confirm::with(
            glib::clone!(
                #[strong]
                sent,
                move |a| sent.borrow_mut().push(a)
            ),
            "Delete “P01”?",
            "",
            "_Delete",
            Asks::Delete("adapter".into(), 1),
        );
        let window = adw::Window::new();
        window.present();
        c.dialog.present(Some(&window));
        let confirm = || {
            c.dialog.emit_by_name::<()>("response", &[&CONFIRM]);
            sent.borrow_mut()
                .pop()
                .expect("the response sends an answer")
        };
        let pump = |done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done() {
                assert!(Instant::now() < deadline, "no frame was painted");
                glib::MainContext::default().iteration(false);
            }
        };
        let remove = |n| {
            format!(
                "Remove it from Device {n} With A Name Long Enough To Wrap Across Lines, keeping \
                 the other profiles in order"
            )
        };

        // Unchanged changes are already shown.
        c.show_steps(&[], deletion(&[]));
        assert_eq!(confirm(), deletion(&[]));

        let first = [remove(2), "Delete “P01”".to_owned()];
        c.show_steps(&first, deletion(&[2]));
        assert_eq!(c.dialog.body(), app_profiles::DELETION_INTRO);
        assert_eq!(listed(&c), first);
        pump(&|| *c.answer.now.borrow() == Some(deletion(&[2])));
        assert_eq!(confirm(), deletion(&[2]));

        // A step added under the same introduction: the answer when the first frame showing it
        // finished painting.
        let painted = Rc::new(RefCell::new(None));
        let second = [remove(2), remove(3), "Delete “P01”".to_owned()];
        c.dialog.add_tick_callback(glib::clone!(
            #[strong]
            painted,
            #[strong(rename_to = steps)]
            c.steps,
            #[strong(rename_to = answer)]
            c.answer,
            move |_, clock| {
                if steps.observe_children().n_items() != 3 {
                    return glib::ControlFlow::Continue;
                }
                // This frame paints the changes after its ticks.
                let (painted, answer) = (painted.clone(), answer.clone());
                clock.connect_after_paint(move |_| {
                    if painted.borrow().is_none() {
                        *painted.borrow_mut() = Some(answer.now.borrow().clone());
                    }
                });
                glib::ControlFlow::Break
            }
        ));
        c.show_steps(&second, deletion(&[2, 3]));
        assert_eq!(c.dialog.body(), app_profiles::DELETION_INTRO);
        assert_eq!(listed(&c), second);
        assert_eq!(
            confirm(),
            deletion(&[2]),
            "a click before the new changes are painted answers the old ones"
        );
        c.show_steps(&second, deletion(&[2, 3]));
        assert_eq!(
            confirm(),
            deletion(&[2]),
            "a repeated update changes nothing"
        );
        pump(&|| *c.answer.now.borrow() == Some(deletion(&[2, 3])));
        assert_eq!(
            *painted.borrow(),
            Some(Some(deletion(&[2]))),
            "the frame that paints the new changes is answered with the old ones"
        );
        assert_eq!(confirm(), deletion(&[2, 3]));
    }

    /// Runs the main loop until `widget` has begun `n` more frames.
    fn frames(widget: &impl IsA<gtk::Widget>, n: u32) {
        let begun = Rc::new(Cell::new(0));
        widget.add_tick_callback(glib::clone!(
            #[strong]
            begun,
            move |_, _| {
                begun.set(begun.get() + 1);
                if begun.get() < n {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            }
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while begun.get() < n {
            assert!(Instant::now() < deadline, "no frame was painted");
            glib::MainContext::default().iteration(false);
        }
    }

    #[test]
    #[ignore = "requires a display"]
    fn a_deletion_marks_its_changes_in_place() {
        use app_profiles::{DeletionView, Mark, Stage};
        adw::init().expect("GTK needs a display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let steps: Vec<String> = [
            "Remove it from Logitech MX Keys S, keeping the other profiles in order",
            "Remove it from Keychron_K3_Pro_Wireless_Keyboard_Office",
            "Remove it from MX Master 3S, keeping the other profiles in order",
            "Remove it from Lofree Flow Lite 84 Low Profile Mechanical Keyboard",
            "Turn off VIA and clear its profile",
            "Clear Vial's profile",
            "Delete “Programming Layer With Navigation Cluster”",
        ]
        .map(String::from)
        .into();
        let view = |stage, marks: Vec<Mark>| DeletionView {
            stage,
            name: "Programming Layer With Navigation Cluster".into(),
            steps: steps.clone(),
            marks,
            waiting: false,
            uncertain: false,
        };
        for width in [980, 360] {
            let window = adw::Window::builder()
                .default_width(width)
                .default_height(900)
                .build();
            window.present();
            let sent = Rc::new(RefCell::new(Vec::new()));
            let c = Confirm::with(
                glib::clone!(
                    #[strong]
                    sent,
                    move |a| sent.borrow_mut().push(a)
                ),
                "",
                "",
                "_Delete",
                Asks::Delete("a".into(), 1),
            )
            .widen();
            c.show_deletion(&view(Stage::Ask, vec![Mark::Pending; 7]), deletion(&[2]));
            c.dialog.present(Some(&window));
            frames(&c.dialog, 3);
            let rows = c.rows.borrow().clone();
            // Where each change's text and marker sit, relative to the list.
            let places = || {
                rows.iter()
                    .map(|(row, marker)| {
                        let text = row.last_child().unwrap();
                        (
                            text.compute_bounds(&c.steps).unwrap(),
                            marker.compute_bounds(&c.steps).unwrap(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let asked = places();
            assert!(
                rows.iter().all(|(r, _)| !gtk::test_accessible_has_property(
                    r,
                    gtk::AccessibleProperty::Description
                )),
                "an unanswered confirmation describes no progress"
            );
            assert_eq!(
                c.dialog.heading().unwrap(),
                format!("Delete “{}”?", view(Stage::Ask, Vec::new()).name)
            );

            let running = view(
                Stage::Running,
                [Mark::Done; 4]
                    .into_iter()
                    .chain([Mark::Active, Mark::Active, Mark::Pending])
                    .collect(),
            );
            // A change found while the deletion runs isn't answered.
            c.show_deletion(&running, deletion(&[3]));
            frames(&c.dialog, 3);
            assert_eq!(
                places(),
                asked,
                "marks keep each change in place at {width}px"
            );
            assert!(c.dialog.heading().unwrap().starts_with("Deleting “"));
            assert_eq!(c.dialog.body(), "Changes, in order:");
            let pages: Vec<_> = rows
                .iter()
                .map(|(_, m)| m.visible_child_name().unwrap())
                .collect();
            assert_eq!(
                pages,
                [
                    "done", "done", "done", "done", "active", "active", "pending"
                ]
            );
            let spinning: Vec<_> = rows
                .iter()
                .map(|(_, m)| {
                    m.child_by_name("active")
                        .and_downcast::<gtk::Spinner>()
                        .unwrap()
                        .is_spinning()
                })
                .collect();
            assert_eq!(spinning, [false, false, false, false, true, true, false]);
            assert!(rows.iter().all(|(r, _)| gtk::test_accessible_has_property(
                r,
                gtk::AccessibleProperty::Description
            )));
            c.dialog.emit_by_name::<()>("response", &[&CONFIRM]);
            assert_eq!(sent.borrow_mut().pop(), Some(Action::Confirm));

            for mark in [Mark::Failed, Mark::Unconfirmed] {
                let mut marks = vec![Mark::Done; 4];
                marks.extend([mark, mark, Mark::Pending]);
                c.show_deletion(&view(Stage::Ask, marks), deletion(&[2]));
                frames(&c.dialog, 3);
                assert_eq!(places(), asked);
                assert_eq!(c.dialog.body(), "Changes, in order:");
            }

            c.show_deletion(&view(Stage::Deleted, vec![Mark::Done; 7]), deletion(&[2]));
            frames(&c.dialog, 3);
            assert_eq!(places(), asked);
            assert!(c.dialog.heading().unwrap().starts_with("Deleted “"));
            assert_eq!(c.dialog.body(), "Changes, in order:");
            assert!(
                c.dialog.has_response(CLOSE)
                    && !c.dialog.has_response(CANCEL)
                    && !c.dialog.has_response(CONFIRM),
                "a finished deletion offers only Close"
            );
            c.dialog.emit_by_name::<()>("response", &[&CLOSE]);
            assert_eq!(sent.borrow_mut().pop(), Some(Action::Cancel));
            c.dialog.force_close();
            window.close();
        }
    }

    #[test]
    #[ignore = "requires a display"]
    fn a_change_found_while_a_deletion_ran_is_answered_once_painted() {
        use app_profiles::{DeletionView, Mark, Stage};
        adw::init().expect("GTK needs a display");
        let sent = Rc::new(RefCell::new(Vec::new()));
        let c = Confirm::with(
            glib::clone!(
                #[strong]
                sent,
                move |a| sent.borrow_mut().push(a)
            ),
            "",
            "",
            "_Delete",
            Asks::Delete("a".into(), 1),
        );
        let window = adw::Window::new();
        window.present();
        c.dialog.present(Some(&window));
        let confirm = || {
            c.dialog.emit_by_name::<()>("response", &[&CONFIRM]);
            sent.borrow_mut().pop().unwrap()
        };
        let view = |stage, steps: &[&str], marks: &[Mark]| DeletionView {
            stage,
            name: "P01".into(),
            steps: steps.iter().map(|s| s.to_string()).collect(),
            marks: marks.to_vec(),
            waiting: false,
            uncertain: false,
        };
        let shown = ["Remove it from Device 2", "Delete “P01”"];
        c.show_deletion(
            &view(Stage::Ask, &shown, &[Mark::Pending; 2]),
            deletion(&[2]),
        );
        frames(&c.dialog, 3);
        assert_eq!(confirm(), deletion(&[2]));
        // Device 3 starts using the profile while the deletion runs, and it fails.
        c.show_deletion(
            &view(Stage::Running, &shown, &[Mark::Active, Mark::Pending]),
            deletion(&[2, 3]),
        );
        let now = [
            "Remove it from Device 2",
            "Remove it from Device 3",
            "Delete “P01”",
        ];
        c.show_deletion(
            &view(
                Stage::Ask,
                &now,
                &[Mark::Done, Mark::Pending, Mark::Pending],
            ),
            deletion(&[3]),
        );
        assert_eq!(
            confirm(),
            Action::Confirm,
            "Device 3 isn't answered before it is painted"
        );
        frames(&c.dialog, 3);
        assert_eq!(confirm(), deletion(&[3]));
    }

    #[test]
    #[ignore = "requires a display"]
    fn a_deletion_is_wider_than_other_confirmations() {
        adw::init().expect("GTK needs a display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let window = adw::Window::builder()
            .default_width(980)
            .default_height(680)
            .build();
        window.present();
        let shown = |c: &Confirm| {
            c.dialog.present(Some(&window));
            let sheet = c.dialog.child().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while sheet.width() == 0 {
                assert!(Instant::now() < deadline, "the dialog was never shown");
                glib::MainContext::default().iteration(false);
            }
            let width = sheet.width();
            c.dialog.force_close();
            width
        };
        let steps = [
            "Remove it from Device 2 With A Name Long Enough To Wrap Across Lines".to_owned(),
            "Delete “P01”".to_owned(),
        ];
        let other = Confirm::with(|_| {}, "Delete “P01”?", "", "_Delete", Asks::Bootloader);
        other.show_steps(&steps, deletion(&[2]));
        let delete = Confirm::with(
            |_| {},
            "Delete “P01”?",
            "",
            "_Delete",
            Asks::Delete("a".into(), 1),
        )
        .widen();
        delete.show_steps(&steps, deletion(&[2]));

        let wide = adw::LengthUnit::Sp.to_px(DELETION_WIDTH, Some(&window.settings()));
        assert_eq!(shown(&delete), wide.ceil() as i32);
        assert!(shown(&other) < shown(&delete));
    }
}
