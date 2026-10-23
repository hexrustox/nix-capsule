//! Wire framing and frame payloads, pinned to `spec/protocol.md`.

use std::error::Error;

use bytes::BytesMut;
use test_case::test_case;
use tokio_util::codec::{Decoder, Encoder};

use nix_capsule::protocol::{
    DecodeError, EncodeError, ErrorMsg, Exit, Frame, FrameCodec, FrameType, Message, Request,
    SignalMsg, VersionMsg,
};

#[test]
fn encodes_tag_then_big_endian_length_then_payload() -> Result<(), Box<dyn Error>> {
    // 0x00010203: the four length bytes differ, so byte order is pinned.
    let payload = vec![0xA5; 66_051];
    let mut codec = FrameCodec;
    let mut dst = BytesMut::new();
    codec.encode(
        Frame {
            frame_type: FrameType::Stdout,
            payload: payload.clone(),
        },
        &mut dst,
    )?;
    assert_eq!(&dst[..5], &[0x03, 0x00, 0x01, 0x02, 0x03][..]);
    assert_eq!(&dst[5..], payload.as_slice());
    Ok(())
}

#[test]
fn decodes_hand_built_wire_bytes() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x02, 0xC3, 0x28]);
    let frame = codec
        .decode(&mut buf)?
        .expect("all seven bytes of one frame are buffered");
    assert_eq!(frame.frame_type, FrameType::Stdout);
    assert_eq!(frame.payload.as_slice(), &[0xC3, 0x28][..]);
    Ok(())
}

#[test]
fn yields_no_frame_while_header_incomplete() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&[0x03]);
    assert!(
        codec.decode(&mut buf)?.is_none(),
        "no frame from a 1-byte header prefix"
    );
    buf.extend_from_slice(&[0x00]);
    assert!(
        codec.decode(&mut buf)?.is_none(),
        "no frame from a 2-byte header prefix"
    );
    buf.extend_from_slice(&[0x00]);
    assert!(
        codec.decode(&mut buf)?.is_none(),
        "no frame from a 3-byte header prefix"
    );
    buf.extend_from_slice(&[0x00]);
    assert!(
        codec.decode(&mut buf)?.is_none(),
        "no frame from a 4-byte header prefix"
    );
    Ok(())
}

#[test]
fn decodes_frame_fed_in_arbitrary_chunks() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&[0x03, 0x00]);
    assert!(
        codec.decode(&mut buf)?.is_none(),
        "no frame from 2 of 5 header bytes"
    );
    buf.extend_from_slice(&[0x00, 0x00, 0x03, b'a']);
    assert!(
        codec.decode(&mut buf)?.is_none(),
        "no frame from a complete header and 1 of 3 payload bytes"
    );
    buf.extend_from_slice(&[b'b', b'c']);
    let frame = codec
        .decode(&mut buf)?
        .expect("the frame completes with the third fragment");
    assert_eq!(frame.frame_type, FrameType::Stdout);
    assert_eq!(frame.payload.as_slice(), b"abc".as_slice());
    Ok(())
}

#[test]
fn decodes_back_to_back_frames_in_fifo_order() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&wire(0x03, b"first"));
    buf.extend_from_slice(&wire(0x04, b"second"));
    buf.extend_from_slice(&[0x02]);

    let first_frame = codec
        .decode(&mut buf)?
        .expect("the first frame is fully buffered");
    assert_eq!(first_frame.frame_type, FrameType::Stdout);
    assert_eq!(first_frame.payload.as_slice(), b"first".as_slice());

    let second_frame = codec
        .decode(&mut buf)?
        .expect("the second frame is fully buffered");
    assert_eq!(second_frame.frame_type, FrameType::Stderr);
    assert_eq!(second_frame.payload.as_slice(), b"second".as_slice());

    let trailing = codec.decode(&mut buf)?;
    assert!(
        trailing.is_none(),
        "a lone 0x02 byte is 1 of 5 header bytes, got {trailing:?}"
    );
    Ok(())
}

#[test]
fn rejects_payload_above_the_cap_from_the_header_alone() {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    // 0x01000001 = 16_777_217 = the 16 MiB cap + 1.
    buf.extend_from_slice(&[0x03, 0x01, 0x00, 0x00, 0x01]);
    let err = codec.decode(&mut buf).unwrap_err();
    assert!(
        matches!(err, DecodeError::PayloadTooLarge(16_777_217)),
        "got {err:?}"
    );
}

