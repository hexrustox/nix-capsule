use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use color_eyre::{
    Section,
    eyre::{Context, Result, eyre},
};
use nix_capsule::path::{self, CACHE_DIR};

#[derive(Parser)]
#[command(
    name = "ncap-ctl",
    about = "Manage the nix-capsule container life-cycle",
    version
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
    /// Print the expanded runtime arguments
    ShowOptions,
    /// Remove all cached dev environments and nix profiles, including the current one
    Clean,
    /// Show container status
    Status,
    /// Show the server log (latest log file)
    Log,
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

struct Config {
    devshell: String,
    devshell_name: OnceLock<String>,
    socket: String,
    socket_dir: PathBuf,
    log_dir: String,
    container: String,
    image: String,
    runtime: String,
    run_opts: Vec<String>,
    server_bin: String,
    nix_bin: String,
    bash_bin: String,
    timeout: u64,
    cache_file: OnceLock<PathBuf>,
    nix_profile: OnceLock<PathBuf>,
    project_root: OnceLock<String>,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    if let Cmd::Completions { shell } = cli.command {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }

    let cfg = Config::from_env()?;
    match cli.command {
        Cmd::Init => init(&cfg),
        Cmd::Start => start(&cfg),
        Cmd::Stop => stop(&cfg),
        Cmd::Restart => restart(&cfg),
        Cmd::Enter => enter(&cfg),
        Cmd::ShowOptions => show_options(&cfg),
        Cmd::Clean => clean(&cfg),
        Cmd::Status => status(&cfg),
        Cmd::Log => log(&cfg),
        _ => unreachable!(),
    }
}

impl Config {
    fn from_env() -> Result<Self> {
        let devshell = env("NCAP_DEVSHELL")?;
        let socket = env("NCAP_SOCKET")?;
        let socket_dir = Path::new(&socket)
            .parent()
            .map(|p| p.to_owned())
            .ok_or_else(|| eyre!("socket path has no parent directory `{socket}`"))?;

        Ok(Self {
            devshell,
            devshell_name: OnceLock::new(),
            socket,
            socket_dir,
            log_dir: env("NCAP_LOG_DIR")?,
            container: env("NCAP_CONTAINER")?,
            image: env("NCAP_IMAGE")?,
            runtime: env("NCAP_RUNTIME")?,
            run_opts: json_env("NCAP_RUN_OPTS")?,
            server_bin: env("NCAP_SERVER")?,
            nix_bin: env("NCAP_NIX")?,
            bash_bin: env("NCAP_BASH")?,
            timeout: env("NCAP_TIMEOUT")?.parse()?,
            cache_file: OnceLock::new(),
            nix_profile: OnceLock::new(),
            project_root: OnceLock::new(),
        })
    }

    fn devshell_name(&self) -> &str {
        self.devshell_name
            .get_or_init(|| sanitize_name(&self.devshell))
    }

    fn project_root(&self) -> &str {
        self.project_root.get_or_init(|| {
            if let Ok(output) = std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
                && output.status.success()
            {
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .to_string_lossy()
                    .into_owned()
            }
        })
    }

    fn cache_file(&self) -> &PathBuf {
        self.cache_file.get_or_init(|| {
            let dir = path::devshell_cache_dir(self.project_root(), self.devshell_name());
            dir.join(path::ENV_CACHE_FILE)
        })
    }

    fn nix_profile(&self) -> &PathBuf {
        self.nix_profile.get_or_init(|| {
            let dir = path::devshell_cache_dir(self.project_root(), self.devshell_name());
            dir.join(path::NIX_PROFILE_FILE)
        })
    }

    fn run_args(&self) -> Vec<String> {
        let pr = self.project_root();
        let mut opts: Vec<String> = self.run_opts.iter().map(|opt| expand_env(opt)).collect();
        opts.push("-v".into());
        opts.push(format!("{pr}:{pr}"));
        opts.push("-w".into());
        opts.push(pr.to_owned());
        opts
    }
}

fn env(name: &str) -> Result<String> {
    std::env::var(name).wrap_err(format!("NCAP_* env var not set: `{name}`"))
}

