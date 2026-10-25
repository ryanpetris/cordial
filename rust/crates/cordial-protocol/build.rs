//! Generates messages from the serial and storage protobuf schemas.
use std::{env, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let proto = manifest.join("../../../proto");
    println!(
        "cargo:rerun-if-changed={}",
        proto.join("cordial.proto").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        proto.join("storage.proto").display()
    );

    let descriptors =
        protox::compile(["cordial.proto", "storage.proto"], [&proto]).expect("compile schemas");
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
}
