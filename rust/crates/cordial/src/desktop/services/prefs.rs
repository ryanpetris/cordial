//! Saved preferences, login start and the Settings page.
//!
//! Linux preferences use GSettings. The configuration directory holds the XDG autostart entry.
use super::{Emit, ServiceEvent};
use crate::ui::app::words::CRITICAL_PERCENT;
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    fs, io,
    path::{Path, PathBuf},
    rc::Rc,
};

/// The titles of the Settings page's rows.
pub const TITLES: [&str; 5] = [
    "Start at Login",
    "Always Show Tray Icon",
    "Low Battery",
    "Low Battery Level",
    "Connections",
];

/// The highest low battery level the user can choose.
const MOST_PERCENT: i64 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub start_at_login: bool,
    pub always_show_tray: bool,
    pub notify_low_battery: bool,
    /// At most this charged is low, from 5 to 50.
    pub low_battery_percent: i64,
    pub notify_connections: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            start_at_login: false,
            always_show_tray: false,
            notify_low_battery: true,
            low_battery_percent: crate::ui::app::words::LOW_BATTERY_PERCENT,
            notify_connections: false,
        }
    }
}

fn level(n: f64) -> i64 {
    (n.round() as i64).clamp(CRITICAL_PERCENT, MOST_PERCENT)
}

const SCHEMA_DIRECTORY: &str = "share/glib-2.0/schemas";

fn schema() -> gio::SettingsSchema {
    let default = gio::SettingsSchemaSource::default();
    if let Ok(exe) = std::env::current_exe() {
        let directory = exe.parent().expect("an executable has a directory");
        for root in [Some(directory), directory.parent()].into_iter().flatten() {
            if let Ok(source) = gio::SettingsSchemaSource::from_directory(
                root.join(SCHEMA_DIRECTORY),
                default.as_ref(),
                false,
            ) && let Some(schema) = source.lookup(crate::desktop::APP_ID, false)
            {
                return schema;
            }
        }
    }
    default
        .and_then(|source| source.lookup(crate::desktop::APP_ID, true))
        .expect("Cordial's GSettings schema files are missing from the installation")
}

fn values(p: Preferences) -> [(&'static str, glib::Variant); 5] {
    [
        ("start-at-login", p.start_at_login.to_variant()),
        ("always-show-tray", p.always_show_tray.to_variant()),
        ("notify-low-battery", p.notify_low_battery.to_variant()),
        (
            "low-battery-percent",
            (p.low_battery_percent as i32).to_variant(),
        ),
        ("notify-connections", p.notify_connections.to_variant()),
    ]
}

/// One Exec argument, quoted for a desktop entry: `"`, `` ` ``, `$` and `\` take a backslash
/// inside double quotes, the value's own escaping then doubles each backslash, and `%` is
/// written `%%`.
fn quote(arg: &str) -> String {
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str(r"\\");
                out.push(c);
            }
            '\\' => out.push_str(r"\\\\"),
            '%' => out.push_str("%%"),
            '\n' => out.push_str(r"\n"),
            '\t' => out.push_str(r"\t"),
            '\r' => out.push_str(r"\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The autostart entry that runs `exe` in the background.
fn entry(exe: &Path) -> io::Result<String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the path isn't UTF-8"))?;
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=Cordial\nComment=Manage Cordial Bluetooth adapters\nExec={} desktop --hidden\nIcon={}\nX-GNOME-Autostart-enabled=true\nTerminal=false\n",
        quote(exe),
        crate::desktop::APP_ID,
    ))
}

/// Replaces `path` with `text` in one step, so a failed write leaves the old file.
fn write(path: &Path, text: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().expect("the path has a directory"))?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    fs::write(&temp, text)?;
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// GSettings preferences and the configuration directory's autostart entry.
pub struct Store {
    root: PathBuf,
    settings: gio::Settings,
}

impl Store {
    /// Opens the desktop's GSettings preferences.
    pub fn open(root: PathBuf) -> Self {
        let settings = gio::Settings::new_full(&schema(), gio::SettingsBackend::NONE, None);
        Self { root, settings }
    }

    pub fn get(&self) -> Preferences {
        Preferences {
            start_at_login: self.settings.boolean("start-at-login"),
            always_show_tray: self.settings.boolean("always-show-tray"),
            notify_low_battery: self.settings.boolean("notify-low-battery"),
            low_battery_percent: self.settings.int("low-battery-percent").into(),
            notify_connections: self.settings.boolean("notify-connections"),
        }
    }

