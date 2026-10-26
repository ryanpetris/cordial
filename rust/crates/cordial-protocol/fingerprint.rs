//! Canonical descriptor bytes for serial schema fingerprints.
use prost::Message;
use prost_types::{FileDescriptorProto, FileDescriptorSet};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn normalize(
    name: &str,
    files: &BTreeMap<String, FileDescriptorProto>,
    cache: &mut BTreeMap<String, FileDescriptorProto>,
) -> FileDescriptorProto {
    if let Some(file) = cache.get(name) {
        return file.clone();
    }
    let mut file = files.get(name).expect("included dependency").clone();
    file.name = None;
    file.source_code_info = None;
    for dependency in &mut file.dependency {
        let normalized = normalize(dependency, files, cache);
        *dependency = sha256(&normalized.encode_to_vec());
    }
    cache.insert(name.to_owned(), file.clone());
    file
}

pub fn compile(path: &str, include: &std::path::Path) -> FileDescriptorSet {
    let mut compiler = protox::Compiler::new([include]).expect("schema include directory");
    compiler
        .include_imports(true)
        .include_source_info(true)
        .open_file(path)
        .expect("compile operational schema");
    let descriptors = compiler.file_descriptor_set();
    assert!(
        compiler.encode_file_descriptor_set() == descriptors.encode_to_vec(),
        "fingerprint normalization requires lossless descriptors; custom option extensions are unsupported"
    );
    descriptors
}

pub fn fingerprint(descriptors: FileDescriptorSet) -> String {
    let files: BTreeMap<_, _> = descriptors
        .file
        .into_iter()
        .map(|file| (file.name.clone().unwrap(), file))
        .collect();
    let mut cache = BTreeMap::new();
    let mut descriptors = FileDescriptorSet {
        file: files
            .keys()
            .map(|name| normalize(name, &files, &mut cache))
            .collect(),
    };
    descriptors.file.sort_by_cached_key(Message::encode_to_vec);
    sha256(&descriptors.encode_to_vec())
}
