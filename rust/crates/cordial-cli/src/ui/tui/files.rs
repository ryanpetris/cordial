//! The Files page replaces the device list and details with a directory of
//! the adapter's filesystem and a download panel. Directories are read one at
//! a time as they are opened, never recursively. A download runs on a
//! controller thread and is written to a private temporary file, which
//! becomes the chosen local file only once the whole file has arrived. An
//! existing local file is replaced only after the Replace confirmation.
//! Switching or losing the adapter clears the page.
use super::{
    Action, Area, Dialog, Model,
    layout::{self, Layout, Tone, accent, dim, err, inherit, ok, pad_str, span, styled, warn},
    view::spinner,
};
use crate::{
    controller::{Command, Outcome, State, Ticket},
    error::Error,
    storage,
    ui::{
        Backend,
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

pub struct Download {
    pub ticket: Ticket,
    pub path: String,
    pub local: PathBuf,
    pub size: Option<u64>,
    pub state: Transfer,
}

#[derive(Default)]
pub struct Files {
    pub open: bool,
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub listing: Option<Ticket>,
    pub state: Option<Listing>,
    /// The selected row's name.
    pub selected: Option<String>,
    pub list_scroll: usize,
    pub panel_scroll: usize,
    pub reveal: bool,
    pub dest: Field,
    pub editing: bool,
    pub dest_err: String,
    pub download: Option<Download>,
}

impl Files {
    /// Drops what belonged to the previous adapter.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
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

/// The destination for a newly selected file: its name in the directory the
/// destination already names, so a chosen directory is kept.
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

impl<B: Backend> Model<B> {
    pub(super) fn files_open(&self, st: &State) -> bool {
        self.files.open && st.available
    }

    pub(super) fn open_files(&mut self) {
        self.close_settings();
        self.files.open = true;
        if self.files.path.is_empty() {
            self.list_files("/".into());
        }
    }

    pub(super) fn close_files(&mut self) {
        self.files.open = false;
        self.files.editing = false;
    }

    /// Starts reading a directory, replacing the rows shown.
    fn list_files(&mut self, path: String) {
        self.files.path = path.clone();
        self.files.entries.clear();
        self.files.selected = None;
        self.files.list_scroll = 0;
        self.files.state = Some(Listing::Loading);
        self.files.listing = Some(self.backend.run(Command::Files(path)));
    }

    /// Takes a file command's result; false for other tickets.
    pub(super) fn files_done(&mut self, ticket: Ticket, result: &Result<Outcome, Error>) -> bool {
        if self.files.listing == Some(ticket) {
            self.files.listing = None;
            self.files.state = Some(match result {
                Ok(Outcome::Files { entries, .. }) => {
                    self.files.entries = entries.clone();
                    Listing::Complete
                }
                Ok(_) => Listing::Complete,
                Err(e) => Listing::Failed(text::error_words(e)),
            });
            return true;
        }
        let Some(d) = self.files.download.as_mut().filter(|d| d.ticket == ticket) else {
            return false;
        };
        let name = display(&d.path);
        match result {
            Ok(Outcome::FileSaved { bytes, .. }) => {
                d.state = Transfer::Saved(*bytes);
                let text = format!(
                    "Downloaded {name} to {}",
                    display(&d.local.display().to_string())
                );
                self.note(super::activity::Kind::Good, text);
            }
            Ok(_) => {}
            Err(e) => {
                let error = format!("{}; no file was saved.", text::error_words(e));
                d.state = Transfer::Failed(error.clone());
                self.note(
                    super::activity::Kind::Bad,
                    format!("Download of {name}: {error}"),
                );
            }
        }
        true
    }

    fn select_file(&mut self, name: String) {
        if self.files.selected.as_deref() == Some(&name) {
            return;
        }
        let dest = keep_directory(&self.files.dest.value(), &name);
        self.files.dest.set_value(&dest);
        self.files.dest_err.clear();
        self.files.selected = Some(name);
        self.files.panel_scroll = 0;
        self.files.reveal = true;
    }

    /// Moves the selection through the rows as listed.
    pub(super) fn move_file(&mut self, delta: isize) {
        let names: Vec<String> = self
            .files
            .rows()
            .into_iter()
            .map(|e| e.name.clone())
            .collect();
        if names.is_empty() {
            return;
        }
        let i = match names
            .iter()
            .position(|n| Some(n) == self.files.selected.as_ref())
        {
            None if delta < 0 => names.len() - 1,
            None => 0,
            Some(i) => i.saturating_add_signed(delta).min(names.len() - 1),
        };
        self.select_file(names[i].clone());
        self.focus = None;
    }

    /// Opens the selected directory, or downloads the selected file.
    pub(super) fn files_enter(&mut self) {
        let Some(name) = self.files.selected.clone() else {
            return;
        };
        match self.files.entry(&name).map(|e| e.directory) {
            Some(true) => self.action(Action::FilesEntry(name)),
            Some(false) => self.action(Action::FilesDownload),
            None => {}
        }
    }

    /// Starts a download of the selected file to the typed destination.
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

    /// Downloads exactly this target, asking before replacing a local file.
    fn request_download(&mut self, target: Target) {
        if self.files.busy() {
            return;
        }
        match storage::check_destination(&target.local, false) {
            Err(e) if e.message == storage::EXISTS => {
                self.dialog = Some(Dialog::Replace(target));
                self.dialog_scroll = 0;
            }
            Err(e) => self.files.dest_err = text::capitalized(&text::error_words(&e)),
            Ok(()) => self.begin_download(target, false),
        }
    }

    fn begin_download(&mut self, target: Target, overwrite: bool) {
        self.files.editing = false;
        self.files.dest_err.clear();
        let ticket = self.backend.run(Command::FileGet {
            path: target.path.clone(),
            local: target.local.clone(),
            overwrite,
        });
        self.files.download = Some(Download {
            ticket,
            path: target.path,
            local: target.local,
            size: target.size,
            state: Transfer::Running,
        });
    }

    /// Handles a Files control; false for other actions.
    pub(super) fn files_action(&mut self, action: &Action) -> bool {
        match action {
            Action::FilesOpen => self.open_files(),
            Action::FilesClose => self.close_files(),
            Action::FilesUp => {
                if let Some(p) = parent(&self.files.path) {
                    self.list_files(p);
                }
            }
            Action::FilesRefresh => {
                let path = self.files.path.clone();
                self.list_files(if path.is_empty() { "/".into() } else { path });
            }
            Action::FilesEntry(name) => match self.files.entry(name).map(|e| e.directory) {
                Some(true) => {
                    let path = child(&self.files.path, name);
                    self.list_files(path);
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
                // The failed transfer's own file and destination, whatever is
                // selected or typed now.
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
    pub(super) fn confirm_replace(&mut self, target: Target) {
        if !self.files.busy() {
            self.begin_download(target, true);
        }
    }

    pub(super) fn files_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let mut b = Layout::new(w.saturating_sub(4));
        let size_w = 10;
        let name_w = b.width.saturating_sub(2 + size_w).max(4);
        let mut selected_line = None;
        if !st.status.ready {
            b.hang(
                Line::default(),
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
            if chosen {
                selected_line = Some(b.lines.len());
            }
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
            let fill = b.width.saturating_sub(layout::line_width(&line));
            line.spans.push(span(" ".repeat(fill), base));
            b.control(line, Action::FilesEntry(e.name.clone()));
        }
        match &self.files.state {
            Some(Listing::Loading) => {
                b.line(styled(format!("  {} Reading…", spinner()), warn()));
            }
            Some(Listing::Failed(e)) => {
                let note = if rows.is_empty() {
                    format!("✕ Couldn't read this directory: {e}")
                } else {
                    format!("✕ Incomplete: {e}")
                };
                b.hang(Line::from("  "), &note, err());
            }
            Some(Listing::Complete) if rows.is_empty() => {
                b.line(styled("  Empty directory", dim()));
            }
            _ => {}
        }
        if self.files.reveal
            && let Some(line) = selected_line
        {
            self.files.reveal = false;
            self.files.list_scroll = self
                .files
                .list_scroll
                .max(line.saturating_sub(h.saturating_sub(4)))
                .min(line);
        }
        let mut pinned = Layout::new(b.width);
        if parent(&self.files.path).is_some() {
            pinned.button("Up", Action::FilesUp, Tone::Normal);
        }
        pinned.button("Refresh", Action::FilesRefresh, Tone::Normal);
        pinned.button_right("Close", Action::FilesClose, Tone::Normal);
        let heading = format!("Files · {}", display(&self.files.path));
        self.frame(&heading, b, pinned, Some(Area::Files), false, w, h)
    }

    pub(super) fn transfer_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let (heading, b, actions) = self.transfer(st, w);
        self.frame(&heading, b, actions, Some(Area::Transfer), false, w, h)
    }

    /// The download panel for the selected file, or the running download.
    pub(super) fn transfer(&mut self, st: &State, w: usize) -> (String, Layout, Layout) {
        let mut b = Layout::new(w.saturating_sub(4));
        let mut actions = Layout::new(b.width);
        let entry = self
            .files
            .selected
            .as_deref()
            .and_then(|n| self.files.entry(n))
            .cloned();
        if let Some(d) = &self.files.download {
            let shown = display(&d.path);
            let local = display(&d.local.display().to_string());
            match &d.state {
                Transfer::Running => {
                    b.field("File", &shown, layout::plain());
                    b.field("Saving To", &local, layout::plain());
                    b.line(styled(format!("{} Downloading…", spinner()), warn()));
                    return ("Download".into(), b, actions);
                }
                Transfer::Saved(n) => {
                    b.field("Saved", &shown, ok());
                    b.field("To", &local, layout::plain());
                    b.field("Size", &format!("{n} bytes"), layout::plain());
                    b.row();
                }
                Transfer::Failed(error) => {
                    b.field("Not Saved", &shown, err());
                    b.para(error, err());
                    if !self.files.dest_err.is_empty() {
                        b.para(&self.files.dest_err.clone(), err());
                    }
                    if self.offers(st, &Action::FilesRetry) {
                        actions.button("Retry", Action::FilesRetry, Tone::Primary);
                    }
                    b.row();
                }
            }
        }
        let Some(e) = entry.filter(|e| !e.directory) else {
            if self.files.download.is_none() {
                b.para(
                    "Select a file to download it. Click a directory to open it.",
                    dim(),
                );
            }
            return ("Download".into(), b, actions);
        };
        b.field(
            "File",
            &display(&child(&self.files.path, &e.name)),
            layout::plain(),
        );
        b.field("Size", &format!("{} bytes", e.size), layout::plain());
        b.line(styled("Save As", dim()));
        let focused = self.files.editing;
        let line = super::view::edit_line(&mut self.files.dest, focused, b.width.saturating_sub(3));
        b.control(line, Action::FilesDest);
        if !self.files.dest_err.is_empty() {
            b.para(&self.files.dest_err.clone(), err());
        }
        b.para(
            "A relative name is saved in the directory cordial was started from.",
            dim(),
        );
        if self.offers(st, &Action::FilesDownload) {
            actions.button("Download", Action::FilesDownload, Tone::Primary);
        }
        (display(&e.name), b, actions)
    }
}
