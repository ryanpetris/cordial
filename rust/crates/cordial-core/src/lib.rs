#![no_std]

extern crate alloc;

pub mod application;
pub mod bluetooth;
pub mod bonds;
pub mod codec;
pub mod compact;
pub mod configurator;
pub mod control;
pub mod deferred;
pub mod devices;
pub mod features;
pub mod forward;
pub mod hid;
pub mod hidpp;
pub mod info;
pub mod interfaces;
pub mod layouts;
pub mod link;
pub mod manager;
pub mod model;
pub mod profiles;
pub mod settings;
pub mod storage;
pub mod wire;

pub mod identity;

pub mod battery;
