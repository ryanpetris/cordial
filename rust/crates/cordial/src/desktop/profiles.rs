//! Profiles: an adapter's profile memory, its configuration interfaces, and its profiles; a
//! device's ordered profile layers; and the picker that chooses a profile for a layer. Profile rules are never read or changed here.
use super::{
    dialogs::{Form, Modal},
    search::{interface_row, layer_row, profile_row},
    shell::Ctx,
    widgets::{
        ADAPTER_ICON, Btn, Item, List, Look, Menu, Status, Tab as TabView, Toggle, set_subtitle,
        set_title, spin, when,
    },
};
use crate::{
    profiles,
    ui::app::{
        Action, App, Live, PickFor, Spot, words,
        world::{AdapterView, DeviceView},
    },
};
use adw::prelude::*;
use cordial_protocol::ConfigurationInterface;
use gtk::{gdk, glib};
use std::{cell::RefCell, rc::Rc};

/// A profile's roles as short marks.
fn marks(roles: Vec<cordial_protocol::Role>) -> String {
    roles
        .into_iter()
        .filter_map(words::role_text)
        .collect::<Vec<_>>()
        .join(", ")
}

/// An interface's card: its switch, then the profile it uses.
struct Interface {
    group: adw::PreferencesGroup,
    toggle: Toggle,
    profile: adw::ComboRow,
    /// The profile IDs behind the profile row's choices, in order.
    ids: Rc<RefCell<Vec<u32>>>,
    labels: Vec<String>,
}

struct ProfileRow {
    row: adw::ActionRow,
    menu: Menu,
}

/// The adapter's Profiles tab.
pub struct AdapterProfiles {
    pub tab: TabView,
    /// Why the last change couldn't be saved.
    save: List,
    status: Status,
    memory: List,
    interfaces: Vec<(ConfigurationInterface, Interface)>,
    error: List,
    retry: Btn,
    list: adw::PreferencesGroup,
    rows: Vec<ProfileRow>,
    unreadable: List,
    spinner: gtk::Spinner,
    new: Btn,
    ctx: Rc<Ctx>,
}

impl AdapterProfiles {
    pub fn new(ctx: &Rc<Ctx>) -> Self {
        let tab = TabView::new();
        let memory = List::new(ctx, "Profile Memory");
        let interfaces: Vec<_> = profiles::INTERFACES
            .into_iter()
            .map(|i| (i, interface(ctx, i)))
            .collect();
        let error = List::new(ctx, "");
        let retry = Btn::new(ctx, "Retry");
        retry.widget.set_valign(gtk::Align::Center);
        let list = adw::PreferencesGroup::builder().title("Profiles").build();
        let new = Btn::icon(ctx, "list-add-symbolic", "New Profile");
        let unreadable = List::new(ctx, "");
        let spinner = super::widgets::spinner();
        let buttons = gtk::Box::builder()
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        buttons.append(&spinner);
        buttons.append(&new.widget);
        list.set_header_suffix(Some(&buttons));
        error.group.set_header_suffix(Some(&retry.widget));
        let save = List::new(ctx, "");
        tab.add(&save.group);
        tab.add(&memory.group);
        for (_, r) in &interfaces {
            tab.add(&r.group);
        }
        for group in [&error.group, &list, &unreadable.group] {
            tab.add(group);
        }
        Self {
            tab,
            save,
            status: Status::new(ADAPTER_ICON),
            memory,
            interfaces,
            error,
            retry,
            list,
            rows: Vec::new(),
            unreadable,
            spinner,
            new,
            ctx: ctx.clone(),
        }
    }

