#[path = "../fingerprint.rs"]
mod fingerprint;
use std::path::PathBuf;

#[test]
fn current_schema_matches_the_recorded_fingerprint() {
    assert_eq!(cordial_protocol::CURRENT_PROTOCOL_VERSION, 1);
    assert_eq!(
        cordial_protocol::OPERATIONAL_SCHEMA_SHA256,
        include_str!("../../../../proto/serial/v1/schema.sha256").trim()
    );
}

#[test]
fn descriptor_normalization_matches_buf_and_includes_dependencies() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fingerprint");
    let original = fingerprint::compile("main.proto", &fixtures);
    assert_eq!(original.file.len(), 3);
    let expected = include_str!("fixtures/fingerprint/schema.sha256").trim();
    assert_eq!(fingerprint::fingerprint(original.clone()), expected);
    let mut renamed = original.clone();
    for file in &mut renamed.file {
        file.name = file.name.as_ref().map(|name| format!("other/{name}"));
        for dependency in &mut file.dependency {
            *dependency = format!("other/{dependency}");
        }
        for location in &mut file.source_code_info.as_mut().unwrap().location {
            location.leading_comments = Some("Different comments and source locations.".into());
            location.span = vec![99];
        }
    }
    renamed.file.reverse();
    assert_eq!(fingerprint::fingerprint(renamed), expected);
    let mut changed = original.clone();
    changed.file[0].message_type[0].field[0].number = Some(9);
    assert_ne!(fingerprint::fingerprint(changed), expected);
    let mut changed = original.clone();
    let main = changed
        .file
        .iter_mut()
        .find(|file| file.name.as_deref() == Some("main.proto"))
        .unwrap();
    main.dependency.reverse();
    assert_ne!(fingerprint::fingerprint(changed), expected);
    let mut changed = original;
    changed.file[0].message_type[0].field[0].r#type = Some(13);
    assert_ne!(fingerprint::fingerprint(changed), expected);
}

#[test]
#[should_panic(expected = "fingerprint normalization requires lossless descriptors")]
fn custom_options_cannot_silently_disappear_from_the_fingerprint() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/options");
    fingerprint::compile("custom.proto", &fixtures);
}
