//! Foreground desktop control client, shared by the shell and terminal UI.
pub mod client;
mod commands;
pub mod controller;
pub mod runner;
mod session;
pub mod transport;
pub mod ui;
mod view;

pub mod storage;