    pub fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>, a: &AdapterView) {
        self.save
            .set(&save_error(app.note(&Spot::AdapterSave(a.id.clone()))));
        let Some(view) = a.view() else {
            self.status.show(a.status_text().0, None);
            self.tab.show(Some(&self.status.widget.clone().upcast()));
            return;
        };
        self.tab.show(None);
        let memory: Vec<Item> = words::memory_percent(view)
            .map(|percent| {
                let look = if words::memory_alert(view) {
                    Look::Error
                } else {
                    Look::Plain
                };
                Item::fact("In Use", format!("{percent}%"))
                    .look(look)
                    .meter(Some(percent), Some(100))
            })
            .into_iter()
            .collect();
        self.memory.set(&memory);

        let editable = app.adapter_editable(a);
        let changes = app.adapter_changes(a);
        let page = app.shown_page(&a.id, false).unwrap_or_default();
        let staged_all = profiles::configured(
            view,
            &app.adapter_drafts
                .get(&a.id)
                .map(|d| d.interfaces.clone())
                .unwrap_or_default(),
        );
        // Each supported interface has a card, and the first one shown carries the heading.
        let mut first = true;
        for (i, r) in &mut self.interfaces {
            let s = staged_all.iter().find(|s| s.interface == *i as i32);
            r.group.set_visible(s.is_some());
            let Some(s) = s else {
                continue;
            };
            let title = if first {
                "Configuration Interfaces"
            } else {
                ""
            };
            first = false;
            if r.group.title() != title {
                r.group.set_title(title);
            }
            let staged = changes.interfaces.iter().any(|u| u.interface == *i);
            let conflict = staged_all.iter().any(|o| {
                o.enabled
                    && o.interface != s.interface
                    && (s.conflicts.contains(&o.interface) || o.conflicts.contains(&s.interface))
            });
            let can_enable = s.profile != 0 && !conflict;
            // Every readable listed profile, None while the interface is off, and the chosen
            // profile even when it isn't listed or can't be read.
            let mut items: Vec<(u32, String)> = Vec::new();
            if !s.enabled {
                items.push((0, "None".into()));
            }
            for p in page.all().0 {
                items.push((p.id, app.profile_text(&a.id, p.id)));
            }
            if s.profile != 0 && !items.iter().any(|(id, _)| *id == s.profile) {
                items.push((s.profile, app.profile_text(&a.id, s.profile)));
            }
            let labels: Vec<String> = items.iter().map(|(_, l)| l.clone()).collect();
            if labels != r.labels {
                let names: Vec<&str> = labels.iter().map(String::as_str).collect();
                r.profile.set_model(Some(&gtk::StringList::new(&names)));
                r.labels = labels;
            }
            *r.ids.borrow_mut() = items.iter().map(|(id, _)| *id).collect();
            let selected = items
                .iter()
                .position(|(id, _)| *id == s.profile)
                .map_or(gtk::INVALID_LIST_POSITION, |p| p as u32);
            if r.profile.selected() != selected {
                r.profile.set_selected(selected);
            }
            // Turning an interface on needs a profile, so the profile can be chosen while off.
            r.profile.set_sensitive(editable);
            r.toggle
                .show(s.enabled, editable && (s.enabled || can_enable));
            set_subtitle(
                &r.toggle.row,
                if !staged && s.enabled && !s.active {
                    "Inactive"
                } else {
                    ""
                },
            );
        }

        let ready = a.live().is_some();
        let disabled = app.profiles_busy(&a.id) || !editable || !ready;
        let error: Vec<Item> = page
            .error
            .iter()
            .map(|e| {
                let text = if e == words::READ_FAILED {
                    "The adapter couldn't read its profiles. Try again.".to_owned()
                } else {
                    format!("The adapter couldn't read its profiles. {e}")
                };
                Item::text(text).look(Look::Error)
            })
            .collect();
        self.error.set(&error);
        self.retry.set(when(!disabled, Action::ProfilesRetry));
        let (list, unreadable) = page.all();
        while self.rows.len() > list.len() {
            if let Some(r) = self.rows.pop() {
                self.list.remove(&r.row);
            }
        }
        while self.rows.len() < list.len() {
            let row = adw::ActionRow::new();
            let menu = profile_menu(&self.ctx);
            row.add_suffix(&menu.button);
            self.list.add(&row);
            self.rows.push(ProfileRow { row, menu });
        }
        for (r, profile) in self.rows.iter_mut().zip(&list) {
            let name = app.profile_text(&a.id, profile.id);
            r.row.set_widget_name(&profile_row(profile.id));
            name_menu(&r.menu, &name);
            set_title(&r.row, &name);
            set_subtitle(&r.row, &marks(profiles::roles(profile)));
            r.menu.set(&[
                (
                    "Copy…".into(),
                    when(!disabled, Action::ProfileCopy(profile.id)),
                ),
                (
                    "Delete…".into(),
                    when(!disabled, Action::ProfileDelete(profile.id)),
                ),
            ]);
        }
        let unreadable: Vec<Item> = unreadable
            .iter()
            .map(|r| {
                Item::fact(
                    format!("Profile {}", r.id),
                    words::couldnt_read(r.error.as_ref()),
                )
                .look(Look::Dim)
            })
            .collect();
        self.unreadable.set(&unreadable);
        spin(&self.spinner, page.reading || app.profiles_busy(&a.id));
        self.new
            .set(when(!disabled && page.listed, Action::ProfileNew));
    }
}

