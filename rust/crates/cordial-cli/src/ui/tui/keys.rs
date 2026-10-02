//! Keyboard control mirrors the mouse: Tab reaches every visible control, and
//! device shortcuts act only when the selected device offers that action.
//! The pointer and Tab share one highlight: moving the pointer highlights the
//! control under it, or none, until a key is pressed.
use super::{Action, Dialog, Hit, MIN_HEIGHT, MIN_WIDTH, Menu, Model, scan_choices};
use crate::{controller::State, model::Prompt, ui::Backend};
use cordial_protocol::value::Value;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A key as the handlers name it, such as `ctrl+c`, `shift+tab` or `q`.
pub fn name(k: &KeyEvent) -> String {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let base = match k.code {
        KeyCode::Char(c) if ctrl => return format!("ctrl+{}", c.to_ascii_lowercase()),
        KeyCode::Char(c) if alt => return format!("alt+{c}"),
        KeyCode::Char(c) => return c.to_string(),
        KeyCode::Enter => "enter",
        KeyCode::Tab if k.modifiers.contains(KeyModifiers::SHIFT) => "shift+tab",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => "shift+tab",
        KeyCode::Esc => "esc",
        KeyCode::Backspace => "backspace",
        KeyCode::Delete => "delete",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "pgup",
        KeyCode::PageDown => "pgdown",
        _ => "",
    };
    base.to_owned()
}

/// Whether an action is an On or Off option, which Space also chooses.
fn on_off_option(a: &Action) -> bool {
    use Action::*;
    matches!(
        a,
        Enable
            | Disable
            | Trust
            | Untrust
            | Block
            | Unblock
            | Hidpp(_)
            | ShowUnnamed(_)
            | Draft(_, Value::Bool(_))
            | Switch(..)
    )
}

impl<B: Backend> Model<B> {
    pub(super) fn focus_hit(&self) -> Option<Hit> {
        let focus = self.focus.as_ref()?;
        self.hits.iter().find(|h| h.action == *focus).cloned()
    }

    fn move_focus(&mut self, delta: isize) {
        let controls: Vec<&Action> = self
            .hits
            .iter()
            .map(|h| &h.action)
            .filter(|a| !matches!(a, Action::Wheel(_)))
            .collect();
        if controls.is_empty() {
            return;
        }
        let n = controls.len() as isize;
        let i = match self
            .focus
            .as_ref()
            .and_then(|f| controls.iter().position(|a| *a == f))
        {
            Some(i) => i as isize,
            None if delta < 0 => 0,
            None => -1,
        };
        self.focus = Some(controls[((i + delta).rem_euclid(n)) as usize].clone());
        self.focus_ctx = self.focus_context();
    }

    /// What the highlighted control belongs to. The highlight names an
    /// action, and actions such as Confirm recur in different dialogs.
    pub(super) fn focus_context(&self) -> String {
        let selected = if matches!(self.focus, Some(Action::Device(_))) {
            "" // Selecting the highlighted row keeps the Tab position.
        } else {
            &self.selected
        };
        let setting = match (&self.focus, &self.page.key) {
            (Some(Action::Setting(_) | Action::Category(_)), _) | (_, None) => String::new(),
            (_, Some(k)) => k.clone(),
        };
        let files = if self.files.open {
            self.files.path.as_str()
        } else {
            ""
        };
        format!(
            "{}|{:?}|{:?}|{:?}|{selected}|{}|{setting}|{files}",
            self.auth_key,
            self.dialog,
            self.menu,
            self.gate(),
            self.page.device
        )
    }

    pub(super) fn sync_focus(&mut self) {
        let c = self.focus_context();
        if c != self.focus_ctx {
            self.focus = None;
            self.focus_ctx = c;
        }
    }

