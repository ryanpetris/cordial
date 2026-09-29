mod common;
use base64::Engine;
use cordial_client::{
    client::{Cancellation, Wait},
    storage,
};
use cordial_protocol::{
    messages::{Command, Empty, Message, StoragePath},
    payloads::FileType,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

fn temp_dir(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("cordial-storage-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn chunk(
    firmware: &common::Firmware,
    id: cordial_protocol::identifiers::RequestId,
    offset: usize,
    bytes: &[u8],
) {
    firmware.send(&Message::<Value, ()>::success(
        id,
        json!({"offset":offset,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        false,
    ));
}

#[test]
fn download_publishes_only_complete_exact_bytes_and_never_logs_payload() {
    for success in [false, true] {
        let (client, firmware) = common::connect();
        let client = Arc::new(client);
        let root = temp_dir(&format!("exact-{success}"));
        let destination = root.join("device.json");
        fs::write(&destination, b"previous").unwrap();
        let c = client.clone();
        let path = destination.clone();
        let progress = Arc::new(Mutex::new(Vec::new()));
        let seen = progress.clone();
        let job = thread::spawn(move || {
            storage::read(
                &c,
                "/device.json",
                &path,
                true,
                &Wait::timeout(Duration::from_secs(2)),
                |n| seen.lock().unwrap().push(n),
            )
        });
        let request = firmware.receive();
        let bytes: Vec<u8> = (0..14000).map(|n| (n % 251) as u8).collect();
        for (i, part) in bytes.chunks(512).enumerate() {
            chunk(&firmware, request.id, i * 512, part);
        }
        if success {
            firmware.reply(&request, json!({"bytes":bytes.len()}));
        } else {
            firmware.disconnect();
        }
        let result = job.join().unwrap();
        assert_eq!(result.is_ok(), success);
        if success {
            assert_eq!(result.unwrap(), bytes.len() as u64);
            assert_eq!(
                *progress.lock().unwrap().last().unwrap(),
                bytes.len() as u64
            );
        }
        assert_eq!(
            fs::read(&destination).unwrap(),
            if success { bytes } else { b"previous".to_vec() }
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1, "no temporary left");
        #[cfg(unix)]
        if success {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        while let Ok(Some(message)) = client.next(Duration::ZERO) {
            assert_ne!(message.command, Some("storage.read"));
        }
        client.shutdown();
        fs::remove_dir_all(&root).unwrap();
    }
}

#[test]
fn existing_destination_needs_overwrite_and_mutation_publishes_nothing() {
    let (client, firmware) = common::connect();
    let root = temp_dir("mutation");
    let destination = root.join("a.bin");
    fs::write(&destination, b"keep").unwrap();
    let wait = Wait::timeout(Duration::from_secs(2));
    let error = storage::read(&client, "/a.bin", &destination, false, &wait, |_| {}).unwrap_err();
    assert_eq!(error.message, storage::EXISTS);
    assert!(
        firmware
            .requests
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );
    assert!(
        storage::read(&client, "/a.bin", &root, true, &wait, |_| {}).is_err(),
        "directory"
    );
    let client = Arc::new(client);
    let c = client.clone();
    let path = destination.clone();
    let job = thread::spawn(move || {
        storage::read(
            &c,
            "/a.bin",
            &path,
            true,
            &Wait::timeout(Duration::from_secs(2)),
            |_| {},
        )
    });
    let request = firmware.receive();
    chunk(&firmware, request.id, 0, b"partial");
    firmware.send(&Message::<(), Value>::failure(
        request.id,
        cordial_protocol::errors::ErrorCode::StorageChanged.into(),
    ));
    let error = job.join().unwrap().unwrap_err();
    assert_eq!(
        error.wire.unwrap().code,
        cordial_protocol::errors::ErrorCode::StorageChanged
    );
    assert_eq!(fs::read(&destination).unwrap(), b"keep");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    client.shutdown();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn cancelled_download_sends_cancel_and_keeps_control_traffic_working() {
    let (client, firmware) = common::connect();
    let client = Arc::new(client);
    let root = temp_dir("cancel");
    let destination = root.join("big.bin");
    let cancellation = Cancellation::default();
    let wait = Wait {
        deadline: Some(std::time::Instant::now() + Duration::from_secs(5)),
        cancellation: cancellation.clone(),
    };
    let c = client.clone();
    let path = destination.clone();
    let job = thread::spawn(move || storage::read(&c, "/big.bin", &path, false, &wait, |_| {}));
    let request = firmware.receive();
    chunk(&firmware, request.id, 0, &[7; 512]);
    // Status is answered while the transfer is open.
    client
        .call(
            Command::Status(Empty {}),
            false,
            &Wait::timeout(Duration::from_secs(2)),
        )
        .unwrap();
    cancellation.cancel();
    assert!(job.join().unwrap().is_err());
    // The harness acknowledges request.cancel itself; it was sent.
    thread::sleep(Duration::from_millis(50));
    assert!(
        firmware
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|n| n == "request.cancel")
    );
    firmware.send(&Message::<(), Value>::failure(
        request.id,
        cordial_protocol::errors::ErrorCode::Cancelled.into(),
    ));
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    thread::sleep(Duration::from_millis(50));
    assert!(client.error().is_none());
    client.shutdown();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn listing_hands_over_rows_and_checks_the_count() {
    let (client, firmware) = common::connect();
    let client = Arc::new(client);
    for honest in [true, false] {
        let c = client.clone();
        let job = thread::spawn(move || {
            let mut rows = Vec::new();
            storage::list(&c, "/", &Wait::timeout(Duration::from_secs(2)), |e| {
                rows.push(e)
            })
            .map(|n| (n, rows))
        });
        let request = firmware.receive();
        assert!(
            matches!(&request.command, Command::StorageList(StoragePath { path }) if path == "/")
        );
        firmware.send(&Message::<Value, ()>::success(
            request.id,
            json!({"name":"bonds","type":"directory","size":0}),
            false,
        ));
        firmware.reply(&request, json!({"count": if honest { 1 } else { 2 }}));
        let result = job.join().unwrap();
        if honest {
            let (n, rows) = result.unwrap();
            assert_eq!(n, 1);
            assert_eq!(rows[0].kind, FileType::Directory);
        } else {
            assert!(result.is_err());
        }
    }
    client.shutdown();
}

#[test]
fn briefly_slow_file_consumer_completes_within_the_bounded_queue() {
    let (client, firmware) = common::connect();
    let client = Arc::new(client);
    let c = client.clone();
    let job = thread::spawn(move || {
        let mut count = 0;
        c.stream(
            Command::StorageRead(StoragePath {
                path: "/large".into(),
            }),
            &Wait::timeout(Duration::from_secs(5)),
            |message| {
                if count == 0 {
                    thread::sleep(Duration::from_millis(300));
                }
                if !message.done() {
                    assert_eq!(message.data()?["offset"], count * 512);
                    count += 1;
                }
                Ok(())
            },
        )
        .unwrap();
        count
    });
    let request = firmware.receive();
    for i in 0..400 {
        firmware.send(&Message::<Value, ()>::success(
            request.id,
            json!({"offset":i*512,"data":"YQ=="}),
            false,
        ));
    }
    firmware.reply(&request, json!({"bytes":400*512}));
    assert_eq!(job.join().unwrap(), 400);
    assert!(client.error().is_none());
    client.shutdown();
}

#[test]
fn stalled_file_consumer_fails_only_its_transfer_while_control_continues() {
    let (client, firmware) = common::connect();
    let client = Arc::new(client);
    let c = client.clone();
    let (release, stalled) = std::sync::mpsc::channel::<()>();
    let job = thread::spawn(move || {
        let mut first = true;
        c.stream(
            Command::StorageRead(StoragePath {
                path: "/large".into(),
            }),
            &Wait::timeout(Duration::from_secs(10)),
            |_| {
                if std::mem::take(&mut first) {
                    stalled.recv().unwrap();
                }
                Ok(())
            },
        )
    });
    let request = firmware.receive();
    // Far more rows than the bounded queue holds, while the consumer is stalled.
    for i in 0..1200 {
        firmware.send(&Message::<Value, ()>::success(
            request.id,
            json!({"offset":i*512,"data":"YQ=="}),
            false,
        ));
    }
    // The reader keeps serving other requests and heartbeats meanwhile.
    let wait = Wait::timeout(Duration::from_secs(2));
    client
        .call(Command::Status(Empty {}), false, &wait)
        .unwrap();
    client
        .call(Command::Heartbeat(Empty {}), false, &wait)
        .unwrap();
    release.send(()).unwrap();
    let error = job.join().unwrap().unwrap_err();
    assert_eq!(error.message, cordial_client::client::TOO_SLOW);
    // The transfer was cancelled; its remaining rows and terminal are discarded.
    thread::sleep(Duration::from_millis(50));
    assert!(
        firmware
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|n| n == "request.cancel")
    );
    firmware.send(&Message::<Value, ()>::success(
        request.id,
        json!({"offset":1200*512,"data":"YQ=="}),
        false,
    ));
    firmware.send(&Message::<(), Value>::failure(
        request.id,
        cordial_protocol::errors::ErrorCode::Cancelled.into(),
    ));
    client
        .call(Command::Status(Empty {}), false, &wait)
        .unwrap();
    assert!(client.error().is_none());
    client.shutdown();
}
