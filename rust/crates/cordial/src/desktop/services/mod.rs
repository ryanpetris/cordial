//! Host services of the desktop app: saved preferences and the Settings page, Start at Login,
//! notifications and the tray icon.
//!
//! Services never touch the app model from a callback. Notification clicks, tray menu items and
//! changes on the Settings page call [`Emit`] with a [`ServiceEvent`] for the app to handle.
mod alerts;
mod prefs;
mod tray;

pub use prefs::TITLES as SETTINGS;

use crate::ui::app::{Action, App, Live, Page};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{collections::HashSet, rc::Rc, sync::Arc};

/// What a service asks of the app.
#[derive(Clone, Debug, PartialEq)]
pub enum ServiceEvent {
    /// Shows the window, at the page when given.
    Present(Option<Page>),
    /// Shows the Settings page from [`Services::settings_page`].
    Preferences,
    Quit,
    /// Performs the action on the page, leaving the shown page and tab as they were. Actions
    /// that open a dialog follow a `Present`.
    Act(Page, Action),
    /// The preferences or the panel's style changed; pass the model to [`Services::update`].
    Changed,
}

/// Delivers events to the app. Tray events arrive on another thread.
pub type Emit = Arc<dyn Fn(ServiceEvent) + Send + Sync>;

/// The app action that notifications activate, with the page they open.
const SHOW_PAGE: &str = "show-page";

/// A page as the notification action's target. None shows the window as it was.
fn target(page: Option<&Page>) -> glib::Variant {
    match page {
        None => Vec::<String>::new(),
        Some(Page::Overview) => vec!["overview".into()],
        Some(Page::Adapter(id)) => vec!["adapter".into(), id.clone()],
        Some(Page::Device(a, id)) => vec!["device".into(), a.clone(), id.to_string()],
    }
    .to_variant()
}

fn page(target: &glib::Variant) -> Option<Page> {
    let parts: Vec<String> = target.get()?;
    match parts.as_slice() {
        [kind] if kind == "overview" => Some(Page::Overview),
        [kind, id] if kind == "adapter" => Some(Page::Adapter(id.clone())),
        [kind, a, id] if kind == "device" => Some(Page::Device(a.clone(), id.parse().ok()?)),
        _ => None,
    }
}

pub struct Services {
    app: adw::Application,
    emit: Emit,
    store: Rc<prefs::Store>,
    alerts: alerts::Alerts,
    /// Notifications sent, to withdraw at shutdown.
    sent: HashSet<String>,
    tray: tray::Tray,
    style: Option<glib::SignalHandlerId>,
    preferences_changed: Option<glib::SignalHandlerId>,
    stopped: bool,
}

impl Services {
    pub fn new(app: &adw::Application, emit: Emit) -> Self {
        let store = prefs::Store::open(glib::user_config_dir());
        prefs::sync_autostart(&store);
        let preferences_changed = store.connect_changed(emit.clone());

        let show = gio::SimpleAction::new(SHOW_PAGE, Some(glib::VariantTy::STRING_ARRAY));
        show.connect_activate({
            let emit = emit.clone();
            move |_, target| emit(ServiceEvent::Present(target.and_then(page)))
        });
        app.add_action(&show);

        let style = adw::StyleManager::default().connect_dark_notify({
            let emit = emit.clone();
            move |_| emit(ServiceEvent::Changed)
        });
        Self {
            app: app.clone(),
            tray: tray::Tray::new(emit.clone()),
            emit,
            store: Rc::new(store),
            alerts: alerts::Alerts::default(),
            sent: HashSet::new(),
            style: Some(style),
            preferences_changed: Some(preferences_changed),
            stopped: false,
        }
    }

    /// Notifies of new alerts and updates the tray for the model's current view.
    pub fn update(&mut self, model: &App<Live>) {
        if self.stopped {
            return;
        }
        let p = self.store.get();
        for note in self.alerts.update(model.adapters(), model.devices(), &p) {
            self.notify(
                note.id,
                &note.title,
                note.body.as_deref(),
                note.page.as_ref(),
            );
        }
        let model = tray::model(model.adapters(), model.devices(), &p);
        self.tray.show(model, tray::variant());
    }

    /// Notifies that an action failed, with `text` as the body. Activating the notification
    /// shows `page`, or the window as it was without one. A later failure replaces it.
    pub fn action_failed(&mut self, text: &str, page: Option<&Page>) {
        if !self.stopped {
            self.notify("action".into(), "Cordial", Some(text), page);
        }
    }

    /// Sends a notification that replaces any earlier one with the same `id`.
    fn notify(&mut self, id: String, title: &str, body: Option<&str>, page: Option<&Page>) {
        let n = gio::Notification::new(title);
        n.set_body(body);
        n.set_default_action_and_target_value(&format!("app.{SHOW_PAGE}"), Some(&target(page)));
        self.app.send_notification(Some(&id), &n);
        self.sent.insert(id);
    }

    /// The Settings page's content, which follows changes made elsewhere while it lives.
    pub fn settings_page(&self) -> gtk::Widget {
        prefs::page(&self.store, &self.emit).upcast()
    }

    /// Removes the tray icon, the notifications and the notification action.
    pub fn shutdown(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        if let Some(id) = self.preferences_changed.take() {
            self.store.disconnect(id);
        }
        self.tray.stop();
        for id in self.sent.drain() {
            self.app.withdraw_notification(&id);
        }
        self.app.remove_action(SHOW_PAGE);
        if let Some(style) = self.style.take() {
            adw::StyleManager::default().disconnect(style);
        }
    }
}

impl Drop for Services {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_targets_name_their_page() {
        for p in [
            Page::Overview,
            Page::Adapter("A:1".into()),
            Page::Device("A:1".into(), 7),
        ] {
            assert_eq!(page(&target(Some(&p))), Some(p));
        }
        assert_eq!(page(&target(None)), None);
        assert_eq!(page(&vec!["device", "A", "x"].to_variant()), None);
        assert_eq!(page(&"adapter".to_variant()), None);
    }
}
