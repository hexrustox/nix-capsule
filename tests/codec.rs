//! Byte-level tests for the wire protocol at its public seam: `nix_capsule::protocol`.
//!
//! Example-based tables pin the documented wire format with exact literals;
//! property-based tests generalize round-tripping, chunk tolerance, and
//! transport-violation handling across arbitrary inputs.

use bytes::{BufMut, BytesMut};
use nix_capsule::protocol::{
    CURRENT_VERSION, DecodeError, EncodeError, ErrorMsg, Exit, FrameCodec, FrameType, MAX_PAYLOAD,
    Message, Request, SignalMsg, VersionMsg,
};
use proptest::prelude::*;
use test_case::test_case;
use tokio_util::codec::{Decoder, Encoder};

fn encoded(msg: &Message) -> Vec<u8> {
    let mut dst = BytesMut::new();
    FrameCodec
        .encode(msg.clone().into_frame().unwrap(), &mut dst)
        .unwrap();
    dst.to_vec()
}

fn decoded(buf: &[u8]) -> Message {
    let mut codec = FrameCodec;
    let mut src = BytesMut::from(buf);
    let frame = codec.decode(&mut src).unwrap().expect("a complete frame");
    Message::from_frame(frame).unwrap()
}

fn framed(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(tag);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Feed `bytes` through a fresh decoder in fixed-size chunks; collect every message.
fn feed_in_chunks(bytes: &[u8], chunk: usize) -> Result<Vec<Message>, DecodeError> {
    let mut codec = FrameCodec;
    let mut src = BytesMut::new();
    let mut out = Vec::new();
    for piece in bytes.chunks(chunk.max(1)) {
        src.extend_from_slice(piece);
        while let Some(frame) = codec.decode(&mut src)? {
            out.push(Message::from_frame(frame).unwrap());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- strategies

prop_compose! {
    fn arb_request()
        (command in ".*",
         args in prop::collection::vec(".*", 0..8),
         cwd in ".*",
         env in prop::collection::vec(".*", 0..8),
         version in any::<Option<String>>())
        -> Request {
        Request { command, args, cwd, env, version }
    }
}

fn arb_message() -> impl Strategy<Value = Message> {
    prop_oneof![
        arb_request().prop_map(Message::Request),
        prop::collection::vec(any::<u8>(), 0..256).prop_map(Message::Stdin),
        prop::collection::vec(any::<u8>(), 0..256).prop_map(Message::Stdout),
        prop::collection::vec(any::<u8>(), 0..256).prop_map(Message::Stderr),
        (any::<Option<u8>>(), any::<Option<u8>>())
            .prop_map(|(code, signal)| Message::Exit(Exit { code, signal })),
        ".*".prop_map(|message| Message::Error(ErrorMsg { message })),
        Just(Message::ServerStopping),
        ".*".prop_map(|version| Message::Version(VersionMsg { version })),
        any::<u8>().prop_map(|signal| Message::Signal(SignalMsg { signal })),
    ]
}

fn arb_known_tag() -> impl Strategy<Value = u8> {
    0x01u8..=0x09
}

// ------------------------------------------------------ exact literal tables

#[test_case(
    Message::Request(Request {
        command: "sh".into(),
        args: vec![],
        cwd: "/".into(),
        env: vec![],
        version: None,
    }),
    framed(0x01, br#"{"command":"sh","args":[],"cwd":"/","env":[]}"#)
    ; "request_omits_absent_version"
)]
#[test_case(
    Message::Stdin(vec![0xde, 0xad]),
    b"\x02\x00\x00\x00\x02\xde\xad".to_vec()
    ; "stdin"
)]
#[test_case(
    Message::Stdout(b"out".to_vec()),
    b"\x03\x00\x00\x00\x03out".to_vec()
    ; "stdout"
)]
#[test_case(
    Message::Stderr(Vec::new()),
    b"\x04\x00\x00\x00\x00".to_vec()
    ; "stderr_empty"
)]
#[test_case(
    Message::Error(ErrorMsg { message: "boom".into() }),
    b"\x06\x00\x00\x00\x12{\"message\":\"boom\"}".to_vec()
    ; "error"
)]
#[test_case(
    Message::ServerStopping,
    b"\x07\x00\x00\x00\x00".to_vec()
    ; "server_stopping_empty_payload"
)]
#[test_case(
    Message::Version(VersionMsg { version: CURRENT_VERSION.into() }),
    framed(
        0x08,
        format!("{{\"version\":\"{CURRENT_VERSION}\"}}").as_bytes()
    )
    ; "version_carries_crate_version"
)]
#[test_case(
    Message::Signal(SignalMsg { signal: 15 }),
    b"\x09\x00\x00\x00\x0d{\"signal\":15}".to_vec()
    ; "signal"
)]
fn encodes_the_documented_wire_bytes(msg: Message, want: Vec<u8>) {
    assert_eq!(encoded(&msg), want);
}

#[test_case(Some(7), None => framed(0x05, br#"{"code":7}"#) ; "code_only")]
#[test_case(None, Some(9) => framed(0x05, br#"{"signal":9}"#) ; "signal_only")]
#[test_case(None, None => framed(0x05, b"{}") ; "neither_set")]
fn encode_exit_emits_only_the_set_field(code: Option<u8>, signal: Option<u8>) -> Vec<u8> {
    encoded(&Message::Exit(Exit { code, signal }))
}

// ------------------------------------------------------------ error taxonomy

