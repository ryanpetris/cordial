//! Files, on development firmware: one directory of the adapter's filesystem at a time, and a
//! download of the selected file to a destination chosen in the system's file chooser. A saved
//! record is downloaded as JSON, so the file written may differ from the one chosen; an existing
//! file there is replaced only after the Replace confirmation.
use super::{
    dialogs::{Form, Modal},
    shell::{Ctx, Ev},
    widgets::{Btn, Item, List, Look, note, para, spin, spinner, when},
};
use crate::ui::{
    app::{
        Action, App, Dialog, Live,
        files::{Listing, Target, Transfer, child, parent, saved_to},
    },
    text::{self, display},
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};

pub struct FilesDialog {
    form: Form,
    adapter: String,
    warning: gtk::Label,
    entries: List,
    reading: gtk::Spinner,
    listing: gtk::Label,
    download_facts: List,
    selected: List,
    dest_err: gtk::Label,
    up: Btn,
    refresh: Btn,
    retry: Btn,
    download: gtk::Button,
    /// Where the file chooser starts.
    dest: Rc<RefCell<String>>,
    replace: Option<Replace>,
}

impl FilesDialog {
    pub fn new(ctx: &Rc<Ctx>, adapter: &str) -> Self {
        let form = Form::new("Files");
        form.dialog.set_content_width(560);
        form.dialog.set_content_height(560);
        let warning = para();
        form.body.append(&warning);
        let entries = List::new(ctx, "");
        form.body.append(&entries.group);
        let reading = spinner();
        let listing = para();
        let status = gtk::Box::builder().spacing(12).build();
        status.append(&reading);
        status.append(&listing);
        form.body.append(&status);
        let download_facts = List::new(ctx, "");
        form.body.append(&download_facts.group);
        let selected = List::new(ctx, "");
        form.body.append(&selected.group);
        let dest_err = para();
        form.body.append(&dest_err);

        let up = Btn::icon(ctx, "go-up-symbolic", "Up");
        let refresh = Btn::icon(ctx, "view-refresh-symbolic", "Refresh");
        form.start.append(&up.widget);
        form.start.append(&refresh.widget);
        let retry = Btn::new(ctx, "Retry");
        form.button(&retry);
        let download = gtk::Button::with_label("Download…");
        download.add_css_class("suggested-action");
        let dest: Rc<RefCell<String>> = Rc::default();
        download.connect_clicked(glib::clone!(
            #[weak]
            ctx,
            #[strong]
            dest,
            move |b| choose(&ctx, b, &dest.borrow())
        ));
        form.button_widget(&download);
        let close = Btn::new(ctx, "Close");
        close.set(Some(Action::Cancel));
        form.button(&close);
        Self {
            form,
            adapter: adapter.to_owned(),
            warning,
            entries,
            reading,
            listing,
            download_facts,
            selected,
            dest_err,
            up,
            refresh,
            retry,
            download,
            dest,
            replace: None,
        }
    }
}

/// Asks where to save the selected file, starting from its suggested destination.
fn choose(ctx: &Rc<Ctx>, button: &gtk::Button, dest: &str) {
    let path = Path::new(dest);
    let chooser = gtk::FileDialog::builder().modal(true).build();
    if let Some(name) = path.file_name() {
        chooser.set_initial_name(Some(&name.to_string_lossy()));
    }
    if let Some(dir) = path.parent().filter(|d| d.is_dir()) {
        chooser.set_initial_folder(Some(&gio::File::for_path(dir)));
    }
    let parent = button.root().and_downcast::<gtk::Window>();
    let ctx = Rc::downgrade(ctx);
    chooser.save(parent.as_ref(), gio::Cancellable::NONE, move |result| {
        let (Some(ctx), Ok(file)) = (ctx.upgrade(), result) else {
            return;
        };
        if let Some(path) = file.path() {
            // The chooser asks before choosing a file that exists.
            let exists = path.exists();
            ctx.send(Ev::Download(path, exists));
        }
    });
}

impl Modal for FilesDialog {
    fn dialog(&self) -> adw::Dialog {
        self.form.dialog.clone()
    }

    fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>) {
        let files = app.files();
        let title = format!("Files · {}", display(&files.path));
        if self.form.dialog.title() != title {
            self.form.dialog.set_title(&title);
        }
        let ready = app
            .adapter(&self.adapter)
            .and_then(|a| a.status.as_ref())
            .is_some_and(|s| s.ready);
        note(
            &self.warning,
            (!ready).then_some("Adapter storage isn't ready; reading files may fail."),
            Look::Warning,
        );
        let rows: Vec<Item> = files
            .rows()
            .into_iter()
            .map(|e| {
                let chosen = files.selected.as_deref() == Some(&e.name);
                let (name, size, icon) = if e.directory {
                    (
                        format!("{}/", display(&e.name)),
                        String::new(),
                        "folder-symbolic",
                    )
                } else {
                    (
                        display(&e.name),
                        text::bytes(e.size),
                        "text-x-generic-symbolic",
                    )
                };
                Item::link(name, "", Action::FilesEntry(e.name.clone()))
                    .value(size)
                    .icon(if chosen {
                        "object-select-symbolic"
                    } else {
                        icon
                    })
                    .look(if chosen { Look::Accent } else { Look::Plain })
            })
            .collect();
        let empty = rows.is_empty();
        self.entries.set(&rows);
        let (busy, listing, look) = match &files.state {
            Some(Listing::Loading) => (true, Some("Reading…".to_owned()), Look::Dim),
            Some(Listing::Failed(e)) if empty => (
                false,
                Some(format!("Couldn't read this directory: {e}")),
                Look::Error,
            ),
            Some(Listing::Failed(e)) => (false, Some(format!("Incomplete: {e}")), Look::Error),
            Some(Listing::Complete) if empty => {
                (false, Some("Empty directory".to_owned()), Look::Dim)
            }
            _ => (false, None, Look::Plain),
        };
        spin(&self.reading, busy);
        note(&self.listing, listing.as_deref(), look);

        let mut facts = Vec::new();
        let mut failed = false;
        if let Some(d) = &files.download {
            let shown = display(&d.path);
            let local = display(&d.local.display().to_string());
            match &d.state {
                Transfer::Running => {
                    facts.push(Item::fact("File", shown));
                    facts.push(Item::fact("Saving To", local));
                    facts.push(Item::text("Downloading…").look(Look::Dim));
                }
                Transfer::Saved(n, unconverted) => {
                    facts.push(Item::fact("Saved", shown).look(Look::Success));
                    facts.push(Item::fact("To", local));
                    facts.push(Item::fact("Size", format!("{n} bytes")));
                    if let Some(why) = unconverted {
                        facts.push(
                            Item::text(format!(
                                "Saved without converting to JSON: {}.",
                                why.reason()
                            ))
                            .look(Look::Warning),
                        );
                    }
                }
                Transfer::Failed(error) => {
                    facts.push(Item::fact("Not Saved", shown).look(Look::Error));
                    facts.push(Item::text(error.clone()).look(Look::Error));
                    failed = true;
                }
            }
        }
        self.download_facts.set(&facts);
        self.retry
            .offer(failed, when(!files.busy(), Action::FilesRetry));

        let entry = files
            .selected
            .as_deref()
            .and_then(|n| files.entry(n))
            .filter(|e| !e.directory && !files.busy());
        let selected: Vec<Item> = entry
            .map(|e| {
                vec![
                    Item::fact("File", display(&child(&files.path, &e.name))),
                    Item::fact("Size", format!("{} bytes", e.size)),
                ]
            })
            .unwrap_or_default();
        self.selected.set(&selected);
        self.download.set_visible(entry.is_some());
        *self.dest.borrow_mut() = files.dest.value();
        note(
            &self.dest_err,
            (!files.dest_err.is_empty()).then_some(files.dest_err.as_str()),
            Look::Error,
        );
        self.up
            .offer(parent(&files.path).is_some(), Some(Action::FilesUp));
        self.refresh.set(Some(Action::FilesRefresh));

        // The Replace confirmation opens over the list and returns to it.
        let target = match app.dialog() {
            Some(Dialog::Replace(_, target)) => Some(target.clone()),
            _ => None,
        };
        if self.replace.as_ref().map(|r| &r.target) != target.as_ref() {
            if let Some(r) = self.replace.take() {
                r.dismiss();
            }
            if let Some(target) = target {
                let r = Replace::new(ctx, target);
                r.form.dialog.present(Some(&self.form.dialog));
                self.replace = Some(r);
            }
        }
        // The list stays open while the confirmation is over it.
        self.form.busy(self.replace.is_some());
    }
}

struct Replace {
    form: Form,
    target: Target,
    closed: Rc<Cell<bool>>,
}

impl Replace {
    fn new(ctx: &Rc<Ctx>, target: Target) -> Self {
        let form = Form::new("Replace File");
        let path: PathBuf = saved_to(&target);
        let heading = para();
        heading.add_css_class("heading");
        heading.set_label(&format!(
            "{} already exists. Replace it?",
            display(&path.display().to_string())
        ));
        let body = para();
        body.set_label("The existing file is replaced only after the whole download succeeds.");
        form.body.append(&heading);
        form.body.append(&body);
        let cancel = Btn::new(ctx, "Cancel");
        cancel.set(Some(Action::Cancel));
        let replace = Btn::destructive(ctx, "Replace");
        replace.set(Some(Action::Confirm));
        form.button(&cancel);
        form.button(&replace);
        let closed = Rc::new(Cell::new(false));
        form.dialog.connect_closed(glib::clone!(
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
        Self {
            form,
            target,
            closed,
        }
    }

    fn dismiss(&self) {
        if !self.closed.replace(true) {
            self.form.dialog.force_close();
        }
    }
}

impl Drop for FilesDialog {
    fn drop(&mut self) {
        if let Some(r) = self.replace.take() {
            r.dismiss();
        }
    }
}
