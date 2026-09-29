use cordial_protocol::{codec, messages::COMMANDS};
use cordial_schema::{assert_valid, generate, validator};
use serde_json::{Value, json};

fn args(name: &str) -> Value {
    match name {
        "adapter.name.set" => json!({"name":"Test adapter"}),
        "adapter.platform.set" => json!({"platform":"linux"}),
        "session.monitor.set" => json!({"enabled":true}),
        "pairing.start" => json!({"candidate_id":"c_1"}),
        "pairing.reply" => {
            json!({"request_id":2,"prompt_id":"p_1","action":"accept","value":"123456"})
        }
        "request.cancel" => json!({"request_id":2}),
        "storage.list" | "storage.read" => json!({"path":"/devices/d_1/device.json"}),
        "device.enabled.set" | "device.hidpp.set" => json!({"device_id":"d_1","enabled":true}),
        "device.trusted.set" => json!({"device_id":"d_1","trusted":true}),
        "device.blocked.set" => json!({"device_id":"d_1","blocked":false}),
        "hidpp.setting.get" | "hidpp.setting.forget" => {
            json!({"device_id":"d_1","key":"backlight.enabled"})
        }
        "hidpp.setting.set" => json!({"device_id":"d_1","key":"backlight.enabled","value":true}),
        "device.info"
        | "device.info.refresh"
        | "device.get"
        | "device.connect"
        | "device.disconnect"
        | "device.unpair"
        | "hidpp.feature.list"
        | "hidpp.setting.list"
        | "hidpp.setting.refresh"
        | "hidpp.setting.apply" => json!({"device_id":"d_1"}),
        _ => json!({}),
    }
}
fn request(name: &str, args: Value) -> Value {
    json!({"v":if name == "adapter.protocol" { 0 } else { 1 },"id":1,"cmd":name,"args":args})
}
fn response(result: Value, done: bool) -> Value {
    json!({"v":1,"type":"response","id":1,"ok":true,"done":done,"result":result})
}
fn device() -> Value {
    json!({"device_id":"d_1","pairing_state":"paired","name":"Synthetic keyboard","transport":"ble","roles":["keyboard"],
        "state":"disconnected","security":null,"enabled":true,"effective_enabled":true,"enabled_reason":null,
        "transport_supported":true,"validation_error":null,"trusted":true,"blocked":false,"reconnect":"auto",
        "last_error":null,"hidpp_enabled":true,"normalization_state":"pending","normalization_error":null,
        "settings_state":"pending","settings_error":null,"settings_revision":1})
}
fn status() -> Value {
    json!({"protocol":1,"firmware_version":"test","hardware_config":"test","hardware_digest":"test","radio_backend":"test",
        "adapter_id":"synthetic","build_profile":"development","boot_id":"boot","session_id":"session",
        "limits":{"max_line_bytes":4096,"max_pending_requests":4,"saved_devices":8,"active_connections":4,"scan_candidates":16,
            "hidpp_settings":32,"hidpp_saved_settings":32,"hidpp_sensors":4,"hidpp_firmware_entities":2,"hidpp_setting_choices":16,"hidpp_features":64},
        "counts":{"saved":1,"paired":1,"preferred_enabled":1,"enabled":1,"connected":0},"capacity":{"enabled":[],"pairing":[]},
        "revision":1,"name": "Test adapter", "host_platform":"linux","monitor":false,"radio_ready":true,"storage_ready":true,
        "heartbeat":{"interval_ms":5000,"timeout_ms":15000,"remaining_ms":15000},"pending":[]})
}
fn setting() -> Value {
    json!({"revision":1,"device_id":"d_1","setting":{"key":"backlight.enabled","type":"bool","writable":true,
        "feature":6530,"feature_version":1,"scope":"device","choices":[],"min":null,"max":null,"step":null,
        "managed":true,"desired":true,"observed":true,"fresh":true,"observed_at_ms":1,"observation_source":"read","state":"applied","error":null}})
}
fn summary() -> Value {
    json!({"revision":1,"device_id":"d_1","count":1,"read":1,"applied":0,"unchanged":0,"unsupported":0,"failed":0,"uncertain":0})
}
fn list_end() -> Value {
    json!({"revision":1,"device_id":"d_1","count":1,"settings_state":"ready","settings_error":null})
}
fn terminal(name: &str) -> Value {
    match name {
        "adapter.protocol" => json!({"protocol":1}),
        "adapter.status" => status(),
        "adapter.capabilities" => json!(["classic", "ble", "debug", "storage_management"]),
        "adapter.wait_ready" => json!({"state":"ready","status":status()}),
        "adapter.bootloader.enter" => json!({"rebooting":true,"mode":"bootsel"}),
        "adapter.platform.set" | "adapter.name.set" => {
            json!({"revision":1,"name": "Test adapter", "host_platform":"linux"})
        }
        "session.heartbeat" => json!({"timeout_ms":15000,"monitor":false}),
        "session.monitor.set" => json!({"revision":1,"enabled":true}),
        "device.list" => json!({"count":1,"revision":1}),
        "device.info" | "device.info.refresh" => information(),
        "device.get" => json!({"revision":1,"device":device()}),
        "discovery.scan" => json!({"count":1,"truncated":false}),
        "pairing.reply" => json!({"accepted":true}),
        "device.unpair" => json!({"device_id":"d_1","removed":true}),
        "hidpp.feature.list" | "hidpp.setting.list" => list_end(),
        "hidpp.setting.get" | "hidpp.setting.set" | "hidpp.setting.forget" => setting(),
        "hidpp.setting.refresh" | "hidpp.setting.apply" => summary(),
        "request.cancel" => json!({"request_id":2,"requested":true}),
        "storage.list" => json!({"count":1}),
        "storage.read" => json!({"bytes":3}),
        _ => json!({"device":device()}),
    }
}
fn chunk(name: &str) -> Option<Value> {
    Some(match name {
        "adapter.wait_ready" => json!({"state":"initializing"}),
        "device.list" => json!({"revision":1,"device":device()}),
        "hidpp.feature.list" => {
            json!({"revision":1,"device_id":"d_1","feature":{"index":1,"id":6530,"version":1,"flags":0,"supported":true}})
        }
        "hidpp.setting.list" => setting(),
        "hidpp.setting.refresh" | "hidpp.setting.apply" => {
            let mut v = setting();
            v["outcome"] = json!("read");
            v
        }
        "storage.list" => json!({"name":"device.json","type":"file","size":3}),
        "storage.read" => json!({"offset":0,"data":"YWJj"}),
        _ => return None,
    })
}

