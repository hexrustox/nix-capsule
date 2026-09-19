//! Deep test harness behind one small interface: owns the TempDir, the
//! Cache files (Env dump, hash, Stamp guard stamp), the fake Runtime adapter
//! + fake nix binaries, the Socket listener guard for Liveness, and the
//! run_ctl env. Tests cross this seam via Fixture::new(Config) plus the
//! three queries evals, launches, saw and the LaunchView `expects_*`
//! behavior assertions — never past it via log strings, raw paths,
//! or direct field access.

use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    net::Shutdown,
    os::unix::{
        self,
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use tempfile::TempDir;

use nix_capsule::ctl::digest;
use nix_capsule::protocol::{FrameType, VersionMsg};

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

/// Freshness of the cached Env dump (CONTEXT.md Freshness: fresh /
/// stale / missing). The Watched files list lives on `Config`, so one
/// `Fresh` variant covers both `["flake.nix"]` and `[]` — the Cache is
/// seeded through the real digest interface over whatever `watch` holds.
pub(crate) enum Freshness {
    Fresh,
    Stale,
    Missing,
}

/// Liveness of the Container (CONTEXT.md Liveness: Running AND
/// Socket-connectable; Running alone is not live). `running` seeds the
/// fake Runtime adapter's inspect flag; `connectable` decides whether
/// the Socket listener guard is held.
pub(crate) struct Liveness {
    pub(crate) running: bool,
    pub(crate) connectable: bool,
}

impl Liveness {
    pub(crate) fn live() -> Self {
        Self {
            running: true,
            connectable: true,
        }
    }

    /// The Socket guard stays held so `start` can succeed.
    pub(crate) fn down() -> Self {
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
pub(crate) enum Failure {
    RunFailOnce,
    PeerDead,
    NeverRunning(String),
}

/// What the wire-speaking responder behind the live socket answers to the
/// Version probe (spec/protocol.md § Version probe). `Matching` and `Skew`
/// reply `ServerVersion` with the given version; `Stale` stands in for a
/// pre-probe Server (replies `Error` and closes); `Silent` is the plain
/// listener — a Server accepts, never answers.
#[derive(Clone)]
pub(crate) enum Responder {
    /// `ServerVersion` with the host binaries' own version — no skew.
    Matching,
    /// `ServerVersion` with a version the caller pins.
    Skew(String),
    /// `Error` and close — a Server that predates the probe.
    Stale,
    /// Accepts, never answers: probe times out.
    Silent,
}

/// Declarative description of the world a test needs. One constructor
/// (`Fixture::new`) plus three queries is the whole public surface.
pub(crate) struct Config {
    pub(crate) liveness: Liveness,
    pub(crate) freshness: Freshness,
    pub(crate) watch: Vec<String>,
    pub(crate) failure: Option<Failure>,
    pub(crate) responder: Responder,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            liveness: Liveness::down(),
            freshness: Freshness::Missing,
            watch: Vec::new(),
            failure: None,
            responder: Responder::Matching,
        }
    }
}

impl Config {
    /// Fresh Cache over `["flake.nix"]` with a live Container.
    pub(crate) fn fresh_live() -> Self {
        Self {
            liveness: Liveness::live(),
            freshness: Freshness::Fresh,
            watch: vec!["flake.nix".to_owned()],
            ..Self::default()
        }
    }

    /// Fresh Cache over `[]` with the Socket guard held (start-only
    /// tests that watch nothing stay truly fresh).
    pub(crate) fn fresh_empty() -> Self {
        Self {
            liveness: Liveness::down(),
            freshness: Freshness::Fresh,
            ..Self::default()
        }
    }

    pub(crate) fn with_responder(mut self, responder: Responder) -> Self {
        self.responder = responder;
        self
    }
}

/// What a test may observe about the launch command, parsed once from
/// the fake Runtime adapter logs; tests never touch the log files
/// directly. Behavior assertions (`expects_default_mounts`,
/// `expects_harden_mounts`, `expects_watch_mount`, …) carry the
/// mount-set policy behind the seam — tests state what they expect,
/// never how a spec string is rendered. Raw argv/mount/text probes
/// are private helpers below, not test API.
pub(crate) struct LaunchView {
    line: String,
    args: Vec<String>,
    runs: usize,
    root: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
    sock: PathBuf,
    socket_dir: PathBuf,
}

impl LaunchView {
    pub(crate) fn is_empty(&self) -> bool {
        self.runs == 0
    }

    pub(crate) fn runs(&self) -> usize {
        self.runs
    }

    /// Exact default mount set plus the Server launch shape: `/nix:ro`,
    /// Socket dir, Project root + `-w`, Cache `:ro`, log `rw`, no `.git`,
    /// Env dump `source` + Server `exec` with `--socket/--log-dir/`
    /// `--timeout/--log-level`, detached `run -d … -- image bash -c`.
    pub(crate) fn expects_default_mounts(&self) {
        assert!(!self.line.is_empty(), "must have run line");
        let socket_dir = self.socket_dir.to_string_lossy().into_owned();
        let root = self.root.to_string_lossy().into_owned();
        let cache = self.cache.to_string_lossy().into_owned();
        let logs = self.logs.to_string_lossy().into_owned();
        let sock = self.sock.to_string_lossy().into_owned();
        assert!(
            self.has_mount("/nix:/nix:ro"),
            "missing /nix ro mount: {self}"
        );
        assert!(
            self.has_mount(&format!("{socket_dir}:{socket_dir}")),
            "missing socket dir mount: {self}"
        );
        assert!(
            self.has_mount(&format!("{root}:{root}")),
            "missing project root mount: {self}"
        );
        assert_eq!(
            self.flag_value("-w"),
            Some(root.as_str()),
            "missing workdir: {self}"
        );
        assert!(
            self.has_mount(&format!("{cache}:{cache}:ro")),
            "missing cache ro mount: {self}"
        );
        assert!(
            self.has_mount(&format!("{logs}:{logs}")),
            "missing log rw mount: {self}"
        );
        self.expects_no_git();
        assert!(
            self.script_contains(&format!("source '{cache}/env'")),
            "missing source dump: {self}"
        );
        assert!(
            self.script_contains("&& exec '/nix/store/fake/bin/ncap-server'"),
            "missing exec server: {self}"
        );
        assert!(
            self.script_contains(&format!("--socket '{sock}'")),
            "missing --socket flag: {self}"
        );
        assert!(
            self.script_contains(&format!("--log-dir '{logs}'")),
            "missing --log-dir flag: {self}"
        );
        assert!(
            self.script_contains("--timeout 2"),
            "missing --timeout flag: {self}"
        );
        assert!(
            self.script_contains("--log-level warning"),
            "missing --log-level flag: {self}"
        );
        assert!(
            self.has_arg("run") && self.has_arg("-d"),
            "missing run -d: {self}"
        );
        assert!(
            self.has_arg("--") && self.has_arg("alpine:latest"),
            "missing image separator: {self}"
        );
        assert!(
            self.has_arg("/nix/store/fake/bin/bash") && self.has_arg("-c"),
            "missing bash -c: {self}"
        );
    }

    /// Whole-shape regression: the launch argv matches element for
    /// element. Membership probes pass even on a duplicated verb
    /// (`run run -d …`); an exact sequence cannot.
    pub(crate) fn expects_exact_default_sequence(&self) {
        let root = self.root.to_string_lossy().into_owned();
        let cache = self.cache.to_string_lossy().into_owned();
        let logs = self.logs.to_string_lossy().into_owned();
        let sock_parent = self.socket_dir.to_string_lossy().into_owned();
        let sock = self.sock.to_string_lossy().into_owned();
        let expected = vec![
            "run".to_owned(),
            "-d".to_owned(),
            "--name".to_owned(),
            "ncap-test".to_owned(),
            "-v".to_owned(),
            "/nix:/nix:ro".to_owned(),
            "-v".to_owned(),
            format!("{sock_parent}:{sock_parent}"),
            "-v".to_owned(),
            format!("{root}:{root}"),
            "-w".to_owned(),
            root.clone(),
            "-v".to_owned(),
            format!("{cache}:{cache}:ro"),
            "-v".to_owned(),
            format!("{logs}:{logs}"),
            "--".to_owned(),
            "alpine:latest".to_owned(),
            "/nix/store/fake/bin/bash".to_owned(),
            "-c".to_owned(),
            format!(
                "source '{cache}/env' && exec '/nix/store/fake/bin/ncap-server' \
                 --socket '{sock}' --log-dir '{logs}' --timeout 2 --log-level warning"
            ),
        ];
        assert_eq!(&self.args, &expected, "launch argv mismatch: {self}");
    }

    pub(crate) fn expects_git_mount(&self) {
        let root = self.root.to_string_lossy().into_owned();
        let expected = format!("{root}/.git:{root}/.git:ro");
        assert!(self.has_mount(&expected), "missing .git ro mount: {self}");
    }

    pub(crate) fn expects_no_git(&self) {
        assert!(!self.has_text(".git"), "unexpected .git mount: {self}");
    }

    /// Harden posture: capability drop, no-new-privileges, read-only
    /// mounts for each present Watched file, skips for each absent one,
    /// and the more-specific watch mount ordered after the root mount.
    pub(crate) fn expects_harden_mounts(&self, present: &[&str], absent: &[&str]) {
        let root = self.root.to_string_lossy().into_owned();
        assert!(self.has_arg("--cap-drop=all"), "missing --cap-drop: {self}");
        assert!(
            self.has_arg("--security-opt=no-new-privileges"),
            "missing --security-opt: {self}"
        );
        let root_mount = format!("{root}:{root}");
        for name in present {
            let expected = format!("{root}/{name}:{root}/{name}:ro");
            assert!(
                self.has_mount(&expected),
                "missing ro watch mount `{name}`: {self}"
            );
            assert!(
                self.ordered_before(&root_mount, &expected),
                "watch mount `{name}` after root: {self}"
            );
        }
        for name in absent {
            let missing = format!("{root}/{name}:{root}/{name}:ro");
            assert!(
                !self.has_mount(&missing),
                "absent entry `{name}` must be skipped: {self}"
            );
        }
    }

    pub(crate) fn expects_no_harden(&self) {
        assert!(
            !self.has_text("--cap-drop"),
            "harden off must not emit cap-drop: {self}"
        );
        assert!(
            !self.has_text("no-new-privileges"),
            "harden off must not emit security-opt: {self}"
        );
    }

    pub(crate) fn expects_no_watch_mount(&self, name: &str) {
        let root = self.root.to_string_lossy().into_owned();
        let mount = format!("{root}/{name}:{root}/{name}:ro");
        assert!(
            !self.has_mount(&mount),
            "harden off must not mount watch file `{name}`: {self}"
        );
    }

    /// An extra runtime option survived as a single argv (no word
    /// splitting on embedded spaces).
    pub(crate) fn expects_opt(&self, opt: &str) {
        assert!(
            self.has_arg(opt),
            "expanded arg must be present without word splitting: {self}"
        );
    }

    /// Argv fragments of a split expansion must be absent.
    pub(crate) fn expects_no_opt(&self, opt: &str) {
        assert!(
            !self.has_arg(opt),
            "split fragment `{opt}` must be absent: {self}"
        );
    }

    pub(crate) fn expects_defaults_before(&self, opt: &str) {
        assert!(
            self.ordered_before("/nix:/nix:ro", opt),
            "defaults must come before extraOptions: {self}"
        );
    }

    pub(crate) fn expects_log_level(&self, level: &str) {
        assert!(
            self.script_contains(&format!("--log-level {level}")),
            "level `{level}` must survive as its own argv: {self}"
        );
    }

    fn has_arg(&self, arg: &str) -> bool {
        self.args.iter().any(|a| a == arg)
    }

    fn has_mount(&self, spec: &str) -> bool {
        self.mount_position(spec).is_some()
    }

    fn flag_value(&self, flag: &str) -> Option<&str> {
        self.args
            .iter()
            .position(|a| a == flag)
            .and_then(|i| self.args.get(i + 1).map(String::as_str))
    }

    fn ordered_before(&self, first: &str, second: &str) -> bool {
        match (self.mount_position(first), self.mount_position(second)) {
            (Some(a), Some(b)) => a < b,
            _ => false,
        }
    }

    fn script_contains(&self, frag: &str) -> bool {
        self.script_arg().is_some_and(|s| s.contains(frag))
    }

    fn has_text(&self, s: &str) -> bool {
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
pub(crate) enum Action {
    Stop,
    Rm,
    RmBeforeRun,
}

/// The Ctl world behind the seam: tempdir, cache, fake runtime/nix binaries,
/// liveness guard, and the `run_ctl` env. Tests declare the world via
/// [`Config`] and observe it via `evals`, `launches`, and `saw`.
pub(crate) struct Fixture {
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
    _live: Option<UnixListener>,
    _stop: Option<Arc<AtomicBool>>,
}

impl Fixture {
    /// The single constructor behind the seam: every test declares the
    /// Liveness, Freshness, Watched files, and Runtime adapter failure it
    /// needs. Env-only tweaks stay as `with_*` setters below.
    pub(crate) fn new(config: Config) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        assemble(tmp, config)
    }

    /// Named-root variant for Project root derivation tests.
    pub(crate) fn with_root_name(name: &str) -> Self {
        let mut fx = Self::new(Config::default());
        fx.set_root(name);
        fx
    }

    pub(crate) fn with_watch_files(mut self, json: &str) -> Self {
        self.env.insert("NCAP_WATCH_FILES".into(), json.into());
        self
    }

    pub(crate) fn with_harden(mut self, on: bool) -> Self {
        self.env.insert(
            "NCAP_HARDEN".into(),
            if on { "true".into() } else { "false".into() },
        );
        self
    }

    pub(crate) fn with_log_level(mut self, level: &str) -> Self {
        self.env.insert("NCAP_LOG_LEVEL".into(), level.into());
        self
    }

    pub(crate) fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub(crate) fn with_timeout(mut self, secs: &str) -> Self {
        self.env.insert("NCAP_TIMEOUT".into(), secs.to_string());
        self
    }

    pub(crate) fn without(mut self, key: &str) -> Self {
        self.env.remove(key);
        self
    }

    pub(crate) fn with_path_prepend(mut self, dir: &Path) -> Self {
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
    pub(crate) fn with_runtime_discoverable(self, name: &str) -> Self {
        let bin = self.tmp_path().join("discover");
        fs::create_dir_all(&bin).expect("discover dir");
        let runtime = self.tmp_path().join("fake-runtime");
        unix::fs::symlink(&runtime, bin.join(name)).expect("symlink fake runtime");
        self.with_path_prepend(&bin)
    }

    /// Whether the Socket sibling `name` (a file placed next to the
    /// Socket by a `clean` layout) still exists. Clean tests assert the
    /// sibling survives without learning the Socket parent path.
    pub(crate) fn socket_sibling_is_file(&self, name: &str) -> bool {
        self.sock.parent().is_some_and(|p| p.join(name).is_file())
    }

    /// `NCAP_SOCKET` currently in the env map (XDG fallback flow).
    pub(crate) fn socket_from_env(&self) -> PathBuf {
        PathBuf::from(self.env.get("NCAP_SOCKET").expect("NCAP_SOCKET in env"))
    }

    /// Bind the Liveness listener guard at the `NCAP_SOCKET` from the env
    /// map (after `apply_setup_env` in the XDG fallback flow), with the
    /// given responder behind it.
    pub(crate) fn hold_socket_from_env(&mut self, responder: Responder) {
        let sock = self.socket_from_env();
        let (listener, stop) = live_socket(&sock, responder);
        self._live = Some(listener);
        self._stop = stop;
    }

    pub(crate) fn project_has(&self, name: &str) -> bool {
        self.root.join(name).exists()
    }

    pub(crate) fn seed_dir(&self, name: &str) {
        fs::create_dir_all(self.root.join(name)).expect("seed dir");
    }

    pub(crate) fn stamp_content(&self) -> String {
        fs::read_to_string(self.cache.join("project")).expect("stamp")
    }

    pub(crate) fn cache_has(&self, name: &str) -> bool {
        self.cache.join(name).exists()
    }

    pub(crate) fn cache_is_dir(&self) -> bool {
        self.cache.is_dir()
    }

    pub(crate) fn logs_has(&self, name: &str) -> bool {
        self.logs.join(name).exists()
    }

    pub(crate) fn logs_is_dir(&self) -> bool {
        self.logs.is_dir()
    }

    pub(crate) fn socket_exists(&self) -> bool {
        self.sock.exists()
    }

    pub(crate) fn socket_parent_is_dir(&self) -> bool {
        self.sock.parent().is_some_and(|p| p.is_dir())
    }

    /// Flip the fake Runtime adapter's Running flag.
    pub(crate) fn set_running(&self, running: bool) {
        fs::write(
            self.state.join("running"),
            if running { "true" } else { "false" },
        )
        .expect("running");
    }

    /// Poison the cached hash so the Cache reads as stale.
    pub(crate) fn make_stale(&self) {
        fs::write(self.cache.join("hash"), "0000000000000000").expect("stale hash");
    }

    /// Remove the cached Env dump so the Cache reads as missing.
    pub(crate) fn make_missing_env(&self) {
        let _ = fs::remove_file(self.cache.join("env"));
    }

    /// Seed two server logs (old + newest) for the log-tail assertion.
    pub(crate) fn seed_server_logs(&self, old: &str, newest: &str) {
        fs::create_dir_all(&self.logs).expect("logs");
        fs::write(self.logs.join("ncap-server-100.log"), old).expect("log");
        fs::write(self.logs.join("ncap-server-999.log"), newest).expect("newest log");
    }

    /// Seed a `.git` dir inside the Project root (git-mount tests).
    pub(crate) fn seed_git(&self) {
        fs::create_dir_all(self.root.join(".git")).expect("git dir");
    }

    /// Full clean layout: Cache files + generation link + foreign cache
    /// file, server logs + foreign log, Socket file + sibling.
    pub(crate) fn seed_clean_full(&self) {
        fs::create_dir_all(&self.cache).expect("cache");
        fs::write(self.cache.join("env"), "export FOO=bar\n").expect("env");
        let digest = digest::compute(&self.root, &["flake.nix".to_owned()]).expect("digest");
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
    pub(crate) fn seed_clean_minimal(&self) {
        fs::create_dir_all(&self.cache).expect("cache");
        fs::write(self.cache.join("env"), "export FOO=bar\n").expect("env");
        let digest = digest::compute(&self.root, &["flake.nix".to_owned()]).expect("digest");
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
    pub(crate) fn set_root(&mut self, name: &str) -> PathBuf {
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
    pub(crate) fn apply_setup_env(&mut self) {
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

    pub(crate) fn not_live(mut self) -> Self {
        self.shutdown_responder();
        self._live = None;
        self
    }

    pub(crate) fn init(&self) -> Output {
        run_ctl(&self.env, &["init"])
    }

    pub(crate) fn start(&self) -> Output {
        run_ctl(&self.env, &["start"])
    }

    pub(crate) fn stop(&self) -> Output {
        run_ctl(&self.env, &["stop"])
    }

    pub(crate) fn status(&self) -> Output {
        run_ctl(&self.env, &["status"])
    }

    pub(crate) fn clean(&self) -> Output {
        run_ctl(&self.env, &["clean"])
    }

    pub(crate) fn restart(&self) -> Output {
        run_ctl(&self.env, &["restart"])
    }

    pub(crate) fn setup_env(&self) -> Output {
        run_ctl(&self.env, &["setup-env"])
    }

    /// Raw invocation without the absolute-path Runtime adapter shim,
    /// for the NCAP_RUNTIME rejection test.
    pub(crate) fn run_raw(&self, args: &[&str]) -> Output {
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
    pub(crate) fn evals(&self) -> usize {
        Self::read_log(&self.nix_log)
            .lines()
            .filter(|l| l.contains("print-dev-env"))
            .count()
    }

    /// Parsed view of the launch command: single-line invocation plus one
    /// argv per line plus the `run` invocation count. Carries the
    /// Project root, Cache, log dir, and Socket paths internally so the
    /// `expects_*` assertions can build mount specs without exposing
    /// path getters to tests.
    pub(crate) fn launches(&self) -> LaunchView {
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
        LaunchView {
            line,
            args,
            runs,
            root: self.root.clone(),
            cache: self.cache.clone(),
            logs: self.logs.clone(),
            sock: self.sock.clone(),
            socket_dir: self.sock.parent().expect("sock parent").to_path_buf(),
        }
    }

    /// Observable Runtime adapter actions: `Stop`, `Rm`, `RmBeforeRun`
    /// (pre-launch `rm` ran before the launch `run`).
    pub(crate) fn saw(&self, action: Action) -> bool {
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

    /// Written by the fake adapter.
    pub(crate) fn run_count_file(&self) -> String {
        Self::read_log(&self.state.join("run_count"))
            .trim()
            .to_owned()
    }

    pub(crate) fn runtime_log(&self) -> String {
        Self::read_log(&self.runtime_log)
    }

    pub(crate) fn clear_runtime_log(&self) {
        let _ = fs::write(&self.runtime_log, "");
        let _ = fs::write(&self.runtime_args_log, "");
    }

    /// Drop the Liveness guard and remove the Socket file stand-in.
    pub(crate) fn drop_live(&mut self) {
        self.shutdown_responder();
        self._live = None;
        let _ = fs::remove_file(&self.sock);
    }

    /// Stop the responder thread and release its cloned listener so the
    /// original guard's drop fully closes the Socket (not-live again).
    fn shutdown_responder(&mut self) {
        if let Some(stopping) = self._stop.take() {
            stopping.store(true, Ordering::SeqCst);
            // A dummy connect unblocks the accept so the thread can exit
            // and release its cloned fd.
            let _ = UnixStream::connect(&self.sock);
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub(crate) fn tmp_path(&self) -> PathBuf {
        self._tmp.path().to_path_buf()
    }
}

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

/// Bind a listener on `sock` so the socket half of the liveness predicate
/// (§ Liveness: Running AND connectable) holds, and (non-`Silent`) spawn
/// the wire-speaking responder thread behind it. The caller must hold the
/// return value until the ctl invocation completes — dropping it closes
/// the socket and the container counts as not live again.
fn live_socket(sock: &Path, responder: Responder) -> (UnixListener, Option<Arc<AtomicBool>>) {
    fs::create_dir_all(sock.parent().unwrap()).expect("sock dir");
    let listener = UnixListener::bind(sock).expect("bind socket");
    if matches!(responder, Responder::Silent) {
        return (listener, None);
    }
    let stopping = Arc::new(AtomicBool::new(false));
    let cloned = listener
        .try_clone()
        .expect("clone listener for responder thread");
    let stopping_for_thread = Arc::clone(&stopping);
    thread::spawn(move || responder_loop(cloned, responder, &stopping_for_thread));
    (listener, Some(stopping))
}

/// One connection at a time: read the 5-byte frame header (+ payload) and
/// answer the Version probe per the configured responder. Any other first
/// tag (a bare liveness connect) just closes. Read timeouts keep the loop
/// alive after a connect-nothing session (liveness polls); `stopping`
/// ends the loop between accepts.
fn responder_loop(listener: UnixListener, responder: Responder, stopping: &AtomicBool) {
    while !stopping.load(Ordering::SeqCst) {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        if stopping.load(Ordering::SeqCst) {
            // The shutdown dummy connection woke the accept: leave at once.
            return;
        }
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        if !answer_probe(&mut stream, responder.clone()) {
            // Read timed out or closed mid-frame: done with this connection.
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// Answer one Version probe connection; `true` when a well-formed request
/// frame arrived. The incoming `RequestVersion` frame carries no payload.
fn answer_probe(stream: &mut UnixStream, responder: Responder) -> bool {
    let mut head = [0u8; 5];
    if read_all(stream, &mut head).is_err() {
        return false;
    }
    let tag = head[0];
    let len = u32::from_be_bytes(head[1..5].try_into().expect("4 bytes"));
    let mut payload = vec![0u8; len as usize];
    if read_all(stream, &mut payload).is_err() {
        return false;
    }
    if tag != FrameType::RequestVersion as u8 {
        return true;
    }
    let (tag, payload) = match responder {
        Responder::Matching => (
            FrameType::ServerVersion as u8,
            serde_json::to_vec(&VersionMsg {
                version: nix_capsule::protocol::CURRENT_VERSION.to_owned(),
            })
            .expect("encode version"),
        ),
        Responder::Skew(version) => (
            FrameType::ServerVersion as u8,
            serde_json::to_vec(&VersionMsg { version })
                .expect("encode version"),
        ),
        Responder::Stale => (
            FrameType::Error as u8,
            serde_json::to_vec(&serde_json::json!({
                "message": "expected a `Request` or `RequestVersion` frame first, got an unknown tag"
            }))
            .expect("encode error"),
        ),
        Responder::Silent => {
            unreachable!("live_socket never spawns the thread for `Silent`")
        }
    };
    let mut frame = vec![tag];
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    stream.write_all(&frame).is_ok()
}

/// Read until `buf` is full or the (2 s) read timeout fires.
fn read_all(stream: &mut UnixStream, buf: &mut [u8]) -> Result<(), ()> {
    let mut filled = 0;
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(()),
        }
    }
    if filled == buf.len() { Ok(()) } else { Err(()) }
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

    let (_live, _stop) = if config.liveness.connectable {
        let (listener, stop) = live_socket(&sock, config.responder.clone());
        (Some(listener), stop)
    } else {
        (None, None)
    };

    // Seed the Cache through the real digest interface where fresh
    // (digest of the configured watch list, no hardcoded hash).
    match config.freshness {
        Freshness::Fresh => {
            let digest = digest::compute(&root, &config.watch).expect("digest");
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
        _stop,
    }
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

fn make_executable(path: &Path) {
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod");
}

fn write_stub(path: &Path, content: &str) {
    fs::write(path, content).expect("write stub");
    make_executable(path);
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
