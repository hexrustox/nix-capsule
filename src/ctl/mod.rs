//! Ctl (`ncap-ctl`): the project's lifecycle brain on the host — config from
//! the `NCAP_*` contract, freshness, stamp guard, and the container flows.

pub mod cli;
pub mod config;
pub mod digest;
pub(crate) mod fs_error;
pub(crate) mod nix;
pub(crate) mod paths;
pub(crate) mod runtime;
pub(crate) mod shell;
pub(crate) mod stamp;
pub(crate) mod version;

use std::fs;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::ctl::config::{Cmd, Config, ConfigError};
use crate::ctl::fs_error::FsError;
use crate::ctl::runtime::RuntimeError;
use crate::ctl::stamp::StampError;
use crate::ctl::version::warn_on_skew;

/// Entry point from the binary: dispatch `cmd` after resolving the config
/// from the process environment. Returns the message to print on stderr, if
/// any (unprefixed, may be multi-line).
pub async fn run(cmd: Cmd) -> Option<String> {
    let lookup = |var: &str| std::env::var(var).ok();
    // `setup-env` resolves the derived vars itself, so it must run before
    // the full demand set — a full resolve would reject the empty values it
    // is meant to fill.
    if cmd == Cmd::SetupEnv {
        return match config::setup_env(&lookup) {
            Ok(script) => {
                print!("{script}");
                None
            }
            Err(err) => fail(err.into()),
        };
    }
    let cfg = match config::resolve(&lookup) {
        Ok(cfg) => cfg,
        Err(err) => return fail(err.into()),
    };
    let result = match cmd {
        Cmd::Init => init(cfg).await,
        Cmd::Start => start(cfg).await,
        Cmd::Stop => stop(cfg).await,
        Cmd::Restart => restart(cfg).await,
        Cmd::Status => status(cfg).await,
        Cmd::Enter => enter(cfg).await,
        Cmd::Log => log(cfg).await,
        Cmd::Clean => clean(cfg).await,
        Cmd::ShowOptions => show_options(cfg).await,
        Cmd::SetupEnv => unreachable!("handled before resolve"),
    };
    match result {
        Ok(()) => None,
        Err(err) => fail(err),
    }
}

/// Failures of the ctl flows: context variants name the failed operation and
/// its object with the raw cause riding as `#[source]`; sub-module errors and
/// the shared io variants pass through transparent.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CtlError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Stamp(#[from] StampError),
    #[error(transparent)]
    PrintDevEnv(#[from] crate::ctl::nix::PrintDevEnvError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Fs(#[from] FsError),
    #[error("cannot hash the watched files: {source}")]
    Digest {
        #[source]
        source: io::Error,
    },
    #[error("socket path `{socket}` has no parent directory")]
    SocketNoParent { socket: String },
    #[error("no `ncap-server-*.log` file in `{dir}`")]
    NoLog { dir: String },
    #[error("cannot run pager `{prog}`: {source}")]
    PagerSpawn {
        prog: String,
        #[source]
        source: io::Error,
    },
    #[error("pager `{prog}` failed")]
    PagerFailed { prog: String },
    #[error("referenced unset variable `{name}`")]
    UnsetVar { name: String },
    #[error("container `{container}` is not running")]
    NotRunning { container: String },
    #[error("no cached dev environment in `{dir}`")]
    NoCachedEnv { dir: String },
}

/// Render `err` without the program prefix — the binary applies it — plus,
/// when the variant carries it, prescriptive advice as a sibling line.
fn fail(err: CtlError) -> Option<String> {
    let message = err.to_string();
    let message = if let Some(advice) = match &err {
        CtlError::NotRunning { .. } | CtlError::NoCachedEnv { .. } => {
            Some("run `ncap-ctl init` to start this project's container")
        }
        CtlError::Config(ConfigError::EmptyProjectName { .. })
        | CtlError::Stamp(StampError::AlreadyClaimed { .. }) => {
            Some("set `project` to a value mapping to a unique root")
        }
        CtlError::Config(ConfigError::NoRuntime) => {
            Some("install `podman` or `docker`, or set `NCAP_RUNTIME` to an installed runtime")
        }
        _ => None,
    } {
        format!("{message}\n{advice}")
    } else {
        message
    };
    Some(message)
}

