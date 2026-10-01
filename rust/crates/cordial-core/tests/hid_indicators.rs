use cordial_core::model::identifiers::HostPlatform;
use cordial_core::{
    bluetooth::{InputReport, ReportType},
    forward::Forwarder,
    hid::{IndicatorError, IndicatorValue, Map},
    link::{Link, LinkId, Profile, ServiceId},
    settings::Catalog,
};

fn descriptor(indicators: &[u8]) -> Vec<u8> {
    let mut bytes = vec![
        5, 1, 9, 6, 0xa1, 1, 5, 7, 9, 4, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2,
    ];
    bytes.extend_from_slice(indicators);
    bytes.push(0xc0);
    bytes
}
fn encode(bytes: &[u8], on: bool, baseline: Option<&[u8]>) -> Result<Vec<u8>, IndicatorError> {
    let map = Map::compile(bytes).unwrap();
    let mut out = [0; 512];
    let result = map
        .indicator_report(
            0,
            if on { 2 } else { 0 },
            IndicatorValue::default(),
            baseline,
            false,
            &mut out,
        )
        .map_err(|failure| failure.reason)?
        .unwrap();
    Ok(out[..result.length].to_vec())
}
fn runtime(bytes: &[u8]) -> (Link, Catalog, Forwarder) {
    let mut catalog = Catalog::default();
    let link = Link::new(
        LinkId {
            slot: 0,
            generation: 1,
        },
        vec![Profile::compile(ServiceId(1), bytes).unwrap()],
        512,
        false,
        HostPlatform::Linux,
        &mut catalog,
    )
    .unwrap();
    (link, catalog, Forwarder::default())
}
fn update(
    link: &mut Link,
    catalog: &mut Catalog,
    forward: &mut Forwarder,
    on: bool,
    baseline: &[u8],
) -> Vec<u8> {
    let target = if on { 2 } else { 0 };
    for now in 0..32 {
        if let Some(output) = link.output(target, now).unwrap() {
            let id = output.id;
            let bytes = output.payload.to_vec();
            link.output_complete(id, Ok(()), catalog, forward, now)
                .unwrap();
            return bytes;
        }
        if let Some(read) = link.report_read(catalog, now) {
            let report =
                InputReport::new(link.id, read.service, read.report_id.unwrap_or(0), baseline)
                    .unwrap();
            link.report_read_complete(read.id, read.kind, Ok(&report), catalog, now);
        }
    }
    panic!(
        "indicator update did not produce a report: {:?}",
        link.warnings
    );
}
#[test]
fn mode_selectors_work_as_arrays_and_separate_boolean_fields() {
    let array = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x3c, 0xa1, 2, 9, 0x3d, 9, 0x41, 0x15, 0, 0x25, 1, 0x75, 8, 0x95,
        1, 0x91, 0, 0xc0, 0xc0,
    ]);
    assert_eq!(encode(&array, true, None).unwrap(), [0]);
    assert_eq!(encode(&array, false, None).unwrap(), [1]);
    let bitmap = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x3c, 0xa1, 2, 9, 0x3d, 0x75, 1, 0x95, 1, 0x91, 2, 9, 0x41, 0x91,
        2, 0xc0, 0xc0,
    ]);
    assert_eq!(encode(&bitmap, true, None).unwrap(), [1]);
    assert_eq!(encode(&bitmap, false, None).unwrap(), [2]);
}
#[test]
fn mode_companions_require_a_fresh_baseline_and_preserve_it() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x3c, 0xa1, 2, 9, 0x3d, 9, 0x41, 9, 0x42, 0x75, 8, 0x95, 3, 0x91,
        2, 0xc0, 0xc0,
    ]);
    assert_eq!(
        encode(&bytes, true, None),
        Err(IndicatorError::ReadRequired)
    );
    assert_eq!(encode(&bytes, true, Some(&[0, 1, 1])).unwrap(), [1, 0, 1]);
}
#[test]
fn color_bitmaps_support_off_beyond_the_logical_maximum() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x47, 0xa1, 2, 9, 0x48, 9, 0x49, 9, 0x41, 0x75, 1, 0x95, 3, 0x91,
        2, 0xc0, 0xc0,
    ]);
    assert_eq!(encode(&bytes, true, Some(&[2])).unwrap(), [2]);
    assert_eq!(encode(&bytes, false, Some(&[2])).unwrap(), [4]);
    assert_eq!(encode(&bytes, true, Some(&[4])).unwrap(), [1]);
}
#[test]
fn rgb_color_and_brightness_survive_off_and_on_with_shared_companions() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x52, 0xa1, 2, 0x15, 0, 0x26, 0xff, 0, 0x75, 8, 0x95, 1, 9, 0x53,
        0x91, 2, 9, 0x54, 0x91, 2, 9, 0x55, 9, 0x56, 0x0b, 1, 0, 0, 0xff, 0x95, 3, 0x91, 2, 0xc0,
        0xc0,
    ]);
    assert_eq!(
        encode(&bytes, false, None),
        Err(IndicatorError::ReadRequired)
    );
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let off = update(
        &mut link,
        &mut catalog,
        &mut forward,
        false,
        &[255, 0, 0, 30, 42],
    );
    assert_eq!(off, [255, 0, 0, 0, 42]);
    assert_eq!(
        update(&mut link, &mut catalog, &mut forward, true, &off),
        [255, 0, 0, 30, 42]
    );
    assert!(link.warnings.is_empty());
}
#[test]
fn an_intensity_gate_restores_a_visible_color_from_black() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x52, 0xa1, 2, 0x15, 0, 0x26, 0xff, 0, 0x75, 8, 0x95, 4, 0x19,
        0x53, 0x29, 0x56, 0x91, 2, 0xc0, 0xc0,
    ]);
    assert_eq!(
        encode(&bytes, true, Some(&[0, 0, 0, 0])).unwrap(),
        [255, 255, 255, 255]
    );
}
#[test]
fn a_color_gate_does_not_preserve_color_off_when_the_lock_is_on() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x3c, 0xa1, 2, 9, 0x3d, 9, 0x41, 0x15, 0, 0x25, 1, 0x75, 8, 0x95,
        1, 0x91, 0, 0xc0, 9, 0x47, 0xa1, 2, 9, 0x41, 9, 0x48, 0x91, 0, 0xc0, 0xc0,
    ]);
    assert_eq!(encode(&bytes, true, Some(&[1, 0])).unwrap(), [0, 1]);
    assert_eq!(encode(&bytes, false, Some(&[0, 1])).unwrap(), [1, 1]);
}
#[test]
fn a_visible_color_array_replaces_stale_secondary_colors() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x47, 0xa1, 2, 9, 0, 9, 0x41, 9, 0x48, 9, 0x49, 0x15, 0, 0x25, 3,
        0x75, 8, 0x95, 2, 0x91, 0, 0xc0, 0xc0,
    ]);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let off = update(&mut link, &mut catalog, &mut forward, false, &[2, 3]);
    assert_eq!(off, [1, 0]);
    assert_eq!(
        update(&mut link, &mut catalog, &mut forward, true, &[2, 0]),
        [2, 0]
    );
    let off = update(&mut link, &mut catalog, &mut forward, false, &[2, 0]);
    assert_eq!(
        update(&mut link, &mut catalog, &mut forward, true, &off),
        [2, 0]
    );
}

