use cordial_protocol::{AdapterInformation, ResourceCapacity, ResourceType, Status};
use prost::Message;

#[test]
fn storage_telemetry_preserves_independent_presence_on_the_wire() {
    for storage_format in [None, Some(0), Some(1)] {
        for count in [None, Some(0), Some(4096)] {
            for limit in [None, Some(0), Some(1 << 20)] {
                let status = Status {
                    information: Some(AdapterInformation {
                        storage_format,
                        ..Default::default()
                    }),
                    resources: vec![ResourceCapacity {
                        r#type: ResourceType::Storage as i32,
                        count,
                        limit,
                    }],
                    ..Default::default()
                };
                assert_eq!(
                    Status::decode(status.encode_to_vec().as_slice()).unwrap(),
                    status
                );
            }
        }
    }
}

#[cfg(feature = "json")]
#[test]
fn storage_telemetry_raw_json_preserves_unknown_and_explicit_zero() {
    for storage_format in [None, Some(0), Some(1)] {
        for count in [None, Some(0), Some(4096)] {
            for limit in [None, Some(0), Some(1 << 20)] {
                let status = Status {
                    information: Some(AdapterInformation {
                        storage_format,
                        ..Default::default()
                    }),
                    resources: vec![ResourceCapacity {
                        r#type: ResourceType::Storage as i32,
                        count,
                        limit,
                    }],
                    ..Default::default()
                };
                let json = serde_json::to_value(&status).unwrap();
                assert_eq!(
                    json["information"].get("storage_format"),
                    storage_format.map(serde_json::Value::from).as_ref()
                );
                assert_eq!(json["resources"][0]["type"], "RESOURCE_TYPE_STORAGE");
                assert_eq!(
                    json["resources"][0].get("count"),
                    count.map(serde_json::Value::from).as_ref()
                );
                assert_eq!(
                    json["resources"][0].get("limit"),
                    limit.map(serde_json::Value::from).as_ref()
                );
                assert_eq!(serde_json::from_value::<Status>(json).unwrap(), status);
            }
        }
    }
}
