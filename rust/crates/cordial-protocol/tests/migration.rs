use cordial_protocol::{
    DeleteFile, ListFiles, MAX_REQUEST_BYTES, MoveFile, ReadFile, Request, Restart,
    STORAGE_MIGRATION_CHUNK_BYTES, STORAGE_MIGRATION_PATH_BYTES, WriteFile, request,
};
use prost::Message;

#[test]
fn bounded_file_requests_fit_the_serial_limit() {
    let path = format!("/{}", "a".repeat(STORAGE_MIGRATION_PATH_BYTES - 1));
    for command in [
        request::Command::ListFiles(ListFiles { path: path.clone() }),
        request::Command::ReadFile(ReadFile {
            path: path.clone(),
            offset: u64::MAX,
            length: Some(STORAGE_MIGRATION_CHUNK_BYTES as u32),
        }),
        request::Command::WriteFile(WriteFile {
            path: path.clone(),
            offset: u64::MAX,
            data: vec![0xff; STORAGE_MIGRATION_CHUNK_BYTES],
        }),
        request::Command::MoveFile(MoveFile {
            source: path.clone(),
            destination: path.clone(),
        }),
        request::Command::DeleteFile(DeleteFile { path }),
        request::Command::Restart(Restart {}),
    ] {
        let request = Request {
            command: Some(command),
        };
        assert!(request.encoded_len() <= MAX_REQUEST_BYTES);
        assert_eq!(Request::decode(&*request.encode_to_vec()).unwrap(), request);
    }
}

#[test]
fn target_storage_format_preserves_absence_and_explicit_zero() {
    for target_storage_format in [None, Some(0), Some(2)] {
        let information = cordial_protocol::AdapterInformation {
            target_storage_format,
            ..Default::default()
        };
        let bytes = information.encode_to_vec();
        assert_eq!(
            bytes,
            match target_storage_format {
                None => vec![],
                Some(value) => vec![0x28, value as u8],
            }
        );
        assert_eq!(
            cordial_protocol::AdapterInformation::decode(&*bytes)
                .unwrap()
                .target_storage_format,
            target_storage_format
        );
    }
}

