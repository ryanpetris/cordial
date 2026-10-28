//! The main window: a sidebar of pages beside the selected page, collapsing to one pane when
//! narrow. A page's Profiles and Diagnostics open over it as subpages with a back button.
//! Settings shows in the page's place until the user goes to a page.
use super::search::Row;
use super::{
    adapter::AdapterPage,
    device::DevicePage,
    dialogs::{self, Shown},
    overview::Overview,
    shell::{Ctx, Ev, QuitState},
    sidebar::Sidebar,
    widgets::{Btn, para, set_label, spin, when},
};
use crate::ui::app::{Action, App, Live, Page, Tab};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// What the header shows for a page.
pub struct Head {
    pub title: String,
    pub subtitle: String,
}

/// A page shown in the content pane.
pub trait View {
    fn tabs(&self) -> &Tabs;
    /// Shows the model; None once the page's target is gone.
    fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>) -> Option<Head>;

    fn widget(&self) -> gtk::Widget {
        self.tabs().nav.clone().upcast()
    }
}

/// Tabs that open over the page, with the tags their navigation pages carry.
const SUBPAGES: [(Tab, &str); 2] = [
    (Tab::Profiles, "profiles"),
    (Tab::Diagnostics, "diagnostics"),
];

fn named(name: &str) -> Option<Tab> {
    SUBPAGES.iter().find(|(_, n)| *n == name).map(|(t, _)| *t)
}

/// A page's tabs: Details as the page itself, and the rest as subpages pushed over it. The
/// model's tab follows the one on screen.
pub struct Tabs {
    ctx: Rc<Ctx>,
    nav: adw::NavigationView,
    root: adw::NavigationPage,
    view: adw::ToolbarView,
    header: adw::HeaderBar,
    title: adw::WindowTitle,
    subpages: Vec<(Tab, adw::NavigationPage, adw::WindowTitle, adw::HeaderBar)>,
    /// Add Device at the start of the Details header.
    add: Btn,
    /// Set once the model's tab has been shown, after which changes animate.
    placed: Cell<bool>,
}

