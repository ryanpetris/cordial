//! Generates the protobuf messages from `proto/cordial.proto` and the key constants from
//! `proto/keys.toml`.
use std::{env, fmt::Write, fs, path::PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
struct Catalog {
    #[serde(default)]
    key: Vec<Key>,
}

#[derive(Deserialize)]
struct Key {
    key: String,
    lists: Vec<String>,
    #[serde(rename = "type")]
    kind: String,
    unit: Option<String>,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    retired: bool,
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let proto = manifest.join("../../../proto");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!(
        "cargo:rerun-if-changed={}",
        proto.join("cordial.proto").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        proto.join("keys.toml").display()
    );

    let descriptors = protox::compile(["cordial.proto"], [&proto]).expect("compile cordial.proto");
    prost_build::Config::new()
        .compile_fds(descriptors.clone())
        .expect("generate messages");
    #[cfg(feature = "json")]
    {
        use prost::Message;
        pbjson_build::Builder::new()
            .register_descriptors(&descriptors.encode_to_vec())
            .expect("register descriptors")
            .preserve_proto_field_names()
            .build(&[".cordial"])
            .expect("generate JSON mapping");
    }

    let catalog: Catalog =
        toml::from_str(&fs::read_to_string(proto.join("keys.toml")).expect("read keys.toml"))
            .expect("parse keys.toml");
    fs::write(out.join("keys.rs"), keys(&catalog)).unwrap();
}

/// Constant names: `pointer.sensor.{n}.dpi` becomes `POINTER_SENSOR_N_DPI`.
fn constant(key: &str) -> String {
    key.replace("{n}", "n")
        .replace(['.', '-'], "_")
        .to_uppercase()
}

fn keys(catalog: &Catalog) -> String {
    let mut code = String::new();
    for key in catalog.key.iter().filter(|k| !k.retired) {
        writeln!(
            code,
            "pub const {}: &str = {:?};",
            constant(&key.key),
            key.key
        )
        .unwrap();
    }
    code.push_str(
        "\n/// Every key in the catalog that is still in use.\npub const KEYS: &[Key] = &[\n",
    );
    for key in catalog.key.iter().filter(|k| !k.retired) {
        let lists = |name| key.lists.iter().any(|l| l == name);
        writeln!(
            code,
            "    Key {{ key: {:?}, adapter: {}, device: {}, setting: {}, kind: Kind::{}, unit: {:?}, values: &{:?} }},",
            key.key,
            lists("adapter"),
            lists("device"),
            lists("setting"),
            match key.kind.as_str() {
                "bool" => "Bool",
                "integer" => "Integer",
                "enum" => "Enum",
                "text" => "Text",
                "color" => "Color",
                other => panic!("unknown key type {other}"),
            },
            key.unit,
            key.values,
        )
        .unwrap();
    }
    code.push_str("];\n");
    code
}