// ---------------------------------------------------------------------------
// Flows
// ---------------------------------------------------------------------------

async fn init(cfg: Config) -> Result<(), CtlError> {
    let root = &cfg.root;
    let cache_dir = &cfg.cache_dir;
    let project = &cfg.project;
    stamp::guard(cache_dir, project, root)?;

    let rt = cfg.runtime();
    let socket = &cfg.socket;
    let live = rt.is_live(socket).await;
    let freshness = digest::check(cache_dir, root, &cfg.watch_files);

    match (live, freshness) {
        (true, digest::Freshness::Fresh) => Ok(()),
        (true, _) => {
            ensure_cache(&cfg).await?;
            // Non-fatal stop.
            let _ = rt.stop().await;
            start_inner(&cfg).await
        }
        (false, _) => {
            ensure_cache(&cfg).await?;
            start_inner(&cfg).await
        }
    }
}

async fn start(cfg: Config) -> Result<(), CtlError> {
    let root = &cfg.root;
    let cache_dir = &cfg.cache_dir;
    let project = &cfg.project;
    stamp::guard(cache_dir, project, root)?;

    let rt = cfg.runtime();
    let socket = &cfg.socket;
    if rt.is_live(socket).await {
        // Live-done branch: the Version probe still runs.
        warn_on_skew(socket).await;
        return Ok(());
    }
    start_inner(&cfg).await?;
    // Fresh readiness reached: probe the Server the launch just started.
    warn_on_skew(socket).await;
    Ok(())
}

async fn stop(cfg: Config) -> Result<(), CtlError> {
    let rt = cfg.runtime();
    if !rt.is_running().await {
        return Ok(());
    }
    match rt.stop().await {
        Ok(_) => Ok(()),
        Err(err) => {
            // If stop failed but the container is now not-running, treat as
            // success (idempotent).
            if !rt.is_running().await {
                Ok(())
            } else {
                Err(err.into())
            }
        }
    }
}

async fn restart(cfg: Config) -> Result<(), CtlError> {
    // Resolution is uniform, so the Restart cfg already carries Init's
    // fields — dispatch directly.
    let rt = cfg.runtime();
    let _ = rt.stop().await;
    init(cfg).await
}

async fn status(cfg: Config) -> Result<(), CtlError> {
    let rt = cfg.runtime();
    let running = rt.is_running().await;

    let socket_connectable = tokio::net::UnixStream::connect(&cfg.socket).await.is_ok();
    // After liveness/connectability: one Version probe; skew warns here.
    if socket_connectable {
        warn_on_skew(&cfg.socket).await;
    }

    let cache_status = match digest::check(&cfg.cache_dir, &cfg.root, &cfg.watch_files) {
        digest::Freshness::Fresh => "fresh",
        digest::Freshness::Stale => "stale",
        digest::Freshness::Missing => "missing",
    };

    if running {
        println!("container: running ({})", cfg.container);
    } else {
        println!("container: not running");
    }
    if socket_connectable {
        println!("socket: connectable ({})", cfg.socket.display());
    } else {
        println!("socket: unreachable ({})", cfg.socket.display());
    }
    println!("cache: {cache_status}");
    Ok(())
}

async fn enter(cfg: Config) -> Result<(), CtlError> {
    let cache_dir = &cfg.cache_dir;
    let rt = cfg.runtime();
    if !rt.is_running().await {
        return Err(CtlError::NotRunning {
            container: cfg.container.clone(),
        });
    }
    rt.exec_interactive(cache_dir).await?;
    Ok(())
}

