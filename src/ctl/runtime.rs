//! Runtime adapter: podman or docker. Both
//! runtimes share the same argument surface; probes use Go-template `inspect`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use super::paths::env_file;
use super::shell::shell_escape;

/// Arguments of the ncap-server binary: exactly its CLI surface
/// (`--socket`, `--log-dir`, `--timeout`).
#[derive(Clone, Debug)]
pub struct ServerArgs {
    pub socket: PathBuf,
    pub log_dir: PathBuf,
    pub timeout: u64,
}

/// The OCI runtime executable, scoped to a single container: the executable
/// name, the container name it manages, and the bash path used in commands.
#[derive(Clone, Debug)]
pub struct Runtime {
    bin: String,
    name: String,
    bash: PathBuf,
}

impl Runtime {
    pub fn new(bin: String, name: String, bash: PathBuf) -> Self {
        Self { bin, name, bash }
    }

    pub fn bin(&self) -> &str {
        &self.bin
    }

    /// `inspect -f {{.State.Running}} <name>` against the container it was
    /// constructed with — `true` means the container's
    /// init process (the server) is reportedly running. This alone is not
    /// liveness (see `is_live`): any spawn or parse failure is treated as
    /// not-running.
    pub async fn is_running(&self) -> bool {
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
    pub async fn is_live(&self, socket: &Path) -> bool {
        if !self.is_running().await {
            return false;
        }
        tokio::net::UnixStream::connect(socket).await.is_ok()
    }

    /// Raw `State` JSON via `inspect -f {{json .State}} <name>`, for the
    /// "never became live" failure report.
    pub async fn inspect_state(&self) -> String {
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

    /// `run -d --name <name> <mounts and options> -- <image> <bash> -c <cmd>` —
    /// detached, against the container it was constructed with. Renders the
    /// server launch command as
    /// `"source '<env_file>' && exec '<server>' --socket '<socket>' --log-dir '<log_dir>' --timeout <timeout>"`
    /// from the raw `env_file`/`server` paths plus the server CLI surface in
    /// `args`.
    /// `extra_args` are the mounts and options assembled by the ctl
    /// (defaults first, `extraOptions` appended after, `harden` prepended).
    /// Returns the container id on success, or the combined stderr/stdout on
    /// failure.
    pub async fn run_detached(
        &self,
        image: &str,
        env_file: &Path,
        server: &Path,
        args: &ServerArgs,
        extra_args: &[String],
    ) -> Result<String, String> {
        let exec_cmd = format!(
            "source '{}' && exec '{}' --socket '{}' --log-dir '{}' --timeout {}",
            shell_escape(&env_file.to_string_lossy()),
            shell_escape(&server.to_string_lossy()),
            shell_escape(&args.socket.to_string_lossy()),
            shell_escape(&args.log_dir.to_string_lossy()),
            args.timeout
        );
        let mut cmd = Command::new(&self.bin);
        cmd.args(["run", "-d", "--name", &self.name]);
        cmd.args(extra_args);
        cmd.args(["--", image]);
        let output = cmd
            .arg(&self.bash)
            .args(["-c", &exec_cmd])
            .output()
            .await
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            let mut msg = String::from_utf8_lossy(&output.stderr).to_string();
            if msg.trim().is_empty() {
                msg = String::from_utf8_lossy(&output.stdout).to_string();
            }
            Err(msg.trim().to_owned())
        }
    }

    /// `stop <name>`.
    pub async fn stop(&self) -> Result<String, String> {
        let output = Command::new(&self.bin)
            .args(["stop", &self.name])
            .output()
            .await
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    /// Whether a container with `name` exists at all (running or stopped):
    /// `inspect <name>` succeeding. Used by the start flow to remove an
    /// exists-but-stopped container before launch.
    pub async fn exists(&self) -> bool {
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
    pub async fn remove(&self) -> Result<String, String> {
        let output = Command::new(&self.bin)
            .args(["rm", &self.name])
            .output()
            .await
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    /// `exec -it <name> <bash> -c "source '<cache>/env' && exec '<bash>'" —
    /// interactive escape hatch against the container it was constructed with.
    /// Inherits stdio so the user's terminal drives
    /// the container shell directly. Returns `Ok` on exit 0, else an error
    /// naming the exit status.
    pub async fn exec_interactive(&self, cache_dir: &Path) -> Result<(), String> {
        let env_file = env_file(cache_dir);
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
            .map_err(|err| err.to_string())?;
        if status.success() {
            Ok(())
        } else {
            match status.code() {
                Some(code) => Err(format!("`{}` exec exited with status {code}", self.bin)),
                None => Err(format!("`{}` exec terminated by signal", self.bin)),
            }
        }
    }

    /// Whether the runtime binary resolves and is executable. Accepts a bare
    /// name (PATH lookup) or an absolute path.
    pub fn check_exists(&self) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let is_executable = |mode: u32| mode & 0o111 != 0;
        let bin_path = if self.bin.contains('/') {
            Path::new(&self.bin).to_path_buf()
        } else if let Some(path_var) = std::env::var_os("PATH") {
            std::env::split_paths(&path_var)
                .map(|dir| dir.join(&self.bin))
                .find(|candidate| {
                    candidate
                        .metadata()
                        .is_ok_and(|meta| is_executable(meta.permissions().mode()))
                })
                .unwrap_or_else(|| Path::new(&self.bin).to_path_buf())
        } else {
            Path::new(&self.bin).to_path_buf()
        };
        let meta = bin_path
            .metadata()
            .map_err(|_| format!("runtime not found: `{}`", bin_path.display()))?;
        if !meta.is_file() {
            return Err(format!("runtime is not a file: `{}`", bin_path.display()));
        }
        if !is_executable(meta.permissions().mode()) {
            return Err(format!(
                "runtime is not executable: `{}`",
                bin_path.display()
            ));
        }
        Ok(())
    }
}

/// Whether stderr indicates a concurrent-start "name in use" conflict.
pub fn is_name_in_use(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("already in use") || lower.contains("name in use")
}
