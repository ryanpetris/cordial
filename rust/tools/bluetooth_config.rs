use std::{collections::BTreeMap, error::Error, fs, path::Path};

pub fn generate(output: &Path, config: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let tools = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools");
    println!("cargo:rerun-if-env-changed=CORDIAL_CONFIG");
    for name in ["bluetooth_config.rs", "bluetooth_limits.json"] {
        println!("cargo:rerun-if-changed={}", tools.join(name).display());
    }
    let config = match config.filter(|path| !path.as_os_str().is_empty()) {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", path.display());
            fs::read_to_string(path)?
        }
        None => "{}".to_owned(),
    };
    let config: serde_json::Value = serde_json::from_str(&config)?;
    let config = config
        .as_object()
        .ok_or("Bluetooth configuration must be a JSON object")?;
    let limits: BTreeMap<String, [usize; 2]> =
        serde_json::from_str(include_str!("bluetooth_limits.json"))?;
    let [classic, ble] = *config
        .get("chip")
        .and_then(serde_json::Value::as_str)
        .and_then(|chip| limits.get(chip))
        .unwrap_or(&limits["default"]);
    let mut rust = String::new();
    let mut header = String::from("#pragma once\n");
    for (name, value) in [
        ("CLASSIC_CONNECTIONS", classic),
        ("BLE_CONNECTIONS", ble),
        ("ACTIVE_CONNECTIONS", classic + ble + 1),
    ] {
        rust.push_str(&format!("pub const {name}: usize = {value};\n"));
        header.push_str(&format!("#define CORDIAL_{name} {value}\n"));
    }
    fs::write(output.join("bluetooth.rs"), rust)?;
    fs::write(output.join("cordial_bluetooth.h"), header)?;
    Ok(())
}
