use bytes::BytesMut;
use nix_capsule::protocol::{
    CURRENT_VERSION, DecodeError, ErrorMessage, Exit, Frame, FrameCodec, FrameType, Message,
    Request, Role, SIGTERM_EXIT, ServerStopping, VersionCheck, VersionMsg,
};
use tokio_util::codec::{Decoder, Encoder};

fn encode_frame(frame: Frame) -> Vec<u8> {
    let mut buf = BytesMut::new();
    FrameCodec.encode(frame, &mut buf).expect("encode");
    buf.to_vec()
}

fn decode_frame(bytes: &[u8]) -> Result<Option<Frame>, std::io::Error> {
    let mut buf = BytesMut::from(bytes);
    FrameCodec.decode(&mut buf)
}

fn round_trip(msg: Message) -> Message {
    let frame = msg.into_frame().expect("encode");
    let bytes = encode_frame(frame);
    let decoded = decode_frame(&bytes)
        .expect("decode")
        .expect("frame present");
    Message::from_frame(decoded).expect("from_frame")
}

#[test]
fn frame_type_to_byte_is_discriminant() {
    assert_eq!(FrameType::Request.to_byte(), 0x01);
    assert_eq!(FrameType::Stdin.to_byte(), 0x02);
    assert_eq!(FrameType::Stdout.to_byte(), 0x03);
    assert_eq!(FrameType::Stderr.to_byte(), 0x04);
    assert_eq!(FrameType::Exit.to_byte(), 0x05);
    assert_eq!(FrameType::Error.to_byte(), 0x06);
    assert_eq!(FrameType::ServerStopping.to_byte(), 0x07);
    assert_eq!(FrameType::Version.to_byte(), 0x08);
}

#[test]
fn frame_type_round_trips_via_byte() {
    for ft in [
        FrameType::Request,
        FrameType::Stdin,
        FrameType::Stdout,
        FrameType::Stderr,
        FrameType::Exit,
        FrameType::Error,
        FrameType::ServerStopping,
        FrameType::Version,
    ] {
        assert_eq!(FrameType::from_u8(ft.to_byte()), Some(ft));
    }
    assert_eq!(FrameType::from_u8(0x00), None);
    assert_eq!(FrameType::from_u8(0x09), None);
    assert_eq!(FrameType::from_u8(0xFF), None);
}

#[test]
fn current_version_matches_cargo_pkg_version() {
    assert_eq!(CURRENT_VERSION, env!("CARGO_PKG_VERSION"));
}

#[test]
fn sigterm_exit_is_143() {
    assert_eq!(SIGTERM_EXIT, 143);
}

#[test]
fn codec_decodes_empty_payload_binary_variant() {
    let frame = round_trip(Message::Stdin(Vec::new()));
    assert!(matches!(frame, Message::Stdin(b) if b.is_empty()));
}

#[test]
fn codec_decodes_one_byte_payload() {
    let frame = round_trip(Message::Stdout(vec![42]));
    assert!(matches!(frame, Message::Stdout(b) if b == vec![42]));
}

#[test]
fn codec_decodes_8kb_payload() {
    let payload = (0..8192).map(|i| (i % 256) as u8).collect::<Vec<_>>();
    let frame = round_trip(Message::Stderr(payload.clone()));
    assert!(matches!(frame, Message::Stderr(b) if b == payload));
}

#[test]
fn codec_decodes_large_payload() {
    let payload = vec![7u8; 100_000];
    let frame = round_trip(Message::Stdin(payload.clone()));
    assert!(matches!(frame, Message::Stdin(b) if b == payload));
}

#[test]
fn codec_returns_none_when_header_missing() {
    assert!(matches!(decode_frame(&[]), Ok(None)));
    assert!(matches!(decode_frame(&[0x01, 0x00, 0x00, 0x00]), Ok(None)));
}

#[test]
fn codec_returns_none_when_payload_incomplete() {
    let bytes = [0x02, 0x00, 0x00, 0x00, 0x05, b'h', b'e', b'l'];
    assert!(matches!(decode_frame(&bytes), Ok(None)));
}

#[test]
fn codec_rejects_unknown_frame_type_byte() {
    let bytes = [0x09, 0x00, 0x00, 0x00, 0x00];
    let result = decode_frame(&bytes);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("unknown frame type"));
}

#[test]
fn message_from_frame_rejects_invalid_json_on_request() {
    let frame = Frame {
        frame_type: FrameType::Request,
        payload: b"not json".to_vec(),
    };
    let result = Message::from_frame(frame);
    assert!(matches!(result, Err(DecodeError::Json(_))));
}

#[test]
fn message_from_frame_rejects_invalid_json_on_exit() {
    let frame = Frame {
        frame_type: FrameType::Exit,
        payload: b"{}".to_vec(),
    };
    assert!(matches!(
        Message::from_frame(frame),
        Err(DecodeError::Json(_))
    ));
}

#[test]
fn request_round_trips() {
    let original = Message::Request(Request {
        command: "cargo".into(),
        args: vec!["test".into(), "--release".into()],
        cwd: "/tmp/project".into(),
        env: vec!["FOO=bar".into()],
        version: Some("0.8.0".into()),
    });
    let round_tripped = round_trip(original);
    match round_tripped {
        Message::Request(r) => {
            assert_eq!(r.command, "cargo");
            assert_eq!(r.args, vec!["test", "--release"]);
            assert_eq!(r.cwd, "/tmp/project");
            assert_eq!(r.env, vec!["FOO=bar"]);
            assert_eq!(r.version.as_deref(), Some("0.8.0"));
        }
        _ => panic!("expected Request, got something else"),
    }
}

