//! The running app: the shared model, the window, background services, and the main loop
//! tasks that feed them messages and redraw.
//!
//! Settings remain editable while writes run. Explicit Quit drains accepted edits before it
//! closes sessions; closing the window only hides it.
use super::{
    search::Target,
    services::{Emit, ServiceEvent, Services},
    window::Window,
};
use crate::{
    controller::{Event, Ticket},
    ui::app::{Action, App, Kind, Live, Msg, Page, Sink, Slot, Spot, Tab},
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::Write,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

/// How many backend messages one redraw covers at most, so a burst can't stall the window.
const BATCH: usize = 256;
/// Controller threads wait while this many messages are unhandled.
const BACKLOG: usize = 256;
const TICK: Duration = Duration::from_millis(500);

/// A request from a widget or a background service.
pub enum Ev {
    Act(Action),
    /// Acts on a page other than the shown one, as a tray menu does.
    ActOn(Page, Action),
    /// The open dialog's text.
    Form(String),
    /// The window closed; the app keeps running.
    Hide,
    Present(Option<Page>),
    /// Shows the window with Settings in the page's place.
    Preferences,
    /// Goes to a search result and shows its row.
    Find(Target),
    Quit,
    CancelQuit,
    QuitAnyway,
    About,
    Redraw,
}

/// Explicit Quit waits for accepted writes and keeps failures visible until acknowledged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum QuitState {
    #[default]
    Idle,
    Pending,
    Failed(String),
}

/// What widget signal handlers share: the request channel and the redraw guard.
pub struct Ctx {
    tx: async_channel::Sender<Ev>,
    /// Set while a redraw shows the model, so handlers ignore the changes it makes.
    updating: Cell<bool>,
}

impl Ctx {
    pub fn send(&self, ev: Ev) {
        if !self.updating.get() {
            let _ = self.tx.try_send(ev);
        }
    }

    pub fn act(&self, action: Action) {
        self.send(Ev::Act(action));
    }

    /// Applies an action after the redraw in progress, for a redraw that leaves the model out of
    /// step with the screen.
    pub fn act_after(&self, action: Action) {
        let _ = self.tx.try_send(Ev::Act(action));
    }

    /// A context for widgets under test, with the requests they send.
    #[cfg(test)]
    pub fn test() -> (Rc<Self>, async_channel::Receiver<Ev>) {
        let (tx, rx) = async_channel::unbounded();
        let ctx = Self {
            tx,
            updating: Cell::new(false),
        };
        (Rc::new(ctx), rx)
    }
}

/// The running app, as its activation reaches it.
pub struct Handle {
    ctx: Rc<Ctx>,
}

impl Handle {
    pub fn present(&self, page: Option<Page>) {
        self.ctx.send(Ev::Present(page));
    }
}

struct Shell {
    app: App<Live>,
    gtk: adw::Application,
    ctx: Rc<Ctx>,
    requests: async_channel::Receiver<Ev>,
    window: Option<Window>,
    services: Services,
    add_device: gio::SimpleAction,
    /// Connects and disconnects a background service started, with the spot of their result.
    tracked: HashMap<(Slot, Ticket), Spot>,
    quit: QuitState,
    /// A simulation fixture observes completed application jobs without changing UI feedback.
    write_trace: Option<std::fs::File>,
    finished: bool,
}

