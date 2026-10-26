//! The model's dialogs over the window. A dialog stays open while its request runs and shows the
//! request's failure; closing it cancels it as Escape does.
use super::{
    add::AddDialog,
    files::FilesDialog,
    profiles::Picker,
    shell::{Ctx, Ev},
    widgets::{Btn, Look, note, para, when},
};
use crate::{
    commands, profiles,
    ui::app::{Action, App, Dialog, Kind, Live, PickFor, Spot, words, world::AdapterView},
};
use adw::prelude::*;
use gtk::glib;
use std::{cell::Cell, rc::Rc};

/// Which dialog is shown. A change of key replaces the dialog; anything else updates it.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Rename(String),
    Forget(String, u32),
    ProfileName(String, Option<u32>),
    ProfileDelete(String, u32),
    Pick(String, PickFor),
    Add,
    Bootloader(String),
    /// Files, with its Replace confirmation.
    Files(String),
}

pub fn key(d: &Dialog) -> Option<Key> {
    Some(match d {
        Dialog::Help => return None,
        Dialog::Rename(a) => Key::Rename(a.clone()),
        Dialog::Forget(a, id) => Key::Forget(a.clone(), *id),
        Dialog::ProfileName(a, copy) => Key::ProfileName(a.clone(), *copy),
        Dialog::ProfileDelete(a, id) => Key::ProfileDelete(a.clone(), *id),
        Dialog::Pick {
            adapter, purpose, ..
        } => Key::Pick(adapter.clone(), *purpose),
        Dialog::AddDevice => Key::Add,
        Dialog::Bootloader(a) => Key::Bootloader(a.clone()),
        Dialog::Files(a) | Dialog::Replace(a, _) => Key::Files(a.clone()),
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
            Key::Bootloader(a) => Box::new(Confirm::bootloader(ctx, a)),
            Key::Files(a) => Box::new(FilesDialog::new(ctx, a)),
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
    toolbar: adw::ToolbarView,
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
            toolbar,
            header,
            body,
            error,
            start,
            end,
        }
    }

    /// Shows or hides the button row, for a body that places its own buttons.
    pub fn show_buttons(&self, shown: bool) {
        if self.toolbar.reveals_bottom_bars() != shown {
            self.toolbar.set_reveal_bottom_bars(shown);
        }
    }

    pub fn button(&self, b: &Btn) {
        self.end.append(&b.widget);
    }

    pub fn button_widget(&self, b: &gtk::Button) {
        self.end.append(b);
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
    Delete(String),
    Bootloader,
}

/// A confirmation: Cancel, or the destructive answer.
struct Confirm {
    form: Form,
    asks: Asks,
    cancel: Btn,
    confirm: Btn,
}

impl Confirm {
    fn new(ctx: &Rc<Ctx>, title: &str, label: &str, lines: &[(&str, bool)], asks: Asks) -> Self {
        let form = Form::new(title);
        for (text, bold) in lines {
            let l = para();
            l.set_label(text);
            if *bold {
                l.add_css_class("heading");
            }
            form.body.append(&l);
        }
        let cancel = Btn::new(ctx, "Cancel");
        let confirm = Btn::destructive(ctx, label);
        form.button(&cancel);
        form.button(&confirm);
        Self {
            form,
            asks,
            cancel,
            confirm,
        }
    }

    fn forget(ctx: &Rc<Ctx>, app: &App<Live>, adapter: &str, id: u32) -> Self {
        let name = app
            .device(adapter, id)
            .map_or_else(|| "this device".to_owned(), |d| d.name.clone());
        Self::new(
            ctx,
            &format!("Forget “{name}”?"),
            "Forget",
            &[(words::FORGET_TEXT, false)],
            Asks::Forget(adapter.to_owned(), id),
        )
    }

    fn delete(ctx: &Rc<Ctx>, app: &App<Live>, adapter: &str, id: u32) -> Self {
        Self::new(
            ctx,
            &format!("Delete “{}”?", app.profile_text(adapter, id)),
            "Delete",
            &[],
            Asks::Delete(adapter.to_owned()),
        )
    }

    fn bootloader(ctx: &Rc<Ctx>, _: &str) -> Self {
        Self::new(
            ctx,
            "Enter Bootloader",
            "Enter Bootloader",
            &[
                ("Restart the adapter into USB programming mode?", true),
                (
                    "Keyboard and mouse input stops until the adapter restarts. Saved bonds are kept and no firmware is installed.",
                    false,
                ),
            ],
            Asks::Bootloader,
        )
    }
}

impl Modal for Confirm {
    fn dialog(&self) -> adw::Dialog {
        self.form.dialog.clone()
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) {
        self.form.note(app.note(&Spot::Dialog));
        let (busy, ready) = match &self.asks {
            Asks::Forget(a, id) => (
                app.running(a, |k| matches!(k, Kind::Forget(d) if d == id)),
                app.device(a, *id).is_some_and(|d| app.adapter_ready(d)),
            ),
            Asks::Delete(a) => (
                app.profiles_busy(a),
                app.adapter(a).and_then(AdapterView::live).is_some(),
            ),
            Asks::Bootloader => (false, true),
        };
        self.form.busy(busy);
        self.cancel.set(when(!busy, Action::Cancel));
        self.confirm.set(when(!busy && ready, Action::Confirm));
    }
}
