//! Keyboard and mouse. Keys mirror the mouse: Tab reaches every visible control, Up and Down
//! move through the sidebar or between controls, and Enter or Space presses the highlighted
//! control. The pointer and Tab share one highlight: moving the pointer highlights the control
//! under it, or none, until a key is pressed.
use super::{
    Action, Area, Dialog, Kind, Menu, Model, Page, Spot, Tab,
    fleet::Fleet,
    layout::{Hit, Styled, span, strip},
};
use crate::{controller::Command, ui::text};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    style::{Modifier, Style},
    text::Line,
};

/// A key as the handlers name it, such as `ctrl+c`, `shift+tab` or `q`.
pub fn name(k: &KeyEvent) -> String {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let base = match k.code {
        KeyCode::Char(c) if ctrl => return format!("ctrl+{}", c.to_ascii_lowercase()),
        KeyCode::Char(c) if alt => return format!("alt+{c}"),
        KeyCode::Char(c) => return c.to_string(),
        KeyCode::F(n) => return format!("f{n}"),
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
        KeyCode::Menu => "menu",
        _ => "",
    };
    base.to_owned()
}

/// A text field: its prompt, the visible text and a cursor while it has focus.
pub fn edit_line(field: &mut crate::ui::field::Field, focused: bool, w: usize) -> Styled {
    let (value, column) = field.view(w.saturating_sub(3).max(1));
    let mut line = Line::from(vec![span("> ", Style::new())]);
    if !focused {
        line.spans.push(span(value, Style::new()));
        return line;
    }
    // The cursor is drawn in reverse video on its character or a space.
    let before: String = strip(&Line::from(value.clone()))
        .chars()
        .scan(0, |used, c| {
            *used += text::width(&c.to_string());
            (*used <= column).then_some(c)
        })
        .collect();
    let rest = &value[before.len()..];
    let mut chars = rest.chars();
    let at = chars.next().map_or(" ".to_string(), |c| c.to_string());
    line.spans.push(span(before, Style::new()));
    line.spans
        .push(span(at, Style::new().add_modifier(Modifier::REVERSED)));
    line.spans
        .push(span(chars.as_str().to_owned(), Style::new()));
    line
}

impl<F: Fleet> Model<F> {
    /// The dialog's text field.
    pub(super) fn field_line(&mut self, w: usize) -> Styled {
        edit_line(&mut self.form, self.form_focused, w)
    }

    pub(super) fn focus_hit(&self) -> Option<Hit> {
        let focus = self.focus.as_ref()?;
        self.hits.iter().find(|h| h.action == *focus).cloned()
    }

    /// The region a cell is in.
    fn area_at(&self, x: usize, y: usize) -> Option<Area> {
        self.regions
            .iter()
            .rev()
            .find(|(_, (rx, ry, rw, rh))| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh)
            .map(|(a, _)| *a)
    }

    /// The control under a cell.
    fn target(&self, x: usize, y: usize) -> Option<Action> {
        self.hits
            .iter()
            .find(|h| y == h.y && x >= h.x && x < h.x + h.w)
            .map(|h| h.action.clone())
    }

    /// What the highlight belongs to; the highlight names an action, and actions recur on other
    /// pages and in other dialogs.
    fn focus_context(&self) -> String {
        // The picker's choice changes without moving the highlight.
        let dialog = match &self.dialog {
            Some(Dialog::Pick {
                adapter, purpose, ..
            }) => format!("Pick {adapter} {purpose:?}"),
            other => format!("{other:?}"),
        };
        format!(
            "{}|{}|{:?}|{:?}|{:?}",
            dialog,
            self.menu.is_some(),
            self.shown(),
            self.tab,
            self.add
                .as_ref()
                .map(|a| a.pairing.as_ref().map(|p| p.phase))
        )
    }

