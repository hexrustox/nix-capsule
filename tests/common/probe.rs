//! A probe module for the wire: raw-language tests speak frames to a
//! [`Server`] or watch its tempdir, through one shared helper set — no
//! per-file copy of frame collection, timeout shaping, or polling.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use nix_capsule::protocol::{CURRENT_VERSION, Exit, FrameCodec, Message, Request};
use tokio::net::UnixStream;
use tokio::time::sleep;
use tokio_util::codec::Framed;

use super::Server;

/// Upper bound on one phase; a red run fails on the assertion, never on the
/// harness itself.
pub const PHASE_LIMIT: Duration = Duration::from_secs(20);

/// A raw wire-protocol connection in probe shape, as [`Server::raw`] hands out.
pub type Raw = Framed<UnixStream, FrameCodec>;

/// Build a `Request` that speaks `sh -c script` from `cwd`, versioned like
/// the real client and carrying no env override; tests needing extra `env`
/// entries or a custom `version` mutate the struct's `pub` fields.
pub fn request(cwd: &Path, script: &str) -> Request {
    Request {
        command: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: cwd.to_string_lossy().into_owned(),
        env: Vec::new(),
        version: Some(CURRENT_VERSION.into()),
    }
}

/// Encode and send `request` over `framed`.
pub async fn send_request_msg(framed: &mut Raw, request: Request) {
    framed
        .send(
            Message::Request(request)
                .into_frame()
                .expect("encode request"),
        )
        .await
        .expect("send request");
}

/// Send `sh -c script` as a Request, versioned like the real client sends.
pub async fn send_request(framed: &mut Raw, cwd: &Path, script: &str) {
    send_request_msg(framed, request(cwd, script)).await;
}

/// Read frames until `done` matches one (which is included) or `limit`
/// elapses; returns everything seen. A timeout shows up as a short list, so
/// assertions name the missing frame instead of hanging the suite.
pub async fn read_frames_until(
    framed: &mut Raw,
    limit: Duration,
    mut done: impl FnMut(&Message) -> bool,
) -> Vec<Message> {
    let mut frames = Vec::new();
    let _ = tokio::time::timeout(limit, async {
        while let Some(frame) = framed.next().await {
            let message = Message::from_frame(frame.expect("frame transport")).expect("decode");
            let finished = done(&message);
            frames.push(message);
            if finished {
                break;
            }
        }
    })
    .await;
    frames
}

/// Read frames until one carries stdout containing `needle` (included); see
/// [`read_frames_until`] for the timeout shape.
pub async fn read_until_stdout_contains(framed: &mut Raw, needle: &str) -> Vec<Message> {
    read_frames_until(framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Stdout(bytes) if String::from_utf8_lossy(bytes).contains(needle))
    })
    .await
}

/// Read frames until the terminal frame (included) or `limit` elapses; see
/// [`read_frames_until`] for the timeout shape.
pub async fn read_until_terminal_within(framed: &mut Raw, limit: Duration) -> Vec<Message> {
    read_frames_until(framed, limit, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await
}

/// Read frames until the terminal frame (included) or [`PHASE_LIMIT`]
/// elapses; see [`read_frames_until`] for the timeout shape.
pub async fn read_until_terminal(framed: &mut Raw) -> Vec<Message> {
    read_until_terminal_within(framed, PHASE_LIMIT).await
}

/// All stdout bytes carried by `frames`.
pub fn stdout_of(frames: &[Message]) -> String {
    frames
        .iter()
        .filter_map(|message| match message {
            Message::Stdout(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .collect()
}

/// One request run against a server: the frames seen through the terminal
/// one, with the derived stdout and terminal frame.
pub struct RawRun {
    pub frames: Vec<Message>,
    pub stdout: String,
    pub terminal: Option<Message>,
}

/// Send `request` and collect frames through the terminal frame; see
/// [`read_until_terminal`] for the timeout shape.
pub async fn run_request(framed: &mut Raw, request: Request) -> RawRun {
    send_request_msg(framed, request).await;
    let frames = read_until_terminal(framed).await;
    RawRun {
        stdout: stdout_of(&frames),
        terminal: terminal_of(&frames).cloned(),
        frames,
    }
}

/// The terminal frame, if one arrived.
pub fn terminal_of(frames: &[Message]) -> Option<&Message> {
    frames
        .iter()
        .find(|message| matches!(message, Message::Exit(_) | Message::Error(_)))
}

/// Assert the terminal frame is a clean exit 0 — no signal, no `Error`.
pub fn assert_clean_exit(frames: &[Message], context: &str) {
    assert_eq!(
        terminal_of(frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "{context}: frames={frames:?}"
    );
}

/// Poll a synchronous predicate every 25 ms until it holds or `limit`
/// elapses; `false` means the deadline passed with the predicate still
/// failing.
pub async fn poll_until(limit: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(25)).await;
    }
}

/// Poll `marker` until it contains `needle`, via [`poll_until`].
pub async fn wait_for_marker(marker: &Path, needle: &str, limit: Duration) -> bool {
    poll_until(limit, || {
        fs::read_to_string(marker).is_ok_and(|content| content.contains(needle))
    })
    .await
}

/// Poll until `name` exists in the server's tempdir — the child's cwd — or
/// panic; children write flag files as observable progress markers.
pub async fn wait_for_flag(server: &Server, name: &str) {
    let flag = server.path().join(name);
    let appeared = poll_until(PHASE_LIMIT, || flag.exists()).await;
    assert!(appeared, "{name} never appeared");
}
