use cordial_core::control::Session;
use cordial_protocol::{
    self as p,
    frame::{self, Decoder},
    message::Kind as M,
    request::Command,
};
use prost::Message;

fn request(command: Command) -> Vec<u8> {
    let mut bytes = Vec::new();
    frame::encode(
        &p::Request {
            command: Some(command),
        },
        &mut bytes,
    );
    bytes
}

fn hello() -> Vec<u8> {
    let mut bytes = Vec::new();
    frame::encode_bytes(
        &p::handshake::encode_client_hello(&p::ClientHello {}).unwrap(),
        &mut bytes,
    );
    bytes
}

fn negotiate(s: &mut Session) -> p::AdapterHello {
    let bytes = hello();
    assert_eq!(s.feed(&bytes), (bytes.len(), None));
    let mut decoder = Decoder::new(Some(p::MAX_HANDSHAKE_BYTES));
    let mut received = None;
    while let Some((token, bytes)) = s.output(7) {
        let bytes = bytes.to_vec();
        for byte in &bytes {
            if let Some(frame) = decoder.push(*byte) {
                assert!(received.is_none());
                received = Some(p::handshake::decode_adapter_hello(frame.unwrap()).unwrap());
            }
        }
        s.output_complete(token, bytes.len());
    }
    assert!(s.open());
    received.unwrap()
}

fn connect(s: &mut Session) {
    s.session(true);
}

#[test]
fn raw_file_streams_require_an_open_session() {
    let mut session = Session::new();
    session.start_stream();
    session.stream_bytes(&[1, 2, 3], true);
    assert_eq!(session.queued(), 0);
    assert!(session.output(63).is_none());
    session.session(true);
    session.start_stream();
    let message = p::Message {
        kind: Some(M::Response(p::Response {
            result: Some(p::response::Result::File(p::FileData {
                path: "/data".into(),
                data: vec![1, 2, 3],
                ..Default::default()
            })),
        })),
    };
    session.stream_bytes(&message.encode_to_vec(), true);
    assert_eq!(drain(&mut session, 2), vec![message.kind.unwrap()]);
}

fn status() -> Vec<u8> {
    request(Command::GetStatus(p::GetStatus {}))
}

/// Drains the output in chunks of `chunk` bytes and decodes the frames.
fn drain(s: &mut Session, chunk: usize) -> Vec<M> {
    let mut decoder = Decoder::new(None);
    let mut out = Vec::new();
    while let Some((token, data)) = s.output(chunk) {
        let data = data.to_vec();
        s.output_complete(token, data.len());
        for b in data {
            if let Some(frame) = decoder.push(b) {
                out.push(p::Message::decode(frame.unwrap()).unwrap().kind.unwrap());
            }
        }
    }
    out
}

fn ok() -> p::Response {
    p::Response { result: None }
}

#[test]
fn reads_the_next_request_only_after_the_response_is_written() {
    let mut s = Session::new();
    connect(&mut s);
    let mut bytes = status();
    bytes.extend(status());
    let first = status().len();
    let (n, r) = s.feed(&bytes);
    assert_eq!(n, first);
    assert!(r.is_some());
    s.respond(ok());
    // The second request waits while the first response is outstanding.
    assert_eq!(s.feed(&bytes[first..]), (0, None));
    assert!(!s.idle());
    let out = drain(&mut s, 7);
    assert_eq!(out, vec![M::Response(ok())]);
    let (n, r) = s.feed(&bytes[first..]);
    assert_eq!(n, bytes.len() - first);
    assert!(r.is_some());
}

