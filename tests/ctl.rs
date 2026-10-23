use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use tempfile::TempDir;
use test_case::test_case;

use nix_capsule::ctl::digest::{Freshness, check, compute, store};
use nix_capsule::protocol::CURRENT_VERSION;

#[test]
fn setup_env_prints_five_export_lines_in_fixed_order() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.deriving_command(&["setup-env"]).output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let base = w.temp.path().display().to_string();
    assert_eq!(
        text(&out.stdout),
        format!(
            "export NCAP_PROJECT='proj'\nexport NCAP_CONTAINER='ncap-proj'\nexport NCAP_SOCKET='{base}/nix-capsule/proj/ncap.sock'\nexport NCAP_CACHE_DIR='{base}/.cache/nix-capsule/proj'\nexport NCAP_LOG_DIR='{base}/.local/state/nix-capsule/proj/logs'\n"
        )
    );
    Ok(())
}

#[test_case("my_project", "my-project" ; "underscore_joins_the_run")]
#[test_case("Foo Bar", "Foo-Bar" ; "case_is_preserved")]
#[test_case("a--b", "a-b" ; "a_run_collapses_to_one_separator")]
#[test_case("_x_", "x" ; "leading_and_trailing_separators_are_stripped")]
#[test_case("--foo--", "foo" ; "no_leading_separator_is_emitted")]
#[test_case("9lives", "9lives" ; "digits_start_an_alphanumeric_run")]
fn setup_env_sanitizes_the_project_name(basename: &str, project: &str) -> anyhow::Result<()> {
    let w = rooted_at(basename)?;
    let out = w.deriving_command(&["setup-env"]).output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).lines().next().unwrap(),
        format!("export NCAP_PROJECT='{project}'")
    );
    Ok(())
}

#[test]
fn setup_env_rejects_a_project_name_that_sanitizes_to_empty() -> anyhow::Result<()> {
    let w = rooted_at("---")?;
    let out = w.deriving_command(&["setup-env"]).output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains(&w.root.display().to_string()), "got {err}");
    assert!(err.contains("project"), "advice names `project`, got {err}");
    Ok(())
}

#[test]
fn setup_env_needs_only_the_project_root() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    for var in [
        "NCAP_IMAGE",
        "NCAP_RUNTIME",
        "NCAP_DEVSHELL",
        "NCAP_NIX",
        "NCAP_SERVER",
        "NCAP_BASH",
        "NCAP_TIMEOUT",
        "NCAP_WATCH_FILES",
        "NCAP_RUN_OPTS",
        "NCAP_HARDEN",
        "NCAP_LOG_LEVEL",
    ] {
        cmd.env_remove(var);
    }
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("export NCAP_PROJECT='proj'\n"));
    Ok(())
}

#[test]
fn setup_env_never_starts_a_container_or_touches_the_cache() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&["setup-env"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(!w.cache.exists(), "the Cache must stay untouched");
    assert!(w.rt_calls().is_empty(), "no Runtime adapter call");
    Ok(())
}

#[test]
fn setup_env_lets_an_explicit_project_win() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    cmd.env("NCAP_PROJECT", "chosen");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).lines().next().unwrap(),
        "export NCAP_PROJECT='chosen'"
    );
    Ok(())
}

#[test]
fn setup_env_escapes_single_quotes_in_export_values() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    cmd.env("NCAP_PROJECT", "it's");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).lines().next().unwrap(),
        r"export NCAP_PROJECT='it'\''s'"
    );
    Ok(())
}

#[test]
fn setup_env_lets_explicit_container_socket_cache_and_log_win() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    cmd.env("NCAP_CONTAINER", "box");
    cmd.env("NCAP_SOCKET", "/run/x.sock");
    cmd.env("NCAP_CACHE_DIR", "/var/cache/x");
    cmd.env("NCAP_LOG_DIR", "/var/log/x");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "export NCAP_PROJECT='proj'\nexport NCAP_CONTAINER='box'\nexport NCAP_SOCKET='/run/x.sock'\nexport NCAP_CACHE_DIR='/var/cache/x'\nexport NCAP_LOG_DIR='/var/log/x'\n"
    );
    Ok(())
}

#[test]
fn setup_env_derives_the_socket_under_xdg_runtime_dir() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    cmd.env("XDG_RUNTIME_DIR", "/xdg/run");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("export NCAP_SOCKET='/xdg/run/nix-capsule/proj/ncap.sock'\n")
    );
    Ok(())
}

#[test]
fn setup_env_falls_back_to_tmpdir_for_the_socket() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    cmd.env("TMPDIR", "/tmpx");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).contains("export NCAP_SOCKET='/tmpx/nix-capsule/proj/ncap.sock'\n"));
    Ok(())
}

#[test]
fn setup_env_ignores_an_empty_tmpdir() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    cmd.env("TMPDIR", "");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).contains("export NCAP_SOCKET='/tmp/nix-capsule/proj/ncap.sock'\n"));
    Ok(())
}

#[test]
fn setup_env_derives_cache_and_logs_under_the_xdg_vars() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    cmd.env("XDG_CACHE_HOME", "/xdg/cache");
    cmd.env("XDG_STATE_HOME", "/xdg/state");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let out = text(&out.stdout);
    assert!(
        out.contains("export NCAP_CACHE_DIR='/xdg/cache/nix-capsule/proj'\n"),
        "got {out}"
    );
    assert!(
        out.contains("export NCAP_LOG_DIR='/xdg/state/nix-capsule/proj/logs'\n"),
        "got {out}"
    );
    Ok(())
}

#[test]
fn setup_env_falls_back_to_home_for_cache_and_logs() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.deriving_command(&["setup-env"]);
    cmd.env("HOME", "/home/x");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let out = text(&out.stdout);
    assert!(
        out.contains("export NCAP_CACHE_DIR='/home/x/.cache/nix-capsule/proj'\n"),
        "got {out}"
    );
    assert!(
        out.contains("export NCAP_LOG_DIR='/home/x/.local/state/nix-capsule/proj/logs'\n"),
        "got {out}"
    );
    Ok(())
}

#[test_case("NCAP_PROJECT_ROOT" ; "project_root")]
#[test_case("NCAP_IMAGE" ; "image")]
#[test_case("NCAP_RUNTIME" ; "runtime")]
#[test_case("NCAP_DEVSHELL" ; "devshell")]
#[test_case("NCAP_NIX" ; "nix")]
#[test_case("NCAP_SERVER" ; "server")]
#[test_case("NCAP_BASH" ; "bash")]
#[test_case("NCAP_TIMEOUT" ; "timeout")]
#[test_case("NCAP_WATCH_FILES" ; "watch_files")]
#[test_case("NCAP_RUN_OPTS" ; "run_opts")]
#[test_case("NCAP_HARDEN" ; "harden")]
#[test_case("NCAP_LOG_LEVEL" ; "log_level")]
fn a_missing_demanded_var_is_an_error_naming_it(var: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env_remove(var);
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stderr).contains(var), "got {}", text(&out.stderr));
    Ok(())
}

#[test_case("NCAP_IMAGE" ; "a_var_without_a_derive")]
#[test_case("NCAP_TIMEOUT" ; "the_timeout")]
fn an_empty_value_counts_as_unset(var: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env(var, "");
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stderr).contains(var), "got {}", text(&out.stderr));
    Ok(())
}

#[test_case("NCAP_WATCH_FILES", "{}" ; "watch_files_an_object")]
#[test_case("NCAP_WATCH_FILES", "[1]" ; "watch_files_non_strings")]
#[test_case("NCAP_RUN_OPTS", "not json" ; "run_opts_not_json")]
#[test_case("NCAP_RUN_OPTS", "[1]" ; "run_opts_non_strings")]
fn a_json_array_var_accepts_only_string_arrays(var: &str, value: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env(var, value);
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stderr).contains(var), "got {}", text(&out.stderr));
    Ok(())
}

