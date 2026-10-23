//! Nix adapter: `print-dev-env` into the cache and profile-history pruning.

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// Failure of `nix print-dev-env`: a spawn error rides as `#[source]`, a
/// failed run carries the captured stderr as a data field.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PrintDevEnvError {
    #[error("cannot run `nix print-dev-env`: {source}")]
    Spawn {
        #[source]
        source: std::io::Error,
    },
    #[error("cannot eval `{devshell}` with `nix print-dev-env`")]
    Failed { devshell: String },
}

/// The spawned-child seam behind the adapter: the real `nix` process in
/// production; tests stand in a fake reporting canned outputs and
/// recording the args it was handed.
pub(crate) trait NixRunner {
    /// Run the command with stdout captured and stderr inherited — the
    /// eval path of spec/ctl.md § Freshness and the digest, whose nix
    /// chatter reaches the host shell.
    async fn run_inherit_stderr(
        &self,
        nix_bin: &Path,
        args: &[String],
    ) -> std::io::Result<std::process::Output>;

    /// Run the command with stdout and stderr both captured and left
    /// unread — the prune path, never leaking nix chatter into the
    /// command's own terminal output.
    async fn run_captured(
        &self,
        nix_bin: &Path,
        args: &[String],
    ) -> std::io::Result<std::process::Output>;
}

struct RealNixRunner;

impl NixRunner for RealNixRunner {
    async fn run_inherit_stderr(
        &self,
        nix_bin: &Path,
        args: &[String],
    ) -> std::io::Result<std::process::Output> {
        Command::new(nix_bin)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?
            .wait_with_output()
            .await
    }

    async fn run_captured(
        &self,
        nix_bin: &Path,
        args: &[String],
    ) -> std::io::Result<std::process::Output> {
        Command::new(nix_bin).args(args).output().await
    }
}

/// Captured stdout is the env dump.
pub(crate) async fn print_dev_env(
    nix_bin: &Path,
    profile: &Path,
    devshell: &str,
) -> Result<Vec<u8>, PrintDevEnvError> {
    print_dev_env_with(&RealNixRunner, nix_bin, profile, devshell).await
}

pub(crate) async fn print_dev_env_with<R: NixRunner>(
    runner: &R,
    nix_bin: &Path,
    profile: &Path,
    devshell: &str,
) -> Result<Vec<u8>, PrintDevEnvError> {
    let output = runner
        .run_inherit_stderr(
            nix_bin,
            &[
                "print-dev-env".to_owned(),
                "--profile".to_owned(),
                profile.to_string_lossy().into_owned(),
                devshell.to_owned(),
            ],
        )
        .await
        .map_err(|source| PrintDevEnvError::Spawn { source })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(PrintDevEnvError::Failed {
            devshell: devshell.to_owned(),
        })
    }
}

/// `nix profile wipe-history --profile <profile>`.
pub(crate) async fn wipe_history(nix_bin: &Path, profile: &Path) {
    wipe_history_with(&RealNixRunner, nix_bin, profile).await
}

pub(crate) async fn wipe_history_with<R: NixRunner>(runner: &R, nix_bin: &Path, profile: &Path) {
    let _ = runner
        .run_captured(
            nix_bin,
            &[
                "profile".to_owned(),
                "wipe-history".to_owned(),
                "--profile".to_owned(),
                profile.to_string_lossy().into_owned(),
            ],
        )
        .await;
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Output;
    use std::sync::Mutex;

    use super::*;

    #[tokio::test]
    async fn print_dev_env_with_a_missing_nix_binary_is_a_spawn_error() {
        let err = print_dev_env(
            Path::new("/nonexistent/ncap-fake-nix"),
            Path::new("/tmp/profile"),
            "a-devshell",
        )
        .await
        .expect_err("a missing binary cannot spawn");
        assert!(
            matches!(err, PrintDevEnvError::Spawn { .. }),
            "a spawn failure must be the `Spawn` variant, got {err:?}"
        );
    }

    #[tokio::test]
    async fn print_dev_env_returns_the_captured_stdout_on_success() -> anyhow::Result<()> {
        let nix = FakeNix::new(0, b"env dump");
        let stdout = print_dev_env_with(
            &nix,
            Path::new("/usr/bin/nix"),
            Path::new("/tmp/profile"),
            "a-devshell",
        )
        .await?;
        assert_eq!(stdout, b"env dump");
        assert_eq!(
            nix.args_of(0),
            owned(&["print-dev-env", "--profile", "/tmp/profile", "a-devshell"])
        );
        Ok(())
    }

    #[tokio::test]
    async fn print_dev_env_names_the_devshell_on_a_failed_eval() {
        let nix = FakeNix::new(1, b"");
        let err = print_dev_env_with(
            &nix,
            Path::new("/usr/bin/nix"),
            Path::new("/tmp/profile"),
            "broken-devshell",
        )
        .await
        .expect_err("a failed eval must error");
        assert!(
            matches!(
                err,
                PrintDevEnvError::Failed { ref devshell } if devshell == "broken-devshell"
            ),
            "a failed eval must be the `Failed` variant echoing the devshell, got {err:?}"
        );
    }

    #[tokio::test]
    async fn wipe_history_prunes_the_profile_and_swallows_a_failed_prune() {
        let nix = FakeNix::new(1, b"");
        wipe_history_with(&nix, Path::new("/usr/bin/nix"), Path::new("/tmp/profile")).await;
        assert_eq!(
            nix.args_of(0),
            owned(&["profile", "wipe-history", "--profile", "/tmp/profile"])
        );
    }

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// The nix binary: reports itself with the canned exit code and
    /// stdout, and remembers the args of every call.
    struct FakeNix {
        code: i32,
        stdout: Vec<u8>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeNix {
        fn new(code: i32, stdout: &[u8]) -> Self {
            Self {
                code,
                stdout: stdout.to_vec(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn args_of(&self, nth: usize) -> Vec<String> {
            self.calls.lock().expect("fake mutex")[nth].clone()
        }

        fn run_fake(&self, args: &[String]) -> io::Result<Output> {
            self.calls.lock().expect("fake mutex").push(args.to_vec());
            Ok(Output {
                status: <std::process::ExitStatus as ExitStatusExt>::from_raw(self.code << 8),
                stdout: self.stdout.clone(),
                stderr: Vec::new(),
            })
        }
    }

    impl NixRunner for FakeNix {
        async fn run_inherit_stderr(&self, _nix_bin: &Path, args: &[String]) -> io::Result<Output> {
            self.run_fake(args)
        }

        async fn run_captured(&self, _nix_bin: &Path, args: &[String]) -> io::Result<Output> {
            self.run_fake(args)
        }
    }
}