#[test]
fn a_color_array_preserves_out_of_range_empty_slots() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x47, 0xa1, 2, 9, 0x41, 9, 0x48, 9, 0x49, 0x15, 1, 0x25, 3, 0x75,
        8, 0x95, 2, 0x91, 0, 0xc0, 0xc0,
    ]);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let off = update(&mut link, &mut catalog, &mut forward, false, &[2, 3]);
    assert_eq!(off, [1, 0]);
    assert_eq!(
        update(&mut link, &mut catalog, &mut forward, true, &[2, 0]),
        [2, 0]
    );
    let off = update(&mut link, &mut catalog, &mut forward, false, &[2, 0]);
    assert_eq!(
        update(&mut link, &mut catalog, &mut forward, true, &off),
        [2, 0]
    );
}
#[test]
fn feature_indicators_use_feature_set_report() {
    let bytes = descriptor(&[5, 8, 9, 2, 0x75, 1, 0x95, 1, 0xb1, 2]);
    let (mut link, _, _) = runtime(&bytes);
    let output = link.output(2, 0).unwrap().unwrap();
    assert_eq!(output.kind, ReportType::Feature);
    assert_eq!(output.payload, [1]);
}
#[test]
fn indicator_failures_include_the_elements_actual_bit_and_usage() {
    let bytes = descriptor(&[
        5, 8, 9, 1, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x91, 2, 9, 2, 0x15, 2, 0x25, 3, 0x75, 2,
        0x91, 2,
    ]);
    let map = Map::compile(&bytes).unwrap();
    let error = map
        .indicator_report(0, 2, IndicatorValue::default(), None, false, &mut [0])
        .unwrap_err();
    assert_eq!(error.reason, IndicatorError::Range);
    assert_eq!(error.bit_offset, Some(1));
    assert_eq!(error.usage, Some(0x80002));
}

