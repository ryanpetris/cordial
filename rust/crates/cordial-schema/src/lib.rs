//! Host-only generation and validation of the current serial wire contract.
use cordial_protocol::{PROTOCOL_VERSION, identifiers::RequestId, messages::*, payloads::*};
use schemars::{JsonSchema, SchemaGenerator};
use serde_json::{Map, Value, json};

fn shape<T: JsonSchema>(generator: &mut SchemaGenerator) -> Value {
    generator.subschema_for::<T>().to_value()
}
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
}
fn response(result: Value, done: bool, id: &Value, version: u8) -> Value {
    object(
        json!({"v":{"const":version},"type":{"const":"response"},"id":id,
        "ok":{"const":true},"done":{"const":done},"result":result}),
        &["v", "type", "id", "ok", "done", "result"],
    )
}
fn event(name: &str, data: Value, id: &Value, correlated: bool) -> Value {
    let mut required = vec!["v", "type", "event", "data"];
    if correlated {
        required.push("request_id");
    }
    object(
        json!({"v":{"const":PROTOCOL_VERSION},"type":{"const":"event"},"event":{"const":name},"request_id":id,"data":data}),
        &required,
    )
}

// JSON Schema does not define Rust's integer format names as assertions.
fn integer_bounds(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            let bounds = match object.get("format").and_then(Value::as_str) {
                Some("uint32") => Some((json!(u32::MIN), json!(u32::MAX))),
                Some("uint64") => Some((json!(u64::MIN), json!(u64::MAX))),
                Some("int32") => Some((json!(i32::MIN), json!(i32::MAX))),
                Some("int64") => Some((json!(i64::MIN), json!(i64::MAX))),
                _ => None,
            };
            if let Some((min, max)) = bounds {
                object.entry("minimum").or_insert(min);
                object.entry("maximum").or_insert(max);
            }
            object.values_mut().for_each(integer_bounds);
        }
        Value::Array(array) => array.iter_mut().for_each(integer_bounds),
        _ => {}
    }
}

