use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use color_eyre::eyre::{Context, Result};
use nix_capsule::path;
use serde::Deserialize;

const USE_FLAKE_TEMPLATE: &str = include_str!("use_flake.sh");

#[derive(Debug, Deserialize)]
struct WatchEntry {
    exists: bool,
    modtime: i64,
    path: String,
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let project_root = std::env::current_dir().wrap_err("failed to get current directory")?;
    let cache_dir = project_root.join(path::CACHE_DIR);
    let state_path = cache_dir.join(path::MTIME_FILE);

    let current = direnv_show_dump(&project_root)?;

    let stored = load_state(&state_path)?;

    let valid = compare(&stored, &current);

    save_state(&state_path, &current)?;

    let cache_valid = if valid { 1 } else { 0 };

    let script = USE_FLAKE_TEMPLATE
        .replace("__NCAP_CACHE__", &cache_valid.to_string())
        .replace("__CACHE_DIR__", &cache_dir.to_string_lossy());
    print!("{script}");

    Ok(())
}

fn direnv_show_dump(project_root: &Path) -> Result<HashMap<String, i64>> {
    let watches_env = std::env::var("DIRENV_WATCHES").unwrap_or_default();

    if watches_env.is_empty() {
        return Ok(HashMap::new());
    }

    let output = Command::new("direnv")
        .args(["show_dump", &watches_env])
        .current_dir(project_root)
        .output()
        .wrap_err("failed to run `direnv show_dump`")?;

    if !output.status.success() {
        eprintln!(
            "`direnv show_dump` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        );
        return Ok(HashMap::new());
    }

    let entries: Vec<WatchEntry> = serde_json::from_slice(&output.stdout)
        .wrap_err("failed to parse `direnv show_dump` output")?;

    Ok(entries
        .into_iter()
        .filter(|e| e.exists && e.modtime > 0)
        .map(|e| (e.path, e.modtime))
        .collect())
}

fn load_state(path: &Path) -> Result<HashMap<String, i64>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let data = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read `{}`", path.display()))?;
    let map = serde_json::from_str(&data)
        .wrap_err_with(|| format!("failed to parse `{}`", path.display()))?;
    Ok(map)
}

fn save_state(path: &Path, current: &HashMap<String, i64>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).wrap_err("failed to create cache directory")?;
    }
    let json = serde_json::to_string(current)
        .wrap_err_with(|| format!("failed to serialize `{}`", path.display()))?;
    std::fs::write(path, json).wrap_err_with(|| format!("failed to save `{}`", path.display()))?;
    Ok(())
}

fn compare(stored: &HashMap<String, i64>, current: &HashMap<String, i64>) -> bool {
    if stored.is_empty() {
        return false;
    }
    stored
        .iter()
        .all(|(path, stored_mtime)| current.get(path) == Some(stored_mtime))
}