#[test]
fn bootstrap_status_reports_committed_and_target_formats_independently_of_readiness() {
    for storage_format in [None, Some(1), Some(2), Some(99)] {
        let status = cordial_protocol::Status {
            ready: false,
            information: Some(cordial_protocol::AdapterInformation {
                storage_format,
                target_storage_format: Some(2),
                ..Default::default()
            }),
            errors: vec![cordial_protocol::Error {
                code: cordial_protocol::ErrorCode::StorageMigrationRequired.into(),
                reason: cordial_protocol::ErrorReason::Storage.into(),
                outcome: cordial_protocol::ErrorOutcome::NoEffect.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            cordial_protocol::Status::decode(&*status.encode_to_vec()).unwrap(),
            status
        );
    }
}

#[cfg(feature = "historical-storage")]
#[test]
fn root_selects_v1_even_when_sequence_looks_like_a_v2_header() {
    use cordial_protocol::storage::{self, v1};
    let root = v1::Format {
        format: 1,
        initialized: true,
    }
    .encode_to_vec();
    let sequence = v1::Sequence {
        next_device: 2,
        next_profile: 1,
    }
    .encode_to_vec();
    assert_eq!(sequence, [8, 2, 16, 1]);
    assert_eq!(storage::extract_format(&root), Ok(1));
    // A payload header scan alone cannot distinguish these versions.
    assert_eq!(storage::extract_format(&sequence), Ok(2));
    assert_eq!(v1::Sequence::decode(&*sequence).unwrap().next_device, 2);
    assert_eq!(
        storage::validate_format(&root),
        Err(storage::FormatError::Unsupported(1))
    );
}

#[cfg(feature = "historical-storage")]
#[test]
fn completed_v2_sequence_collides_with_a_loose_v1_decode() {
    use cordial_protocol::storage::{v1, v2};
    let bytes = [0x08, 0x02, 0x10, 0x05, 0x18, 0x07];
    let completed = v2::Sequence::decode(&bytes[..]).unwrap();
    assert_eq!(completed.format, 2);
    assert_eq!(completed.next_device, 5);
    assert_eq!(completed.next_profile, 7);
    assert_eq!(completed.encode_to_vec(), bytes);
    // A loose v1 decode silently ignores outer tag 3 and changes the IDs.
    let loose = v1::Sequence::decode(&bytes[..]).unwrap();
    assert_eq!(loose.next_device, 2);
    assert_eq!(loose.next_profile, 5);
    assert_ne!(loose.encode_to_vec(), bytes);
}

#[cfg(feature = "historical-storage")]
#[test]
fn v1_payloads_shift_only_outer_tags_and_keep_nested_bytes() {
    use cordial_protocol::storage::{self, v1, v2};
    use prost::encoding::{DecodeContext, decode_key, encode_key, skip_field};

    fn shifted(old: impl Message) -> Vec<u8> {
        let bytes = old.encode_to_vec();
        let mut input = bytes.as_slice();
        let mut output = vec![8, 2];
        while !input.is_empty() {
            let (tag, wire) = decode_key(&mut input).unwrap();
            let start = input;
            skip_field(wire, tag, &mut input, DecodeContext::default()).unwrap();
            encode_key(tag + 1, wire, &mut output);
            output.extend_from_slice(&start[..start.len() - input.len()]);
        }
        output
    }
    fn nested<T: Message + Default>(old: impl Message) -> T {
        T::decode(&*old.encode_to_vec()).unwrap()
    }
    macro_rules! same {
        ($old:expr, $new:expr) => {
            assert_eq!(shifted($old), $new.encode_to_vec());
        };
    }
    let format = storage::CURRENT_FORMAT;
    same!(
        v1::Identity {
            address: vec![1; 6],
            ir: vec![2; 16],
            er: vec![3; 16],
            irk: vec![4; 16]
        },
        v2::Identity {
            format,
            address: vec![1; 6],
            ir: vec![2; 16],
            er: vec![3; 16],
            irk: vec![4; 16]
        }
    );
    let transport = v1::TransportPreference {
        transport: 2,
        enabled: true,
    };
    let interface = v1::InterfacePreference {
        interface: 1,
        enabled: true,
        profile: Some(7),
    };
    same!(
        v1::Adapter {
            name: Some("Adapter".into()),
            host_platform: 2,
            transports: vec![transport],
            configuration_interfaces: vec![interface]
        },
        v2::Adapter {
            format,
            name: Some("Adapter".into()),
            host_platform: 2,
            transports: vec![nested(transport)],
            configuration_interfaces: vec![nested(interface)]
        }
    );
    same!(
        v1::Sequence {
            next_device: 2,
            next_profile: 7
        },
        v2::Sequence {
            format,
            next_device: 2,
            next_profile: 7
        }
    );
    let policy = v1::Policy {
        id: 5,
        name: "Device".into(),
        trusted: true,
        ..Default::default()
    };
    let bond = v1::Bond {
        owner: 5,
        ..Default::default()
    };
    same!(
        v1::Device {
            policy: Some(policy.clone()),
            bond: Some(bond.clone())
        },
        v2::Device {
            format,
            policy: Some(nested(policy)),
            bond: Some(nested(bond))
        }
    );
    let preference = v1::Preference {
        integration: 1,
        value: 9,
        ..Default::default()
    };
    same!(
        v1::Settings {
            preferences: vec![preference.clone()]
        },
        v2::Settings {
            format,
            preferences: vec![nested(preference)]
        }
    );
    let report = v1::Report {
        service: 1,
        r#type: 1,
        id: 2,
        value_handle: 3,
        properties: 4,
        cccd_handle: 5,
    };
    same!(
        v1::Layout {
            maps: vec![vec![1, 2, 3]],
            reports: vec![report],
            database_hash: vec![4; 16]
        },
        v2::Layout {
            format,
            maps: vec![vec![1, 2, 3]],
            reports: vec![nested(report)],
            database_hash: vec![4; 16]
        }
    );
    same!(
        v1::Profile {
            name: "Profile".into(),
            roles: vec![1, 2]
        },
        v2::Profile {
            format,
            name: "Profile".into(),
            roles: vec![1, 2]
        }
    );
    let rule = v1::Rule {
        input: 0x70004,
        collection: 0x10006,
        effect: Some(v1::rule::Effect::Remap(v1::Remap {
            outputs: vec![v1::Output {
                usage: 0x70005,
                collection: 0x10006,
            }],
        })),
    };
    same!(
        v1::Rules {
            rules: vec![rule.clone()]
        },
        v2::Rules {
            format,
            rules: vec![nested(rule)]
        }
    );
    assert_eq!(
        v1::Format {
            format: 1,
            initialized: true
        }
        .encode_to_vec(),
        [8, 1, 16, 1]
    );
    assert_eq!(
        v2::Format {
            format,
            initialized: true
        }
        .encode_to_vec(),
        [8, 2, 16, 1]
    );
}

#[cfg(feature = "migration")]
#[test]
fn ipc_control_and_event_round_trip_without_serial_name_collisions() {
    use cordial_protocol::migration::{self, migration_control::Action, migration_event::Event};
    let control = migration::MigrationControl {
        action: Some(Action::Start(migration::MigrationStart {
            adapter_id: "0123456789ABCDEF".into(),
            port_hint: String::new(),
        })),
    };
    assert_eq!(
        migration::MigrationControl::decode(&*control.encode_to_vec()).unwrap(),
        control
    );
    let event = migration::MigrationEvent {
        event: Some(Event::Success(migration::MigrationSuccess {
            adapter_id: "0123456789ABCDEF".into(),
            target_format: 2,
        })),
    };
    let bytes = event.encode_to_vec();
    assert!(bytes.len() <= migration::MAX_FRAME_BYTES);
    let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(&bytes);
    assert_eq!(
        u32::from_le_bytes(framed[..4].try_into().unwrap()) as usize,
        bytes.len()
    );
    assert_eq!(
        migration::MigrationEvent::decode(&framed[4..]).unwrap(),
        event
    );
}
