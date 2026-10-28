mod proxy;

use cordial::{
    VERSION,
    controller::Cancellation,
    migration,
    runner::{Options, Script},
    ui::{self, UiOptions, text},
};
use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};
const USAGE_COMMANDS: &str = "Usage: cordial [--port PORT] [--json] [--timeout SECONDS] [COMMAND ...]\n       cordial [--port PORT] [--json] tui";
#[cfg(feature = "desktop")]
const USAGE_DESKTOP: &str = "\n       cordial [--port PORT] [--json] desktop [--hidden]";
#[cfg(not(feature = "desktop"))]
const USAGE_DESKTOP: &str = "";
const USAGE_OPTIONS: &str = "\n       cordial migrate --ipc\n\nNo command opens the interactive shell. Piped input is a command script.\n\n  --port PORT        USB CDC serial port\n  --json             Print each response and event as one line of protobuf JSON to stderr\n  --timeout SECONDS  Overall one-shot timeout; finite scan duration\n  --version          Print version\n  --help             Show usage";
fn main() -> ExitCode {
    let result = execute();
    let _ = cordial_client::diagnostics::flush();
    result
}

fn execute() -> ExitCode {
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
        Err(RunError::Reported) => ExitCode::FAILURE,
        Err(RunError::Message(error)) => {
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
enum RunError {
    Message(String),
    // The diagnostic was queued, or stderr failed and cannot carry another one.
    Reported,
}

impl From<String> for RunError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for RunError {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}

impl From<ui::UiError> for RunError {
    fn from(error: ui::UiError) -> Self {
        match error {
            ui::UiError::Reported(_) => Self::Reported,
            error => Self::Message(error.to_string()),
        }
    }
}

fn run(mut options: Options) -> Result<(), RunError> {
    if options.args == ["migrate", "--ipc"] {
        if options.port.is_some() || options.json || !options.timeout.is_zero() {
            return Err("migrate --ipc takes no other options".into());
        }
        let ipc = migration::Ipc::default();
        let interrupt = ipc.interrupter();
        ctrlc::set_handler(move || interrupt.interrupt()).map_err(|e| e.to_string())?;
        return ipc
            .run(io::stdin(), &mut io::stdout().lock(), &migration::live())
            .map_err(|e| RunError::Message(e.to_string()));
    }
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    if options.args.first().is_some_and(|s| s == "tui") {
        if options.args.len() != 1 {
            return Err("tui takes no arguments".into());
        }
        if !interactive {
            return Err("tui requires an interactive terminal".into());
        }
        let (ui, interrupt) = ui::tui(UiOptions {
            port: options.port,
            json: options.json,
        });
        ctrlc::set_handler(move || interrupt.interrupt()).map_err(|e| e.to_string())?;
        return ui.run().map_err(Into::into);
    }
    if interactive && options.args.is_empty() {
        let (ui, interrupt) = ui::shell(UiOptions {
            port: options.port,
            json: options.json,
        });
        ctrlc::set_handler(move || interrupt.interrupt()).map_err(|e| e.to_string())?;
        return ui.run().map_err(Into::into);
    }
    let cancellation = Cancellation::default();
    let signal = cancellation.clone();
    ctrlc::set_handler(move || signal.cancel()).map_err(|e| e.to_string())?;
    let output = if options.json {
        cordial::json::Output::start()
    } else {
        cordial::json::Output::completed()
    };
    let connector: std::sync::Arc<cordial::controller::Connector> =
        std::sync::Arc::new(|port, handler| {
            cordial_client::serial::open_with_handler(port, handler)
        });
    let connector = cordial::json::observe(connector, output.observer());
    // The shared writer logs raw records while the foreground runner prints human output.
    options.json = false;
    let mut diagnostics = output.diagnostics();
    let result = Script {
        options,
        cancellation,
        engine: None,
    }
    .with_connector(
        io::stdin(),
        &mut io::stdout(),
        &mut diagnostics,
        move |port, handler| connector(port, handler),
    );
    if let Err(error) = &result {
        let _ = writeln!(diagnostics, "{}", text::safe(&error.to_string()));
    }
    output.finish();
    if result.is_err() {
        return Err(RunError::Reported);
    }
    Ok(())
}
