use cordial_protocol::{
    GetDevice, GetStatus, Request,
    frame::{self, Decoder, FrameError},
    request::Command,
};
use prost::Message;

fn frames(decoder: &mut Decoder, bytes: &[u8]) -> Vec<Result<Vec<u8>, FrameError>> {
    bytes
        .iter()
        .filter_map(|&b| decoder.push(b).map(|r| r.map(<[u8]>::to_vec)))
        .collect()
}

#[test]
fn round_trips_every_block_boundary() {
    let mut decoder = Decoder::new(None);
    for length in [0, 1, 2, 253, 254, 255, 256, 508, 509, 510, 2000] {
        for zeros in [false, true] {
            let bytes: Vec<u8> = (0..length)
                .map(|i| {
                    if zeros && i % 7 == 0 {
                        0
                    } else {
                        (i % 251 + 1) as u8
                    }
                })
                .collect();
            let mut encoded = Vec::new();
            frame::encode_bytes(&bytes, &mut encoded);
            assert_eq!(encoded.last(), Some(&0));
            assert!(!encoded[..encoded.len() - 1].contains(&0));
            let decoded = frames(&mut decoder, &encoded);
            if length == 0 {
                // The encoding of an empty message is a single code byte.
                assert_eq!(decoded, vec![Ok(Vec::new())]);
            } else {
                assert_eq!(decoded, vec![Ok(bytes)]);
            }
        }
    }
}

#[test]
fn skips_empty_frames_and_resynchronizes() {
    let mut decoder = Decoder::new(None);
    let mut stream = vec![9, 9, 9, 0, 0, 0];
    frame::encode(
        &Request {
            command: Some(Command::GetStatus(GetStatus {})),
        },
        &mut stream,
    );
    let decoded = frames(&mut decoder, &stream);
    // The leftover bytes decode as a malformed frame; empty frames are skipped.
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[0], Err(FrameError::Malformed));
    let request = Request::decode(decoded[1].as_ref().unwrap().as_slice()).unwrap();
    assert!(matches!(request.command, Some(Command::GetStatus(_))));
}

#[test]
fn enforces_decoded_and_encoded_limits_and_recovers_at_the_delimiter() {
    let limit = 508;
    let mut decoder = Decoder::new(Some(limit));
    let mut stream = Vec::new();
    frame::encode_bytes(&vec![1; limit], &mut stream);
    frame::encode_bytes(&vec![0; limit + 1], &mut stream);
    stream.extend(vec![0xff; limit + limit / 254 + 2]);
    stream.push(frame::DELIMITER);
    frame::encode_bytes(&[4; 1], &mut stream);
    assert_eq!(
        frames(&mut decoder, &stream),
        vec![
            Ok(vec![1; limit]),
            Err(FrameError::TooLong),
            Err(FrameError::TooLong),
            Ok(vec![4]),
        ]
    );
}

#[test]
fn reset_drops_a_partial_frame() {
    let mut decoder = Decoder::new(None);
    let mut first = Vec::new();
    frame::encode_bytes(b"old", &mut first);
    for &b in &first[..2] {
        assert!(decoder.push(b).is_none());
    }
    decoder.reset();
    let mut second = Vec::new();
    frame::encode_bytes(b"new", &mut second);
    assert_eq!(frames(&mut decoder, &second), vec![Ok(b"new".to_vec())]);
}

#[test]
fn unknown_commands_decode_as_no_command() {
    // Field 99 stands for a command added after this build.
    let mut bytes = Vec::new();
    prost::encoding::message::encode(99, &GetDevice { device: 1 }, &mut bytes);
    let request = Request::decode(bytes.as_slice()).unwrap();
    assert!(request.command.is_none());
}

#[test]
fn unknown_fields_are_ignored() {
    let mut bytes = GetDevice { device: 1 }.encode_to_vec();
    prost::encoding::string::encode(15, &"later".to_string(), &mut bytes);
    assert_eq!(GetDevice::decode(bytes.as_slice()).unwrap().device, 1);
}

#[test]
fn resource_values_preserve_independent_presence_and_explicit_zero() {
    use cordial_protocol::{ResourceCapacity, ResourceType};

    for count in [None, Some(0), Some(7)] {
        for limit in [None, Some(0), Some(4)] {
            let resource = ResourceCapacity {
                r#type: ResourceType::Slots as i32,
                count,
                limit,
            };
            let decoded = ResourceCapacity::decode(resource.encode_to_vec().as_slice()).unwrap();
            assert_eq!(decoded, resource);
        }
    }
}