#[test_case("[]" ; "an_empty_array")]
#[test_case(r#"["a"]"# ; "a_string_array")]
fn a_json_array_var_accepts_string_arrays(value: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_RUN_OPTS", value);
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test_case("/abs/watch" ; "an_absolute_entry")]
#[test_case("../up" ; "a_parent_component")]
#[test_case("a/../../up" ; "a_parent_component_in_the_middle")]
fn a_watch_file_entry_must_be_project_root_relative(entry: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_WATCH_FILES", format!(r#"["{entry}"]"#));
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("NCAP_WATCH_FILES"), "got {err}");
    assert!(err.contains(entry), "the entry is named, got {err}");
    Ok(())
}

#[test]
fn a_watch_file_entry_that_exists_must_be_a_file() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir(w.root.join("a-dir"))?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_WATCH_FILES", r#"["a-dir"]"#);
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("NCAP_WATCH_FILES"), "got {err}");
    assert!(err.contains("a-dir"), "the entry is named, got {err}");
    Ok(())
}

#[test]
fn a_watch_file_entry_may_be_a_broken_symlink() -> anyhow::Result<()> {
    let w = World::new()?;
    std::os::unix::fs::symlink(w.root.join("nowhere"), w.root.join("broken"))?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_WATCH_FILES", r#"["broken"]"#);
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test_case("yes" ; "a_word")]
#[test_case("1" ; "a_number")]
fn harden_must_be_true_or_false(value: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_HARDEN", value);
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("NCAP_HARDEN"),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test_case("trace" ; "a_level_below_debug")]
#[test_case("DEBUG" ; "the_wrong_case")]
#[test_case("info2" ; "an_unknown_level")]
fn log_level_must_be_one_of_the_four(value: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_LOG_LEVEL", value);
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("NCAP_LOG_LEVEL"),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test_case("-1" ; "negative")]
#[test_case("1.5" ; "fractional")]
#[test_case("abc" ; "not_a_number")]
fn timeout_must_be_a_non_negative_integer(value: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_TIMEOUT", value);
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("NCAP_TIMEOUT"),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn timeout_zero_is_allowed() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_TIMEOUT", "0");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn env_forward_is_validated_by_the_client_only() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_ENV_FORWARD", "not json at all");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn an_unknown_runtime_name_is_an_error_naming_the_var() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["status"]);
    cmd.env("NCAP_RUNTIME", "firecracker");
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("NCAP_RUNTIME"),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn auto_prefers_an_executable_podman() -> anyhow::Result<()> {
    let w = World::new()?;
    w.install_runtime("docker")?;
    let mut cmd = w.ctl_command(&["status"]);
    cmd.env("NCAP_RUNTIME", "auto");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.rt_names(), vec!["podman".to_string()]);
    Ok(())
}

#[test]
fn auto_skips_a_podman_without_an_execute_bit() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::remove_file(w.bin.join("podman"))?;
    write_script(&w.bin.join("podman"), "#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(w.bin.join("podman"), fs::Permissions::from_mode(0o644))?;
    w.install_runtime("docker")?;
    let mut cmd = w.ctl_command(&["status"]);
    cmd.env("NCAP_RUNTIME", "auto");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.rt_names(), vec!["docker".to_string()]);
    Ok(())
}

#[test]
fn auto_skips_a_podman_that_is_a_directory() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::remove_file(w.bin.join("podman"))?;
    fs::create_dir(w.bin.join("podman"))?;
    w.install_runtime("docker")?;
    let mut cmd = w.ctl_command(&["status"]);
    cmd.env("NCAP_RUNTIME", "auto");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.rt_names(), vec!["docker".to_string()]);
    Ok(())
}

#[test]
fn auto_falls_back_to_docker() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::remove_file(w.bin.join("podman"))?;
    w.install_runtime("docker")?;
    let mut cmd = w.ctl_command(&["status"]);
    cmd.env("NCAP_RUNTIME", "auto");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.rt_names(), vec!["docker".to_string()]);
    Ok(())
}

#[test]
fn auto_with_neither_runtime_errors_without_liveness_output() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::remove_file(w.bin.join("podman"))?;
    let mut cmd = w.ctl_command(&["status"]);
    cmd.env("NCAP_RUNTIME", "auto");
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("NCAP_RUNTIME"),
        "got {}",
        text(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "no fallthrough to liveness output, got {}",
        text(&out.stdout)
    );
    Ok(())
}

#[test]
fn an_explicit_runtime_skips_detection() -> anyhow::Result<()> {
    let w = World::new()?;
    w.install_runtime("docker")?;
    let mut cmd = w.ctl_command(&["status"]);
    cmd.env("NCAP_RUNTIME", "docker");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.rt_names(), vec!["docker".to_string()]);
    Ok(())
}

#[test]
fn a_runtime_probe_spawn_failure_counts_as_not_running() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::remove_file(w.bin.join("podman"))?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("not running"),
        "got {}",
        text(&out.stdout)
    );
    Ok(())
}

#[test]
fn a_runtime_probe_parse_failure_counts_as_not_running() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("not-a-bool")?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("not running"),
        "got {}",
        text(&out.stdout)
    );
    Ok(())
}

#[test]
fn running_without_a_connectable_socket_is_not_live() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.fresh_cache(&[])?;
    let out = w.ctl(&["init"]);
    assert!(
        w.verbs().contains(&"run".to_string()),
        "not live means a launch, got {:?}; stderr: {}",
        w.verbs(),
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn running_with_a_connectable_socket_is_live() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.fresh_cache(&[])?;
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(w.verbs().is_empty(), "nothing starts, got {:?}", w.verbs());
    Ok(())
}

#[test]
fn status_prints_three_lines() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).lines().count(),
        3,
        "got {}",
        text(&out.stdout)
    );
    Ok(())
}

#[test]
fn status_names_the_container_when_it_runs() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let _server = w.serve_current();
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let line = text(&out.stdout).lines().next().unwrap().to_string();
    assert!(line.contains("running"), "got {line}");
    assert!(line.contains("ncap-proj"), "got {line}");
    Ok(())
}

#[test]
fn status_reports_not_running() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stdout)
            .lines()
            .next()
            .unwrap()
            .contains("not running")
    );
    Ok(())
}

#[test]
fn status_names_the_socket_path_when_connectable() -> anyhow::Result<()> {
    let w = World::new()?;
    let _server = w.serve_current();
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let line = text(&out.stdout).lines().nth(1).unwrap().to_string();
    assert!(line.contains("connectable"), "got {line}");
    assert!(line.contains(&w.socket.display().to_string()), "got {line}");
    Ok(())
}

#[test]
fn status_names_the_socket_path_when_unreachable() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let line = text(&out.stdout).lines().nth(1).unwrap().to_string();
    assert!(line.contains("unreachable"), "got {line}");
    assert!(line.contains(&w.socket.display().to_string()), "got {line}");
    Ok(())
}

#[test]
fn status_reports_a_fresh_cache() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).lines().nth(2).unwrap().contains("fresh"));
    Ok(())
}

#[test]
fn status_reports_a_stale_cache() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    fs::write(w.cache.join("hash"), "deadbeefdeadbeef\n")?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stdout).lines().nth(2).unwrap().contains("stale"));
    Ok(())
}

#[test]
fn status_reports_a_missing_cache() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stdout)
            .lines()
            .nth(2)
            .unwrap()
            .contains("missing")
    );
    Ok(())
}

#[test]
fn status_warns_on_version_skew_without_failing() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let _server = w.serve(Reply::Version("0.0.0-other".to_string()));
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(text(&out.stdout).lines().count(), 3);
    let err = text(&out.stderr);
    assert!(
        err.contains("0.0.0-other"),
        "the Server's version is named, got {err}"
    );
    assert!(
        err.contains(CURRENT_VERSION),
        "the host's version is named, got {err}"
    );
    assert!(
        err.contains("restart"),
        "the advice is `ncap-ctl restart`, got {err}"
    );
    Ok(())
}

#[test]
fn status_without_skew_warns_not() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let _server = w.serve_current();
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        out.stderr.is_empty(),
        "stderr must stay quiet, got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn status_warns_when_the_server_rejects_the_version_probe() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let _server = w.serve(Reply::Reject);
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(!err.is_empty(), "the same warning prints");
    assert!(
        err.contains("restart"),
        "the advice is identical, got {err}"
    );
    Ok(())
}

