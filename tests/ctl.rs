//! Integration tests for `ncap-ctl` core lifecycle (ticket 06): fake runtime
//! standing in for podman/docker and a stub `nix`, call-counting for eval
//! avoidance, plus the stamp guard, readiness deadline, race recovery, and
//! status dimensions.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::fixture;
use common::probe::DRAIN_DEADLINE;

/// The drain deadline as the `--timeout` string the fixture passes through.
fn drain_timeout() -> String {
    DRAIN_DEADLINE.as_secs().to_string()
}

// ---------------------------------------------------------------------------
// Refusal: each command names the missing var
// ---------------------------------------------------------------------------

#[test]
fn init_refuses_when_a_demanded_var_is_missing() {
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
        "NCAP_LOG_LEVEL",
    ];
    for var in demanded {
        // For NCAP_PROJECT_ROOT removal, NCAP_CONTAINER etc. still set so
        // the error should still name NCAP_PROJECT_ROOT, not a derived var.
        let fx = fixture::Fixture::new(fixture::Config::default()).without(var);
        let out = fx.init();
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
    let cases = [
        (r#"/abs/nix"#, "/abs/nix"),
        (r#"../escape"#, "../escape"),
        (r#"adir"#, "adir"),
    ];
    for (entry_json, entry) in cases {
        let fx = fixture::Fixture::new(fixture::Config::default());
        fx.seed_dir("adir");
        let fx = fx.with_watch_files(&format!(r#"["{entry_json}"]"#));
        let out = fx.status();
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
    // Uniform resolve: start without NCAP_NIX/NCAP_DEVSHELL must refuse.
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty())
        .without("NCAP_NIX")
        .without("NCAP_DEVSHELL");
    let out = fx.start();
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
    let fx2 = fixture::Fixture::new(fixture::Config::fresh_empty()).without("NCAP_IMAGE");
    let out2 = fx2.start();
    assert!(!out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(stderr2.contains("NCAP_IMAGE"), "stderr={stderr2}");
}

#[test]
fn stop_refuses_without_container_or_derivation() {
    // No NCAP_CONTAINER, no NCAP_PROJECT, no root → must name NCAP_PROJECT_ROOT
    let fx = fixture::Fixture::new(fixture::Config::default())
        .without("NCAP_PROJECT_ROOT")
        .without("NCAP_PROJECT")
        .without("NCAP_CONTAINER")
        .without("NCAP_SOCKET")
        .without("NCAP_CACHE_DIR")
        .without("NCAP_LOG_DIR")
        .without("NCAP_IMAGE")
        .without("NCAP_SERVER")
        .without("NCAP_NIX")
        .without("NCAP_BASH")
        .without("NCAP_DEVSHELL");
    let out = fx.stop();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NCAP_PROJECT_ROOT"), "stderr={stderr}");

    // With only NCAP_CONTAINER, uniform resolve still demands the full env.
    let fx2 = fixture::Fixture::new(fixture::Config::default())
        .without("NCAP_PROJECT_ROOT")
        .without("NCAP_PROJECT")
        .without("NCAP_SOCKET")
        .without("NCAP_CACHE_DIR")
        .without("NCAP_LOG_DIR")
        .without("NCAP_IMAGE")
        .without("NCAP_SERVER")
        .without("NCAP_NIX")
        .without("NCAP_BASH")
        .without("NCAP_DEVSHELL")
        .with_env("NCAP_CONTAINER", "ncap-foo");
    let out2 = fx2.stop();
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
    // Remove explicit container/project so setup-env derivation is exercised.
    let fx = fixture::Fixture::with_root_name("my-proj")
        .without("NCAP_CONTAINER")
        .without("NCAP_PROJECT");

    let out = fx.setup_env();
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
    let out_init = fx.init();
    assert!(!out_init.status.success());
    let stderr_init = String::from_utf8_lossy(&out_init.stderr);
    assert!(stderr_init.contains("NCAP_PROJECT"), "stderr={stderr_init}");

    // Empty sanitization: root "###" → hard error telling to set project
    let bad_root = fx.tmp_path().join("###");
    fs::create_dir_all(&bad_root).expect("bad root");
    let cache2 = fx.tmp_path().join("cache2");
    let fx2 = fixture::Fixture::new(fixture::Config::default())
        .with_env("NCAP_PROJECT_ROOT", &bad_root.to_string_lossy())
        .with_env("NCAP_CACHE_DIR", &cache2.to_string_lossy())
        .without("NCAP_CONTAINER")
        .without("NCAP_PROJECT");
    let out2 = fx2.setup_env();
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
    let mut fx = fixture::Fixture::new(fixture::Config::default());

    let root_a = fx.set_root("root-a");
    // First init: stamp absent → written, then start (container down → eval + start)
    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fx.stamp_content(), root_a.to_string_lossy().as_ref());

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
    let fx = fixture::Fixture::new(fixture::Config::fresh_live());
    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fx.evals(),
        0,
        "fresh+running must not eval (interface query, not log grep)"
    );
    assert_eq!(
        fx.launches().runs(),
        0,
        "fresh+running must not start (interface query, not log grep)"
    );
}

#[test]
fn init_running_but_stale_triggers_reeval_and_restart() {
    let fx = fixture::Fixture::new(fixture::Config {
        liveness: fixture::Liveness::live(),
        freshness: fixture::Freshness::Stale,
        watch: vec!["flake.nix".to_owned()],
        failure: None,
    });
    // Live but stale ⇒ re-eval, non-fatal stop, then start to readiness.
    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fx.evals() >= 1,
        "stale must re-eval through the fixture interface"
    );
    assert!(
        fx.saw(fixture::Action::Stop),
        "stale must stop before restarting"
    );
    assert!(fx.launches().runs() >= 1, "stale must start after re-eval");
}

#[test]
fn init_down_triggers_ensure_cache_and_start() {
    let fx = fixture::Fixture::new(fixture::Config::default()).with_watch_files("[]");
    // No cache yet: init must eval then start.
    let out = fx.init();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fx.evals() >= 1,
        "down+missing must eval through the fixture interface"
    );
    assert!(
        fx.launches().runs() >= 1,
        "down must start through the fixture interface"
    );
    assert!(fx.cache_has("env"), "env must be cached");
}

// ---------------------------------------------------------------------------
// Liveness: Running without a connectable socket is not live
// ---------------------------------------------------------------------------

#[test]
fn running_without_socket_is_not_live() {
    // Fresh cache: a live container would make init return early with
    // "already running and fresh" and zero evals. Deliberately no
    // listener on the Socket path.
    let fx = fixture::Fixture::new(fixture::Config {
        liveness: fixture::Liveness {
            running: true,
            connectable: false,
        },
        freshness: fixture::Freshness::Fresh,
        watch: Vec::new(),
        failure: None,
    })
    .with_timeout(&drain_timeout());

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
        fx.launches().runs() >= 1,
        "not-live init must attempt start: {}",
        fx.runtime_log()
    );
    assert_eq!(
        fx.evals(),
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
        fx.launches().runs() >= 1,
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
    let fx = fixture::Fixture::new(fixture::Config {
        failure: Some(fixture::Failure::NeverRunning(
            r#"{"Running":false,"Status":"exited","Error":"bad image"}"#.to_owned(),
        )),
        ..fixture::Config::fresh_empty()
    })
    .with_timeout(&drain_timeout());
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
    let fx = fixture::Fixture::new(fixture::Config {
        failure: Some(fixture::Failure::RunFailOnce),
        ..fixture::Config::fresh_live()
    });
    let out = fx.start();
    assert!(
        out.status.success(),
        "peer running ⇒ success: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !fx.saw(fixture::Action::Rm),
        "peer running must not rm: {}",
        fx.runtime_log()
    );
}

#[test]
fn concurrent_start_peer_dead_removes_and_retries_once() {
    // First inspect after failure says false so we go to rm+retry; the
    // retry's run succeeds and the poll then sees Running.
    let fx = fixture::Fixture::new(fixture::Config {
        failure: Some(fixture::Failure::PeerDead),
        ..fixture::Config::fresh_empty()
    });
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
        fx.saw(fixture::Action::Rm),
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
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty());
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
        fx.saw(fixture::Action::RmBeforeRun),
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
    let fx = fixture::Fixture::new(fixture::Config::fresh_live()).not_live();
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
    let mut fx = fixture::Fixture::new(fixture::Config::default());
    // Drop the Liveness guard so a plain Socket file can stand in.
    fx.drop_live();
    fx.seed_clean_full();
    let sibling = fx.socket_parent_dir().join("sibling.txt");

    let out = fx.clean();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // All four cache files plus the generation link are gone.
    assert!(!fx.cache_has("env"), "env must be removed");
    assert!(!fx.cache_has("hash"), "hash must be removed");
    assert!(!fx.cache_has("profile"), "profile must be removed");
    assert!(!fx.cache_has("project"), "stamp must be removed");
    assert!(!fx.cache_has("profile-1-link"), "gen link must be removed");
    // The foreign cache file survives, so the dir stays.
    assert!(
        fx.cache_has("unrelated.txt"),
        "foreign cache file must survive"
    );
    assert!(fx.cache_is_dir(), "non-empty cache dir must survive");

    // Server logs are gone; the foreign log file survives.
    assert!(
        !fx.logs_has("ncap-server-1000.log"),
        "server log 1 must be removed"
    );
    assert!(
        !fx.logs_has("ncap-server-2000.log"),
        "server log 2 must be removed"
    );
    assert!(
        fx.logs_has("not-a-server-log.txt"),
        "foreign log file must survive"
    );
    assert!(fx.logs_is_dir(), "non-empty log dir must survive");

    // The socket file is gone; the sibling survives and the parent stays.
    assert!(!fx.socket_exists(), "socket file must be removed");
    assert!(sibling.is_file(), "socket-dir sibling must survive");
    assert!(
        fx.socket_parent_is_dir(),
        "non-empty socket parent must survive"
    );
}

#[test]
fn clean_removes_empty_dirs_and_missing_paths_are_fine() {
    let mut fx = fixture::Fixture::new(fixture::Config::default());
    fx.drop_live();
    fx.seed_clean_minimal();

    let out = fx.clean();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!fx.cache_is_dir(), "emptied cache dir should be removed");
    assert!(!fx.logs_is_dir(), "emptied log dir should be removed");
    assert!(!fx.socket_exists(), "socket file must be removed");
    assert!(
        !fx.socket_parent_is_dir(),
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
    let fx = fixture::Fixture::new(fixture::Config::default());
    // Liveness needs a connectable Socket: Running alone is not live
    // (down() already holds the guard).
    let out = fx.restart();
    assert!(
        out.status.success(),
        "restart on stopped: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fx.launches().runs() >= 1,
        "restart must start through the fixture interface"
    );
}

// ---------------------------------------------------------------------------
// Status covers all three dimensions
// ---------------------------------------------------------------------------

#[test]
fn status_reports_all_three_dimensions() {
    let fx = fixture::Fixture::new(fixture::Config::fresh_live());
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
    let fx = fixture::Fixture::new(fixture::Config::default());
    let out = fx.run_raw(&["stop"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NCAP_RUNTIME"), "stderr={stderr}");
}

#[test]
fn missing_runtime_is_an_error_naming_it() {
    let fx = fixture::Fixture::new(fixture::Config::default()).without("NCAP_RUNTIME");
    let bin_dir = fx.tmp_path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");

    // No NCAP_RUNTIME → error naming it per the contract.
    // Prepend bin_dir to PATH so `podman` would resolve if defaulted.
    let fx = fx.with_path_prepend(&bin_dir);
    let out = fx.stop();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NCAP_RUNTIME"), "stderr={stderr}");
}

#[test]
fn invalid_runtime_is_rejected() {
    let fx = fixture::Fixture::new(fixture::Config::default()).with_env("NCAP_RUNTIME", "nerdctl");
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
    let mut fx = fixture::Fixture::new(fixture::Config::default());
    fx.drop_live();
    // No explicit NCAP_SOCKET — let it derive via XDG fallback. Also drop
    // the preset project/container so derivation follows the Project root
    // basename (`proj`), as in the Host shell flow.
    let tmpdir = fx.tmp_path().join("my-tmp");
    fs::create_dir_all(&tmpdir).expect("tmpdir");
    let xdg_fallback = tmpdir.clone();
    let home = fx.tmp_path().join("home");

    let mut fx = fx
        .without("NCAP_SOCKET")
        .without("NCAP_CACHE_DIR")
        .without("NCAP_LOG_DIR")
        .without("NCAP_PROJECT")
        .without("NCAP_CONTAINER")
        .with_env("HOME", &home.to_string_lossy())
        .with_env("TMPDIR", &xdg_fallback.to_string_lossy());
    // No XDG_RUNTIME_DIR, no NCAP_SOCKET/CACHE/LOG → derive via setup-env.
    // Also need to set HOME so XDG fallbacks have a base
    fs::create_dir_all(fx.tmp_path().join("home")).expect("home");

    // Resolve derived vars through setup-env, then feed them to init
    // (strict resolve no longer derives).
    fx.apply_setup_env();

    // Liveness needs a connectable socket: Running alone is not live. The
    // socket path is derived (no NCAP_SOCKET); its parent is pre-created
    // mode 0700 so the assertion below holds (the ctl leaves an existing
    // dir untouched).
    let derived_sock = fx.socket_from_env();
    fs::create_dir_all(derived_sock.parent().unwrap()).expect("sock dir");
    fs::set_permissions(
        derived_sock.parent().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .expect("sock dir mode");
    fx.hold_socket_from_env();

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
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty());
    // Ensure .git does NOT exist for this base case
    assert!(!fx.project_has(".git"));
    fx.set_running(false);

    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let launch = fx.launches();
    assert!(!launch.is_empty(), "must have run line");
    let socket_dir = fx.socket_parent_dir().to_string_lossy().into_owned();
    let root = fx.project_root().to_path_buf();
    let cache = fx.cache_dir().to_path_buf();
    let logs = fx.log_dir().to_path_buf();
    let sock = fx.socket_path().to_path_buf();

    // Exact default mount set, asserted against the argv vector so shell
    // rendering can't break them.
    assert!(
        launch.has_mount("/nix:/nix:ro"),
        "missing /nix ro mount: {launch}"
    );
    assert!(
        launch.has_mount(&format!("{}:{}", socket_dir, socket_dir)),
        "missing socket dir mount: {launch}"
    );
    assert!(
        launch.has_mount(&format!("{}:{}", root.display(), root.display())),
        "missing project root mount: {launch}"
    );
    assert_eq!(
        launch.flag_value("-w"),
        Some(root.to_string_lossy().as_ref()),
        "missing workdir: {launch}"
    );
    assert!(
        launch.has_mount(&format!("{}:{}:ro", cache.display(), cache.display())),
        "missing cache ro mount: {launch}"
    );
    assert!(
        launch.has_mount(&format!("{}:{}", logs.display(), logs.display())),
        "missing log rw mount: {launch}"
    );
    // .git must be absent
    assert!(
        !launch.has_text(".git"),
        "unexpected .git mount without git dir: {launch}"
    );
    // Launch command shape: quoted source dump && quoted exec server with flags
    assert!(
        launch.script_contains(&format!("source '{}/env'", cache.display())),
        "missing source dump: {launch}"
    );
    assert!(
        launch.script_contains("&& exec '/nix/store/fake/bin/ncap-server'"),
        "missing exec server: {launch}"
    );
    assert!(
        launch.script_contains(&format!("--socket '{}'", sock.display())),
        "missing --socket flag: {launch}"
    );
    assert!(
        launch.script_contains(&format!("--log-dir '{}'", logs.display())),
        "missing --log-dir flag: {launch}"
    );
    assert!(
        launch.script_contains("--timeout 2"),
        "missing --timeout flag: {launch}"
    );
    assert!(
        launch.script_contains("--log-level warning"),
        "missing --log-level flag: {launch}"
    );
    // Ensure detached and image/bash shape
    assert!(
        launch.has_arg("run") && launch.has_arg("-d"),
        "missing run -d: {launch}"
    );
    assert!(
        launch.has_arg("--") && launch.has_arg("alpine:latest"),
        "missing image separator: {launch}"
    );
    assert!(
        launch.has_arg("/nix/store/fake/bin/bash") && launch.has_arg("-c"),
        "missing bash -c: {launch}"
    );
}

#[test]
fn git_mount_present_readonly_when_git_dir_exists() {
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty());
    fx.seed_git();
    fx.set_running(false);
    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let launch = fx.launches();
    let root = fx.project_root().to_path_buf();
    let expected = format!("{}/.git:{}/.git:ro", root.display(), root.display());
    assert!(
        launch.has_mount(&expected),
        "missing .git ro mount: {launch}"
    );
}

#[test]
fn git_mount_absent_without_error_outside_git_repo() {
    // No .git
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty());
    fx.set_running(false);
    // Liveness needs a connectable Socket (fresh_empty holds the guard).
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let launch = fx.launches();
    assert!(
        !launch.has_text(".git"),
        "unexpected .git mount outside repo: {launch}"
    );
}

#[test]
fn extra_options_expansion_unset_var_fails_naming_it_before_run() {
    // Reference an unset variable via $VAR
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty())
        .with_env("NCAP_RUN_OPTS", r#"["-v $UNSET_NCAP_XYZ:/mnt"]"#);
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
        fx.launches().runs(),
        0,
        "must not have run the container before error: {}",
        fx.runtime_log()
    );
}