/// A profile row's menu, named for assistive technologies by its profile at each redraw.
fn profile_menu(ctx: &Rc<Ctx>) -> Menu {
    let menu = Menu::new(ctx);
    menu.button.set_icon_name("view-more-symbolic");
    menu.button.add_css_class("flat");
    menu.button.set_tooltip_text(Some("Profile Menu"));
    menu
}

fn name_menu(menu: &Menu, name: &str) {
    menu.button
        .update_property(&[gtk::accessible::Property::Label(&format!(
            "{name} Profile Menu"
        ))]);
}

/// Why the last change couldn't be saved, as a row.
fn save_error(note: Option<&str>) -> Vec<Item> {
    note.map(|n| Item::text(n).look(Look::Error))
        .into_iter()
        .collect()
}

fn interface(ctx: &Rc<Ctx>, i: ConfigurationInterface) -> Interface {
    let label = profiles::interface_label(i);
    let toggle = Toggle::new(
        label,
        glib::clone!(
            #[weak]
            ctx,
            move |on| ctx.act(Action::InterfaceEnabled(i, on))
        ),
    );
    // The visible title is Profile, and the accessible name says whose profile it is.
    let profile = adw::ComboRow::builder()
        .title("Profile")
        .name(interface_row(i))
        .build();
    let named = profile.upcast_ref::<gtk::Widget>();
    named.reset_relation(gtk::AccessibleRelation::LabelledBy);
    named.update_property(&[gtk::accessible::Property::Label(&format!(
        "{label} Profile"
    ))]);
    let ids: Rc<RefCell<Vec<u32>>> = Rc::default();
    profile.connect_selected_notify(glib::clone!(
        #[weak]
        ctx,
        #[strong]
        ids,
        move |r| {
            if let Some(id) = ids.borrow().get(r.selected() as usize) {
                ctx.act(Action::InterfaceProfile(i, *id));
            }
        }
    ));
    let group = adw::PreferencesGroup::new();
    group.add(&toggle.row);
    group.add(&profile);
    Interface {
        group,
        toggle,
        profile,
        ids,
        labels: Vec::new(),
    }
}

/// Where a layer sits: its device, the device's layers as shown, and its place among them.
#[derive(Clone)]
struct Place {
    adapter: String,
    device: u32,
    layers: Vec<u32>,
    index: usize,
}

struct LayerRow {
    row: adw::ActionRow,
    remove: Btn,
    /// The row's layer while the layers can be changed. A drag carries the place it starts from.
    place: Rc<RefCell<Option<Place>>>,
}

/// A device's Profiles tab: its ordered layers, each change applied as it is made. A layer moves
/// by dragging its row onto another row's place.
/// Without layers, a status page offers to add one, as GNOME Settings' Printers page does without
/// printers.
pub struct Layers {
    pub tab: TabView,
    /// Why the last change couldn't be saved.
    save: List,
    group: adw::PreferencesGroup,
    add: Btn,
    empty: adw::StatusPage,
    empty_add: Btn,
    rows: Vec<LayerRow>,
    ctx: Rc<Ctx>,
}

impl Layers {
    pub fn new(ctx: &Rc<Ctx>) -> Self {
        let tab = TabView::new();
        let save = List::new(ctx, "");
        tab.add(&save.group);
        let group = adw::PreferencesGroup::builder().title("Profiles").build();
        let add = Btn::icon(ctx, "list-add-symbolic", "Add Profile");
        group.set_header_suffix(Some(&add.widget));
        tab.add(&group);
        let empty_add = Btn::suggested(ctx, "Add Profile…");
        empty_add.widget.add_css_class("pill");
        empty_add.widget.set_halign(gtk::Align::Center);
        let empty = adw::StatusPage::builder()
            .icon_name("view-list-symbolic")
            .title("No Profiles")
            .child(&empty_add.widget)
            .vexpand(true)
            .build();
        Self {
            tab,
            save,
            group,
            add,
            empty,
            empty_add,
            rows: Vec::new(),
            ctx: ctx.clone(),
        }
    }

