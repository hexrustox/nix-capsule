//! Ctl (`ncap-ctl`): the project's lifecycle brain on the host — config from
//! the `NCAP_*` contract, freshness, stamp guard, and the container flows.

pub mod config;
pub mod digest;
pub mod names;
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
    let cfg = match config::resolve(cmd, &lookup) {
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
    let root = cfg.root.as_deref().expect("init demands root");
    let cache_dir = cfg.cache_dir.as_deref().expect("init demands cache_dir");
    let project = cfg.project.as_deref().expect("init demands project");
    stamp::guard(cache_dir, project, root).map_err(|err| err.to_string())?;

    let rt = runtime::Runtime::new(cfg.runtime.clone());
    let running = rt.is_running(&cfg.container).await;
    let freshness = digest::check(cache_dir, root, &cfg.watch_files);

    match (running, freshness) {
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
    let root = cfg.root.as_deref().expect("start demands root");
    let cache_dir = cfg.cache_dir.as_deref().expect("start demands cache_dir");
    let project = cfg.project.as_deref().expect("start demands project");
    stamp::guard(cache_dir, project, root).map_err(|err| err.to_string())?;

    let rt = runtime::Runtime::new(cfg.runtime.clone());
    if rt.is_running(&cfg.container).await {
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
    // Non-fatal stop, then init.
    let rt = runtime::Runtime::new(cfg.runtime.clone());
    let _ = rt.stop(&cfg.container).await;
    // Re-resolve as Init so the demand set is Init's (includes nix/devshell).
    // Reuse the same env; just dispatch to init with the same cfg fields
    // re-resolved. For simplicity, call init with a re-resolved Init config
    // when the current cfg came from Restart. The Restart cfg already has
    // Init's fields (root, project, etc.), so we can just call init directly
    // on the same cfg after adjusting the cmd tag.
    let mut init_cfg = cfg;
    init_cfg.cmd = Cmd::Init;
    // Ensure the Restart cfg has all Init fields; if resolve gave us Restart
    // with Init's fields, they're present. Re-check if devshell/nix were
    // missing (Restart demands them, so they are present).
    init(init_cfg).await
}

async fn status(cfg: Config) -> Result<(), String> {
    let rt = runtime::Runtime::new(cfg.runtime.clone());
    let running = rt.is_running(&cfg.container).await;

    let socket_connectable = if let Some(socket) = cfg.socket.as_deref() {
        UnixStream::connect(socket).await.is_ok()
    } else {
        false
    };

    let cache_status = if let Some(cache_dir) = cfg.cache_dir.as_deref() {
        // For status, the root may be None when watch files are empty and
        // paths were explicit. Use the cache check only when we can compute
        // the digest; otherwise fall back to env-file existence.
        if cfg.watch_files.is_empty() && cfg.root.is_none() {
            if cache_dir.join("env").is_file() {
                // No watch files to hash — treat as fresh when env exists and
                // hash file matches empty digest, else stale.
                let empty_digest = digest::of(Path::new("/"), &[]).unwrap_or_default();
                match fs::read_to_string(cache_dir.join("hash")) {
                    Ok(cached) if cached.trim() == empty_digest => "fresh",
                    Ok(_) => "stale",
                    Err(_) => "stale",
                }
            } else {
                "missing"
            }
        } else {
            let root = cfg
                .root
                .as_deref()
                .unwrap_or_else(|| Path::new("/tmp"));
            match digest::check(cache_dir, root, &cfg.watch_files) {
                digest::Freshness::Fresh => "fresh",
                digest::Freshness::Stale => "stale",
                digest::Freshness::Missing => "missing",
            }
        }
    } else {
        "missing"
    };

    if running {
        println!("container: running ({})", cfg.container);
    } else {
        println!("container: not running");
    }
    if let Some(socket) = cfg.socket.as_deref() {
        if socket_connectable {
            println!("socket: connectable ({})", socket.display());
        } else {
            println!("socket: unreachable ({})", socket.display());
        }
    }
    println!("cache: {cache_status}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers shared by init/start
// ---------------------------------------------------------------------------

async fn ensure_cache(cfg: &Config) -> Result<(), String> {
    let root = cfg.root.as_deref().expect("ensure_cache demands root");
    let cache_dir = cfg.cache_dir.as_deref().expect("ensure_cache demands cache_dir");
    let freshness = digest::check(cache_dir, root, &cfg.watch_files);
    if freshness == digest::Freshness::Fresh {
        return Ok(());
    }
    // Stale or missing → eval.
    let nix_bin = cfg.nix.as_deref().expect("ensure_cache demands nix");
    let devshell = cfg.devshell.as_deref().expect("ensure_cache demands devshell");
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
    let cache_dir = cfg.cache_dir.as_deref().expect("start_inner demands cache_dir");
    let socket = cfg.socket.as_deref().expect("start_inner demands socket");
    let log_dir = cfg.log_dir.as_deref().expect("start_inner demands log_dir");
    let server = cfg.server.as_deref().expect("start_inner demands server");
    let bash = cfg.bash.as_deref().expect("start_inner demands bash");
    let image = cfg.image.as_deref().expect("start_inner demands image");

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

    let rt = runtime::Runtime::new(cfg.runtime.clone());

    let run_result = rt.run_detached(&cfg.container, image, bash, &exec_cmd).await;

    let run_ok = match run_result {
        Ok(_) => true,
        Err(stderr) if runtime::is_name_in_use(&stderr) => {
            // Concurrent-start race: re-inspect.
            if rt.is_running(&cfg.container).await {
                eprintln!("container `{}` is already running", cfg.container);
                return Ok(());
            }
            // Dead container with the same name — remove and retry once.
            let _ = rt.remove(&cfg.container).await;
            match rt.run_detached(&cfg.container, image, bash, &exec_cmd).await {
                Ok(_) => true,
                Err(stderr) => return Err(format!("{} run failed: {stderr}", rt.bin())),
            }
        }
        Err(stderr) => {
            return Err(format!("{} run failed: {stderr}", rt.bin()));
        }
    };

    if !run_ok {
        unreachable!();
    }

    // Poll until State.Running within the deadline.
    let deadline = Instant::now() + Duration::from_secs(cfg.timeout);
    loop {
        if rt.is_running(&cfg.container).await {
            eprintln!("container `{}` is running", cfg.container);
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let state = rt.inspect_state(&cfg.container).await;
    let tail = newest_log_tail(log_dir);
    Err(format!(
        "container `{}` never reached Running within {}s (state: {state})\n{tail}",
        cfg.container, cfg.timeout
    ))
}

fn newest_log_tail(log_dir: &Path) -> String {
    let dir = match fs::read_dir(log_dir) {
        Ok(dir) => dir,
        Err(_) => return "(no log dir)".to_owned(),
    };
    let mut entries: Vec<(u64, PathBuf)> = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(epoch) = parse_log_epoch(&name) {
            entries.push((epoch, entry.path()));
        }
    }
    if entries.is_empty() {
        return "(no log file)".to_owned();
    }
    entries.sort_by_key(|(epoch, _)| *epoch);
    let newest = &entries.last().unwrap().1;
    let content = fs::read_to_string(newest).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let tail_start = lines.len().saturating_sub(20);
    lines[tail_start..].join("\n")
}

fn parse_log_epoch(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("ncap-server-")?;
    let epoch = rest.strip_suffix(".log")?;
    epoch.parse().ok()
}
