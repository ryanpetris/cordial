fn main() {
    println!("cargo:rerun-if-env-changed=CORDIAL_VERSION");
    use std::{env, path::PathBuf, process::Command};
    embuild::espidf::sysenv::output();
    println!("cargo:rustc-link-arg=-Wl,--undefined=CORDIAL_METADATA");
    let development = std::env::var_os("CARGO_FEATURE_DEVELOPMENT").is_some();
    let production = std::env::var_os("CARGO_FEATURE_PRODUCTION").is_some();
    assert!(
        development != production,
        "Select exactly one firmware profile"
    );
    let config = embuild::espidf::sysenv::cfg_args().expect("native SDK configuration");
    assert_eq!(
        config.get("esp_idf_cordial_development").is_some(),
        development,
        "SDK configuration differs from the selected firmware profile"
    );
    let choices = [
        ("BTSTACK", "esp_idf_bt_controller_only"),
        ("ESP_NIMBLE", "esp_idf_bt_nimble_enabled"),
    ];
    let mut selected = 0;
    for (feature, sdk) in choices {
        let enabled = env::var_os(format!("CARGO_FEATURE_{feature}")).is_some();
        selected += usize::from(enabled);
        if enabled {
            assert!(
                config.get(sdk).is_some(),
                "Bluetooth selection differs from SDK"
            );
        }
    }
    assert_eq!(selected, 1, "Select exactly one Bluetooth host");
    println!("cargo:rerun-if-env-changed=CORDIAL_CONFIG");
    if env::var_os("CARGO_FEATURE_FIRMWARE").is_some() {
        let board =
            PathBuf::from(env::var_os("CORDIAL_CONFIG").expect("firmware requires CORDIAL_CONFIG"));
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let generator = root.join("tools/firmware_config.py");
        println!("cargo:rerun-if-changed={}", board.display());
        println!("cargo:rerun-if-changed={}", generator.display());
        println!(
            "cargo:rerun-if-changed={}",
            root.join("tools/firmware_artifact.py").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            root.join("Cargo.toml").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            root.join("tools/esp_config.py").display()
        );
        assert!(
            Command::new("python3")
                .arg(generator)
                .arg(board)
                .arg(env::var_os("OUT_DIR").unwrap())
                .args([
                    "--profile",
                    if development {
                        "development"
                    } else {
                        "production"
                    }
                ])
                .status()
                .expect("generate board configuration")
                .success()
        );
    }
}