    pub(super) fn sync_focus(&mut self) {
        let c = self.focus_context();
        if c != self.focus_ctx {
            self.focus_ctx = c;
            if !matches!(self.focus, Some(Action::Open(_))) {
                self.focus = None;
            }
        }
        if let Some(f) = &self.focus
            && !self.hits.iter().any(|h| h.action == *f)
        {
            self.focus = None;
        }
    }

    /// Highlights the control under the pointer, if the pointer placed the highlight.
    pub(super) fn hovered(&mut self) {
        if !self.hover {
            return;
        }
        self.focus = self.target(self.px, self.py);
        if self.focus == Some(Action::Field) && self.form_focused
            || self.focus == Some(Action::FilesDest) && self.files.editing
        {
            self.focus = None;
        }
    }

    /// Drops the pointer's highlight once its cell is no longer known.
    pub(super) fn unhover(&mut self) {
        if self.hover {
            self.focus = None;
            self.hover = false;
        }
    }

    /// Moves the highlight through the controls in `area`, or through every control.
    fn move_focus(&mut self, area: Option<Area>, delta: isize) {
        let controls: Vec<Action> = self
            .hits
            .iter()
            .filter(|h| area.is_none_or(|a| self.area_at(h.x, h.y) == Some(a)))
            .map(|h| h.action.clone())
            .collect();
        let mut unique: Vec<Action> = Vec::new();
        for c in controls {
            if !unique.contains(&c) {
                unique.push(c);
            }
        }
        if unique.is_empty() {
            return;
        }
        let n = unique.len() as isize;
        let i = match self
            .focus
            .as_ref()
            .and_then(|f| unique.iter().position(|a| a == f))
        {
            Some(i) => i as isize,
            None if delta < 0 => 0,
            None => -1,
        };
        self.focus = Some(unique[((i + delta).rem_euclid(n)) as usize].clone());
        self.form_focused = false;
    }

    /// The pages in sidebar order.
    fn sidebar_pages(&self) -> Vec<Page> {
        std::iter::once(Page::Overview)
            .chain(self.devices.iter().map(|d| d.page()))
            .chain(self.adapters.iter().map(|a| Page::Adapter(a.id.clone())))
            .collect()
    }

    /// Moves the selection through the sidebar and shows its page.
    fn move_selection(&mut self, delta: isize) {
        let pages = self.sidebar_pages();
        let shown = self.shown();
        let i = pages.iter().position(|p| *p == shown).unwrap_or(0);
        let next = i.saturating_add_signed(delta).min(pages.len() - 1);
        let page = pages[next].clone();
        self.open(page.clone());
        self.focus = Some(Action::Open(page));
    }

    /// Switches to the tab `delta` away among the shown page's tabs.
    fn switch_tab(&mut self, delta: isize) {
        let tabs: Vec<Tab> = self
            .hits
            .iter()
            .filter_map(|h| match h.action {
                Action::Tab(t) => Some(t),
                _ => None,
            })
            .collect();
        if tabs.is_empty() {
            return;
        }
        let i = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
        let next = i.saturating_add_signed(delta).min(tabs.len() - 1);
        self.action(Action::Tab(tabs[next]));
    }

    /// The Save shown on the page's bar.
    fn visible_save(&self) -> Option<Action> {
        [
            Action::AdapterSave,
            Action::DetailsSave,
            Action::LayersSave,
            Action::SettingsSave,
        ]
        .into_iter()
        .find(|a| self.hits.iter().any(|h| h.action == *a))
    }

    pub(super) fn mouse(&mut self, m: MouseEvent) {
        self.px = usize::from(m.column);
        self.py = usize::from(m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.armed = self.target(self.px, self.py);
                self.focus = None;
                if self.armed.is_none() && self.menu.is_some() {
                    self.menu = None;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let target = self.target(self.px, self.py);
                let armed = self.armed.take();
                if let Some(target) = target
                    && Some(&target) == armed.as_ref()
                {
                    self.action(target);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(Action::Open(Page::Adapter(id))) = self.target(self.px, self.py) {
                    self.adapter_menu(&id, self.px, self.py + 1);
                }
            }
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.hover = true;
                self.hovered();
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                if let Some(area) = self.area_at(self.px, self.py) {
                    self.scroll(area, if up { -1 } else { 1 });
                }
            }
            _ => {}
        }
    }