#[test]
fn accepts_header_declaring_exactly_the_cap() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    // 0x01000000 = 16_777_216 = the cap itself.
    buf.extend_from_slice(&[0x03, 0x01, 0x00, 0x00, 0x00]);
    let decoded = codec.decode(&mut buf)?;
    assert!(
        decoded.is_none(),
        "the cap itself is a legal length; the frame is merely incomplete, got {decoded:?}"
    );
    Ok(())
}

#[test]
fn rejects_encoding_payload_above_the_cap() {
    let mut codec = FrameCodec;
    let mut dst = BytesMut::new();
    let frame = Frame {
        frame_type: FrameType::Stdout,
        payload: vec![0; 16_777_217],
    };
    let err = codec.encode(frame, &mut dst).unwrap_err();
    assert!(
        matches!(err, EncodeError::PayloadTooLarge(16_777_217)),
        "got {err:?}"
    );
}

#[test]
fn encodes_payload_at_exactly_the_cap() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut dst = BytesMut::new();
    codec.encode(
        Frame {
            frame_type: FrameType::Stdout,
            payload: vec![0xA7; 16_777_216],
        },
        &mut dst,
    )?;
    assert_eq!(&dst[..5], &[0x03, 0x01, 0x00, 0x00, 0x00][..]);
    assert_eq!(dst.len(), 16_777_221);
    Ok(())
}

#[test_case(0x00; "below_the_tag_table")]
#[test_case(0x0C; "above_the_tag_table")]
fn rejects_unknown_tag_byte(tag: u8) {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&[tag, 0x00, 0x00, 0x00, 0x00]);
    let err = codec.decode(&mut buf).unwrap_err();
    assert!(
        matches!(&err, DecodeError::UnknownFrameType(seen) if *seen == tag),
        "got {err:?}"
    );
}

#[test_case(
    Message::Request(Request {
        command: "git".into(),
        args: vec!["status".into(), "--porcelain".into()],
        cwd: "/build".into(),
        env: vec!["LC_ALL=C".into()],
    }),
    0x01,
    br#"{"command":"git","args":["status","--porcelain"],"cwd":"/build","env":["LC_ALL=C"]}"#;
    "request_serializes_every_field_as_json"
)]
#[test_case(
    Message::Stdin(b"\xFF\xFE\x00".to_vec()),
    0x02,
    b"\xFF\xFE\x00";
    "stdin_carries_raw_non_utf8_bytes"
)]
#[test_case(
    Message::Stdin(Vec::new()),
    0x02,
    b"";
    "stdin_eof_travels_as_a_zero_length_frame"
)]
#[test_case(
    Message::Stdout(b"stdout-bytes".to_vec()),
    0x03,
    b"stdout-bytes";
    "stdout_carries_raw_bytes"
)]
#[test_case(
    Message::Stderr(b"stderr!".to_vec()),
    0x04,
    b"stderr!";
    "stderr_carries_raw_bytes"
)]
#[test_case(
    Message::Exit(Exit {
        code: Some(7),
        signal: None,
    }),
    0x05,
    br#"{"code":7}"#;
    "exit_code_omits_the_unset_signal_field"
)]
#[test_case(
    Message::Exit(Exit {
        code: None,
        signal: Some(9),
    }),
    0x05,
    br#"{"signal":9}"#;
    "exit_signal_omits_the_unset_code_field"
)]
#[test_case(
    Message::Exit(Exit {
        code: None,
        signal: None,
    }),
    0x05,
    br#"{}"#;
    "exit_with_neither_field_is_the_unknowable_status"
)]
#[test_case(
    Message::Error(ErrorMsg {
        message: "spawn failed".into(),
    }),
    0x06,
    br#"{"message":"spawn failed"}"#;
    "error_serializes_its_message_as_json"
)]
#[test_case(
    Message::ServerStopping,
    0x07,
    b"";
    "server_stopping_carries_an_empty_payload"
)]
#[test_case(
    Message::Version(VersionMsg {
        version: "9.9.9".into(),
    }),
    0x08,
    br#"{"version":"9.9.9"}"#;
    "reserved_version_frame_carries_its_payload"
)]
#[test_case(
    Message::Signal(SignalMsg { signal: 15 }),
    0x09,
    br#"{"signal":15}"#;
    "signal_serializes_its_number_as_json"
)]
#[test_case(
    Message::RequestVersion,
    0x0A,
    b"";
    "request_version_carries_an_empty_payload"
)]
#[test_case(
    Message::ServerVersion(VersionMsg {
        version: "10.0.0-rc.1".into(),
    }),
    0x0B,
    br#"{"version":"10.0.0-rc.1"}"#;
    "server_version_serializes_its_version_as_json"
)]
fn round_trips_message_as_wire_frame(
    message: Message,
    expected_tag: u8,
    expected_payload: &[u8],
) -> Result<(), Box<dyn Error>> {
    let frame = message.clone().into_frame()?;
    assert_eq!(frame.frame_type.to_byte(), expected_tag);
    assert_eq!(frame.payload.as_slice(), expected_payload);

    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    codec.encode(frame, &mut buf)?;
    let expected_wire = wire(expected_tag, expected_payload);
    assert_eq!(&buf[..], &expected_wire[..]);

    let decoded = codec
        .decode(&mut buf)?
        .expect("one complete frame was just encoded");
    assert_eq!(Message::from_frame(decoded)?, message);
    Ok(())
}

