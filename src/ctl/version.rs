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
/// `RequestVersion`, gives the reply one bounded wait, and renders the
/// skew warning line — `Some` means print it, `None` stays silent
/// (unreachable socket, no answer inside the wait, reply that is neither
/// a skewed version nor an `Error`. Whatever prints never fails the
/// command).
pub(crate) async fn skew_warning(socket: &Path) -> Option<String> {
    let stream = UnixStream::connect(socket).await.ok()?;
    let mut framed = Framed::new(stream, FrameCodec);

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
        Err(_) | Ok(None) | Ok(Some(Err(_))) => return None,
        Ok(Some(Ok(frame))) => frame,
    };
    match Message::from_frame(frame) {
        // Skew is exact string inequality against the host binaries' version
        // (spec/ctl.md § Version probe).
        Ok(Message::ServerVersion(VersionMsg { version })) if version != CURRENT_VERSION => {
            Some(format!(
                "`ncap-server` on `{}` is running version {version}, the host binaries are \
                 {CURRENT_VERSION}\nrun `ncap-ctl restart` to match them back at one version",
                socket.display(),
            ))
        }
        // A pre-probe Server rejects the unknown tag with `Error` and close —
        // the same skew warning, minus a version it will not report.
        Ok(Message::Error(_)) => Some(format!(
            "`ncap-server` on `{}` will not report a version (it predates the version probe), \
             the host binaries are {CURRENT_VERSION}\nrun `ncap-ctl restart` to match them back \
             at one version",
            socket.display(),
        )),
        _ => None,
    }
}
