use super::*;
use cordial_core::storage::RecordStore;

fn storage(status: &p::Status) -> &p::ResourceCapacity {
    status
        .resources
        .iter()
        .find(|r| r.r#type == p::ResourceType::Storage as i32)
        .unwrap()
}

#[test]
fn storage_format_is_validated_independently_of_radio_readiness() {
    let mut t = Test::new(false);
    assert_eq!(
        t.status().information.unwrap().storage_format,
        Some(storage::CURRENT_FORMAT)
    );
    t.event(Event::Failed(ErrorCode::RadioUnavailable));
    let status = t.status();
    assert!(!status.ready);
    assert_eq!(
        status.information.unwrap().storage_format,
        Some(storage::CURRENT_FORMAT)
    );
    let valid = t.store.records[&[0; 9]].clone();

    for format in [
        None,
        Some(vec![0xff]),
        Some(
            storage::Format {
                format: 1,
                initialized: true,
            }
            .encode_to_vec(),
        ),
    ] {
        match format {
            Some(bytes) => {
                t.store.records.insert([0; 9], bytes);
            }
            None => {
                t.store.records.remove(&[0; 9]);
            }
        }
        assert_eq!(t.status().information.unwrap().storage_format, None);
    }
    t.store.records.insert([0; 9], valid);
    t.store.fail_load = Some([0; 9]);
    assert_eq!(t.status().information.unwrap().storage_format, None);
}

#[test]
fn fallible_store_forwards_optional_total_and_startup_errors() {
    let mut store = Ok(Store {
        total_bytes: Some(4096),
        ..Store::default()
    });
    assert_eq!(block_on(store.total_bytes()), Ok(4096));
    store.as_mut().unwrap().total_bytes = None;
    assert_eq!(
        block_on(store.total_bytes()),
        Err(cordial_core::storage::Error::Unavailable)
    );
    let mut failed: Result<Store, _> = Err(cordial_core::storage::Error::Layout);
    assert_eq!(
        block_on(failed.total_bytes()),
        Err(cordial_core::storage::Error::Layout)
    );
}

#[test]
fn storage_bytes_keep_zero_unknown_and_failed_measurements_distinct() {
    let mut t = Test::new(false);
    t.store.total_bytes = Some(1 << 20);
    t.store.available = Some(900_000);
    let status = t.status();
    assert_eq!(
        (storage(&status).count, storage(&status).limit),
        (Some(148_576), Some(1 << 20))
    );
    t.store.available = t.store.total_bytes;
    assert_eq!(storage(&t.status()).count, Some(0));

    t.store.fail_count = true;
    let last_admission = t.app.manager.available_bytes;
    let status = t.status();
    assert_eq!(
        (storage(&status).count, storage(&status).limit),
        (None, Some(1 << 20))
    );
    assert_eq!(t.app.manager.available_bytes, last_admission);
    assert!(status.ready);
    assert_eq!(
        status.information.unwrap().storage_format,
        Some(storage::CURRENT_FORMAT)
    );

    t.store.fail_count = false;
    t.store.total_bytes = None;
    let status = t.status();
    assert_eq!(
        (storage(&status).count, storage(&status).limit),
        (None, None)
    );
    t.store.total_bytes = Some(0);
    t.store.available = Some(0);
    let status = t.status();
    assert_eq!(
        (storage(&status).count, storage(&status).limit),
        (Some(0), Some(0))
    );
}

#[test]
fn storage_recounts_emit_changes_without_crossing_the_full_threshold() {
    let mut t = Test::new(false);
    t.store.total_bytes = Some(1 << 20);
    t.store.available = Some(900_000);
    t.status();
    t.events();

    t.store.available = Some(800_000);
    block_on(t.store.save(record_key(5, 1), &[1])).unwrap();
    let events = t.events();
    let status = events
        .iter()
        .find_map(|e| match e {
            Ev::Adapter(s) => Some(s),
            _ => None,
        })
        .unwrap();
    assert_eq!(storage(status).count, Some(248_576));
    assert!(
        !status
            .errors
            .iter()
            .any(|e| e.code == p::ErrorCode::StorageFull as i32)
    );
    assert!(!t.events().iter().any(|e| matches!(e, Ev::Adapter(_))));

    t.store.fail_count = true;
    block_on(t.store.save(record_key(5, 1), &[2])).unwrap();
    let events = t.events();
    let status = events
        .iter()
        .find_map(|e| match e {
            Ev::Adapter(s) => Some(s),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (storage(status).count, storage(status).limit),
        (None, Some(1 << 20))
    );
    assert_eq!(t.app.manager.available_bytes, 800_000);

    t.store.fail_count = false;
    let status = t.status();
    assert_eq!(storage(&status).count, Some(248_576));
    assert!(
        t.events()
            .iter()
            .any(|e| matches!(e, Ev::Adapter(s) if storage(s).count == Some(248_576)))
    );

    t.store.total_bytes = None;
    t.status();
    assert!(t.events().iter().any(
        |e| matches!(e, Ev::Adapter(s) if storage(s).count.is_none() && storage(s).limit.is_none())
    ));
    t.store.total_bytes = Some(1 << 20);
    t.store.fail_count = true;
    t.status();
    assert!(t.events().iter().any(|e| matches!(e, Ev::Adapter(s) if storage(s).count.is_none() && storage(s).limit == Some(1 << 20))));
}
