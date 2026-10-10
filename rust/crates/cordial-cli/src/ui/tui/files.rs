//! Files, on development firmware: a dialog listing one directory of the adapter's filesystem at
//! a time, with a download of the selected file. A download is written to a private temporary
//! file, which becomes the chosen local file only once the whole file has arrived; an existing
//! local file is replaced only after the Replace confirmation.
use super::{
    Action, Dialog, Job, Kind, Model,
    fleet::Fleet,
    layout::{self, Layout, Tone, accent, dim, err, inherit, ok, pad_str, span, styled, warn},
    render::{DialogView, busy, button_if},
};
use crate::{
    controller::{Command, Outcome},
    error::Error,
    storage,
    ui::{
        field::Field,
        text::{self, display},
    },
};
use cordial_protocol::FileEntry;
use ratatui::text::Line;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum Listing {
    Loading,
    Complete,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Transfer {
    Running,
    Saved(u64),
    Failed(String),
}

/// What one download reads and where it saves it.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub path: String,
    pub local: PathBuf,
    pub size: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Download {
    pub path: String,
    pub local: PathBuf,
    pub size: Option<u64>,
    pub state: Transfer,
}

#[derive(Clone, Debug, Default)]
pub struct Files {
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub state: Option<Listing>,
    /// The selected row's name.
    pub selected: Option<String>,
    pub dest: Field,
    pub editing: bool,
    pub dest_err: String,
    pub download: Option<Download>,
}

impl Files {
    pub fn busy(&self) -> bool {
        self.download
            .as_ref()
            .is_some_and(|d| d.state == Transfer::Running)
    }
    /// Rows in display order: directories, then files, each by name.
    pub fn rows(&self) -> Vec<&FileEntry> {
        let mut rows: Vec<&FileEntry> = self.entries.iter().collect();
        rows.sort_by(|a, b| (!a.directory, &a.name).cmp(&(!b.directory, &b.name)));
        rows
    }
    pub fn entry(&self, name: &str) -> Option<&FileEntry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

/// The destination for a newly selected file: its name in the directory the destination already
/// names, so a chosen directory is kept.
pub fn keep_directory(dest: &str, name: &str) -> String {
    let path = std::path::Path::new(dest);
    let dir = if dest.ends_with(std::path::is_separator) {
        Some(path)
    } else {
        path.parent().filter(|p| !p.as_os_str().is_empty())
    };
    match dir {
        Some(dir) => dir.join(name).display().to_string(),
        None => name.to_owned(),
    }
}

/// An adapter path below `dir`.
pub fn child(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The directory containing `path`, or None at the root.
pub fn parent(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    let cut = path.trim_end_matches('/').rfind('/')?;
    Some(if cut == 0 {
        "/".into()
    } else {
        path[..cut].into()
    })
}

impl<F: Fleet> Model<F> {
    pub(super) fn open_files(&mut self, adapter: &str) {
        self.files = Files::default();
        self.dialog = Some(Dialog::Files(adapter.to_owned()));
        self.dialog_scroll = 0;
        self.list_files(adapter, "/".into());
    }

    fn files_adapter(&self) -> Option<String> {
        match &self.dialog {
            Some(Dialog::Files(a) | Dialog::Replace(a, _)) => Some(a.clone()),
            _ => None,
        }
    }

    fn list_files(&mut self, adapter: &str, path: String) {
        self.files.path = path.clone();
        self.files.entries.clear();
        self.files.selected = None;
        self.files.state = Some(Listing::Loading);
        self.dialog_scroll = 0;
        self.run(adapter, Kind::Files, Command::Files(path));
    }

    pub(super) fn files_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        match (&job.kind, &job.command) {
            _ if self.files_adapter().as_deref() != Some(job.adapter.as_str())
                && !matches!(job.kind, Kind::Bootloader) => {}
            (Kind::Files, Command::Files(path)) if *path == self.files.path => {
                self.files.state = Some(match result {
                    Ok(Outcome::Files { entries, .. }) => {
                        self.files.entries = entries.clone();
                        Listing::Complete
                    }
                    Ok(_) => Listing::Complete,
                    Err(e) => Listing::Failed(text::error_words(e)),
                });
            }
            (Kind::FileGet, Command::FileGet { path, .. }) => {
                let Some(d) = self.files.download.as_mut().filter(|d| d.path == *path) else {
                    return;
                };
                match result {
                    Ok(Outcome::FileSaved { bytes, .. }) => d.state = Transfer::Saved(*bytes),
                    Ok(_) => {}
                    Err(e) => {
                        d.state = Transfer::Failed(format!(
                            "{} No file was saved.",
                            super::words::read_failure(e)
                        ))
                    }
                }
            }
            (Kind::Bootloader, _) => {
                if let Err(e) = result {
                    self.toast(super::words::failure(e));
                }
            }
            _ => {}
        }
    }

    fn select_file(&mut self, name: String) {
        if self.files.selected.as_deref() == Some(&name) {
            return;
        }
        let dest = keep_directory(&self.files.dest.value(), &name);
        self.files.dest.set_value(&dest);
        self.files.dest_err.clear();
        self.files.selected = Some(name);
    }

    fn start_download(&mut self) {
        let Some(e) = self
            .files
            .selected
            .as_deref()
            .and_then(|n| self.files.entry(n))
            .filter(|e| !e.directory)
            .cloned()
        else {
            return;
        };
        if self.files.dest.value().trim().is_empty() {
            self.files.dest_err = "Enter a local file name.".into();
            return;
        }
        self.request_download(Target {
            path: child(&self.files.path, &e.name),
            local: PathBuf::from(self.files.dest.value()),
            size: Some(e.size),
        });
    }

    fn request_download(&mut self, target: Target) {
        let Some(adapter) = self.files_adapter() else {
            return;
        };
        if self.files.busy() {
            return;
        }
        match storage::check_destination(&target.local, false) {
            Err(e) if e.message == storage::EXISTS => {
                self.dialog = Some(Dialog::Replace(adapter, target));
            }
            Err(e) => self.files.dest_err = text::capitalized(&text::error_words(&e)),
            Ok(()) => self.begin_download(&adapter, target, false),
        }
    }

    fn begin_download(&mut self, adapter: &str, target: Target, overwrite: bool) {
        self.files.editing = false;
        self.files.dest_err.clear();
        self.files.download = Some(Download {
            path: target.path.clone(),
            local: target.local.clone(),
            size: target.size,
            state: Transfer::Running,
        });
        self.run(
            adapter,
            Kind::FileGet,
            Command::FileGet {
                path: target.path,
                local: target.local,
                overwrite,
            },
        );
    }

    /// Handles a Files control; false for other actions.
    pub(super) fn files_action(&mut self, action: &Action) -> bool {
        let Some(adapter) = self.files_adapter() else {
            return false;
        };
        match action {
            Action::FilesUp => {
                if let Some(p) = parent(&self.files.path) {
                    self.list_files(&adapter, p);
                }
            }
            Action::FilesRefresh => {
                let path = self.files.path.clone();
                self.list_files(&adapter, path);
            }
            Action::FilesEntry(name) => match self.files.entry(name).map(|e| e.directory) {
                Some(true) => {
                    let path = child(&self.files.path, name);
                    self.list_files(&adapter, path);
                }
                Some(false) => self.select_file(name.clone()),
                None => {}
            },
            Action::FilesDest => {
                if !self.files.busy() {
                    self.focus = None;
                    self.files.editing = true;
                }
            }
            Action::FilesDownload => self.start_download(),
            Action::FilesRetry => {
                if let Some(d) = self
                    .files
                    .download
                    .as_ref()
                    .filter(|d| matches!(d.state, Transfer::Failed(_)))
                {
                    let target = Target {
                        path: d.path.clone(),
                        local: d.local.clone(),
                        size: d.size,
                    };
                    self.request_download(target);
                }
            }
            _ => return false,
        }
        true
    }

    /// The Replace confirmation's answer: that target, replacing its file.
    pub(super) fn confirm_replace(&mut self, adapter: &str, target: Target) {
        self.dialog = Some(Dialog::Files(adapter.to_owned()));
        if !self.files.busy() {
            self.begin_download(adapter, target, true);
        }
    }

    pub(super) fn files_dialog(&mut self, adapter: &str, w: usize) -> DialogView {
        let mut b = Layout::new(w);
        let size_w = 10;
        let name_w = w.saturating_sub(2 + size_w).max(4);
        let ready = self
            .adapter(adapter)
            .and_then(|a| a.status.as_ref())
            .is_some_and(|s| s.ready);
        if !ready {
            b.para(
                "Adapter storage isn't ready; reading files may fail.",
                warn(),
            );
        }
        if parent(&self.files.path).is_some() {
            b.control(styled("  ..", accent()), Action::FilesUp);
        }
        let rows: Vec<FileEntry> = self.files.rows().into_iter().cloned().collect();
        for e in &rows {
            let chosen = self.files.selected.as_deref() == Some(&e.name);
            let base = if chosen {
                layout::selected()
            } else {
                layout::plain()
            };
            let (name, size) = if e.directory {
                (format!("{}/", display(&e.name)), String::new())
            } else {
                (display(&e.name), text::bytes(e.size))
            };
            let look = if e.directory {
                inherit(accent(), base)
            } else {
                base
            };
            let mut line = Line::from(vec![
                span(if chosen { "▌ " } else { "  " }, inherit(accent(), base)),
                span(
                    pad_str(&layout::truncate_str(&name, name_w - 1), name_w),
                    look,
                ),
                span(format!("{size:>size_w$}"), inherit(dim(), base)),
            ]);
            let fill = w.saturating_sub(layout::line_width(&line));
            line.spans.push(span(" ".repeat(fill), base));
            b.control(line, Action::FilesEntry(e.name.clone()));
        }
        match &self.files.state {
            Some(Listing::Loading) => b.line(busy("Reading…")),
            Some(Listing::Failed(e)) => {
                let note = if rows.is_empty() {
                    format!("Couldn't read this directory: {e}")
                } else {
                    format!("Incomplete: {e}")
                };
                b.para(&note, err());
            }
            Some(Listing::Complete) if rows.is_empty() => {
                b.line(styled("  Empty directory", dim()))
            }
            _ => {}
        }
        b.row();
        let entry = self
            .files
            .selected
            .as_deref()
            .and_then(|n| self.files.entry(n))
            .cloned();
        let mut buttons = Layout::new(w);
        if let Some(d) = self.files.download.clone() {
            let shown = display(&d.path);
            let local = display(&d.local.display().to_string());
            match &d.state {
                Transfer::Running => {
                    b.fact("File", &shown, layout::plain());
                    b.fact("Saving To", &local, layout::plain());
                    b.line(busy("Downloading…"));
                }
                Transfer::Saved(n) => {
                    b.fact("Saved", &shown, ok());
                    b.fact("To", &local, layout::plain());
                    b.fact("Size", &format!("{n} bytes"), layout::plain());
                }
                Transfer::Failed(error) => {
                    b.fact("Not Saved", &shown, err());
                    b.para(error, err());
                    buttons.button("Retry", Action::FilesRetry, Tone::Normal);
                }
            }
        }
        if let Some(e) = entry.filter(|e| !e.directory)
            && !self.files.busy()
        {
            b.row();
            b.fact(
                "File",
                &display(&child(&self.files.path, &e.name)),
                layout::plain(),
            );
            b.fact("Size", &format!("{} bytes", e.size), layout::plain());
            b.line(styled("Save As", dim()));
            let focused = self.files.editing;
            let line = super::keys::edit_line(&mut self.files.dest, focused, w.saturating_sub(3));
            b.control(line, Action::FilesDest);
            if !self.files.dest_err.is_empty() {
                b.para(&self.files.dest_err.clone(), err());
            }
            buttons.button("Download", Action::FilesDownload, Tone::Primary);
        }
        if parent(&self.files.path).is_some() {
            buttons.button("Up", Action::FilesUp, Tone::Normal);
        }
        button_if(
            &mut buttons,
            "Refresh",
            Action::FilesRefresh,
            Tone::Normal,
            true,
        );
        buttons.button_right("Close", Action::Cancel, Tone::Normal);
        DialogView {
            title: format!("Files · {}", display(&self.files.path)),
            body: b,
            buttons,
        }
    }

    pub(super) fn replace_dialog(&mut self, target: &Target, w: usize) -> DialogView {
        let mut body = Layout::new(w);
        body.para(
            &format!(
                "{} already exists. Replace it?",
                display(&target.local.display().to_string())
            ),
            layout::bold(),
        );
        body.row();
        body.para(
            "The existing file is replaced only after the whole download succeeds.",
            layout::plain(),
        );
        let mut buttons = Layout::new(w);
        let mut right = Layout::new(w);
        right.button("Cancel", Action::Cancel, Tone::Normal);
        right.button("Replace", Action::Confirm, Tone::Danger);
        buttons.align_right(right);
        DialogView {
            title: "Replace File".into(),
            body,
            buttons,
        }
    }

    pub(super) fn bootloader_dialog(&mut self, w: usize) -> DialogView {
        let mut body = Layout::new(w);
        body.para(
            "Restart the adapter into USB programming mode?",
            layout::bold(),
        );
        body.row();
        body.para(
            "Keyboard and mouse input stops until the adapter restarts. Saved bonds are kept and no firmware is installed.",
            layout::plain(),
        );
        let mut buttons = Layout::new(w);
        let mut right = Layout::new(w);
        right.button("Cancel", Action::Cancel, Tone::Normal);
        right.button("Enter Bootloader", Action::Confirm, Tone::Danger);
        buttons.align_right(right);
        DialogView {
            title: "Enter Bootloader".into(),
            body,
            buttons,
        }
    }
}