#[test_case(0x00 ; "below_range")]
#[test_case(0x0a ; "just_past_max")]
#[test_case(0x7f ; "ascii_control_zone")]
#[test_case(0xff ; "all_bits_set")]
fn unknown_tag_bytes_reject_decoding(bad_tag: u8) {
    let mut codec = FrameCodec;
    let mut src = BytesMut::from(framed(bad_tag, b"x").as_slice());
    match codec.decode(&mut src) {
        Err(DecodeError::UnknownFrameType(b)) => assert_eq!(b, bad_tag),
        other => panic!("tag {bad_tag:#x}: expected UnknownFrameType, got {other:?}"),
    }
}

// `ServerStopping` is deliberately absent: its payload is tolerated, not parsed.
#[test_case(FrameType::Request ; "request")]
#[test_case(FrameType::Exit ; "exit")]
#[test_case(FrameType::Error ; "error")]
#[test_case(FrameType::Version ; "version")]
#[test_case(FrameType::Signal ; "signal")]
fn malformed_struct_payloads_fail_decoding_without_panicking(tag: FrameType) {
    let mut codec = FrameCodec;
    let mut src = BytesMut::from(framed(tag.to_byte(), b"{not json").as_slice());
    let frame = codec
        .decode(&mut src)
        .unwrap()
        .expect("framing tolerates a junk payload");
    assert!(matches!(
        Message::from_frame(frame),
        Err(DecodeError::Json(_))
    ));
}

#[test]
fn encoder_rejects_oversized_payloads() {
    let mut codec = FrameCodec;
    let mut dst = BytesMut::new();
    let frame = Message::Stdin(vec![0; MAX_PAYLOAD + 1])
        .into_frame()
        .unwrap();
    match codec.encode(frame, &mut dst) {
        Err(EncodeError::PayloadTooLarge(n)) => assert_eq!(n, MAX_PAYLOAD + 1),
        other => panic!("expected PayloadTooLarge, got {other:?}"),
    }
}

// ---------------------------------------------------------------- boundaries

#[test]
fn a_declared_length_of_exactly_16_mib_is_accepted() {
    let payload = vec![0xa5; MAX_PAYLOAD];
    let wire = framed(0x03, &payload);
    assert_eq!(
        decoded(&wire),
        Message::Stdout(payload),
        "a full 16 MiB payload must decode"
    );
}

#[test]
fn a_declared_length_above_16_mib_is_rejected_at_header_time() {
    let mut codec = FrameCodec;
    let mut src = BytesMut::new();
    src.put_u8(0x03);
    src.put_u32(MAX_PAYLOAD as u32 + 1);

    match codec.decode(&mut src) {
        Err(DecodeError::PayloadTooLarge(n)) => assert_eq!(n, MAX_PAYLOAD + 1),
        other => panic!("expected PayloadTooLarge, got {other:?}"),
    }
}

// --------------------------------------------------------------- properties

proptest! {
    #[test]
    fn decoding_an_encoded_message_returns_it_unchanged(msg in arb_message()) {
        prop_assert_eq!(decoded(&encoded(&msg)), msg);
    }

    #[test]
    fn a_single_stream_frame_survives_any_chunking(
        payload in prop::collection::vec(any::<u8>(), 0..300),
        chunk in 1usize..=200,
        kind in 0u8..3,
    ) {
        let want = match kind {
            0 => Message::Stdin(payload.clone()),
            1 => Message::Stdout(payload.clone()),
            _ => Message::Stderr(payload),
        };
        let wire = encoded(&want);
        prop_assert_eq!(
            feed_in_chunks(&wire, chunk).unwrap(),
            vec![want],
            "chunk size {} broke the stream",
            chunk
        );
    }

    #[test]
    fn a_sequence_of_frames_survives_any_chunking(
        msgs in prop::collection::vec(arb_message(), 1..=6),
        chunk in 1usize..=200,
    ) {
        let wire: Vec<u8> = msgs.iter().flat_map(encoded).collect();
        prop_assert_eq!(
            feed_in_chunks(&wire, chunk).unwrap(),
            msgs,
            "chunk size {} broke framing boundaries",
            chunk
        );
    }

    #[test]
    fn lengths_above_the_cap_fail_before_buffering_regardless_of_tag_or_body(
        tag in arb_known_tag(),
        declared in MAX_PAYLOAD as u32 + 1..=u32::MAX,
        buffered in 0usize..=48,
    ) {
        let mut codec = FrameCodec;
        let mut src = BytesMut::new();
        src.put_u8(tag);
        src.put_u32(declared);
        src.extend(std::iter::repeat_n(0xa5, buffered));

        match codec.decode(&mut src) {
            Err(DecodeError::PayloadTooLarge(n)) => prop_assert_eq!(n, declared as usize),
            other => panic!("expected PayloadTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn no_strict_prefix_of_a_frame_yields_that_frame(
        msg in arb_message(),
        cut_pct in 1u32..=99,
    ) {
        let wire = encoded(&msg);
        let stop = ((wire.len() as u32 * cut_pct / 100) as usize)
            .clamp(1, wire.len() - 1);

        let mut codec = FrameCodec;
        let mut src = BytesMut::new();
        for piece in &wire[..stop] {
            src.extend_from_slice(std::slice::from_ref(piece));
            prop_assert!(
                codec.decode(&mut src).unwrap().is_none(),
                "a {}-byte prefix produced a frame",
                src.len()
            );
        }
    }
}