    fn autostart(&self) -> PathBuf {
        self.root
            .join(format!("autostart/{}.desktop", crate::desktop::APP_ID))
    }

    fn transaction(&self) -> gio::Settings {
        let settings = gio::Settings::new_full(
            &self
                .settings
                .settings_schema()
                .expect("settings have a schema"),
            self.settings.backend().as_ref(),
            None,
        );
        settings.delay();
        settings
    }

    /// Saves changed values together. Schema ranges reject invalid battery levels.
    pub fn set(&self, p: Preferences) -> io::Result<()> {
        if !(CRITICAL_PERCENT..=MOST_PERCENT).contains(&p.low_battery_percent) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the low battery level must be between 5 and 50",
            ));
        }
        let transaction = self.transaction();
        for (key, value) in values(p) {
            if self.settings.value(key) != value {
                transaction
                    .set_value(key, &value)
                    .map_err(io::Error::other)?;
            }
        }
        transaction.apply();
        gio::Settings::sync();
        if values(p)
            .into_iter()
            .any(|(key, value)| self.settings.value(key) != value)
        {
            return Err(io::Error::other(
                "GSettings could not confirm the saved preferences",
            ));
        }
        Ok(())
    }

    pub fn connect_changed(&self, emit: Emit) -> glib::SignalHandlerId {
        let root = self.root.clone();
        self.settings.connect_changed(None, move |settings, key| {
            if key == "start-at-login" {
                sync_autostart(&Self {
                    root: root.clone(),
                    settings: settings.clone(),
                });
            }
            emit(ServiceEvent::Changed);
        })
    }

    pub fn disconnect(&self, id: glib::SignalHandlerId) {
        self.settings.disconnect(id);
    }

    /// Writes the autostart entry for `exe`, or removes it without one.
    pub fn set_autostart(&self, exe: Option<&Path>) -> io::Result<()> {
        match exe {
            Some(exe) => write(&self.autostart(), &entry(exe)?),
            None => remove(&self.autostart()),
        }
    }
}

/// Removes `path`, which may be gone already.
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The executable login start runs: the AppImage when running from one, since this executable
/// then lives in a mount that ends with the app, else this executable.
fn launcher(appimage: Option<std::ffi::OsString>, exe: io::Result<PathBuf>) -> io::Result<PathBuf> {
    match appimage.map(PathBuf::from) {
        Some(path) if path.is_absolute() => Ok(path),
        _ => exe,
    }
}

fn this_launcher() -> io::Result<PathBuf> {
    launcher(std::env::var_os("APPIMAGE"), std::env::current_exe())
}

/// Points the autostart entry at this launcher while Start at Login is on, and removes it
/// while it is off.
pub fn sync_autostart(store: &Store) {
    let synced = if store.get().start_at_login {
        this_launcher().and_then(|exe| store.set_autostart(Some(&exe)))
    } else {
        store.set_autostart(None)
    };
    if let Err(e) = synced {
        eprintln!("updating autostart failed: {e}");
    }
}