/// Starts the model and its tasks on the main loop.
pub fn start(
    gtk: &adw::Application,
    port: Option<String>,
    simulate: Option<usize>,
    observer: Option<crate::json::Observer>,
) -> Handle {
    let (backend, messages) = async_channel::bounded::<Msg>(BACKLOG);
    let sink: Sink = Arc::new(move |m| {
        // The window has gone once the receiver closes; nothing waits for the message.
        let _ = backend.send_blocking(m);
    });
    let live = match simulate {
        Some(count) => crate::sim::desktop(sink, count),
        None => Live::new(sink, port),
    };
    let live = match observer {
        Some(observer) => live.observe(observer),
        None => live,
    };
    let mut app = App::new(live);
    app.enable_immediate();
    app.keep_deleted();
    let write_trace = simulate
        .and_then(|_| std::env::var_os("CORDIAL_SIM_WRITE_TRACE"))
        .and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        });
    let (tx, requests) = async_channel::unbounded::<Ev>();
    let ctx = Rc::new(Ctx {
        tx: tx.clone(),
        updating: Cell::new(false),
    });
    let emit: Emit = Arc::new(move |e| {
        let _ = tx.try_send(service(e));
    });
    let add_device = app_actions(gtk, &ctx);
    let shell = Rc::new(RefCell::new(Shell {
        app,
        gtk: gtk.clone(),
        ctx: ctx.clone(),
        requests: requests.clone(),
        window: None,
        services: Services::new(gtk, emit),
        add_device,
        tracked: HashMap::new(),
        quit: QuitState::Idle,
        write_trace,
        finished: false,
    }));
    shell.borrow_mut().redraw();
    glib::spawn_future_local(glib::clone!(
        #[strong]
        shell,
        async move {
            while let Ok(m) = messages.recv().await {
                let mut s = shell.borrow_mut();
                s.message(m);
                for _ in 1..BATCH {
                    let Ok(m) = messages.try_recv() else { break };
                    s.message(m);
                }
                s.redraw();
            }
        }
    ));
    glib::spawn_future_local(glib::clone!(
        #[strong]
        shell,
        async move {
            while let Ok(ev) = requests.recv().await {
                let mut s = shell.borrow_mut();
                s.handle(ev);
                while let Ok(ev) = requests.try_recv() {
                    s.handle(ev);
                }
                s.redraw();
            }
        }
    ));
    glib::timeout_add_local(TICK, move || {
        let mut s = shell.borrow_mut();
        s.app.tick();
        s.redraw();
        if s.finished {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
    Handle { ctx }
}

/// The request a background service makes.
fn service(e: ServiceEvent) -> Ev {
    match e {
        ServiceEvent::Present(page) => Ev::Present(page),
        ServiceEvent::Preferences => Ev::Preferences,
        ServiceEvent::Quit => Ev::Quit,
        ServiceEvent::Act(page, action) => Ev::ActOn(page, action),
        ServiceEvent::Changed => Ev::Redraw,
    }
}

/// Adds the application's actions, returning Add Device, which is available only while the
/// shown page's adapter can add a device.
fn app_actions(gtk: &adw::Application, ctx: &Rc<Ctx>) -> gio::SimpleAction {
    let request = |name: &str, ev: fn() -> Ev| {
        let action = gio::SimpleAction::new(name, None);
        let ctx = Rc::downgrade(ctx);
        action.connect_activate(move |_, _| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.send(ev());
            }
        });
        gtk.add_action(&action);
        action
    };
    request("quit", || Ev::Quit);
    request("about", || Ev::About);
    request("preferences", || Ev::Preferences);
    request("show", || Ev::Present(None));
    request("refresh", || Ev::Act(Action::RefreshAdapters));
    let add = request("add-device", || Ev::Act(Action::AddDevice));
    add.set_enabled(false);
    add
}

impl Shell {
    /// Applies a backend message, reporting a tracked action's failure.
    fn message(&mut self, m: Msg) {
        let writes: Vec<_> = self
            .app
            .jobs
            .iter()
            .filter_map(|(key, job)| trace_target(job).map(|target| (*key, target)))
            .collect();
        let outcome = match &m {
            Msg::Fleet(slot, event) => match event.as_ref() {
                Event::Done { ticket, result, .. } => Some((
                    (*slot, *ticket),
                    match result {
                        Ok(_) => "ok",
                        Err(error) if crate::ui::app::write_outcome_unknown(error) => "unknown",
                        Err(_) => "error",
                    },
                )),
                _ => None,
            },
            _ => None,
        };
        let finished = match &m {
            Msg::Fleet(slot, e) => match e.as_ref() {
                Event::Done { ticket, .. } => self.tracked.remove(&(*slot, *ticket)),
                Event::Closed => {
                    self.tracked.retain(|(s, _), _| s != slot);
                    None
                }
                _ => None,
            },
            _ => None,
        };
        self.app.update(m);
        if let Some(trace) = &mut self.write_trace {
            for (key, target) in writes {
                if !self.app.jobs.contains_key(&key) {
                    let result = outcome
                        .filter(|(done, _)| *done == key)
                        .map_or("unknown", |(_, result)| result);
                    let _ = writeln!(trace, "{target} {result}");
                }
            }
        }
        if let Some(spot) = finished {
            self.report(&spot);
        }
    }

