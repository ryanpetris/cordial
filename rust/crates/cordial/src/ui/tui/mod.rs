//! Terminal input, focus, scrolling, and rendering for the shared application.
mod adapter;
mod add;
mod device;
mod keys;
pub(crate) mod layout;
mod profiles;
mod render;
mod settings;
#[cfg(test)]
mod tests;
use crate::ui::{
    Msg,
    app::{self, App},
    field::Field,
};
pub(crate) use app::Live;
use app::*;
use app::{fleet, words, world};
use layout::Hit;
use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};
pub const MIN_WIDTH: usize = 60;
pub const MIN_HEIGHT: usize = 16;
/// Regions that scroll with the wheel and keep their own Tab order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Area {
    Sidebar,
    Main,
    Dialog,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub x: usize,
    pub y: usize,
    pub items: Vec<(String, Option<Action>)>,
}
pub(crate) struct Model<F: Fleet> {
    app: App<F>,
    form: Field,
    menu: Option<Menu>,
    pub(crate) width: usize,
    pub(crate) height: usize,
    side_scroll: usize,
    main_scroll: usize,
    dialog_scroll: usize,
    reveal: bool,
    pub(super) hits: Vec<Hit>,
    regions: Vec<(Area, (usize, usize, usize, usize))>,
    body_area: (usize, usize, usize, usize),
    focus: Option<Action>,
    focus_ctx: String,
    hover: bool,
    px: usize,
    py: usize,
    armed: Option<Action>,
    enter_page: bool,
    form_focused: bool,
}
impl<F: Fleet> Deref for Model<F> {
    type Target = App<F>;
    fn deref(&self) -> &Self::Target {
        &self.app
    }
}
impl<F: Fleet> DerefMut for Model<F> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.app
    }
}
impl<F: Fleet> Model<F> {
    pub(crate) fn new(fleet: F) -> Self {
        Self {
            app: App::new(fleet),
            form: Field::new(64),
            menu: None,
            width: 100,
            height: 30,
            side_scroll: 0,
            main_scroll: 0,
            dialog_scroll: 0,
            reveal: false,
            hits: Vec::new(),
            regions: Vec::new(),
            body_area: (0, 0, 0, 0),
            focus: None,
            focus_ctx: String::new(),
            hover: false,
            px: 0,
            py: 0,
            armed: None,
            enter_page: false,
            form_focused: false,
        }
    }
}
/// How long to wait for the next message before a tick.
pub fn tick_interval(animating: bool) -> Duration {
    if animating {
        Duration::from_millis(120)
    } else {
        Duration::from_millis(500)
    }
}

/// A spinner frame for the current time.
pub(super) fn spinner() -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    FRAMES[(ms / 100 % 10) as usize]
}

impl<F: Fleet> Model<F> {
    fn send_form(&mut self) {
        self.app.form.set_value(&self.form.value());
        self.app.menu = self.menu.as_ref().map(|m| app::Menu {
            items: m.items.clone(),
        });
    }

    fn receive_form(&mut self, reset_cursor: bool) {
        self.form.limit = self.app.form.limit;
        if reset_cursor || self.form.value() != self.app.form.value() {
            self.form.set_value(self.app.form.as_str());
        }
        match &self.app.menu {
            None => self.menu = None,
            Some(menu) => {
                let (x, y) = self.menu.as_ref().map_or((0, 0), |m| (m.x, m.y));
                self.menu = Some(Menu {
                    x,
                    y,
                    items: menu.items.clone(),
                });
            }
        }
    }

    fn follow_view(
        &mut self,
        before: (
            Page,
            Tab,
            Option<Dialog>,
            String,
            Option<crate::settings::Field>,
        ),
    ) {
        let dialog_changed = match (&before.2, &self.dialog) {
            (
                Some(Dialog::Pick {
                    adapter: a,
                    purpose: p,
                    ..
                }),
                Some(Dialog::Pick {
                    adapter: b,
                    purpose: q,
                    ..
                }),
            ) => (a, p) != (b, q),
            (a, b) => a != b,
        };
        if before.0 != self.page {
            self.form_focused = false;
            self.reveal = true;
        }
        if (before.0, before.1) != (self.page.clone(), self.tab) {
            self.main_scroll = 0;
        }
        if dialog_changed {
            self.dialog_scroll = 0;
        }
        if dialog_changed || before.3 != self.prompt_key() {
            self.form_focused = matches!(
                self.dialog,
                Some(Dialog::Rename(_) | Dialog::ProfileName(..))
            ) || self.pairing_code_input();
        }
        self.receive_form(
            dialog_changed || before.3 != self.prompt_key() || before.4 != self.editing,
        );
    }