/// The Settings page's content, with its own toasts. Each change applies and saves at once.
pub fn page(store: &Rc<Store>, emit: &Emit) -> adw::ToastOverlay {
    let overlay = adw::ToastOverlay::new();
    let p = store.get();
    let toast = {
        let overlay = overlay.downgrade();
        move |text: &str| {
            if let Some(overlay) = overlay.upgrade() {
                overlay.add_toast(adw::Toast::new(text));
            }
        }
    };
    // Applies a change unless it is already in use, as when a failed change sets the control
    // back, and tells the app to show it.
    let change = {
        let store = store.clone();
        let emit = emit.clone();
        let toast = toast.clone();
        move |edit: &dyn Fn(&mut Preferences)| {
            let mut p = store.get();
            edit(&mut p);
            if p == store.get() {
                return true;
            }
            let saved = store.set(p);
            if let Err(e) = &saved {
                sync_autostart(&store);
                eprintln!("saving preferences failed: {e}");
                toast("Cordial couldn't save the setting.");
            }
            emit(ServiceEvent::Changed);
            saved.is_ok()
        }
    };

    let login = adw::SwitchRow::builder()
        .title("Start at Login")
        .active(p.start_at_login)
        .build();
    login.connect_active_notify({
        let store = store.clone();
        let change = change.clone();
        move |row| {
            let on = row.is_active();
            if on == store.get().start_at_login {
                return;
            }
            let applied = if on {
                this_launcher().and_then(|exe| store.set_autostart(Some(&exe)))
            } else {
                store.set_autostart(None)
            };
            match applied {
                Ok(()) => {
                    if !change(&|p| p.start_at_login = on) {
                        row.set_active(store.get().start_at_login);
                    }
                }
                Err(e) => {
                    eprintln!("updating autostart failed: {e}");
                    toast("Cordial couldn't change Start at Login.");
                    row.set_active(!on);
                }
            }
        }
    });
    let tray = switch(
        "Always Show Tray Icon",
        "always-show-tray",
        store,
        &change,
        |p, on| {
            p.always_show_tray = on;
        },
    );
    let general = adw::PreferencesGroup::new();
    general.add(&login);
    general.add(&tray);

    let low = switch(
        "Low Battery",
        "notify-low-battery",
        store,
        &change,
        |p, on| p.notify_low_battery = on,
    );
    let percent = adw::SpinRow::with_range(CRITICAL_PERCENT as f64, MOST_PERCENT as f64, 5.0);
    percent.set_title("Low Battery Level");
    // AT-SPI leaves out the presentation role libadwaita 1.6 and later give a spin row, together
    // with its spin button, so the row takes the list item role other rows have and its title
    // names the spin button.
    let widget = percent.upcast_ref::<gtk::Widget>();
    widget.set_accessible_role(gtk::AccessibleRole::ListItem);
    widget.update_property(&[gtk::accessible::Property::Label("Low Battery Level")]);
    // A numeric row refuses the percent sign its text shows.
    percent.set_numeric(false);
    percent.set_value(p.low_battery_percent as f64);
    percent.connect_output(|row| {
        row.set_text(&format!("{}%", row.value()));
        true
    });
    // Text that isn't a number keeps the level. libadwaita 1.5.0 and 1.5.1 lose the value a spin
    // row's input handler returns, so the row leaves the plain number for GTK to read.
    percent.connect_input(|row| {
        let text = row.text();
        let typed = text.trim().trim_end_matches('%').trim().parse::<f64>();
        let level = typed.ok().filter(|v| v.is_finite()).unwrap_or(row.value());
        let plain = level.to_string();
        if text != plain {
            row.set_text(&plain);
        }
        None
    });
    percent.connect_value_notify({
        let change = change.clone();
        let store = store.clone();
        move |row| {
            let n = level(row.value());
            if !change(&|p| p.low_battery_percent = n) {
                row.set_value(store.get().low_battery_percent as f64);
            }
        }
    });
    let connections = switch(
        "Connections",
        "notify-connections",
        store,
        &change,
        |p, on| p.notify_connections = on,
    );
    let notifications = adw::PreferencesGroup::builder()
        .title("Notifications")
        .build();
    notifications.add(&low);
    notifications.add(&percent);
    notifications.add(&connections);

    for (key, row) in [
        ("start-at-login", &login),
        ("always-show-tray", &tray),
        ("notify-low-battery", &low),
        ("notify-connections", &connections),
    ] {
        store
            .settings
            .bind(key, row, "active")
            .flags(gio::SettingsBindFlags::GET)
            .build();
    }
    store
        .settings
        .bind("low-battery-percent", &percent, "value")
        .flags(gio::SettingsBindFlags::GET)
        .build();

    let page = adw::PreferencesPage::new();
    page.add(&general);
    page.add(&notifications);
    overlay.set_child(Some(&page));
    overlay
}