#[test]
fn status_skips_the_version_probe_silently_when_the_socket_is_unreachable() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        out.stderr.is_empty(),
        "stderr must stay quiet, got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn status_skips_a_silent_server_silently() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let _server = w.serve(Reply::Silent);
    let out = w.ctl(&["status"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        out.stderr.is_empty(),
        "stderr must stay quiet, got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn init_on_a_live_fresh_project_finishes_without_evaluating_or_starting() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.fresh_cache(&[])?;
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        w.nix_calls().is_empty(),
        "no Nix evaluation, got {:?}",
        w.nix_calls()
    );
    assert!(w.verbs().is_empty(), "nothing starts, got {:?}", w.verbs());
    Ok(())
}

#[test]
fn init_on_a_live_stale_project_reevaluates_stops_and_starts() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.fresh_cache(&[])?;
    fs::write(w.cache.join("hash"), "deadbeefdeadbeef")?;
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.nix_verb(0), "print-dev-env");
    assert_eq!(w.verbs(), vec!["stop", "rm", "run"]);
    Ok(())
}

#[test]
fn init_on_a_live_project_with_a_missing_cache_reevaluates_and_starts() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.nix_verb(0), "print-dev-env");
    assert_eq!(w.verbs(), vec!["stop", "rm", "run"]);
    Ok(())
}

#[test]
fn init_on_a_not_live_fresh_project_starts_without_evaluating() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        w.nix_calls().is_empty(),
        "no Nix evaluation, got {:?}",
        w.nix_calls()
    );
    assert_eq!(w.verbs(), vec!["rm", "run"]);
    Ok(())
}

#[test]
fn init_on_a_not_live_stale_project_evaluates_then_starts() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&["watched"])?;
    fs::write(w.cache.join("hash"), "deadbeefdeadbeef")?;
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.nix_verb(0), "print-dev-env");
    assert_eq!(w.verbs(), vec!["rm", "run"]);
    Ok(())
}

#[test]
fn init_evaluates_with_the_complete_devshell_uri() -> anyhow::Result<()> {
    let w = World::new()?;
    let _server = w.serve_current();
    let mut cmd = w.ctl_command(&["init"]);
    cmd.env("NCAP_DEVSHELL", ".#bare-name");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let calls = w.nix_calls();
    assert_eq!(
        calls[0].args.last().map(String::as_str),
        Some(".#bare-name")
    );
    Ok(())
}

#[test]
fn init_writes_the_env_dump_and_the_hash() -> anyhow::Result<()> {
    let w = World::new()?;
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        fs::read_to_string(w.cache.join("env"))?,
        "export NCAP_STUB_DUMP=1\n"
    );
    assert!(w.cache.join("hash").is_file());
    Ok(())
}

#[test]
fn init_prunes_the_profile_history_after_the_eval() -> anyhow::Result<()> {
    let w = World::new()?;
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let calls = w.nix_calls();
    assert!(calls.len() >= 2, "a prune follows the eval, got {calls:?}");
    assert_eq!(calls[0].args[0], "print-dev-env");
    Ok(())
}

#[test]
fn init_survives_a_failing_profile_prune() -> anyhow::Result<()> {
    let w = World::new()?;
    w.mark_failprune();
    let _server = w.serve_current();
    let out = w.ctl(&["init"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn start_without_an_env_dump_errors_advising_init() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&["start"]);
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("init"),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn start_creates_the_socket_dir_with_mode_0700() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let mode = fs::metadata(w.socket.parent().unwrap())?
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    Ok(())
}

#[test]
fn start_leaves_an_existing_socket_dir_untouched() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    fs::create_dir(w.socket.parent().unwrap())?;
    fs::set_permissions(
        w.socket.parent().unwrap(),
        fs::Permissions::from_mode(0o755),
    )?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let mode = fs::metadata(w.socket.parent().unwrap())?
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755);
    Ok(())
}

#[test]
fn start_creates_the_log_dir() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    assert!(w.logs.is_dir(), "the log dir must be created");
    Ok(())
}

#[test]
fn start_removes_a_stopped_container_before_launch() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    w.write_state("false")?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    assert_eq!(w.verbs(), vec!["rm", "run"]);
    Ok(())
}

#[test]
fn the_launch_command_carries_the_image_bash_and_server_script() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let tail = launch_tail(&w.rt_run());
    assert_eq!(
        tail,
        vec![
            "example/image:tag".to_string(),
            w.bash.display().to_string(),
            "-c".to_string(),
            format!(
                "source '{}/env' && exec '{}' --socket '{}' --log-dir '{}' --timeout 5 --log-level info",
                w.cache.display(),
                w.server.display(),
                w.socket.display(),
                w.logs.display(),
            ),
        ]
    );
    Ok(())
}

#[test]
fn the_launch_command_single_quote_escapes_interpolated_paths() -> anyhow::Result<()> {
    let w = World::new()?;
    let quoted = w.temp.path().join("it's-server");
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    cmd.env("NCAP_SERVER", &quoted);
    let _ = cmd.output()?;
    let tail = launch_tail(&w.rt_run());
    let escaped = quoted.display().to_string().replace('\'', r"'\''");
    assert!(
        tail[3].contains(&format!("exec '{escaped}'")),
        "got {}",
        tail[3]
    );
    Ok(())
}

#[test]
fn the_mount_build_order_is_exact() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let sockdir = w.socket.parent().unwrap().to_str().unwrap();
    let listed = bind_sources(
        &opts,
        &[
            "/nix",
            sockdir,
            w.root.to_str().unwrap(),
            w.cache.to_str().unwrap(),
            w.logs.to_str().unwrap(),
        ],
    );
    assert_eq!(
        listed,
        vec![
            "/nix",
            sockdir,
            w.root.to_str().unwrap(),
            w.cache.to_str().unwrap(),
            w.logs.to_str().unwrap()
        ]
    );
    Ok(())
}

#[test]
fn the_cache_mount_is_read_only() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let mount = arg_for(&opts, w.cache.to_str().unwrap());
    assert!(
        mount_is_read_only(mount),
        "the Cache mounts read-only, got {mount}"
    );
    Ok(())
}

#[test]
fn the_socket_project_and_log_mounts_are_read_write() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    for source in [
        w.socket.parent().unwrap().to_str().unwrap(),
        w.root.to_str().unwrap(),
        w.logs.to_str().unwrap(),
    ] {
        let mount = arg_for(&opts, source);
        assert!(
            !mount_is_read_only(mount),
            "{source} mounts read-write, got {mount}"
        );
    }
    Ok(())
}

#[test]
fn the_workdir_flag_carries_the_project_root() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let at = opts
        .iter()
        .position(|a| a == "-w")
        .expect("`-w` is pinned in the build order");
    assert_eq!(opts[at + 1], w.root.to_str().unwrap());
    Ok(())
}

#[test]
fn the_git_directory_is_mounted_when_it_is_a_real_directory() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir(w.root.join(".git"))?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let git = format!("{}/.git", w.root.display());
    assert_eq!(bind_sources(&opts, &[&git]), vec![git.as_str()]);
    Ok(())
}

#[test]
fn the_git_directory_is_skipped_for_a_gitfile() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::write(w.root.join(".git"), "gitdir: elsewhere\n")?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let git = format!("{}/.git", w.root.display());
    assert!(bind_sources(&opts, &[&git]).is_empty(), "got {opts:?}");
    Ok(())
}

#[test]
fn the_git_directory_is_skipped_for_a_symlink() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir(w.temp.path().join("elsewhere"))?;
    std::os::unix::fs::symlink(w.temp.path().join("elsewhere"), w.root.join(".git"))?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let git = format!("{}/.git", w.root.display());
    assert!(bind_sources(&opts, &[&git]).is_empty(), "got {opts:?}");
    Ok(())
}

#[test]
fn extra_options_append_after_the_mounts_in_order() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    cmd.env("NCAP_RUN_OPTS", r#"["--label","a=b","--foo"]"#);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let tail: Vec<&str> = opts[opts.len() - 3..].iter().map(String::as_str).collect();
    assert_eq!(tail, ["--label", "a=b", "--foo"]);
    Ok(())
}

