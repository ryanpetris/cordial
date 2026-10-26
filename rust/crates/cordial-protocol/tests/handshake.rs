use cordial_protocol::{GetStatus, Request, frame, handshake::*, request::Command};
use prost::Message;

#[test]
fn shared_wire_fixtures() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/handshake.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let bytes: Vec<u8> = serde_json::from_value(case["bytes"].clone()).unwrap();
        for (actual, error) in [
            (
                decode_client_hello(&bytes).map(|_| ()),
                &case["client_error"],
            ),
            (
                decode_adapter_hello(&bytes).map(|hello| {
                    assert_eq!(hello.protocol_version, Some(1));
                }),
                &case["adapter_error"],
            ),
        ] {
            let expected = match error.as_str() {
                None => Ok(()),
                Some("missing_version") => Err(HandshakeError::MissingVersion),
                Some("zero_version") => Err(HandshakeError::ZeroVersion),
                Some("duplicate_version") => Err(HandshakeError::DuplicateVersion),
                Some("invalid_version") => Err(HandshakeError::InvalidVersion),
                Some("malformed") => Err(HandshakeError::Malformed),
                other => panic!("unknown error {other:?}"),
            };
            assert_eq!(actual, expected, "{case}");
        }
    }
}

#[test]
fn client_hello_is_an_empty_query_not_a_bare_delimiter_or_request() {
    let hello = ClientHello {};
    let bytes = encode_client_hello(&hello).unwrap();
    assert!(bytes.is_empty());
    assert_eq!(decode_client_hello(&bytes), Ok(hello));
    let mut encoded = Vec::new();
    frame::encode_bytes(&bytes, &mut encoded);
    assert_eq!(encoded, [1, 0]);

    let request = Request {
        command: Some(Command::GetStatus(GetStatus {})),
    }
    .encode_to_vec();
    assert!(!request.is_empty());
    assert_eq!(
        decode_client_hello(&request),
        Err(HandshakeError::Malformed)
    );
    let mut decoder = frame::Decoder::new(Some(MAX_HANDSHAKE_BYTES));
    assert!(decoder.push(0).is_none());
    assert!(decoder.push(1).is_none());
    assert_eq!(decoder.push(0), Some(Ok([].as_slice())));
    let mut stream = vec![0, 0];
    frame::encode_bytes(&request, &mut stream);
    stream.extend([0, 1, 0, 0]);
    let frames: Vec<_> = stream
        .into_iter()
        .filter_map(|byte| decoder.push(byte).map(|result| result.map(<[u8]>::to_vec)))
        .collect();
    assert_eq!(frames, [Ok(request), Ok(Vec::new())]);
}

#[test]
fn adapter_versions_metadata_and_unknown_fields_roundtrip() {
    for version in [1, 2, u32::MAX] {
        let adapter = AdapterHello {
            protocol_version: Some(version),
            ..Default::default()
        };
        assert_eq!(
            decode_adapter_hello(&encode_adapter_hello(&adapter).unwrap()),
            Ok(adapter)
        );
    }
    let adapter = AdapterHello {
        protocol_version: Some(CURRENT_PROTOCOL_VERSION),
        firmware_version: Some("0.0.0".into()),
        serial_number: Some("example".into()),
        name: Some(String::new()),
    };
    assert_eq!(
        decode_adapter_hello(&encode_adapter_hello(&adapter).unwrap()),
        Ok(adapter)
    );
    // Unknown bytes, varint, fixed32, fixed64 and a group contain no top-level version.
    let unknown = [
        42, 2, 8, 99, 48, 2, 61, 0, 0, 0, 0, 65, 0, 0, 0, 0, 0, 0, 0, 0, 75, 8, 99, 76,
    ];
    for bytes in [
        [unknown.as_slice(), &[8, 1]].concat(),
        [&[8, 1], unknown.as_slice()].concat(),
    ] {
        assert_eq!(
            decode_adapter_hello(&bytes).unwrap().protocol_version,
            Some(1)
        );
    }
    let reordered = [34, 1, b'A', 26, 1, b'B', 18, 1, b'C', 8, 1];
    let adapter = decode_adapter_hello(&reordered).unwrap();
    assert_eq!(adapter.name.as_deref(), Some("A"));
    assert_eq!(adapter.serial_number.as_deref(), Some("B"));
    assert_eq!(adapter.firmware_version.as_deref(), Some("C"));
}

#[test]
fn metadata_wire_types_and_utf8_are_validated() {
    for bytes in [&[8, 1, 18, 1, 0xff][..], &[8, 1, 16, 1]] {
        assert_eq!(decode_adapter_hello(bytes), Err(HandshakeError::Malformed));
    }
}

#[test]
fn handshake_limits_apply_to_encoding_decoding_and_frames() {
    let mut adapter = AdapterHello {
        protocol_version: Some(1),
        name: Some("a".repeat(MAX_HANDSHAKE_BYTES - 5)),
        ..Default::default()
    };
    let bytes = encode_adapter_hello(&adapter).unwrap();
    assert_eq!(bytes.len(), MAX_HANDSHAKE_BYTES);
    assert!(decode_adapter_hello(&bytes).is_ok());
    let mut encoded = Vec::new();
    frame::encode_bytes(&bytes, &mut encoded);
    let mut decoder = frame::Decoder::new(Some(MAX_HANDSHAKE_BYTES));
    let result = encoded
        .into_iter()
        .filter_map(|byte| decoder.push(byte).map(|result| result.map(<[u8]>::to_vec)))
        .last()
        .unwrap();
    assert_eq!(result.unwrap(), bytes);
    adapter.name.as_mut().unwrap().push('a');
    assert_eq!(encode_adapter_hello(&adapter), Err(HandshakeError::TooLong));
    let bytes = adapter.encode_to_vec();
    assert_eq!(decode_adapter_hello(&bytes), Err(HandshakeError::TooLong));
    assert_eq!(decode_client_hello(&bytes), Err(HandshakeError::TooLong));
    let mut encoded = Vec::new();
    frame::encode_bytes(&bytes, &mut encoded);
    assert_eq!(
        encoded
            .into_iter()
            .filter_map(|byte| decoder.push(byte).map(|result| result.map(<[u8]>::to_vec)))
            .last()
            .unwrap(),
        Err(frame::FrameError::TooLong)
    );
    assert_eq!(
        encode_adapter_hello(&AdapterHello::default()),
        Err(HandshakeError::MissingVersion)
    );
    assert_eq!(
        encode_adapter_hello(&AdapterHello {
            protocol_version: Some(0),
            ..Default::default()
        }),
        Err(HandshakeError::ZeroVersion)
    );
    let mut nested = vec![8, 1];
    nested.extend([43].repeat(101));
    nested.extend([44].repeat(101));
    assert_eq!(
        decode_adapter_hello(&nested),
        Err(HandshakeError::Malformed)
    );
}
