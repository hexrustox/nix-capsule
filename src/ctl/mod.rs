//! Ctl (`ncap-ctl`): the project's lifecycle brain on the host — config from
//! the `NCAP_*` contract, freshness, stamp guard, and the container flows.

pub mod config;
pub mod digest;
pub mod nix;
pub mod paths;
pub mod runtime;
pub mod stamp;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::net::UnixStream;

use config::{Cmd, Config};

/// Entry point from the binary: resolve `cmd` from the process environment and
/// dispatch. Returns the exit code the process should report.
pub async fn run(cmd: Cmd) -> i32 {
    let lookup = |var: &str| std::env::var(var).ok();
    let cfg = match config::resolve(&lookup) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("ncap-ctl: {err}");
            return 1;
        }
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
    };
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("ncap-ctl: {err}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// Flows
// ---------------------------------------------------------------------------

async fn init(cfg: Config) -> Result<(), String> {
    // Stamp guard first.
    let root = &cfg.root;
    let cache_dir = &cfg.cache_dir;
    let project = &cfg.project;
    stamp::guard(cache_dir, project, root).map_err(|err| err.to_string())?;

    let rt = runtime::Runtime::new(cfg.runtime.clone());
    let socket = &cfg.socket;
    let live = rt.is_live(&cfg.container, socket).await;
    let freshness = digest::check(cache_dir, root, &cfg.watch_files);

    match (live, freshness) {
        (true, digest::Freshness::Fresh) => {
            eprintln!("container `{}` is already running and fresh", cfg.container);
            Ok(())
        }
        (true, _) => {
            // Running but stale/missing → re-eval + restart.
            ensure_cache(&cfg).await?;
            // Non-fatal stop.
            let _ = rt.stop(&cfg.container).await;
            start_inner(&cfg).await
        }
        (false, _) => {
            ensure_cache(&cfg).await?;
            start_inner(&cfg).await
        }
    }
}

async fn start(cfg: Config) -> Result<(), String> {
    let root = &cfg.root;
    let cache_dir = &cfg.cache_dir;
    let project = &cfg.project;
    stamp::guard(cache_dir, project, root).map_err(|err| err.to_string())?;

    let rt = runtime::Runtime::new(cfg.runtime.clone());
    let socket = &cfg.socket;
    if rt.is_live(&cfg.container, socket).await {
        eprintln!("container `{}` is already running", cfg.container);
        return Ok(());
    }
    start_inner(&cfg).await
}

async fn stop(cfg: Config) -> Result<(), String> {
    let rt = runtime::Runtime::new(cfg.runtime.clone());
    if !rt.is_running(&cfg.container).await {
        eprintln!("container `{}` is not running", cfg.container);
        return Ok(());
    }
    match rt.stop(&cfg.container).await {
        Ok(_) => {
            eprintln!("container `{}` stopped", cfg.container);
            Ok(())
        }
        Err(stderr) => {
            // If stop failed but the container is now not-running, treat as
            // success (idempotent).
            if !rt.is_running(&cfg.container).await {
                eprintln!("container `{}` is not running", cfg.container);
                Ok(())
            } else {
                Err(stderr)
            }
        }
    }
}

async fn restart(cfg: Config) -> Result<(), String> {
    // Non-fatal stop, then init. Resolution is uniform, so the Restart cfg
    // already carries Init's fields — dispatch directly.
    let rt = runtime::Runtime::new(cfg.runtime.clone());
    let _ = rt.stop(&cfg.container).await;
    init(cfg).await
}

async fn status(cfg: Config) -> Result<(), String> {
    let rt = runtime::Runtime::new(cfg.runtime.clone());
    let running = rt.is_running(&cfg.container).await;

    let socket_connectable = UnixStream::connect(&cfg.socket).await.is_ok();

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

async fn enter(cfg: Config) -> Result<(), String> {
    let cache_dir = &cfg.cache_dir;
    let bash = &cfg.bash;
    if !cache_dir.join("env").is_file() {
        return Err("no cached dev environment found; run `ncap-ctl init` first".to_owned());
    }
    let rt = runtime::Runtime::new(cfg.runtime.clone());
    if !rt.is_running(&cfg.container).await {
        return Err(format!(
            "container `{}` is not running; run `ncap-ctl init` to start it",
            cfg.container
        ));
    }
    rt.exec_interactive(&cfg.container, bash, cache_dir).await
}

async fn log(cfg: Config) -> Result<(), String> {
    let log_dir = &cfg.log_dir;
    let newest =
        newest_log_path(log_dir).ok_or_else(|| format!("no log file in {}", log_dir.display()))?;
    let (prog, args) = pager_command();
    let mut cmd = tokio::process::Command::new(&prog);
    cmd.args(&args);
    cmd.arg(&newest);
    cmd.stdin(std::process::Stdio::inherit());
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());
    let status = cmd.status().await.map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        match status.code() {
            Some(code) => Err(format!("pager `{prog}` exited with status {code}")),
            None => Err(format!("pager `{prog}` terminated by signal")),
        }
    }
}

