use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use color_eyre::{
    Section,
    eyre::{Context, Result, eyre},
};
use nix_capsule::path;

#[derive(Parser)]
#[command(
    name = "ncap-ctl",
    about = "Manage the nix-capsule container lifecycle",
    version
)]
struct Cli {
    /// Unix socket path
    #[arg(short, long, env = "NCAP_SOCKET")]
    socket: String,

    /// Server log directory
    #[arg(short, long, env = "NCAP_LOG_DIR")]
    log_dir: String,

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
    /// Enter an interactive shell inside the container
    Enter,
    /// Print the expanded runtime arguments
    ShowOptions,
    /// Remove all cached dev environments and nix profiles, including the current one
    Clean,
    /// Show container status
    Status,
    /// Show the server log (latest log file)
    Log {
        /// Disable pager and print the log to stdout
        #[arg(long)]
        no_pager: bool,
    },
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
    runtime: Runtime,
    run_opts: Vec<String>,
    server_bin: String,
    nix: NixInvoker,
    bash_bin: String,
    timeout: u64,
    project_root: String,
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

    let cfg = Config::from_cli(&cli)?;
    match cli.command {
        Cmd::Init => init(&cfg),
        Cmd::Start => start(&cfg),
        Cmd::Stop => stop(&cfg),
        Cmd::Restart => restart(&cfg),
        Cmd::Enter => enter(&cfg),
        Cmd::ShowOptions => show_options(&cfg),
        Cmd::Clean => clean(&cfg),
        Cmd::Status => status(&cfg),
        Cmd::Log { no_pager } => log(&cfg, no_pager),
        _ => unreachable!(),
    }
}

impl Config {
    fn from_cli(cli: &Cli) -> Result<Self> {
        let devshell = env("NCAP_DEVSHELL")?;
        let socket = cli.socket.clone();
        let socket_dir = Path::new(&socket)
            .parent()
            .map(|p| p.to_owned())
            .ok_or_else(|| eyre!("socket path has no parent directory `{socket}`"))?;

        let log_dir = cli.log_dir.clone();

        Ok(Self {
            devshell,
            devshell_name: OnceLock::new(),
            socket,
            socket_dir,
            log_dir,
            container: env("NCAP_CONTAINER")?,
            image: env("NCAP_IMAGE")?,
            runtime: Runtime::new(env("NCAP_RUNTIME")?),
            run_opts: json_env("NCAP_RUN_OPTS")?,
            server_bin: env("NCAP_SERVER")?,
            nix: NixInvoker::new(env("NCAP_NIX")?),
            bash_bin: env("NCAP_BASH")?,
            timeout: env("NCAP_TIMEOUT")?.parse()?,
            project_root: env("PROJECT_ROOT")?,
        })
    }

    fn devshell_name(&self) -> &str {
        self.devshell_name
            .get_or_init(|| sanitize_name(&self.devshell))
    }

    fn cache_file(&self) -> PathBuf {
        path::env_file(Path::new(&self.project_root), self.devshell_name())
    }

    /// True when the direnv signal says watched inputs are unchanged *and*
    /// the cached activation script exists. NCAP_CACHE is a transient signal
    /// from ncap-direnv; it is not a user-configurable path or toggle.
    fn cache_is_usable(&self) -> bool {
        std::env::var("NCAP_CACHE").ok().is_some_and(|s| s == "1")
            && self.cache_file().exists()
    }

    fn nix_profile(&self) -> PathBuf {
        path::nix_profile(Path::new(&self.project_root), self.devshell_name())
    }

    fn run_args(&self) -> Vec<String> {
        let pr = &self.project_root;
        let opts: Vec<String> = self.run_opts.iter().map(|opt| expand_env(opt)).collect();
        let cache_root = path::cache_root(Path::new(pr));
        let mut defaults = vec![
            "--replace".into(),
            "--name".into(),
            self.container.clone(),
            "-v".into(),
            "/nix:/nix:ro".into(),
            "-v".into(),
            format!(
                "{}:{}",
                self.socket_dir.display(),
                self.socket_dir.display()
            ),
            "-v".into(),
            format!("{pr}:{pr}"),
            "-w".into(),
            pr.to_owned(),
            "-v".into(),
            format!("{}:{}:ro", cache_root.display(), cache_root.display()),
        ];
        if Path::new(&format!("{pr}/.git")).is_dir() {
            defaults.push("-v".into());
            defaults.push(format!("{pr}/.git:{pr}/.git:ro"));
        }
        defaults.extend(opts);
        defaults
    }
}

fn env(name: &str) -> Result<String> {
    std::env::var(name).wrap_err(format!("env var not set: `{name}`"))
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
    std::fs::create_dir_all(path::devshell_dir(
        Path::new(&cfg.project_root),
        cfg.devshell_name(),
    ))?;

    if !cfg.cache_is_usable() {
        eprintln!(
            "evaluating devshell `{}` with nix print-dev-env...",
            cfg.devshell
        );
        let profile = cfg.nix_profile();
        let stdout = cfg
            .nix
            .print_dev_env(&profile.to_string_lossy(), &cfg.devshell)
            .wrap_err_with(|| format!("failed to evaluate devshell `{}`", cfg.devshell))
            .suggestion("check that the flake attribute exists and Nix can evaluate it")?;
        std::fs::write(cfg.cache_file(), &stdout)?;
        eprintln!("devshell cached");

        cfg.nix.wipe_history(&profile.to_string_lossy())?;

        restart(cfg)
    } else {
        start(cfg)
    }
}

