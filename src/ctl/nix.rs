//! Nix adapter: `print-dev-env` into the cache and profile-history pruning.

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// Invoke `nix print-dev-env --profile <profile> <devshell>` and return the
/// captured stdout (the env dump).
pub async fn print_dev_env(nix_bin: &Path, profile: &Path, devshell: &str) -> Result<Vec<u8>, String> {
    let output = Command::new(nix_bin)
        .args([
            "print-dev-env",
            "--profile",
            &profile.to_string_lossy(),
            devshell,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// `nix profile wipe-history --profile <profile>`.
pub async fn wipe_history(nix_bin: &Path, profile: &Path) -> Result<(), String> {
    let output = Command::new(nix_bin)
        .args(["profile", "wipe-history", "--profile", &profile.to_string_lossy()])
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}