#[test]
fn extra_options_expansion_sets_var_is_passed_and_no_word_splitting() {
    // $TEST_EXPAND should expand to "/tmp/foo bar" containing a space; no word splitting means it stays one arg
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty())
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
    let launch = fx.launches();
    // The expanded arg must survive as a single argv, not split on the
    // embedded space.
    assert!(
        launch.has_arg("-v /tmp/foo bar:/mnt"),
        "expanded arg must be present without word splitting: {launch}"
    );
    assert!(
        !launch.has_arg("/tmp/foo") && !launch.has_arg("bar:/mnt"),
        "split fragments must be absent: {launch}"
    );
    // Ensure defaults still come before the extra option
    assert!(
        launch.ordered_before("/nix:/nix:ro", "-v /tmp/foo bar:/mnt"),
        "defaults must come before extraOptions: {launch}"
    );
}

#[test]
fn harden_adds_security_flags_and_ro_mounts_for_present_watch_files_skips_missing() {
    // missing.nix is absent
    let fx = fixture::Fixture::new(fixture::Config::fresh_live())
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
    let launch = fx.launches();
    let root = fx.project_root().to_path_buf();
    assert!(
        launch.has_arg("--cap-drop=all"),
        "missing --cap-drop: {launch}"
    );
    assert!(
        launch.has_arg("--security-opt=no-new-privileges"),
        "missing --security-opt: {launch}"
    );
    let expected = format!(
        "{}/flake.nix:{}/flake.nix:ro",
        root.display(),
        root.display()
    );
    assert!(
        launch.has_mount(&expected),
        "missing ro watch mount: {launch}"
    );
    let missing = format!(
        "{}/missing.nix:{}/missing.nix:ro",
        root.display(),
        root.display()
    );
    assert!(
        !launch.has_mount(&missing),
        "missing entry must be skipped: {launch}"
    );
    // More-specific mount after root
    let root_mount = format!("{}:{}", root.display(), root.display());
    assert!(
        launch.ordered_before(&root_mount, &expected),
        "watch mount after root: {launch}"
    );
}