fn start(cfg: &Config) -> Result<()> {
    if cfg.runtime.is_running(&cfg.container) {
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
    let id = cfg
        .runtime
        .run(cfg.run_args(), &cfg.image, &cfg.bash_bin, &exec_cmd)?;
    if !id.is_empty() {
        eprintln!("container started: `{id}`");
    }

    Ok(())
}

fn stop(cfg: &Config) -> Result<()> {
    eprintln!("stopping container `{}`...", cfg.container);
    let msg = cfg.runtime.stop(&cfg.container)?;
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
    let status = cfg
        .runtime
        .exec_interactive(&cfg.container, &cfg.bash_bin, &shell_cmd)?;

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
    let cache_base = path::cache_root(Path::new(&cfg.project_root));
    if cache_base.exists() {
        std::fs::remove_dir_all(&cache_base)
            .wrap_err_with(|| format!("failed to remove `{}`", cache_base.display()))?;
        eprintln!("removed `{}`", cache_base.display());
    }
    Ok(())
}

fn status(cfg: &Config) -> Result<()> {
    let cache_exists = cfg.cache_file().exists();

    if cfg.runtime.is_running(&cfg.container) {
        let id = cfg.runtime.container_id(&cfg.container)?;
        println!("container is running");
        println!("  id:      {id}");
        println!("  name:    {}", cfg.container);
        println!("  runtime: {}", cfg.runtime.bin());
        println!("  socket:  {}", cfg.socket);
    } else {
        println!("container is not running");
        if cache_exists {
            println!("  (cached environment exists — run `ncap-ctl start`)");
        } else {
            println!("  (no cached environment — run `ncap-ctl init`)");
        }
    }

    if cfg.cache_is_usable() {
        println!("  cache:   valid (direnv inputs unchanged)");
    } else if cache_exists {
        println!("  cache:   stale (will re-evaluate on init)");
    } else {
        println!("  cache:   none");
    }

    Ok(())
}

fn log(cfg: &Config, no_pager: bool) -> Result<()> {
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
        .filter(|e| path::parse_log_filename(&e.file_name().to_string_lossy()).is_some())
        .collect();

    entries
        .sort_by_key(|e| path::parse_log_filename(&e.file_name().to_string_lossy()).unwrap_or(0));

    let latest = entries
        .into_iter()
        .last()
        .ok_or_else(|| eyre!("no log files found in `{}`", log_dir.display()))?;

    let path = latest.path();

    if !no_pager {
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
    }

    let content = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("failed to read `{}`", path.display()))?;

    print!("{content}");
    Ok(())
}

/// Container runtime adapter (podman/docker). Owns the Go-template format
/// strings and the stdio-mode decision per operation.
pub struct Runtime {
    bin: String,
}

impl Runtime {
    pub fn new(bin: String) -> Self {
        Self { bin }
    }

    pub fn bin(&self) -> &str {
        &self.bin
    }

    /// `run -d` a container with the given volume/workdir args, image, and
    /// `bash -c` exec command. Returns the container id printed on stdout.
    pub fn run(
        &self,
        container_args: Vec<String>,
        image: &str,
        bash_bin: &str,
        exec_cmd: &str,
    ) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(["run", "-d"])
            .args(container_args)
            .args(["--", image, bash_bin, "-c", exec_cmd]);
        let stdout = run_piped(cmd, &format!("{} run", self.bin))?;
        Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
    }

    /// `stop` the named container. Returns the runtime's stdout message.
    pub fn stop(&self, container: &str) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(["stop", container]);
        let stdout = run_piped(cmd, &format!("{} stop", self.bin))?;
        Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
    }

    /// `exec -it` an interactive shell into the container. Inherits stdio
    /// and returns the child's exit status (caller decides whether to exit).
    pub fn exec_interactive(
        &self,
        container: &str,
        bash_bin: &str,
        shell_cmd: &str,
    ) -> Result<ExitStatus> {
        let status = Command::new(&self.bin)
            .args(["exec", "-it", container, bash_bin, "-c", shell_cmd])
            .status()
            .wrap_err("failed to enter container")
            .suggestion("check that the container is running (`ncap-ctl start`)")?;
        Ok(status)
    }

    /// `inspect -f {{.State.Running}}`. Returns false on any spawn/parse
    /// failure (the container is treated as not-running).
    pub fn is_running(&self, container: &str) -> bool {
        let Ok(output) = Command::new(&self.bin)
            .args(["inspect", "-f", "{{.State.Running}}", container])
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&output.stdout).trim() == "true"
    }

    /// `inspect -f {{.Id}}`. Returns the container id.
    pub fn container_id(&self, container: &str) -> Result<String> {
        let output = Command::new(&self.bin)
            .args(["inspect", "-f", "{{.Id}}", container])
            .output()
            .wrap_err("failed to inspect container")?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

/// `nix` adapter for `print-dev-env` and `profile wipe-history`.
pub struct NixInvoker {
    bin: String,
}

impl NixInvoker {
    pub fn new(bin: String) -> Self {
        Self { bin }
    }

    /// `nix print-dev-env --profile <profile> <devshell>` — captured stdout
    /// (the devshell activation script).
    pub fn print_dev_env(&self, profile: &str, devshell: &str) -> Result<Vec<u8>> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(["print-dev-env", "--profile", profile, devshell]);
        run_piped(cmd, "nix print-dev-env")
    }

    /// `nix profile wipe-history --profile <profile>` — status inherit.
    pub fn wipe_history(&self, profile: &str) -> Result<()> {
        Command::new(&self.bin)
            .args(["profile", "wipe-history", "--profile", profile])
            .status()
            .wrap_err("nix profile wipe-history")?;
        Ok(())
    }
}

/// Capture stdout, inherit stderr, bail with a labelled error on non-zero
/// exit. Shared by both adapters for the "capture-and-fail" spawning pattern.
fn run_piped(mut cmd: Command, label: &str) -> Result<Vec<u8>> {
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
