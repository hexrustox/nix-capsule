use std::path::PathBuf;

pub const CACHE_DIR: &str = ".ncap-cache";
pub const DIRENV_MTIMES_FILE: &str = "direnv-mtimes.json";
pub const ENV_CACHE_FILE: &str = "env";
pub const NIX_PROFILE_FILE: &str = "profile";

pub fn devshell_cache_dir(project_root: &str, devshell_name: &str) -> PathBuf {
    PathBuf::from(format!("{project_root}/{CACHE_DIR}/{devshell_name}"))
}