#[test]
fn every_command_has_codec_checked_requests_and_both_stream_phases() {
    let artifacts = generate();
    assert_eq!(
        artifacts.catalog["commands"].as_object().unwrap().len(),
        COMMANDS.len()
    );
    for command in COMMANDS {
        let name = command.as_str();
        let value = request(name, args(name));
        let bytes = serde_json::to_vec(&value).unwrap();
        let decoded = codec::decode_request(&bytes).unwrap();
        assert_valid(&format!("{name}.request"), &value);
        assert_valid(
            &format!("{name}.request"),
            &serde_json::to_value(decoded).unwrap(),
        );
        let mut reply = response(terminal(name), true);
        reply["v"] = json!(command.wire_version());
        assert_valid(&format!("{name}.response"), &reply);
        assert_eq!(
            artifacts.catalog["commands"][name]["streaming"],
            chunk(name).is_some()
        );
        if let Some(chunk) = chunk(name) {
            assert_valid(&format!("{name}.response"), &response(chunk, false));
        }
        let failure = json!({"v":command.wire_version(),"type":"response","id":1,"ok":false,"done":true,"error":{"code":"invalid_args"}});
        if name == "adapter.protocol" {
            assert!(!validator(&format!("{name}.response")).is_valid(&failure));
            assert_eq!(
                artifacts.catalog["commands"][name]["error_definitions"],
                json!([])
            );
        } else {
            assert_valid(&format!("{name}.response"), &failure);
        }
    }
}