    fn view_context(
        &self,
    ) -> (
        Page,
        Tab,
        Option<Dialog>,
        String,
        Option<crate::settings::Field>,
    ) {
        (
            self.page.clone(),
            self.tab,
            self.dialog.clone(),
            self.prompt_key(),
            self.editing,
        )
    }

    fn prompt_key(&self) -> String {
        self.add
            .as_ref()
            .and_then(|a| a.pairing.as_ref())
            .map(|p| p.prompt_key.clone())
            .unwrap_or_default()
    }

    pub(super) fn action(&mut self, action: Action) {
        if self.quitting() {
            return;
        }
        let before = self.view_context();
        let menu_before = self.menu.clone();
        let at = self
            .hits
            .iter()
            .find(|h| h.action == action)
            .map(|h| (h.x, h.y + 1));
        if matches!(
            action,
            Action::Open(_) | Action::Show(_) | Action::RenameAdapter(_)
        ) {
            self.reveal = true;
        }
        if matches!(action, Action::Tab(_)) {
            self.main_scroll = 0;
        }
        if action == Action::Field {
            self.form_focused = true;
            self.focus = None;
        }
        self.send_form();
        self.app.action(action);
        self.follow_view(before);
        if let (Some(menu), Some((x, y))) = (&mut self.menu, at)
            && menu_before.as_ref().is_none_or(|m| m.items != menu.items)
        {
            menu.x = x;
            menu.y = y;
        }
    }

    pub(super) fn open(&mut self, page: Page) {
        let before = self.view_context();
        self.app.open(page);
        self.reveal = true;
        self.follow_view(before);
    }

    pub(super) fn commit_editing(&mut self) {
        self.send_form();
        self.app.commit_editing();
        self.receive_form(false);
    }

    pub(crate) fn tick(&mut self) {
        let before = self.view_context();
        self.send_form();
        self.app.tick();
        self.follow_view(before);
    }

    pub(crate) fn update(&mut self, msg: Msg) {
        use ratatui::crossterm::event::MouseEventKind;
        if let Msg::Mouse(m) = &msg
            && matches!(m.kind, MouseEventKind::Moved | MouseEventKind::Drag(_))
        {
            self.mouse(*m);
            return;
        }
        let before = self.view_context();
        self.send_form();
        self.app.refresh();
        match msg {
            Msg::Interrupt => self.app.quit(),
            Msg::Resize(w, h) => {
                self.width = usize::from(w).max(1);
                self.height = usize::from(h).max(1);
                self.armed = None;
                self.menu = None;
                self.app.menu = None;
                self.unhover();
            }
            Msg::Ports(result) => self.app.update(app::Msg::Ports(result)),
            Msg::Migration(e) => self.app.update(app::Msg::Migration(e)),
            Msg::Migrated(result) => self.app.update(app::Msg::Migrated(result)),
            Msg::Fleet(slot, e) => self.app.update(app::Msg::Fleet(slot, e)),
            Msg::Controller(_) => {}
            Msg::Mouse(m) => {
                self.mouse(m);
                self.send_form();
            }
            Msg::Key(k) => {
                self.key(k);
                self.send_form();
            }
            Msg::Paste(text) => {
                self.paste(&text);
                self.send_form();
            }
        }
        self.app.refresh();
        self.app.sync();
        self.follow_view(before);
    }

    fn paste(&mut self, text: &str) {
        if self.form_focused || self.editing.is_some() {
            if matches!(self.dialog, Some(Dialog::AddDevice)) {
                self.add_form_changed();
            }
            self.form.insert(text);
        }
    }
}
