use std::path::{Path, PathBuf};
use std::process::Output;

use clap::{Parser, Subcommand};
use color_eyre::eyre::{Context, Result};
use nix_capsule::path;

#[derive(Parser)]
#[command(
    name = "ncap-ctl",
    about = "Manage the nix-capsule container life-cycle"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Evaluate the devshell, cache it, and start/restart the container
    Init,
    /// Start the container from a cached devshell
    Start,
    /// Stop the running container
    Stop,
    /// Stop and restart the container
    Restart,
    /// Enter an interactive shell inside the devshell container
    Enter,
}

struct Config {
    devshell: String,
    socket: String,
    socket_dir: PathBuf,
    container: String,
    image: String,
    runtime: String,
    podman_opts: Vec<String>,
    init_bin: String,
    server_bin: String,
    nix_bin: String,
    bash_bin: String,
    project_root: String,
    cache_file: PathBuf,
    nix_profile: PathBuf,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let cfg = Config::from_env()?;
    match cli.command {
        Cmd::Init => init(&cfg),
        Cmd::Start => start(&cfg),
        Cmd::Stop => stop(&cfg),
        Cmd::Restart => restart(&cfg),
        Cmd::Enter => enter(&cfg),
    }
}

impl Config {
    fn from_env() -> Result<Self> {
        let project_root = project_root()?;
        let devshell = env("NCAP_DEVSHELL")?;
        let devshell_name = sanitize_name(&devshell);
        let cache_dir = PathBuf::from(format!(
            "{}/{}/{devshell_name}",
            project_root,
            path::CACHE_DIR
        ));
        let socket = env("NCAP_SOCKET")?;
        let socket_dir = Path::new(&socket)
            .parent()
            .map(|p| p.to_owned())
            .unwrap_or_else(|| PathBuf::from("/tmp"));

        Ok(Self {
            devshell,
            socket,
            socket_dir,
            container: env("NCAP_CONTAINER")?,
            image: env("NCAP_IMAGE")?,
            runtime: env("NCAP_RUNTIME")?,
            podman_opts: json_env("NCAP_PODMAN_OPTS")?,
            init_bin: env("NCAP_INIT")?,
            server_bin: env("NCAP_SERVER")?,
            nix_bin: env("NCAP_NIX")?,
            bash_bin: env("NCAP_BASH")?,
            project_root,
            cache_file: cache_dir.join("env"),
            nix_profile: cache_dir.join("profile"),
        })
    }

    fn podman_run_args(&self) -> Vec<String> {
        self.podman_opts
            .iter()
            .map(|opt| expand(opt, &self.project_root))
            .collect()
    }
}

fn env(name: &str) -> Result<String> {
    std::env::var(name).wrap_err(format!("NCAP_* env var not set: {name}"))
}

fn json_env(name: &str) -> Result<Vec<String>> {
    let val = env(name)?;
    serde_json::from_str(&val).wrap_err("failed to parse NCAP_PODMAN_OPTS as JSON")
}

