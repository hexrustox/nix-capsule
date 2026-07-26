use std::path::{Path, PathBuf};

pub const CACHE_DIR: &str = ".ncap-cache";
pub const DIRENV_MTIMES_FILE: &str = "direnv-mtimes.json";
pub const ENV_CACHE_FILE: &str = "env";
pub const NIX_PROFILE_FILE: &str = "profile";

const LOG_PREFIX: &str = "ncap-server-";
const LOG_SUFFIX: &str = ".log";

pub fn cache_root(project_root: &Path) -> PathBuf {
    project_root.join(CACHE_DIR)
}

pub fn devshell_dir(project_root: &Path, devshell_name: &str) -> PathBuf {
    cache_root(project_root).join(devshell_name)
}

pub fn env_file(project_root: &Path, devshell_name: &str) -> PathBuf {
    devshell_dir(project_root, devshell_name).join(ENV_CACHE_FILE)
}

pub fn nix_profile(project_root: &Path, devshell_name: &str) -> PathBuf {
    devshell_dir(project_root, devshell_name).join(NIX_PROFILE_FILE)
}

pub fn direnv_mtimes_file(project_root: &Path) -> PathBuf {
    cache_root(project_root).join(DIRENV_MTIMES_FILE)
}

pub fn log_filename(secs: u64) -> String {
    format!("{LOG_PREFIX}{secs}{LOG_SUFFIX}")
}

pub fn parse_log_filename(name: &str) -> Option<u64> {
    name.strip_prefix(LOG_PREFIX)?
        .strip_suffix(LOG_SUFFIX)?
        .parse()
        .ok()
}
