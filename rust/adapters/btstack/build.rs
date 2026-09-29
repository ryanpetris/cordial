use std::{env, path::PathBuf, process::Command};

const REVISION: &str = "e38553977a25fb0b55b383c72c289be0975f422c";

fn main() {
    println!("cargo:rerun-if-env-changed=CORDIAL_BTSTACK_SOURCE");
    println!("cargo:rerun-if-changed=c");
    if env::var_os("CARGO_FEATURE_FFI").is_none() {
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = env::var_os("CORDIAL_BTSTACK_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../.cache/dependencies/btstack"));
    println!("cargo:rerun-if-changed={}", source.display());
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(args)
            .output()
            .expect("inspect BTstack checkout");
        assert!(
            out.status.success(),
            "Fetch the pinned BTstack dependency before building"
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(
        git(&["rev-parse", "HEAD"]).trim(),
        REVISION,
        "BTstack revision differs from the selected pin"
    );
    assert!(
        git(&["status", "--porcelain", "--untracked-files=all"]).is_empty(),
        "BTstack must be unmodified"
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let generated = Command::new("python3")
        .arg("-B")
        .arg(source.join("tool/compile_gatt.py"))
        .arg("c/host.gatt")
        .arg(output.join("host.h"))
        .output()
        .expect("generate the local ATT database");
    assert!(
        generated.status.success(),
        "ATT generation failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let mut build = cc::Build::new();
    build
        .std("c11")
        .opt_level_str("s")
        .warnings(true)
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .include("c")
        .include(output)
        .include(source.join("src"))
        .include(source.join("3rd-party/micro-ecc"))
        .include(source.join("3rd-party/rijndael"))
        .define("ENABLE_BLE", None);
    for path in [
        "3rd-party/micro-ecc/uECC.c",
        "3rd-party/rijndael/rijndael.c",
        "src/ad_parser.c",
        "src/btstack_crypto.c",
        "src/btstack_hid.c",
        "src/btstack_hid_parser.c",
        "src/btstack_linked_list.c",
        "src/btstack_memory.c",
        "src/btstack_memory_pool.c",
        "src/btstack_run_loop.c",
        "src/btstack_run_loop_base.c",
        "src/btstack_tlv.c",
        "src/btstack_util.c",
        "src/hci.c",
        "src/hci_cmd.c",
        "src/hci_dump.c",
        "src/hci_event.c",
        "src/hci_event_builder.c",
        "src/l2cap.c",
        "src/l2cap_signaling.c",
        "src/ble/att_db.c",
        "src/ble/att_dispatch.c",
        "src/ble/att_server.c",
        "src/ble/gatt_client.c",
        "src/ble/sm.c",
        "src/ble/gatt-service/hids_host.c",
    ] {
        println!("cargo:rerun-if-changed={}", source.join(path).display());
        build.file(source.join(path));
    }
    if env::var_os("CARGO_FEATURE_CLASSIC").is_some() {
        build.define("ENABLE_CLASSIC", None);
        for path in [
            "src/classic/hid_host.c",
            "src/classic/sdp_client.c",
            "src/classic/sdp_util.c",
        ] {
            println!("cargo:rerun-if-changed={}", source.join(path).display());
            build.file(source.join(path));
        }
    }
    build
        .files([
            "c/runtime.c",
            "c/profiles.c",
            "c/gatt.c",
            "c/information.c",
            "c/bond_db.c",
        ])
        .compile("cordial_btstack");
    if env::var("TARGET").unwrap().starts_with("thumb") {
        let output = build
            .get_compiler()
            .to_command()
            .arg("-print-libgcc-file-name")
            .output()
            .expect("compiler runtime path");
        assert!(output.status.success(), "locate ARM compiler runtime");
        let library = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        assert!(library.is_file(), "ARM compiler runtime missing");
        println!(
            "cargo:rustc-link-search=native={}",
            library.parent().unwrap().display()
        );
        println!("cargo:rustc-link-lib=static=gcc");
    }
}