fn relative_caps(
    link: &mut Link,
    catalog: &mut Catalog,
    forward: &mut Forwarder,
    feedback: u8,
    start: u64,
) -> Vec<Vec<u8>> {
    let mut writes = Vec::new();
    for now in start..start + 32 {
        if let Some(output) = link.output(2, now).unwrap() {
            let id = output.id;
            writes.push(output.payload.to_vec());
            link.output_complete(id, Ok(()), catalog, forward, now)
                .unwrap();
        } else if let Some(read) = link.report_read(catalog, now) {
            if read.kind == ReportType::Input {
                let report = InputReport::new(link.id, read.service, 0, &[feedback]).unwrap();
                link.report_read_complete(read.id, read.kind, Ok(&report), catalog, now);
            } else {
                link.report_read_complete(
                    read.id,
                    read.kind,
                    Err(cordial_core::model::errors::ErrorCode::UnsupportedHid),
                    catalog,
                    now,
                );
            }
        }
    }
    writes
}
#[test]
fn only_the_unknown_scoped_indicator_gets_an_unknown_state_warning() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 0, 0x75, 1, 0x95, 1, 9, 2, 0x91, 6, 9, 2, 0x81, 2, 0xc0, 9, 2, 0xa1, 0,
        9, 2, 0x91, 6, 0xc0,
    ]);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    assert_eq!(
        relative_caps(&mut link, &mut catalog, &mut forward, 0, 0),
        vec![vec![0], vec![1]]
    );
    assert_eq!(link.warnings.len(), 1);
    let warning = link.warnings[0];
    assert_eq!(
        warning.code,
        cordial_core::model::errors::WarningCode::IndicatorStateUnknown
    );
    assert_eq!(warning.bit_offset, Some(1));
    assert_eq!(warning.report_type, Some(ReportType::Output));
}
#[test]
fn conflicting_live_feedback_is_reread_and_actual_device_resets_are_corrected() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 0, 0x75, 1, 0x95, 1, 9, 2, 0x91, 6, 9, 2, 0x81, 2, 0xc0,
    ]);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    assert_eq!(
        relative_caps(&mut link, &mut catalog, &mut forward, 0, 0),
        vec![vec![0], vec![1]]
    );
    // A queued pre-write sample disagrees, but a fresh read still reports On.
    link.input(ServiceId(1), 0, &[0], &mut catalog, &mut forward, 40)
        .unwrap();
    assert!(
        relative_caps(&mut link, &mut catalog, &mut forward, 2, 41)
            .iter()
            .all(|bytes| bytes == &[0])
    );
    // A later reset produces the same notification; fresh Off feedback now
    // establishes the state needed for exactly one corrective toggle.
    link.input(ServiceId(1), 0, &[0], &mut catalog, &mut forward, 80)
        .unwrap();
    let writes = relative_caps(&mut link, &mut catalog, &mut forward, 0, 81);
    assert_eq!(writes.iter().filter(|bytes| bytes == &&vec![1]).count(), 1);
    assert!(link.warnings.is_empty());
}
#[test]
fn positive_only_rgb_channels_can_use_a_separate_intensity_gate() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x52, 0xa1, 2, 0x15, 1, 0x26, 0xff, 0, 0x75, 8, 0x95, 3, 0x19,
        0x53, 0x29, 0x55, 0x91, 2, 0x15, 0, 0x25, 100, 0x95, 1, 9, 0x56, 0x91, 2, 0xc0, 0xc0,
    ]);
    assert_eq!(
        encode(&bytes, false, Some(&[255, 1, 1, 30])).unwrap(),
        [255, 1, 1, 0]
    );
    assert_eq!(
        encode(&bytes, true, Some(&[255, 1, 1, 0])).unwrap(),
        [255, 1, 1, 100]
    );
}