async fn log(cfg: Config) -> Result<(), CtlError> {
    let log_dir = &cfg.log_dir;
    let newest = paths::newest_server_log_path(log_dir).ok_or_else(|| CtlError::NoLog {
        dir: log_dir.display().to_string(),
    })?;
    let (prog, args) = pager_command();
    let mut cmd = tokio::process::Command::new(&prog);
    cmd.args(&args);
    cmd.arg(&newest);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    let status = cmd.status().await.map_err(|source| CtlError::PagerSpawn {
        prog: prog.clone(),
        source,
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CtlError::PagerFailed { prog })
    }
}

async fn clean(cfg: Config) -> Result<(), CtlError> {
    let rt = cfg.runtime();
    // Stop the container (best-effort, idempotent) then remove it.
    if rt.is_running().await {
        let _ = rt.stop().await;
    }
    let _ = rt.remove().await;

    // Remove only the project's own files: the four named cache files, the
    // profile generation links `nix print-dev-env` leaves beside the
    // profile, and the `ncap-server-*.log` files. Never wipe the whole
    // cache/log dir contents: an explicit `NCAP_CACHE_DIR`/`NCAP_LOG_DIR`
    // may point into a shared dir, where a recursive delete would destroy
    // unrelated files — the same rationale as the socket parent below.
    clean_cache_dir(&cfg.cache_dir)?;
    clean_log_dir(&cfg.log_dir)?;
    if let Some(parent) = cfg.socket.parent() {
        // Delete the socket file itself, then best-effort remove the parent
        // dir if empty. Never `remove_dir_all` the parent: an explicit
        // NCAP_SOCKET may point into a shared dir (e.g. `/tmp/x.sock`),
        // where a recursive delete would destroy unrelated files. Parent
        // removal failure is non-fatal (non-empty, permission, ...).
        match fs::remove_file(&cfg.socket) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(FsError::Remove {
                    path: cfg.socket.display().to_string(),
                    source: err,
                }
                .into());
            }
        }
        let _ = fs::remove_dir(parent);
    }

    Ok(())
}

