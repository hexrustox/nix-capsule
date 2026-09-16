//! Deep test harness behind one small interface: owns the TempDir, the
//! Cache files (Env dump, hash, Stamp guard stamp), the fake Runtime adapter
//! + fake nix binaries, the Socket listener guard for Liveness, and the
//!   run_ctl env. Tests cross this seam via Fixture::new(Config) plus the
//!   three queries evals, launches, saw — never past it via log strings
//!   or direct field access.

use std::{
    collections::HashMap,
    fs,
    os::unix::{self, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::TempDir;

use super::client::bin_path;

const NCAP_VARS: &[&str] = &[
    "NCAP_PROJECT",
    "NCAP_PROJECT_ROOT",
    "NCAP_CONTAINER",
    "NCAP_SOCKET",
    "NCAP_CACHE_DIR",
    "NCAP_LOG_DIR",
    "NCAP_IMAGE",
    "NCAP_RUNTIME",
    "NCAP_RUN_OPTS",
    "NCAP_WATCH_FILES",
    "NCAP_SERVER",
    "NCAP_NIX",
    "NCAP_BASH",
    "NCAP_TIMEOUT",
    "NCAP_HARDEN",
    "NCAP_LOG_LEVEL",
    "NCAP_DEVSHELL",
    "NCAP_ENV_FORWARD",
    "NCAP_CACHE",
];

fn run_ctl(env: &HashMap<String, String>, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin_path("ncap-ctl"));
    cmd.args(args);
    for var in NCAP_VARS {
        cmd.env_remove(var);
    }
    // Absolute-path fake runtimes can no longer pass through uniform resolve
    // (`podman`/`docker` only): translate one into a `podman` shim on PATH so
    // the child validates while invoking the same stub.
    let mut owned_env = env.clone();
    shim_runtime_env(&mut owned_env);
    for (key, value) in &owned_env {
        cmd.env(key, value);
    }
    // Ensure TMPDIR/XDG vars from test env win; if not set, remove ambient
    // so derivation tests see the unset state.
    for var in [
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ] {
        if !env.contains_key(var) {
            cmd.env_remove(var);
        }
    }
    cmd.output().expect("spawn ncap-ctl")
}

fn shim_runtime_env(env: &mut HashMap<String, String>) {
    let Some(rt) = env.get("NCAP_RUNTIME").cloned() else {
        return;
    };
    if !rt.starts_with('/') {
        return;
    }
    let rt_path = PathBuf::from(&rt);
    if !rt_path.is_file() {
        return;
    }
    let Some(parent) = rt_path.parent() else {
        return;
    };
    let shim = parent.join("podman");
    if !shim.exists() {
        let _ = unix::fs::symlink(&rt_path, &shim);
        if !shim.exists() {
            let _ = fs::copy(&rt_path, &shim);
        }
    }
    let parent_str = parent.to_string_lossy().into_owned();
    let base_path = env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let joined = if base_path.is_empty() {
        parent_str
    } else {
        format!("{parent_str}:{base_path}")
    };
    env.insert("PATH".into(), joined);
    env.insert("NCAP_RUNTIME".into(), "podman".into());
}

fn make_executable(path: &Path) {
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod");
}

fn write_stub(path: &Path, content: &str) {
    fs::write(path, content).expect("write stub");
    make_executable(path);
}

/// Bind a listener on `sock` so the socket half of the liveness predicate
/// (§ Liveness: Running AND connectable) holds. The caller must hold the
/// return value until the ctl invocation completes — dropping it closes the
/// socket and the container counts as not live again.
fn live_socket(sock: &Path) -> std::os::unix::net::UnixListener {
    fs::create_dir_all(sock.parent().unwrap()).expect("sock dir");
    std::os::unix::net::UnixListener::bind(sock).expect("bind socket")
}

/// Freshness of the cached Env dump (CONTEXT.md Freshness: fresh /
/// stale / missing). The Watched files list lives on `Config`, so one
/// `Fresh` variant covers both `["flake.nix"]` and `[]` — the Cache is
/// seeded through the real digest interface over whatever `watch` holds.
pub enum Freshness {
    Fresh,
    Stale,
    Missing,
}

/// Liveness of the Container (CONTEXT.md Liveness: Running AND
/// Socket-connectable; Running alone is not live). `running` seeds the
/// fake Runtime adapter's inspect flag; `connectable` decides whether
/// the Socket listener guard is held.
pub struct Liveness {
    pub running: bool,
    pub connectable: bool,
}

impl Liveness {
    /// Running and connectable.
    pub fn live() -> Self {
        Self {
            running: true,
            connectable: true,
        }
    }

    /// Not running; the Socket guard is still held so `start` can succeed.
    pub fn down() -> Self {
        Self {
            running: false,
            connectable: true,
        }
    }
}

/// Failure modes of the fake Runtime adapter (concurrent-start race,
/// readiness deadline, stop failure). One enum behind the seam replaces
/// the old `with_*` failure builders; the bash flag files stay private
/// to the module implementation. `RunFailAlways` / `StopFail` have no
/// test yet; they document modes the stub honors for future tests.
pub enum Failure {
    RunFailOnce,
    PeerDead,
    NeverRunning(String),
}

/// Declarative description of the world a test needs. One constructor
/// (`Fixture::new`) plus three queries is the whole public surface.
pub struct Config {
    pub liveness: Liveness,
    pub freshness: Freshness,
    pub watch: Vec<String>,
    pub failure: Option<Failure>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            liveness: Liveness::down(),
            freshness: Freshness::Missing,
            watch: Vec::new(),
            failure: None,
        }
    }
}

