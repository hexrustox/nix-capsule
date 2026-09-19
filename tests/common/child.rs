//! Child-lifecycle helpers: what a test observes about a Child after the
//! wire goes quiet — marker files, flag files, and `/proc` reaping. Tests
//! reach for these when the subject is disconnect TERM, shutdown TERM, or
//! zombie reaping; wire frames stay in [`super::probe`], shell text in
//! [`super::script`].

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use super::server::Server;

/// Tight bound: full-[`crate::common::script::SHELL_BODY`] holds — the drain
/// deadline must expire and the client bail — must land within ~one want.
pub(crate) const WAIT_TIGHT: Duration = Duration::from_secs(2);

/// Roomy bound: reaping a provably-dead child and the group's TERM-trap
/// markers must arrive well inside the harness headroom.
pub(crate) const WAIT_ROOMY: Duration = Duration::from_secs(5);

/// Upper bound on one phase; a red run fails on the assertion, never on the
/// harness itself. Stays below a `SHELL_BODY`-second `sleep`, so a survivor
/// outlasts the test.
pub(crate) const WAIT_PHASE: Duration = Duration::from_secs(20);

/// Poll a synchronous predicate every 25 ms until it holds or `limit`
/// elapses; `false` means the deadline passed with the predicate still
/// failing.
pub(crate) async fn poll_until(limit: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Poll `marker` until it contains `needle`, via [`poll_until`].
pub(crate) async fn wait_for_marker(marker: &Path, needle: &str, limit: Duration) -> bool {
    poll_until(limit, || {
        fs::read_to_string(marker).is_ok_and(|content| content.contains(needle))
    })
    .await
}

/// Poll until `name` exists in the server's tempdir — the child's cwd — or
/// panic; children write flag files as observable progress markers.
pub(crate) async fn wait_for_flag(server: &Server, name: &str) {
    let flag = server.path().join(name);
    let appeared = poll_until(WAIT_PHASE, || flag.exists()).await;
    assert!(appeared, "{name} never appeared");
}

/// Send `script` as a request, wait for the child to announce `ready` on
/// stdout, then drop the connection abruptly — the client vanishes before
/// any terminal frame.
pub(crate) async fn request_and_vanish(server: &Server, script: &str, ready: &str) {
    let mut framed = server.raw().await;
    super::probe::send_request(&mut framed, server.path(), script).await;
    super::probe::read_until_stdout_contains(&mut framed, ready).await;
    drop(framed);
}

/// Send `script` as a request, wait for the child to announce `ready` on
/// stdout, drop the connection abruptly, and assert the disconnect TERM
/// stamp `gone` lands in `marker` within `limit`.
pub(crate) async fn vanish_and_confirm_gone(
    server: &Server,
    script: &str,
    ready: &str,
    marker: &Path,
    limit: Duration,
) {
    request_and_vanish(server, script, ready).await;
    assert!(
        wait_for_marker(marker, "gone", limit).await,
        "the group outlived {limit:?} after the client vanished"
    );
}

/// Run `script` over a fresh connection and collect frames through the
/// terminal one, asserting a clean exit with `expect` on stdout. The server
/// is left running — stop it after any marker reads, whose tempdir teardown
/// `stop` takes with it.
pub(crate) async fn second_connection_succeeds(
    server: &Server,
    script: &str,
    expect: &str,
    context: &str,
) {
    let mut framed = server.raw().await;
    let run =
        super::probe::run_raw_request(&mut framed, super::probe::request(server.path(), script))
            .await;
    super::probe::assert_clean_exit(&run.frames, context);
    assert!(run.stdout.contains(expect), "stdout={:?}", run.stdout);
}

/// Pids of zombie processes whose parent is `server_pid`, scanned straight
/// from `/proc`: a child the server never reaped stays visible here in state
/// `Z` forever, so an empty result means nothing is left to reap.
pub(crate) fn zombies_under(server_pid: u32) -> Vec<u32> {
    let mut zombies = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return zombies;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `comm` may carry spaces and parens; the fixed fields resume after
        // the last `)`. State is field 3, ppid field 4.
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or_default();
        let ppid = fields.next().unwrap_or_default();
        if state == "Z" && ppid == server_pid.to_string() {
            zombies.push(pid);
        }
    }
    zombies
}
