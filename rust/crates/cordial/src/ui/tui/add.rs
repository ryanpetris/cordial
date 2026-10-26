//! Add Device: scans with one adapter while the dialog is open, pairs a nearby device, answers
//! its prompts, and follows the new device until it connects or can't. Closing the dialog stops
//! the scan and cancels a pairing that hasn't saved its bond.
use super::{
    Action, Model,
    fleet::Fleet,
    layout::{self, Layout, Tone, dim, err, ok, span, styled},
    render::{DialogView, busy, button_if},
    words::{self, Scope},
};
use crate::model::{self, Prompt};
use cordial_protocol::{self as p, CodeKind};
use ratatui::{style::Style, text::Line};

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

use crate::ui::app::add::*;

impl<F: Fleet> Model<F> {
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