#[test]
fn every_notification_has_a_checked_example() {
    let events = generate().catalog["events"].as_object().unwrap().clone();
    for name in events.keys() {
        let data = match name.as_str() {
            "adapter.changed" => {
                json!({"revision":1,"name": "Test adapter", "host_platform":"linux"})
            }
            "device.changed" | "device.paired" | "device.connected" => {
                json!({"revision":1,"device":device()})
            }
            "device.disconnected" => json!({"revision":1,"device":device(),"reason":"requested"}),
            "device.unpaired" => json!({"revision":1,"device_id":"d_1"}),
            "device.info.changed" => information(),
            "hidpp.setting.changed" => setting(),
            "discovery.result" => {
                json!({"candidate_id":"c_1","name":"Synthetic","kind":"keyboard","transport":"ble","rssi":-40})
            }
            "pairing.prompt" => {
                json!({"candidate_id":"c_1","prompt_id":"p_1","method":"enter_passkey","expires_in_ms":30000})
            }
            "pairing.display" => {
                json!({"candidate_id":"c_1","prompt_id":"p_1","method":"passkey","expires_in_ms":30000,"value":"123456"})
            }
            "events.lost" => json!({"revision":1,"dropped":1}),
            "protocol.error" => json!({"code":"invalid_json"}),
            _ => panic!("missing event fixture: {name}"),
        };
        assert_valid(
            &format!("{name}.event"),
            &json!({"v":1,"type":"event","event":name,"request_id":1,"data":data}),
        );
    }
}

#[test]
fn invalid_requests_fail_schema_and_the_real_codec() {
    let schema = validator("Request");
    let mut invalid = vec![
        request("status", json!({})),
        request("unknown", json!({})),
        request("device.get", json!({"device_id":"d_1\n"})),
        request("storage.list", json!({"path":"/devices\n"})),
        request("adapter.status", json!({"extra":true})),
        request("device.get", json!({"device_id":"bad id"})),
        request("device.get", json!({"device_id":""})),
        request("discovery.scan", json!({"transport":"wifi"})),
        request("discovery.scan", json!({"duration_ms":999})),
        request("discovery.scan", json!({"duration_ms":60001})),
        request("device.connect", json!({"device_id":"d_1","timeout_ms":0})),
        request(
            "pairing.start",
            json!({"candidate_id":"c_1","timeout_ms":180001}),
        ),
        request("storage.read", json!({"path":"/devices/../identity.json"})),
        request("storage.list", json!({"path":"relative"})),
        request(
            "hidpp.setting.set",
            json!({"device_id":"d_1","key":"UPPER","value":true}),
        ),
        request(
            "hidpp.setting.set",
            json!({"device_id":"d_1","key":"backlight.enabled","value":null}),
        ),
        request(
            "hidpp.setting.set",
            json!({"device_id":"d_1","key":"backlight.enabled","value":9007199254740992_u64}),
        ),
        request(
            "device.enabled.set",
            json!({"device_id":"d_1","enabled":"true"}),
        ),
    ];
    for (field, value) in [
        ("id", json!(0)),
        ("id", json!(2147483648_u64)),
        ("v", json!(2)),
        ("extra", json!(true)),
    ] {
        let mut bad = request("adapter.status", json!({}));
        bad[field] = value;
        invalid.push(bad);
    }
    for bad in invalid {
        assert!(!schema.is_valid(&bad), "schema accepted {bad}");
        assert!(
            codec::decode_request(&serde_json::to_vec(&bad).unwrap()).is_err(),
            "codec accepted {bad}"
        );
    }
}

#[test]
fn responses_require_the_right_command_and_phase() {
    let schema = validator("storage.read.response");
    assert!(!schema.is_valid(&response(json!({"count":3}), true)));
    assert!(!schema.is_valid(&response(json!({"bytes":3}), false)));
    assert!(!schema.is_valid(&response(json!({"offset":0,"data":"YWJj"}), true)));
    for data in ["AB==".to_owned(), "A".repeat(684)] {
        assert!(!schema.is_valid(&response(json!({"offset":0,"data":data}), false)));
    }
    assert!(!schema.is_valid(&response(json!({"bytes":4294967296_u64}), true)));
    let mut mixed = response(json!({"bytes":3}), true);
    mixed["error"] = json!({"code":"busy"});
    assert!(!schema.is_valid(&mixed));
    for bad in [
        json!(["ble", "ble"]),
        json!(["wifi"]),
        json!({"capabilities":["ble"]}),
    ] {
        assert!(!validator("adapter.capabilities.response").is_valid(&response(bad, true)));
    }
}

#[test]
fn generated_artifacts_are_current() {
    let artifacts = generate();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../schema");
    for (name, value) in [
        ("wire.schema.json", artifacts.wire),
        ("commands.json", artifacts.catalog),
    ] {
        assert_eq!(
            std::fs::read_to_string(root.join(name)).unwrap(),
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
            "run cargo run -p cordial-schema"
        );
    }
}