impl Config {
    /// Fresh Cache over `["flake.nix"]` with a live Container.
    pub fn fresh_live() -> Self {
        Self {
            liveness: Liveness::live(),
            freshness: Freshness::Fresh,
            watch: vec!["flake.nix".to_owned()],
            failure: None,
        }
    }

    /// Fresh Cache over `[]` with the Socket guard held (start-only
    /// tests that watch nothing stay truly fresh).
    pub fn fresh_empty() -> Self {
        Self {
            liveness: Liveness::down(),
            freshness: Freshness::Fresh,
            watch: Vec::new(),
            failure: None,
        }
    }
}

/// What a test may observe about the launch command, parsed once from
/// the fake Runtime adapter logs; tests never touch the log files
/// directly. Structured queries (`has_mount`, `flag_value`, `has_arg`,
/// `ordered_before`, `script_contains`) assert against the argv vector
/// so shell rendering (quoting, spacing) can't break them;
/// `has_text` remains for genuinely free-form probes (absence checks).
pub struct LaunchView {
    line: String,
    args: Vec<String>,
    runs: usize,
}

impl LaunchView {
    /// Whether the fake adapter saw no `run` invocation.
    pub fn is_empty(&self) -> bool {
        self.runs == 0
    }

    /// How many `run` invocations the fake adapter saw.
    pub fn runs(&self) -> usize {
        self.runs
    }

    /// The full argv the fake adapter saw for the launch, verb first,
    /// one entry per argv element. Escape hatch for exact-sequence
    /// assertion: whole-shape regression tests (a duplicated or
    /// misordered verb fails here, `has_arg` cannot).
    pub fn argv(&self) -> &[String] {
        &self.args
    }

    /// Exact argv present (detached `-d`, image separator `--`,
    /// single-argv flags like `--cap-drop=all`, whole extra options).
    pub fn has_arg(&self, arg: &str) -> bool {
        self.args.iter().any(|a| a == arg)
    }

    /// Mount spec present, in any argv shape the launch uses:
    /// standalone spec argv, joined `"-v {spec}"` argv (extra options),
    /// or a `"-v", "{spec}"` pair (default mounts).
    pub fn has_mount(&self, spec: &str) -> bool {
        self.mount_position(spec).is_some()
    }

    /// Value argv following a flag argv (`-w`, `--socket`,
    /// `--log-dir`, `--timeout`).
    pub fn flag_value(&self, flag: &str) -> Option<&str> {
        self.args
            .iter()
            .position(|a| a == flag)
            .and_then(|i| self.args.get(i + 1).map(String::as_str))
    }

    /// Ordering between two mount specs (or joined extra-option argv):
    /// the more-specific mount must come after the broader one.
    pub fn ordered_before(&self, first: &str, second: &str) -> bool {
        match (self.mount_position(first), self.mount_position(second)) {
            (Some(a), Some(b)) => a < b,
            _ => false,
        }
    }

    /// Fragment of the `bash -c` launch script (Env dump `source`,
    /// Server `exec`).
    pub fn script_contains(&self, frag: &str) -> bool {
        self.script_arg().is_some_and(|s| s.contains(frag))
    }