fn switch(
    title: &str,
    key: &'static str,
    store: &Rc<Store>,
    change: &(impl Fn(&dyn Fn(&mut Preferences)) -> bool + Clone + 'static),
    set: fn(&mut Preferences, bool),
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .active(store.settings.boolean(key))
        .build();
    let change = change.clone();
    let store = store.clone();
    row.connect_active_notify(move |row| {
        let on = row.is_active();
        if !change(&|p| set(p, on)) {
            row.set_active(store.settings.boolean(key));
        }
    });
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("cordial-prefs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn settings(root: &Path, backend: &gio::SettingsBackend) -> gio::Settings {
        let directory = root.join("schemas");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("dev.petris.cordial.gschema.xml"),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../packaging/dev.petris.cordial.gschema.xml"
            )),
        )
        .unwrap();
        assert!(
            std::process::Command::new("glib-compile-schemas")
                .arg("--strict")
                .arg(&directory)
                .status()
                .unwrap()
                .success()
        );
        let schema = gio::SettingsSchemaSource::from_directory(&directory, None, false)
            .unwrap()
            .lookup(crate::desktop::APP_ID, false)
            .unwrap();
        gio::Settings::new_full(&schema, Some(backend), None)
    }

    fn test_store(root: PathBuf) -> Store {
        let settings = settings(&root, &gio::memory_settings_backend_new());
        Store { root, settings }
    }

    #[test]
    fn gsettings_round_trips_and_observes_external_changes() {
        glib::MainContext::new()
            .with_thread_default(|| {
                let root = root("settings");
                let backend = gio::memory_settings_backend_new();
                let settings = settings(&root, &backend);
                let store = Store {
                    root: root.clone(),
                    settings: settings.clone(),
                };
                assert_eq!(store.get(), Preferences::default());
                let p = Preferences {
                    always_show_tray: true,
                    low_battery_percent: 30,
                    ..Preferences::default()
                };
                store.set(p).unwrap();
                assert_eq!(
                    Store {
                        root: root.clone(),
                        settings: settings.clone(),
                    }
                    .get(),
                    p
                );
                let external = gio::Settings::new_full(
                    &settings.settings_schema().unwrap(),
                    Some(&backend),
                    None,
                );
                let changed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let id = store.connect_changed({
                    let changed = changed.clone();
                    std::sync::Arc::new(move |_| {
                        changed.store(true, std::sync::atomic::Ordering::Relaxed)
                    })
                });
                assert!(!store.get().notify_connections);
                external.set_boolean("notify-connections", true).unwrap();
                assert!(store.get().notify_connections);
                assert!(changed.load(std::sync::atomic::Ordering::Relaxed));
                assert!(
                    store
                        .set(Preferences {
                            low_battery_percent: 51,
                            ..p
                        })
                        .is_err()
                );
                assert_eq!(store.get().low_battery_percent, 30);
                store.disconnect(id);
                fs::remove_dir_all(root).unwrap();
            })
            .unwrap();
    }

    #[test]
    fn rejected_writes_keep_the_saved_preferences() {
        let root = root("readonly");
        let store = Store {
            root: root.clone(),
            settings: settings(&root, &gio::null_settings_backend_new()),
        };
        let saved = store.get();
        for edited in [
            Preferences {
                start_at_login: true,
                ..saved
            },
            Preferences {
                always_show_tray: true,
                ..saved
            },
            Preferences {
                notify_low_battery: false,
                ..saved
            },
            Preferences {
                low_battery_percent: 35,
                ..saved
            },
            Preferences {
                notify_connections: true,
                ..saved
            },
        ] {
            assert!(store.set(edited).is_err());
            assert_eq!(store.get(), saved);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires a display"]
    fn dialog_rolls_back_controls_when_gsettings_rejects_saves() {
        fn find(w: &gtk::Widget, title: &str) -> Option<adw::PreferencesRow> {
            if let Ok(row) = w.clone().downcast::<adw::PreferencesRow>()
                && row.title() == title
            {
                return Some(row);
            }
            let mut child = w.first_child();
            while let Some(c) = child {
                if let Some(row) = find(&c, title) {
                    return Some(row);
                }
                child = c.next_sibling();
            }
            None
        }

        adw::init().expect("GTK needs a display");
        let root = root("dialog-readonly");
        let store = Rc::new(Store {
            root: root.clone(),
            settings: settings(&root, &gio::null_settings_backend_new()),
        });
        let saved = store.get();
        assert_eq!(saved, Preferences::default());
        let emit: Emit = std::sync::Arc::new(|_| {});
        let dialog = page(&store, &emit);
        let window = adw::Window::builder().content(&dialog).build();
        window.present();
        while glib::MainContext::default().iteration(false) {}
        assert!(!store.autostart().exists());
        for title in TITLES {
            assert!(find(dialog.upcast_ref(), title).is_some(), "{title}");
        }

        for (title, active) in [
            ("Start at Login", saved.start_at_login),
            ("Always Show Tray Icon", saved.always_show_tray),
            ("Low Battery", saved.notify_low_battery),
            ("Connections", saved.notify_connections),
        ] {
            let row = find(dialog.upcast_ref(), title)
                .unwrap()
                .downcast::<adw::SwitchRow>()
                .unwrap();
            assert_eq!(row.is_active(), active, "{title}");
            row.set_active(!active);
            assert_eq!(row.is_active(), active, "{title} rolls back immediately");
            assert_eq!(store.get(), saved);
        }
        assert!(root.join("autostart").is_dir());
        assert!(!store.autostart().exists());

        let row = find(dialog.upcast_ref(), "Low Battery Level")
            .unwrap()
            .downcast::<adw::SpinRow>()
            .unwrap();
        assert_eq!(row.value(), saved.low_battery_percent as f64);
        row.set_text("35%");
        row.update();
        assert_eq!(row.value(), saved.low_battery_percent as f64);
        assert_eq!(store.get(), saved);
        window.close();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keyfile_backend_persists_preferences() {
        let root = root("persistent");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("preferences.ini");
        let backend = || gio::keyfile_settings_backend_new(file.to_str().unwrap(), "/", None);
        let expected = Preferences {
            notify_connections: true,
            ..Preferences::default()
        };
        {
            let store = Store {
                root: root.clone(),
                settings: settings(&root, &backend()),
            };
            store.set(expected).unwrap();
        }
        let reopened = Store {
            root: root.clone(),
            settings: settings(&root, &backend()),
        };
        assert_eq!(reopened.get(), expected);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quotes_exec_arguments() {
        assert_eq!(quote("/usr/bin/cordial"), r#""/usr/bin/cordial""#);
        assert_eq!(quote("/a b/c"), r#""/a b/c""#);
        assert_eq!(quote(r#"/a"b"#), r#""/a\\"b""#);
        assert_eq!(quote("/$HOME/`x`"), r#""/\\$HOME/\\`x\\`""#);
        assert_eq!(quote(r"/a\b"), r#""/a\\\\b""#);
        assert_eq!(quote("/100%"), r#""/100%%""#);
        assert_eq!(quote("/a\nb"), r#""/a\nb""#);
    }

    #[test]
    fn login_start_runs_the_appimage_when_there_is_one() {
        let exe = || Ok(PathBuf::from("/tmp/.mount_cordial/usr/bin/cordial"));
        let run = |appimage: Option<&str>| launcher(appimage.map(Into::into), exe()).unwrap();
        assert_eq!(
            run(Some("/opt/Apps/cordial.AppImage")),
            Path::new("/opt/Apps/cordial.AppImage")
        );
        assert_eq!(run(None), Path::new("/tmp/.mount_cordial/usr/bin/cordial"));
        assert_eq!(
            run(Some("")),
            Path::new("/tmp/.mount_cordial/usr/bin/cordial")
        );
        assert_eq!(
            run(Some("cordial.AppImage")),
            Path::new("/tmp/.mount_cordial/usr/bin/cordial")
        );
        let missing = launcher(None, Err(io::ErrorKind::NotFound.into()));
        assert!(missing.is_err());
    }

    #[test]
    fn a_failed_preference_save_does_not_keep_login_start_enabled_after_relaunch() {
        let root = root("sync-failed-save");
        let store = Store {
            root: root.clone(),
            settings: settings(&root, &gio::null_settings_backend_new()),
        };
        store
            .set_autostart(Some(Path::new("/usr/bin/cordial")))
            .unwrap();
        let preferences = Preferences {
            start_at_login: true,
            ..store.get()
        };
        assert!(store.set(preferences).is_err());
        assert!(store.autostart().exists());
        let reopened = test_store(root.clone());
        assert!(!reopened.get().start_at_login);
        sync_autostart(&reopened);
        assert!(!reopened.autostart().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_an_autostart_write_failure() {
        let root = root("autostart-failed");
        let store = test_store(root.clone());
        fs::create_dir_all(store.autostart()).unwrap();
        assert!(
            store
                .set_autostart(Some(Path::new("/usr/bin/cordial")))
                .is_err()
        );
        assert!(!store.get().start_at_login);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writes_and_removes_the_autostart_entry() {
        let root = root("autostart");
        let store = test_store(root.clone());
        let file = root.join("autostart/dev.petris.cordial.desktop");
        let unrelated = root.join("autostart/cordial.desktop");
        let unrelated_content =
            b"[Desktop Entry]\nType=Application\nName=Unrelated Application\nExec=unrelated\n";
        fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
        fs::write(&unrelated, unrelated_content).unwrap();
        store.set_autostart(None).unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read(&unrelated).unwrap(), unrelated_content);
        store
            .set_autostart(Some(Path::new("/opt/My Apps/cordial")))
            .unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(text.contains("\nExec=\"/opt/My Apps/cordial\" desktop --hidden\n"));
        assert!(text.contains(&format!("\nIcon={}\n", crate::desktop::APP_ID)));
        assert_eq!(fs::read(&unrelated).unwrap(), unrelated_content);
        store.set_autostart(None).unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read(&unrelated).unwrap(), unrelated_content);
        store.set_autostart(None).unwrap();
        assert_eq!(fs::read(&unrelated).unwrap(), unrelated_content);
        fs::remove_dir_all(root).unwrap();
    }
}