    fn scroll(&mut self, area: Area, delta: isize) {
        let s = match area {
            Area::Sidebar => &mut self.side_scroll,
            Area::Main => &mut self.main_scroll,
            Area::Dialog => &mut self.dialog_scroll,
        };
        *s = s.saturating_add_signed(delta);
    }

    /// Opens an adapter's menu at a place.
    fn adapter_menu(&mut self, id: &str, x: usize, y: usize) {
        let Some(a) = self.adapter(id) else { return };
        let mut items = Vec::new();
        if a.connected() {
            items.push((
                "Disconnect".to_owned(),
                Some(Action::DisconnectAdapter(id.into())),
            ));
        } else {
            let can = a.conn == super::world::Conn::Disconnected;
            items.push((
                "Connect".to_owned(),
                can.then(|| Action::ConnectAdapter(id.into())),
            ));
        }
        let can_rename = a.live().is_some();
        items.push((
            "Rename".to_owned(),
            can_rename.then(|| Action::RenameAdapter(id.into())),
        ));
        self.menu = Some(Menu { x, y, items });
        self.focus = None;
    }

    pub(super) fn key(&mut self, k: KeyEvent) {
        let key = name(&k);
        self.hover = false;
        self.armed = None;
        if key == "ctrl+c" || key == "ctrl+d" {
            self.quit();
            return;
        }
        if self.quitting {
            return;
        }
        if self.editing.is_some() {
            match key.as_str() {
                "enter" => {
                    self.commit_editing();
                    if let Page::Device(a, id) = self.shown() {
                        self.save_settings(&a, id);
                    }
                }
                "esc" => self.revert_editing(),
                "ctrl+s" => {
                    self.commit_editing();
                    if let Page::Device(a, id) = self.shown() {
                        self.save_settings(&a, id);
                    }
                }
                "ctrl+n" | "ctrl+r" | "f5" => {
                    self.commit_editing();
                    self.global_key(&key);
                }
                "tab" | "shift+tab" | "up" | "down" => {
                    self.commit_editing();
                    self.move_focus(
                        Some(Area::Main),
                        if key == "shift+tab" || key == "up" {
                            -1
                        } else {
                            1
                        },
                    );
                }
                _ => {
                    self.form.key(&k);
                }
            }
            return;
        }
        if self.files.editing {
            match key.as_str() {
                "enter" => self.action(Action::FilesDownload),
                "esc" | "tab" | "shift+tab" => self.files.editing = false,
                _ => {
                    self.files.dest_err.clear();
                    self.files.dest.key(&k);
                }
            }
            return;
        }
        if self.form_focused && self.dialog.is_some() {
            match key.as_str() {
                "enter" => {
                    let accept = matches!(self.dialog, Some(Dialog::AddDevice));
                    self.action(if accept {
                        Action::Accept
                    } else {
                        Action::Submit
                    });
                }
                "esc" => self.action(Action::Cancel),
                "tab" | "shift+tab" => {
                    self.form_focused = false;
                    self.move_focus(None, if key == "tab" { 1 } else { -1 });
                }
                _ => {
                    if self.form.key(&k) {
                        self.notes.remove(&Spot::Dialog);
                        if matches!(self.dialog, Some(Dialog::AddDevice)) {
                            self.add_form_changed();
                        }
                    }
                }
            }
            return;
        }
        if self.menu.is_some() {
            match key.as_str() {
                "esc" => self.menu = None,
                "up" | "shift+tab" => self.move_focus(None, -1),
                "down" | "tab" => self.move_focus(None, 1),
                "enter" | " " => {
                    if let Some(a) = self.focus.clone() {
                        self.action(a);
                    }
                }
                _ => {}
            }
            return;
        }
        let in_dialog = self.dialog.is_some();
        match key.as_str() {
            "esc" => {
                if in_dialog {
                    self.action(Action::Cancel);
                } else {
                    self.focus = None;
                }
            }
            "tab" => self.move_focus(None, 1),
            "shift+tab" => self.move_focus(None, -1),
            "enter" | " " => match self.focus.clone() {
                Some(Action::Open(page)) if !in_dialog && key == "enter" => {
                    self.open(page);
                    self.focus = None;
                    self.enter_page = true;
                }
                Some(a) => self.action(a),
                None if in_dialog && key == "enter" => self.action(Action::Submit),
                None => {}
            },
            "up" | "down" => {
                let delta = if key == "up" { -1 } else { 1 };
                let area = self.focus_hit().and_then(|h| self.area_at(h.x, h.y));
                match area {
                    _ if in_dialog => self.move_focus(Some(Area::Dialog), delta),
                    Some(Area::Main) => self.move_focus(Some(Area::Main), delta),
                    _ => self.move_selection(delta),
                }
            }
            "left" | "right" => {
                let delta = if key == "left" { -1 } else { 1 };
                if !self.adjust_focused(delta) && !in_dialog {
                    self.switch_tab(delta as isize);
                }
            }
            "[" if !in_dialog => self.switch_tab(-1),
            "]" if !in_dialog => self.switch_tab(1),
            "1" | "2" | "3" | "4" if !in_dialog => {
                let n: usize = key.parse().unwrap_or(1);
                let tab = self
                    .hits
                    .iter()
                    .filter_map(|h| match h.action {
                        Action::Tab(t) => Some(t),
                        _ => None,
                    })
                    .nth(n - 1);
                if let Some(t) = tab {
                    self.action(Action::Tab(t));
                }
            }
            "pgup" => self.scroll(if in_dialog { Area::Dialog } else { Area::Main }, -5),
            "pgdown" => self.scroll(if in_dialog { Area::Dialog } else { Area::Main }, 5),
            "+" | "=" => {
                self.adjust_focused(1);
            }
            "-" => {
                self.adjust_focused(-1);
            }
            "ctrl+s" if !in_dialog => {
                if let Some(save) = self.visible_save() {
                    self.action(save);
                }
            }
            "ctrl+n" if !in_dialog => self.action(Action::AddDevice),
            "f5" | "ctrl+r" => self.action(Action::RefreshAdapters),
            "?" | "f1" if !in_dialog => self.action(Action::Help),
            "m" | "menu" if !in_dialog => {
                if let Page::Adapter(id) = self.shown() {
                    let (x, y) = self
                        .hits
                        .iter()
                        .find(|h| h.action == Action::Open(Page::Adapter(id.clone())))
                        .map_or((2, 2), |h| (h.x + 2, h.y + 1));
                    self.adapter_menu(&id, x, y);
                }
            }
            "q" if !in_dialog => self.quit(),
            _ => {}
        }
    }