#[test]
fn error_details_match_the_operation_and_error_code() {
    let failure = |code: &str, details: Value| json!({"v":1,"type":"response","id":1,"ok":false,"done":true,"error":{"code":code,"details":details}});
    assert!(
        !validator("storage.read.response").is_valid(&failure("busy", json!({"device_id":"d_1"})))
    );
    assert!(!validator("device.enabled.set.response").is_valid(&failure("capacity", summary())));
    assert!(
        !validator("hidpp.setting.apply.response")
            .is_valid(&failure("settings_refresh_failed", summary()))
    );
    assert_valid(
        "pairing.start.response",
        &failure("blocked", json!({"device_id":"d_1"})),
    );
    assert_valid(
        "pairing.start.response",
        &failure("storage_failed", json!({"device_id":"d_1"})),
    );
    assert_valid(
        "device.enabled.set.response",
        &failure("capacity", json!({"reason":"enabled_full"})),
    );
    assert_valid(
        "hidpp.setting.apply.response",
        &failure("settings_apply_failed", summary()),
    );
    assert_valid(
        "storage.read.response",
        &failure("storage_failed", json!({"outcome":"unknown"})),
    );
}

#[test]
fn connect_capacity_details_are_part_of_its_response_contract() {
    assert_valid(
        "device.connect.response",
        &json!({"v":1,"type":"response","id":1,"ok":false,"done":true,
        "error":{"code":"capacity","details":{"reason":"connections_full"}}}),
    );
}

fn information() -> Value {
    json!({"revision":1,"device_id":"d_1","fields":[{"key":"battery_percent","instance":0,"value":50,"available":true,"fresh":true}]})
}

#[test]
fn resetting_an_adapter_name_requires_an_explicit_null() {
    let value = request("adapter.name.set", json!({"name":null}));
    assert_valid("adapter.name.set.request", &value);
    let request = codec::decode_request(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        request.command,
        cordial_protocol::messages::Command::Name(cordial_protocol::messages::AdapterName {
            name: None
        })
    ));
    let missing = serde_json::json!({"v":1,"id":1,"cmd":"adapter.name.set","args":{}});
    assert!(codec::decode_request(&serde_json::to_vec(&missing).unwrap()).is_err());
    assert!(!validator("adapter.name.set.request").is_valid(&missing));
}

#[test]
fn discovery_accepts_extensible_arguments_and_results() {
    let schema = validator("adapter.protocol.request");
    for args in [
        None,
        Some(Value::Null),
        Some(json!({})),
        Some(json!({"future":{"values":[1,true,null]}})),
    ] {
        let mut query = json!({"v":0,"id":1,"cmd":"adapter.protocol"});
        if let Some(args) = args {
            query["args"] = args;
        }
        assert!(schema.is_valid(&query), "{query}");
        assert!(codec::decode_request(&serde_json::to_vec(&query).unwrap()).is_ok());
    }
    for args in [json!(1), json!(false), json!(""), json!([])] {
        let query = request("adapter.protocol", args);
        assert!(!schema.is_valid(&query));
        assert!(codec::decode_request(&serde_json::to_vec(&query).unwrap()).is_err());
    }
    let schema = validator("adapter.protocol.response");
    let mut reply = response(
        json!({"protocol":2,"future":{"values":[1,true,null]}}),
        true,
    );
    reply["v"] = json!(0);
    assert!(schema.is_valid(&reply));
    let decoded: cordial_protocol::messages::ProtocolResult =
        serde_json::from_value(reply["result"].clone()).unwrap();
    assert_eq!(decoded.protocol, 2);
    for protocol in [json!(null), json!("1"), json!(-1), json!(1.5), json!(256)] {
        reply["result"]["protocol"] = protocol;
        assert!(!schema.is_valid(&reply));
    }
    reply["result"] = json!({"extra":1});
    assert!(!schema.is_valid(&reply));
    for (name, version) in [("adapter.protocol", 1), ("adapter.status", 0)] {
        let mut query = request(name, json!({}));
        query["v"] = json!(version);
        assert!(!validator("Request").is_valid(&query));
        assert!(codec::decode_request(&serde_json::to_vec(&query).unwrap()).is_err());
    }
}