    /// Substring over the rendered invocation line, for free-form
    /// probes (absence checks like `.git`). Prefer the structured
    /// queries above for mounts, flags, and ordering.
    pub fn has_text(&self, s: &str) -> bool {
        self.line.contains(s)
    }

    fn mount_position(&self, spec: &str) -> Option<usize> {
        let joined = format!("-v {spec}");
        self.args
            .iter()
            .position(|a| a == spec || a == &joined)
            .or_else(|| {
                self.args
                    .windows(2)
                    .position(|w| w[0] == "-v" && w[1] == *spec)
                    // `windows` index is the pair start, which sorts
                    // the same as the spec argv for ordering purposes.
                    .map(|i| i + 1)
            })
    }

    fn script_arg(&self) -> Option<&str> {
        self.args
            .iter()
            .position(|a| a == "-c")
            .and_then(|i| self.args.get(i + 1).map(String::as_str))
    }
}

impl std::fmt::Display for LaunchView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line)
    }
}

/// Observable adapter actions for `saw`.
pub enum Action {
    Stop,
    Rm,
    RmBeforeRun,
}

/// The Ctl world behind the seam: tempdir, cache, fake runtime/nix binaries,
/// liveness guard, and the `run_ctl` env. Tests declare the world via
/// [`Config`] and observe it via `evals`, `launches`, and `saw`.
pub struct Fixture {
    root: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
    sock: PathBuf,
    env: HashMap<String, String>,
    state: PathBuf,
    runtime_log: PathBuf,
    runtime_args_log: PathBuf,
    nix_log: PathBuf,
    _tmp: TempDir,
    _live: Option<std::os::unix::net::UnixListener>,
}

/// A live-capable fake Runtime adapter: `run` flips Running true so the
/// start readiness poll (Liveness) can succeed; `stop` clears it. Honors
/// `run_fail_first` (concurrent-start race), `run_fail_always`,
/// `run_never` (readiness deadline), `stop_fail`, and `state_json` for
/// inspect state output — one stub template behind the seam.
/// Logs the single-line invocation to `runtime_log` plus one argv per
/// line for `run` invocations to `runtime_args_log` — both read once
/// behind the `launches` query, never by tests directly.
fn write_live_stub(runtime_bin: &Path, state: &Path, runtime_log: &Path, runtime_args_log: &Path) {
    let stub = format!(
        r#"#!/usr/bin/env bash
LOG="{}"
ARGS_LOG="{}"
STATE_DIR="{}"
echo "$@" >> "$LOG"
case "$1" in
  inspect)
TEMPLATE="$3"
if [[ "$TEMPLATE" == *'State.Running'* ]]; then
  COUNT=$(cat "$STATE_DIR/run_count" 2>/dev/null || echo 0)
  if [[ -f "$STATE_DIR/peer_dead" && $COUNT -ge 2 ]]; then echo "true";
  else cat "$STATE_DIR/running" 2>/dev/null || echo "false"; fi
elif [[ "$TEMPLATE" == *'json .State'* ]] || [[ "$TEMPLATE" == *'json'* ]]; then
  cat "$STATE_DIR/state_json" 2>/dev/null || echo '{{"Running":false,"Status":"exited"}}'
else
  echo "exists"
fi
exit 0
;;
  run)
COUNT_FILE="$STATE_DIR/run_count"
COUNT=$(cat "$COUNT_FILE" 2>/dev/null || echo 0)
COUNT=$((COUNT+1))
echo $COUNT > "$COUNT_FILE"
printf "%s\n" "$@" >> "$ARGS_LOG"
if [[ -f "$STATE_DIR/run_fail_first" && $COUNT -eq 1 ]]; then
  echo "Error: container name \"ncap-test\" is already in use - name in use" >&2
  exit 1
fi
if [[ -f "$STATE_DIR/run_fail_always" ]]; then
  cat "$STATE_DIR/run_fail_always" >&2
  exit 1
fi
if [[ ! -f "$STATE_DIR/run_never" ]]; then
  echo "true" > "$STATE_DIR/running"
fi
echo "fake-id-$COUNT"
exit 0
;;
  stop)
if [[ -f "$STATE_DIR/stop_fail" ]]; then
  echo "no such container" >&2
  exit 1