async fn show_options(cfg: Config) -> Result<(), CtlError> {
    for opt in &cfg.run_opts {
        let expanded = expand_one(opt)?;
        println!("{expanded}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers shared by init/start
// ---------------------------------------------------------------------------

async fn ensure_cache(cfg: &Config) -> Result<(), CtlError> {
    let root = &cfg.root;
    let cache_dir = &cfg.cache_dir;
    let freshness = digest::check(cache_dir, root, &cfg.watch_files);
    if freshness == digest::Freshness::Fresh {
        return Ok(());
    }
    let nix_bin = &cfg.nix;
    let devshell = &cfg.devshell;
    let profile = paths::profile_file(cache_dir);

    fs::create_dir_all(cache_dir).map_err(|source| FsError::CreateDir {
        dir: cache_dir.display().to_string(),
        source,
    })?;

    let output = nix::print_dev_env(nix_bin, &profile, devshell).await?;

    let env_path = paths::env_file(cache_dir);
    fs::write(&env_path, &output).map_err(|source| FsError::Write {
        path: env_path.display().to_string(),
        source,
    })?;

    // Prune profile history; non-fatal.
    nix::wipe_history(nix_bin, &profile).await;

    let digest_hex =
        digest::compute(root, &cfg.watch_files).map_err(|source| CtlError::Digest { source })?;
    digest::store(cache_dir, &digest_hex).map_err(|source| FsError::Write {
        path: paths::hash_file(cache_dir).display().to_string(),
        source,
    })?;
    Ok(())
}

async fn start_inner(cfg: &Config) -> Result<(), CtlError> {
    let cache_dir = &cfg.cache_dir;
    let socket = &cfg.socket;
    let log_dir = &cfg.log_dir;
    let server = &cfg.server;
    let image = &cfg.image;

    // The env dump must exist — otherwise the container cannot source it.
    if !paths::env_file(cache_dir).is_file() {
        return Err(CtlError::NoCachedEnv {
            dir: cache_dir.display().to_string(),
        });
    }

    if let Some(parent) = socket.parent() {
        paths::ensure_dir_0700(parent).map_err(|source| FsError::CreateDir {
            dir: parent.display().to_string(),
            source,
        })?;
    }
    fs::create_dir_all(log_dir).map_err(|source| FsError::CreateDir {
        dir: log_dir.display().to_string(),
        source,
    })?;

    // Assemble mount set and options. Expansion errors are fatal before the
    // runtime is ever invoked, naming the unset variable.
    let mount_args = build_runtime_args(cfg)?;

    let rt = cfg.runtime();

    let server_args = runtime::ServerArgs {
        socket: socket.clone(),
        log_dir: log_dir.clone(),
        timeout: cfg.timeout,
        log_level: cfg.log_level,
    };

    // A container with the target name that exists but is not running is
    // removed before launch (spec/ctl.md § start flow).
    if !rt.is_running().await && rt.exists().await {
        let _ = rt.remove().await;
    }

    let run_result = rt
        .run_detached(
            image,
            &paths::env_file(cache_dir),
            server,
            &server_args,
            &mount_args,
        )
        .await;

    match run_result {
        Ok(_) => {}
        Err(RuntimeError::Failed {
            output,
            verb: "run",
            ..
        }) if runtime::is_name_in_use(&output) => {
            // Concurrent-start race: re-inspect.
            if rt.is_running().await {
                return Ok(());
            }
            let _ = rt.remove().await;
            rt.run_detached(
                image,
                &paths::env_file(cache_dir),
                server,
                &server_args,
                &mount_args,
            )
            .await?;
        }
        Err(err) => return Err(err.into()),
    };

    let deadline = Instant::now() + Duration::from_secs(cfg.timeout);
    loop {
        if rt.is_live(socket).await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let state = rt.inspect_state().await;
    Err(CtlError::Runtime(RuntimeError::NotLive {
        container: cfg.container.clone(),
        timeout: cfg.timeout,
        state,
    }))
}

/// The pager from `PAGER`, split into program and args for `Command`;
/// `less -R` when `PAGER` is unset or names nothing.
fn pager_command() -> (String, Vec<String>) {
    if let Ok(pager) = std::env::var("PAGER") {
        let mut parts = pager.split_whitespace().map(str::to_owned);
        if let Some(prog) = parts.next() {
            return (prog, parts.collect());
        }
    }
    ("less".to_owned(), vec!["-R".to_owned()])
}

/// Remove the project's cache files: the four named files of the cache
/// layout (`env`, `hash`, `profile`, `project`) plus the profile generation
/// links (`profile-<N>-link`) that `nix print-dev-env` creates beside the
/// profile. Anything else in the dir is left untouched. Then best-effort
/// remove the dir itself when empty.
fn clean_cache_dir(cache_dir: &Path) -> Result<(), CtlError> {
    for file in [
        paths::env_file(cache_dir),
        paths::hash_file(cache_dir),
        paths::profile_file(cache_dir),
        paths::project_stamp_file(cache_dir),
    ] {
        remove_file_if_exists(&file)?;
    }
    remove_profile_generation_links(cache_dir)?;
    // Non-fatal: non-empty (foreign files), permission, ...
    let _ = fs::remove_dir(cache_dir);
    Ok(())
}

/// Remove every `ncap-server-<digits>.log` file in the log dir, leaving any
/// other entry untouched. Then best-effort remove the dir itself when empty.
fn clean_log_dir(log_dir: &Path) -> Result<(), CtlError> {
    let entries = match fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(FsError::ReadDir {
                dir: log_dir.display().to_string(),
                source: err,
            }
            .into());
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| FsError::ReadDir {
            dir: log_dir.display().to_string(),
            source,
        })?;
        let name = entry.file_name();
        if paths::parse_server_log_epoch(&name.to_string_lossy()).is_some() {
            remove_file_if_exists(&entry.path())?;
        }
    }
    // Non-fatal: non-empty (foreign files), permission, ...
    let _ = fs::remove_dir(log_dir);
    Ok(())
}

/// Remove every `profile-<N>-link` entry in `cache_dir`, the generation
/// links `nix print-dev-env` maintains for the profile. A symlink is removed
/// itself, never followed (`symlink_metadata`); only a real directory
/// recurses.
fn remove_profile_generation_links(cache_dir: &Path) -> Result<(), CtlError> {
    let entries = match fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(FsError::ReadDir {
                dir: cache_dir.display().to_string(),
                source: err,
            }
            .into());
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| FsError::ReadDir {
            dir: cache_dir.display().to_string(),
            source,
        })?;
        let name = entry.file_name();
        let is_link = name
            .to_string_lossy()
            .strip_prefix("profile-")
            .is_some_and(|rest| {
                rest.strip_suffix("-link").is_some_and(|digits| {
                    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
                })
            });
        if !is_link {
            continue;
        }
        let path = entry.path();
        let file_type = fs::symlink_metadata(&path);
        let file_type = file_type.map_err(|source| FsError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let removed = if file_type.is_dir() && !file_type.is_symlink() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removed.map_err(|source| FsError::Remove {
            path: path.display().to_string(),
            source,
        })?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<(), CtlError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(FsError::Remove {
            path: path.display().to_string(),
            source: err,
        }
        .into()),
    }
}

fn build_runtime_args(cfg: &Config) -> Result<Vec<String>, CtlError> {
    let root = &cfg.root;
    let socket = &cfg.socket;
    let cache_dir = &cfg.cache_dir;
    let log_dir = &cfg.log_dir;
    let socket_dir = socket.parent().ok_or_else(|| CtlError::SocketNoParent {
        socket: socket.display().to_string(),
    })?;

    let mut args = Vec::new();

    if cfg.harden {
        args.push("--cap-drop=all".to_owned());
        args.push("--security-opt=no-new-privileges".to_owned());
    }

    args.push("-v".to_owned());
    args.push("/nix:/nix:ro".to_owned());
    args.push("-v".to_owned());
    args.push(format!("{}:{}", socket_dir.display(), socket_dir.display()));
    args.push("-v".to_owned());
    args.push(format!("{}:{}", root.display(), root.display()));
    args.push("-w".to_owned());
    args.push(root.display().to_string());
    args.push("-v".to_owned());
    args.push(format!(
        "{}:{}:ro",
        cache_dir.display(),
        cache_dir.display()
    ));
    args.push("-v".to_owned());
    args.push(format!("{}:{}", log_dir.display(), log_dir.display()));

    let git_path = root.join(".git");
    // Only a real directory mounts: worktree gitfiles (plain files) and
    // symlinks (even to dirs) do not. `symlink_metadata` so a symlink is
    // judged itself, never followed.
    if fs::symlink_metadata(&git_path)
        .is_ok_and(|meta| meta.file_type().is_dir() && !meta.file_type().is_symlink())
    {
        args.push("-v".to_owned());
        args.push(format!("{}:{}:ro", git_path.display(), git_path.display()));
    }

    if cfg.harden {
        for entry in &cfg.watch_files {
            let src = root.join(entry);
            if src.exists() {
                args.push("-v".to_owned());
                args.push(format!("{}:{}:ro", src.display(), src.display()));
            }
        }
    }

    for opt in &cfg.run_opts {
        let expanded = expand_one(opt)?;
        args.push(expanded);
    }

    Ok(args)
}

fn expand_one(input: &str) -> Result<String, CtlError> {
    expand_with(input, &|name| std::env::var(name).ok())
}

fn expand_with(input: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, CtlError> {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            expand_braced(&mut chars, &mut out, lookup)?;
        } else if matches!(chars.peek(), Some(ch) if ch.is_ascii_alphabetic() || *ch == '_') {
            expand_unbraced(&mut chars, &mut out, lookup)?;
        } else {
            out.push('$');
        }
    }
    Ok(out)
}