#[test]
fn events_wait_for_a_free_output_and_never_split_a_frame() {
    let mut s = Session::new();
    connect(&mut s);
    assert!(s.idle());
    s.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
        id: 1_234_567,
    }));
    assert!(!s.idle());
    // A response queued behind a partly written event follows it whole.
    let (token, chunk) = s.output(3).unwrap();
    let written = chunk.len();
    s.output_complete(token, written);
    s.respond(ok());
    let mut decoder = Decoder::new(None);
    let mut rest = Vec::new();
    while let Some((token, data)) = s.output(4) {
        rest.extend_from_slice(data);
        let n = data.len();
        s.output_complete(token, n);
    }
    let mut first = Vec::new();
    frame::encode(
        &p::Message {
            kind: Some(M::Event(p::Event {
                kind: Some(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
                    id: 1_234_567,
                })),
            })),
        },
        &mut first,
    );
    let mut frames = Vec::new();
    for b in first[..written].iter().chain(&rest) {
        if let Some(f) = decoder.push(*b) {
            frames.push(p::Message::decode(f.unwrap()).unwrap().kind.unwrap());
        }
    }
    assert_eq!(frames.len(), 2);
    assert!(matches!(frames[0], M::Event(_)));
    assert_eq!(frames[1], M::Response(ok()));
}

#[test]
fn completions_from_an_old_session_are_ignored() {
    let mut s = Session::new();
    connect(&mut s);
    s.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
        id: 1_234_567,
    }));
    let (token, _) = s.output(2).unwrap();
    assert!(s.session(true), "a new session ends the old one");
    assert!(s.open());
    s.output_complete(token, 2);
    assert!(s.output(64).is_none());
}

#[test]
fn closed_ports_take_no_input_and_send_nothing() {
    let mut s = Session::new();
    assert_eq!(s.feed(&status()), (0, None));
    s.respond(ok());
    assert!(s.output(64).is_none());
    connect(&mut s);
    let mut partial = status();
    partial.truncate(2);
    assert_eq!(s.feed(&partial), (2, None));
    assert!(s.session(false));
    connect(&mut s);
    // The partial request from the earlier session cannot merge with a new one.
    let (_, r) = s.feed(&status());
    assert!(r.is_some());
}

#[test]
fn bad_frames_are_answered_without_reaching_the_application() {
    let mut s = Session::new();
    connect(&mut s);
    let code = |s: &mut Session| match drain(s, 64).pop() {
        Some(M::Response(p::Response {
            result: Some(p::response::Result::Error(e)),
        })) => p::ErrorCode::try_from(e.code).unwrap(),
        other => panic!("{other:?}"),
    };
    // A frame that is valid COBS but not a Request.
    let mut bytes = Vec::new();
    frame::encode_bytes(&[0xff, 0xff, 0xff], &mut bytes);
    assert_eq!(s.feed(&bytes).1, None);
    assert_eq!(code(&mut s), p::ErrorCode::BadRequest);
    // A nonempty Request with no known command.
    let mut bytes = Vec::new();
    frame::encode_bytes(&[0x98, 0x06, 1], &mut bytes);
    assert_eq!(s.feed(&bytes).1, None);
    assert_eq!(code(&mut s), p::ErrorCode::UnknownCommand);
    let mut bytes = Vec::new();
    frame::encode_bytes(
        &vec![1; cordial_protocol::MAX_REQUEST_BYTES + 1],
        &mut bytes,
    );
    assert_eq!(s.feed(&bytes).1, None);
    assert_eq!(code(&mut s), p::ErrorCode::TooLong);
    // Bare delimiters are skipped silently.
    assert_eq!(s.feed(&[0, 0, 0]), (3, None));
    assert!(s.idle());
}

#[test]
fn bootloader_entry_stops_reading_requests() {
    let mut s = Session::new();
    connect(&mut s);
    s.end(p::SessionEndingReason::Bootloader, 0);
    assert_eq!(s.feed(&status()), (0, None));
}

fn list_response(id: u32) -> p::Response {
    p::Response {
        result: Some(p::response::Result::Profiles(p::ProfileList {
            entries: vec![p::ProfileListEntry {
                entry: Some(p::profile_list_entry::Entry::Profile(p::Profile {
                    id,
                    name: "A profile".into(),
                    roles: vec![],
                })),
            }],
        })),
    }
}

