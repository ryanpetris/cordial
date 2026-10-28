//! The Cordial command line: one-shot commands, scripts, the interactive shell, and the
//! terminal UI, all over one foreground session with an adapter, and the desktop app.
mod commands;
pub mod controller;
#[cfg(feature = "desktop")]
pub mod desktop;
pub mod error;
pub mod json;
pub mod migration;
pub mod model;
pub mod profiles;
pub mod records;
pub mod runner;
pub mod settings;
pub mod sim;
pub mod storage;
pub mod ui;
pub mod view;

/// The release version, or the crate version in development builds.
pub const VERSION: &str = match option_env!("CORDIAL_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