#[test]
fn usage_selected_arrays_accept_direct_led_selectors_and_keyboard_aliases() {
    for selections in [
        &[5, 8, 9, 0, 9, 1, 9, 2][..],
        &[5, 7, 9, 0, 9, 0x53, 9, 0x39][..],
    ] {
        let mut items = vec![5, 8, 9, 0x3a, 0xa1, 2];
        items.extend_from_slice(selections);
        items.extend_from_slice(&[0x15, 0, 0x25, 2, 0x75, 8, 0x95, 1, 0x91, 0, 0xc0]);
        assert_eq!(encode(&descriptor(&items), true, None).unwrap(), [2]);
        assert_eq!(encode(&descriptor(&items), false, None).unwrap(), [0]);
    }
}
#[test]
fn rgb_colors_survive_updates_across_output_and_feature_reports() {
    let mut bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x52, 0xa1, 2, 0x15, 0, 0x26, 0xff, 0, 0x75, 8, 0x95, 1, 0x85, 1,
        9, 0x53, 0x91, 2, 0x85, 2, 9, 0x54, 9, 0x55, 0x95, 2, 0xb1, 2, 0xc0, 0xc0,
    ]);
    bytes.splice(6..6, [0x85, 10]);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let mut reports = [
        (ReportType::Output, 1, vec![255]),
        (ReportType::Feature, 2, vec![128, 0]),
    ];
    for on in [false, true, false, true] {
        for now in 0..64 {
            if let Some(output) = link.output(if on { 2 } else { 0 }, now).unwrap() {
                let row = reports
                    .iter_mut()
                    .find(|r| r.0 == output.kind && r.1 == output.report_id.unwrap())
                    .unwrap();
                row.2 = output.payload.to_vec();
                let id = output.id;
                link.output_complete(id, Ok(()), &mut catalog, &mut forward, now)
                    .unwrap();
            } else if let Some(read) = link.report_read(&catalog, now) {
                let row = reports
                    .iter()
                    .find(|r| r.0 == read.kind && r.1 == read.report_id.unwrap())
                    .unwrap();
                let report = InputReport::new(link.id, read.service, row.1, &row.2).unwrap();
                link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
            }
        }
        assert_eq!(reports[0].2, if on { vec![255] } else { vec![0] });
        assert_eq!(reports[1].2, if on { vec![128, 0] } else { vec![0, 0] });
        assert!(link.warnings.is_empty(), "{:?}", link.warnings);
    }
}
fn relative_rgb(
    extra_output: &[u8],
    relative_globals: &[u8],
    feedback_globals: &[u8],
    flags: u8,
) -> Vec<u8> {
    let mut items = vec![5, 8, 9, 2, 0xa1, 2, 9, 0x52, 0xa1, 2, 0x85, 1];
    items.extend_from_slice(relative_globals);
    items.extend_from_slice(&[0x75, 8, 0x95, 3, 0x19, 0x53, 0x29, 0x55, 0x91, flags]);
    items.extend_from_slice(extra_output);
    items.extend_from_slice(&[0x85, 2]);
    items.extend_from_slice(feedback_globals);
    items.extend_from_slice(&[
        0x75, 8, 0x95, 3, 0x19, 0x53, 0x29, 0x55, 0x81, 2, 0xc0, 0xc0,
    ]);
    let mut bytes = descriptor(&items);
    bytes.splice(6..6, [0x85, 10]);
    bytes
}

#[test]
fn relative_rgb_deltas_restore_the_whole_tuple_without_edge_rearm() {
    let bytes = relative_rgb(&[], &[0x15, 0xf6, 0x25, 10], &[0x15, 0, 0x26, 0xff, 0], 6);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let mut color = [30, 0, 10];
    for (epoch, on) in [false, true, false, true].into_iter().enumerate() {
        let mut writes = 0;
        for now in epoch as u64 * 100..epoch as u64 * 100 + 64 {
            if let Some(output) = link.output(if on { 2 } else { 0 }, now).unwrap() {
                assert_eq!(output.report_id, Some(1));
                for (current, &delta) in color.iter_mut().zip(output.payload) {
                    let delta = delta as i8;
                    assert!((-10..=10).contains(&delta));
                    *current = (i16::from(*current) + i16::from(delta)) as u8;
                }
                writes += 1;
                let id = output.id;
                link.output_complete(id, Ok(()), &mut catalog, &mut forward, now)
                    .unwrap();
            } else if let Some(read) = link.report_read(&catalog, now) {
                assert_eq!(read.kind, ReportType::Input);
                let report = InputReport::new(link.id, read.service, 2, &color).unwrap();
                link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
            }
        }
        assert_eq!(color, if on { [30, 0, 10] } else { [0, 0, 0] });
        assert_eq!(writes, 3);
        assert!(link.warnings.is_empty(), "{:?}", link.warnings);
    }
}