#[test]
fn streamed_envelopes_merge_in_one_frame_and_late_error_replaces_the_list() {
    for fail in [false, true] {
        let mut session = Session::new();
        connect(&mut session);
        session.start_stream();
        let mut decoder = Decoder::new(None);
        let mut total = 0;
        for id in 1..=100 {
            assert!(session.stream_ready());
            session.stream_response(list_response(id), false);
            assert!(!session.idle());
            assert_eq!(session.feed(&status()), (0, None));
            while let Some((token, bytes)) = session.output(7) {
                let bytes = bytes.to_vec();
                assert!(!bytes.contains(&0));
                assert!(session.output(7).is_none());
                assert!(!session.stream_ready());
                session.output_complete(token, 0);
                let (retry, same) = session.output(7).unwrap();
                assert_eq!(same, bytes);
                total += bytes.len();
                for byte in bytes.iter().copied() {
                    assert!(decoder.push(byte).is_none());
                }
                session.output_complete(retry, bytes.len());
            }
        }
        assert!(total > 1024);
        if fail {
            session.fail(p::Error {
                code: p::ErrorCode::StorageFailed as i32,
                ..Default::default()
            });
        } else {
            session.stream_response(
                p::Response {
                    result: Some(p::response::Result::Profiles(p::ProfileList::default())),
                },
                true,
            );
        }
        let mut messages = Vec::new();
        while let Some((token, bytes)) = session.output(1) {
            let byte = bytes[0];
            if let Some(frame) = decoder.push(byte) {
                messages.push(p::Message::decode(frame.unwrap()).unwrap());
            }
            session.output_complete(token, 1);
        }
        assert_eq!(messages.len(), 1);
        let Some(M::Response(response)) = messages.pop().unwrap().kind else {
            panic!()
        };
        match response.result.unwrap() {
            p::response::Result::Error(error) => {
                assert!(fail);
                assert_eq!(error.code, p::ErrorCode::StorageFailed as i32);
            }
            p::response::Result::Profiles(list) => {
                assert!(!fail);
                assert_eq!(list.entries.len(), 100);
            }
            other => panic!("{other:?}"),
        }
        assert!(session.idle());
        session.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 7 }));
        assert!(matches!(drain(&mut session, 7).as_slice(), [M::Event(_)]));
    }
}

#[test]
fn closing_a_stream_drops_its_partial_block_and_inflight_completion() {
    let mut session = Session::new();
    connect(&mut session);
    session.start_stream();
    for id in 1..=100 {
        session.stream_response(list_response(id), false);
        if !session.stream_ready() {
            break;
        }
    }
    let (stale, _) = session.output(1).unwrap();
    session.session(false);
    connect(&mut session);
    session.respond(ok());
    session.output_complete(stale, 1);
    assert_eq!(drain(&mut session, 1), [M::Response(ok())]);
    assert!(session.idle());
}

#[test]
fn request_limit_counts_the_complete_decoded_envelope() {
    for length in [1023, 1024, 1025] {
        let mut session = Session::new();
        connect(&mut session);
        let command = Command::SetAdapter(p::SetAdapter {
            name: Some("x".repeat(length - 6)),
            ..Default::default()
        });
        let request = p::Request {
            command: Some(command),
        };
        assert_eq!(request.encoded_len(), length);
        let mut bytes = Vec::new();
        frame::encode(&request, &mut bytes);
        let (consumed, received) = session.feed(&bytes);
        assert_eq!(consumed, bytes.len());
        if length <= 1024 {
            assert_eq!(received, Some(request));
        } else {
            assert!(received.is_none());
            let messages = drain(&mut session, 64);
            let [
                M::Response(p::Response {
                    result: Some(p::response::Result::Error(error)),
                }),
            ] = messages.as_slice()
            else {
                panic!()
            };
            assert_eq!(error.code, p::ErrorCode::TooLong as i32);
        }
    }
}

