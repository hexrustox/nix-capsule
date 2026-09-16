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

/// Invoke `nix print-dev-env --profile <profile> <devshell>` and return the
/// captured stdout (the env dump).
pub(crate) async fn print_dev_env(
    nix_bin: &Path,
    profile: &Path,
    devshell: &str,
) -> Result<Vec<u8>, PrintDevEnvError> {
    let output = Command::new(nix_bin)
        .args([
            "print-dev-env",
            "--profile",
            &profile.to_string_lossy(),
            devshell,
        ])
        .stderr(Stdio::inherit())
        .output()
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
    let _ = Command::new(nix_bin)
        .args([
            "profile",
            "wipe-history",
            "--profile",
            &profile.to_string_lossy(),
        ])
        .output()
        .await;
}
