use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=CORDIAL_VERSION");
    println!("cargo:rustc-check-cfg=cfg(board_configured)");
    println!("cargo:rerun-if-env-changed=CORDIAL_CONFIG");
    let Some(config) = env::var_os("CORDIAL_CONFIG") else {
        assert!(
            env::var_os("CARGO_FEATURE_FIRMWARE").is_none(),
            "A firmware executable requires CORDIAL_CONFIG"
        );
        // Library-only checks don't instantiate hardware configuration.
        return;
    };
    println!("cargo:rustc-cfg=board_configured");
    let config = PathBuf::from(config).canonicalize().expect("board path");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let generator = root.join("tools/firmware_config.py");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let profile = match (
        env::var_os("CARGO_FEATURE_DEVELOPMENT").is_some(),
        env::var_os("CARGO_FEATURE_PRODUCTION").is_some(),
    ) {
        (true, false) => "development",
        (false, true) => "production",
        _ => panic!("Select exactly one firmware profile: development or production"),
    };
    println!("cargo:rerun-if-changed={}", config.display());
    println!("cargo:rerun-if-changed={}", generator.display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("tools/firmware_artifact.py").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("Cargo.toml").display()
    );
    assert!(
        Command::new("python3")
            .arg(generator)
            .arg(&config)
            .arg(&out)
            .args(["--profile", profile])
            .status()
            .expect("run configuration generator")
            .success(),
        "invalid board configuration"
    );
    if env::var_os("CARGO_FEATURE_FIRMWARE").is_some() {
        let dependencies = root.join("tools/firmware_dependencies.py");
        println!("cargo:rerun-if-changed={}", dependencies.display());
        let prepared = Command::new("python3")
            .arg(&dependencies)
            .args([
                "--radio",
                if env::var_os("CARGO_FEATURE_PICO_SDK_CYW43").is_some() {
                    "pico-sdk-cyw43"
                } else {
                    "embassy-cyw43"
                },
            ])
            .output()
            .expect("prepare firmware dependencies");
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stderr)
        );
        let paths = String::from_utf8(prepared.stdout).expect("dependency paths");
        // Both radio compositions use LittleFS's C string helpers.
        let target = env::var("TARGET").unwrap().replace(['-', '.'], "_");
        let compiler = env::var(format!("CC_{target}")).expect("ARM compiler");
        let flags: &[&str] = if env::var_os("CARGO_FEATURE_RP2040").is_some() {
            &["-mcpu=cortex-m0plus", "-mthumb"]
        } else {
            &[
                "-mcpu=cortex-m33",
                "-mthumb",
                "-mfloat-abi=hard",
                "-mfpu=fpv5-sp-d16",
            ]
        };
        let output = Command::new(compiler)
            .args(flags)
            .arg("-print-file-name=libc_nano.a")
            .output()
            .expect("locate nano libc");
        assert!(output.status.success());
        let libc = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        assert!(libc.is_file(), "ARM nano C library missing");
        println!(
            "cargo:rustc-link-search=native={}",
            libc.parent().unwrap().display()
        );
        println!("cargo:rustc-link-lib=static=c_nano");
        if env::var_os("CARGO_FEATURE_RP2040").is_none() {
            println!("cargo:rustc-link-arg=--wrap=memcpy");
        }
        if env::var_os("CARGO_FEATURE_PICO_SDK_CYW43").is_some() {
            let paths: Vec<_> = paths.lines().collect();
            let native = root.join("tools/pico_radio.py");
            println!("cargo:rerun-if-changed={}", native.display());
            println!(
                "cargo:rerun-if-changed={}",
                root.join("platforms/pico/c").display()
            );
            assert!(
                Command::new("python3")
                    .arg(native)
                    .arg(&config)
                    .arg(&out)
                    .args(paths)
                    .status()
                    .expect("build SDK radio")
                    .success()
            );
            println!("cargo:rustc-link-search={}", out.join("radio").display());
            println!("cargo:rustc-link-lib=static=cordial_radio");
        } else {
            let firmware = PathBuf::from(paths.lines().last().expect("controller firmware path"));
            for (name, file) in [
                ("CYW43_WIFI", "43439A0.bin"),
                ("CYW43_BT", "43439A0_btfw.bin"),
                ("CYW43_NVRAM", "nvram_rp2040.bin"),
            ] {
                let path = firmware.join(file);
                println!("cargo:rerun-if-changed={}", path.display());
                println!("cargo:rustc-env={name}={}", path.display());
            }
        }
    }
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg=-Tlink.x");
    if env::var_os("CARGO_FEATURE_RP2040").is_some() {
        println!("cargo:rustc-link-arg=-Tlink-rp.x");
    }
}
