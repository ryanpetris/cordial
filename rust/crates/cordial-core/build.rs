use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=CORDIAL_CONFIG");
    if let Some(config) = env::var_os("CORDIAL_CONFIG") {
        println!("cargo:rerun-if-changed={}", PathBuf::from(config).display());
    }
    let generator =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/bluetooth_config.py");
    println!("cargo:rerun-if-changed={}", generator.display());
    assert!(
        Command::new("python3")
            .arg(generator)
            .arg(env::var_os("OUT_DIR").unwrap())
            .status()
            .expect("generate Bluetooth capacities")
            .success()
    );
}