#[test]
fn tolerates_unknown_json_fields_in_struct_payload() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&wire(
        0x01,
        br#"{"command":"nix","args":["build"],"cwd":"/build","env":["FOO=bar"],"future_field":true}"#,
    ));
    let frame = codec
        .decode(&mut buf)?
        .expect("the full frame is buffered");
    assert_eq!(
        Message::from_frame(frame)?,
        Message::Request(Request {
            command: "nix".into(),
            args: vec!["build".into()],
            cwd: "/build".into(),
            env: vec!["FOO=bar".into()],
        })
    );
    Ok(())
}

#[test]
fn rejects_server_stopping_with_non_empty_payload() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&wire(0x07, b"leftover"));
    let frame = codec
        .decode(&mut buf)?
        .expect("framing accepted the eight payload bytes");
    let err = Message::from_frame(frame).unwrap_err();
    assert!(
        matches!(err, DecodeError::NonEmptyServerStopping(8)),
        "got {err:?}"
    );
    Ok(())
}

#[test]
fn rejects_request_version_with_non_empty_payload() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&wire(0x0A, b"junk"));
    let frame = codec
        .decode(&mut buf)?
        .expect("framing accepted the four payload bytes");
    let err = Message::from_frame(frame).unwrap_err();
    assert!(
        matches!(err, DecodeError::NonEmptyRequestVersion(4)),
        "got {err:?}"
    );
    Ok(())
}

#[test]
fn rejects_exit_with_both_code_and_signal_on_decode() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&wire(0x05, br#"{"code":2,"signal":9}"#));
    let frame = codec
        .decode(&mut buf)?
        .expect("framing accepted the payload bytes");
    let err = Message::from_frame(frame).unwrap_err();
    assert!(matches!(err, DecodeError::InvalidExit), "got {err:?}");
    Ok(())
}

#[test]
fn rejects_exit_with_both_code_and_signal_on_encode() {
    let err = Message::Exit(Exit {
        code: Some(2),
        signal: Some(9),
    })
    .into_frame()
    .unwrap_err();
    assert!(matches!(err, EncodeError::InvalidExit), "got {err:?}");
}

#[test]
fn returns_decode_error_for_payload_that_is_not_json() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&wire(0x06, b"not json"));
    let frame = codec
        .decode(&mut buf)?
        .expect("framing accepted the payload bytes");
    let err = Message::from_frame(frame).unwrap_err();
    assert!(matches!(err, DecodeError::Json { .. }), "got {err:?}");
    Ok(())
}

#[test]
fn returns_decode_error_for_wrong_shape_json() -> Result<(), Box<dyn Error>> {
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&wire(
        0x01,
        br#"{"command":3,"args":[],"cwd":"/build","env":[]}"#,
    ));
    let frame = codec
        .decode(&mut buf)?
        .expect("framing accepted the payload bytes");
    let err = Message::from_frame(frame).unwrap_err();
    assert!(matches!(err, DecodeError::Json { .. }), "got {err:?}");
    Ok(())
}

// spec/protocol.md § Framing: one tag byte, big-endian u32 length, payload.
fn wire(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(5 + payload.len());
    bytes.push(tag);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}
