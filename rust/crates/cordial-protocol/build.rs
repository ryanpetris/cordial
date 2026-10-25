//! Generates messages from the serial and storage protobuf schemas.
use std::{env, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let proto = manifest.join("../../../proto");
    let mut schemas = vec!["cordial.proto", "storage/v2/storage.proto"];
    if cfg!(feature = "historical-storage") {
        schemas.push("storage/v1/storage.proto");
    }
    if cfg!(feature = "migration") {
        schemas.push("migration.proto");
    }
    for schema in &schemas {
        println!("cargo:rerun-if-changed={}", proto.join(schema).display());
    }
    let descriptors = protox::compile(&schemas, [&proto]).expect("compile schemas");
    let mut config = prost_build::Config::new();
    config.type_attribute(".cordial.Error", "#[derive(Eq, PartialOrd, Ord)]");
    #[cfg(feature = "json")]
    {
        config.type_attribute(".cordial.Error", "#[derive(serde::Serialize, serde::Deserialize)] #[serde(default, deny_unknown_fields)]");
        for field in ["code", "reason", "outcome"] {
            config.field_attribute(format!(".cordial.Error.{field}"), format!("#[serde(with = \"crate::error_json::{field}\", skip_serializing_if = \"crate::error_json::is_zero\")]"));
        }
        config.field_attribute(
            ".cordial.Error.related",
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
            .exclude([".cordial.Error"])
            .build(&[".cordial"])
            .expect("generate JSON mapping");
    }
}
