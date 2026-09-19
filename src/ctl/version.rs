//! Version probe: after liveness, `status` and `start` Ctl open one
//! Version probe Connection (spec/protocol.md § Version probe) and warn on
//! Version skew (CONTEXT.md Version skew). `init` never probes.

use std::{path::Path, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use crate::protocol::{CURRENT_VERSION, FrameCodec, Message, VersionMsg};

/// One bounded wait for the `ServerVersion` reply: a Server that accepts
/// but never answers inside it skips the probe silently.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The one Version probe Ctl opens for `status` and `start`: sends
/// `RequestVersion`, gives the reply one bounded wait, and prints the
/// skew warning itself — the binary prefix lives here because nothing
/// upstream carries it (unreachable socket, no answer inside the wait,
/// reply that is neither a skewed version nor an `Error` all stay
/// silent). Whatever prints never fails the command.
pub(crate) async fn warn_on_skew(socket: &Path) {
    let mut framed = match UnixStream::connect(socket).await {
        Ok(stream) => Framed::new(stream, FrameCodec),
        // Unreachable socket: skip silently.
        Err(_) => return,
    };

    let _ = tokio::time::timeout(
        PROBE_TIMEOUT,
        framed.send(
            Message::RequestVersion
                .into_frame()
                .expect("probe frame encodes"),
        ),
    )
    .await;

    let reply = tokio::time::timeout(PROBE_TIMEOUT, framed.next()).await;
    let frame = match reply {
        Ok(Some(Ok(frame))) => frame,
        _ => return,
    };
    match Message::from_frame(frame) {
        // Skew is exact string inequality against the host binaries' version
        // (spec/ctl.md § Version probe). The printing exception to the
        // toplevel-rendering rule (docs/agents/error.md § Rendering): a
        // notice, not an error, and never failing the command.
        Ok(Message::ServerVersion(VersionMsg { version })) => {
            if version != CURRENT_VERSION {
                eprintln!(
                    "ncap-ctl: server on `{}` is running version {version}, the host binaries are {CURRENT_VERSION}",
                    socket.display(),
                );
                eprintln!("run `ncap-ctl restart` to match them back at one version");
            }
        }
        // A pre-probe Server rejects the unknown tag with `Error` and close —
        // the same skew warning, minus a version it will not report.
        _ => {
            eprintln!(
                "ncap-ctl: server on `{}` will not report a version, the host binaries are {CURRENT_VERSION}",
                socket.display(),
            );
            eprintln!("run `ncap-ctl restart` to match them back at one version");
        }
    }
}