    /// Highlights the control drawn under the pointer, if the pointer placed
    /// the highlight. Each frame repeats it, so the highlight leaves controls
    /// that scroll, move or are covered while the pointer stays still.
    pub(super) fn hovered(&mut self) {
        if !self.hover {
            return;
        }
        self.focus = self.target(self.px, self.py);
        if self.focus == Some(Action::Input) && self.form_focused
            || self.focus == Some(Action::FilesDest) && self.files.editing
        {
            self.focus = None; // The field being typed in keeps its cursor.
        }
        self.focus_ctx = self.focus_context();
    }

    /// Drops the pointer's highlight once its cell is no longer known.
    pub(super) fn unhover(&mut self) {
        if self.hover {
            self.focus = None;
            self.hover = false;
        }
    }

    /// The first of `actions` the selected device offers now.
    fn offered(&self, actions: &[Action]) -> Option<Action> {
        let st = self.state()?;
        self.device_actions(&st)
            .into_iter()
            .map(|c| c.action)
            .find(|a| actions.contains(a))
    }

    fn shortcut(&mut self, actions: &[Action]) {
        if let Some(a) = self.offered(actions) {
            self.action(a);
        }
    }

    /// Moves the selection through the rows as listed: saved, then nearby.
    fn move_selection(&mut self, delta: isize) {
        let Some(st) = self.state() else {
            return;
        };
        let ids: Vec<String> = st
            .devices
            .iter()
            .map(|d| d.id.clone())
            .chain(st.candidates.iter().map(|c| c.id.clone()))
            .collect();
        if ids.is_empty() {
            return;
        }
        let i = match ids.iter().position(|id| *id == self.selected) {
            None if delta < 0 => ids.len() - 1,
            None => 0,
            Some(i) => i.saturating_add_signed(delta).min(ids.len() - 1),
        };
        self.selected = ids[i].clone();
        self.detail_scroll = 0;
        self.reveal = true;
        self.focus = None;
    }

