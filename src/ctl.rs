use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

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
        let cache_dir = PathBuf::from(format!("{}/.ncap-cache/{devshell_name}", project_root));
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

fn init(cfg: &Config) -> Result<()> {
    std::fs::create_dir_all(cfg.cache_file.parent().unwrap())?;

    let ncaps = std::env::var("NCAP_CACHE").unwrap_or_default();
    if ncaps != "1" || !cfg.cache_file.exists() {
        eprintln!("Caching dev environment...");
        let output = std::process::Command::new(&cfg.nix_bin)
            .args([
                "print-dev-env",
                "--profile",
                &cfg.nix_profile.to_string_lossy(),
                &cfg.devshell,
            ])
            .output()
            .context("nix print-dev-env")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("nix print-dev-env failed: {stderr}");
        }
        std::fs::write(&cfg.cache_file, &output.stdout)?;

        let _ = std::process::Command::new(&cfg.nix_bin)
            .args([
                "profile",
                "wipe-history",
                "--profile",
                &cfg.nix_profile.to_string_lossy(),
            ])
            .status();

        restart(cfg)
    } else {
        start(cfg)
    }
}

fn start(cfg: &Config) -> Result<()> {
    if is_running(&cfg.runtime, &cfg.container) {
        eprintln!("Container '{}' is already running.", cfg.container);
        return Ok(());
    }

    if !cfg.cache_file.exists() {
        anyhow::bail!("No cached dev environment found. Run 'ncap-ctl init' first.");
    }

    std::fs::create_dir_all(&cfg.socket_dir)?;

    let mut run = std::process::Command::new(&cfg.runtime);
    run.args(["run", "-d"]);
    for opt in cfg.podman_run_args() {
        run.arg(opt);
    }
    run.args(["--", &cfg.image, &cfg.init_bin, "--socket", &cfg.socket]);
    let status = run.status().context("podman run")?;
    if !status.success() {
        anyhow::bail!("podman run failed with exit code: {status}");
    }

    let exec_cmd = format!(
        "source {} && exec {} --socket {}",
        cfg.cache_file.display(),
        cfg.server_bin,
        cfg.socket,
    );
    std::process::Command::new(&cfg.runtime)
        .args(["exec", "-d", &cfg.container, &cfg.bash_bin, "-c", &exec_cmd])
        .status()
        .context("podman exec")?;

    Ok(())
}

fn stop(cfg: &Config) -> Result<()> {
    std::process::Command::new(&cfg.runtime)
        .args(["stop", &cfg.container])
        .status()
        .context("podman stop")?;
    Ok(())
}

fn restart(cfg: &Config) -> Result<()> {
    stop(cfg)?;
    start(cfg)
}

fn enter(cfg: &Config) -> Result<()> {
    if !cfg.cache_file.exists() {
        anyhow::bail!("No cached dev environment found. Run 'ncap-ctl init' first.");
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
        .context("podman exec")?;

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

fn env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("NCAP_* env var not set: {name}"))
}

fn json_env(name: &str) -> Result<Vec<String>> {
    let val = env(name)?;
    serde_json::from_str(&val).context("Failed to parse JSON")
}