async fn clean(cfg: Config) -> Result<(), String> {
    let rt = runtime::Runtime::new(cfg.runtime.clone());
    // Stop the container (best-effort, idempotent) then remove it.
    if rt.is_running(&cfg.container).await {
        let _ = rt.stop(&cfg.container).await;
    }
    let _ = rt.remove(&cfg.container).await;

    // Clear cache/log contents entry-by-entry, then best-effort remove the
    // dirs themselves when empty. Never `remove_dir_all` the top dirs: an
    // explicit `NCAP_CACHE_DIR`/`NCAP_LOG_DIR` may point into a shared dir.
    remove_dir_contents(&cfg.cache_dir)?;
    remove_dir_contents(&cfg.log_dir)?;
    if let Some(parent) = cfg.socket.parent() {
        // Delete the socket file itself, then best-effort remove the parent
        // dir if empty. Never `remove_dir_all` the parent: an explicit
        // NCAP_SOCKET may point into a shared dir (e.g. `/tmp/x.sock`),
        // where a recursive delete would destroy unrelated files. Parent
        // removal failure is non-fatal (non-empty, permission, ...).
        match fs::remove_file(&cfg.socket) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.to_string()),
        }
        let _ = fs::remove_dir(parent);
    }
    eprintln!("cleaned project `{}`", cfg.project);
    Ok(())
}