    /// Notifies why a background action failed, unless its device's Details page is on screen
    /// to show it.
    fn report(&mut self, spot: &Spot) {
        let Spot::DeviceBar(adapter, id) = spot else {
            return;
        };
        let page = Page::Device(adapter.clone(), *id);
        let seen = self
            .window
            .as_ref()
            .is_some_and(|w| w.window.is_visible() && !w.settings_shown())
            && self.app.shown() == page
            && self.app.tab() == Tab::Details
            && self
                .app
                .note(&Spot::DetailsSave(adapter.clone(), *id))
                .is_none();
        if let (false, Some(text)) = (seen, self.app.note(spot)) {
            let text = text.to_owned();
            self.services.action_failed(&text, Some(&page));
        }
    }

    fn handle(&mut self, ev: Ev) {
        if self.app.quitting() && !matches!(ev, Ev::Redraw) {
            return;
        }
        // Going to a page, even the one under Settings, leaves Settings.
        let opens = matches!(
            ev,
            Ev::Act(Action::Open(_) | Action::Show(_))
                | Ev::Present(Some(_))
                | Ev::Find(Target { page: Some(_), .. })
        );
        let page = self.app.page.clone();
        self.dispatch(ev);
        if (opens || self.app.page != page)
            && let Some(w) = &mut self.window
        {
            w.leave_settings();
        }
    }

    fn dispatch(&mut self, ev: Ev) {
        match ev {
            Ev::Act(Action::Quit) => self.handle(Ev::Quit),
            // The shortcut adds a device with the shown page's adapter. Without one,
            // the window comes forward so the user can choose an adapter.
            Ev::Act(Action::AddDevice) | Ev::ActOn(_, Action::AddDevice) => {
                self.present();
                if let Some(adapter) = self.add_target() {
                    self.app.act(Action::AddDeviceTo(adapter));
                }
            }
            Ev::Act(action) => self.app.act(action),
            Ev::ActOn(page, action) => self.act_on(page, action),
            Ev::Form(text) => self.app.set_form(&text),
            Ev::Hide => self.app.hide(),
            Ev::Present(page) => {
                if let Some(page) = page {
                    self.app.act(Action::Show(page));
                }
                self.present();
            }
            Ev::Preferences => {
                self.present();
                if let Some(w) = &mut self.window {
                    let services = &self.services;
                    w.show_settings(|| services.settings_page());
                }
            }
            Ev::Find(target) => {
                match target.page {
                    Some(page) => {
                        self.app.act(Action::Open(page));
                        self.app.act(Action::Tab(target.tab));
                    }
                    None => self.dispatch(Ev::Preferences),
                }
                if let Some(w) = &mut self.window {
                    w.find(target.row);
                }
            }
            Ev::About => {
                self.present();
                if let Some(w) = &self.window {
                    super::about(&w.window);
                }
            }
            Ev::Quit => {
                if matches!(self.quit, QuitState::Idle) {
                    if let Some(w) = &self.window {
                        w.commit_editing();
                    }
                    self.app.commit_editing();
                    self.quit = QuitState::Pending;
                }
            }
            Ev::CancelQuit => {
                self.quit = QuitState::Idle;
                self.app.acknowledge_write_error();
                self.present();
            }
            Ev::QuitAnyway => {
                if matches!(self.quit, QuitState::Failed(_)) {
                    self.app.acknowledge_write_error();
                    self.quit = QuitState::Pending;
                }
            }
            Ev::Redraw => {}
        }
    }