#[test]
fn live_numeric_feedback_before_ack_is_not_applied_twice_and_neutral_updates_converge() {
    let bytes = relative_rgb(&[], &[0x15, 0xf6, 0x25, 10], &[0x15, 0, 0x26, 0xff, 0], 6);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let mut color = [20, 0, 0];
    let mut writes = 0;
    for now in 0..64 {
        if let Some(output) = link.output(0, now).unwrap() {
            let id = output.id;
            for (current, &delta) in color.iter_mut().zip(output.payload) {
                *current = (i16::from(*current) + i16::from(delta as i8)) as u8;
            }
            writes += 1;
            link.input(ServiceId(1), 2, &color, &mut catalog, &mut forward, now)
                .unwrap();
            link.output_complete(id, Ok(()), &mut catalog, &mut forward, now)
                .unwrap();
        } else if let Some(read) = link.report_read(&catalog, now) {
            let report = InputReport::new(link.id, read.service, 2, &color).unwrap();
            link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
        }
    }
    assert_eq!(color, [0, 0, 0]);
    assert_eq!(
        writes, 3,
        "two real deltas and one confirmed neutral update"
    );
    assert!(link.warnings.is_empty(), "{:?}", link.warnings);
}

#[test]
fn numeric_aliases_use_one_writer_for_each_shared_channel() {
    let alias = [0x85, 3, 0x19, 0x53, 0x29, 0x55, 0xb1, 6];
    let bytes = relative_rgb(
        &alias,
        &[0x15, 0xf6, 0x25, 10],
        &[0x15, 0, 0x26, 0xff, 0],
        6,
    );
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let mut color = [20, 0, 0];
    for now in 0..64 {
        if let Some(output) = link.output(0, now).unwrap() {
            if output.kind == ReportType::Feature {
                assert_eq!(output.payload, [0, 0, 0]);
            }
            for (current, &delta) in color.iter_mut().zip(output.payload) {
                *current = (i16::from(*current) + i16::from(delta as i8)) as u8;
            }
            let id = output.id;
            link.output_complete(id, Ok(()), &mut catalog, &mut forward, now)
                .unwrap();
        } else if let Some(read) = link.report_read(&catalog, now) {
            assert_eq!(read.kind, ReportType::Input);
            let report = InputReport::new(link.id, read.service, 2, &color).unwrap();
            link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
        }
    }
    assert_eq!(color, [0, 0, 0]);
    assert!(link.warnings.is_empty(), "{:?}", link.warnings);
}

#[test]
fn nonlinear_numeric_leds_have_a_specific_diagnostic() {
    let bytes = relative_rgb(
        &[],
        &[0x15, 0xf6, 0x25, 10],
        &[0x15, 0, 0x26, 0xff, 0],
        0x16,
    );
    let (mut link, mut catalog, _) = runtime(&bytes);
    for now in 0..32 {
        assert!(link.output(0, now).unwrap().is_none());
        if let Some(read) = link.report_read(&catalog, now) {
            let report = InputReport::new(link.id, read.service, 2, &[20, 0, 0]).unwrap();
            link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
        }
    }
    assert_eq!(link.warnings.len(), 1);
    assert_eq!(
        link.warnings[0].code,
        cordial_core::model::errors::WarningCode::IndicatorNonlinearUnsupported
    );
}

#[test]
fn direct_and_keyboard_selectors_in_one_array_control_both_lights() {
    let items = [
        5, 8, 9, 0x3a, 0xa1, 2, 9, 0, 9, 2, 0x0b, 0x39, 0, 7, 0, 0x15, 0, 0x25, 2, 0x75, 8, 0x95,
        2, 0x91, 0, 0xc0,
    ];
    assert_eq!(encode(&descriptor(&items), true, None).unwrap(), [1, 2]);
    assert_eq!(encode(&descriptor(&items), false, None).unwrap(), [0, 0]);
}