#[test]
fn harden_prepends_the_capability_flags() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    cmd.env("NCAP_HARDEN", "true");
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let head: Vec<&str> = opts[0..2].iter().map(String::as_str).collect();
    assert_eq!(head, ["--cap-drop=all", "--security-opt=no-new-privileges"]);
    Ok(())
}

#[test]
fn harden_mounts_present_watched_files_read_only() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::write(w.root.join("watched"), "contents\n")?;
    w.fresh_cache(&["watched"])?;
    let mut cmd = w.ctl_command(&["start"]);
    cmd.env("NCAP_HARDEN", "true");
    cmd.env("NCAP_WATCH_FILES", r#"["watched"]"#);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    let watched = format!("{}/watched", w.root.display());
    let mount = arg_for(&opts, &watched);
    assert!(
        mount_is_read_only(mount),
        "a watched file mounts read-only, got {mount}"
    );
    let sockdir = w.socket.parent().unwrap().to_str().unwrap();
    let listed = bind_sources(
        &opts,
        &[
            "/nix",
            sockdir,
            w.root.to_str().unwrap(),
            w.cache.to_str().unwrap(),
            w.logs.to_str().unwrap(),
            &watched,
        ],
    );
    assert_eq!(listed.last().copied(), Some(watched.as_str()));
    Ok(())
}