impl Tabs {
    pub fn new(ctx: &Rc<Ctx>) -> Self {
        let title = adw::WindowTitle::new("", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let add = add_button(ctx);
        header.pack_start(&add.widget);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        let root = adw::NavigationPage::new(&view, "");
        let nav = adw::NavigationView::new();
        nav.add(&root);
        nav.connect_visible_page_notify(glib::clone!(
            #[weak]
            ctx,
            move |nav| {
                let tab = nav
                    .visible_page()
                    .and_then(|page| page.tag())
                    .as_deref()
                    .and_then(named)
                    .unwrap_or(Tab::Details);
                ctx.act(Action::Tab(tab));
            }
        ));
        Self {
            ctx: ctx.clone(),
            nav,
            root,
            view,
            header,
            title,
            subpages: Vec::new(),
            add,
            placed: Cell::new(false),
        }
    }

    /// Shows Details as the page, or adds a subpage for another tab.
    pub fn add(&mut self, tab: Tab, widget: &impl IsA<gtk::Widget>) {
        let Some((_, name)) = SUBPAGES.iter().find(|(t, _)| *t == tab) else {
            self.view.set_content(Some(widget));
            return;
        };
        let title = adw::WindowTitle::new(tab.label(), "");
        let view = adw::ToolbarView::builder().content(widget).build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        view.add_top_bar(&header);
        let page = adw::NavigationPage::builder()
            .title(tab.label())
            .tag(*name)
            .child(&view)
            .build();
        self.nav.add(&page);
        self.subpages.push((tab, page, title, header));
    }

    /// Puts a control at the end of a tab's header, after the tab is added.
    pub fn pack_end(&self, tab: Tab, widget: &impl IsA<gtk::Widget>) {
        let header = self
            .subpages
            .iter()
            .find(|(t, ..)| *t == tab)
            .map_or(&self.header, |(.., h)| h);
        header.pack_end(widget);
    }

    /// Adds a bar under the page's header.
    pub fn add_top_bar(&self, widget: &impl IsA<gtk::Widget>) {
        self.view.add_top_bar(widget);
    }

    /// Offers these tabs and shows `tab`, or Details when it isn't offered. Details then becomes
    /// the model's tab too, so a tab offered again later doesn't reopen over what the user is
    /// doing.
    pub fn show(&self, offered: &[Tab], tab: Tab) {
        let shown = if offered.contains(&tab) {
            tab
        } else {
            self.ctx.act_after(Action::Tab(Tab::Details));
            Tab::Details
        };
        let visible = self.nav.visible_page();
        match self.subpages.iter().find(|(t, ..)| *t == shown) {
            Some((_, page, ..)) if visible.as_ref() != Some(page) => {
                if self.placed.get() && visible.as_ref() == Some(&self.root) {
                    self.nav.push(page);
                } else {
                    self.nav.replace(&[self.root.clone(), page.clone()]);
                }
            }
            Some(_) => {}
            None => {
                if visible.as_ref() != Some(&self.root) {
                    self.nav.pop_to_page(&self.root);
                }
            }
        }
        self.placed.set(true);
    }

    /// Offers Add Device with the page's adapter while it is ready, or hides it on a page
    /// without one.
    fn offer_add(&self, app: &App<Live>, adapter: Option<&str>) {
        let action = adapter.map(|a| when(app.can_add_to(a), Action::AddDeviceTo(a.to_owned())));
        self.add.offer(action.is_some(), action.flatten());
    }

    /// Names the page in its header and in its subpages' headers.
    fn head(&self, head: &Head) {
        let title = &self.title;
        if title.title() != head.title {
            title.set_title(&head.title);
            self.root.set_title(&head.title);
            for (_, _, t, _) in &self.subpages {
                t.set_subtitle(&head.title);
            }
        }
        if title.subtitle() != head.subtitle {
            title.set_subtitle(&head.subtitle);
        }
    }
}

fn add_button(ctx: &Rc<Ctx>) -> Btn {
    let b = Btn::icon(ctx, "list-add-symbolic", "Add Device");
    b.hide();
    b
}

pub struct Window {
    pub window: adw::ApplicationWindow,
    ctx: Rc<Ctx>,
    split: adw::NavigationSplitView,
    sidebar: Sidebar,
    content: adw::NavigationPage,
    holder: adw::Bin,
    toasts: adw::ToastOverlay,
    toast: Option<String>,
    page: Option<(Page, Box<dyn View>)>,
    /// The Settings page, once built, and whether it is shown in the page's place.
    settings: Option<gtk::Widget>,
    settings_shown: bool,
    dialog: Option<Shown>,
    quit: RefCell<Option<Quit>>,
    /// The row a search result goes to, shown at the next redraw.
    found: Option<Row>,
    revealing: Revealing,
}

impl Window {
    pub fn new(app: &adw::Application, ctx: &Rc<Ctx>) -> Self {
        let split = adw::NavigationSplitView::new();
        let sidebar = Sidebar::new(ctx, &split);
        split.set_sidebar(Some(&sidebar.page));

        let holder = adw::Bin::builder().vexpand(true).build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&holder));
        let content = adw::NavigationPage::builder()
            .title("Overview")
            .child(&toasts)
            .build();
        split.set_content(Some(&content));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Cordial")
            .default_width(980)
            .default_height(680)
            .height_request(294)
            .hide_on_close(true)
            .content(&split)
            .build();
        // Fits a page's header and rows, which grow with the text scale, so the minimum follows
        // it like the breakpoint does.
        let fit = |window: &adw::ApplicationWindow, settings: &gtk::Settings| {
            let width = adw::LengthUnit::Sp.to_px(480.0, Some(settings));
            window.set_width_request(width.ceil() as i32);
        };
        let settings = window.settings();
        fit(&window, &settings);
        settings.connect_gtk_xft_dpi_notify(glib::clone!(
            #[weak]
            window,
            move |settings| fit(&window, settings)
        ));
        window.connect_close_request(glib::clone!(
            #[weak]
            ctx,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                ctx.send(Ev::Hide);
                glib::Propagation::Proceed
            }
        ));
        // The sidebar takes at least 180sp, so the panes collapse until the page beside it gets
        // as much width as the narrowest window gives a page alone.
        let breakpoint = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 660sp").expect("the condition is valid"),
        );
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        let search = gio::SimpleAction::new("search", None);
        search.connect_activate(glib::clone!(
            #[weak(rename_to = bar)]
            sidebar.search,
            #[weak(rename_to = entry)]
            sidebar.entry,
            move |_, _| {
                bar.set_search_mode(true);
                entry.grab_focus();
            }
        ));
        window.add_action(&search);
        // A search result waiting to be laid out gives way to the user's next key press, click,
        // scroll, or touch. Focus changes can't show that, since GTK and the split view move focus
        // themselves while the result's page comes on screen.
        let revealing = Revealing::default();
        let input = gtk::EventControllerLegacy::new();
        input.set_propagation_phase(gtk::PropagationPhase::Capture);
        input.connect_event(glib::clone!(
            #[strong]
            revealing,
            move |_, event| {
                use gtk::gdk::EventType::{ButtonPress, KeyPress, Scroll, TouchBegin};
                if matches!(
                    event.event_type(),
                    ButtonPress | KeyPress | Scroll | TouchBegin
                ) {
                    cancel(&revealing);
                }
                glib::Propagation::Proceed
            }
        ));
        window.add_controller(input);
        // Search waits while a dialog covers the window.
        window.connect_visible_dialog_notify(glib::clone!(
            #[weak]
            search,
            move |window| search.set_enabled(window.visible_dialog().is_none())
        ));

        Self {
            window,
            ctx: ctx.clone(),
            split,
            sidebar,
            content,
            holder,
            toasts,
            toast: None,
            page: None,
            settings: None,
            settings_shown: false,
            dialog: None,
            quit: RefCell::default(),
            found: None,
            revealing,
        }
    }

    /// Shows the content pane, and `row` once its page is on screen.
    pub fn find(&mut self, row: Option<Row>) {
        self.split.set_show_content(true);
        self.found = row;
    }

    /// Shows Settings in the page's place, building it with `content` the first time.
    pub fn show_settings(&mut self, content: impl FnOnce() -> gtk::Widget) {
        self.settings.get_or_insert_with(|| {
            let view = adw::ToolbarView::builder().content(&content()).build();
            view.add_top_bar(&adw::HeaderBar::new());
            view.upcast()
        });
        self.settings_shown = true;
        if self.split.is_collapsed() {
            self.split.set_show_content(true);
        }
    }

    /// Shows the model's page in place of Settings.
    pub fn leave_settings(&mut self) {
        self.settings_shown = false;
    }

    pub fn settings_shown(&self) -> bool {
        self.settings_shown
    }

    pub fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>) {
        self.sidebar.update(app, self.settings_shown);
        let shown = app.shown();
        let new = self.page.as_ref().map(|(p, _)| p) != Some(&shown);
        if new {
            let view: Box<dyn View> = match &shown {
                Page::Overview => Box::new(Overview::new(ctx)),
                Page::Adapter(id) => Box::new(AdapterPage::new(ctx, id)),
                Page::Device(a, id) => Box::new(DevicePage::new(ctx, a, *id)),
            };
            if self.page.is_some() && self.split.is_collapsed() {
                self.split.set_show_content(true);
            }
            self.page = Some((shown, view));
        }
        if let Some((page, view)) = &mut self.page {
            // A page whose target is gone keeps its names until the model leaves it.
            let head = view.update(ctx, app);
            let title = match &head {
                _ if self.settings_shown => Some("Settings"),
                Some(head) => Some(head.title.as_str()),
                None => None,
            };
            if let Some(title) = title
                && self.content.title() != title
            {
                self.content.set_title(title);
            }
            if let Some(head) = &head {
                view.tabs().head(head);
            }
            let adapter = match page {
                Page::Adapter(a) | Page::Device(a, _) => Some(a.as_str()),
                Page::Overview => None,
            };
            view.tabs().offer_add(app, adapter);
            // A new page goes on screen once it is named, since a navigation page needs its
            // title when it is mapped.
            let child = match &self.settings {
                Some(settings) if self.settings_shown => settings.clone(),
                _ => view.widget(),
            };
            if self.holder.child().as_ref() != Some(&child) {
                self.holder.set_child(Some(&child));
            }
            if let Some(row) = self.found.take() {
                // A page's tabs are sibling pages of its navigation view, and those off screen
                // stay shown, so only the visible one is searched.
                let tab = child
                    .downcast_ref::<adw::NavigationView>()
                    .and_then(|nav| nav.visible_page())
                    .map_or_else(|| child.clone(), |page| page.upcast());
                reveal(&tab, &row, &self.revealing);
            }
        }
        let toast = app.toast_text().map(str::to_owned);
        if toast != self.toast {
            if let Some(text) = &toast {
                self.toasts.add_toast(
                    adw::Toast::builder()
                        .title(glib::markup_escape_text(text))
                        .timeout(6)
                        .build(),
                );
            }
            self.toast = toast;
        }
        self.update_dialog(ctx, app);
    }

    /// Commits a number being typed into the focused row, as leaving the row does. Quit from
    /// the keyboard or the tray leaves focus where it is, so this runs before quitting.
    pub fn commit_editing(&self) {
        let row = GtkWindowExt::focus(&self.window)
            .and_then(|w| w.ancestor(adw::SpinRow::static_type()))
            .and_then(|w| w.downcast::<adw::SpinRow>().ok());
        if let Some(row) = row {
            row.update();
        }
    }

    /// Shows an explicit Quit waiting for settings to be applied, or why one couldn't be, in a
    /// window of its own, since Quit can come from the tray while the main window is hidden.
    pub fn set_quit_state(&self, state: &QuitState) {
        let mut quit = self.quit.borrow_mut();
        let failed = match state {
            QuitState::Idle => {
                if let Some(q) = quit.take() {
                    q.dialog.force_close();
                }
                return;
            }
            QuitState::Pending => None,
            QuitState::Failed(reason) => Some(reason.as_str()),
        };
        let new = quit.is_none();
        let q = quit.get_or_insert_with(|| Quit::new(&self.ctx));
        spin(&q.spinner, failed.is_none());
        set_label(&q.text, failed.unwrap_or("Settings are being applied."));
        q.buttons.set_visible(failed.is_some());
        if new {
            q.dialog.present(None::<&gtk::Widget>);
        }
    }

    /// Shows the model's dialog over the window, replacing one it no longer has.
    fn update_dialog(&mut self, ctx: &Rc<Ctx>, app: &App<Live>) {
        let key = dialogs::key(app);
        if self.dialog.as_ref().map(Shown::key) != key.as_ref() {
            if let Some(d) = self.dialog.take() {
                d.dismiss();
            }
            // A dialog opens once the window is on screen.
            if let Some(key) = key
                && self.window.is_visible()
            {
                let d = Shown::open(ctx, app, key);
                d.present(&self.window);
                self.dialog = Some(d);
            }
        }
        if let Some(d) = &mut self.dialog {
            d.update(ctx, app);
        }
    }
}

