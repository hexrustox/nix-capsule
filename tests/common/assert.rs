//! Shared assertions spoken through the harness Interface: exit/stdio,
//! orderly shutdown, announcement, and error-frame absence. Tests cross this
//! Seam instead of re-stating the same predicates per file.

use std::process::ExitStatus;

use nix_capsule::protocol::Message;

use super::client::ClientOutput;

/// Assert `out` exits with `code` and, when given, matches the exact stdout.
pub fn assert_exit_and_stdout(out: &ClientOutput, code: i32, stdout: Option<&str>) {
    assert_eq!(out.status.code(), Some(code));
    if let Some(want) = stdout {
        assert_eq!(out.stdout, want);
    }
}

/// An orderly shutdown: the server exits 0 — never by signal — and the
/// socket file is gone. Reads the socket state before `Server::stop`.
pub fn assert_orderly_shutdown(status: &ExitStatus, socket_gone: bool) {
    assert_eq!(
        status.code(),
        Some(0),
        "an orderly shutdown exits 0, not by signal"
    );
    assert!(socket_gone, "the socket file must be gone after exit");
}

/// The shutdown must be announced with a `ServerStopping` frame before the
/// drain acts on the connection.
pub fn assert_announced(frames: &[Message], context: &str) {
    assert!(
        frames
            .iter()
            .any(|message| matches!(message, Message::ServerStopping)),
        "{context}: frames={frames:?}"
    );
}

/// A failed kill or a vanished child must never surface as an `Error` frame.
pub fn assert_no_error_frames(frames: &[Message]) {
    assert!(
        !frames
            .iter()
            .any(|message| matches!(message, Message::Error(_))),
        "unexpected Error frames: {frames:?}"
    );
}