#[test]
fn relative_selector_aliases_have_independent_feedback_states() {
    let items = [
        5, 8, 9, 0x3a, 0xa1, 2, 0x85, 1, 9, 0, 9, 2, 0x0b, 0x39, 0, 7, 0, 0x15, 0, 0x25, 2, 0x75,
        8, 0x95, 2, 0x91, 4, 0x85, 2, 9, 0, 9, 2, 0x0b, 0x39, 0, 7, 0, 0x81, 0, 0xc0,
    ];
    let mut bytes = descriptor(&items);
    bytes.splice(6..6, [0x85, 10]);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let mut writes = Vec::new();
    for now in 0..32 {
        if let Some(output) = link.output(2, now).unwrap() {
            writes.push(output.payload.to_vec());
            let id = output.id;
            link.output_complete(id, Ok(()), &mut catalog, &mut forward, now)
                .unwrap();
        } else if let Some(read) = link.report_read(&catalog, now) {
            assert_eq!(read.kind, ReportType::Input);
            let report = InputReport::new(link.id, read.service, 2, &[1, 0]).unwrap();
            link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
        }
    }
    assert_eq!(writes, [vec![0, 0], vec![2, 0]]);
    assert!(link.warnings.is_empty(), "{:?}", link.warnings);
}

#[test]
fn ordinal_rgb_instances_keep_separate_color_tuples() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x52, 0xa1, 2, 5, 0xa, 9, 1, 0xa1, 2, 5, 8, 0x15, 0, 0x26, 0xff, 0,
        0x75, 8, 0x95, 3, 0x19, 0x53, 0x29, 0x55, 0x91, 2, 0xc0, 5, 0xa, 9, 2, 0xa1, 2, 5, 8, 0x19,
        0x53, 0x29, 0x55, 0x91, 2, 0xc0, 0xc0, 0xc0,
    ]);
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let off = update(
        &mut link,
        &mut catalog,
        &mut forward,
        false,
        &[0, 0, 0, 0, 128, 0],
    );
    assert_eq!(off, [0; 6]);
    assert_eq!(
        update(&mut link, &mut catalog, &mut forward, true, &off),
        [255, 255, 255, 0, 128, 0]
    );
}

#[test]
fn preserved_volatile_rgb_fields_use_no_change_values_without_a_read() {
    let bytes = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x3c, 0xa1, 2, 9, 0x3d, 9, 0x41, 0x75, 8, 0x95, 1, 0x91, 0, 0xc0,
        9, 0x52, 0xa1, 2, 0x15, 0, 0x26, 0xff, 0, 0x75, 16, 0x95, 3, 0x19, 0x53, 0x29, 0x55, 0x91,
        0x82, 0xc0, 0xc0,
    ]);
    assert_eq!(encode(&bytes, false, None).unwrap(), [1, 0, 1, 0, 1, 0, 1]);
}

#[test]
fn relative_values_use_physical_scales_and_equivalent_unit_systems() {
    let bytes = relative_rgb(
        &[],
        &[
            0x15, 0xf6, 0x25, 10, 0x35, 0xfb, 0x45, 5, 0x67, 3, 0, 0x10, 0,
        ],
        &[0x15, 0, 0x25, 100, 0x35, 0, 0x45, 100, 0x67, 1, 0, 0x10, 0],
        6,
    );
    let (mut link, mut catalog, mut forward) = runtime(&bytes);
    let mut color = [20, 0, 0];
    let mut writes = 0;
    for now in 0..64 {
        if let Some(output) = link.output(0, now).unwrap() {
            for (current, &delta) in color.iter_mut().zip(output.payload) {
                assert_eq!((delta as i8) % 2, 0);
                *current = (i16::from(*current) + i16::from(delta as i8) / 2) as u8;
            }
            writes += 1;
            let id = output.id;
            link.output_complete(id, Ok(()), &mut catalog, &mut forward, now)
                .unwrap();
        } else if let Some(read) = link.report_read(&catalog, now) {
            let report = InputReport::new(link.id, read.service, 2, &color).unwrap();
            link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
        }
    }
    assert_eq!(color, [0, 0, 0]);
    assert_eq!(writes, 4);
    assert!(link.warnings.is_empty(), "{:?}", link.warnings);
}