    /// Handles a key in the full-screen view; false leaves it to the focused
    /// text field.
    pub(super) fn full_key(&mut self, key: &str) -> bool {
        if self.width < MIN_WIDTH || self.height < MIN_HEIGHT {
            // Only Quit is drawn while the terminal is too small.
            if key == "q" || key == "enter" && self.focus == Some(Action::Quit) {
                self.quit();
                return true;
            }
            return !self.form_focused;
        }
        self.sync_focus();
        match key {
            "esc" => {
                if self.focus.is_some()
                    && self.menu.is_none()
                    && self.dialog.is_none()
                    && self.auth().is_none()
                    && !self.chooser
                {
                    self.focus = None;
                } else {
                    self.action(Action::CancelDialog);
                }
                return true;
            }
            "tab" | "shift+tab" => {
                self.move_focus(if key == "tab" { 1 } else { -1 });
                self.form_focused = false;
                self.files.editing = false;
                return true;
            }
            _ => {}
        }
        // Space also chooses a focused option, outside text entry.
        let check = key == " "
            && !self.form_focused
            && !self.files.editing
            && self.focus.as_ref().is_some_and(on_off_option);
        if (key == "enter" || check)
            && let Some(h) = self.focus_hit()
        {
            self.action(h.action); // Also ahead of a text field.
            return true;
        }
        if self.files.editing {
            self.focus = None;
            if key == "enter" {
                self.files.editing = false;
                self.action(Action::FilesDownload);
                return true;
            }
            return false;
        }
        if self.form_focused {
            self.focus = None; // Typing returns the keys to the field.
            if key == "enter" && self.dialog == Some(Dialog::Rename) {
                self.action(Action::SaveName);
                return true;
            }
            if key == "enter" && self.auth().is_some() {
                self.action(Action::Accept);
                return true;
            }
            return false;
        }
        match key {
            "q" => {
                self.quit();
                return true;
            }
            "?" => {
                self.action(Action::Help);
                return true;
            }
            _ => {}
        }
        let up = key == "up" || key == "k";
        let down = key == "down" || key == "j";
        let step = if up { -1 } else { 1 };
        let a = self.auth();
        if self.menu.is_some() || self.gate().is_some() && self.dialog.is_none() && a.is_none() {
            if up || down {
                self.move_focus(step);
            } else if key == "r" && self.menu.is_none() && self.gate() == Some("chooser") {
                self.action(Action::RefreshPorts);
            }
            return true;
        }
        if let Some((_, prompt)) = a {
            if matches!(prompt, Prompt::ConfirmCode(_)) {
                match key {
                    "y" => self.action(Action::Accept),
                    "n" => self.action(Action::Reject),
                    _ => {}
                }
            }
            return true;
        }
        match self.dialog {
            Some(Dialog::Help | Dialog::Diagnostics) => {
                if key == "r" && self.dialog == Some(Dialog::Diagnostics) {
                    // Like the dialog's Refresh Info button, only for a connected device.
                    if self.diagnosed_connected() {
                        self.action(Action::RefreshInfo);
                    }
                    return true;
                }
                let delta: isize = match key {
                    "up" | "k" => -1,
                    "down" | "j" => 1,
                    "pgup" => -5,
                    "pgdown" => 5,
                    _ => 0,
                };
                self.dialog_scroll = self.dialog_scroll.saturating_add_signed(delta);
                return true;
            }
            Some(Dialog::Remove(_) | Dialog::Bootloader | Dialog::Replace(_)) => {
                match key {
                    "y" => self.action(Action::Confirm),
                    "n" => self.action(Action::CancelDialog),
                    _ => {}
                }
                return true;
            }
            Some(Dialog::Settings | Dialog::Rename) => {
                match key {
                    "up" | "k" | "left" => self.move_focus(-1),
                    "down" | "j" | "right" => self.move_focus(1),
                    _ => {}
                }
                return true;
            }
            None => {}
        }
        let Some(st) = self.state() else {
            return true;
        };
        if self.files_open(&st) {
            match key {
                _ if up || down => self.move_file(step),
                "home" | "end" => {
                    self.files.selected = None;
                    self.move_file(if key == "home" { 1 } else { -1 });
                }
                "enter" => self.files_enter(),
                "backspace" | "u" => self.action(Action::FilesUp),
                "left" => self.move_focus(-1),
                "right" => self.move_focus(1),
                "r" => self.action(Action::FilesRefresh),
                "d" => self.action(Action::FilesDownload),
                "pgup" => self.event_scroll += 5,
                "pgdown" => self.event_scroll = self.event_scroll.saturating_sub(5),
                "a" => self.action(Action::Menu(Menu::Adapter)),
                _ => {}
            }
            return true;
        }
        if self.settings_open(&st) {
            // Left and Right move between buttons once one is highlighted;
            // otherwise they edit the selected setting.
            let button = self.focus.is_some();
            match key {
                _ if up || down => self.move_setting(step),
                "left" if button => self.move_focus(-1),
                "right" if button => self.move_focus(1),
                "left" | "right" => self.edit_selected(if key == "left" { -1 } else { 1 }, false),
                " " => self.edit_selected(0, true),
                "backspace" => self.undo_selected(),
                "home" | "end" => {
                    self.page.key = None;
                    self.move_setting(if key == "home" { 1 } else { -1 });
                }
                "pgup" => self.event_scroll += 5,
                "pgdown" => self.event_scroll = self.event_scroll.saturating_sub(5),
                "s" => self.action(Action::SaveAll),
                "r" => self.action(Action::SettingsRefresh),
                "a" => self.action(Action::Menu(Menu::Adapter)),
                _ => {}
            }
            return true;
        }
        match key {
            _ if up || down => self.move_selection(step),
            "home" | "end" => {
                self.selected.clear();
                self.move_selection(if key == "home" { 1 } else { -1 });
            }
            "pgup" => self.event_scroll += 5,
            "pgdown" => self.event_scroll = self.event_scroll.saturating_sub(5),
            // Enter never disconnects: the selected device may be the
            // keyboard in use.
            "enter" => self.shortcut(&[Action::Pair, Action::Connect]),
            "s" if st.scanning.is_some() && st.available => self.action(Action::ScanOff),
            // The widest scan offered: both transports, or the only one.
            "s" if st.available => {
                if let Some(t) = scan_choices(&st).first() {
                    self.action(Action::Scan(*t));
                }
            }
            "a" => self.action(Action::Menu(Menu::Adapter)),
            "r" if st.available => self.action(Action::Refresh),
            "o" => self.shortcut(&[Action::DeviceSettings]),
            "i" => self.shortcut(&[Action::Diagnostics]),
            "p" => self.shortcut(&[Action::Pair]),
            "c" => self.shortcut(&[Action::Connect]),
            "d" => self.shortcut(&[Action::Disconnect]),
            "e" => self.shortcut(&[Action::Enable, Action::Disable]),
            "t" => self.shortcut(&[Action::Trust, Action::Untrust]),
            "b" => self.shortcut(&[Action::Block, Action::Unblock]),
            "x" | "delete" => self.shortcut(&[Action::Remove]),
            "h" => self.shortcut(&[Action::Hide]),
            _ => {}
        }
        true
    }

