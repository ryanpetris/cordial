//! Files, on development firmware: a dialog listing one directory of the adapter's filesystem at
//! a time, with a download of the selected file. A saved record is downloaded as JSON, under its
//! name with `.json` in place of `.pb`. A download is written to a private temporary file, which
//! becomes the chosen local file only once the whole file has arrived; an existing local file is
//! replaced only after the Replace confirmation.
use super::{
    Action, Model,
    fleet::Fleet,
    layout::{self, Layout, Tone, accent, dim, err, inherit, ok, pad_str, span, styled, warn},
    render::{DialogView, busy, button_if},
};
use crate::ui::text::{self, display};
use cordial_protocol::FileEntry;
use ratatui::text::Line;

use crate::ui::app::files::*;

impl<F: Fleet> Model<F> {
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
                Transfer::Saved(n, unconverted) => {
                    b.fact("Saved", &shown, ok());
                    b.fact("To", &local, layout::plain());
                    b.fact("Size", &format!("{n} bytes"), layout::plain());
                    if let Some(why) = unconverted {
                        b.para(
                            &format!("Saved without converting to JSON: {}.", why.reason()),
                            warn(),
                        );
                    }
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
            let focused = self.files_editing;
            let line = super::keys::edit_line(&mut self.files_dest, focused, w.saturating_sub(3));
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
                display(&saved_to(target).display().to_string())
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
