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
    s.session(true);
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
    s.session(true);
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
    s.session(true);
    s.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
        id: 1_234_567,
    }));
    let (token, _) = s.output(2).unwrap();
    assert!(s.session(true), "a new session ends the old one");
    assert!(s.idle(), "a new session drops unsent output");
    s.output_complete(token, 2);
    assert!(s.output(64).is_none());
}

#[test]
fn closed_ports_take_no_input_and_send_nothing() {
    let mut s = Session::new();
    assert_eq!(s.feed(&status()), (0, None));
    s.respond(ok());
    assert!(s.output(64).is_none());
    s.session(true);
    let mut partial = status();
    partial.truncate(2);
    assert_eq!(s.feed(&partial), (2, None));
    assert!(s.session(false));
    s.session(true);
    // The partial request from the earlier session cannot merge with a new one.
    let (_, r) = s.feed(&status());
    assert!(r.is_some());
}

#[test]
fn bad_frames_are_answered_without_reaching_the_application() {
    let mut s = Session::new();
    s.session(true);
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
    // A Request with no command.
    let mut bytes = Vec::new();
    frame::encode(&p::Request { command: None }, &mut bytes);
    assert_eq!(s.feed(&bytes).1, None);
    assert_eq!(code(&mut s), p::ErrorCode::UnknownCommand);
    let mut bytes = Vec::new();
    frame::encode_bytes(
        &vec![1; cordial_protocol::MAX_REQUEST_BYTES + 1],
        &mut bytes,
    );
    assert_eq!(s.feed(&bytes).1, None);
    assert_eq!(code(&mut s), p::ErrorCode::TooLong);
    // Empty frames are skipped silently.
    assert_eq!(s.feed(&[0, 0, 0]), (3, None));
    assert!(s.idle());
}

#[test]
fn bootloader_entry_stops_reading_requests() {
    let mut s = Session::new();
    s.session(true);
    s.stop_commands();
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
        session.session(true);
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
    session.session(true);
    session.start_stream();
    for id in 1..=100 {
        session.stream_response(list_response(id), false);
        if !session.stream_ready() {
            break;
        }
    }
    let (stale, _) = session.output(1).unwrap();
    session.session(false);
    session.session(true);
    session.respond(ok());
    session.output_complete(stale, 1);
    assert_eq!(drain(&mut session, 1), [M::Response(ok())]);
    assert!(session.idle());
}

#[test]
fn request_limit_counts_the_complete_decoded_envelope() {
    for length in [1023, 1024, 1025] {
        let mut session = Session::new();
        session.session(true);
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
