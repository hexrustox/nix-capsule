//! Runtime adapter: podman (default), docker, or an absolute path. Both
//! runtimes share the same argument surface; probes use Go-template `inspect`.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// The OCI runtime executable.
#[derive(Clone, Debug)]
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

    /// `inspect -f {{.State.Running}} <name>` — `true` means the container's
    /// init process (the server) is live. Any spawn or parse failure is
    /// treated as not-running.
    pub async fn is_running(&self, name: &str) -> bool {
        let output = Command::new(&self.bin)
            .args(["inspect", "-f", "{{.State.Running}}", name])
            .output()
            .await;
        match output {
            Ok(output) => String::from_utf8_lossy(&output.stdout).trim() == "true",
            Err(_) => false,
        }
    }

    /// Raw `State` JSON via `inspect -f {{json .State}} <name>`, for the
    /// "never reaching Running" failure report.
    pub async fn inspect_state(&self, name: &str) -> String {
        let output = Command::new(&self.bin)
            .args(["inspect", "-f", "{{json .State}}", name])
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
    /// detached. `extra_args` are the mounts and options assembled by the ctl
    /// (defaults first, `extraOptions` appended after, `harden` prepended).
    /// Returns the container id on success, or the combined stderr/stdout on
    /// failure.
    pub async fn run_detached(
        &self,
        name: &str,
        image: &str,
        bash: &Path,
        exec_cmd: &str,
        extra_args: &[String],
    ) -> Result<String, String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(["run", "-d", "--name", name]);
        cmd.args(extra_args);
        cmd.args(["--", image]);
        let output = cmd
            .arg(bash)
            .args(["-c", exec_cmd])
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
    pub async fn stop(&self, name: &str) -> Result<String, String> {
        let output = Command::new(&self.bin)
            .args(["stop", name])
            .output()
            .await
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    /// `rm <name>` — used to clear a dead container after a "name in use" race.
    pub async fn remove(&self, name: &str) -> Result<String, String> {
        let output = Command::new(&self.bin)
            .args(["rm", name])
            .output()
            .await
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
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

#[allow(dead_code)]
fn _unused_osstr_hint(_: &dyn AsRef<OsStr>) {}
fn _unused_stdio_hint() {
    let _ = Stdio::piped();
}