#[test]
fn hello_queries_wait_for_complete_output_and_preserve_request_order() {
    let mut session = Session::new();
    session.session(true);
    assert!(session.open());
    assert!(session.idle());
    assert!(session.feed(&status()).1.is_some());
    session.respond(ok());
    assert_eq!(session.feed(&hello()), (0, None));
    assert_eq!(drain(&mut session, 1), [M::Response(ok())]);

    for _ in 0..2 {
        let query = hello();
        assert_eq!(query, [1, 0]);
        let mut input = query.clone();
        input.extend(status());
        assert_eq!(session.feed(&input), (query.len(), None));
        assert_eq!(session.queued(), 1);
        assert_eq!(session.feed(&input[query.len()..]), (0, None));
        let mut decoder = Decoder::new(Some(p::MAX_HANDSHAKE_BYTES));
        let mut received = None;
        while let Some((token, bytes)) = session.output(1) {
            let byte = bytes[0];
            if let Some(frame) = decoder.push(byte) {
                received = Some(p::handshake::decode_adapter_hello(frame.unwrap()).unwrap());
            }
            assert!(session.output(1).is_none());
            session.output_complete(token, 0);
            assert_eq!(session.feed(&status()), (0, None));
            let (retry, bytes) = session.output(1).unwrap();
            assert_eq!(bytes, [byte]);
            session.output_complete(retry, 2);
            assert_eq!(session.feed(&status()), (0, None));
            session.output_complete(retry, 1);
        }
        assert_eq!(
            received.unwrap().protocol_version,
            Some(p::CURRENT_PROTOCOL_VERSION)
        );
        assert_eq!(session.queued(), 0);
        assert!(session.feed(&input[query.len()..]).1.is_some());
        session.respond(ok());
        assert_eq!(drain(&mut session, 1), [M::Response(ok())]);
    }
}

#[test]
fn hello_queued_behind_a_partial_event_keeps_both_frames_complete() {
    let mut session = Session::new();
    session.session(true);
    session.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 7 }));
    let (token, bytes) = session.output(1).unwrap();
    let mut wire = bytes.to_vec();
    assert_eq!(session.feed(&hello()), (2, None));
    assert_eq!(session.queued(), 2);
    session.output_complete(token, wire.len());
    while let Some((token, bytes)) = session.output(1) {
        wire.extend_from_slice(bytes);
        let length = bytes.len();
        session.output_complete(token, length);
    }
    let mut decoder = Decoder::new(None);
    let frames: Vec<_> = wire
        .into_iter()
        .filter_map(|byte| decoder.push(byte).map(|frame| frame.unwrap().to_vec()))
        .collect();
    assert_eq!(frames.len(), 2);
    assert!(matches!(
        p::Message::decode(frames[0].as_slice()).unwrap().kind,
        Some(M::Event(_))
    ));
    assert_eq!(
        p::handshake::decode_adapter_hello(&frames[1])
            .unwrap()
            .protocol_version,
        Some(p::CURRENT_PROTOCOL_VERSION)
    );
}

#[test]
fn reconnect_during_hello_discards_partial_input_output_and_stale_completions() {
    let mut session = Session::new();
    session.session(true);
    let hello = hello();
    session.feed(&hello);
    let (old, _) = session.output(64).unwrap();
    session.session(true);
    assert_eq!(session.queued(), 0);
    session.feed(&hello);
    let (current, bytes) = session.output(64).unwrap();
    let length = bytes.len();
    session.output_complete(old, length);
    assert!(session.open());
    assert!(session.output(64).is_none());
    session.output_complete(current, length);
    assert!(session.open());

    session.session(true);
    let partial = &hello[..hello.len() - 1];
    assert_eq!(session.feed(partial), (partial.len(), None));
    session.session(false);
    connect(&mut session);
    assert!(session.feed(&status()).1.is_some());
}

