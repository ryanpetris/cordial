use cordial_protocol::keys::{self, Kind};

#[test]
fn finds_plain_keys() {
    let (key, index) = keys::lookup(keys::BATTERY_LEVEL).unwrap();
    assert_eq!(key.kind, Kind::Integer);
    assert!(key.device && !key.setting && !key.adapter);
    assert_eq!(index, None);
}

#[test]
fn matches_templates_with_their_index() {
    let (key, index) = keys::lookup("pointer.sensor.1.dpi").unwrap();
    assert_eq!(key.key, keys::POINTER_SENSOR_N_DPI);
    assert!(key.setting);
    assert_eq!(index, Some(1));
    assert_eq!(
        keys::indexed(keys::POINTER_SENSOR_N_DPI, 1),
        "pointer.sensor.1.dpi"
    );
}

#[test]
fn rejects_unknown_and_malformed_keys() {
    assert!(keys::lookup("pointer.sensor.x.dpi").is_none());
    assert!(keys::lookup("pointer.sensor..dpi").is_none());
    assert!(keys::lookup("pointer.sensor.1.dpi.x").is_none());
    assert!(keys::lookup("battery").is_none());
    assert!(keys::lookup("no.such.key").is_none());
}

#[test]
fn enum_keys_list_their_values() {
    let (key, _) = keys::lookup(keys::WHEEL_MODE).unwrap();
    assert_eq!(key.kind, Kind::Enum);
    assert_eq!(key.values, &["freespin", "ratchet"]);
}
