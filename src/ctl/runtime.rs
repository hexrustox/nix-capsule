//! Runtime adapter: podman or docker, sharing one argument surface;
//! probes use Go-template `inspect`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use super::shell::shell_escape;

/// Failures of the runtime adapter: spawn errors ride as `#[source]`, a
/// failed run/stop/rm carries the captured output as a data field, exec
/// failures name the exit status.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeError {
    #[error("cannot spawn `{bin} {verb}`: {source}")]
    Spawn {
        bin: String,
        verb: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("`{bin} {verb}` failed:\n{output}")]
    Failed {
        bin: String,
        verb: &'static str,
        output: String,
    },
    #[error("`{bin} exec` failed")]
    ExecFailed { bin: String },
    #[error("container `{container}` never became live within {timeout}s (state: {state})")]
    NotLive {
        container: String,
        timeout: u64,
        state: String,
    },
}

/// Arguments of the ncap-server binary: exactly its CLI surface
/// (`--socket`, `--log-dir`, `--timeout`, `--log-level`).
#[derive(Clone, Debug)]
pub(super) struct ServerArgs {
    /// Unix socket path the server binds.
    pub(super) socket: PathBuf,
    /// Directory for per-run server logs.
    pub(super) log_dir: PathBuf,
    /// Drain grace for live connections at shutdown, seconds.
    pub(super) timeout: u64,
    /// Minimum severity the server logs at.
    pub(super) log_level: crate::server::LogLevel,
}

/// The OCI runtime executable scoped to a single container.
#[derive(Clone, Debug)]
pub(super) struct Runtime {
    bin: String,
    name: String,
    bash: PathBuf,
}

impl Runtime {
    /// Scope the runtime executable to the named container.
    pub(super) fn new(bin: String, name: String, bash: PathBuf) -> Self {
        Self { bin, name, bash }
    }

    /// `inspect -f {{.State.Running}} <name>` against the container it was
    /// constructed with — `true` means the container's
    /// init process (the server) is reportedly running. This alone is not
    /// liveness (see `is_live`): any spawn or parse failure is treated as
    /// not-running.
    pub(super) async fn is_running(&self) -> bool {
        let output = Command::new(&self.bin)
            .args(["inspect", "-f", "{{.State.Running}}", &self.name])
            .output()
            .await;
        match output {
            Ok(output) => String::from_utf8_lossy(&output.stdout).trim() == "true",
            Err(_) => false,
        }
    }

    /// Liveness predicate (§ Liveness): `Running` AND socket-connectable.
    /// One predicate serves both the `init` liveness probe and the `start`
    /// readiness poll. `Running` alone is not live (the container is still
    /// sourcing the Env dump ahead of the Server's bind).
    pub(super) async fn is_live(&self, socket: &Path) -> bool {
        if !self.is_running().await {
            return false;
        }
        tokio::net::UnixStream::connect(socket).await.is_ok()
    }