    /// The keys that act anywhere outside dialogs.
    fn global_key(&mut self, key: &str) {
        match key {
            "ctrl+n" if self.dialog.is_none() => self.action(Action::AddDevice),
            "f5" | "ctrl+r" => self.action(Action::RefreshAdapters),
            _ => {}
        }
    }

    /// Presses a control.
    pub(super) fn action(&mut self, action: Action) {
        if self.quitting {
            return;
        }
        if self.editing.is_some() && !matches!(action, Action::SettingStep(..)) {
            self.commit_editing();
        }
        match &action {
            Action::Quit => return self.quit(),
            Action::Help => {
                self.menu = None;
                self.dialog = Some(Dialog::Help);
                self.dialog_scroll = 0;
                return;
            }
            Action::RefreshAdapters => return self.refresh_adapters(),
            Action::AddDevice => return self.open_add(),
            Action::Open(page) | Action::Show(page) => {
                self.menu = None;
                return self.open(page.clone());
            }
            Action::ConnectAdapter(id) => {
                self.menu = None;
                return self.connect_adapter(id);
            }
            Action::DisconnectAdapter(id) => {
                self.menu = None;
                return self.disconnect_adapter(id);
            }
            Action::RenameAdapter(id) => {
                self.menu = None;
                let page = Page::Adapter(id.clone());
                if self.shown() != page {
                    self.open(page);
                }
                return self.open_rename(id);
            }
            Action::Tab(t) => {
                self.tab = *t;
                self.main_scroll = 0;
                return;
            }
            Action::Field => {
                self.form_focused = true;
                self.focus = None;
                return;
            }
            Action::Cancel => return self.cancel(),
            Action::Submit => return self.submit(),
            Action::Confirm => return self.confirm(),
            Action::ResetName => {
                if let Some(Dialog::Rename(id)) = self.dialog.clone() {
                    self.submit_rename(&id, true);
                }
                return;
            }
            Action::Pick(id) => {
                if let Some(Dialog::Pick { chosen, .. }) = &mut self.dialog {
                    *chosen = *id;
                }
                return;
            }
            Action::PickPage(next) => {
                if let Some(Dialog::Pick { adapter, .. }) = self.dialog.clone() {
                    let step = if *next {
                        super::Step::Forward
                    } else {
                        super::Step::Back
                    };
                    self.read_page(&adapter, true, step);
                }
                return;
            }
            _ => {}
        }
        if self.dialog == Some(Dialog::AddDevice) && self.add_action(&action) {
            return;
        }
        if self.files_action(&action) {
            return;
        }
        match self.shown() {
            Page::Adapter(id) => {
                self.adapter_action(&id, &action);
            }
            Page::Device(a, id) => {
                self.device_action(&a, id, &action);
            }
            Page::Overview => {}
        }
    }

