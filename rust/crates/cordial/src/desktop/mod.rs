//! The desktop app: a GTK application that keeps running in the background after its window
//! closes. Launching it again while it runs opens the existing window.
//!
//! The shared [`App`](crate::ui::app::App) model lives on the GTK main thread. Controller threads
//! deliver their messages through a bounded channel; widget signal handlers never touch the
//! model and send their requests through a second channel instead, so both are handled by the
//! main loop between redraws.
mod adapter;
mod add;
mod device;
mod dialogs;
mod overview;
mod profiles;
mod search;
mod services;
mod settings;
mod shell;
mod sidebar;
mod widgets;
mod window;

use crate::{json, runner};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, OnceCell, RefCell},
    io::Write,
    process::ExitCode,
    rc::Rc,
    sync::Mutex,
};

// GTK callbacks and the tray worker share the active stderr queue.
static DIAGNOSTICS: Mutex<Option<json::Diagnostics>> = Mutex::new(None);

fn diagnostic(message: std::fmt::Arguments<'_>) {
    let diagnostics = DIAGNOSTICS.lock().unwrap().clone();
    if let Some(mut diagnostics) = diagnostics {
        let _ = writeln!(diagnostics, "{message}");
    } else {
        eprintln!("{message}");
    }
}

/// The application ID, which also names the desktop entry and the icon.
pub const APP_ID: &str = "dev.petris.cordial";

pub const USAGE: &str = "Usage: cordial [--port PORT] [--json] desktop [--hidden]\n\n  --port PORT  USB CDC serial port\n  --json       Print each response and event as one line of protobuf JSON to stderr\n  --hidden     Start without opening the window\n  --help       Show usage";

/// Simulated adapters replace serial ports when this names a positive count.
const SIMULATE: &str = "CORDIAL_DESKTOP_SIMULATE";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub port: Option<String>,
    /// Starts in the background, as when launched at login.
    pub hidden: bool,
    pub json: bool,
}

impl Options {
    /// Reads `desktop` and the arguments after it. Returns `None` for `--help`.
    pub fn parse(options: runner::Options) -> Result<Option<Self>, String> {
        if !options.timeout.is_zero() {
            return Err("desktop cannot use --timeout".into());
        }
        let mut desktop = Self {
            port: options.port,
            hidden: false,
            json: options.json,
        };
        for arg in options.args.iter().skip(1) {
            match arg.as_str() {
                "--hidden" => desktop.hidden = true,
                "--json" => desktop.json = true,
                "--help" | "-h" => return Ok(None),
                _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
                _ => return Err(format!("unexpected argument: {arg}")),
            }
        }
        Ok(Some(desktop))
    }
}

/// The argument that starts the app in the background. GTK sees it in place of `--hidden`, so
/// a launch the running instance receives keeps its window as it is.
const HIDDEN: &str = "hidden";

/// Whether a launch with these arguments shows the window.
fn presents(args: &[std::ffi::OsString]) -> bool {
    !args.iter().skip(1).any(|a| a == HIDDEN)
}

/// The number of simulated adapters an environment value asks for.
fn simulated(value: Option<&str>) -> Option<usize> {
    value?.trim().parse().ok().filter(|n| *n > 0)
}

/// Runs the app until it quits, or hands the launch over to the instance already running.
pub fn run(options: Options) -> ExitCode {
    let output = if options.json {
        json::Output::start()
    } else {
        json::Output::completed()
    };
    *DIAGNOSTICS.lock().unwrap() = Some(output.diagnostics());
    let observer = Some(output.observer());
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let shell: Rc<OnceCell<shell::Handle>> = Rc::default();
    let hold = RefCell::new(None);
    let port = Cell::new(options.port);
    let simulate = simulated(std::env::var(SIMULATE).ok().as_deref());
    app.connect_startup(glib::clone!(
        #[strong]
        shell,
        move |app| {
            startup(app);
            // Without a window, the app keeps running until it is asked to quit.
            hold.replace(Some(app.hold()));
            let _ = shell.set(shell::start(app, port.take(), simulate, observer.clone()));
        }
    ));
    app.connect_command_line(move |_, line| {
        if presents(&line.arguments())
            && let Some(shell) = shell.get()
        {
            shell.present(None);
        }
        glib::ExitCode::SUCCESS
    });
    // The command line belongs to `cordial`; GTK sees only whether the window opens.
    let args: &[&str] = if options.hidden {
        &["cordial", HIDDEN]
    } else {
        &["cordial"]
    };
    let result = if let Err(error) = app.register(None::<&gio::Cancellable>) {
        diagnostic(format_args!("Cordial couldn't start. {error}"));
        ExitCode::FAILURE
    } else if options.json && app.is_remote() {
        diagnostic(format_args!(
            "Cordial is already running. Quit Cordial before starting desktop --json."
        ));
        ExitCode::FAILURE
    } else {
        app.run_with_args(args).into()
    };
    output.finish();
    result
}

