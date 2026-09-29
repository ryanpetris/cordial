use cordial_core::info::{Information, standard};
use cordial_protocol::{
    info::{InfoField, InfoKey as K},
    settings::SettingValue as V,
};
fn field(i: &Information, key: K, instance: u8) -> InfoField {
    i.snapshot()
        .into_iter()
        .find(|f| f.key == key && f.instance == instance)
        .unwrap()
}
#[test]
fn battery_source_is_pinned_and_toggle_requires_new_readings() {
    use cordial_protocol::identifiers::Transport;
    let mut i = Information::default();
    i.battery.configure(Transport::Classic, true);
    i.connection(true, true);
    standard(&mut i, 0x2a19, 0, &[51]);
    assert!(!field(&i, K::BatteryPercent, 0).available);
    i.observe(true, K::BatteryPercent, 0, V::Integer(52));
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(52));
    i.connection(true, false);
    assert!(!field(&i, K::BatteryPercent, 0).available);
    i.battery.hid_reading(0, K::BatteryPercent, V::Integer(40));
    i.observe(true, K::BatteryPercent, 0, V::Integer(99));
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(40));
    i.connection(true, true);
    assert!(!field(&i, K::BatteryPercent, 0).available);
    i.observe(true, K::BatteryPercent, 0, V::Integer(53));
    i.invalidate_vendor(&[K::BatteryPercent]);
    assert!(!field(&i, K::BatteryPercent, 0).available);
    let mut ble = Information::default();
    ble.connection(true, true);
    standard(&mut ble, 0x2a19, 0, &[51]);
    ble.observe(true, K::BatteryPercent, 0, V::Integer(99));
    assert_eq!(field(&ble, K::BatteryPercent, 0).value, V::Integer(51));
    ble.connection(true, false);
    assert_eq!(field(&ble, K::BatteryPercent, 0).value, V::Integer(51));
    ble.battery.hidpp_reports(true);
    ble.connection(true, true);
    standard(&mut ble, 0x2a19, 0, &[51]);
    standard(&mut ble, 0x2bed, 0, &[0, 0x41, 0]);
    assert!(!field(&ble, K::BatteryPercent, 0).available);
    assert!(!field(&ble, K::BatteryCharging, 0).available);
    ble.battery.vendor_reading(Some(82), Some(true));
    standard(&mut ble, 0x2a19, 0, &[51]);
    assert_eq!(field(&ble, K::BatteryPercent, 0).value, V::Integer(82));
    assert_eq!(field(&ble, K::BatteryCharging, 0).value, V::Bool(true));
    ble.connection(true, false);
    assert!(!field(&ble, K::BatteryPercent, 0).available);
    assert!(!field(&ble, K::BatteryCharging, 0).available);
    ble.battery.vendor_reading(Some(99), Some(true));
    standard(&mut ble, 0x2a19, 0, &[51]);
    assert_eq!(field(&ble, K::BatteryPercent, 0).value, V::Integer(51));
    assert!(!field(&ble, K::BatteryCharging, 0).available);
}
#[test]
fn battery_delta_only_contains_changed_fields_and_unknown_clears() {
    let mut i = Information::default();
    i.connection(true, false);
    standard(&mut i, 0x2a26, 0, b"1.2.3");
    standard(&mut i, 0x2a19, 0, &[51]);
    i.changes();
    standard(&mut i, 0x2a19, 0, &[50]);
    let delta = i.changes();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].key, K::BatteryPercent);
    assert_eq!(delta[0].value, V::Integer(50));
    standard(&mut i, 0x2a19, 0, &[255]);
    let delta = i.changes();
    assert_eq!(delta.len(), 1);
    assert!(delta.iter().all(|f| !f.available && f.value == V::Null));
    assert_eq!(field(&i, K::Firmware, 0).value, V::Text("1.2.3".into()));
    standard(&mut i, 0x2a19, 0, &[0]);
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(0));
    assert!(!field(&i, K::BatteryCharging, 0).available);
}
#[test]
fn disconnect_and_reconnect_clear_battery_data() {
    let mut i = Information::default();
    i.connection(true, true);
    standard(&mut i, 0x2a19, 0, &[70]);
    i.changes();
    i.connection(false, true);
    assert!(!field(&i, K::BatteryPercent, 0).available);
    assert!(
        i.changes()
            .iter()
            .any(|f| f.key == K::BatteryPercent && f.value == V::Null)
    );
    i.connection(true, true);
    assert!(!field(&i, K::BatteryPercent, 0).available);
}
#[test]
fn multiple_batteries_choose_main_or_lowest_with_matching_charging() {
    let mut i = Information::default();
    i.connection(true, true);
    standard(&mut i, 0x2a19, 0, &[20]);
    standard(&mut i, 0x2a19, 1, &[80]);
    standard(&mut i, 0x2bed, 0, &[0, 0x41, 0]);
    standard(&mut i, 0x2bed, 1, &[0, 0x21, 0]);
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(20));
    assert_eq!(field(&i, K::BatteryCharging, 0).value, V::Bool(false));
    standard(&mut i, 0x2904, 1, &[4, 0, 0xad, 0x27, 1, 6, 1]);
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(80));
    assert_eq!(field(&i, K::BatteryCharging, 0).value, V::Bool(true));
    assert_eq!(
        i.snapshot()
            .iter()
            .filter(|f| matches!(f.key, K::BatteryPercent | K::BatteryCharging))
            .count(),
        2
    );
    standard(&mut i, 0x2bed, 1, &[0, 0, 0]);
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(20));
}
#[test]
fn optional_status_length_and_presence_are_validated_and_text_is_bounded() {
    let mut i = Information::default();
    i.connection(true, false);
    standard(&mut i, 0x2bed, 0, &[0, 0x21, 0]);
    assert_eq!(field(&i, K::BatteryCharging, 0).value, V::Bool(true));
    for bad in [&[0, 0x20, 0][..], &[1, 0x21, 0][..], &[0, 0x21][..]] {
        standard(&mut i, 0x2bed, 0, bad);
        assert!(!field(&i, K::BatteryCharging, 0).available);
    }
    standard(&mut i, 0x2bed, 0, &[8, 0x21, 0x80]);
    assert_eq!(field(&i, K::BatteryCharging, 0).value, V::Bool(true));
    standard(&mut i, 0x2a00, 0, &[b'a'; 512]);
    assert!(field(&i, K::Name, 0).valid());
    standard(&mut i, 0x2a50, 0, &[2, 0x6d, 4, 1, 2, 3, 4]);
    assert_eq!(field(&i, K::VendorId, 0).value, V::Integer(0x046d));
    assert_eq!(
        field(&i, K::VendorIdNamespace, 0).value,
        V::Text("usb".into())
    );
}
#[test]
fn equivalent_providers_produce_identical_wire_snapshots() {
    let mut a = Information::default();
    a.connection(true, true);
    let mut b = Information::default();
    b.battery
        .configure(cordial_protocol::identifiers::Transport::Classic, true);
    b.connection(true, true);
    standard(&mut a, 0x2a19, 0, &[50]);
    b.observe(true, K::BatteryPercent, 0, V::Integer(50));
    assert_eq!(
        serde_json::to_string(&a.snapshot()).unwrap(),
        serde_json::to_string(&b.snapshot()).unwrap()
    );
}