    /// Acts on another page without leaving the shown one, so its tab and results stay. A page
    /// that has gone since the tray menu was built has nothing to act on.
    fn act_on(&mut self, page: Page, action: Action) {
        if !self.app.exists(&page) {
            return;
        }
        let before: Vec<(Slot, Ticket)> = self.app.jobs.keys().copied().collect();
        // The device's bar reports this action alone, not an earlier one's result.
        if let Page::Device(adapter, id) = &page {
            self.app
                .notes
                .remove(&Spot::DeviceBar(adapter.clone(), *id));
        }
        act_at(&mut self.app, page.clone(), action);
        // A connect or disconnect reports at its device's bar, at once when it can't start.
        let mut started = false;
        for (key, job) in &self.app.jobs {
            if let Kind::Device(_, spot @ Spot::DeviceBar(..)) = &job.kind
                && !before.contains(key)
            {
                self.tracked.insert(*key, spot.clone());
                started = true;
            }
        }
        if !started && let Page::Device(adapter, id) = page {
            self.report(&Spot::DeviceBar(adapter, id));
        }
    }

    /// Shows the window, building it on first use; the redraw that follows each request fills
    /// it in before it is drawn.
    fn present(&mut self) {
        let w = self
            .window
            .get_or_insert_with(|| Window::new(&self.gtk, &self.ctx));
        w.window.present();
    }

    fn redraw(&mut self) {
        // Requests that widgets queued apply first, so the redraw doesn't undo text being typed.
        while let Ok(ev) = self.requests.try_recv() {
            self.handle(ev);
        }
        self.app.apply_pending();
        if matches!(self.quit, QuitState::Pending) {
            if let Some(error) = self.app.write_error() {
                self.quit = QuitState::Failed(error.to_owned());
            } else if !self.app.writes_pending() {
                self.app.quit();
            }
        }
        if !matches!(self.quit, QuitState::Idle) && !self.app.quitting() {
            self.window
                .get_or_insert_with(|| Window::new(&self.gtk, &self.ctx));
        }
        if let Some(w) = &self.window {
            w.set_quit_state(&self.quit);
        }
        // A hidden window catches up when it is shown again.
        if let Some(w) = self.window.as_mut().filter(|w| w.window.is_visible()) {
            self.ctx.updating.set(true);
            w.update(&self.ctx, &self.app);
            self.ctx.updating.set(false);
        }
        self.add_device
            .set_enabled(!self.app.quitting() && self.add_target().is_some());
        if self.finished {
            return;
        }
        self.services.update(&self.app);
        if self.app.done() {
            self.finished = true;
            self.services.shutdown();
            self.gtk.quit();
        }
    }
    /// The visible page's adapter, when Add Device can open with it.
    fn add_target(&self) -> Option<String> {
        if self.window.as_ref().is_some_and(Window::settings_shown) {
            return None;
        }
        match self.app.shown() {
            Page::Adapter(a) | Page::Device(a, _) if self.app.can_add_to(&a) => Some(a),
            _ => None,
        }
    }
}

/// Applies an action to a page, which needn't be the one shown; the shown page keeps its tab.
fn act_at(app: &mut App<Live>, page: Page, action: Action) {
    if app.shown() == page {
        return app.act(action);
    }
    app.commit_editing();
    let shown = (app.page.clone(), app.tab, app.page_shown);
    app.page = page.clone();
    app.act(action);
    if app.page == page {
        (app.page, app.tab, app.page_shown) = shown;
        app.tick();
    }
}

/// A stable, simulation-only completion record. The controller has handled its write's
/// confirmation before Done; a session loss records an unknown result exactly once.
fn trace_target(job: &crate::ui::app::Job) -> Option<String> {
    Some(match &job.kind {
        Kind::AdapterSave(_) => format!("adapter {}", job.adapter),
        Kind::DeviceSave { device, tab, .. } => format!(
            "device {} {device} {}",
            job.adapter,
            match tab {
                Tab::Profiles => "profiles",
                Tab::Settings => "settings",
                _ => "details",
            }
        ),
        Kind::SettingsSave { device, .. } => format!("settings {} {device}", job.adapter),
        _ => return None,
    })
}