fn json_env(name: &str) -> Result<Vec<String>> {
    let val = env(name)?;
    serde_json::from_str(&val).wrap_err("failed to parse `NCAP_RUN_OPTS` as json")
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
    result = result.trim_matches('-').to_owned();
    if result.is_empty() {
        "default".to_string()
    } else {
        result
    }
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
                    result.push('$');
                    result.push_str(&name);
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
    std::fs::create_dir_all(cfg.cache_file().parent().unwrap())?;

    let valid = std::env::var("NCAP_CACHE").ok();
    if valid.is_none_or(|s| s != "1") || !cfg.cache_file().exists() {
        eprintln!("caching dev environment");
        let profile = cfg.nix_profile().to_string_lossy();
        let mut nix_cmd = std::process::Command::new(&cfg.nix_bin);
        nix_cmd.args(["print-dev-env", "--profile", &profile, &cfg.devshell]);
        let stdout = run_piped(nix_cmd, "nix print-dev-env")?;
        std::fs::write(cfg.cache_file(), &stdout)?;

        std::process::Command::new(&cfg.nix_bin)
            .args(["profile", "wipe-history", "--profile", &profile])
            .status()
            .wrap_err("nix profile wipe-history")?;

        restart(cfg)
    } else {
        start(cfg)
    }
}

fn start(cfg: &Config) -> Result<()> {
    if is_running(&cfg.runtime, &cfg.container) {
        eprintln!("container `{}` is already running", cfg.container);
        return Ok(());
    }

    if !cfg.cache_file().exists() {
        return Err(eyre!("no cached dev environment found"))
            .suggestion("run `ncap-ctl init` first");
    }

    if cfg.socket_dir.is_dir() && !cfg.socket_dir.is_symlink() {
        if let Ok(mut dir) = std::fs::read_dir(&cfg.socket_dir)
            && dir.next().is_some()
        {
            eprintln!(
                "socket directory `{}` is not empty",
                cfg.socket_dir.display(),
            );
        }
    } else {
        std::fs::create_dir_all(&cfg.socket_dir)?;
        eprintln!("created socket directory `{}`", cfg.socket_dir.display());
    }

    let exec_cmd = format!(
        "source {} && exec {} --socket {} --log-dir {} --timeout {}",
        cfg.cache_file().display(),
        cfg.server_bin,
        cfg.socket,
        cfg.log_dir,
        cfg.timeout,
    );
    eprintln!("starting container `{}`...", cfg.container);
    let mut run = std::process::Command::new(&cfg.runtime);
    run.args(["run", "-d"]);
    for opt in cfg.run_args() {
        run.arg(opt);
    }
    run.args(["--", &cfg.image, &cfg.bash_bin, "-c", &exec_cmd]);
    let stdout = run_piped(run, &format!("{} run", cfg.runtime))?;
    let id = String::from_utf8_lossy(&stdout).trim().to_owned();
    if !id.is_empty() {
        eprintln!("container started: `{id}`");
    }

    Ok(())
}

fn stop(cfg: &Config) -> Result<()> {
    eprintln!("stopping container `{}`...", cfg.container);
    let mut stop = std::process::Command::new(&cfg.runtime);
    stop.args(["stop", &cfg.container]);
    let stdout = run_piped(stop, &format!("{} stop", cfg.runtime))?;
    let msg = String::from_utf8_lossy(&stdout).trim().to_owned();
    if !msg.is_empty() {
        eprintln!("container stopped: `{msg}`");
    }
    Ok(())
}

fn restart(cfg: &Config) -> Result<()> {
    eprintln!("restarting container `{}`...", cfg.container);
    // stop failures are non-fatal for restart
    let _ = stop(cfg);
    start(cfg)
}

fn enter(cfg: &Config) -> Result<()> {
    if !cfg.cache_file().exists() {
        return Err(eyre!("no cached dev environment found"))
            .suggestion("run `ncap-ctl init` first");
    }

    let shell_cmd = format!(
        "source {} && exec {}",
        cfg.cache_file().display(),
        cfg.bash_bin,
    );
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
        .wrap_err("failed to enter container")
        .suggestion("check that the container is running (`ncap-ctl start`)")?;

    std::process::exit(status.code().unwrap_or(1));
}

fn show_options(cfg: &Config) -> Result<()> {
    for opt in cfg.run_args() {
        println!("{opt}");
    }
    Ok(())
}

fn clean(cfg: &Config) -> Result<()> {
    eprintln!("cleaning all cached dev environments...");
    let _ = stop(cfg);
    let cache_base = PathBuf::from(format!("{}/{}", cfg.project_root(), CACHE_DIR));
    if cache_base.exists() {
        std::fs::remove_dir_all(&cache_base)
            .wrap_err_with(|| format!("failed to remove `{}`", cache_base.display()))?;
        eprintln!("removed `{}`", cache_base.display());
    }
    Ok(())
}