fi
echo "false" > "$STATE_DIR/running"
echo "stopped"
exit 0
;;
  rm) echo "removed" >> "$LOG"; echo "rm" > "$STATE_DIR/rm_called"; exit 0 ;;
  *) exit 1 ;;
esac
"#,
        runtime_log.display(),
        runtime_args_log.display(),
        state.display()
    );
    write_stub(runtime_bin, &stub);
}

fn assemble(tmp: TempDir, config: Config) -> Fixture {
    let root = tmp.path().join("proj");
    fs::create_dir_all(&root).expect("root");
    fs::write(root.join("flake.nix"), "x").expect("watch file");
    let cache = tmp.path().join("cache");
    let logs = tmp.path().join("logs");
    let sock = tmp.path().join("sock/ncap.sock");
    let state = tmp.path().join("state");
    fs::create_dir_all(&state).expect("state");
    fs::write(
        state.join("running"),
        if config.liveness.running {
            "true"
        } else {
            "false"
        },
    )
    .expect("running");
    let runtime_log = tmp.path().join("runtime.log");
    let runtime_args_log = tmp.path().join("runtime-args.log");
    let nix_log = tmp.path().join("nix.log");
    let runtime_bin = tmp.path().join("fake-runtime");
    let nix_bin = tmp.path().join("fake-nix");
    write_live_stub(&runtime_bin, &state, &runtime_log, &runtime_args_log);
    fake_nix(&nix_bin, &nix_log, "export FOO=bar\n");

    let watch_json = format!(
        "[{}]",
        config
            .watch
            .iter()
            .map(|w| format!("{w:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut env = base_env(&root, &cache, &logs, &sock, &runtime_bin, &nix_bin);
    env.insert("NCAP_WATCH_FILES".into(), watch_json);

    let _live = if config.liveness.connectable {
        Some(live_socket(&sock))
    } else {
        None
    };

    // Seed the Cache through the real digest interface where fresh
    // (digest of the configured watch list, no hardcoded hash).
    match config.freshness {
        Freshness::Fresh => {
            let digest = nix_capsule::ctl::digest::compute(&root, &config.watch).expect("digest");
            fs::create_dir_all(&cache).expect("cache");
            fs::write(cache.join("env"), "export FOO=bar\n").expect("env");
            fs::write(cache.join("hash"), &digest).expect("hash");
            fs::write(cache.join("project"), root.to_string_lossy().as_ref()).expect("stamp");
        }
        Freshness::Stale => {
            fs::create_dir_all(&cache).expect("cache");
            fs::write(cache.join("env"), "export OLD=1\n").expect("env");
            fs::write(cache.join("hash"), "0000000000000000").expect("stale hash");
            fs::write(cache.join("project"), root.to_string_lossy().as_ref()).expect("stamp");
        }
        Freshness::Missing => {}
    }

    // Failure modes of the fake Runtime adapter, behind the seam.
    match config.failure.as_ref() {
        None => {}
        Some(Failure::RunFailOnce) => {
            fs::write(state.join("run_fail_first"), "").expect("run_fail_first");
        }
        Some(Failure::PeerDead) => {
            // Peer-dead retry = first run fails with name-in-use, then the
            // retry succeeds: same flag, plus inspect sees Running after rm.
            fs::write(state.join("run_fail_first"), "").expect("run_fail_first");
            fs::write(state.join("peer_dead"), "").expect("peer_dead");
        }
        Some(Failure::NeverRunning(state_json)) => {
            fs::write(state.join("run_never"), "").expect("run_never");
            fs::write(state.join("running"), "false").expect("running");
            fs::write(state.join("state_json"), state_json).expect("state json");
        }
    }

    // Touch empty logs so query methods never hit missing files.
    let _ = fs::write(&runtime_log, "");
    let _ = fs::write(&runtime_args_log, "");
    let _ = fs::write(&nix_log, "");

    Fixture {
        root,
        cache,
        logs,
        sock,
        state,
        runtime_log,
        runtime_args_log,
        nix_log,
        env,
        _tmp: tmp,
        _live,
    }
}

impl Fixture {
    /// The single constructor behind the seam: every test declares the
    /// Liveness, Freshness, Watched files, and Runtime adapter failure it
    /// needs. Env-only tweaks stay as `with_*` setters below.
    pub fn new(config: Config) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        assemble(tmp, config)
    }

    /// Named-root variant for Project root derivation tests.
    pub fn with_root_name(name: &str) -> Self {
        let mut fx = Self::new(Config::default());
        fx.set_root(name);
        fx
    }

    /// Override `NCAP_WATCH_FILES` with a JSON list (traversal-refusal tests).
    pub fn with_watch_files(mut self, json: &str) -> Self {
        self.env.insert("NCAP_WATCH_FILES".into(), json.into());
        self
    }

    /// Override `NCAP_HARDEN`.
    pub fn with_harden(mut self, on: bool) -> Self {
        self.env.insert(
            "NCAP_HARDEN".into(),
            if on { "true".into() } else { "false".into() },
        );
        self
    }

    /// Override `NCAP_LOG_LEVEL`.
    pub fn with_log_level(mut self, level: &str) -> Self {
        self.env.insert("NCAP_LOG_LEVEL".into(), level.into());
        self
    }

    /// Insert one var into the `run_ctl` env.
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Override `NCAP_TIMEOUT` (drain-grace seconds).
    pub fn with_timeout(mut self, secs: &str) -> Self {
        self.env.insert("NCAP_TIMEOUT".into(), secs.to_string());
        self
    }

    /// Drop one var from the `run_ctl` env (refusal tests).
    pub fn without(mut self, key: &str) -> Self {
        self.env.remove(key);
        self
    }

    /// Prepend a dir to `PATH` in the `run_ctl` env.
    pub fn with_path_prepend(mut self, dir: &Path) -> Self {
        let base = self
            .env
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        let joined = if base.is_empty() {
            dir.to_string_lossy().into_owned()
        } else {
            format!("{}:{base}", dir.display())
        };
        self.env.insert("PATH".into(), joined);
        self
    }

    /// Make the fake Runtime adapter discoverable as `name` (`podman` or
    /// `docker`) for `NCAP_RUNTIME=auto`: a stub dir holds symlinks to the
    /// adapter and to the `bash`/`cat` binaries it needs, and `PATH` is set
    /// to exactly that dir — deterministic regardless of the ambient
    /// machine (which may carry a real runtime somewhere on `PATH`).
    pub fn with_runtime_discoverable(self, name: &str) -> Self {
        let bin = self.tmp_path().join("discover");
        fs::create_dir_all(&bin).expect("discover dir");
        let runtime = self.tmp_path().join("fake-runtime");
        unix::fs::symlink(&runtime, bin.join(name)).expect("symlink fake runtime");
        self.with_path_prepend(&bin)
    }

    /// Borrowed views of fixture paths for building launch expectations.
    /// Tests state what they expect; the layout itself stays inside.
    pub fn project_root(&self) -> &Path {
        &self.root
    }

    /// The Cache dir backing this fixture.
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    /// The log dir handed to the child.
    pub fn log_dir(&self) -> &Path {
        &self.logs
    }

    /// The socket path handed to the child.
    pub fn socket_path(&self) -> &Path {
        &self.sock
    }

    /// Parent dir of [`Self::socket_path`], for mount expectations.
    pub fn socket_parent_dir(&self) -> PathBuf {
        self.sock.parent().expect("sock parent").to_path_buf()
    }

    /// `NCAP_SOCKET` currently in the env map (XDG fallback flow).
    pub fn socket_from_env(&self) -> PathBuf {
        PathBuf::from(self.env.get("NCAP_SOCKET").expect("NCAP_SOCKET in env"))
    }

    /// Bind the Liveness listener guard at the `NCAP_SOCKET` from the env
    /// map (after `apply_setup_env` in the XDG fallback flow).
    pub fn hold_socket_from_env(&mut self) {
        let sock = self.socket_from_env();
        self._live = Some(live_socket(&sock));
    }

    /// Does `name` exist inside the Project root?
    pub fn project_has(&self, name: &str) -> bool {
        self.root.join(name).exists()
    }

    /// Create a dir inside the Project root (e.g. `adir` refusal case).
    pub fn seed_dir(&self, name: &str) {
        fs::create_dir_all(self.root.join(name)).expect("seed dir");
    }

    /// Contents of the Stamp guard stamp file.
    pub fn stamp_content(&self) -> String {
        fs::read_to_string(self.cache.join("project")).expect("stamp")
    }

    /// Cache / log / Socket existence queries for `clean` assertions.
    pub fn cache_has(&self, name: &str) -> bool {
        self.cache.join(name).exists()
    }

    /// Whether the Cache dir still exists.
    pub fn cache_is_dir(&self) -> bool {
        self.cache.is_dir()
    }

    /// Whether a log file exists.
    pub fn logs_has(&self, name: &str) -> bool {
        self.logs.join(name).exists()
    }

    /// Whether the log dir still exists.
    pub fn logs_is_dir(&self) -> bool {
        self.logs.is_dir()
    }

    /// Whether the Socket file exists.
    pub fn socket_exists(&self) -> bool {
        self.sock.exists()
    }

    /// Whether the Socket parent dir still exists.
    pub fn socket_parent_is_dir(&self) -> bool {
        self.sock.parent().is_some_and(|p| p.is_dir())
    }

    /// Flip the fake Runtime adapter's Running flag.
    pub fn set_running(&self, running: bool) {
        fs::write(
            self.state.join("running"),
            if running { "true" } else { "false" },
        )
        .expect("running");
    }

    /// Poison the cached hash so the Cache reads as stale.
    pub fn make_stale(&self) {
        fs::write(self.cache.join("hash"), "0000000000000000").expect("stale hash");
    }

    /// Remove the cached Env dump so the Cache reads as missing.
    pub fn make_missing_env(&self) {
        let _ = fs::remove_file(self.cache.join("env"));
    }

    /// Seed two server logs (old + newest) for the log-tail assertion.
    pub fn seed_server_logs(&self, old: &str, newest: &str) {
        fs::create_dir_all(&self.logs).expect("logs");
        fs::write(self.logs.join("ncap-server-100.log"), old).expect("log");
        fs::write(self.logs.join("ncap-server-999.log"), newest).expect("newest log");
    }

    /// Seed a `.git` dir inside the Project root (git-mount tests).
    pub fn seed_git(&self) {
        fs::create_dir_all(self.root.join(".git")).expect("git dir");
    }

    /// Full clean layout: Cache files + generation link + foreign cache
    /// file, server logs + foreign log, Socket file + sibling.
    pub fn seed_clean_full(&self) {
        fs::create_dir_all(&self.cache).expect("cache");
        fs::write(self.cache.join("env"), "export FOO=bar\n").expect("env");
        let digest = nix_capsule::ctl::digest::compute(&self.root, &["flake.nix".to_owned()])
            .expect("digest");
        fs::write(self.cache.join("hash"), &digest).expect("hash");
        fs::write(self.cache.join("profile"), "profile").expect("profile");
        fs::write(
            self.cache.join("project"),
            self.root.to_string_lossy().as_ref(),
        )
        .expect("stamp");
        fs::write(self.cache.join("profile-1-link"), "link").expect("gen link");
        fs::write(self.cache.join("unrelated.txt"), "keep me").expect("foreign");
        fs::create_dir_all(&self.logs).expect("logs");
        fs::write(self.logs.join("ncap-server-1000.log"), "old").expect("log 1");
        fs::write(self.logs.join("ncap-server-2000.log"), "new").expect("log 2");
        fs::write(self.logs.join("not-a-server-log.txt"), "keep me").expect("foreign log");
        // Drop any Liveness listener residue so a plain file can stand in.
        let _ = fs::remove_file(&self.sock);
        fs::create_dir_all(self.sock.parent().unwrap()).expect("sock dir");
        fs::write(&self.sock, "socket").expect("socket file");
        let sibling = self.sock.parent().unwrap().join("sibling.txt");
        fs::write(&sibling, "keep me").expect("sibling");
    }

    /// Minimal clean layout: only owned files, so empty dirs are removed.
    pub fn seed_clean_minimal(&self) {
        fs::create_dir_all(&self.cache).expect("cache");
        fs::write(self.cache.join("env"), "export FOO=bar\n").expect("env");
        let digest = nix_capsule::ctl::digest::compute(&self.root, &["flake.nix".to_owned()])
            .expect("digest");
        fs::write(self.cache.join("hash"), &digest).expect("hash");
        fs::write(self.cache.join("profile"), "profile").expect("profile");
        fs::write(
            self.cache.join("project"),
            self.root.to_string_lossy().as_ref(),
        )
        .expect("stamp");
        fs::create_dir_all(&self.logs).expect("logs");
        fs::write(self.logs.join("ncap-server-1000.log"), "log").expect("log");
        let _ = fs::remove_file(&self.sock);
        fs::create_dir_all(self.sock.parent().unwrap()).expect("sock dir");
        fs::write(&self.sock, "socket").expect("socket file");
    }

    /// Point the Fixture at another Project root under the same Cache
    /// (Stamp guard collision test).
    pub fn set_root(&mut self, name: &str) -> PathBuf {
        let new_root = self._tmp.path().join(name);
        fs::create_dir_all(&new_root).expect("root");
        self.root = new_root.clone();
        self.env.insert(
            "NCAP_PROJECT_ROOT".into(),
            new_root.to_string_lossy().into_owned(),
        );
        new_root
    }

    /// Parse `setup-env` output back into the env map (XDG fallback flow).
    pub fn apply_setup_env(&mut self) {
        let out = self.setup_env();
        assert!(
            out.status.success(),
            "setup-env: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let body = line.strip_prefix("export ").unwrap_or(line);
            let (var, quoted) = body.split_once('=').expect("VAR='value'");
            let value = quoted
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
                .unwrap_or(quoted)
                .replace("'\\''", "'");
            self.env.insert(var.to_owned(), value);
        }
    }

    /// Drop the Liveness listener guard (the Container counts as not live).
    pub fn not_live(mut self) -> Self {
        self._live = None;
        self
    }

    /// Run `ncap-ctl init` in this fixture's env.
    pub fn init(&self) -> Output {
        run_ctl(&self.env, &["init"])
    }

    /// Run `ncap-ctl start` in this fixture's env.
    pub fn start(&self) -> Output {
        run_ctl(&self.env, &["start"])
    }

    /// Run `ncap-ctl stop` in this fixture's env.
    pub fn stop(&self) -> Output {
        run_ctl(&self.env, &["stop"])
    }

    /// Run `ncap-ctl status` in this fixture's env.
    pub fn status(&self) -> Output {
        run_ctl(&self.env, &["status"])
    }

    /// Run `ncap-ctl clean` in this fixture's env.
    pub fn clean(&self) -> Output {
        run_ctl(&self.env, &["clean"])
    }

    /// Run `ncap-ctl restart` in this fixture's env.
    pub fn restart(&self) -> Output {
        run_ctl(&self.env, &["restart"])
    }

    /// Run `ncap-ctl setup-env` in this fixture's env.
    pub fn setup_env(&self) -> Output {
        run_ctl(&self.env, &["setup-env"])
    }

    /// Raw invocation without the absolute-path Runtime adapter shim,
    /// for the NCAP_RUNTIME rejection test.
    pub fn run_raw(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(bin_path("ncap-ctl"));
        cmd.args(args);
        for var in NCAP_VARS {
            cmd.env_remove(var);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        for var in [
            "TMPDIR",
            "XDG_RUNTIME_DIR",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        ] {
            if !self.env.contains_key(var) {
                cmd.env_remove(var);
            }
        }
        cmd.output().expect("spawn ncap-ctl")
    }

    fn read_log(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    /// Number of `print-dev-env` evals seen by the fake `nix`.
    pub fn evals(&self) -> usize {
        Self::read_log(&self.nix_log)
            .lines()
            .filter(|l| l.contains("print-dev-env"))
            .count()
    }

    /// Parsed view of the launch command: single-line invocation plus one
    /// argv per line plus the `run` invocation count.
    pub fn launches(&self) -> LaunchView {
        let line = Self::read_log(&self.runtime_log)
            .lines()
            .find(|l| l.contains("run "))
            .unwrap_or_default()
            .to_owned();
        let args = Self::read_log(&self.runtime_args_log)
            .lines()
            .map(|l| l.to_owned())
            .collect::<Vec<_>>();
        let runs = Self::read_log(&self.runtime_log)
            .lines()
            .filter(|l| l.contains("run "))
            .count();
        LaunchView { line, args, runs }
    }

    /// Observable Runtime adapter actions: `Stop`, `Rm`, `RmBeforeRun`
    /// (pre-launch `rm` ran before the launch `run`).
    pub fn saw(&self, action: Action) -> bool {
        let log = Self::read_log(&self.runtime_log);
        match action {
            Action::Stop => log.lines().any(|l| l.contains("stop ")),
            Action::Rm => log.contains("rm ") || self.state.join("rm_called").is_file(),
            Action::RmBeforeRun => match (log.find("rm "), log.find("run ")) {
                (Some(rm), Some(run)) => rm < run,
                _ => false,
            },
        }
    }

    /// Contents of the `run_count` file written by the fake adapter.
    pub fn run_count_file(&self) -> String {
        Self::read_log(&self.state.join("run_count"))
            .trim()
            .to_owned()
    }

    /// Full contents of the fake Runtime adapter log.
    pub fn runtime_log(&self) -> String {
        Self::read_log(&self.runtime_log)
    }

    /// Empty the fake Runtime adapter logs (between phases of one test).
    pub fn clear_runtime_log(&self) {
        let _ = fs::write(&self.runtime_log, "");
        let _ = fs::write(&self.runtime_args_log, "");
    }

    /// Drop the Liveness guard and remove the Socket file stand-in.
    pub fn drop_live(&mut self) {
        self._live = None;
        let _ = fs::remove_file(&self.sock);
    }

    /// The tempdir backing this fixture.
    pub fn tmp_path(&self) -> PathBuf {
        self._tmp.path().to_path_buf()
    }
}

fn fake_nix(path: &Path, nix_log: &Path, env_content: &str) {
    // env_content is what print-dev-env should output (written to a file
    // that the stub cats).
    let env_file = path.with_extension("env");
    fs::write(&env_file, env_content).expect("write nix env file");
    let script = format!(
        r#"#!/usr/bin/env bash
LOG="{}"
ENV_SRC="{}"
echo "$@" >> "$LOG"
case "$1" in
  print-dev-env)
# Find --profile value (next arg after --profile)
PROFILE=""
PREV=""
for ARG in "$@"; do
  if [[ "$PREV" == "--profile" ]]; then PROFILE="$ARG"; fi
  PREV="$ARG"
done
if [[ -n "$PROFILE" ]]; then
  mkdir -p "$(dirname "$PROFILE")"
  touch "$PROFILE"
fi
cat "$ENV_SRC"
exit 0
;;
  profile)
# wipe-history
echo "wipe-history $@" >> "$LOG"
exit 0
;;
  *)
echo "unknown nix $1" >&2
exit 1
;;
esac
"#,
        nix_log.display(),
        env_file.display()
    );
    write_stub(path, &script);
}

fn base_env(
    project_root: &Path,
    cache_dir: &Path,
    log_dir: &Path,
    socket: &Path,
    runtime_bin: &Path,
    nix_bin: &Path,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "NCAP_PROJECT_ROOT".into(),
        project_root.to_string_lossy().into_owned(),
    );
    env.insert(
        "NCAP_CACHE_DIR".into(),
        cache_dir.to_string_lossy().into_owned(),
    );
    env.insert(
        "NCAP_LOG_DIR".into(),
        log_dir.to_string_lossy().into_owned(),
    );
    env.insert("NCAP_SOCKET".into(), socket.to_string_lossy().into_owned());
    env.insert("NCAP_CONTAINER".into(), "ncap-test".into());
    env.insert("NCAP_PROJECT".into(), "test".into());
    env.insert("NCAP_IMAGE".into(), "alpine:latest".into());
    env.insert(
        "NCAP_SERVER".into(),
        "/nix/store/fake/bin/ncap-server".into(),
    );
    env.insert("NCAP_NIX".into(), nix_bin.to_string_lossy().into_owned());
    env.insert("NCAP_BASH".into(), "/nix/store/fake/bin/bash".into());
    env.insert("NCAP_DEVSHELL".into(), ".#container".into());
    env.insert(
        "NCAP_RUNTIME".into(),
        runtime_bin.to_string_lossy().into_owned(),
    );
    env.insert("NCAP_TIMEOUT".into(), "2".into());
    env.insert("NCAP_WATCH_FILES".into(), "[]".into());
    env.insert("NCAP_RUN_OPTS".into(), "[]".into());
    env.insert("NCAP_HARDEN".into(), "false".into());
    env.insert("NCAP_LOG_LEVEL".into(), "warning".into());
    // Keep HOME for XDG fallbacks where needed; tests override when testing fallback.
    if let Ok(home) = std::env::var("HOME") {
        env.insert("HOME".into(), home);
    }
    env
}
