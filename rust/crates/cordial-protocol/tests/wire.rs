use cordial_protocol::{
    codec::{self, FrameError, Framer},
    identifiers::*,
    messages::*,
    settings::*,
};

#[test]
fn requests_match_current_commands_and_accept_field_order_and_escapes() {
    let request = codec::decode_request(br#"{"args":{"value":false,"key":"backlight.enabled","device_id":"d_0000000000000001"},"cmd":"hidpp.setting.s\u0065t","id":7,"v":1}"#).unwrap();
    assert!(matches!(
        request.command,
        Command::SettingsSet(SettingSet {
            value: SettingValue::Bool(false),
            ..
        })
    ));
    let bytes = codec::encode(&request).unwrap();
    assert_eq!(
        codec::decode_request(&bytes[..bytes.len() - 1]).unwrap(),
        request
    );
    let request =
        codec::decode_request(br#"{"v":1,"id":1,"cmd":"discovery.scan","args":{}}"#).unwrap();
    assert!(matches!(
        request.command,
        Command::Scan(Scan {
            transport: ScanTransport::Both,
            duration_ms: 10_000
        })
    ));
    assert!(
        codec::decode_request(
            br#"{"v":1,"id":1,"cmd":"discovery.scan","args":{"duration_ms":10}}"#
        )
        .is_err()
    );
    assert!(
        codec::decode_request(br#"{"v":1,"id":1,"cmd":"adapter.status","args":{"unknown":1}}"#)
            .is_err()
    );
    assert!(codec::decode_request(br#"{"v":1,"id":0,"cmd":"adapter.status","args":{}}"#).is_err());
    assert!(codec::decode_request(br#"{"v":2,"id":1,"cmd":"adapter.status","args":{}}"#).is_err());
}

#[test]
fn request_error_categories_reject_duplicates_in_complete_requests() {
    use codec::DecodeError;
    assert!(matches!(
        codec::decode_request(b"{"),
        Err(DecodeError::Json(_))
    ));
    for bytes in [
        br#"{"v":1,"v":1,"id":1,"cmd":"adapter.status","args":{}}"#.as_slice(),
        br#"{"v":1,"id":1,"id":1,"cmd":"adapter.status","args":{}}"#,
        br#"{"v":1,"id":1,"cmd":"adapter.status","cmd":"adapter.status","args":{}}"#,
        br#"{"v":1,"id":1,"cmd":"adapter.status","args":{},"args":{}}"#,
        br#"{"v":1,"id":1,"cmd":"adapter.status","args":{},"\u0061rgs":{}}"#,
        br#"{"v":1,"cmd":"adapter.status","args":{}}"#,
    ] {
        assert!(matches!(
            codec::decode_request(bytes),
            Err(DecodeError::InvalidRequest { .. })
        ));
    }
    assert!(matches!(
        codec::decode_request(
            br#"{"v":1,"id":1,"cmd":"device.list","args":{"filter":"paired","filter":"connected"}}"#
        ),
        Err(DecodeError::Arguments { .. })
    ));
    assert!(codec::decode_request(br#"{"v":1,"id":1,"cmd":"adapter.status","args":{}}"#).is_ok());
    assert!(
        codec::decode_request(br#"{"v":1,"id":1,"cmd":"device.list","args":{"filter":"paired"}}"#)
            .is_ok()
    );
}

#[test]
fn request_envelopes_must_be_objects() {
    use codec::DecodeError;
    for bytes in [
        br#"[1,5,"adapter.status",{}]"#.as_slice(),
        br#"[1,5,"device.list",{"filter":"connected"}]"#,
        b"[7]",
    ] {
        assert!(matches!(
            codec::decode_request(bytes),
            Err(DecodeError::InvalidRequest { supplied: None })
        ));
    }
    assert!(matches!(
        codec::decode_request(b"["),
        Err(DecodeError::Json(_))
    ));
    assert!(
        codec::decode_request(b" \t\r\n{\"v\":1,\"id\":5,\"cmd\":\"adapter.status\",\"args\":{}}")
            .is_ok()
    );
}

#[test]
fn setting_values_and_unknown_feature_ids_round_trip_without_new_controls() {
    for value in [
        SettingValue::Null,
        SettingValue::Bool(true),
        SettingValue::Integer(1600),
        SettingValue::Text("special_actions".into()),
    ] {
        let bytes = codec::encode(&value).unwrap();
        assert_eq!(
            codec::decode::<SettingValue>(&bytes[..bytes.len() - 1]).unwrap(),
            value
        );
    }
    assert!(codec::decode::<SettingValue>(b"1.5").is_err());
    assert!(codec::decode::<SettingValue>(b"[]").is_err());
    let feature: cordial_protocol::hidpp::Feature =
        codec::decode(br#"{"index":12,"id":65534,"version":6,"flags":64,"supported":false}"#)
            .unwrap();
    assert_eq!(feature.id.0, 65534);
    assert_eq!(feature.index.0, 12);
    assert!(feature.flags.device_hidden());
    assert!(!feature.supported);
}

#[test]
fn framing_survives_split_and_overlong_lines_without_losing_next_command() {
    let mut storage = [0; 16];
    let mut framer = Framer::new(&mut storage);
    let mut lines = Vec::new();
    for piece in [
        b"hel".as_slice(),
        b"lo\nsecond\n",
        b"01234567890123456789\nlast\n",
    ] {
        for &byte in piece {
            if let Some(line) = framer.push(byte) {
                lines.push(line.map(Vec::from));
            }
        }
    }
    assert_eq!(
        lines,
        vec![
            Ok(b"hello".to_vec()),
            Ok(b"second".to_vec()),
            Err(FrameError::TooLong),
            Ok(b"last".to_vec())
        ]
    );
}

#[test]
fn correlated_results_and_events_keep_wire_envelopes() {
    let id = RequestId::try_from(11).unwrap();
    let message: Message<Enabled, serde_json::Value> =
        Message::success(id, Enabled { enabled: true }, true);
    let encoded = codec::encode(&message).unwrap();
    let object: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(object["type"], "response");
    assert_eq!(object["id"], 11);
    assert_eq!(object["ok"], true);
    assert_eq!(object["done"], true);
    assert!(object.get("error").is_none());
    assert_eq!(
        codec::decode::<Message<Enabled, serde_json::Value>>(&encoded[..encoded.len() - 1])
            .unwrap(),
        message
    );
}

#[test]
fn bounded_encoder_preserves_wire_values_and_line_limit() {
    fn equivalent<T: serde::Serialize>(value: &T) {
        let encoded = codec::encode(value).unwrap();
        assert_eq!(encoded.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encoded).unwrap(),
            serde_json::to_value(value).unwrap()
        );
    }
    let id = RequestId::try_from(7).unwrap();
    let data = serde_json::json!({
        "revision": u64::MAX, "text": "\"\\\u{0000}\n\t\r\u{001f}é\u{10400}",
        "absent": null, "values": [true, false, -123, 0, 123]
    });
    equivalent(&Message::<_, ()>::success(id, &data, false));
    equivalent(&Message::<(), _>::event(
        "device.changed".into(),
        Some(id),
        &data,
    ));
    equivalent(&Message::<(), _>::event(
        "device.changed".into(),
        None,
        &data,
    ));
    equivalent(&Message::<(), ()>::failure(
        id,
        WireError {
            code: cordial_protocol::errors::ErrorCode::Capacity,
            details: Some(cordial_protocol::payloads::ErrorDetails::Capacity(
                cordial_protocol::payloads::CapacityDetails {
                    reason: cordial_protocol::errors::CapacityReason::EnabledFull,
                },
            )),
        },
    ));
    #[derive(serde::Serialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    enum Ready {
        Initializing,
        Ready { status: serde_json::Value },
    }
    equivalent(&Ready::Initializing);
    equivalent(&Ready::Ready {
        status: serde_json::json!({"storage_ready": true}),
    });
    let text = "x".repeat(cordial_protocol::MAX_LINE_BYTES - 3);
    assert_eq!(
        codec::encode(&text).unwrap().len(),
        cordial_protocol::MAX_LINE_BYTES
    );
    assert_eq!(
        codec::encode(&(text + "x")),
        Err(codec::EncodeError::TooLong)
    );
}

#[test]
fn command_failures_keep_correlation_and_envelopes_are_strict() {
    use codec::DecodeError;
    let id = RequestId::try_from(7).unwrap();
    assert!(matches!(
        codec::decode_request(br#"{"v":1,"id":7,"cmd":"device.list","args":{}}"#)
            .unwrap()
            .command,
        Command::Devices(DeviceFilter {
            filter: Filter::Saved
        })
    ));
    assert!(
        matches!(codec::decode_request(br#"{"v":1,"id":7,"cmd":"unknown","args":{}}"#), Err(DecodeError::UnknownCommand { id: found }) if found == id)
    );
    assert!(
        matches!(codec::decode_request(br#"{"v":1,"id":7,"cmd":"adapter.status","args":{"unused":true}}"#), Err(DecodeError::Arguments { id: found }) if found == id)
    );
    assert!(matches!(
        codec::decode_request(br#"{"v":1,"id":7,"cmd":"adapter.status","args":{},"extra":true}"#),
        Err(DecodeError::InvalidRequest { .. })
    ));
    assert!(
        matches!(codec::decode_request(br#"{"v":2,"id":7,"cmd":"unknown","args":{}}"#), Err(DecodeError::Version { id: found }) if found == id)
    );
    assert!(matches!(
        codec::decode_request(br#"{"v":1,"id":7,"cmd":"adapter.status","args":[]}"#),
        Err(DecodeError::InvalidRequest { .. })
    ));
    let request = codec::decode_request(
        br#"{"v":1,"id":7,"cmd":"hidpp.setting.get","args":{"device_id":"d_1","key":"foo.bar"}}"#,
    )
    .unwrap();
    assert!(
        matches!(request.command, Command::SettingsGet(SettingRef {key, ..}) if key == "foo.bar")
    );
    let mut buffer = [0; 100];
    let mut framer = Framer::new(&mut buffer);
    let mut lines = Vec::new();
    for byte in b"\n\r\nhello\r\n" {
        if let Some(line) = framer.push(*byte) {
            lines.push(line.unwrap().to_vec());
        }
    }
    assert_eq!(lines, [b"hello".to_vec()]);
}

#[test]
fn enablement_commands_and_capacity_status_use_current_wire_names() {
    use cordial_protocol::errors::*;
    for enabled in [true, false] {
        let line = format!(
            r#"{{"v":1,"id":3,"cmd":"device.enabled.set","args":{{"device_id":"d_1","enabled":{enabled}}}}}"#
        );
        let request = codec::decode_request(line.as_bytes()).unwrap();
        assert_eq!(
            request.command,
            Command::DeviceEnabled(DeviceEnabled {
                device_id: DeviceId("d_1".into()),
                enabled
            })
        );
        assert_eq!(request.command.name(), "device.enabled.set");
    }
    let capacity: Capacity = serde_json::from_str(
        r#"{"enabled":[{"transports":["classic","ble"],"limit":7,"enabled":2},{"transports":["ble"],"limit":3,"enabled":3}],
            "pairing":[{"transport":"ble","available":false,"reason":"connections_full","estimated_additional":12}]}"#,
    )
    .unwrap();
    assert_eq!(
        capacity.pairing[0].reason,
        Some(PairUnavailable::ConnectionsFull)
    );
    let mut status: Status = serde_json::from_value(serde_json::json!({
        "protocol":1,"firmware_version":"x","hardware_config":"x","hardware_digest":"x",
        "radio_backend":"x","adapter_id":"x","build_profile":"development","boot_id":"b",
        "session_id":"s",
        "limits":{"max_line_bytes":4096,"max_pending_requests":4,"saved_devices":64,
            "active_connections":4,"scan_candidates":16,"hidpp_settings":32,
            "hidpp_saved_settings":16,"hidpp_sensors":2,"hidpp_firmware_entities":2,
            "hidpp_setting_choices":16,"hidpp_features":256},
        "counts":{"saved":3,"paired":3,"preferred_enabled":2,"enabled":2,"connected":1},
        "capacity":{"enabled":[],"pairing":[]},"revision":1,"name": "Test adapter", "host_platform":"linux",
        "monitor":false,"radio_ready":true,"storage_ready":true,
        "heartbeat":{"interval_ms":5000,"timeout_ms":15000,"remaining_ms":1},"pending":[]
    }))
    .unwrap();
    assert!(
        Capabilities(vec![Capability::Classic, Capability::Ble])
            .supports_command(CommandId::DeviceEnabled)
    );
    assert_eq!(status.enabled_remaining(Transport::Ble), None);
    status.capacity = capacity;
    assert_eq!(status.enabled_remaining(Transport::Ble), Some(0));
    assert_eq!(status.enabled_remaining(Transport::Classic), Some(5));
    assert!(status.pairing(Transport::Classic).is_none());
    for (code, text) in [
        (ErrorCode::Disabled, "\"disabled\""),
        (ErrorCode::StorageFull, "\"storage_full\""),
    ] {
        assert_eq!(serde_json::to_string(&code).unwrap(), text);
    }
    assert!(ValidationError::BondMismatch.needs_pairing());
    assert!(!ValidationError::ReadFailed.needs_pairing());
    assert_eq!(
        serde_json::to_string(&DisabledReason::UnsupportedTransport).unwrap(),
        "\"unsupported_transport\""
    );
    assert_eq!(
        serde_json::from_str::<CapacityReason>("\"enabled_full\"").unwrap(),
        CapacityReason::EnabledFull
    );
    assert_eq!(
        serde_json::from_str::<StorageOutcome>("\"unknown\"").unwrap(),
        StorageOutcome::Unknown
    );
}

#[test]
fn setting_key_codes_are_frozen() {
    // Persisted setting-record keys use these codes; changing one orphans saved preferences.
    let codes: Vec<(SettingKey, u8)> = SettingKey::ALL.iter().map(|k| (*k, *k as u8)).collect();
    assert_eq!(
        codes.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
        [
            7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 22, 23, 24, 25, 26, 27, 28
        ]
    );
    for (key, code) in [
        (SettingKey::BacklightEnabled, 8),
        (SettingKey::WheelMode, 16),
    ] {
        assert_eq!(key as u8, code, "{key:?}");
    }
}

#[test]
fn largest_information_snapshot_fits_one_response_and_contains_no_provider_fields() {
    use cordial_protocol::info::{DeviceInfo, InfoField, InfoKey, MAX_TEXT};
    use cordial_protocol::settings::SettingValue as V;
    let fields = InfoKey::ALL
        .into_iter()
        .flat_map(|key| {
            (0..key.instances()).map(move |instance| {
                let value = match key {
                    InfoKey::BatteryPercent => V::Integer(100),
                    InfoKey::BatteryCharging => V::Bool(true),
                    InfoKey::Kind => V::Text("keyboard_mouse".into()),
                    InfoKey::VendorIdNamespace => V::Text("bluetooth".into()),
                    InfoKey::VendorId | InfoKey::ProductId | InfoKey::ProductVersion => {
                        V::Integer(65535)
                    }
                    _ => V::Text("\\".repeat(MAX_TEXT)),
                };
                InfoField {
                    key,
                    instance,
                    value,
                    available: true,
                    fresh: true,
                }
            })
        })
        .collect::<Vec<_>>();
    assert!(fields.iter().all(InfoField::valid));
    let payload = DeviceInfo {
        revision: cordial_protocol::MAX_REVISION,
        device_id: cordial_protocol::identifiers::DeviceId("d_0000000000000001".into()),
        fields,
    };
    let envelope = serde_json::json!({"v":1,"id":2147483647,"type":"response","ok":true,"done":true,"result":payload});
    let encoded = cordial_protocol::codec::encode(&envelope).unwrap();
    let s = std::str::from_utf8(&encoded).unwrap();
    for forbidden in ["hidpp", "uuid", "feature", "provider", "source"] {
        assert!(!s.contains(forbidden));
    }
}