pub struct Artifacts {
    pub wire: Value,
    pub catalog: Value,
}
pub fn generate() -> Artifacts {
    let mut generator = SchemaGenerator::default();
    let mut output = schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    let id = shape::<RequestId>(&mut generator);
    let mut definitions = Map::new();
    let codes = shape::<cordial_protocol::errors::ErrorCode>(&mut output);
    definitions.insert("BareError".into(), object(json!({"code":codes}), &["code"]));
    definitions.insert("StorageMutationError".into(), object(
        json!({"code":{"enum":["storage_failed","storage_full"]},"details":shape::<MutationDetails>(&mut output)}), &["code","details"]));
    definitions.insert(
        "CapacityError".into(),
        object(
            json!({"code":{"const":"capacity"},"details":shape::<CapacityDetails>(&mut output)}),
            &["code", "details"],
        ),
    );
    // Pair cleanup can replace the original rejection with a backend/storage
    // error while retaining the identity of the existing device.
    definitions.insert(
        "ExistingDeviceError".into(),
        object(
            json!({"code":codes,"details":shape::<ExistingDeviceDetails>(&mut output)}),
            &["code", "details"],
        ),
    );
    for (name, code) in [
        ("SettingsApplyError", "settings_apply_failed"),
        ("SettingsRefreshError", "settings_refresh_failed"),
    ] {
        definitions.insert(
            name.into(),
            object(
                json!({"code":{"enum":[code,"not_connected"]},
            "details":shape::<SettingsSummary>(&mut output)}),
                &["code", "details"],
            ),
        );
    }
    let mut requests = Vec::new();
    let mut responses = Vec::new();
    let mut commands = Map::new();
    for &command in COMMANDS {
        use CommandId as C;
        // Exhaustive match: a new command cannot omit its contract definition.
        let version = command.wire_version();
        let (args, terminal, chunks) = match command {
            C::Protocol => (
                json!({"type":["object","null"],"additionalProperties":true}),
                shape::<ProtocolResult>(&mut generator),
                None,
            ),
            C::Status => (
                shape::<Empty>(&mut generator),
                shape::<Status>(&mut output),
                None,
            ),
            C::Capabilities => (
                shape::<Empty>(&mut generator),
                shape::<Capabilities>(&mut output),
                None,
            ),
            C::Ready => {
                let ready = shape::<ReadyResult>(&mut output);
                let terminal = json!({"allOf":[ready,{"properties":{"state":{"const":"ready"}}}]});
                let chunk = object(json!({"state":{"const":"initializing"}}), &["state"]);
                (shape::<Empty>(&mut generator), terminal, Some(chunk))
            }
            C::Heartbeat => (
                shape::<Empty>(&mut generator),
                shape::<HeartbeatResult>(&mut output),
                None,
            ),
            C::Bootloader => (
                shape::<Empty>(&mut generator),
                shape::<BootloaderResult>(&mut output),
                None,
            ),
            C::Monitor => (
                shape::<Enabled>(&mut generator),
                shape::<MonitorResult>(&mut output),
                None,
            ),
            C::Devices => (
                shape::<DeviceFilter>(&mut generator),
                shape::<DeviceListEnd>(&mut output),
                Some(shape::<DeviceSnapshot>(&mut output)),
            ),
            C::DeviceInfo | C::DeviceInfoRefresh => (
                shape::<DeviceRef>(&mut generator),
                shape::<cordial_protocol::info::DeviceInfo>(&mut output),
                None,
            ),
            C::Info => (
                shape::<DeviceRef>(&mut generator),
                shape::<DeviceSnapshot>(&mut output),
                None,
            ),
            C::Scan => (
                shape::<Scan>(&mut generator),
                shape::<ScanEnd>(&mut output),
                None,
            ),
            C::Pair => (
                shape::<Pair>(&mut generator),
                shape::<DeviceResult>(&mut output),
                None,
            ),
            C::PairReply => (
                shape::<PairReply>(&mut generator),
                shape::<PairReplyResult>(&mut output),
                None,
            ),
            C::Connect => (
                shape::<Connect>(&mut generator),
                shape::<DeviceResult>(&mut output),
                None,
            ),
            C::Disconnect => (
                shape::<DeviceRef>(&mut generator),
                shape::<DeviceResult>(&mut output),
                None,
            ),
            C::Unpair => (
                shape::<DeviceRef>(&mut generator),
                shape::<DeviceRemoved>(&mut output),
                None,
            ),
            C::DeviceEnabled | C::Hidpp => (
                shape::<DeviceEnabled>(&mut generator),
                shape::<DeviceResult>(&mut output),
                None,
            ),
            C::DeviceTrusted => (
                shape::<DeviceTrusted>(&mut generator),
                shape::<DeviceResult>(&mut output),
                None,
            ),
            C::DeviceBlocked => (
                shape::<DeviceBlocked>(&mut generator),
                shape::<DeviceResult>(&mut output),
                None,
            ),
            C::Name => (
                // Serialization requires the nullable field, matching the request decoder.
                shape::<AdapterName>(&mut output),
                shape::<AdapterSettings>(&mut output),
                None,
            ),
            C::Platform => (
                shape::<Platform>(&mut generator),
                shape::<AdapterSettings>(&mut output),
                None,
            ),
            C::Features => (
                shape::<DeviceRef>(&mut generator),
                shape::<SettingsListEnd>(&mut output),
                Some(shape::<FeatureChunk>(&mut output)),
            ),
            C::Settings => (
                shape::<DeviceRef>(&mut generator),
                shape::<SettingsListEnd>(&mut output),
                Some(shape::<SettingChunk>(&mut output)),
            ),
            C::SettingsGet | C::SettingsForget => (
                shape::<SettingRef>(&mut generator),
                shape::<SettingChunk>(&mut output),
                None,
            ),
            C::SettingsSet => (
                shape::<SettingSet>(&mut generator),
                shape::<SettingChunk>(&mut output),
                None,
            ),
            C::SettingsRefresh | C::SettingsApply => (
                shape::<DeviceRef>(&mut generator),
                shape::<SettingsSummary>(&mut output),
                Some(shape::<SettingOutcomeChunk>(&mut output)),
            ),
            C::Cancel => (
                shape::<RequestRef>(&mut generator),
                shape::<CancelResult>(&mut output),
                None,
            ),
            C::StorageList => (
                shape::<StoragePath>(&mut generator),
                shape::<StorageListEnd>(&mut output),
                Some(shape::<FileEntry>(&mut output)),
            ),
            C::StorageRead => (
                shape::<StoragePath>(&mut generator),
                shape::<StorageReadEnd>(&mut output),
                Some(shape::<StorageChunk>(&mut output)),
            ),
        };
        let name = command.as_str();
        let request = object(
            json!({"v":{"const":version},"id":id,"cmd":{"const":name},"args":args}),
            if command == C::Protocol {
                &["v", "id", "cmd"]
            } else {
                &["v", "id", "cmd", "args"]
            },
        );
        let request_key = format!("{name}.request");
        definitions.insert(request_key.clone(), request);
        requests.push(json!({"$ref":format!("#/$defs/{request_key}")}));
        let mut errors = if command == C::Protocol {
            vec![]
        } else {
            vec!["BareError", "StorageMutationError"]
        };
        if matches!(
            command,
            C::Pair | C::Connect | C::DeviceEnabled | C::DeviceBlocked
        ) {
            errors.push("CapacityError");
        }
        match command {
            C::Pair => errors.push("ExistingDeviceError"),
            C::SettingsApply => errors.push("SettingsApplyError"),
            C::SettingsRefresh => errors.push("SettingsRefreshError"),
            _ => {}
        }
        let error_key = format!("{name}.error");
        let error_refs: Vec<_> = errors
            .iter()
            .map(|name| json!({"$ref":format!("#/$defs/{name}")}))
            .collect();
        definitions.insert(
            error_key.clone(),
            if errors.is_empty() {
                json!(false)
            } else {
                json!({"oneOf":error_refs})
            },
        );
        let failure = object(
            json!({"v":{"const":version},"type":{"const":"response"},"id":id,
            "ok":{"const":false},"done":{"const":true},"error":{"$ref":format!("#/$defs/{error_key}")}}),
            &["v", "type", "id", "ok", "done", "error"],
        );
        let mut variants = vec![response(terminal, true, &id, version)];
        if command != C::Protocol {
            variants.push(failure);
        }
        if let Some(chunk) = chunks.as_ref() {
            variants.push(response(chunk.clone(), false, &id, version));
        }
        let response_key = format!("{name}.response");
        definitions.insert(response_key.clone(), json!({"anyOf":variants}));
        responses.push(json!({"$ref":format!("#/$defs/{response_key}")}));
        let required = match command {
            C::Bootloader => vec![Capability::Debug],
            C::StorageList | C::StorageRead => vec![Capability::StorageManagement],
            _ => vec![],
        };
        commands.insert(name.into(), json!({"request":format!("wire.schema.json#/$defs/{request_key}"),
            "response":format!("wire.schema.json#/$defs/{response_key}"),"streaming":chunks.is_some(),
            "required_capabilities":required,
            "error":format!("wire.schema.json#/$defs/{error_key}"),"error_definitions":errors,
            "transport_capability":matches!(command,C::Scan|C::Pair|C::Connect)}));
    }
    let mut events = Map::new();
    let mut event_refs = Vec::new();
    macro_rules! notification {
        ($name:literal, $ty:ty, $correlated:expr) => {{
            let key = concat!($name,".event");
            definitions.insert(key.into(), event($name,shape::<$ty>(&mut output),&id,$correlated));
            events.insert($name.into(), json!({"schema":concat!("wire.schema.json#/$defs/",$name,".event")}));
            event_refs.push(json!({"$ref":format!("#/$defs/{key}")}));
        }};
    }
    notification!("adapter.changed", AdapterSettings, false);
    notification!(
        "device.info.changed",
        cordial_protocol::info::DeviceInfo,
        false
    );
    notification!("device.changed", DeviceSnapshot, false);
    notification!("device.paired", DeviceSnapshot, false);
    notification!("device.connected", DeviceSnapshot, false);
    notification!("device.disconnected", DeviceDisconnected, false);
    notification!("device.unpaired", DeviceUnpaired, false);
    notification!("hidpp.setting.changed", SettingChunk, false);
    notification!("discovery.result", Candidate, true);
    notification!("pairing.prompt", Prompt, true);
    notification!("pairing.display", Prompt, true);
    notification!("events.lost", LostEvents, false);
    notification!("protocol.error", ProtocolError, false);
    definitions.extend(generator.take_definitions(true));
    for (name, schema) in output.take_definitions(true) {
        if let Some(input) = definitions.get(&name) {
            assert_eq!(
                input, &schema,
                "shared input/output type needs separate schemas: {name}"
            );
        }
        definitions.insert(name, schema);
    }
    // Custom validation not expressed by ordinary Serde derives.
    use cordial_protocol::info::InfoKey as I;
    let mut information = Vec::new();
    for key in I::ALL {
        let value = match key {
            I::BatteryPercent => json!({"type":"integer","minimum":0,"maximum":100}),
            I::VendorId | I::ProductId | I::ProductVersion => {
                json!({"type":"integer","minimum":0,"maximum":65535})
            }
            I::VendorIdNamespace => json!({"enum":["usb","bluetooth"]}),
            I::BatteryCharging => json!({"type":"boolean"}),
            I::Kind => json!({"enum":["keyboard","mouse","keyboard_mouse","other"]}),
            _ => json!({"type":"string","minLength":1,"maxLength":64,
                "description":"At most 64 UTF-8 bytes; no control characters."}),
        };
        information.push(json!({"properties":{
            "key":{"const":key},"instance":{"maximum":key.instances()-1}},
            "if":{"properties":{"available":{"const":true}}},
            "then":{"properties":{"value":value}},
            "else":{"properties":{"value":{"type":"null"},"fresh":{"const":false}}}}));
    }
    definitions["InfoField"]["allOf"] = json!([{"oneOf":information}]);
    definitions["DeviceInfo"]["properties"]["fields"]["maxItems"] = json!(28);
    definitions["Scan"]["properties"]["duration_ms"] = json!({"anyOf":[{"const":0},{"type":"integer","minimum":1000,"maximum":60000}],"default":10000});
    for (name, max, default) in [("Pair", 180000, 120000), ("Connect", 60000, 30000)] {
        definitions[name]["properties"]["timeout_ms"] =
            json!({"type":"integer","minimum":1000,"maximum":max,"default":default});
    }
    definitions["StoragePath"]["properties"]["path"] =
        json!({"type":"string","maxLength":255,"pattern":r"^(?!.*(?:^|/)\.{1,2}(?:/|$))/[ -~]*$"});
    definitions["PairReply"]["properties"]["value"] =
        json!({"type":["string","null"],"maxLength":16});
    definitions["SettingSet"]["properties"]["value"] = json!({"anyOf":[{"type":"boolean"},{"type":"string"},{"type":"integer","minimum":-(cordial_protocol::MAX_REVISION as i64),"maximum":cordial_protocol::MAX_REVISION}]});
    for definition in definitions.values_mut() {
        if let Some(properties) = definition
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        {
            for field in ["revision", "settings_revision"] {
                if let Some(schema) = properties.get_mut(field) {
                    schema["maximum"] = json!(cordial_protocol::MAX_REVISION);
                }
            }
        }
    }
    definitions["ProtocolResult"]["additionalProperties"] = json!(true);
    definitions["Status"]["properties"]["protocol"] = json!({"const":PROTOCOL_VERSION});
    definitions["SettingValue"] = json!({"anyOf":[{"type":"null"},{"type":"boolean"},{"type":"string"},
        {"type":"integer","minimum":-(cordial_protocol::MAX_REVISION as i64),"maximum":cordial_protocol::MAX_REVISION}]});
    definitions["StorageChunk"]["properties"]["data"] = json!({"type":"string","minLength":4,"maxLength":684,
        "pattern":r"^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/][AQgw]==|[A-Za-z0-9+/]{2}[AEIMQUYcgkosw048]=)?$",
        "if":{"minLength":684},"then":{"pattern":"=$"}});
    definitions.insert("Request".into(), json!({"oneOf":requests}));
    definitions.insert("Response".into(), json!({"anyOf":responses}));
    definitions.insert("Event".into(), json!({"oneOf":event_refs}));
    definitions.values_mut().for_each(integer_bounds);
    Artifacts {
        wire: json!({"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"urn:cordial:wire:1",
            "title":"Cordial serial protocol", "$defs":definitions,
            "oneOf":[{"$ref":"#/$defs/Request"},{"$ref":"#/$defs/Response"},{"$ref":"#/$defs/Event"}]}),
        catalog: json!({"protocol":PROTOCOL_VERSION,"commands":commands,"events":events}),
    }
}

/// Validate a message using the request's command, not an ambiguous union of results.
pub fn validator(definition: &str) -> jsonschema::Validator {
    let mut schema = generate().wire;
    schema.as_object_mut().unwrap().remove("oneOf");
    schema["$ref"] = json!(format!("#/$defs/{definition}"));
    jsonschema::validator_for(&schema).expect("generated schema is valid")
}

/// Cached validators for fixtures and emitted firmware messages.
pub fn assert_valid(definition: &str, message: &Value) {
    use std::sync::{Mutex, OnceLock};
    static VALIDATORS: OnceLock<Mutex<std::collections::BTreeMap<String, jsonschema::Validator>>> =
        OnceLock::new();
    let mut validators = VALIDATORS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let validator = validators
        .entry(definition.into())
        .or_insert_with(|| validator(definition));
    let errors: Vec<_> = validator
        .iter_errors(message)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{definition}: {}\n{message}",
        errors.join("; ")
    );
}
