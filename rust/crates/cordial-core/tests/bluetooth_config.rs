use std::{fs, path::Path, time::SystemTime};

#[path = "../../../tools/bluetooth_config.rs"]
mod bluetooth_config;

fn assert_generated(output: &Path, config: Option<&Path>, expected: [usize; 3]) {
    bluetooth_config::generate(output, config).unwrap();
    let mut rust = String::new();
    let mut header = String::from("#pragma once\n");
    for (name, value) in ["CLASSIC", "BLE", "ACTIVE"].into_iter().zip(expected) {
        rust.push_str(&format!("pub const {name}_CONNECTIONS: usize = {value};\n"));
        header.push_str(&format!("#define CORDIAL_{name}_CONNECTIONS {value}\n"));
    }
    assert_eq!(
        fs::read_to_string(output.join("bluetooth.rs")).unwrap(),
        rust
    );
    assert_eq!(
        fs::read_to_string(output.join("cordial_bluetooth.h")).unwrap(),
        header
    );
}

#[test]
fn generates_matching_rust_and_c_capacities() {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let output =
        std::env::temp_dir().join(format!("cordial-bluetooth-{}-{nonce}", std::process::id()));
    fs::create_dir(&output).unwrap();
    let config = output.join("board.json");

    assert_generated(&output, None, [6, 14, 21]);
    assert_generated(&output, Some(Path::new("")), [6, 14, 21]);
    for (json, expected) in [
        ("{}", [6, 14, 21]),
        (r#"{"chip":"unknown"}"#, [6, 14, 21]),
        (r#"{"chip":"rp2350"}"#, [6, 14, 21]),
        (r#"{"chip":"rp2040"}"#, [2, 8, 11]),
        (r#"{"chip":"esp32s3"}"#, [0, 8, 9]),
        (r#"{"chip":null}"#, [6, 14, 21]),
        (r#"{"chip":42}"#, [6, 14, 21]),
        (r#"{"chip":false}"#, [6, 14, 21]),
        (r#"{"chip":[]}"#, [6, 14, 21]),
        (r#"{"chip":{}}"#, [6, 14, 21]),
    ] {
        fs::write(&config, json).unwrap();
        assert_generated(&output, Some(&config), expected);
    }
    let boards = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../boards");
    for (board, expected) in [
        ("pico_w", [2, 8, 11]),
        ("pico2_w", [6, 14, 21]),
        ("pimoroni_pico_plus2_w", [6, 14, 21]),
        ("waveshare_rp2350b_plus_w", [6, 14, 21]),
        ("esp32s3", [0, 8, 9]),
    ] {
        assert_generated(
            &output,
            Some(&boards.join(format!("{board}.json"))),
            expected,
        );
    }

    let rust = fs::read(output.join("bluetooth.rs")).unwrap();
    let header = fs::read(output.join("cordial_bluetooth.h")).unwrap();
    for json in ["", "{", "null", "[]", "false", "42", r#""rp2040""#] {
        fs::write(&config, json).unwrap();
        assert!(bluetooth_config::generate(&output, Some(&config)).is_err());
        assert_eq!(fs::read(output.join("bluetooth.rs")).unwrap(), rust);
        assert_eq!(
            fs::read(output.join("cordial_bluetooth.h")).unwrap(),
            header
        );
    }
    fs::remove_file(&config).unwrap();
    assert!(bluetooth_config::generate(&output, Some(&config)).is_err());
    fs::remove_dir_all(output).unwrap();
}
