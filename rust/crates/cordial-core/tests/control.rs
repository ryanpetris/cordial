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
        id: "d_1".into(),
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
                    id: "d_1".into(),
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
        id: "d_1".into(),
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