#[test]
fn unreachable_relative_targets_report_a_range_error_without_writing() {
    let bytes = relative_rgb(
        &[],
        &[0x15, 0xf6, 0x25, 10, 0x35, 0xec, 0x45, 20],
        &[0x15, 0, 0x25, 100, 0x35, 0, 0x45, 100],
        6,
    );
    let (mut link, mut catalog, _) = runtime(&bytes);
    for now in 0..32 {
        assert!(link.output(0, now).unwrap().is_none());
        if let Some(read) = link.report_read(&catalog, now) {
            let report = InputReport::new(link.id, read.service, 2, &[3, 0, 0]).unwrap();
            link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
        }
    }
    assert_eq!(link.warnings.len(), 1);
    assert_eq!(
        link.warnings[0].code,
        cordial_core::model::errors::WarningCode::IndicatorRangeUnsupported
    );
}

#[test]
fn unencodable_trailing_selector_does_not_block_a_reachable_lock() {
    let bytes = descriptor(&[
        5, 8, 9, 0, 9, 1, 9, 2, 0x15, 0, 0x25, 1, 0x75, 8, 0x95, 1, 0x91, 0,
    ]);
    let map = Map::compile(&bytes).unwrap();
    let mut out = [0];
    assert!(
        map.indicator_report(0, 3, IndicatorValue::default(), None, false, &mut out)
            .unwrap()
            .is_some()
    );
    assert_eq!(out, [1]);
}
#[test]
fn an_indicator_whose_range_does_not_fit_its_field_is_left_unused() {
    for globals in [
        &[0x15, 0xff, 0x25, 1, 0x75, 1][..],
        &[0x15, 0, 0x26, 0xff, 0, 0x75, 4][..],
    ] {
        let mut indicator = vec![5, 8, 9, 2];
        indicator.extend_from_slice(globals);
        indicator.extend_from_slice(&[0x95, 1, 0x91, 6]);
        let map = Map::compile(&descriptor(&indicator)).unwrap();
        let limitation = map.limitations()[0];
        assert_eq!(
            limitation.code,
            cordial_core::model::errors::WarningCode::IndicatorRangeUnsupported
        );
        assert_eq!(
            (limitation.kind, limitation.usage_page, limitation.usage),
            (1, 8, 2)
        );
        // The keyboard's own input still decodes.
        let input = map.decode(&mut map.state().unwrap(), 0, &[1]).unwrap();
        assert!(input.held.keys[0] & 0x10 != 0);
    }
}

#[test]
fn transient_feedback_reads_retry_with_the_feedback_report_context() {
    use cordial_core::model::errors::{ErrorCode, WarningCode};
    let mut toggle = descriptor(&[
        5, 8, 9, 2, 0xa1, 0, 0x85, 1, 0x75, 1, 0x95, 1, 9, 2, 0x91, 6, 0x85, 2, 9, 2, 0x81, 2, 0xc0,
    ]);
    toggle.splice(6..6, [0x85, 10]);
    let rgb = relative_rgb(&[], &[0x15, 0xf6, 0x25, 10], &[0x15, 0, 0x26, 0xff, 0], 6);
    for (bytes, feedback) in [(toggle, vec![1]), (rgb, vec![2, 0, 1])] {
        let (mut link, mut catalog, mut forward) = runtime(&bytes);
        let mut reads = 0;
        let mut failed_at = 0;
        for now in 0..1100 {
            if let Some(output) = link.output(0, now).unwrap() {
                if reads == 1 {
                    panic!("indicator wrote before feedback retry");
                }
                let id = output.id;
                link.output_complete(id, Ok(()), &mut catalog, &mut forward, now)
                    .unwrap();
            } else if let Some(read) = link.report_read(&catalog, now) {
                assert_eq!(read.report_id, Some(2));
                assert_eq!(read.kind, ReportType::Input);
                if reads == 0 {
                    failed_at = now;
                    link.report_read_complete(
                        read.id,
                        read.kind,
                        Err(ErrorCode::Timeout),
                        &mut catalog,
                        now,
                    );
                    assert!(
                        link.warnings
                            .iter()
                            .any(|w| w.code == WarningCode::IndicatorReadFailed
                                && w.report_id == Some(2)
                                && w.report_type == Some(ReportType::Input))
                    );
                } else {
                    assert!(now >= failed_at + 1000);
                    let report = InputReport::new(link.id, read.service, 2, &feedback).unwrap();
                    link.report_read_complete(read.id, read.kind, Ok(&report), &mut catalog, now);
                }
                reads += 1;
            }
        }
        assert!(reads >= 2);
        assert!(link.warnings.is_empty(), "{:?}", link.warnings);
    }
}