#[test]
fn harden_off_emits_no_flags_nor_extra_mounts() {
    let fx =
        fixture::Fixture::new(fixture::Config::fresh_live()).with_watch_files(r#"["flake.nix"]"#);
    fx.set_running(false);
    // No Harden set (default off); Liveness guard held by fresh_live.
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let launch = fx.launches();
    let root = fx.project_root().to_path_buf();
    assert!(
        !launch.has_text("--cap-drop"),
        "harden off must not emit cap-drop: {launch}"
    );
    assert!(
        !launch.has_text("no-new-privileges"),
        "harden off must not emit security-opt: {launch}"
    );
    let watch_mount = format!(
        "{}/flake.nix:{}/flake.nix:ro",
        root.display(),
        root.display()
    );
    assert!(
        !launch.has_mount(&watch_mount),
        "harden off must not mount watch file: {launch}"
    );
}

#[test]
fn extra_options_braced_expansion_and_literal_passthrough() {
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty())
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
    let launch = fx.launches();
    // Argv-level lock-in: each option must survive as its own argv.
    assert!(
        launch.has_arg("--braced=braced-val"),
        "braced expansion: {launch}"
    );
    assert!(
        launch.has_arg("literal-no-expand"),
        "literal passthrough: {launch}"
    );
    assert!(
        launch.has_arg("-v braced-val:/mnt"),
        "dollar expansion: {launch}"
    );
}

