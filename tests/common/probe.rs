//! The wire module: raw-language tests speak frames to a [`Server`] through one shared helper set.
//!
//! One request run per connection, frames collected through the Terminal
//! frame. Child lifecycle (markers, vanish, reaping) lives in
//! [`super::child`]; shell text lives in [`super::script`]. The Version
//! probe stays on this Connection (ADR-0004: no version-on-exec).

use std::{path::Path, time::Duration};

use futures_util::{SinkExt, StreamExt};
use nix_capsule::protocol::{Exit, FrameCodec, Message, Request, SignalMsg};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use super::child::WAIT_PHASE;
use super::server::Server;

/// A raw wire-protocol connection in probe shape, as [`Server::raw`] hands out.
pub type Raw = Framed<UnixStream, FrameCodec>;

/// Build a `Request` that speaks `sh -c script` from `cwd`, carrying no env
/// override; tests needing extra `env` entries mutate the struct's `pub`
/// fields.
pub fn request(cwd: &Path, script: &str) -> Request {
    Request {
        command: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: cwd.to_string_lossy().into_owned(),
        env: Vec::new(),
    }
}

/// The Version probe carries an empty payload.
pub async fn send_request_version(framed: &mut Raw) {
    framed
        .send(Message::RequestVersion.into_frame().expect("encode probe"))
        .await
        .expect("send probe");
}

pub async fn send_request(framed: &mut Raw, cwd: &Path, script: &str) {
    framed
        .send(
            Message::Request(request(cwd, script))
                .into_frame()
                .expect("encode request"),
        )
        .await
        .expect("send request");
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
    read_frames_until(framed, WAIT_PHASE, |message| {
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

/// Read frames until the terminal frame (included) or [`WAIT_PHASE`]
/// elapses; see [`read_frames_until`] for the timeout shape.
pub async fn read_until_terminal(framed: &mut Raw) -> Vec<Message> {
    read_until_terminal_within(framed, WAIT_PHASE).await
}

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
    /// Every frame seen, including the terminal one.
    pub frames: Vec<Message>,
    pub stdout: String,
    pub terminal: Option<Message>,
}

/// Send `request` and collect frames through the terminal frame; see
/// [`read_until_terminal`] for the timeout shape. The one RawRun interface:
/// every wire test enters through here instead of stacking send/read pairs.
pub async fn run_raw_request(framed: &mut Raw, request: Request) -> RawRun {
    framed
        .send(
            Message::Request(request)
                .into_frame()
                .expect("encode request"),
        )
        .await
        .expect("send request");
    let frames = read_until_terminal(framed).await;
    let stdout = stdout_of(&frames);
    let terminal = terminal_of(&frames).cloned();
    RawRun {
        frames,
        stdout,
        terminal,
    }
}

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

pub async fn send_signal(framed: &mut Raw, signal: u8) {
    framed
        .send(
            Message::Signal(SignalMsg { signal })
                .into_frame()
                .expect("encode signal"),
        )
        .await
        .expect("send signal");
}

/// Send `sh -c script`, wait for stdout to contain `READY`, deliver `number`,
/// and read until a terminal frame or `limit` elapses.
pub async fn ready_signal_terminal(
    framed: &mut Raw,
    server: &Server,
    script: &str,
    signal: i32,
    limit: Duration,
) -> Vec<Message> {
    send_request(framed, server.path(), script).await;
    read_until_stdout_contains(framed, "READY").await;
    send_signal(framed, signal as u8).await;
    read_until_terminal_within(framed, limit).await
}

/// Start a real server and open one raw connection to it — the two-step
/// dance every wire-level test opens with. Stop the server in the test when
/// the phase ordering demands it, exactly as though the builder had run.
pub async fn start_test_server() -> (Server, Raw) {
    let server = Server::builder().start().await;
    let framed = server.raw().await;
    (server, framed)
}