    fn row(&self) -> LayerRow {
        let row = adw::ActionRow::new();
        let handle = gtk::Image::builder()
            .icon_name("list-drag-handle-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        row.add_prefix(&handle);
        let remove = Btn::icon(&self.ctx, "user-trash-symbolic", "Remove");
        row.add_suffix(&remove.widget);
        let place: Rc<RefCell<Option<Place>>> = Rc::default();

        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        source.connect_prepare(glib::clone!(
            #[weak]
            row,
            #[strong]
            place,
            #[upgrade_or_default]
            move |source, x, y| {
                let place = place.borrow().clone()?;
                source.set_icon(
                    Some(&gtk::WidgetPaintable::new(Some(&row))),
                    x as i32,
                    y as i32,
                );
                Some(gdk::ContentProvider::for_value(
                    &glib::BoxedAnyObject::new(place).to_value(),
                ))
            }
        ));
        row.add_controller(source);

        let target =
            gtk::DropTarget::new(glib::BoxedAnyObject::static_type(), gdk::DragAction::MOVE);
        let ctx = Rc::downgrade(&self.ctx);
        target.connect_drop(glib::clone!(
            #[strong]
            place,
            move |_, value, _, _| {
                let Some(ctx) = ctx.upgrade() else {
                    return false;
                };
                let Ok(dragged) = value.get::<glib::BoxedAnyObject>() else {
                    return false;
                };
                let Ok(from) = dragged.try_borrow::<Place>() else {
                    return false;
                };
                let Some(to) = place.borrow().clone() else {
                    return false;
                };
                // A drag from another device's layers, or from an order since changed, moves
                // nothing.
                if (&from.adapter, from.device, &from.layers)
                    != (&to.adapter, to.device, &to.layers)
                {
                    return false;
                }
                ctx.act(Action::LayerMove {
                    adapter: to.adapter,
                    device: to.device,
                    layers: to.layers,
                    from: from.index,
                    to: to.index,
                });
                true
            }
        ));
        row.add_controller(target);
        LayerRow { row, remove, place }
    }

    pub fn update(&mut self, app: &App<Live>, d: &DeviceView) {
        let layers = app.device_values(d).layers.unwrap_or_default();
        let editable = app.device_editable(d);
        while self.rows.len() > layers.len() {
            if let Some(r) = self.rows.pop() {
                self.group.remove(&r.row);
            }
        }
        while self.rows.len() < layers.len() {
            let r = self.row();
            self.group.add(&r.row);
            self.rows.push(r);
        }
        let n = layers.len();
        for (i, (r, id)) in self.rows.iter_mut().zip(&layers).enumerate() {
            let name = app.profile_text(&d.adapter, *id);
            r.row.set_widget_name(&layer_row(i));
            set_title(&r.row, &name);
            set_subtitle(&r.row, &marks(app.profile_roles(&d.adapter, *id)));
            r.remove
                .widget
                .update_property(&[gtk::accessible::Property::Label(&format!("Remove {name}"))]);
            r.remove.set(when(editable, Action::LayerRemove(i)));
            *r.place.borrow_mut() = editable.then(|| Place {
                adapter: d.adapter.clone(),
                device: d.d.id,
                layers: layers.clone(),
                index: i,
            });
        }
        let max = app
            .adapter(&d.adapter)
            .and_then(|a| a.status.as_ref())
            .and_then(profiles::max_layers)
            .unwrap_or(0) as usize;
        let add = when(editable && n < max, Action::LayerAdd);
        self.empty_add.set(add.clone());
        self.add.set(add);
        let note = app.note(&Spot::LayersSave(d.adapter.clone(), d.d.id));
        self.save.set(&save_error(note));
        // A save error shows with the groups.
        let empty = n == 0 && note.is_none();
        self.tab
            .show(empty.then(|| self.empty.upcast_ref::<gtk::Widget>()));
    }
}

struct Choice {
    row: adw::ActionRow,
    check: gtk::CheckButton,
    id: Rc<RefCell<u32>>,
}

/// The profile picker for a configuration interface or a device's layers.
pub struct Picker {
    form: Form,
    adapter: String,
    purpose: PickFor,
    group: adw::PreferencesGroup,
    first: gtk::CheckButton,
    choices: Vec<Choice>,
    /// Shown once the adapter lists no profiles to choose, with New Profile.
    empty: adw::StatusPage,
    new: Btn,
    spinner: gtk::Spinner,
    retry: Btn,
    choose: Btn,
    ctx: Rc<Ctx>,
}

impl Picker {
    pub fn new(ctx: &Rc<Ctx>, adapter: &str, purpose: PickFor) -> Self {
        let title = match purpose {
            PickFor::Interface(i) => format!("{} Profile", profiles::interface_label(i)),
            PickFor::Layer(_) => "Add Profile".into(),
        };
        let form = Form::new(&title);
        // The profiles fill in after the dialog opens.
        form.dialog.set_content_height(520);
        let group = adw::PreferencesGroup::new();
        form.body.append(&group);
        let new = Btn::suggested(ctx, "New Profile…");
        new.widget.add_css_class("pill");
        new.widget.set_halign(gtk::Align::Center);
        let empty = adw::StatusPage::builder()
            .icon_name("view-list-symbolic")
            .title("No Profiles")
            .child(&new.widget)
            .visible(false)
            .build();
        empty.add_css_class("compact");
        form.body.append(&empty);
        let spinner = super::widgets::spinner();
        let retry = Btn::new(ctx, "Retry");
        let cancel = Btn::new(ctx, "Cancel");
        let choose = Btn::suggested(ctx, "Choose");
        form.start.append(&spinner);
        form.start.append(&retry.widget);
        form.button(&cancel);
        form.button(&choose);
        cancel.set(Some(Action::Cancel));
        Self {
            form,
            adapter: adapter.to_owned(),
            purpose,
            group,
            first: gtk::CheckButton::new(),
            choices: Vec::new(),
            empty,
            new,
            spinner,
            retry,
            choose,
            ctx: ctx.clone(),
        }
    }