#[test]
fn failed_appearance_read_uses_the_device_kind_hint() {
    let mut i = Information::default();
    i.connection(true, false);
    i.kind_hint(cordial_protocol::messages::DeviceKind::Keyboard);
    standard(&mut i, 0x2a01, 0, &[0xc1, 3]);
    standard(&mut i, 0x2a01, 0, &[]);
    assert_eq!(field(&i, K::Kind, 0).value, V::Text("keyboard".into()));
    i.kind_hint(cordial_protocol::messages::DeviceKind::Unknown);
    assert!(!field(&i, K::Kind, 0).available);
}

#[test]
fn names_only_change_for_a_valid_new_name_and_read_errors_keep_values_stale() {
    use cordial_core::info::standard_failed;
    let mut i = Information::default();
    i.connection(true, true);
    standard(&mut i, 0x2a00, 0, b"Known name");
    for missing in [&[][..], b"\0", b" \n\r"] {
        standard(&mut i, 0x2a00, 0, missing);
        assert_eq!(field(&i, K::Name, 0).value, V::Text("Known name".into()));
    }
    i.observe(true, K::Name, 0, V::Text("Preferred name".into()));
    i.observe(true, K::Name, 0, V::Text("".into()));
    i.observe(true, K::Name, 0, V::Null);
    assert_eq!(
        field(&i, K::Name, 0).value,
        V::Text("Preferred name".into())
    );
    standard(&mut i, 0x2a19, 0, &[51]);
    standard_failed(&mut i, 0x2a19, 0);
    let battery = field(&i, K::BatteryPercent, 0);
    assert_eq!(battery.value, V::Null);
    assert!(!battery.available && !battery.fresh);
    standard(&mut i, 0x2a19, 0, &[255]);
    assert!(!field(&i, K::BatteryPercent, 0).available);
    i.connection(true, false);
    standard(&mut i, 0x2a00, 0, b"New name");
    assert_eq!(field(&i, K::Name, 0).value, V::Text("New name".into()));
}
#[test]
fn disconnect_keeps_preferred_kind_and_descriptor_roles_strengthen_appearance() {
    use cordial_protocol::messages::DeviceKind;
    let mut i = Information::default();
    i.connection(true, true);
    i.kind_hint(DeviceKind::KeyboardMouse);
    standard(&mut i, 0x2a01, 0, &[0xc1, 3]);
    assert_eq!(
        field(&i, K::Kind, 0).value,
        V::Text("keyboard_mouse".into())
    );
    i.observe(true, K::Kind, 0, V::Text("keyboard".into()));
    assert_eq!(field(&i, K::Kind, 0).value, V::Text("keyboard".into()));
    i.connection(false, true);
    let kind = field(&i, K::Kind, 0);
    assert_eq!(kind.value, V::Text("keyboard".into()));
    assert!(!kind.fresh);
}

#[test]
fn ble_optional_percentage_coarse_and_energy_are_normalized() {
    let mut i = Information::default();
    i.connection(true, false);
    standard(&mut i, 0x2bed, 0, &[2, 0x21, 0, 0]);
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(0));
    standard(&mut i, 0x2bed, 0, &[0, 0x81, 1]); // critical, unknown charging
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(5));
    assert_eq!(field(&i, K::BatteryCharging, 0).value, V::Null);
    // available energy=5, full capacity=10, rate=+2
    standard(&mut i, 0x2bf0, 0, &[0x1c, 5, 0, 10, 0, 2, 0]);
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(50));
    assert_eq!(field(&i, K::BatteryCharging, 0).value, V::Bool(true));
    standard(&mut i, 0x2a19, 0, &[80]);
    assert_eq!(field(&i, K::BatteryPercent, 0).value, V::Integer(80));
    // Unknown IEEE-11073 values cannot masquerade as a reading.
    standard(&mut i, 0x2bf0, 0, &[0x1c, 0xff, 7, 10, 0, 0xff, 7]);
    assert_eq!(field(&i, K::BatteryCharging, 0).value, V::Null);
}
