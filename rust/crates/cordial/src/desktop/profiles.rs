//! Profiles: an adapter's profile memory, its configuration interfaces, and its profiles; a
//! device's ordered profile layers; and the picker that chooses a profile for a layer. Profile rules are never read or changed here.
use super::{
    dialogs::{Form, Modal},
    search::{layer_row, profile_row},
    shell::Ctx,
    widgets::{
        ADAPTER_ICON, Btn, Item, List, Look, Menu, Status, Tab as TabView, set_label, set_look,
        set_subtitle, set_title, spin, when,
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

struct Interface {
    row: adw::ComboRow,
    /// The profile IDs behind the row's choices, in order.
    ids: Rc<RefCell<Vec<u32>>>,
    labels: Vec<String>,
    switch: gtk::Switch,
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
    memory_bar: gtk::LevelBar,
    memory_text: gtk::Label,
    memory_group: adw::PreferencesGroup,
    interfaces_group: adw::PreferencesGroup,
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
        let memory_bar = gtk::LevelBar::builder()
            .width_request(160)
            .valign(gtk::Align::Center)
            .build();
        let memory_text = gtk::Label::new(None);
        let memory = adw::ActionRow::builder().title("In Use").build();
        memory.add_suffix(&memory_bar);
        memory.add_suffix(&memory_text);
        let memory_group = adw::PreferencesGroup::builder()
            .title("Profile Memory")
            .build();
        memory_group.add(&memory);
        let interfaces_group = adw::PreferencesGroup::builder()
            .title("Configuration Interfaces")
            .build();
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
        for group in [
            &save.group,
            &memory_group,
            &interfaces_group,
            &error.group,
            &list,
            &unreadable.group,
        ] {
            tab.add(group);
        }
        Self {
            tab,
            save,
            status: Status::new(ADAPTER_ICON),
            memory_bar,
            memory_text,
            memory_group,
            interfaces_group,
            interfaces: Vec::new(),
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

    pub fn update(&mut self, ctx: &Rc<Ctx>, app: &App<Live>, a: &AdapterView) {
        self.save
            .set(&save_error(app.note(&Spot::AdapterSave(a.id.clone()))));
        let Some(view) = a.view() else {
            self.status.show(a.status_text().0, None);
            self.tab.show(Some(&self.status.widget.clone().upcast()));
            return;
        };
        self.tab.show(None);
        match words::memory_percent(view) {
            Some(percent) => {
                self.memory_group.set_visible(true);
                self.memory_bar
                    .set_value(f64::from(percent.min(100)) / 100.0);
                set_label(&self.memory_text, &format!("{percent}%"));
                let look = if words::memory_alert(view) {
                    Look::Error
                } else {
                    Look::Plain
                };
                set_look(&self.memory_text, look);
            }
            None => self.memory_group.set_visible(false),
        }

        let editable = app.adapter_editable(a);
        let changes = app.adapter_changes(a);
        let known: Vec<ConfigurationInterface> = profiles::interfaces(view)
            .iter()
            .filter_map(|s| ConfigurationInterface::try_from(s.interface).ok())
            .filter(|i| profiles::INTERFACES.contains(i))
            .collect();
        let page = app.shown_page(&a.id, false).unwrap_or_default();
        if known != self.interfaces.iter().map(|(i, _)| *i).collect::<Vec<_>>() {
            for (_, r) in self.interfaces.drain(..) {
                self.interfaces_group.remove(&r.row);
            }
            for i in &known {
                let r = interface_row(ctx, *i);
                self.interfaces_group.add(&r.row);
                self.interfaces.push((*i, r));
            }
        }
        self.interfaces_group.set_visible(!known.is_empty());
        let staged_all = profiles::configured(
            view,
            &app.adapter_drafts
                .get(&a.id)
                .map(|d| d.interfaces.clone())
                .unwrap_or_default(),
        );
        for (i, r) in &mut self.interfaces {
            let Some(s) = staged_all.iter().find(|s| s.interface == *i as i32) else {
                continue;
            };
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
                let name = words::clean(&p.name);
                let name = if name.is_empty() {
                    format!("Profile {}", p.id)
                } else {
                    name
                };
                items.push((p.id, name));
            }
            if s.profile != 0 && !items.iter().any(|(id, _)| *id == s.profile) {
                items.push((s.profile, app.profile_text(&a.id, s.profile)));
            }
            let labels: Vec<String> = items.iter().map(|(_, l)| l.clone()).collect();
            if labels != r.labels {
                let names: Vec<&str> = labels.iter().map(String::as_str).collect();
                r.row.set_model(Some(&gtk::StringList::new(&names)));
                r.labels = labels;
            }
            *r.ids.borrow_mut() = items.iter().map(|(id, _)| *id).collect();
            let selected = items
                .iter()
                .position(|(id, _)| *id == s.profile)
                .map_or(gtk::INVALID_LIST_POSITION, |p| p as u32);
            if r.row.selected() != selected {
                r.row.set_selected(selected);
            }
            r.row.set_sensitive(editable);
            if r.switch.is_active() != s.enabled {
                r.switch.set_active(s.enabled);
            }
            r.switch
                .set_sensitive(editable && (s.enabled || can_enable));
            set_subtitle(
                &r.row,
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
            let menu = Menu::new(&self.ctx);
            menu.button.set_icon_name("view-more-symbolic");
            menu.button.add_css_class("flat");
            menu.button.set_tooltip_text(Some("Profile Menu"));
            row.add_suffix(&menu.button);
            self.list.add(&row);
            self.rows.push(ProfileRow { row, menu });
        }
        for (r, profile) in self.rows.iter_mut().zip(&list) {
            let name = words::clean(&profile.name);
            r.row.set_widget_name(&profile_row(profile.id));
            r.menu
                .button
                .update_property(&[gtk::accessible::Property::Label(&name)]);
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

/// Why the last change couldn't be saved, as a row.
fn save_error(note: Option<&str>) -> Vec<Item> {
    note.map(|n| Item::text(n).look(Look::Error))
        .into_iter()
        .collect()
}

fn interface_row(ctx: &Rc<Ctx>, i: ConfigurationInterface) -> Interface {
    let row = adw::ComboRow::builder()
        .title(profiles::interface_label(i))
        .build();
    let ids: Rc<RefCell<Vec<u32>>> = Rc::default();
    row.connect_selected_notify(glib::clone!(
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
    let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    switch.update_property(
        &[gtk::accessible::Property::Label(profiles::interface_label(
            i,
        ))],
    );
    switch.connect_active_notify(glib::clone!(
        #[weak]
        ctx,
        move |s| ctx.act(Action::InterfaceEnabled(i, s.is_active()))
    ));
    row.add_suffix(&switch);
    Interface {
        row,
        ids,
        labels: Vec::new(),
        switch,
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
/// by dragging its row onto another row's place. Without layers, a status page offers to add one,
/// as GNOME Settings' Printers page does without printers.
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
        self.tab.show(empty.then(|| self.empty.upcast_ref::<gtk::Widget>()));
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
    spinner: gtk::Spinner,
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
        let spinner = super::widgets::spinner();
        let cancel = Btn::new(ctx, "Cancel");
        let choose = Btn::suggested(ctx, "Choose");
        form.start.append(&spinner);
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
            spinner,
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
                words::clean(&profile.name),
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
        self.form.note(page.error.as_deref());
        spin(&self.spinner, page.reading);
        self.choose.set(when(none || chosen != 0, Action::Submit));
    }
}