#[test]
fn optional_metadata_never_exceeds_the_handshake_bound() {
    let mut session = Session::with_identity("test", &"x".repeat(p::MAX_HANDSHAKE_BYTES));
    session.session(true);
    let hello = negotiate(&mut session);
    assert_eq!(hello.protocol_version, Some(p::CURRENT_PROTOCOL_VERSION));
    assert!(hello.serial_number.is_none());
}

fn ending(reason: p::SessionEndingReason) -> M {
    M::Event(p::Event {
        kind: Some(p::event::Kind::SessionEnding(p::SessionEnding {
            reason: reason as i32,
        })),
    })
}

#[test]
fn session_ending_is_last_and_waits_for_its_delimiter_completion() {
    for reason in [
        p::SessionEndingReason::Unknown,
        p::SessionEndingReason::Reboot,
        p::SessionEndingReason::ConfigurationChange,
        p::SessionEndingReason::DeviceConnection,
        p::SessionEndingReason::DeviceDisconnection,
        p::SessionEndingReason::Bootloader,
    ] {
        let mut session = Session::new();
        session.session(true);
        session.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 7 }));
        session.respond(ok());
        session.end(reason, 0);
        let mut decoder = Decoder::new(None);
        let mut messages = Vec::new();
        while let Some((token, bytes)) = session.output(1) {
            let byte = bytes[0];
            assert_ne!(session.queued(), 0);
            session.end(p::SessionEndingReason::Unknown, 0);
            assert_eq!(session.feed(&status()), (0, None));
            assert_eq!(session.feed(&hello()), (0, None));
            session.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 9 }));
            session.output_complete(token, 0);
            let (retry, same) = session.output(1).unwrap();
            assert_eq!(same, [byte]);
            if let Some(frame) = decoder.push(byte) {
                messages.push(p::Message::decode(frame.unwrap()).unwrap().kind.unwrap());
            }
            assert_ne!(session.queued(), 0, "submission is not completion");
            session.output_complete(retry, 1);
        }
        assert_eq!(messages, [M::Response(ok()), ending(reason)]);
        assert_eq!(session.queued(), 0);
        assert!(!session.idle());
        assert_eq!(session.feed(&status()), (0, None));
    }
}

#[test]
fn terminal_pairing_waits_for_owed_response_and_survives_shutdown() {
    for step in [
        p::pairing::Step::Done(p::PairingDone { device: 77 }),
        p::pairing::Step::Error(p::Error {
            code: p::ErrorCode::Cancelled as i32,
            ..Default::default()
        }),
    ] {
        for started in [false, true] {
            let mut session = Session::new();
            session.session(true);
            let pairing = p::event::Kind::Pairing(p::Pairing {
                candidate: 1,
                step: Some(step.clone()),
            });
            session.event(pairing.clone());
            let mut wire = Vec::new();
            let in_flight = if started {
                let (token, bytes) = session.output(1).unwrap();
                wire.extend_from_slice(bytes);
                Some(token)
            } else {
                None
            };
            assert!(session.feed(&status()).1.is_some());
            session.respond(ok());
            session.end(p::SessionEndingReason::DeviceConnection, 0);
            assert_eq!(session.queued(), 3);
            if let Some(token) = in_flight {
                session.output_complete(token, 1);
            }
            while let Some((token, bytes)) = session.output(1) {
                wire.extend_from_slice(bytes);
                assert!(session.queued() > 0);
                session.output_complete(token, 1);
            }
            let mut decoder = Decoder::new(None);
            let messages: Vec<_> = wire
                .into_iter()
                .filter_map(|byte| {
                    decoder
                        .push(byte)
                        .map(|frame| p::Message::decode(frame.unwrap()).unwrap().kind.unwrap())
                })
                .collect();
            let pairing = M::Event(p::Event {
                kind: Some(pairing),
            });
            let mut expected = vec![M::Response(ok()), pairing];
            if started {
                expected.swap(0, 1);
            }
            expected.push(ending(p::SessionEndingReason::DeviceConnection));
            assert_eq!(messages, expected);
            assert_eq!(session.queued(), 0);
        }
    }
}