    /// The keys that apply to what is on screen. Enter presses the
    /// highlighted control first, on every screen.
    pub(super) fn hints(&self, st: Option<&State>) -> String {
        let enter = if self.focus_hit().is_some() {
            "⏎ press highlighted · "
        } else {
            ""
        };
        let a = self.auth();
        if self.menu.is_some() {
            return format!("↑↓ move · {enter}esc close");
        }
        if let Some((_, prompt)) = a {
            let display = matches!(prompt, Prompt::ShowCode(..));
            let esc = if display {
                "esc cancel pairing"
            } else {
                "esc cancel"
            };
            let lead = if display {
                ""
            } else if matches!(prompt, Prompt::ConfirmCode(_)) {
                "y codes match · n codes differ · "
            } else if self.form_focused && enter.is_empty() {
                "type the code · ⏎ submit · "
            } else if self.form_focused {
                "type the code · "
            } else {
                "tab move · " // Tab left the field.
            };
            let text = format!("{lead}{enter}{esc}");
            return match text.trim_end_matches(" · ") {
                "" => "ctrl+c quit".into(),
                text => text.into(),
            };
        }
        match (&self.dialog, self.gate()) {
            (Some(Dialog::Help), _) => return format!("↑↓ scroll · {enter}esc close"),
            (Some(Dialog::Diagnostics), _) if self.diagnosed_connected() => {
                return format!("↑↓ scroll · {enter}r refresh · esc close");
            }
            (Some(Dialog::Diagnostics), _) => return format!("↑↓ scroll · {enter}esc close"),
            (Some(Dialog::Remove(_) | Dialog::Bootloader | Dialog::Replace(_)), _) => {
                return format!("y confirm · {enter}n or esc cancel");
            }
            (Some(Dialog::Settings | Dialog::Rename), _) => {
                return format!("←→ move · {enter}esc close");
            }
            (None, Some("chooser")) => return format!("↑↓ tab move · {enter}r refresh · q quit"),
            (None, Some(_)) => return format!("tab move · {enter}? help · q quit"),
            _ => {}
        }
        if let Some(st) = st
            && self.files_open(st)
        {
            if self.files.editing {
                return format!("type a file name · {enter}⏎ download · esc done");
            }
            return format!("↑↓ select · {enter}⏎ open · ⌫ up · r refresh · esc close");
        }
        if let Some(st) = st
            && self.settings_open(st)
        {
            let _ = st;
            return format!(
                "↑↓ select · ←→ change · {enter}s save · ⌫ undo · r refresh · esc back"
            );
        }
        let primary = if !enter.is_empty() {
            " · ⏎ press highlighted"
        } else {
            match self.offered(&[Action::Pair, Action::Connect]) {
                Some(Action::Pair) => " · ⏎ pair",
                Some(Action::Connect) => " · ⏎ connect",
                _ => "",
            }
        };
        let scan = match st {
            Some(st) if st.scanning.is_some() => " · s stop scan",
            Some(st) if !scan_choices(st).is_empty() => " · s scan",
            _ => "",
        };
        format!("↑↓ select{primary}{scan} · a adapter · ? help · q quit")
    }
}
