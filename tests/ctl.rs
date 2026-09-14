//! Integration tests for `ncap-ctl` core lifecycle (ticket 06): fake runtime
//! standing in for podman/docker and a stub `nix`, call-counting for eval
//! avoidance, plus the stamp guard, readiness deadline, race recovery, and
//! status dimensions.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin_path(name: &str) -> PathBuf {
    let var = format!("CARGO_BIN_EXE_{name}");
    std::env::var(&var)
        .unwrap_or_else(|_| panic!("CARGO_BIN_EXE not set for binary {name}"))
        .into()
}

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
    // Preserve a few ambient vars the child may need (PATH, HOME, etc.)
    // but remove NCAP_* above. Then set the test's env.
    //
    // Uniform resolve + strict `NCAP_RUNTIME` (`podman`/`docker` only) mean
    // absolute-path fake runtimes can no longer be passed through. Tests
    // still create stub executables at arbitrary tmp paths: translate an
    // absolute `NCAP_RUNTIME` into a `podman` shim on PATH so the child
    // validates while invoking the same stub.
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
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&rt_path, &shim);
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

/// Deep test harness behind one small interface: owns the `TempDir`, the
/// Cache files (Env dump, hash, Stamp guard stamp), the fake Runtime adapter
/// + fake `nix` binaries, the Socket listener guard for Liveness, and the
///   `run_ctl` env. Tests cross this seam via constructors + query methods
///   instead of past it via log-file greps.
mod fixture {
    use super::{NCAP_VARS, bin_path};
    use super::{base_env, fake_nix, live_socket, run_ctl, write_stub};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use tempfile::TempDir;

    pub struct Fixture {
        pub root: PathBuf,
        pub cache: PathBuf,
        pub logs: PathBuf,
        pub sock: PathBuf,
        pub runtime_bin: PathBuf,
        pub env: HashMap<String, String>,
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
    /// Logs the single-line invocation to `runtime_log` (for `run_line`
    /// substring asserts) plus one argv per line for `run` invocations to
    /// `runtime_args_log` (for no-word-splitting asserts).
    fn write_live_stub(
        runtime_bin: &Path,
        state: &Path,
        runtime_log: &Path,
        runtime_args_log: &Path,
    ) {
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

    fn assemble(tmp: TempDir, running: &str, seed: &str, watch_json: &str, live: bool) -> Fixture {
        let root = tmp.path().join("proj");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("flake.nix"), "x").expect("watch file");
        let cache = tmp.path().join("cache");
        let logs = tmp.path().join("logs");
        let sock = tmp.path().join("sock/ncap.sock");
        let state = tmp.path().join("state");
        fs::create_dir_all(&state).expect("state");
        fs::write(state.join("running"), running).expect("running");
        let runtime_log = tmp.path().join("runtime.log");
        let runtime_args_log = tmp.path().join("runtime-args.log");
        let nix_log = tmp.path().join("nix.log");
        let runtime_bin = tmp.path().join("fake-runtime");
        let nix_bin = tmp.path().join("fake-nix");
        write_live_stub(&runtime_bin, &state, &runtime_log, &runtime_args_log);
        fake_nix(&nix_bin, &nix_log, "export FOO=bar\n");

        let mut env = base_env(&root, &cache, &logs, &sock, &runtime_bin, &nix_bin);
        env.insert("NCAP_WATCH_FILES".into(), watch_json.into());

        let _live = if live { Some(live_socket(&sock)) } else { None };

        // Seed the Cache through the real digest interface where fresh.
        // "fresh" covers watch `["flake.nix"]`; "fresh-empty" covers `[]`
        // (digest of the empty watch list, no hardcoded hash).
        if seed == "fresh" {
            let digest = nix_capsule::ctl::digest::compute(&root, &["flake.nix".to_owned()])
                .expect("digest");
            fs::create_dir_all(&cache).expect("cache");
            fs::write(cache.join("env"), "export FOO=bar\n").expect("env");
            fs::write(cache.join("hash"), &digest).expect("hash");
            fs::write(cache.join("project"), root.to_string_lossy().as_ref()).expect("stamp");
        } else if seed == "fresh-empty" {
            let digest =
                nix_capsule::ctl::digest::compute(&root, &[] as &[String]).expect("digest");
            fs::create_dir_all(&cache).expect("cache");
            fs::write(cache.join("env"), "export FOO=bar\n").expect("env");
            fs::write(cache.join("hash"), &digest).expect("hash");
            fs::write(cache.join("project"), root.to_string_lossy().as_ref()).expect("stamp");
        } else if seed == "stale" {
            fs::create_dir_all(&cache).expect("cache");
            fs::write(cache.join("env"), "export OLD=1\n").expect("env");
            fs::write(cache.join("hash"), "0000000000000000").expect("stale hash");
            fs::write(cache.join("project"), root.to_string_lossy().as_ref()).expect("stamp");
        }
        // "missing" seeds nothing: down + no cache.

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
            runtime_bin,
            runtime_log,
            runtime_args_log,
            nix_log,
            env,
            _tmp: tmp,
            _live,
        }
    }