fn project_root() -> Result<String> {
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        && output.status.success()
    {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Ok(std::env::current_dir()?.to_string_lossy().into_owned())
}

fn sanitize_name(devshell: &str) -> String {
    let replaced: String = devshell
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut result = String::with_capacity(replaced.len());
    let mut last_dash = false;
    for c in replaced.chars() {
        if c == '-' {
            if !last_dash {
                result.push(c);
            }
            last_dash = true;
        } else {
            result.push(c);
            last_dash = false;
        }
    }
    result.trim_matches('-').to_owned()
}

fn expand(s: &str, project_root: &str) -> String {
    let s = s.replace("$PROJECT_ROOT", project_root);
    expand_env(&s)
}

fn expand_env(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some(&'{') = chars.peek() {
                chars.next();
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '}' {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                chars.next();
                if let Ok(val) = std::env::var(&name) {
                    result.push_str(&val);
                } else {
                    result.push_str(&format!("${{{name}}}"));
                }
            } else if matches!(chars.peek(), Some(c) if c.is_ascii_alphabetic() || *c == '_') {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Ok(val) = std::env::var(&name) {
                    result.push_str(&val);
                } else {
                    result.push_str(&format!("${name}"));
                }
            } else {
                result.push('$');
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn init(cfg: &Config) -> Result<()> {
    std::fs::create_dir_all(cfg.cache_file.parent().unwrap())?;

    let valid = std::env::var("NCAP_CACHE").ok();
    if valid.is_none_or(|s| s != "1") || !cfg.cache_file.exists() {
        eprintln!("caching dev environment...");
        let output = std::process::Command::new(&cfg.nix_bin)
            .args([
                "print-dev-env",
                "--profile",
                &cfg.nix_profile.to_string_lossy(),
                &cfg.devshell,
            ])
            .output()
            .wrap_err("failed to evaluate devshell")?;
        if !output.status.success() {
            dump_failure(&output);
            color_eyre::eyre::bail!("nix print-dev-env failed with exit code: {}", output.status,);
        }
        std::fs::write(&cfg.cache_file, &output.stdout)?;

        let output = std::process::Command::new(&cfg.nix_bin)
            .args([
                "profile",
                "wipe-history",
                "--profile",
                &cfg.nix_profile.to_string_lossy(),
            ])
            .output()
            .wrap_err("failed to clean nix profile")?;
        if !output.status.success() {
            dump_failure(&output);
        }

        restart(cfg)
    } else {
        start(cfg)
    }
}

fn start(cfg: &Config) -> Result<()> {
    if is_running(&cfg.runtime, &cfg.container) {
        eprintln!("container {} is already running", cfg.container);
        return Ok(());
    }

    if !cfg.cache_file.exists() {
        color_eyre::eyre::bail!("no cached dev environment found — run ncap-ctl init first");
    }

    std::fs::create_dir_all(&cfg.socket_dir)?;

    let mut run = std::process::Command::new(&cfg.runtime);
    run.args(["run", "-d"]);
    for opt in cfg.podman_run_args() {
        run.arg(opt);
    }
    run.args(["--", &cfg.image, &cfg.init_bin, "--socket", &cfg.socket]);
    let output = run.output().wrap_err("failed to start container")?;
    if !output.status.success() {
        dump_failure(&output);
        color_eyre::eyre::bail!("podman run failed with exit code: {}", output.status);
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !id.is_empty() {
        eprintln!("container started: {id}");
    }

    let exec_cmd = format!(
        "source {} && exec {} --socket {}",
        cfg.cache_file.display(),
        cfg.server_bin,
        cfg.socket,
    );
    let output = std::process::Command::new(&cfg.runtime)
        .args(["exec", "-d", &cfg.container, &cfg.bash_bin, "-c", &exec_cmd])
        .output()
        .wrap_err("failed to start server in container")?;
    if !output.status.success() {
        dump_failure(&output);
        color_eyre::eyre::bail!("podman exec failed with exit code: {}", output.status);
    }

    Ok(())
}

fn stop(cfg: &Config) -> Result<()> {
    let output = std::process::Command::new(&cfg.runtime)
        .args(["stop", &cfg.container])
        .output()
        .wrap_err("failed to stop container")?;
    if !output.status.success() {
        dump_failure(&output);
    }
    let msg = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !msg.is_empty() {
        eprintln!("container stopped: {msg}");
    }
    Ok(())
}

fn restart(cfg: &Config) -> Result<()> {
    stop(cfg)?;
    start(cfg)
}

fn enter(cfg: &Config) -> Result<()> {
    if !cfg.cache_file.exists() {
        color_eyre::eyre::bail!("no cached dev environment found — run ncap-ctl init first");
    }

    let shell_cmd = format!("source {}; exec {}", cfg.cache_file.display(), cfg.bash_bin,);
    let status = std::process::Command::new(&cfg.runtime)
        .args([
            "exec",
            "-it",
            &cfg.container,
            &cfg.bash_bin,
            "-c",
            &shell_cmd,
        ])
        .status()
        .wrap_err("failed to enter container")?;

    std::process::exit(status.code().unwrap_or(1));
}

fn is_running(runtime: &str, container: &str) -> bool {
    let Ok(output) = std::process::Command::new(runtime)
        .args(["inspect", "-f", "{{.State.Running}}", container])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).trim() == "true"
}

fn dump_failure(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stdout.is_empty() && stderr.is_empty() {
        return;
    }
    eprint!("{stdout}{stderr}");
}