#[test]
fn exit_round_trips() {
    let rt = round_trip(Message::Exit(Exit { exit_code: 42 }));
    assert!(matches!(rt, Message::Exit(e) if e.exit_code == 42));
}

#[test]
fn error_round_trips_with_cause() {
    let original = Message::Error(ErrorMessage {
        error: "disk full".into(),
        cause: Some("write failed".into()),
    });
    let rt = round_trip(original);
    match rt {
        Message::Error(e) => {
            assert_eq!(e.error, "disk full");
            assert_eq!(e.cause.as_deref(), Some("write failed"));
        }
        _ => panic!("expected Error"),
    }
}

#[test]
fn error_round_trips_without_cause() {
    let original = Message::Error(ErrorMessage {
        error: "boom".into(),
        cause: None,
    });
    let rt = round_trip(original);
    assert!(matches!(rt, Message::Error(e) if e.error == "boom" && e.cause.is_none()));
}

#[test]
fn server_stopping_round_trips_with_reason() {
    let original = Message::ServerStopping(ServerStopping {
        reason: Some("drained".into()),
    });
    let rt = round_trip(original);
    assert!(matches!(rt, Message::ServerStopping(s) if s.reason.as_deref() == Some("drained")));
}

#[test]
fn version_round_trips() {
    let original = Message::Version(VersionMsg {
        version: "9.9.9".into(),
    });
    let rt = round_trip(original);
    assert!(matches!(rt, Message::Version(v) if v.version == "9.9.9"));
}

#[test]
fn stdin_round_trips_binary_garbage() {
    let payload = vec![0x00, 0xFF, 0x01, 0xFE];
    let rt = round_trip(Message::Stdin(payload.clone()));
    assert!(matches!(rt, Message::Stdin(b) if b == payload));
}

#[test]
fn message_frame_type_matches_variant() {
    assert_eq!(
        Message::Request(Request {
            command: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            env: Vec::new(),
            version: None,
        })
        .frame_type(),
        FrameType::Request
    );
    assert_eq!(Message::Stdin(vec![]).frame_type(), FrameType::Stdin);
    assert_eq!(Message::Stdout(vec![]).frame_type(), FrameType::Stdout);
    assert_eq!(Message::Stderr(vec![]).frame_type(), FrameType::Stderr);
    assert_eq!(
        Message::Exit(Exit { exit_code: 0 }).frame_type(),
        FrameType::Exit
    );
    assert_eq!(
        Message::Error(ErrorMessage {
            error: String::new(),
            cause: None
        })
        .frame_type(),
        FrameType::Error
    );
    assert_eq!(
        Message::ServerStopping(ServerStopping { reason: None }).frame_type(),
        FrameType::ServerStopping
    );
    assert_eq!(
        Message::Version(VersionMsg {
            version: String::new()
        })
        .frame_type(),
        FrameType::Version
    );
}

#[test]
fn version_check_match_for_client() {
    let msg = VersionMsg {
        version: CURRENT_VERSION.into(),
    };
    let check = VersionCheck::from(Some(&msg), CURRENT_VERSION, Role::Client);
    assert!(matches!(check, VersionCheck::Match));
}

#[test]
fn version_check_match_for_server() {
    let msg = VersionMsg {
        version: CURRENT_VERSION.into(),
    };
    let check = VersionCheck::from(Some(&msg), CURRENT_VERSION, Role::Server);
    assert!(matches!(check, VersionCheck::Match));
}

#[test]
fn version_check_mismatch_client_role_records_client_and_server_strings() {
    let msg = VersionMsg {
        version: "0.7.0".into(),
    };
    let check = VersionCheck::from(Some(&msg), "0.8.0", Role::Client);
    match check {
        VersionCheck::Mismatch { client, server } => {
            assert_eq!(client, "0.8.0");
            assert_eq!(server, "0.7.0");
        }
        _ => panic!("expected Mismatch, got {check:?}"),
    }
}

#[test]
fn version_check_mismatch_server_role_swaps_perspective() {
    let msg = VersionMsg {
        version: "0.7.0".into(),
    };
    let check = VersionCheck::from(Some(&msg), "0.8.0", Role::Server);
    match check {
        VersionCheck::Mismatch { client, server } => {
            assert_eq!(client, "0.7.0");
            assert_eq!(server, "0.8.0");
        }
        _ => panic!("expected Mismatch, got {check:?}"),
    }
}

#[test]
fn version_check_client_missing_when_server_gets_no_version() {
    let check = VersionCheck::from(None, "0.8.0", Role::Server);
    match check {
        VersionCheck::ClientMissing { server } => assert_eq!(server, "0.8.0"),
        _ => panic!("expected ClientMissing, got {check:?}"),
    }
}

#[test]
fn version_check_server_missing_when_client_gets_no_version() {
    let check = VersionCheck::from(None, "0.8.0", Role::Client);
    match check {
        VersionCheck::ServerMissing { client } => assert_eq!(client, "0.8.0"),
        _ => panic!("expected ServerMissing, got {check:?}"),
    }
}

#[test]
fn version_check_never_returns_client_missing_for_client_role() {
    let msg = VersionMsg {
        version: CURRENT_VERSION.into(),
    };
    let check = VersionCheck::from(Some(&msg), CURRENT_VERSION, Role::Client);
    assert!(!matches!(
        check,
        VersionCheck::ClientMissing { .. } | VersionCheck::Mismatch { .. }
    ));
}