fn status(cfg: &Config) -> Result<()> {
    if is_running(&cfg.runtime, &cfg.container) {
        let output = std::process::Command::new(&cfg.runtime)
            .args(["inspect", "-f", "{{.Id}}", &cfg.container])
            .output()
            .wrap_err("failed to inspect container")?;
        let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        println!("container is running");
        println!("  id:      {id}");
        println!("  name:    {}", cfg.container);
        println!("  runtime: {}", cfg.runtime);
        println!("  socket:  {}", cfg.socket);
    } else {
        println!("container is not running");
        if cfg.cache_file().exists() {
            println!("  (cached environment exists — run `ncap-ctl start`)");
        } else {
            println!("  (no cached environment — run `ncap-ctl init`)");
        }
    }
    Ok(())
}

fn log(cfg: &Config) -> Result<()> {
    let log_dir = Path::new(&cfg.log_dir);
    if !log_dir.exists() {
        return Err(eyre!(
            "log directory does not exist: `{}`",
            log_dir.display()
        ));
    }

    let mut entries: Vec<_> = std::fs::read_dir(log_dir)
        .wrap_err_with(|| format!("failed to read log directory `{}`", log_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("ncap-server-") && name.ends_with(".log")
        })
        .collect();

    entries.sort_by_key(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        name.strip_prefix("ncap-server-")
            .and_then(|s| s.strip_suffix(".log"))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    });

    let latest = entries
        .into_iter()
        .last()
        .ok_or_else(|| eyre!("no log files found in `{}`", log_dir.display()))?;

    let path = latest.path();

    let pager = std::env::var("PAGER")
        .ok()
        .filter(|p| !p.is_empty())
        .or_else(|| {
            std::process::Command::new("less")
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|_| "less -R".to_string())
        });

    if let Some(ref pager) = pager {
        let parts: Vec<&str> = pager.split_whitespace().collect();
        let (cmd, args) = parts.split_first().unwrap();
        std::process::Command::new(cmd)
            .args(args)
            .arg(&path)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .wrap_err_with(|| format!("failed to run pager `{pager}`"))?;
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("failed to read `{}`", path.display()))?;

    print!("{content}");
    Ok(())
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

fn run_piped(mut cmd: std::process::Command, label: &str) -> Result<Vec<u8>> {
    let label = label.to_owned();
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .wrap_err(label.clone())?
        .wait_with_output()
        .wrap_err(label.clone())?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.is_empty() {
            eprint!("{stdout}");
        }
        color_eyre::eyre::bail!("`{label}` failed with exit code {}", output.status);
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;
    use test_case::test_case;

    #[test_case(".#container" => "container".to_owned(); "flake_shorthand")]
    #[test_case("./path#container" => "path-container".to_owned(); "relative_path_hash")]
    #[test_case("./path/to#" => "path-to".to_owned(); "relative_path_implicit")]
    #[test_case("container" => "container".to_owned(); "clean_name")]
    #[test_case("/abs/path#shell" => "abs-path-shell".to_owned(); "absolute_path_hash")]
    #[test_case("github:owner/repo#devShell" => "github-owner-repo-devShell".to_owned(); "flake_uri")]
    #[test_case("a#b#c" => "a-b-c".to_owned(); "multiple_hashes")]
    #[test_case(".nix#dev" => "nix-dev".to_owned(); "leading_dot_hash")]
    #[test_case("#" => "default".to_owned(); "just_hash")]
    #[test_case("" => "default".to_owned(); "empty")]
    fn test_sanitize_name(input: &str) -> String {
        sanitize_name(input)
    }

    static SETUP: LazyLock<()> = LazyLock::new(|| unsafe {
        std::env::set_var("FOO", "bar");
    });

    #[test_case("" => String::new(); "empty")]
    #[test_case("plain" => "plain".to_owned(); "pass_through")]
    #[test_case("$FOO" => "bar".to_owned(); "simple_var")]
    #[test_case("${FOO}" => "bar".to_owned(); "braced_var")]
    #[test_case("prefix_${FOO}_suffix" => "prefix_bar_suffix".to_owned(); "inline_braced")]
    #[test_case("$UNDEFINED_VAR" => "$UNDEFINED_VAR".to_owned(); "undefined_simple")]
    #[test_case("${UNDEFINED_VAR}" => "${UNDEFINED_VAR}".to_owned(); "undefined_braced")]
    #[test_case("$$" => "$$".to_owned(); "double_dollar")]
    #[test_case("$" => "$".to_owned(); "lone_dollar")]
    #[test_case("${}" => "${}".to_owned(); "empty_braces")]
    #[test_case("$123abc" => "$123abc".to_owned(); "var_starts_with_digit")]
    fn test_expand_env(input: &str) -> String {
        *SETUP;
        expand_env(input)
    }
}