#[test]
fn permanent_read_capacity_errors_allow_optional_colors_and_settle_required_baselines() {
    use cordial_core::model::errors::{ErrorCode, WarningCode};
    let pure_rgb = descriptor(&[
        5, 8, 9, 2, 0xa1, 2, 9, 0x52, 0xa1, 2, 0x15, 0, 0x26, 0xff, 0, 0x75, 8, 0x95, 3, 0x19,
        0x53, 0x29, 0x55, 0x91, 2, 0xc0, 0xc0,
    ]);
    let mixed = descriptor(&[
        5, 8, 9, 2, 0x75, 1, 0x95, 1, 0x91, 2, 0x06, 0, 0xff, 9, 1, 0x91, 2,
    ]);
    for (bytes, can_write) in [(pure_rgb, true), (mixed, false)] {
        let (mut link, mut catalog, mut forward) = runtime(&bytes);
        let mut reads = 0;
        let mut writes = 0;
        for now in 0..2000 {
            if let Some(output) = link.output(2, now).unwrap() {
                writes += 1;
                let id = output.id;
                link.output_complete(id, Ok(()), &mut catalog, &mut forward, now)
                    .unwrap();
            } else if let Some(read) = link.report_read(&catalog, now) {
                reads += 1;
                link.report_read_complete(
                    read.id,
                    read.kind,
                    Err(ErrorCode::HidReportTooLarge),
                    &mut catalog,
                    now,
                );
            }
        }
        assert_eq!(reads, 1);
        assert_eq!(writes > 0, can_write);
        if !can_write {
            assert!(
                link.warnings
                    .iter()
                    .any(|w| w.code == WarningCode::IndicatorReportTooLarge)
            );
        }
    }
}

#[test]
fn a_one_bit_led_inheriting_a_wide_logical_maximum_is_an_indicator() {
    // Keyboard collection of a BLE keyboard: the LED output follows an 8-bit key array and keeps
    // its Logical Maximum of 255.
    let bytes = [
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
        0x95, 0x05, 0x75, 0x08, 0x15, 0x00, 0x26, 0xff, 0x00, 0x05, 0x07, 0x19, 0x00, 0x29, 0xff,
        0x81, 0x00, 0x05, 0xff, 0x09, 0x03, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0x95, 0x05, 0x75,
        0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01,
        0xc0,
    ];
    let map = Map::compile(&bytes).unwrap();
    assert!(map.reports().iter().any(|r| r.id == 1 && r.leds));
    assert_eq!(encode(&bytes, true, None).unwrap(), [0b10]);
    assert_eq!(encode(&bytes, false, None).unwrap(), [0]);
}

#[test]
fn an_unused_absolute_field_keeps_its_bits_when_its_report_is_written() {
    // A 4-bit Num Lock field with a 0..255 range is left unused; Caps Lock follows it at bit 4.
    let bytes = descriptor(&[
        5, 8, 9, 1, 0x15, 0, 0x26, 0xff, 0, 0x75, 4, 0x95, 1, 0x91, 2, 9, 2, 0x25, 1, 0x75, 1,
        0x95, 1, 0x91, 2, 0x75, 3, 0x95, 1, 0x91, 1,
    ]);
    assert!(matches!(
        encode(&bytes, true, None),
        Err(IndicatorError::ReadRequired)
    ));
    assert_eq!(encode(&bytes, true, Some(&[0x05])).unwrap(), [0x15]);
    assert_eq!(encode(&bytes, false, Some(&[0x15])).unwrap(), [0x05]);
}

#[test]
fn a_report_with_an_unused_relative_field_is_not_written() {
    // An unused 4-bit absolute field, an unused 4-bit relative field, then Caps Lock.
    let bytes = descriptor(&[
        5, 8, 9, 1, 0x15, 0, 0x26, 0xff, 0, 0x75, 4, 0x95, 1, 0x91, 2, 9, 3, 0x91, 6, 9, 2, 0x25,
        1, 0x75, 1, 0x95, 1, 0x91, 2, 0x75, 7, 0x95, 1, 0x91, 1,
    ]);
    let map = Map::compile(&bytes).unwrap();
    assert_eq!(map.limitations().len(), 2);
    for baseline in [None, Some(&[0x35, 0][..])] {
        assert!(matches!(
            encode(&bytes, true, baseline),
            Err(IndicatorError::Range)
        ));
    }
}