#[test]
fn replacing_a_session_discards_its_pending_terminal_pairing() {
    let mut session = Session::new();
    session.session(true);
    session.event(p::event::Kind::Pairing(p::Pairing {
        candidate: 1,
        step: Some(p::pairing::Step::Done(p::PairingDone { device: 77 })),
    }));
    session.respond(ok());
    session.end(p::SessionEndingReason::DeviceConnection, 0);
    let (stale, bytes) = session.output(1).unwrap();
    let length = bytes.len();
    assert_eq!(session.queued(), 3);
    session.session(false);
    session.session(true);
    session.output_complete(stale, length);
    assert_eq!(session.queued(), 0);
    assert!(session.idle());
    session.respond(ok());
    assert_eq!(drain(&mut session, 1), [M::Response(ok())]);
}

#[test]
fn ending_finishes_the_event_already_in_flight() {
    let mut session = Session::new();
    session.session(true);
    let event = p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 7 });
    session.event(event.clone());
    let (token, bytes) = session.output(1).unwrap();
    let mut wire = bytes.to_vec();
    session.end(p::SessionEndingReason::DeviceConnection, 0);
    session.output_complete(token, 1);
    while let Some((token, bytes)) = session.output(2) {
        wire.extend_from_slice(bytes);
        let length = bytes.len();
        session.output_complete(token, length);
    }
    let mut decoder = Decoder::new(None);
    let messages: Vec<_> = wire
        .into_iter()
        .filter_map(|byte| {
            decoder
                .push(byte)
                .map(|frame| p::Message::decode(frame.unwrap()).unwrap().kind.unwrap())
        })
        .collect();
    assert_eq!(
        messages,
        [
            M::Event(p::Event { kind: Some(event) }),
            ending(p::SessionEndingReason::DeviceConnection)
        ]
    );
}

#[test]
fn ending_aborts_a_partly_transmitted_listing_with_busy() {
    let mut session = Session::new();
    session.session(true);
    session.start_stream();
    for id in 1..=100 {
        session.stream_response(list_response(id), false);
        if !session.stream_ready() {
            break;
        }
    }
    let (token, bytes) = session.output(3).unwrap();
    let mut wire = bytes.to_vec();
    session.end(p::SessionEndingReason::ConfigurationChange, 0);
    session.output_complete(token, 3);
    while let Some((token, bytes)) = session.output(7) {
        wire.extend_from_slice(bytes);
        let length = bytes.len();
        session.output_complete(token, length);
    }
    let mut decoder = Decoder::new(None);
    let messages: Vec<_> = wire
        .into_iter()
        .filter_map(|byte| {
            decoder
                .push(byte)
                .map(|frame| p::Message::decode(frame.unwrap()).unwrap().kind.unwrap())
        })
        .collect();
    assert_eq!(
        messages,
        [
            M::Response(p::Response {
                result: Some(p::response::Result::Error(p::Error {
                    code: p::ErrorCode::Busy as i32,
                    ..Default::default()
                })),
            }),
            ending(p::SessionEndingReason::ConfigurationChange)
        ]
    );
}

#[test]
fn closed_and_replaced_sessions_discard_the_final_event_and_its_completion() {
    let mut session = Session::new();
    session.end(p::SessionEndingReason::Reboot, 0);
    assert_eq!(session.queued(), 0);
    assert!(session.output(63).is_none());
    session.session(true);
    session.end(p::SessionEndingReason::DeviceConnection, 0);
    let (stale, bytes) = session.output(63).unwrap();
    let old_length = bytes.len();
    session.session(false);
    session.session(true);
    assert!(session.feed(&status()).1.is_some());
    session.respond(ok());
    let (current, bytes) = session.output(63).unwrap();
    let length = bytes.len();
    session.output_complete(stale, old_length);
    assert!(session.output(63).is_none());
    assert_eq!(session.queued(), 1);
    session.output_complete(current, length);
    assert_eq!(session.queued(), 0);
    assert!(session.idle());
}