#[test]
fn harden_skips_absent_and_broken_watched_files() -> anyhow::Result<()> {
    let w = World::new()?;
    std::os::unix::fs::symlink(w.root.join("nowhere"), w.root.join("broken"))?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    cmd.env("NCAP_HARDEN", "true");
    cmd.env("NCAP_WATCH_FILES", r#"["gone","broken"]"#);
    let _ = cmd.output()?;
    let opts = mounts_and_options(&w.rt_run());
    assert!(
        bind_sources(&opts, &["gone", "broken"]).is_empty(),
        "got {opts:?}"
    );
    Ok(())
}

#[test]
fn a_socket_path_without_a_parent_directory_is_a_launch_error_naming_the_socket()
-> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    let mut cmd = w.ctl_command(&["start"]);
    cmd.env("NCAP_SOCKET", "/");
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("socket path `/` has no parent directory"),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn start_that_never_reaches_readiness_fails_with_the_inspect_state() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    w.write_state("pending")?;
    w.mark_rundown();
    let mut cmd = w.ctl_command(&["start"]);
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("pending"),
        "the inspect state is reported, got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn a_lost_race_with_a_running_container_succeeds() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    w.write_state("false")?;
    w.mark_runfail("name in use")?;
    w.mark_runwon();
    let _server = w.serve_current();
    let out = w.ctl(&["start"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn a_lost_race_with_a_dead_container_starts_once_more() -> anyhow::Result<()> {
    let w = World::new()?;
    w.fresh_cache(&[])?;
    w.write_state("false")?;
    w.mark_runfail("name in use")?;
    let _server = w.serve_current();
    let out = w.ctl(&["start"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.verbs(), vec!["rm", "run", "rm", "run"]);
    Ok(())
}

#[test]
fn stop_names_the_container() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let out = w.ctl(&["stop"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let calls = w.rt_calls();
    let stop = calls
        .iter()
        .find(|c| c.args[0] == "stop")
        .expect("a `stop` call");
    assert_eq!(stop.args[1], "ncap-proj");
    Ok(())
}

#[test]
fn stop_succeeds_when_the_container_is_not_running() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("false")?;
    let out = w.ctl(&["stop"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn stop_succeeds_when_the_stop_fails_but_the_container_is_gone() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("false")?;
    w.mark_stopfail();
    let out = w.ctl(&["stop"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn stop_fails_when_the_stop_fails_and_the_container_still_runs() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.mark_stopfail();
    let out = w.ctl(&["stop"]);
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn restart_treats_a_failed_stop_as_non_fatal() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.fresh_cache(&["watched"])?;
    fs::write(w.cache.join("hash"), "deadbeefdeadbeef")?;
    w.mark_stopfail();
    let _server = w.serve_current();
    let out = w.ctl(&["restart"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.nix_verb(0), "print-dev-env");
    Ok(())
}

#[test]
fn restart_reaches_the_stamp_guard_before_any_cache_touch() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    fs::write(w.cache.join("project"), "/elsewhere\n")?;
    let out = w.ctl(&["restart"]);
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(
        err.contains("/elsewhere"),
        "the keyed root is named, got {err}"
    );
    assert!(
        err.contains("project"),
        "the advice names `project`, got {err}"
    );
    assert!(
        w.verbs().contains(&"stop".to_string()),
        "the stop runs first, got {:?}",
        w.verbs()
    );
    Ok(())
}

#[test]
fn enter_sources_the_env_dump_and_execs_the_container_shell() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let out = w.ctl(&["enter"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let calls = w.rt_calls();
    let exec = calls
        .iter()
        .find(|c| c.args[0] == "exec")
        .expect("an `exec` call");
    assert_eq!(
        exec.args,
        vec![
            "exec".to_string(),
            "-it".to_string(),
            "ncap-proj".to_string(),
            w.bash.display().to_string(),
            "-c".to_string(),
            format!(
                "source '{}/env' && exec '{}'",
                w.cache.display(),
                w.bash.display()
            ),
        ]
    );
    Ok(())
}

#[test]
fn enter_on_a_down_container_errors_advising_init() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("false")?;
    let out = w.ctl(&["enter"]);
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("init"),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn the_stamp_guard_writes_the_project_root_creating_the_cache_dir() -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&["start"]);
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        fs::read_to_string(w.cache.join("project"))?.trim_end_matches(['\n', '\r']),
        w.root.display().to_string()
    );
    Ok(())
}

#[test]
fn the_stamp_guard_passes_on_the_same_root() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.fresh_cache(&[])?;
    fs::create_dir_all(&w.cache)?;
    fs::write(w.cache.join("project"), format!("{}\n", w.root.display()))?;
    let _server = w.serve_current();
    let out = w.ctl(&["start"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        out.stderr.is_empty(),
        "passes silently, got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn the_stamp_guard_rejects_a_different_keyed_root() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    fs::write(w.cache.join("project"), "/elsewhere\n")?;
    let out = w.ctl(&["start"]);
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("proj"), "the project is named, got {err}");
    assert!(
        err.contains("/elsewhere"),
        "the keyed root is named, got {err}"
    );
    assert!(
        err.contains("project"),
        "the advice names `project`, got {err}"
    );
    Ok(())
}

#[test_case("" ; "no_trailing_newline")]
#[test_case("\n" ; "one_newline")]
#[test_case("\r" ; "one_carriage_return")]
#[test_case("\r\n" ; "both")]
fn the_stamp_guard_trims_trailing_newlines(suffix: &str) -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.fresh_cache(&[])?;
    fs::create_dir_all(&w.cache)?;
    fs::write(
        w.cache.join("project"),
        format!("{}{suffix}", w.root.display()),
    )?;
    let _server = w.serve_current();
    let out = w.ctl(&["start"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn clean_removes_the_four_cache_files_and_the_generation_links() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    for name in ["env", "hash", "profile", "project"] {
        fs::write(w.cache.join(name), "x\n")?;
    }
    let target = w.temp.path().join("target");
    fs::write(&target, "keep me\n")?;
    std::os::unix::fs::symlink(&target, w.cache.join("profile-3-link"))?;
    fs::create_dir(w.cache.join("profile-12-link"))?;
    fs::write(w.cache.join("profile-12-link").join("inner"), "x\n")?;
    fs::write(w.cache.join("profile-x-link"), "x\n")?;
    fs::write(w.cache.join("profile-3-link2"), "x\n")?;
    let out = w.ctl(&["clean"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    for name in [
        "env",
        "hash",
        "profile",
        "project",
        "profile-3-link",
        "profile-12-link",
    ] {
        assert!(!w.cache.join(name).exists(), "{name} must be removed");
    }
    assert!(w.cache.join("profile-x-link").is_file());
    assert!(w.cache.join("profile-3-link2").is_file());
    assert!(target.is_file(), "a symlink is removed without following");
    Ok(())
}

#[test]
fn clean_leaves_unrelated_cache_files() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    fs::write(w.cache.join("env"), "x\n")?;
    fs::write(w.cache.join("notes.txt"), "keep me\n")?;
    let out = w.ctl(&["clean"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        w.cache.join("notes.txt").is_file(),
        "never recursive on an explicit path"
    );
    Ok(())
}

#[test]
fn clean_removes_the_log_files() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.logs)?;
    fs::write(w.logs.join("ncap-server-1.log"), "x\n")?;
    fs::write(w.logs.join("ncap-server-2.log"), "x\n")?;
    fs::write(w.logs.join("other.txt"), "keep me\n")?;
    let out = w.ctl(&["clean"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(!w.logs.join("ncap-server-1.log").exists());
    assert!(!w.logs.join("ncap-server-2.log").exists());
    assert!(w.logs.join("other.txt").is_file());
    Ok(())
}

#[test]
fn clean_removes_the_socket_file_and_its_empty_parent() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(w.socket.parent().unwrap())?;
    fs::write(&w.socket, "")?;
    let out = w.ctl(&["clean"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(!w.socket.exists());
    assert!(!w.socket.parent().unwrap().exists());
    Ok(())
}

#[test]
fn clean_leaves_a_non_empty_socket_parent() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(w.socket.parent().unwrap())?;
    fs::write(&w.socket, "")?;
    fs::write(w.socket.parent().unwrap().join("sibling"), "keep me\n")?;
    let out = w.ctl(&["clean"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(!w.socket.exists());
    assert!(w.socket.parent().unwrap().is_dir());
    Ok(())
}

#[test]
fn clean_stops_and_removes_the_container() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    let out = w.ctl(&["clean"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(w.verbs(), vec!["stop", "rm"]);
    Ok(())
}

#[test]
fn clean_survives_a_failed_stop() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_state("true")?;
    w.mark_stopfail();
    let out = w.ctl(&["clean"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn clean_twice_succeeds() -> anyhow::Result<()> {
    let w = World::new()?;
    let first = w.ctl(&["clean"]);
    assert!(first.status.success(), "stderr: {}", text(&first.stderr));
    let second = w.ctl(&["clean"]);
    assert!(second.status.success(), "stderr: {}", text(&second.stderr));
    Ok(())
}

#[test]
fn digest_of_an_empty_watch_list_is_the_spec_literal() -> anyhow::Result<()> {
    let w = World::new()?;
    assert_eq!(compute(&w.root, &[])?, "ef46db3751d8e999");
    Ok(())
}

#[test]
fn digest_of_one_file_records_its_contents() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::write(w.root.join("a.txt"), "hello")?;
    assert_eq!(
        compute(&w.root, &["a.txt".to_string()])?,
        "f89eb5c681a8a072"
    );
    Ok(())
}

#[test]
fn digest_sorts_the_entries_by_relative_path() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::write(w.root.join("a.txt"), "hello")?;
    fs::write(w.root.join("b.txt"), "bbb")?;
    let unsorted = vec!["b.txt".to_string(), "a.txt".to_string()];
    assert_eq!(compute(&w.root, &unsorted)?, "6cdcd94993f8f8af");
    Ok(())
}

#[test]
fn digest_records_the_absence_of_a_missing_entry() -> anyhow::Result<()> {
    let w = World::new()?;
    assert_eq!(
        compute(&w.root, &["gone.txt".to_string()])?,
        "c3bbcec241c69a88"
    );
    Ok(())
}

#[test]
fn digest_records_the_relative_path_of_a_moved_file() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::write(w.root.join("c.txt"), "same")?;
    fs::write(w.root.join("d.txt"), "same")?;
    assert_eq!(
        compute(&w.root, &["c.txt".to_string()])?,
        "c2862c1387d1343a"
    );
    assert_eq!(
        compute(&w.root, &["d.txt".to_string()])?,
        "a292f96d97aea3cd"
    );
    Ok(())
}

#[test]
fn digest_ignores_a_mtime_only_touch() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::write(w.root.join("a.txt"), "hello")?;
    let file = fs::OpenOptions::new()
        .write(true)
        .open(w.root.join("a.txt"))?;
    file.set_modified(SystemTime::now() + Duration::from_secs(60))?;
    assert_eq!(
        compute(&w.root, &["a.txt".to_string()])?,
        "f89eb5c681a8a072"
    );
    Ok(())
}

#[test]
fn store_writes_lowercase_hex_without_a_trailing_newline() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    store(&w.cache, "deadbeef")?;
    assert_eq!(fs::read(w.cache.join("hash"))?, b"deadbeef".to_vec());
    Ok(())
}

#[test]
fn check_reports_missing_without_an_env_dump() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    fs::write(w.cache.join("hash"), "deadbeefdeadbeef")?;
    assert_eq!(check(&w.cache, &w.root, &[]), Freshness::Missing);
    Ok(())
}

#[test]
fn check_reports_fresh_when_the_hash_matches_after_trimming() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    fs::write(w.cache.join("env"), "dump\n")?;
    fs::write(
        w.cache.join("hash"),
        format!("{}\n", compute(&w.root, &[])?),
    )?;
    assert_eq!(check(&w.cache, &w.root, &[]), Freshness::Fresh);
    Ok(())
}

#[test]
fn check_reports_stale_when_the_hash_differs() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    fs::write(w.cache.join("env"), "dump\n")?;
    fs::write(w.cache.join("hash"), "deadbeefdeadbeef")?;
    assert_eq!(check(&w.cache, &w.root, &[]), Freshness::Stale);
    Ok(())
}

#[test]
fn check_reports_stale_when_the_hash_is_unreadable() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    fs::write(w.cache.join("env"), "dump\n")?;
    fs::create_dir(w.cache.join("hash"))?;
    assert_eq!(check(&w.cache, &w.root, &[]), Freshness::Stale);
    Ok(())
}

#[test]
fn check_reports_stale_on_a_compute_error_even_against_an_empty_hash() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.cache)?;
    fs::create_dir(w.root.join("a-dir"))?;
    fs::write(w.cache.join("env"), "dump\n")?;
    fs::write(w.cache.join("hash"), "")?;
    let entries = vec!["a-dir".to_string()];
    assert_eq!(check(&w.cache, &w.root, &entries), Freshness::Stale);
    Ok(())
}

#[test]
fn show_options_prints_one_expanded_arg_per_line() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_RUN_OPTS", r#"["one","two"]"#);
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(out.stdout, b"one\ntwo\n".to_vec());
    Ok(())
}

#[test]
fn show_options_expands_braced_variables() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("HOME_PATH", "/home/x");
    cmd.env("NCAP_RUN_OPTS", r#"["${HOME_PATH}/m"]"#);
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(out.stdout, b"/home/x/m\n".to_vec());
    Ok(())
}

#[test]
fn show_options_fails_naming_an_unset_variable() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env_remove("NOT_SET_ANYWHERE");
    cmd.env("NCAP_RUN_OPTS", r#"["$NOT_SET_ANYWHERE"]"#);
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("NOT_SET_ANYWHERE"),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn show_options_expands_an_empty_value_to_nothing() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("EMPTY", "");
    cmd.env("NCAP_RUN_OPTS", r#"["x${EMPTY}y"]"#);
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(out.stdout, b"xy\n".to_vec());
    Ok(())
}

#[test]
fn show_options_does_not_split_expanded_values_on_whitespace() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("SPACED", "a b");
    cmd.env("NCAP_RUN_OPTS", r#"["$SPACED"]"#);
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(out.stdout, b"a b\n".to_vec());
    Ok(())
}

#[test_case("$", "$" ; "a_lone_dollar")]
#[test_case("$$", "$$" ; "two_dollars")]
#[test_case("$5", "$5" ; "a_digit_name")]
#[test_case("$-x", "$-x" ; "a_dash_name")]
#[test_case("${}", "${}" ; "an_empty_brace")]
#[test_case("${5}", "${5}" ; "a_braced_digit")]
#[test_case("${foo-bar}", "${foo-bar}" ; "a_braced_non_name")]
#[test_case("${foo", "${foo" ; "an_unterminated_brace")]
fn show_options_leaves_a_dollar_that_names_nothing_literal(
    entry: &str,
    expected: &str,
) -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("NCAP_RUN_OPTS", format!(r#"["{entry}"]"#));
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(out.stdout, format!("{expected}\n").into_bytes());
    Ok(())
}

#[test]
fn show_options_expands_the_longest_name_run() -> anyhow::Result<()> {
    let w = World::new()?;
    let mut cmd = w.ctl_command(&["show-options"]);
    cmd.env("foo", "F");
    cmd.env("NCAP_RUN_OPTS", r#"["$foo-bar"]"#);
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(out.stdout, b"F-bar\n".to_vec());
    Ok(())
}

#[test_case(&[] ; "no_flag")]
#[test_case(&["--no-pager"] ; "no_pager")]
#[test_case(&["--follow"] ; "follow")]
fn log_without_a_log_file_errors_naming_the_log_dir(flags: &[&str]) -> anyhow::Result<()> {
    let w = World::new()?;
    let out = w.ctl(&[["log"].as_slice(), flags].concat());
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains(&w.logs.display().to_string()),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn log_no_pager_writes_the_file_byte_for_byte() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(1, b"line one\nline two\n")?;
    let out = w.ctl(&["log", "--no-pager"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(out.stdout, b"line one\nline two\n".to_vec());
    Ok(())
}

#[test]
fn log_no_pager_stays_quiet_on_stderr() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(1, b"x\n")?;
    let out = w.ctl(&["log", "--no-pager"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        out.stderr.is_empty(),
        "stderr must stay quiet, got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn log_no_pager_exits_quietly_on_a_broken_pipe() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(1, &[b'x'; 65536])?;
    let mut cmd = w.ctl_command(&["log", "--no-pager"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut pipe = child.stdout.take().expect("stdout is piped");
    let mut head = [0u8; 8];
    pipe.read_exact(&mut head)?;
    drop(pipe);
    let out = child.wait_with_output()?;
    assert!(
        out.status.success(),
        "a broken pipe is not failure, got {}",
        text(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "stderr must stay quiet, got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn log_no_pager_fails_on_a_stdout_write_error() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(1, b"x\n")?;
    // A read-only stdout fd fails with `EBADF`, which std's `Stdout`
    // swallows (its stdio hack for processes launched without valid std
    // fds); `/dev/full` fails with `ENOSPC`, which surfaces.
    let devfull = fs::OpenOptions::new().write(true).open("/dev/full")?;
    let mut cmd = w.ctl_command(&["log", "--no-pager"]);
    cmd.stdout(Stdio::from(devfull));
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(!out.stderr.is_empty(), "the failure is loud");
    Ok(())
}

#[test]
fn log_opens_the_file_in_the_pager_with_split_arguments() -> anyhow::Result<()> {
    let w = World::new()?;
    let log = w.write_log(1, b"x\n")?;
    w.install_pager("my-pager")?;
    let mut cmd = w.ctl_command(&["log"]);
    cmd.env("PAGER", "my-pager --flag");
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("PAGER-RAN"),
        "stdio is inherited, got {}",
        text(&out.stdout)
    );
    let calls = read_calls(&w.pager_log);
    assert_eq!(calls[0].name, "my-pager");
    assert_eq!(
        calls[0].args,
        vec!["--flag".to_string(), log.display().to_string()]
    );
    Ok(())
}

#[test_case("" ; "blank")]
#[test_case("unset" ; "unset")]
fn log_falls_back_to_less_with_the_r_flag_when_the_pager_is_blank(
    pager: &str,
) -> anyhow::Result<()> {
    let w = World::new()?;
    let log = w.write_log(1, b"x\n")?;
    w.install_pager("less")?;
    let mut cmd = w.ctl_command(&["log"]);
    if pager == "unset" {
        cmd.env_remove("PAGER");
    } else {
        cmd.env("PAGER", pager);
    }
    let out = cmd.output()?;
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let calls = read_calls(&w.pager_log);
    assert_eq!(calls[0].name, "less");
    assert_eq!(
        calls[0].args,
        vec!["-R".to_string(), log.display().to_string()]
    );
    Ok(())
}

#[test]
fn log_fails_when_the_pager_cannot_spawn() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(1, b"x\n")?;
    let mut cmd = w.ctl_command(&["log"]);
    cmd.env("PAGER", "/no/such/pager");
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn log_fails_when_the_pager_exits_non_zero() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(1, b"x\n")?;
    write_script(&w.bin.join("failing"), "#!/bin/sh\nexit 3\n")?;
    let mut cmd = w.ctl_command(&["log"]);
    cmd.env("PAGER", "failing");
    let out = cmd.output()?;
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    Ok(())
}

#[test]
fn log_follow_announces_the_first_file() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(7, b"seven\n")?;
    let mut cmd = w.ctl_command(&["log", "--follow"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let tap = tap(&mut child);
    wait_until(&tap, "the first banner", |out| {
        out.contains("ncap-server-7.log")
    })?;
    let out = text(&tap.snapshot());
    let line = out
        .lines()
        .find(|l| l.contains("ncap-server-7.log"))
        .unwrap();
    assert!(line.starts_with("==>"), "got {line}");
    assert!(line.ends_with("<=="), "got {line}");
    let _ = child.kill();
    Ok(())
}

#[test]
fn log_follow_streams_from_the_first_byte_and_appends() -> anyhow::Result<()> {
    let w = World::new()?;
    let log = w.write_log(1, b"first\n")?;
    let mut cmd = w.ctl_command(&["log", "--follow"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let tap = tap(&mut child);
    wait_until(&tap, "the first bytes", |out| out.contains("first"))?;
    fs::OpenOptions::new()
        .append(true)
        .open(&log)?
        .write_all(b"second\n")?;
    wait_until(&tap, "the appended bytes", |out| out.contains("second"))?;
    let _ = child.kill();
    Ok(())
}

#[test]
fn log_follow_drains_then_switches_to_newer_files_in_ascending_epochs() -> anyhow::Result<()> {
    let w = World::new()?;
    let first = w.write_log(1, b"one-a\n")?;
    let mut cmd = w.ctl_command(&["log", "--follow"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let tap = tap(&mut child);
    wait_until(&tap, "the first banner", |out| {
        out.contains("ncap-server-1.log")
    })?;
    fs::OpenOptions::new()
        .append(true)
        .open(&first)?
        .write_all(b"one-b\n")?;
    w.write_log(4, b"four\n")?;
    w.write_log(9, b"nine\n")?;
    wait_until(&tap, "the newer files", |out| {
        out.contains("ncap-server-9.log") && out.contains("nine")
    })?;
    let out = text(&tap.snapshot());
    let drained = out
        .find("one-b")
        .expect("the attached file drains to EOF first");
    let switched = out
        .find("ncap-server-4.log")
        .expect("the newer file attaches");
    assert!(drained < switched, "drain-then-switch, got {out}");
    assert!(
        out.find("ncap-server-4.log").unwrap() < out.find("ncap-server-9.log").unwrap(),
        "epochs ascending, got {out}"
    );
    let _ = child.kill();
    Ok(())
}

#[test]
fn log_follow_lands_both_files_after_two_restarts() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(1, b"one\n")?;
    let mut cmd = w.ctl_command(&["log", "--follow"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let tap = tap(&mut child);
    wait_until(&tap, "the first banner", |out| {
        out.contains("ncap-server-1.log")
    })?;
    w.write_log(2, b"two\n")?;
    w.write_log(3, b"three\n")?;
    wait_until(&tap, "both restarted files", |out| {
        out.contains("two") && out.contains("three")
    })?;
    let _ = child.kill();
    Ok(())
}

#[test]
fn log_follow_keeps_running_when_the_file_vanishes() -> anyhow::Result<()> {
    let w = World::new()?;
    let log = w.write_log(1, b"one\n")?;
    let mut cmd = w.ctl_command(&["log", "--follow"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let tap = tap(&mut child);
    wait_until(&tap, "the first bytes", |out| out.contains("one"))?;
    fs::remove_file(&log)?;
    w.write_log(2, b"two\n")?;
    wait_until(&tap, "the next file after a vanishing one", |out| {
        out.contains("two")
    })?;
    let _ = child.kill();
    Ok(())
}

#[test]
fn log_follow_ends_on_sigint_with_success() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(1, b"one\n")?;
    let mut cmd = w.ctl_command(&["log", "--follow"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let tap = tap(&mut child);
    wait_until(&tap, "the first banner", |out| {
        out.contains("ncap-server-1.log")
    })?;
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let out = child.wait_with_output()?;
    assert!(
        out.status.success(),
        "interruption is not failure, got {}",
        text(&out.stderr)
    );
    Ok(())
}

#[test]
fn log_picks_the_newest_log_by_numeric_epoch() -> anyhow::Result<()> {
    let w = World::new()?;
    w.write_log(2, b"two\n")?;
    w.write_log(10, b"ten\n")?;
    let out = w.ctl(&["log", "--no-pager"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(out.stdout, b"ten\n".to_vec());
    Ok(())
}

#[test]
fn log_ignores_file_names_that_are_not_server_logs() -> anyhow::Result<()> {
    let w = World::new()?;
    fs::create_dir_all(&w.logs)?;
    fs::write(w.logs.join("ncap-server-abc.log"), "x\n")?;
    fs::write(w.logs.join("ncap-server-1.log.txt"), "x\n")?;
    fs::write(w.logs.join("ncap-server-.log"), "x\n")?;
    fs::write(w.logs.join("other.log"), "x\n")?;
    let out = w.ctl(&["log", "--no-pager"]);
    assert!(!out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains(&w.logs.display().to_string()),
        "got {}",
        text(&out.stderr)
    );
    Ok(())
}

// ------------------------------------------------------------------- helpers

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

fn write_script(path: &Path, body: &str) -> io::Result<()> {
    fs::write(path, body)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

struct World {
    temp: TempDir,
    root: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
    socket: PathBuf,
    bin: PathBuf,
    nix: PathBuf,
    nix_log: PathBuf,
    rt_log: PathBuf,
    pager_log: PathBuf,
    state: PathBuf,
    absent: PathBuf,
    runcount: PathBuf,
    runfail: PathBuf,
    runwon: PathBuf,
    rundown: PathBuf,
    stopfail: PathBuf,
    failprune: PathBuf,
    bash: PathBuf,
    server: PathBuf,
}

impl World {
    fn new() -> anyhow::Result<World> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("proj");
        fs::create_dir_all(&root)?;
        let bin = temp.path().join("bin");
        fs::create_dir_all(&bin)?;
        let w = World {
            nix: temp.path().join("print-dev-env-stub"),
            nix_log: temp.path().join("nix.log"),
            rt_log: temp.path().join("rt.log"),
            pager_log: temp.path().join("pager.log"),
            state: temp.path().join("state"),
            absent: temp.path().join("absent"),
            runcount: temp.path().join("runcount"),
            runfail: temp.path().join("runfail"),
            runwon: temp.path().join("runwon"),
            rundown: temp.path().join("rundown"),
            stopfail: temp.path().join("stopfail"),
            failprune: temp.path().join("failprune"),
            bash: temp.path().join("bash-store-path"),
            server: temp.path().join("server-store-path"),
            cache: temp.path().join("cache"),
            logs: temp.path().join("logs"),
            socket: temp.path().join("sock").join("ncap.sock"),
            root,
            bin,
            temp,
        };
        fs::write(&w.state, "false\n")?;
        fs::write(&w.runcount, "0\n")?;
        write_script(&w.nix, &w.nix_stub())?;
        w.install_runtime("podman")?;
        Ok(w)
    }

    fn ctl_command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ncap-ctl"));
        cmd.args(args)
            .env_clear()
            .env("PATH", &self.bin)
            .env("HOME", self.temp.path())
            .env("TMPDIR", self.temp.path())
            .env("NCAP_PROJECT_ROOT", &self.root)
            .env("NCAP_PROJECT", "proj")
            .env("NCAP_CONTAINER", "ncap-proj")
            .env("NCAP_SOCKET", &self.socket)
            .env("NCAP_CACHE_DIR", &self.cache)
            .env("NCAP_LOG_DIR", &self.logs)
            .env("NCAP_IMAGE", "example/image:tag")
            .env("NCAP_RUNTIME", "podman")
            .env("NCAP_DEVSHELL", "/flake#shell")
            .env("NCAP_NIX", &self.nix)
            .env("NCAP_SERVER", &self.server)
            .env("NCAP_BASH", &self.bash)
            .env("NCAP_TIMEOUT", "5")
            .env("NCAP_WATCH_FILES", "[]")
            .env("NCAP_RUN_OPTS", "[]")
            .env("NCAP_HARDEN", "false")
            .env("NCAP_LOG_LEVEL", "info");
        cmd
    }

    fn ctl(&self, args: &[&str]) -> Output {
        self.ctl_command(args)
            .output()
            .expect("ncap-ctl spawns: it is a bin target of this crate")
    }

    /// A `Command` with the five project-scoped vars unset so `setup-env`
    /// derives them — an explicit value wins otherwise.
    fn deriving_command(&self, args: &[&str]) -> Command {
        let mut cmd = self.ctl_command(args);
        for var in [
            "NCAP_PROJECT",
            "NCAP_CONTAINER",
            "NCAP_SOCKET",
            "NCAP_CACHE_DIR",
            "NCAP_LOG_DIR",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    fn fresh_cache(&self, entries: &[&str]) -> anyhow::Result<()> {
        fs::create_dir_all(&self.cache)?;
        fs::write(self.cache.join("env"), "export NCAP_STUB_DUMP=1\n")?;
        let entries: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        store(&self.cache, &compute(&self.root, &entries)?)?;
        Ok(())
    }

    fn write_log(&self, epoch: u64, bytes: &[u8]) -> anyhow::Result<PathBuf> {
        fs::create_dir_all(&self.logs)?;
        let path = self.logs.join(format!("ncap-server-{epoch}.log"));
        fs::write(&path, bytes)?;
        Ok(path)
    }

    fn write_state(&self, value: &str) -> anyhow::Result<()> {
        fs::write(&self.state, format!("{value}\n"))?;
        Ok(())
    }

    fn mark_runfail(&self, message: &str) -> anyhow::Result<()> {
        fs::write(&self.runfail, format!("{message}\n"))?;
        Ok(())
    }

    fn mark_runwon(&self) {
        fs::write(&self.runwon, "").expect("the race marker is writable");
    }

    fn mark_rundown(&self) {
        fs::write(&self.rundown, "").expect("the readiness marker is writable");
    }

    fn mark_stopfail(&self) {
        fs::write(&self.stopfail, "").expect("the stop marker is writable");
    }

    fn mark_failprune(&self) {
        fs::write(&self.failprune, "").expect("the prune marker is writable");
    }

    fn install_runtime(&self, name: &str) -> anyhow::Result<()> {
        write_script(&self.bin.join(name), &self.runtime_stub(name))?;
        Ok(())
    }

    fn install_pager(&self, name: &str) -> anyhow::Result<()> {
        let log = shell_quote(&self.pager_log);
        write_script(
            &self.bin.join(name),
            &format!(
                "#!/bin/sh\nprintf 'CALL {name}\\n' >> {log}\nprintf 'ARG %s\\n' \"$@\" >> {log}\nprintf 'PAGER-RAN\\n'\n"
            ),
        )?;
        Ok(())
    }

    fn rt_calls(&self) -> Vec<Call> {
        read_calls(&self.rt_log)
    }

    fn rt_names(&self) -> Vec<String> {
        self.rt_calls().iter().map(|c| c.name.clone()).collect()
    }

    fn verbs(&self) -> Vec<String> {
        self.rt_calls()
            .iter()
            .filter(|c| c.args.first().map(String::as_str) != Some("inspect"))
            .map(|c| c.args[0].clone())
            .collect()
    }

    fn rt_run(&self) -> Call {
        self.rt_calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("run"))
            .unwrap_or_else(|| panic!("no `run` call recorded: {:?}", self.rt_calls()))
    }

    fn nix_calls(&self) -> Vec<Call> {
        read_calls(&self.nix_log)
    }

    fn nix_verb(&self, index: usize) -> String {
        self.nix_calls()[index].args[0].clone()
    }

    fn serve(&self, reply: Reply) -> FakeServer {
        let parent = self.socket.parent().expect("the socket has a parent");
        fs::create_dir_all(parent).expect("the fake Server's parent is created");
        let listener = UnixListener::bind(&self.socket).expect("the fake Server binds");
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || serve_loop(listener, reply, thread_stop));
        FakeServer {
            socket: self.socket.clone(),
            stop,
        }
    }

    /// A Server that answers the Version probe with the host's own version.
    fn serve_current(&self) -> FakeServer {
        self.serve(Reply::Version(CURRENT_VERSION.to_string()))
    }

    fn runtime_stub(&self, name: &str) -> String {
        r#"#!/bin/sh
log={log}
state={state}
absent={absent}
runcount={runcount}
runfail={runfail}
rundown={rundown}
runwon={runwon}
stopfail={stopfail}
printf 'CALL {name}\n' >> "$log"
printf 'ARG %s\n' "$@" >> "$log"
case "$1" in
  inspect)
    if [ -f "$absent" ]; then
      printf 'no such container\n' >&2
      exit 1
    fi
    want_json=1
    for a in "$@"; do
      case "$a" in
        *{{*) want_json=0 ;;
      esac
    done
    if [ "$want_json" -eq 1 ]; then
      printf '%s\n' '{"State":{"Running":false,"Status":"exited"}}'
    else
      IFS= read -r line < "$state" || line=false
      printf '%s\n' "$line"
    fi
    ;;
  run)
    n=0
    IFS= read -r n < "$runcount" || n=0
    n=$((n + 1))
    printf '%s\n' "$n" > "$runcount"
    if [ -f "$runfail" ] && [ "$n" -eq 1 ]; then
      IFS= read -r msg < "$runfail" || msg='run failed'
      printf '%s\n' "$msg" >&2
      if [ -f "$runwon" ]; then
        printf 'true\n' > "$state"
      fi
      exit 125
    fi
    if [ ! -f "$rundown" ]; then
      printf 'true\n' > "$state"
    fi
    ;;
  stop)
    if [ -f "$stopfail" ]; then
      printf 'stop failed\n' >&2
      exit 1
    fi
    printf 'false\n' > "$state"
    ;;
esac
exit 0
"#
        .replace("{log}", &shell_quote(&self.rt_log))
        .replace("{state}", &shell_quote(&self.state))
        .replace("{absent}", &shell_quote(&self.absent))
        .replace("{runcount}", &shell_quote(&self.runcount))
        .replace("{runfail}", &shell_quote(&self.runfail))
        .replace("{rundown}", &shell_quote(&self.rundown))
        .replace("{runwon}", &shell_quote(&self.runwon))
        .replace("{stopfail}", &shell_quote(&self.stopfail))
        .replace("{name}", name)
    }

    fn nix_stub(&self) -> String {
        r#"#!/bin/sh
log={log}
failprune={failprune}
printf 'CALL nix\n' >> "$log"
printf 'ARG %s\n' "$@" >> "$log"
if [ "$1" = "print-dev-env" ]; then
  printf 'export NCAP_STUB_DUMP=1\n'
  exit 0
fi
if [ -f "$failprune" ]; then
  printf 'prune failed\n' >&2
  exit 1
fi
exit 0
"#
        .replace("{log}", &shell_quote(&self.nix_log))
        .replace("{failprune}", &shell_quote(&self.failprune))
    }
}

fn rooted_at(name: &str) -> anyhow::Result<World> {
    let mut w = World::new()?;
    w.root = w.temp.path().join(name);
    fs::create_dir_all(&w.root)?;
    Ok(w)
}

#[derive(Debug)]
struct Call {
    name: String,
    args: Vec<String>,
}

fn read_calls(path: &Path) -> Vec<Call> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut calls: Vec<Call> = Vec::new();
    for line in raw.lines() {
        if let Some(name) = line.strip_prefix("CALL ") {
            calls.push(Call {
                name: name.to_string(),
                args: Vec::new(),
            });
        } else if let Some(arg) = line.strip_prefix("ARG ")
            && let Some(call) = calls.last_mut()
        {
            call.args.push(arg.to_string());
        }
    }
    calls
}

enum Reply {
    Version(String),
    Reject,
    Silent,
}

struct FakeServer {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
    }
}

fn serve_loop(listener: UnixListener, reply: Reply, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        let Ok((mut stream, _)) = listener.accept() else {
            continue;
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if !matches!(read_frame(&mut stream), Ok((0x0A, _))) {
            continue;
        }
        match &reply {
            Reply::Version(version) => {
                let payload = format!("{{\"version\":\"{version}\"}}");
                let _ = write_frame(&mut stream, 0x0B, payload.as_bytes());
            }
            Reply::Reject => {
                let _ = write_frame(&mut stream, 0x06, br#"{"message":"unknown frame"}"#);
            }
            Reply::Silent => {
                let mut buf = [0u8; 1];
                let _ = stream.read(&mut buf);
            }
        }
    }
}

fn write_frame(out: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    out.write_all(&[tag])?;
    out.write_all(&(payload.len() as u32).to_be_bytes())?;
    out.write_all(payload)
}

fn read_frame(inp: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    inp.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let mut payload = vec![0u8; len];
    inp.read_exact(&mut payload)?;
    Ok((header[0], payload))
}

struct Tap {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Tap {
    fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().expect("tap mutex").clone()
    }
}

fn tap(child: &mut Child) -> Tap {
    let mut stdout = child.stdout.take().expect("the caller piped stdout");
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let handle = Arc::clone(&bytes);
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => handle
                    .lock()
                    .expect("tap mutex")
                    .extend_from_slice(&buf[..n]),
            }
        }
    });
    Tap { bytes }
}

fn wait_until(tap: &Tap, what: &str, pred: impl Fn(&str) -> bool) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = text(&tap.snapshot());
        if pred(&seen) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow::anyhow!("timed out waiting for {what}; saw: {seen}"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn mounts_and_options(call: &Call) -> Vec<String> {
    let end = call
        .args
        .iter()
        .position(|a| a == "--")
        .expect("the launch template pins `--` before the image");
    call.args[4..end].to_vec()
}

fn launch_tail(call: &Call) -> Vec<String> {
    let start = call
        .args
        .iter()
        .position(|a| a == "--")
        .expect("the launch template pins `--` before the image")
        + 1;
    call.args[start..].to_vec()
}

fn arg_for<'a>(opts: &'a [String], source: &str) -> &'a str {
    opts.iter()
        .find(|a| a.contains(source))
        .map(String::as_str)
        .unwrap_or_else(|| panic!("no argument mentions `{source}`: {opts:#?}"))
}

/// The bind sources in the order the Runtime adapter's argv carries them,
/// independent of the flag rendering: an argument is the bind that mentions
/// its source. `root` is skipped after `-w`, where the same path is the
/// workdir rather than a bind.
fn bind_sources<'a>(opts: &[String], sources: &[&'a str]) -> Vec<&'a str> {
    let mut listed: Vec<&str> = Vec::new();
    let mut args = opts.iter();
    while let Some(arg) = args.next() {
        if arg == "-w" {
            args.next();
            continue;
        }
        for source in sources {
            if arg.contains(source) && !listed.contains(source) {
                listed.push(source);
            }
        }
    }
    listed
}

/// Read-only-ness of one mount argument as a delimited `ro`/`readonly`
/// token, so a path that happens to carry those letters cannot decide it.
fn mount_is_read_only(arg: &str) -> bool {
    arg.split([':', ',', '='])
        .any(|token| token == "ro" || token == "readonly")
}