#[test]
fn typed_information_preserves_unknown_false_zero_and_enum_readings() {
    use cordial_protocol::{
        AdapterInformation, BatteryInformation, DeviceInformation, FunctionRowModeSetting,
        IntegerSetting, KeyboardInformation,
    };

    let adapter = AdapterInformation {
        firmware_version: Some("0.0.0".into()),
        board_name: Some("Board".into()),
        development_build: Some(false),
        storage_format: Some(cordial_protocol::storage::CURRENT_FORMAT),
    };
    assert_eq!(
        AdapterInformation::decode(adapter.encode_to_vec().as_slice()).unwrap(),
        adapter
    );
    let information = DeviceInformation {
        battery: Some(BatteryInformation {
            level: Some(0),
            charging: Some(false),
        }),
        keyboard: Some(KeyboardInformation {
            platform: Some(999),
            function_row: None,
        }),
        ..Default::default()
    };
    assert_eq!(
        DeviceInformation::decode(information.encode_to_vec().as_slice()).unwrap(),
        information
    );
    for value in [None, Some(0), Some(-1)] {
        for saved in [None, Some(0), Some(800)] {
            let setting = IntegerSetting {
                value,
                saved,
                ..Default::default()
            };
            assert_eq!(
                IntegerSetting::decode(setting.encode_to_vec().as_slice()).unwrap(),
                setting
            );
        }
    }
    let setting = FunctionRowModeSetting {
        value: Some(999),
        saved: None,
        status: None,
        choices: vec![1, 2],
    };
    assert_eq!(
        FunctionRowModeSetting::decode(setting.encode_to_vec().as_slice()).unwrap(),
        setting
    );
}

#[test]
fn incremental_encoder_matches_contiguous_encoding_at_every_split() {
    for length in [0, 1, 253, 254, 255, 508, 509, 1024] {
        for zeros in [false, true] {
            let bytes: Vec<_> = (0..length)
                .map(|i| {
                    if zeros && i % 13 == 0 {
                        0
                    } else {
                        (i % 251 + 1) as u8
                    }
                })
                .collect();
            let mut expected = Vec::new();
            frame::encode_bytes(&bytes, &mut expected);
            for split in 0..=length {
                let mut encoder = frame::Encoder::new();
                let mut encoded = Vec::new();
                encoder.push(&bytes[..split], &mut encoded);
                assert!(!encoded.contains(&0));
                encoder.push(&bytes[split..], &mut encoded);
                encoder.finish(&mut encoded);
                assert_eq!(encoded, expected, "length {length}, split {split}");
            }
        }
    }
}

#[test]
fn complete_errors_roundtrip_and_order_by_every_detail_recursively() {
    use cordial_protocol::{Error, ErrorCode, ErrorOutcome, ErrorReason};
    let base = Error {
        code: ErrorCode::Timeout as i32,
        ..Default::default()
    };
    let mut errors = vec![
        Error {
            code: ErrorCode::UnsupportedHid as i32,
            ..base.clone()
        },
        Error {
            related: vec![base.clone(), base.clone()],
            ..base.clone()
        },
        Error {
            related: vec![base.clone()],
            ..base.clone()
        },
        Error {
            outcome: ErrorOutcome::Complete as i32,
            ..base.clone()
        },
        Error {
            outcome: ErrorOutcome::Unknown as i32,
            ..base.clone()
        },
        Error {
            reason: ErrorReason::Radio as i32,
            ..base.clone()
        },
        base.clone(),
        base.clone(),
    ];
    errors.sort();
    errors.dedup();
    assert_eq!(errors.len(), 7);
    assert_eq!(errors[0], base);
    assert_eq!(errors[1].related, core::slice::from_ref(&base));
    assert_eq!(errors[2].related, [base.clone(), base.clone()]);
    assert_eq!(errors[3].outcome(), ErrorOutcome::Unknown);
    assert_eq!(errors[4].outcome(), ErrorOutcome::Complete);
    assert_eq!(errors[5].reason(), ErrorReason::Radio);
    assert_eq!(errors[6].code(), ErrorCode::UnsupportedHid);
    for error in errors {
        assert_eq!(
            Error::decode(error.encode_to_vec().as_slice()).unwrap(),
            error
        );
    }
    let unknown = Error {
        code: 201,
        reason: 202,
        outcome: 203,
        related: vec![base],
    };
    assert_eq!(
        Error::decode(unknown.encode_to_vec().as_slice()).unwrap(),
        unknown
    );
}

#[cfg(feature = "json")]
#[test]
fn json_keeps_generic_error_details_and_nested_failures() {
    use cordial_protocol::{Error, ErrorCode, ErrorOutcome, ErrorReason};
    let error = Error {
        code: ErrorCode::Internal as i32,
        reason: ErrorReason::ScanCandidates as i32,
        outcome: ErrorOutcome::Partial as i32,
        related: vec![Error {
            code: 201,
            reason: 202,
            outcome: 203,
            related: vec![],
        }],
    };
    let json = serde_json::to_string(&error).unwrap();
    assert!(json.contains("ERROR_REASON_SCAN_CANDIDATES"));
    assert!(json.contains("ERROR_OUTCOME_PARTIAL"));
    assert_eq!(serde_json::from_str::<Error>(&json).unwrap(), error);
    let absent = serde_json::from_str::<Error>("{\"code\":\"ERROR_CODE_STORAGE_FAILED\"}").unwrap();
    assert_eq!(absent.outcome(), ErrorOutcome::Unspecified);
}