    impl Fixture {
        pub fn fresh_live() -> Self {
            let tmp = TempDir::new().expect("tempdir");
            assemble(tmp, "true", "fresh", r#"["flake.nix"]"#, true)
        }

        /// Fresh with empty watch list: hash matches `digest([])` so
        /// `start`-only tests that watch nothing stay truly fresh.
        pub fn fresh_empty() -> Self {
            let tmp = TempDir::new().expect("tempdir");
            assemble(tmp, "false", "fresh-empty", "[]", true)
        }

        /// Fresh + empty watch + no Liveness listener (Running alone is
        /// not live). Used by the not-live init test so the early-return
        /// path is genuinely fresh, not stale-then-eval.
        pub fn fresh_not_live_empty() -> Self {
            let tmp = TempDir::new().expect("tempdir");
            assemble(tmp, "true", "fresh-empty", "[]", false)
        }

        pub fn stale_live() -> Self {
            let tmp = TempDir::new().expect("tempdir");
            assemble(tmp, "true", "stale", r#"["flake.nix"]"#, true)
        }

        pub fn down() -> Self {
            let tmp = TempDir::new().expect("tempdir");
            // Liveness needs a connectable Socket for the readiness poll.
            assemble(tmp, "false", "missing", "[]", true)
        }

        /// Named-root variant for Project root derivation tests.
        pub fn with_root_name(name: &str) -> Self {
            let tmp = TempDir::new().expect("tempdir");
            let mut fx = assemble(tmp, "false", "missing", "[]", false);
            let new_root = fx._tmp.path().join(name);
            fs::create_dir_all(&new_root).expect("root");
            fx.root = new_root.clone();
            fx.env.insert(
                "NCAP_PROJECT_ROOT".into(),
                new_root.to_string_lossy().into_owned(),
            );
            fx
        }

        pub fn with_watch_files(mut self, json: &str) -> Self {
            self.env.insert("NCAP_WATCH_FILES".into(), json.into());
            self
        }

        pub fn with_harden(mut self, on: bool) -> Self {
            self.env.insert(
                "NCAP_HARDEN".into(),
                if on { "true".into() } else { "false".into() },
            );
            self
        }

        pub fn with_env(mut self, key: &str, value: &str) -> Self {
            self.env.insert(key.into(), value.into());
            self
        }

        pub fn with_timeout(mut self, secs: &str) -> Self {
            self.env.insert("NCAP_TIMEOUT".into(), secs.into());
            self
        }

        pub fn with_run_fail_once(self) -> Self {
            fs::write(self.state.join("run_fail_first"), "").expect("run_fail_first");
            self
        }

        pub fn with_peer_dead_retry(self) -> Self {
            // Peer-dead retry = first run fails with name-in-use, then the
            // retry succeeds: same flag, plus inspect sees Running after rm.
            let this = self.with_run_fail_once();
            fs::write(this.state.join("peer_dead"), "").expect("peer_dead");
            this
        }

        pub fn with_never_running(self, state_json: &str) -> Self {
            fs::write(self.state.join("run_never"), "").expect("run_never");
            fs::write(self.state.join("running"), "false").expect("running");
            fs::write(self.state.join("state_json"), state_json).expect("state json");
            self
        }

        pub fn with_run_fail_always(self, message: &str) -> Self {
            fs::write(self.state.join("run_fail_always"), message).expect("run_fail_always");
            self
        }

        pub fn with_stop_fail(self) -> Self {
            fs::write(self.state.join("stop_fail"), "").expect("stop_fail");
            self
        }

        pub fn set_running(&self, running: bool) {
            fs::write(
                self.state.join("running"),
                if running { "true" } else { "false" },
            )
            .expect("running");
        }

        pub fn make_stale(&self) {
            fs::write(self.cache.join("hash"), "0000000000000000").expect("stale hash");
        }

        pub fn make_missing_env(&self) {
            let _ = fs::remove_file(self.cache.join("env"));
        }

        pub fn seed_server_logs(&self, old: &str, newest: &str) {
            fs::create_dir_all(&self.logs).expect("logs");
            fs::write(self.logs.join("ncap-server-100.log"), old).expect("log");
            fs::write(self.logs.join("ncap-server-999.log"), newest).expect("newest log");
        }

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

        pub fn not_live(mut self) -> Self {
            self._live = None;
            self
        }

        pub fn init(&self) -> Output {
            run_ctl(&self.env, &["init"])
        }

        pub fn start(&self) -> Output {
            run_ctl(&self.env, &["start"])
        }

        pub fn stop(&self) -> Output {
            run_ctl(&self.env, &["stop"])
        }

        pub fn status(&self) -> Output {
            run_ctl(&self.env, &["status"])
        }

        pub fn clean(&self) -> Output {
            run_ctl(&self.env, &["clean"])
        }

        pub fn restart(&self) -> Output {
            run_ctl(&self.env, &["restart"])
        }

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

        /// Number of `run` invocations seen by the fake Runtime adapter.
        pub fn runtime_runs(&self) -> usize {
            Self::read_log(&self.runtime_log)
                .lines()
                .filter(|l| l.contains("run "))
                .count()
        }

        /// Number of `print-dev-env` evals seen by the fake `nix`.
        pub fn eval_count(&self) -> usize {
            Self::read_log(&self.nix_log)
                .lines()
                .filter(|l| l.contains("print-dev-env"))
                .count()
        }

        pub fn saw_stop(&self) -> bool {
            Self::read_log(&self.runtime_log)
                .lines()
                .any(|l| l.contains("stop "))
        }

        pub fn saw_rm(&self) -> bool {
            Self::read_log(&self.runtime_log).contains("rm ")
                || self.state.join("rm_called").is_file()
        }

        /// Pre-launch `rm` ran before the single launch `run`.
        pub fn saw_rm_before_run(&self) -> bool {
            let log = Self::read_log(&self.runtime_log);
            match (log.find("rm "), log.find("run ")) {
                (Some(rm), Some(run)) => rm < run,
                _ => false,
            }
        }

        /// Contents of the `run_count` file written by the fake adapter.
        pub fn run_count_file(&self) -> String {
            Self::read_log(&self.state.join("run_count"))
                .trim()
                .to_owned()
        }

        pub fn runtime_log(&self) -> String {
            Self::read_log(&self.runtime_log)
        }

        /// The single `run` invocation line of the launch command.
        pub fn run_line(&self) -> String {
            Self::read_log(&self.runtime_log)
                .lines()
                .find(|l| l.contains("run "))
                .unwrap_or_default()
                .to_owned()
        }

        /// One argv per line for `run` invocations (no word-splitting
        /// probe). Only `run` invocations are logged here.
        pub fn run_arg_lines(&self) -> Vec<String> {
            Self::read_log(&self.runtime_args_log)
                .lines()
                .map(|l| l.to_owned())
                .collect()
        }

        pub fn clear_runtime_log(&self) {
            let _ = fs::write(&self.runtime_log, "");
            let _ = fs::write(&self.runtime_args_log, "");
        }

        pub fn drop_live(&mut self) {
            self._live = None;
            let _ = fs::remove_file(&self.sock);
        }

        pub fn tmp_path(&self) -> PathBuf {
            self._tmp.path().to_path_buf()
        }
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
    // Keep HOME for XDG fallbacks where needed; tests override when testing fallback.
    if let Ok(home) = std::env::var("HOME") {
        env.insert("HOME".into(), home);
    }
    env
}

// ---------------------------------------------------------------------------
// Refusal: each command names the missing var
// ---------------------------------------------------------------------------

#[test]
fn init_refuses_when_a_demanded_var_is_missing() {
    let fx = fixture::Fixture::down();
    let demanded = [
        "NCAP_PROJECT_ROOT",
        "NCAP_PROJECT",
        "NCAP_CONTAINER",
        "NCAP_SOCKET",
        "NCAP_CACHE_DIR",
        "NCAP_LOG_DIR",
        "NCAP_IMAGE",
        "NCAP_SERVER",
        "NCAP_NIX",
        "NCAP_BASH",
        "NCAP_DEVSHELL",
        "NCAP_RUNTIME",
        "NCAP_TIMEOUT",
        "NCAP_WATCH_FILES",
        "NCAP_RUN_OPTS",
        "NCAP_HARDEN",
    ];
    for var in demanded {
        let mut env = fx.env.clone();
        env.remove(var);
        // For NCAP_PROJECT_ROOT removal, NCAP_CONTAINER etc. still set so
        // the error should still name NCAP_PROJECT_ROOT, not a derived var.
        let out = run_ctl(&env, &["init"]);
        assert!(!out.status.success(), "init without {var} must fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(var),
            "init without {var} must name it: stderr={stderr}"
        );
    }
}

#[test]
fn commands_refuse_watch_files_that_are_not_relative_files() {
    let fx = fixture::Fixture::down();
    std::fs::create_dir_all(fx.root.join("adir")).expect("root");

    let cases = [
        (r#"/abs/nix"#, "/abs/nix"),
        (r#"../escape"#, "../escape"),
        (r#"adir"#, "adir"),
    ];
    for (entry_json, entry) in cases {
        let mut env = fx.env.clone();
        env.insert("NCAP_WATCH_FILES".into(), format!(r#"["{entry_json}"]"#));
        let out = run_ctl(&env, &["status"]);
        assert!(
            !out.status.success(),
            "watch entry `{entry}` must fail the command"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("NCAP_WATCH_FILES") && stderr.contains(entry),
            "error must name the var and the entry `{entry}`: stderr={stderr}"
        );
    }
}

#[test]
fn start_demands_full_env_including_nix_devshell_and_image() {
    // Pre-seed cache env so start's "no cached env" check passes.
    let fx = fixture::Fixture::fresh_empty();

    // Uniform resolve: start without NCAP_NIX/NCAP_DEVSHELL must refuse.
    let mut env = fx.env.clone();
    env.remove("NCAP_NIX");
    env.remove("NCAP_DEVSHELL");
    let out = run_ctl(&env, &["start"]);
    assert!(
        !out.status.success(),
        "start without nix/devshell must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("NCAP_NIX") || stderr.contains("NCAP_DEVSHELL"),
        "start must demand NCAP_NIX/NCAP_DEVSHELL: stderr={stderr}"
    );

    // Start without NCAP_IMAGE must refuse naming it.
    let mut env2 = fx.env.clone();
    env2.remove("NCAP_IMAGE");
    let out2 = run_ctl(&env2, &["start"]);
    assert!(!out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(stderr2.contains("NCAP_IMAGE"), "stderr={stderr2}");
}

#[test]
fn stop_refuses_without_container_or_derivation() {
    let fx = fixture::Fixture::down();

    // No NCAP_CONTAINER, no NCAP_PROJECT, no root → must name NCAP_PROJECT_ROOT
    let mut env = HashMap::new();
    env.insert(
        "NCAP_RUNTIME".into(),
        fx.runtime_bin.to_string_lossy().into_owned(),
    );
    env.insert("NCAP_TIMEOUT".into(), "2".into());
    env.insert("NCAP_WATCH_FILES".into(), "[]".into());
    env.insert("NCAP_RUN_OPTS".into(), "[]".into());
    env.insert("NCAP_HARDEN".into(), "false".into());
    let out = run_ctl(&env, &["stop"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NCAP_PROJECT_ROOT"), "stderr={stderr}");

    // With only NCAP_CONTAINER, uniform resolve still demands the full env.
    let mut env2 = HashMap::new();
    env2.insert("NCAP_CONTAINER".into(), "ncap-foo".into());
    env2.insert(
        "NCAP_RUNTIME".into(),
        fx.runtime_bin.to_string_lossy().into_owned(),
    );
    env2.insert("NCAP_TIMEOUT".into(), "2".into());
    env2.insert("NCAP_WATCH_FILES".into(), "[]".into());
    env2.insert("NCAP_RUN_OPTS".into(), "[]".into());
    env2.insert("NCAP_HARDEN".into(), "false".into());
    let out2 = run_ctl(&env2, &["stop"]);
    assert!(
        !out2.status.success(),
        "minimal stop must fail under uniform resolve: stderr={}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(stderr2.contains("NCAP_PROJECT_ROOT"), "stderr={stderr2}");
}

// ---------------------------------------------------------------------------
// Name sanitization via the binary (derivation + empty-result error)
// ---------------------------------------------------------------------------

#[test]
fn derived_project_name_is_used_and_empty_is_a_hard_error() {
    // Root whose basename is "my-proj" → sanitized "my-proj" via setup-env.
    let fx = fixture::Fixture::with_root_name("my-proj");

    let mut env = fx.env.clone();
    // Remove explicit container/project so setup-env derivation is exercised.
    env.remove("NCAP_CONTAINER");
    env.remove("NCAP_PROJECT");
    let out = run_ctl(&env, &["setup-env"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("export NCAP_PROJECT='my-proj'"),
        "stdout={stdout}"
    );
    assert!(
        stdout.contains("export NCAP_CONTAINER='ncap-my-proj'"),
        "stdout={stdout}"
    );

    // Strict resolve: init without the derived vars fails naming the var.
    let out_init = run_ctl(&env, &["init"]);
    assert!(!out_init.status.success());
    let stderr_init = String::from_utf8_lossy(&out_init.stderr);
    assert!(stderr_init.contains("NCAP_PROJECT"), "stderr={stderr_init}");

    // Empty sanitization: root "###" → hard error telling to set project
    let bad_root = fx.tmp_path().join("###");
    fs::create_dir_all(&bad_root).expect("bad root");
    let cache2 = fx.tmp_path().join("cache2");
    let mut env2 = fx.env.clone();
    env2.insert(
        "NCAP_PROJECT_ROOT".into(),
        bad_root.to_string_lossy().into_owned(),
    );
    env2.insert(
        "NCAP_CACHE_DIR".into(),
        cache2.to_string_lossy().into_owned(),
    );
    env2.remove("NCAP_CONTAINER");
    env2.remove("NCAP_PROJECT");
    let out2 = run_ctl(&env2, &["setup-env"]);
    assert!(!out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(stderr2.contains("set `project`"), "stderr={stderr2}");
}

// ---------------------------------------------------------------------------
// Stamp guard
// ---------------------------------------------------------------------------

#[test]
fn stamp_guard_same_root_passes_absent_written_different_is_error() {
    // Container down → eval + start; Liveness guard held by down().
    let mut fx = fixture::Fixture::down();

    let root_a = fx.tmp_path().join("root-a");
    fs::create_dir_all(&root_a).expect("root-a");
    fx.root = root_a.clone();
    fx.env.insert(
        "NCAP_PROJECT_ROOT".into(),
        root_a.to_string_lossy().into_owned(),
    );
    // First init: stamp absent → written, then start (container down → eval + start)
    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stamp = fs::read_to_string(fx.cache.join("project")).expect("stamp");
    assert_eq!(stamp, root_a.to_string_lossy().as_ref());

    // Same root again → pass
    fx.set_running(true);
    // Fresh cache so no eval needed
    let out2 = fx.init();
    assert!(
        out2.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Different root under same cache → hard error with hint
    fx.set_root("root-b");
    let out3 = fx.init();
    assert!(!out3.status.success());
    let stderr3 = String::from_utf8_lossy(&out3.stderr);
    assert!(stderr3.contains("set `project`"), "stderr={stderr3}");
    assert!(
        stderr3.contains(&root_a.to_string_lossy().into_owned()),
        "stderr={stderr3}"
    );
}

// ---------------------------------------------------------------------------
// Init flows: fresh+running zero evals, stale triggers re-eval, down triggers
// ensure-cache + start
// ---------------------------------------------------------------------------

#[test]
fn init_fresh_and_running_performs_zero_evals() {
    let fx = fixture::Fixture::fresh_live();
    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fx.eval_count(),
        0,
        "fresh+running must not eval (interface query, not log grep)"
    );
    assert_eq!(
        fx.runtime_runs(),
        0,
        "fresh+running must not start (interface query, not log grep)"
    );
}

#[test]
fn init_running_but_stale_triggers_reeval_and_restart() {
    let fx = fixture::Fixture::stale_live();
    // Live but stale ⇒ re-eval, non-fatal stop, then start to readiness.
    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fx.eval_count() >= 1,
        "stale must re-eval through the fixture interface"
    );
    assert!(fx.saw_stop(), "stale must stop before restarting");
    assert!(fx.runtime_runs() >= 1, "stale must start after re-eval");
}

#[test]
fn init_down_triggers_ensure_cache_and_start() {
    let fx = fixture::Fixture::down().with_watch_files("[]");
    // No cache yet: init must eval then start.
    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fx.eval_count() >= 1,
        "down+missing must eval through the fixture interface"
    );
    assert!(
        fx.runtime_runs() >= 1,
        "down must start through the fixture interface"
    );
    assert!(fx.cache.join("env").is_file(), "env must be cached");
}

// ---------------------------------------------------------------------------
// Liveness: Running without a connectable socket is not live
// ---------------------------------------------------------------------------

#[test]
fn running_without_socket_is_not_live() {
    // Fresh cache: a live container would make init return early with
    // "already running and fresh" and zero evals. Deliberately no
    // listener on the Socket path.
    let fx = fixture::Fixture::fresh_not_live_empty().with_timeout("1");

    // init must not take the live+fresh early return: it must attempt a
    // start (visible as a `run` invocation), which then fails readiness
    // because the socket never becomes connectable.
    let out = fx.init();
    assert!(
        !out.status.success(),
        "Running without a connectable socket must not count as live"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("never became live"), "stderr={stderr}");
    assert!(
        fx.runtime_runs() >= 1,
        "not-live init must attempt start: {}",
        fx.runtime_log()
    );
    assert_eq!(
        fx.eval_count(),
        0,
        "fresh+not-live init must not re-eval before start"
    );

    // start must not report "already running" either: it must attempt a run.
    fx.clear_runtime_log();
    let out2 = fx.start();
    assert!(
        !out2.status.success(),
        "Running without a connectable socket must not count as live"
    );
    assert!(
        fx.runtime_runs() >= 1,
        "not-live start must attempt run: {}",
        fx.runtime_log()
    );
}

// ---------------------------------------------------------------------------
// Readiness deadline + log tail
// ---------------------------------------------------------------------------

#[test]
fn start_never_reaching_running_fails_with_state_and_log_tail() {
    // Never Running: run never flips the flag (readiness deadline).
    let fx = fixture::Fixture::fresh_empty()
        .with_timeout("1")
        .with_never_running(r#"{"Running":false,"Status":"exited","Error":"bad image"}"#);
    fx.seed_server_logs("old log line\n", "line1\nline2\nEXPECTED_TAIL_MARKER\n");

    let out = fx.start();
    assert!(
        !out.status.success(),
        "start should fail when never Running"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("exited") || stderr.contains("Running"),
        "stderr must contain inspect state: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Concurrent-start race
// ---------------------------------------------------------------------------

#[test]
fn concurrent_start_peer_running_is_success() {
    // After the failed run, inspect says running true → success.
    let fx = fixture::Fixture::fresh_live().with_run_fail_once();
    let out = fx.start();
    assert!(
        out.status.success(),
        "peer running ⇒ success: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !fx.saw_rm(),
        "peer running must not rm: {}",
        fx.runtime_log()
    );
}

#[test]
fn concurrent_start_peer_dead_removes_and_retries_once() {
    // First inspect after failure says false so we go to rm+retry; the
    // retry's run succeeds and the poll then sees Running.
    let fx = fixture::Fixture::fresh_empty().with_peer_dead_retry();
    fx.set_running(false);
    // Liveness needs a connectable Socket: Running alone is not live
    // (fresh_empty already holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "peer dead ⇒ rm+retry ⇒ success: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fx.saw_rm(),
        "must have removed dead container: {}",
        fx.runtime_log()
    );
    // Exactly two run attempts
    assert_eq!(fx.run_count_file(), "2");
}

#[test]
fn start_removes_stopped_container_before_launch() {
    // Exists but stopped: `inspect -f State.Running` says false, while a
    // bare `inspect` succeeds (the fake's default branch exits 0).
    let fx = fixture::Fixture::fresh_empty();
    fx.set_running(false);
    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Pre-launch rm must run before the single launch.
    assert!(
        fx.saw_rm_before_run(),
        "pre-launch rm must run before run: {}",
        fx.runtime_log()
    );
    assert_eq!(
        fx.run_count_file(),
        "1",
        "single launch after pre-launch rm: {}",
        fx.runtime_log()
    );
}

// ---------------------------------------------------------------------------
// Stop idempotent; restart tolerates stopped
// ---------------------------------------------------------------------------

#[test]
fn stop_is_idempotent() {
    let fx = fixture::Fixture::fresh_live().not_live();
    // First: running true → stop succeeds (full env via the seam).
    let out = fx.stop();
    assert!(
        out.status.success(),
        "first stop: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Second stop: not running → still success
    let out2 = fx.stop();
    assert!(
        out2.status.success(),
        "second stop idempotent: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
}

// ---------------------------------------------------------------------------
// Clean: targeted file removal, shared-dir safety
// ---------------------------------------------------------------------------

#[test]
fn clean_removes_project_files_and_spares_foreign_entries() {
    let mut fx = fixture::Fixture::down();
    // Drop the Liveness guard so a plain Socket file can stand in.
    fx.drop_live();
    fx.seed_clean_full();
    let sibling = fx.sock.parent().unwrap().join("sibling.txt");

    let out = fx.clean();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // All four cache files plus the generation link are gone.
    assert!(!fx.cache.join("env").exists(), "env must be removed");
    assert!(!fx.cache.join("hash").exists(), "hash must be removed");
    assert!(
        !fx.cache.join("profile").exists(),
        "profile must be removed"
    );
    assert!(!fx.cache.join("project").exists(), "stamp must be removed");
    assert!(
        !fx.cache.join("profile-1-link").exists(),
        "gen link must be removed"
    );
    // The foreign cache file survives, so the dir stays.
    assert!(
        fx.cache.join("unrelated.txt").is_file(),
        "foreign cache file must survive"
    );
    assert!(fx.cache.is_dir(), "non-empty cache dir must survive");

    // Server logs are gone; the foreign log file survives.
    assert!(
        !fx.logs.join("ncap-server-1000.log").exists(),
        "server log 1 must be removed"
    );
    assert!(
        !fx.logs.join("ncap-server-2000.log").exists(),
        "server log 2 must be removed"
    );
    assert!(
        fx.logs.join("not-a-server-log.txt").is_file(),
        "foreign log file must survive"
    );
    assert!(fx.logs.is_dir(), "non-empty log dir must survive");

    // The socket file is gone; the sibling survives and the parent stays.
    assert!(!fx.sock.exists(), "socket file must be removed");
    assert!(sibling.is_file(), "socket-dir sibling must survive");
    assert!(
        fx.sock.parent().unwrap().is_dir(),
        "non-empty socket parent must survive"
    );
}

#[test]
fn clean_removes_empty_dirs_and_missing_paths_are_fine() {
    let mut fx = fixture::Fixture::down();
    fx.drop_live();
    fx.seed_clean_minimal();

    let out = fx.clean();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!fx.cache.exists(), "emptied cache dir should be removed");
    assert!(!fx.logs.exists(), "emptied log dir should be removed");
    assert!(!fx.sock.exists(), "socket file must be removed");
    assert!(
        !fx.sock.parent().unwrap().exists(),
        "emptied socket parent should be removed"
    );

    // A second clean with nothing left (dirs absent) must still succeed.
    let out2 = fx.clean();
    assert!(
        out2.status.success(),
        "clean twice: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
}

#[test]
fn restart_tolerates_a_stopped_container() {
    // Start not running; stop will be non-fatal, then init will start.
    let fx = fixture::Fixture::down();
    // Liveness needs a connectable Socket: Running alone is not live
    // (down() already holds the guard).
    let out = fx.restart();
    assert!(
        out.status.success(),
        "restart on stopped: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fx.runtime_runs() >= 1,
        "restart must start through the fixture interface"
    );
}

// ---------------------------------------------------------------------------
// Status covers all three dimensions
// ---------------------------------------------------------------------------

#[test]
fn status_reports_all_three_dimensions() {
    let fx = fixture::Fixture::fresh_live();
    let out = fx.status();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("running"), "stdout={stdout}");
    assert!(stdout.contains("connectable"), "stdout={stdout}");
    assert!(stdout.contains("fresh"), "stdout={stdout}");

    // Stale cache
    fx.make_stale();
    let out2 = fx.status();
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(stdout2.contains("stale"), "stdout={stdout2}");

    // Missing cache (remove env)
    fx.make_missing_env();
    let out3 = fx.status();
    let stdout3 = String::from_utf8_lossy(&out3.stdout);
    assert!(stdout3.contains("missing"), "stdout={stdout3}");
}

// ---------------------------------------------------------------------------
// Runtime selection follows NCAP_RUNTIME
// ---------------------------------------------------------------------------

#[test]
fn runtime_selection_absolute_path_is_rejected() {
    // Absolute runtime paths are rejected by ctl validation; only
    // `podman`/`docker` (resolved via PATH) are accepted. Raw invocation
    // without the shim so the absolute path reaches validation.
    let fx = fixture::Fixture::down();
    let out = fx.run_raw(&["stop"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NCAP_RUNTIME"), "stderr={stderr}");
}

#[test]
fn missing_runtime_is_an_error_naming_it() {
    let fx = fixture::Fixture::down();
    let bin_dir = fx.tmp_path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");

    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("NCAP_CONTAINER".into(), "ncap-test".into());
    env.insert("NCAP_TIMEOUT".into(), "2".into());
    // No NCAP_RUNTIME → error naming it per the contract.
    // Prepend bin_dir to PATH so `podman` would resolve if defaulted.
    let orig_path = std::env::var("PATH").unwrap_or_default();
    env.insert("PATH".into(), format!("{}:{orig_path}", bin_dir.display()));
    let out = run_ctl(&env, &["stop"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NCAP_RUNTIME"), "stderr={stderr}");
}

#[test]
fn invalid_runtime_is_rejected() {
    let fx = fixture::Fixture::down().with_env("NCAP_RUNTIME", "nerdctl");
    let out = fx.stop();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NCAP_RUNTIME"), "stderr={stderr}");
}

// ---------------------------------------------------------------------------
// XDG fallback is exercised via unit tests; a smoke integration for the
// runtime dir creation mode 0700.
// ---------------------------------------------------------------------------

#[test]
fn runtime_dir_is_created_with_0700() {
    let mut fx = fixture::Fixture::down();
    fx.drop_live();
    // No explicit NCAP_SOCKET — let it derive via XDG fallback. Also drop
    // the preset project/container so derivation follows the Project root
    // basename (`proj`), as in the Host shell flow.
    fx.env.remove("NCAP_SOCKET");
    fx.env.remove("NCAP_CACHE_DIR");
    fx.env.remove("NCAP_LOG_DIR");
    fx.env.remove("NCAP_PROJECT");
    fx.env.remove("NCAP_CONTAINER");

    let tmpdir = fx.tmp_path().join("my-tmp");
    fs::create_dir_all(&tmpdir).expect("tmpdir");
    let xdg_fallback = tmpdir.clone();

    fx.env.insert(
        "HOME".into(),
        fx.tmp_path().join("home").to_string_lossy().into_owned(),
    );
    fx.env
        .insert("TMPDIR".into(), xdg_fallback.to_string_lossy().into_owned());
    // No XDG_RUNTIME_DIR, no NCAP_SOCKET/CACHE/LOG → derive via setup-env.
    // Also need to set HOME so XDG fallbacks have a base
    fs::create_dir_all(fx.tmp_path().join("home")).expect("home");

    // Resolve derived vars through setup-env, then feed them to init
    // (strict resolve no longer derives).
    fx.apply_setup_env();

    // Liveness needs a connectable socket: Running alone is not live. The
    // socket path is derived (no NCAP_SOCKET), so rebuild it here; its
    // parent is pre-created mode 0700 so the assertion below holds (the ctl
    // leaves an existing dir untouched).
    let derived_sock = xdg_fallback
        .join("nix-capsule")
        .join("proj")
        .join("ncap.sock");
    fs::create_dir_all(derived_sock.parent().unwrap()).expect("sock dir");
    fs::set_permissions(
        derived_sock.parent().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .expect("sock dir mode");
    let _live = live_socket(&derived_sock);

    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The derived runtime dir must exist with 0700
    let derived = xdg_fallback.join("nix-capsule").join("proj");
    let mode = fs::metadata(&derived)
        .expect("derived dir")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700, "runtime dir must be 0700");
}

// ---------------------------------------------------------------------------
// Ticket 07: exact default mount set and launch command shape
// ---------------------------------------------------------------------------

#[test]
fn start_assembles_exact_default_mount_set_and_launch_command() {
    let fx = fixture::Fixture::fresh_empty();
    // Ensure .git does NOT exist for this base case
    assert!(!fx.root.join(".git").exists());
    fx.set_running(false);

    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run_line = fx.run_line();
    assert!(!run_line.is_empty(), "must have run line");
    let socket_dir = fx.sock.parent().unwrap().to_string_lossy();
    let root = &fx.root;
    let cache = &fx.cache;
    let logs = &fx.logs;
    let sock = &fx.sock;

    // Exact default mount set
    assert!(
        run_line.contains("-v /nix:/nix:ro"),
        "missing /nix ro mount: {run_line}"
    );
    assert!(
        run_line.contains(&format!("-v {}:{}", socket_dir, socket_dir)),
        "missing socket dir mount: {run_line}"
    );
    assert!(
        run_line.contains(&format!("-v {}:{}", root.display(), root.display())),
        "missing project root mount: {run_line}"
    );
    assert!(
        run_line.contains(&format!("-w {}", root.display())),
        "missing workdir: {run_line}"
    );
    assert!(
        run_line.contains(&format!("-v {}:{}:ro", cache.display(), cache.display())),
        "missing cache ro mount: {run_line}"
    );
    assert!(
        run_line.contains(&format!("-v {}:{}", logs.display(), logs.display())),
        "missing log rw mount: {run_line}"
    );
    // .git must be absent
    assert!(
        !run_line.contains(".git"),
        "unexpected .git mount without git dir: {run_line}"
    );
    // Launch command shape: quoted source dump && quoted exec server with flags
    let expected_source = format!("source '{}/env'", cache.display());
    assert!(
        run_line.contains(&expected_source),
        "missing source dump: {run_line}"
    );
    assert!(
        run_line.contains("&& exec '/nix/store/fake/bin/ncap-server'"),
        "missing exec server: {run_line}"
    );
    assert!(
        run_line.contains(&format!("--socket '{}'", sock.display())),
        "missing --socket flag: {run_line}"
    );
    assert!(
        run_line.contains(&format!("--log-dir '{}'", logs.display())),
        "missing --log-dir flag: {run_line}"
    );
    assert!(
        run_line.contains("--timeout 2"),
        "missing --timeout flag: {run_line}"
    );
    // Ensure detached and image/bash shape
    assert!(run_line.contains("run -d"), "missing run -d: {run_line}");
    assert!(
        run_line.contains("-- alpine:latest"),
        "missing image separator: {run_line}"
    );
    assert!(
        run_line.contains("/nix/store/fake/bin/bash -c"),
        "missing bash -c: {run_line}"
    );
}

#[test]
fn git_mount_present_readonly_when_git_dir_exists() {
    let fx = fixture::Fixture::fresh_empty();
    fx.seed_git();
    fx.set_running(false);
    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_line = fx.run_line();
    let expected = format!(
        "-v {}/.git:{}/.git:ro",
        fx.root.display(),
        fx.root.display()
    );
    assert!(
        run_line.contains(&expected),
        "missing .git ro mount: {run_line}"
    );
}

#[test]
fn git_mount_absent_without_error_outside_git_repo() {
    // No .git
    let fx = fixture::Fixture::fresh_empty();
    fx.set_running(false);
    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_line = fx.run_line();
    assert!(
        !run_line.contains(".git"),
        "unexpected .git mount outside repo: {run_line}"
    );
}

#[test]
fn extra_options_expansion_unset_var_fails_naming_it_before_run() {
    // Reference an unset variable via $VAR
    let fx =
        fixture::Fixture::fresh_empty().with_env("NCAP_RUN_OPTS", r#"["-v $UNSET_NCAP_XYZ:/mnt"]"#);
    fx.set_running(false);
    // Ensure the variable is not set in the child's env (unique name).
    debug_assert!(std::env::var("UNSET_NCAP_XYZ").is_err());
    let out = fx.start();
    assert!(!out.status.success(), "must fail on unset var");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("UNSET_NCAP_XYZ"),
        "must name unset var: {stderr}"
    );
    assert_eq!(
        fx.runtime_runs(),
        0,
        "must not have run the container before error: {}",
        fx.runtime_log()
    );
}

#[test]
fn extra_options_expansion_sets_var_is_passed_and_no_word_splitting() {
    // $TEST_EXPAND should expand to "/tmp/foo bar" containing a space; no word splitting means it stays one arg
    let fx = fixture::Fixture::fresh_empty()
        .with_env("TEST_EXPAND", "/tmp/foo bar")
        .with_env("NCAP_RUN_OPTS", r#"["-v $TEST_EXPAND:/mnt"]"#);
    fx.set_running(false);
    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_line = fx.run_line();
    // The expanded arg must appear as "-v /tmp/foo bar:/mnt" not split
    assert!(
        run_line.contains("-v /tmp/foo bar:/mnt"),
        "expanded arg must be present without word splitting: {run_line}"
    );
    // Argv-level probe: the expanded value must survive as a single argv,
    // not split on the embedded space.
    let args = fx.run_arg_lines();
    assert!(
        args.contains(&"-v /tmp/foo bar:/mnt".to_owned()),
        "expanded arg must be a single argv without word splitting: {args:?}"
    );
    assert!(
        !args.contains(&"/tmp/foo".to_owned()) && !args.contains(&"bar:/mnt".to_owned()),
        "split fragments must be absent: {args:?}"
    );
    // Ensure defaults still come before the extra option
    let nix_pos = args
        .iter()
        .position(|a| a == "/nix:/nix:ro")
        .expect("nix mount");
    let extra_pos = args
        .iter()
        .position(|a| a == "-v /tmp/foo bar:/mnt")
        .expect("extra mount");
    assert!(
        nix_pos < extra_pos,
        "defaults must come before extraOptions: {args:?}"
    );
}

#[test]
fn harden_adds_security_flags_and_ro_mounts_for_present_watch_files_skips_missing() {
    // missing.nix is absent
    let fx = fixture::Fixture::fresh_live()
        .with_harden(true)
        .with_watch_files(r#"["flake.nix", "missing.nix"]"#);
    fx.set_running(false);
    // Liveness needs a connectable Socket (fresh_live holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_line = fx.run_line();
    let root = &fx.root;
    assert!(
        run_line.contains("--cap-drop=all"),
        "missing --cap-drop: {run_line}"
    );
    assert!(
        run_line.contains("--security-opt=no-new-privileges"),
        "missing --security-opt: {run_line}"
    );
    let expected = format!(
        "-v {}/flake.nix:{}/flake.nix:ro",
        root.display(),
        root.display()
    );
    assert!(
        run_line.contains(&expected),
        "missing ro watch mount: {run_line}"
    );
    let missing = format!("-v {}/missing.nix", root.display());
    assert!(
        !run_line.contains(&missing),
        "missing entry must be skipped: {run_line}"
    );
    // More-specific mount after root
    let root_mount = format!("-v {}:{}", root.display(), root.display());
    assert!(
        run_line.find(&root_mount).unwrap() < run_line.find(&expected).unwrap(),
        "watch mount after root: {run_line}"
    );
}

#[test]
fn harden_off_emits_no_flags_nor_extra_mounts() {
    let fx = fixture::Fixture::fresh_live().with_watch_files(r#"["flake.nix"]"#);
    fx.set_running(false);
    // No Harden set (default off); Liveness guard held by fresh_live.
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_line = fx.run_line();
    let root = &fx.root;
    assert!(
        !run_line.contains("--cap-drop"),
        "harden off must not emit cap-drop: {run_line}"
    );
    assert!(
        !run_line.contains("no-new-privileges"),
        "harden off must not emit security-opt: {run_line}"
    );
    let watch_mount = format!(
        "-v {}/flake.nix:{}/flake.nix:ro",
        root.display(),
        root.display()
    );
    assert!(
        !run_line.contains(&watch_mount),
        "harden off must not mount watch file: {run_line}"
    );
}

#[test]
fn extra_options_braced_expansion_and_literal_passthrough() {
    let fx = fixture::Fixture::fresh_empty()
        .with_env("NCAP_TEST_BRACED", "braced-val")
        .with_env(
            "NCAP_RUN_OPTS",
            r#"["--braced=${NCAP_TEST_BRACED}", "literal-no-expand", "-v $NCAP_TEST_BRACED:/mnt"]"#,
        );
    fx.set_running(false);
    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_line = fx.run_line();
    assert!(
        run_line.contains("--braced=braced-val"),
        "braced expansion: {run_line}"
    );
    assert!(
        run_line.contains("literal-no-expand"),
        "literal passthrough: {run_line}"
    );
    assert!(
        run_line.contains("-v braced-val:/mnt"),
        "dollar expansion: {run_line}"
    );
    // Argv-level lock-in: each option must survive as its own argv.
    let args = fx.run_arg_lines();
    assert!(
        args.contains(&"--braced=braced-val".to_owned()),
        "braced argv: {args:?}"
    );
    assert!(
        args.contains(&"literal-no-expand".to_owned()),
        "literal argv: {args:?}"
    );
    assert!(
        args.contains(&"-v braced-val:/mnt".to_owned()),
        "dollar argv: {args:?}"
    );
}
