use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct WatchEntry {
    exists: bool,
    modtime: i64,
    path: String,
}

fn main() -> Result<()> {
    let project_root = std::env::current_dir()?;
    let cache_dir = project_root.join(".ncap-cache");
    let state_path = cache_dir.join("watch-state.json");

    let current = direnv_show_dump(&project_root).unwrap_or_default();

    let stored = load_state(&state_path).unwrap_or_default();

    let valid = compare(&stored, &current);

    save_state(&state_path, &current)?;

    let valid = if valid { 1 } else { 0 };
    let cache_dir_str = cache_dir.display();

    println!(
        "export NCAP_CACHE={valid}\n\
         \n\
         use_flake() {{\n\
         \x20 local cache_dir=\"{cache_dir_str}\"\n\
         \x20 mkdir -p \"$cache_dir\"\n\
         \x20 if [[ $NCAP_CACHE -eq 0 ]]; then\n\
         \x20   nix print-dev-env \"$@\" > \"$cache_dir/env\"\n\
         \x20 fi\n\
         \x20 source \"$cache_dir/env\"\n\
         }}"
    );

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
        .output()?;

    if !output.status.success() {
        return Ok(HashMap::new());
    }

    let entries: Vec<WatchEntry> = serde_json::from_slice(&output.stdout)?;

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
    let data = std::fs::read_to_string(path)?;
    let map = serde_json::from_str(&data)?;
    Ok(map)
}

fn save_state(path: &Path, current: &HashMap<String, i64>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(current)?;
    std::fs::write(path, json)?;
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