    /// Raw `State` JSON via `inspect -f {{json .State}} <name>`, for the
    /// "never became live" failure report.
    pub(super) async fn inspect_state(&self) -> String {
        let output = Command::new(&self.bin)
            .args(["inspect", "-f", "{{json .State}}", &self.name])
            .output()
            .await;
        match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            }
            Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            Err(err) => err.to_string(),
        }
    }

    /// Spawn `<bin> <verb> <args...>` via `output()`, returning trimmed
    /// stdout when the runtime exits 0; otherwise a [`RuntimeError::Spawn`]
    /// (exec failure) or [`RuntimeError::Failed`] carrying the captured
    /// output — stderr, falling back to stdout when stderr is empty. The
    /// shared success/$? check behind `run_detached`, `stop`, and `remove`.
    /// `configure` receives the command after `<bin> <verb>` and may append
    /// arbitrary args before spawn.
    async fn run_checked(
        &self,
        verb: &'static str,
        configure: impl FnOnce(&mut Command),
    ) -> Result<String, RuntimeError> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg(verb);
        configure(&mut cmd);
        let output = cmd.output().await.map_err(|source| RuntimeError::Spawn {
            bin: self.bin.clone(),
            verb,
            source,
        })?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(RuntimeError::Failed {
                bin: self.bin.clone(),
                verb,
                output: captured_output(&output),
            })
        }
    }

    /// `run -d --name <name> <mounts and options> -- <image> <bash> -c <cmd>` —
    /// detached, against the container it was constructed with. Renders the
    /// server launch command as
    /// `"source '<env_file>' && exec '<server>' --socket '<socket>' --log-dir '<log_dir>' --timeout <timeout> --log-level <log_level>"`
    /// from the raw `env_file`/`server` paths plus the server CLI surface in
    /// `args`. The validated log level renders bare like the numeric timeout
    /// (no shell escaping, unlike paths).
    /// `extra_args` are the mounts and options assembled by the ctl
    /// (defaults first, `extraOptions` appended after, `harden` prepended).
    /// Returns the container id on success, or the runtime's captured output
    /// on failure, as a [`RuntimeError::Failed`].
    pub(super) async fn run_detached(
        &self,
        image: &str,
        env_file: &Path,
        server: &Path,
        args: &ServerArgs,
        extra_args: &[String],
    ) -> Result<String, RuntimeError> {
        self.run_checked("run", |cmd| {
            let exec_cmd = format!(
                "source '{}' && exec '{}' --socket '{}' --log-dir '{}' --timeout {} --log-level {}",
                shell_escape(&env_file.to_string_lossy()),
                shell_escape(&server.to_string_lossy()),
                shell_escape(&args.socket.to_string_lossy()),
                shell_escape(&args.log_dir.to_string_lossy()),
                args.timeout,
                args.log_level
            );
            cmd.args(["-d", "--name", &self.name]);
            cmd.args(extra_args);
            cmd.args(["--", image]);
            cmd.arg(&self.bash);
            cmd.args(["-c", &exec_cmd]);
        })
        .await
    }

    /// `stop <name>`.
    pub(super) async fn stop(&self) -> Result<String, RuntimeError> {
        self.run_checked("stop", |cmd| {
            cmd.arg(&self.name);
        })
        .await
    }

    /// Whether a container with `name` exists at all (running or stopped):
    /// `inspect <name>` succeeding. Used by the start flow to remove an
    /// exists-but-stopped container before launch.
    pub(super) async fn exists(&self) -> bool {
        match Command::new(&self.bin)
            .args(["inspect", &self.name])
            .output()
            .await
        {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    /// `rm <name>` — used to clear a dead container after a "name in use" race.
    pub(super) async fn remove(&self) -> Result<String, RuntimeError> {
        self.run_checked("rm", |cmd| {
            cmd.arg(&self.name);
        })
        .await
    }

    /// `exec -it <name> <bash> -c "source '<cache>/env' && exec '<bash>'" —
    /// interactive escape hatch against the container it was constructed with.
    /// Inherits stdio so the user's terminal drives
    /// the container shell directly. Returns `Ok` on exit 0, else an error
    /// naming the exit status.
    pub(super) async fn exec_interactive(&self, cache_dir: &Path) -> Result<(), RuntimeError> {
        let env_file = super::paths::env_file(cache_dir);
        let cmd_str = format!(
            "source '{}' && exec '{}'",
            shell_escape(&env_file.to_string_lossy()),
            shell_escape(&self.bash.to_string_lossy())
        );
        let status = Command::new(&self.bin)
            .args(["exec", "-it", &self.name])
            .arg(&self.bash)
            .args(["-c", &cmd_str])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await
            .map_err(|source| RuntimeError::Spawn {
                bin: self.bin.clone(),
                verb: "exec",
                source,
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(RuntimeError::ExecFailed {
                bin: self.bin.clone(),
            })
        }
    }
}

/// The captured output of a failed runtime invocation, for the error
/// payload: stderr, falling back to stdout when stderr is empty, trimmed.
fn captured_output(output: &std::process::Output) -> String {
    let mut msg = String::from_utf8_lossy(&output.stderr).to_string();
    if msg.trim().is_empty() {
        msg = String::from_utf8_lossy(&output.stdout).to_string();
    }
    msg.trim().to_owned()
}

pub(super) fn is_name_in_use(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("already in use") || lower.contains("name in use")
}
