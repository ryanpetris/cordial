mod proxy;

use cordial::{
    VERSION,
    controller::Cancellation,
    migration,
    runner::{Options, Script},
    ui::{self, UiOptions, text},
};
use std::{
    io::{self, IsTerminal},
    process::ExitCode,
};
const USAGE_COMMANDS: &str = "Usage: cordial [--port PORT] [--json] [--timeout SECONDS] [COMMAND ...]\n       cordial [--port PORT] tui";
#[cfg(feature = "desktop")]
const USAGE_DESKTOP: &str = "\n       cordial [--port PORT] desktop [--hidden]";
#[cfg(not(feature = "desktop"))]
const USAGE_DESKTOP: &str = "";
const USAGE_OPTIONS: &str = "\n       cordial migrate --ipc\n\nNo command opens the interactive shell. Piped input is a command script.\n\n  --port PORT        USB CDC serial port\n  --json             Print each response and event as one line of protobuf\n                     JSON. File contents are never printed.\n  --timeout SECONDS  Overall one-shot timeout; finite scan duration\n  --version          Print version\n  --help             Show usage";
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(success) = proxy::dispatch(&args) {
        return if success {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{}", text::safe(&error));
            return ExitCode::from(2);
        }
    };
    if matches!(
        options.args.first().map(String::as_str),
        Some("--help" | "-h")
    ) {
        println!("{USAGE_COMMANDS}{USAGE_DESKTOP}{USAGE_OPTIONS}");
        return ExitCode::SUCCESS;
    }
    if options.args == ["--version"] {
        println!("cordial {VERSION}");
        return ExitCode::SUCCESS;
    }
    if options.args.first().is_some_and(|s| s == "desktop") {
        return desktop(options);
    }
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", text::safe(&error));
            ExitCode::FAILURE
        }
    }
}
#[cfg(feature = "desktop")]
fn desktop(options: Options) -> ExitCode {
    use cordial::desktop;
    match desktop::Options::parse(options) {
        Ok(Some(options)) => {
            if let Err(error) = ctrlc::set_handler(desktop::interrupt) {
                eprintln!("{}", text::safe(&error.to_string()));
                return ExitCode::FAILURE;
            }
            desktop::run(options)
        }
        Ok(None) => {
            println!("{}", desktop::USAGE);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", text::safe(&error));
            ExitCode::FAILURE
        }
    }
}
#[cfg(not(feature = "desktop"))]
fn desktop(_: Options) -> ExitCode {
    eprintln!("this build of cordial has no desktop app");
    ExitCode::FAILURE
}
fn run(options: Options) -> Result<(), String> {
    if options.args == ["migrate", "--ipc"] {
        if options.port.is_some() || options.json || !options.timeout.is_zero() {
            return Err("migrate --ipc takes no other options".into());
        }
        let ipc = migration::Ipc::default();
        let interrupt = ipc.interrupter();
        ctrlc::set_handler(move || interrupt.interrupt()).map_err(|e| e.to_string())?;
        return ipc
            .run(io::stdin(), &mut io::stdout().lock(), &migration::live())
            .map_err(|e| e.to_string());
    }
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    if options.args.first().is_some_and(|s| s == "tui") {
        if options.args.len() != 1 {
            return Err("tui takes no arguments".into());
        }
        if options.json || !interactive {
            return Err("tui requires an interactive terminal and cannot use --json".into());
        }
        let (ui, interrupt) = ui::tui(UiOptions { port: options.port });
        ctrlc::set_handler(move || interrupt.interrupt()).map_err(|e| e.to_string())?;
        return ui.run().map_err(|e| e.to_string());
    }
    if interactive && !options.json && options.args.is_empty() {
        let (ui, interrupt) = ui::shell(UiOptions { port: options.port });
        ctrlc::set_handler(move || interrupt.interrupt()).map_err(|e| e.to_string())?;
        return ui.run().map_err(|e| e.to_string());
    }
    let cancellation = Cancellation::default();
    let signal = cancellation.clone();
    ctrlc::set_handler(move || signal.cancel()).map_err(|e| e.to_string())?;
    Script {
        options,
        cancellation,
        engine: migration::live(),
    }
    .run(io::stdin(), &mut io::stdout(), &mut io::stderr())
    .map_err(|e| e.to_string())
}