/// Asks the running app to quit, as its Quit does. Safe to call from any thread.
pub fn interrupt() {
    glib::idle_add_once(|| {
        if let Some(app) = gio::Application::default() {
            app.activate_action("quit", None);
        }
    });
}

fn startup(app: &adw::Application) {
    widgets::add_style();
    let accels: [(&str, &[&str]); 6] = [
        ("app.refresh", &["F5", "<Control>r"]),
        ("app.add-device", &["<Control>n"]),
        ("app.preferences", &["<Control>comma"]),
        ("app.quit", &["<Control>q"]),
        ("window.close", &["<Control>w"]),
        ("win.search", &["<Control>f"]),
    ];
    for (action, keys) in accels {
        app.set_accels_for_action(action, keys);
    }
}

const WEBSITE: &str = "https://github.com/ryanpetris/cordial";

fn about(window: &impl IsA<gtk::Widget>) {
    adw::AboutDialog::builder()
        .application_name("Cordial")
        .application_icon(APP_ID)
        .version(crate::VERSION)
        .website(WEBSITE)
        .copyright("\u{a9} 2026 Ryan Petris")
        // The wording libadwaita gives a known license, linking to the project's own copy.
        .license(format!(
            "This application comes with absolutely no warranty. See the \
             <a href=\"{WEBSITE}/blob/master/LICENSE\">MIT License</a> for details."
        ))
        .build()
        .present(Some(window));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Options>, String> {
        let options = runner::Options::parse(args.iter().map(|s| s.to_string())).unwrap();
        Options::parse(options)
    }

    #[test]
    fn reads_port_and_hidden() {
        assert_eq!(parse(&["desktop"]), Ok(Some(Options::default())));
        assert_eq!(
            parse(&["--port", "/dev/ttyACM0", "desktop", "--hidden"]),
            Ok(Some(Options {
                port: Some("/dev/ttyACM0".into()),
                hidden: true,
                json: false,
            }))
        );
    }

    #[test]
    fn help_wins_over_other_arguments() {
        assert_eq!(parse(&["desktop", "--hidden", "--help"]), Ok(None));
        assert_eq!(parse(&["desktop", "-h"]), Ok(None));
    }

    #[test]
    fn refuses_other_arguments() {
        assert_eq!(
            parse(&["desktop", "--hidden=1"]),
            Err("unknown option: --hidden=1".into())
        );
        assert_eq!(
            parse(&["desktop", "--port", "x"]),
            Err("unknown option: --port".into())
        );
        assert_eq!(
            parse(&["desktop", "status"]),
            Err("unexpected argument: status".into())
        );
        assert_eq!(
            parse(&["--timeout", "5", "desktop"]),
            Err("desktop cannot use --timeout".into())
        );
    }

    #[test]
    fn accepts_json_before_and_after_desktop() {
        let expected = Ok(Some(Options {
            json: true,
            ..Options::default()
        }));
        assert_eq!(parse(&["--json", "desktop"]), expected);
        assert_eq!(parse(&["desktop", "--json"]), expected);
        assert_eq!(
            parse(&["desktop", "--json", "--hidden"]),
            Ok(Some(Options {
                json: true,
                hidden: true,
                port: None
            }))
        );
        assert_eq!(
            parse(&["desktop", "--json=1"]),
            Err("unknown option: --json=1".into())
        );
    }

    #[test]
    fn a_hidden_launch_leaves_the_window_alone() {
        let args = |a: &[&str]| {
            a.iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>()
        };
        assert!(presents(&args(&["cordial"])));
        assert!(!presents(&args(&["cordial", HIDDEN])));
    }

    #[test]
    fn simulates_only_a_positive_count() {
        assert_eq!(simulated(Some("2")), Some(2));
        assert_eq!(simulated(Some(" 3 ")), Some(3));
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("-1"),
            Some("two"),
            Some("1.5"),
        ] {
            assert_eq!(simulated(value), None, "{value:?}");
        }
    }
}
