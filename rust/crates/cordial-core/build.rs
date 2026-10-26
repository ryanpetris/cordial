use std::{env, path::PathBuf};

#[path = "../../tools/bluetooth_config.rs"]
mod bluetooth_config;

fn main() {
    let config = env::var_os("CORDIAL_CONFIG").map(PathBuf::from);
    bluetooth_config::generate(
        &PathBuf::from(env::var_os("OUT_DIR").unwrap()),
        config.as_deref(),
    )
    .expect("generate Bluetooth capacities");
}