/// A search result waiting for a frame that lays it out before it is scrolled to and focused.
struct Pending {
    page: gtk::Widget,
    tick: gtk::TickCallbackId,
    unmap: glib::SignalHandlerId,
}

type Revealing = Rc<RefCell<Option<Pending>>>;

/// Drops the search result waiting to be shown, if any.
fn cancel(revealing: &Revealing) {
    if let Some(p) = revealing.take() {
        p.tick.remove();
        p.page.disconnect(p.unmap);
    }
}

/// Scrolls the first shown row matching `target` into view and focuses it, in place of a result
/// still waiting to be shown.
fn reveal(page: &gtk::Widget, target: &Row, revealing: &Revealing) {
    /// The first shown widget of type `T` under `w`, `w` included, for which `hit` holds.
    fn first<T: IsA<gtk::Widget>>(w: &gtk::Widget, hit: &dyn Fn(&T) -> bool) -> Option<T> {
        if !w.is_visible() {
            return None;
        }
        if let Some(t) = w.downcast_ref::<T>()
            && hit(t)
        {
            return Some(t.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(t) = first(&c, hit) {
                return Some(t);
            }
            child = c.next_sibling();
        }
        None
    }
    cancel(revealing);
    let row = match target {
        Row::Title(title) => first(page, &|r: &adw::PreferencesRow| r.title() == *title),
        Row::Name(name) => first(page, &|r: &adw::PreferencesRow| r.widget_name() == *name),
        Row::Group(title) => first(page, &|g: &adw::PreferencesGroup| g.title() == *title)
            .and_then(|g| first(g.upcast_ref(), &|_: &adw::PreferencesRow| true)),
        Row::In(group, title) => {
            first(page, &|g: &adw::PreferencesGroup| g.widget_name() == *group).and_then(|g| {
                first(g.upcast_ref(), &|r: &adw::PreferencesRow| {
                    r.title() == *title
                })
            })
        }
    };
    let Some(mut row) = row else {
        return;
    };
    // A row inside an expander row shows once the expander opens. One whose expander can't open
    // isn't shown, so its expander row is shown instead.
    if let Some(expander) = row
        .parent()
        .and_then(|p| p.ancestor(adw::ExpanderRow::static_type()))
        .and_downcast::<adw::ExpanderRow>()
    {
        if expander.enables_expansion() {
            expander.set_expanded(true);
        } else {
            row = expander.upcast();
        }
    }
    // A viewport scrolls to where a row is laid out at the time, so a row on a page just put on
    // screen, or in an expander still opening, waits for a frame that has laid it out in place.
    let tick = row.add_tick_callback(glib::clone!(
        #[weak]
        revealing,
        #[upgrade_or]
        glib::ControlFlow::Break,
        move |row, _| {
            let viewport = row
                .ancestor(gtk::Viewport::static_type())
                .and_then(|w| w.downcast::<gtk::Viewport>().ok());
            let revealer = row
                .ancestor(gtk::Revealer::static_type())
                .and_downcast::<gtk::Revealer>();
            let closed = revealer.as_ref().is_some_and(|r| !r.reveals_child());
            let opening = revealer.is_some_and(|r| !r.is_child_revealed());
            let unplaced =
                opening || row.height() == 0 || viewport.as_ref().is_some_and(|v| v.height() == 0);
            if !closed && unplaced {
                return glib::ControlFlow::Continue;
            }
            if let Some(p) = revealing.take() {
                p.page.disconnect(p.unmap);
            }
            if !closed {
                if let Some(viewport) = viewport {
                    viewport.scroll_to(row, None);
                }
                row.grab_focus();
            }
            glib::ControlFlow::Break
        }
    ));
    // Leaving the page cancels the wait. A page off screen would otherwise hold the callback until
    // it comes back, and the row would take focus then.
    let unmap = page.connect_unmap(glib::clone!(
        #[weak]
        revealing,
        move |_| cancel(&revealing)
    ));
    revealing.replace(Some(Pending {
        page: page.clone(),
        tick,
        unmap,
    }));
}

/// The window an explicit Quit shows until settings finish applying. When one fails, the user
/// either goes back to editing or quits anyway.
struct Quit {
    dialog: adw::Dialog,
    spinner: gtk::Spinner,
    text: gtk::Label,
    buttons: gtk::Box,
}

impl Quit {
    fn new(ctx: &Rc<Ctx>) -> Self {
        let spinner = gtk::Spinner::new();
        let text = para();
        text.set_xalign(0.5);
        text.set_justify(gtk::Justification::Center);
        text.set_accessible_role(gtk::AccessibleRole::Status);
        let button = |label: &str, ev: fn() -> Ev| {
            let b = gtk::Button::with_label(label);
            b.connect_clicked(glib::clone!(
                #[weak]
                ctx,
                move |_| ctx.send(ev())
            ));
            b
        };
        let cancel = button("Continue Editing", || Ev::CancelQuit);
        let quit = button("Quit Anyway", || Ev::QuitAnyway);
        quit.add_css_class("destructive-action");
        let buttons = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .homogeneous(true)
            .build();
        buttons.append(&cancel);
        buttons.append(&quit);
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        body.append(&spinner);
        body.append(&text);
        body.append(&buttons);
        let dialog = adw::Dialog::builder()
            .title("Cordial")
            .content_width(360)
            .can_close(false)
            .child(&body)
            .build();
        Self {
            dialog,
            spinner,
            text,
            buttons,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Iterates the main loop until `done` holds.
    fn pump(done: &dyn Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(
                Instant::now() < deadline,
                "the row was never focused and scrolled into view"
            );
            glib::MainContext::default().iteration(false);
        }
    }

    /// Whether focus is in `w`.
    fn focused(window: &gtk::Window, w: &impl IsA<gtk::Widget>) -> bool {
        GtkWindowExt::focus(window).is_some_and(|f| f.is_ancestor(w) || f == *w.upcast_ref())
    }

    /// Whether the middle of `w` is within its viewport's visible area.
    fn in_view(w: &impl IsA<gtk::Widget>) -> bool {
        let viewport = w.ancestor(gtk::Viewport::static_type()).unwrap();
        let bounds = w.compute_bounds(&viewport).unwrap();
        (0.0..viewport.height() as f32).contains(&bounds.center().y())
    }

    #[test]
    #[ignore = "requires a display"]
    fn search_opens_the_expander_row_holding_its_result() {
        adw::init().expect("GTK needs a display");
        let page = adw::PreferencesPage::new();
        let filler = adw::PreferencesGroup::new();
        let top = adw::ActionRow::builder().title("Row 0").build();
        filler.add(&top);
        for i in 1..30 {
            filler.add(&adw::ActionRow::builder().title(format!("Row {i}")).build());
        }
        page.add(&filler);
        let group = adw::PreferencesGroup::new();
        let expander = |title: &str, inner: &str, expands: bool| {
            let row = adw::ExpanderRow::builder()
                .title(title)
                .enable_expansion(expands)
                .build();
            let inner = adw::SpinRow::builder()
                .title(inner)
                .adjustment(&gtk::Adjustment::new(30.0, 1.0, 254.0, 1.0, 10.0, 0.0))
                .build();
            row.add_row(&inner);
            group.add(&row);
            (row, inner)
        };
        let (on, threshold) = expander("Automatic Free Spin", "Threshold", true);
        let (off, hidden) = expander("Other Switch", "Hidden", false);
        page.add(&group);
        let holder = adw::Bin::builder().child(&page).build();
        let window = gtk::Window::builder()
            .default_width(480)
            .default_height(300)
            .child(&holder)
            .build();
        window.present();
        pump(&|| page.height() > 0);
        assert!(!on.is_expanded());
        let revealing = Revealing::default();
        let reveal = |title: &str| reveal(page.upcast_ref(), &Row::Title(title.into()), &revealing);

        reveal("Threshold");
        assert!(on.is_expanded(), "the result's expander row opens");
        pump(&|| focused(&window, &threshold) && in_view(&threshold));

        reveal("Hidden");
        assert!(
            !off.is_expanded(),
            "an expander row that can't open stays closed"
        );
        pump(&|| focused(&window, &off) && in_view(&off));
        assert!(!focused(&window, &hidden));

        let revealer = threshold
            .ancestor(gtk::Revealer::static_type())
            .and_downcast::<gtk::Revealer>()
            .unwrap();
        let frames = || {
            let count = Rc::new(Cell::new(0));
            let seen = count.clone();
            window.add_tick_callback(move |_, _| {
                seen.set(seen.get() + 1);
                if seen.get() < 2 {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            });
            pump(&|| count.get() == 2);
        };
        for disable in [false, true] {
            on.set_expanded(false);
            pump(&|| !revealer.is_child_revealed());
            reveal("Threshold");
            if disable {
                on.set_enable_expansion(false);
            } else {
                on.set_expanded(false);
            }
            off.grab_focus();
            frames();
            on.set_enable_expansion(true);
            on.set_expanded(true);
            pump(&|| revealer.is_child_revealed());
            frames();
            assert!(focused(&window, &off), "a cancelled search stole focus");
        }

        on.set_expanded(false);
        pump(&|| !revealer.is_child_revealed());
        reveal("Threshold");
        reveal("Row 0");
        pump(&|| focused(&window, &top) && in_view(&top));
        pump(&|| revealer.is_child_revealed());
        frames();
        assert!(focused(&window, &top), "a replaced search stole focus");

        on.set_expanded(false);
        pump(&|| !revealer.is_child_revealed());
        reveal("Threshold");
        holder.set_child(Some(&gtk::Button::with_label("Elsewhere")));
        frames();
        holder.set_child(Some(&page));
        top.grab_focus();
        pump(&|| revealer.is_child_revealed());
        frames();
        assert!(
            focused(&window, &top),
            "a search on a page left before its result showed took focus on return"
        );
    }
}