    /// Closes the open menu or dialog, unless its request is still running.
    fn cancel(&mut self) {
        if self.menu.take().is_some() {
            return;
        }
        let busy = match self.dialog.clone() {
            Some(Dialog::Rename(id)) => self.renaming(&id),
            Some(Dialog::Forget(a, d)) => {
                self.running(&a, |k| matches!(k, Kind::Forget(x) if *x == d))
            }
            Some(Dialog::ProfileName(a, _) | Dialog::ProfileDelete(a, _)) => {
                self.running(&a, |k| matches!(k, Kind::Profile))
            }
            Some(Dialog::AddDevice) => {
                self.close_add();
                return;
            }
            Some(Dialog::Replace(a, _)) => {
                self.dialog = Some(Dialog::Files(a));
                return;
            }
            _ => false,
        };
        if !busy {
            self.dialog = None;
            self.form_focused = false;
            self.notes.remove(&Spot::Dialog);
        }
    }

    /// The open dialog's main action.
    fn submit(&mut self) {
        match self.dialog.clone() {
            Some(Dialog::Rename(id)) => self.submit_rename(&id, false),
            Some(Dialog::ProfileName(a, copy)) => self.submit_profile_name(&a, copy),
            Some(Dialog::Pick {
                adapter,
                purpose,
                chosen,
            }) => {
                let none = match purpose {
                    super::PickFor::Interface(i) => self
                        .adapter(&adapter)
                        .and_then(|a| self.staged_interface(a, i))
                        .is_some_and(|s| !s.enabled),
                    super::PickFor::Layer(_) => false,
                };
                if none || chosen != 0 {
                    self.choose_profile(&adapter, purpose, chosen);
                }
            }
            Some(Dialog::Help) => self.dialog = None,
            _ => {}
        }
    }

    /// The open confirmation's answer.
    fn confirm(&mut self) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        if self.adapter_confirm(&dialog) {
            return;
        }
        match dialog {
            Dialog::Forget(a, id) => self.forget_confirm(&a, id),
            Dialog::ProfileDelete(a, id) => self.delete_profile(&a, id),
            Dialog::Bootloader(a) => {
                self.dialog = None;
                self.run(&a, Kind::Bootloader, Command::Bootloader);
            }
            Dialog::Replace(a, target) => self.confirm_replace(&a, target),
            _ => {}
        }
    }
}