async fn show_options(cfg: Config) -> Result<(), String> {
    for opt in &cfg.run_opts {
        let expanded = expand_one(opt)?;
        println!("{expanded}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers shared by init/start
// ---------------------------------------------------------------------------

async fn ensure_cache(cfg: &Config) -> Result<(), String> {
    let root = &cfg.root;
    let cache_dir = &cfg.cache_dir;
    let freshness = digest::check(cache_dir, root, &cfg.watch_files);
    if freshness == digest::Freshness::Fresh {
        return Ok(());
    }
    // Stale or missing → eval.
    let nix_bin = &cfg.nix;
    let devshell = &cfg.devshell;
    let profile = cache_dir.join("profile");

    fs::create_dir_all(cache_dir).map_err(|err| err.to_string())?;

    eprintln!("evaluating devshell `{devshell}` with nix print-dev-env...");
    let output = nix::print_dev_env(nix_bin, &profile, devshell)
        .await
        .map_err(|err| format!("nix print-dev-env failed: {err}"))?;

    let env_file = digest::env_file(cache_dir);
    fs::write(&env_file, &output).map_err(|err| err.to_string())?;
    eprintln!("devshell cached");

    // Prune profile history; non-fatal.
    let _ = nix::wipe_history(nix_bin, &profile).await;

    let digest_hex = digest::of(root, &cfg.watch_files).map_err(|err| err.to_string())?;
    digest::store(cache_dir, &digest_hex).map_err(|err| err.to_string())?;
    Ok(())
}

async fn start_inner(cfg: &Config) -> Result<(), String> {
    let cache_dir = &cfg.cache_dir;
    let socket = &cfg.socket;
    let log_dir = &cfg.log_dir;
    let server = &cfg.server;
    let bash = &cfg.bash;
    let image = &cfg.image;

    // The env dump must exist — otherwise the container cannot source it.
    if !cache_dir.join("env").is_file() {
        return Err("no cached dev environment found; run `ncap-ctl init` first".to_owned());
    }

    // Ensure the socket's parent dir exists with 0700.
    if let Some(parent) = socket.parent() {
        paths::ensure_dir_0700(parent).map_err(|err| err.to_string())?;
    }
    fs::create_dir_all(log_dir).map_err(|err| err.to_string())?;

    let exec_cmd = format!(
        "source {} && exec {} --socket {} --log-dir {} --timeout {}",
        cache_dir.join("env").display(),
        server.display(),
        socket.display(),
        log_dir.display(),
        cfg.timeout
    );

    // Assemble mount set and options. Expansion errors are fatal before the
    // runtime is ever invoked, naming the unset variable.
    let mount_args = build_runtime_args(cfg)?;

    let rt = runtime::Runtime::new(cfg.runtime.clone());

    // A container with the target name that exists but is not running is
    // removed before launch (spec/ctl.md § start flow).
    if !rt.is_running(&cfg.container).await && rt.exists(&cfg.container).await {
        let _ = rt.remove(&cfg.container).await;
    }

    let run_result = rt
        .run_detached(&cfg.container, image, bash, &exec_cmd, &mount_args)
        .await;

    match run_result {
        Ok(_) => {}
        Err(stderr) if runtime::is_name_in_use(&stderr) => {
            // Concurrent-start race: re-inspect.
            if rt.is_running(&cfg.container).await {
                eprintln!("container `{}` is already running", cfg.container);
                return Ok(());
            }
            // Dead container with the same name — remove and retry once.
            let _ = rt.remove(&cfg.container).await;
            match rt
                .run_detached(&cfg.container, image, bash, &exec_cmd, &mount_args)
                .await
            {
                Ok(_) => {}
                Err(stderr) => return Err(format!("{} run failed: {stderr}", rt.bin())),
            }
        }
        Err(stderr) => {
            return Err(format!("{} run failed: {stderr}", rt.bin()));
        }
    };

    // Poll the liveness predicate until live within the deadline.
    let deadline = Instant::now() + Duration::from_secs(cfg.timeout);
    loop {
        if rt.is_live(&cfg.container, socket).await {
            eprintln!("container `{}` is running", cfg.container);
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let state = rt.inspect_state(&cfg.container).await;
    Err(format!(
        "container `{}` never became live within {}s (state: {state})",
        cfg.container, cfg.timeout
    ))
}

// TODO sync with server
fn parse_log_epoch(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("ncap-server-")?;
    let epoch = rest.strip_suffix(".log")?;
    epoch.parse().ok()
}

fn newest_log_path(log_dir: &Path) -> Option<PathBuf> {
    let dir = fs::read_dir(log_dir).ok()?;
    let mut entries: Vec<(u64, PathBuf)> = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(epoch) = parse_log_epoch(&name) {
            entries.push((epoch, entry.path()));
        }
    }
    entries.sort_by_key(|(epoch, _)| *epoch);
    entries.pop().map(|(_, path)| path)
}

fn pager_command() -> (String, Vec<String>) {
    if let Ok(pager) = std::env::var("PAGER") {
        let trimmed = pager.trim();
        if !trimmed.is_empty() {
            let parts: Vec<String> = trimmed.split_whitespace().map(|s| s.to_owned()).collect();
            if !parts.is_empty() {
                return (parts[0].clone(), parts[1..].to_vec());
            }
        }
    }
    ("less".to_owned(), vec!["-R".to_owned()])
}

/// Clear the entries inside `dir`, then best-effort remove `dir` itself when
/// empty. Never `remove_dir_all` the top dir: an explicit `NCAP_CACHE_DIR`
/// or `NCAP_LOG_DIR` may point into a shared dir, where a recursive delete
/// would destroy unrelated files — the same rationale as the socket parent
/// in `clean`. Removal failure of the (now-empty) dir itself is non-fatal.
fn remove_dir_contents(dir: &Path) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        // `symlink_metadata` so a symlink inside is removed itself, never
        // followed; only real directories recurse (one level, on the child).
        let file_type = fs::symlink_metadata(&path)
            .map_err(|err| err.to_string())?
            .file_type();
        if file_type.is_dir() && !file_type.is_symlink() {
            fs::remove_dir_all(&path).map_err(|err| err.to_string())?;
        } else {
            fs::remove_file(&path).map_err(|err| err.to_string())?;
        }
    }
    let _ = fs::remove_dir(dir);
    Ok(())
}

fn build_runtime_args(cfg: &Config) -> Result<Vec<String>, String> {
    let root = &cfg.root;
    let socket = &cfg.socket;
    let cache_dir = &cfg.cache_dir;
    let log_dir = &cfg.log_dir;
    let socket_dir = socket
        .parent()
        .ok_or_else(|| format!("socket path `{}` has no parent directory", socket.display()))?;

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
    // Worktree gitfiles are files, not dirs — mount whenever the path exists.
    if std::fs::symlink_metadata(&git_path).is_ok() {
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

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn expand_one(input: &str) -> Result<String, String> {
    expand_with(input, &|name| std::env::var(name).ok())
}

fn expand_with(input: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            if matches!(chars.peek(), Some('{')) {
                chars.next();
                let mut name = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch == '}' {
                        break;
                    }
                    name.push(ch);
                    chars.next();
                }
                let closed = chars.next();
                if closed != Some('}') {
                    // No closing brace — treat as literal.
                    out.push_str(&format!("${{{name}"));
                    if let Some(ch) = closed {
                        out.push(ch);
                    }
                    continue;
                }
                if name.is_empty() {
                    out.push_str("${}");
                    continue;
                }
                // Only ${NAME} expands; anything else stays literal per the
                // name regex (e.g. ${5}, ${foo-bar}).
                if !is_env_name(&name) {
                    out.push_str(&format!("${{{name}}}"));
                    continue;
                }
                match lookup(&name) {
                    Some(val) => out.push_str(&val),
                    None => {
                        return Err(format!("referenced unset variable `{name}`"));
                    }
                }
            } else if matches!(chars.peek(), Some(ch) if ch.is_ascii_alphabetic() || *ch == '_') {
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
                    Some(val) => out.push_str(&val),
                    None => {
                        return Err(format!("referenced unset variable `{name}`"));
                    }
                }
            } else {
                out.push('$');
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
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

    #[test_case("x-$UNSET-y", "UNSET" ; "dollar_unset_names_var")]
    #[test_case("x-${UNSET}-y", "UNSET" ; "braced_unset_names_var")]
    #[test_case("${UNSET}", "UNSET" ; "braced_alone_names_var")]
    fn unset_var_is_an_error_naming_it(template: &str, name: &str) {
        let err = expand_with(template, &lookup_of(&[])).expect_err("unset must error");
        assert!(err.contains(name), "err={err}");
    }
}