// ---------------------------------------------------------------------------
// Ticket log-level: env-contract validation and launch argv lock-in
// ---------------------------------------------------------------------------

#[test]
fn off_vocabulary_log_level_is_rejected_naming_the_var_and_the_four_values() {
    for bad in ["verbose", "Warning", "warn", "debug "] {
        let fx = fixture::Fixture::new(fixture::Config::fresh_empty()).with_log_level(bad);
        let out = fx.status();
        assert!(
            !out.status.success(),
            "log level `{bad}` must fail the command"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("NCAP_LOG_LEVEL"),
            "error must name the var: stderr={stderr}"
        );
        for level in ["debug", "info", "warning", "error"] {
            assert!(
                stderr.contains(level),
                "error must spell out `{level}`: stderr={stderr}"
            );
        }
    }
}

#[test]
fn each_accepted_log_level_survives_as_its_own_launch_argv() {
    for level in ["debug", "info", "warning", "error"] {
        let fx = fixture::Fixture::new(fixture::Config::fresh_empty()).with_log_level(level);
        fx.set_running(false);
        // Liveness needs a connectable Socket (fresh_empty holds the guard).
        let out = fx.start();
        assert!(
            out.status.success(),
            "level `{level}`: stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let launch = fx.launches();
        assert!(
            launch.script_contains(&format!("--log-level {level}")),
            "level `{level}` must survive as its own argv: {launch}"
        );
    }
}

#[test]
fn full_env_start_flow_carries_the_log_level() {
    // End-to-end start flow with a full env set: the validated level reaches
    // the server launch command.
    let fx = fixture::Fixture::new(fixture::Config::fresh_empty()).with_log_level("error");
    fx.set_running(false);
    let out = fx.start();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fx.launches().script_contains("--log-level error"),
        "full env must carry the level: {}",
        fx.launches()
    );
}
