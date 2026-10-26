//! Files, on development firmware: a dialog listing one directory of the adapter's filesystem at
//! a time, with a download of the selected file. A saved record is downloaded as JSON, under its
//! name with `.json` in place of `.pb`. A download is written to a private temporary file, which
//! becomes the chosen local file only once the whole file has arrived; an existing local file is
//! replaced only after the Replace confirmation.
use super::{Action, App, Dialog, Job, Kind, fleet::Fleet};
use crate::{
    controller::{Command, Outcome},
    error::Error,
    records::{self, Unconverted},
    storage,
    ui::text::{self},
};
use cordial_protocol::FileEntry;
use std::path::PathBuf;

use super::Input;
#[derive(Clone, Debug, PartialEq)]
pub enum Listing {
    Loading,
    Complete,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Transfer {
    Running,
    /// Saved to the download's local file, with the reason when it was saved unconverted.
    Saved(u64, Option<Unconverted>),
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
    pub dest: Input,
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
    /// Looks up an entry whose name is exactly one local filename component.
    pub fn entry(&self, name: &str) -> Option<&FileEntry> {
        if std::path::Path::new(name).file_name() != Some(name.as_ref()) || name.contains('\0') {
            return None;
        }
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

/// Where a download is expected to be saved: as JSON for a known record.
pub(crate) fn saved_to(target: &Target) -> PathBuf {
    records::destination(&target.path, &target.local, false)
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

impl<F: Fleet> App<F> {
    /// Downloads the selected file to a native file chooser's destination.
    /// `overwrite` records whether the chooser confirmed replacing an existing file.
    pub fn download_to(&mut self, local: PathBuf, overwrite: bool) {
        let Some(adapter) = self.files_adapter() else {
            return;
        };
        if self.files.busy() {
            return;
        }
        let Some(entry) = self
            .files
            .selected
            .as_deref()
            .and_then(|name| self.files.entry(name))
            .filter(|e| !e.directory)
        else {
            return;
        };
        let target = Target {
            path: child(&self.files.path, &entry.name),
            local,
            size: Some(entry.size),
        };
        if overwrite && saved_to(&target) == target.local {
            self.begin_download(&adapter, target, true);
        } else {
            self.request_download(target);
        }
    }

    pub(crate) fn open_files(&mut self, adapter: &str) {
        self.files = Files::default();
        self.dialog = Some(Dialog::Files(adapter.to_owned()));
        self.list_files(adapter, "/".into());
    }
    pub(crate) fn files_adapter(&self) -> Option<String> {
        match &self.dialog {
            Some(Dialog::Files(a) | Dialog::Replace(a, _)) => Some(a.clone()),
            _ => None,
        }
    }
    pub(crate) fn list_files(&mut self, adapter: &str, path: String) {
        self.files.path = path.clone();
        self.files.entries.clear();
        self.files.selected = None;
        self.files.state = Some(Listing::Loading);
        self.run(adapter, Kind::Files, Command::Files(path));
    }
    pub(crate) fn files_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
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
                    Ok(Outcome::FileSaved {
                        bytes,
                        local,
                        unconverted,
                        ..
                    }) => {
                        d.local.clone_from(local);
                        d.state = Transfer::Saved(*bytes, *unconverted);
                    }
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
    pub(crate) fn select_file(&mut self, name: String) {
        if self.files.selected.as_deref() == Some(&name) {
            return;
        }
        let dest = keep_directory(&self.files.dest.value(), &name);
        let dest = records::destination(
            &child(&self.files.path, &name),
            std::path::Path::new(&dest),
            false,
        );
        self.files.dest.set_value(&dest.display().to_string());
        self.files.dest_err.clear();
        self.files.selected = Some(name);
    }
    pub(crate) fn start_download(&mut self) {
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
    pub(crate) fn request_download(&mut self, target: Target) {
        let Some(adapter) = self.files_adapter() else {
            return;
        };
        if self.files.busy() {
            return;
        }
        match storage::check_destination(&saved_to(&target), false) {
            Err(e) if e.message == storage::EXISTS => {
                self.dialog = Some(Dialog::Replace(adapter, target));
            }
            Err(e) => self.files.dest_err = text::capitalized(&text::error_words(&e)),
            Ok(()) => self.begin_download(&adapter, target, false),
        }
    }
    pub(crate) fn begin_download(&mut self, adapter: &str, target: Target, overwrite: bool) {
        self.files.dest_err.clear();
        self.files.download = Some(Download {
            path: target.path.clone(),
            local: saved_to(&target),
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
                raw: false,
            },
        );
    }
    /// Handles a Files control; false for other actions.
    pub(crate) fn files_action(&mut self, action: &Action) -> bool {
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
            Action::FilesDest => {}
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
    pub(crate) fn confirm_replace(&mut self, adapter: &str, target: Target) {
        self.dialog = Some(Dialog::Files(adapter.to_owned()));
        if !self.files.busy() {
            self.begin_download(adapter, target, true);
        }
    }
}
