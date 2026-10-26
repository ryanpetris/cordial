//! Generates messages from the serial and storage protobuf schemas.
use std::{env, path::PathBuf};

mod fingerprint;

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let proto = manifest.join("../../../proto");
    println!("cargo:rerun-if-changed={}", proto.display());
    let version: u32 = std::fs::read_to_string(proto.join("serial/current"))
        .expect("read current protocol revision")
        .trim()
        .parse()
        .expect("nonzero uint32 protocol revision");
    assert_ne!(version, 0);
    let package = format!("cordial.serial.v{version}");
    println!("cargo:rustc-env=CORDIAL_OPERATIONAL_PACKAGE={package}");
    let serial = format!("serial/v{version}/cordial.proto");
    let operational = fingerprint::compile(&serial, &proto);
    let fingerprint = fingerprint::fingerprint(operational);
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("current.rs"), format!(
        "/// The operational protocol revision in this build.\npub const CURRENT_PROTOCOL_VERSION: u32 = {version};\n/// SHA-256 of the normalized operational descriptor set.\npub const OPERATIONAL_SCHEMA_SHA256: &str = \"{fingerprint}\";\n"
    )).expect("write current protocol constants");
    let mut schemas = vec![serial.as_str(), "handshake.proto", "storage/v2/storage.proto"];
    if cfg!(feature = "historical-storage") {
        schemas.push("storage/v1/storage.proto");
    }
    if cfg!(feature = "migration") {
        schemas.push("migration.proto");
    }
    let descriptors = protox::compile(&schemas, [&proto]).expect("compile schemas");
    let mut config = prost_build::Config::new();
    config.type_attribute(
        format!(".{package}.Error"),
        "#[derive(Eq, PartialOrd, Ord)]",
    );
    #[cfg(feature = "json")]
    {
        config.type_attribute(format!(".{package}.Error"), "#[derive(serde::Serialize, serde::Deserialize)] #[serde(default, deny_unknown_fields)]");
        for field in ["code", "reason", "outcome"] {
            config.field_attribute(format!(".{package}.Error.{field}"), format!("#[serde(with = \"crate::error_json::{field}\", skip_serializing_if = \"crate::error_json::is_zero\")]"));
        }
        config.field_attribute(
            format!(".{package}.Error.related"),
            "#[serde(skip_serializing_if = \"alloc::vec::Vec::is_empty\")]",
        );
    }
    config
        .compile_fds(descriptors.clone())
        .expect("generate messages");
    #[cfg(feature = "json")]
    {
        use prost::Message;
        pbjson_build::Builder::new()
            .register_descriptors(&descriptors.encode_to_vec())
            .expect("register descriptors")
            .preserve_proto_field_names()
            .exclude([format!(".{package}.Error")])
            .build(&[".cordial"])
            .expect("generate JSON mapping");
    }
}