/// Expand one `${...}` form: its `$` and `{` already consumed. A non-name
/// body (`${5}`, `${foo-bar}`) stays literal per the name regex; a missing
/// closing brace leaves the prefix literal.
fn expand_braced(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), CtlError> {
    let mut name = String::new();
    while let Some(&ch) = chars.peek() {
        if ch == '}' {
            break;
        }
        name.push(ch);
        chars.next();
    }
    if chars.next().is_none() {
        out.push_str("${");
        out.push_str(&name);
        return Ok(());
    }
    if name.is_empty() {
        out.push_str("${}");
        return Ok(());
    }
    if !is_env_name(&name) {
        out.push_str("${");
        out.push_str(&name);
        out.push('}');
        return Ok(());
    }
    match lookup(&name) {
        Some(val) => {
            out.push_str(&val);
            Ok(())
        }
        None => Err(CtlError::UnsetVar { name }),
    }
}

/// Expand one `$NAME` form: its `$` and first name char already consumed,
/// so `NAME` is appended until a non-`[A-Za-z0-9_]` char.
fn expand_unbraced(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), CtlError> {
    let mut name = String::new();
    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            name.push(ch);
            chars.next();
        } else {
            break;
        }
    }
    match lookup(&name) {
        Some(val) => {
            out.push_str(&val);
            Ok(())
        }
        None => Err(CtlError::UnsetVar { name }),
    }
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    /// A lookup over literal pairs, standing in for the process environment.
    /// Absent names yield `None`, the same signal `expand_one` gets from
    /// `std::env::var(..).ok()`.
    fn lookup_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test_case("prefix-$A-suffix", &[("A", "hello")], "prefix-hello-suffix" ; "dollar_var_mid_string")]
    #[test_case("a-${A}-b", &[("A", "world")], "a-world-b" ; "braced_var_mid_string")]
    #[test_case("$A:$A", &[("A", "/tmp/cargo")], "/tmp/cargo:/tmp/cargo" ; "dollar_var_repeats")]
    #[test_case("${A}:${A}", &[("A", "/tmp/cargo")], "/tmp/cargo:/tmp/cargo" ; "braced_var_repeats")]
    #[test_case("a-$A-b", &[("A", "")], "a--b" ; "empty_value_is_empty_not_error")]
    #[test_case("a-${A}-b", &[("A", "")], "a--b" ; "braced_empty_value_is_empty_not_error")]
    #[test_case("-v $A:/mnt", &[("A", "/tmp/foo bar")], "-v /tmp/foo bar:/mnt" ; "space_value_stays_one_arg")]
    fn expands_set_vars(template: &str, env: &[(&str, &str)], want: &str) {
        let out = expand_with(template, &lookup_of(env)).expect("expand succeeds");
        assert_eq!(out, want, "template={template}");
    }

    #[test_case("literal-no-dollar" ; "plain_text")]
    #[test_case("price $ 5" ; "lone_dollar_stays")]
    #[test_case("a-$5b" ; "dollar_digit_is_not_a_name")]
    #[test_case("${5}" ; "braced_digit_stays_literal")]
    #[test_case("${foo-bar}" ; "braced_dash_stays_literal")]
    #[test_case("${foo bar}" ; "braced_space_stays_literal")]
    #[test_case("${}" ; "braced_empty_stays_literal")]
    #[test_case("$" ; "bare_dollar")]
    #[test_case("$$" ; "double_dollar")]
    #[test_case("$-x" ; "dollar_dash_is_not_a_name")]
    #[test_case("${5-NC AP}" ; "invalid_name_never_consults_env")]
    fn leaves_non_names_literal(input: &str) {
        // Invalid braced names never consult the environment and never error,
        // even when the text inside names a variable set to another value.
        let out =
            expand_with(input, &lookup_of(&[("5", "set"), ("foo", "set")])).expect("literal stays");
        assert_eq!(out, input);
    }

    #[test_case("x-$UNSET-y" ; "dollar_unset_names_var")]
    #[test_case("x-${UNSET}-y" ; "braced_unset_names_var")]
    #[test_case("${UNSET}" ; "braced_alone_names_var")]
    fn unset_var_is_an_error_naming_it(template: &str) {
        let err = expand_with(template, &lookup_of(&[])).expect_err("unset must error");
        assert!(
            matches!(err, CtlError::UnsetVar { ref name } if name == "UNSET"),
            "err={err}"
        );
    }
}
