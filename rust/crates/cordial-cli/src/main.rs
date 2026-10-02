use cordial_cli::{
    controller::Cancellation,
    runner::{Options, Script},
    ui::{self, UiOptions, text},
};
use std::{
    io::{self, IsTerminal},
    process::ExitCode,
};
const USAGE: &str = "Usage: cordial [--port PORT] [--json] [--timeout SECONDS] [COMMAND ...]\n       cordial [--port PORT] tui\n\nNo command opens the interactive shell. Piped input is a command script.\n\n  --port PORT        USB CDC serial port\n  --json             Print each response and event as one line of protobuf\n                     JSON. File contents are never printed.\n  --timeout SECONDS  Overall one-shot timeout; finite scan duration\n  --version          Print version\n  --help             Show usage";
fn main() -> ExitCode {
    let options = match Options::parse(std::env::args().skip(1)) {
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
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if options.args == ["--version"] {
        println!(
            "cordial {}",
            option_env!("CORDIAL_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
        );
        return ExitCode::SUCCESS;
    }
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", text::safe(&error));
            ExitCode::FAILURE
        }
    }
}
fn run(options: Options) -> Result<(), String> {
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
    }
    .run(io::stdin(), &mut io::stdout(), &mut io::stderr())
    .map_err(|e| e.to_string())
}