    fn choice(&self) -> Choice {
        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .build();
        check.set_group(Some(&self.first));
        let row = adw::ActionRow::builder().activatable_widget(&check).build();
        row.add_prefix(&check);
        let id = Rc::new(RefCell::new(0));
        check.connect_toggled(glib::clone!(
            #[weak(rename_to = ctx)]
            self.ctx,
            #[strong]
            id,
            move |c| {
                if c.is_active() {
                    ctx.act(Action::Pick(*id.borrow()));
                }
            }
        ));
        Choice { row, check, id }
    }
}

impl Modal for Picker {
    fn dialog(&self) -> adw::Dialog {
        self.form.dialog.clone()
    }

    fn update(&mut self, _: &Rc<Ctx>, app: &App<Live>) {
        let Some(crate::ui::app::Dialog::Pick { chosen, .. }) = app.dialog() else {
            return;
        };
        let chosen = *chosen;
        let page = app.shown_page(&self.adapter, true).unwrap_or_default();
        let staged = match self.purpose {
            PickFor::Interface(i) => app
                .adapter(&self.adapter)
                .and_then(|a| app.staged_interface(a, i)),
            PickFor::Layer(_) => None,
        };
        let none = staged.as_ref().is_some_and(|s| !s.enabled);
        let current = staged.map_or(0, |s| s.profile);
        let mut items: Vec<(u32, String, String)> = Vec::new();
        if none {
            items.push((0, "None".into(), String::new()));
        }
        let (list, _) = page.all();
        for profile in &list {
            items.push((
                profile.id,
                app.profile_text(&self.adapter, profile.id),
                marks(profiles::roles(profile)),
            ));
        }
        if current != 0 && !list.iter().any(|p| p.id == current) {
            items.push((
                current,
                app.profile_text(&self.adapter, current),
                String::new(),
            ));
        }
        while self.choices.len() > items.len() {
            if let Some(c) = self.choices.pop() {
                self.group.remove(&c.row);
            }
        }
        while self.choices.len() < items.len() {
            let c = self.choice();
            self.group.add(&c.row);
            self.choices.push(c);
        }
        let mut any = false;
        for (c, (id, label, marks)) in self.choices.iter().zip(&items) {
            *c.id.borrow_mut() = *id;
            set_title(&c.row, label);
            set_subtitle(&c.row, marks);
            let on = *id == chosen;
            any |= on;
            if c.check.is_active() != on {
                c.check.set_active(on);
            }
        }
        if !any {
            // No choice is checked while the chosen profile isn't listed.
            self.first.set_active(true);
        }
        self.group.set_visible(!items.is_empty());
        let empty =
            list.is_empty() && current == 0 && page.listed && !page.reading && page.error.is_none();
        self.empty.set_visible(empty);
        let editable = app
            .adapter(&self.adapter)
            .is_some_and(|a| a.live().is_some() && app.adapter_editable(a));
        self.new.set(when(
            editable && !app.profiles_busy(&self.adapter),
            Action::PickNewProfile,
        ));
        self.form.note(page.error.as_deref());
        self.retry.offer(
            page.error.is_some(),
            when(app.picker_retryable(&self.adapter), Action::PickRetry),
        );
        spin(&self.spinner, page.reading);
        self.choose.set(when(none || chosen != 0, Action::Submit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::search::{self, Row, Target};
    use crate::ui::app::{Page, Tab};
    use std::{sync::Arc, time::Duration};

    #[test]
    #[ignore = "requires a display"]
    fn cleaned_empty_profile_names_use_id_fallbacks() {
        adw::init().expect("GTK needs a display");
        let world = crate::sim::Desktop::new(1);
        {
            let mut sim = world.adapters()[0].0.lock().unwrap();
            for (id, name) in [
                (1, "\u{200b}\u{202e}"),
                (2, " \t\n "),
                (3, " Pointer\u{202e} "),
            ] {
                sim.profiles.iter_mut().find(|p| p.id == id).unwrap().name = name.into();
            }
        }
        let adapter = world.ports()[0].serial.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(world.live(Arc::new(move |msg| {
            let _ = tx.send(msg);
        })));
        let next = |app: &mut App<Live>| {
            app.update(
                rx.recv_timeout(Duration::from_secs(5))
                    .expect("the simulated adapter answers"),
            );
        };
        while !app.shown_page(&adapter, false).is_some_and(|p| p.listed)
            || app.device(&adapter, 2).is_none()
        {
            next(&mut app);
        }
        let (ctx, requests) = Ctx::test();
        let mut library = AdapterProfiles::new(&ctx);
        library.update(&ctx, &app, app.adapter(&adapter).unwrap());
        for (id, name) in [(1, "Profile 1"), (2, "Profile 2"), (3, "Pointer")] {
            let row = library
                .rows
                .iter()
                .find(|r| r.row.widget_name() == profile_row(id))
                .unwrap();
            assert_eq!(row.row.title(), name);
            assert!(search::find(&search::index(&app), name).iter().any(|h| {
                h.title == name
                    && h.target
                        == Target {
                            page: Some(Page::Adapter(adapter.clone())),
                            tab: Tab::Profiles,
                            row: Some(Row::Name(profile_row(id))),
                        }
            }));
        }
        for (_, interface) in &library.interfaces {
            for (id, label) in interface.ids.borrow().iter().zip(&interface.labels) {
                if *id != 0 {
                    assert_eq!(*label, app.profile_text(&adapter, *id));
                }
            }
        }
        app.show(Page::Device(adapter.clone(), 2));
        app.act(Action::LayerAdd);
        while !app.shown_page(&adapter, true).is_some_and(|p| p.listed) {
            next(&mut app);
        }
        let purpose = PickFor::Layer(2);
        let mut picker = Picker::new(&ctx, &adapter, purpose);
        picker.update(&ctx, &app);
        for (id, name) in [(1, "Profile 1"), (2, "Profile 2"), (3, "Pointer")] {
            let choice = picker
                .choices
                .iter()
                .find(|c| *c.id.borrow() == id)
                .unwrap();
            assert_eq!(choice.row.title(), name);
            while requests.try_recv().is_ok() {}
            choice.check.set_active(true);
            assert!(
                matches!(requests.try_recv(), Ok(super::super::shell::Ev::Act(Action::Pick(picked))) if picked == id)
            );
        }
        let mut layers = Layers::new(&ctx);
        layers.update(&app, app.device(&adapter, 2).unwrap());
        assert_eq!(layers.rows[0].row.title(), "Profile 1");
        assert_eq!(
            layers.rows[0].remove.widget.tooltip_text().as_deref(),
            Some("Remove")
        );
        {
            use glib::translate::{IntoGlib, ToGlibPtr, from_glib_full};
            let mismatch: Option<glib::GString> = unsafe {
                from_glib_full(gtk::ffi::gtk_test_accessible_check_property(
                    layers.rows[0]
                        .remove
                        .widget
                        .upcast_ref::<gtk::Accessible>()
                        .to_glib_none()
                        .0,
                    gtk::AccessibleProperty::Label.into_glib(),
                    c"Remove Profile 1".as_ptr(),
                ))
            };
            assert_eq!(mismatch, None);
        }
        assert!(
            app.deletion_steps(&adapter, 1)
                .last()
                .unwrap()
                .contains("Profile 1")
        );
        app.quit();
        while !app.done() {
            next(&mut app);
        }
    }
}
