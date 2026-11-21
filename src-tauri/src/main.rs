#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;
use std::path::Path;

#[derive(Parser, Debug)]
#[command(name = "dambo-ui")]
#[command(
    author = "kshava",
    version = "0.1.0",
    about = "Dambo Danmaku & Event Controller"
)]
struct Args {
    #[arg(short, long, default_value = "dambo_config.toml")]
    pub config: String,
}

fn resolve_config_path(raw: &str) -> String {
    let raw_path = Path::new(raw);
    if raw_path.is_absolute() || raw_path.exists() {
        return raw.to_string();
    }

    if let Ok(exe_path) = std::env::current_exe() {
        for dir in exe_path.ancestors().skip(1).take(6) {
            let res_candidate = dir.join("Resources").join(raw);
            if res_candidate.exists() {
                return res_candidate.to_string_lossy().into_owned();
            }
            let candidate = dir.join(raw);
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }

    if let Some(repo_root) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        let candidate = repo_root.join(raw);
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    raw.to_string()
}

fn main() {
    let filtered_args = std::env::args().filter(|arg| !arg.starts_with("-psn_"));
    let args = Args::parse_from(filtered_args);
    let resolved_config = resolve_config_path(&args.config);
    dambo_ui::run(&resolved_config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_config_path_from_non_repo_cwd() {
        let orig_dir = std::env::current_dir().unwrap();
        let temp = std::env::temp_dir();
        std::env::set_current_dir(&temp).unwrap();

        let resolved = resolve_config_path("dambo_config.toml");
        let resolved_path = Path::new(&resolved);
        let exists = resolved_path.exists();

        let _ = std::env::set_current_dir(&orig_dir);
        assert!(
            exists,
            "Expected resolved config path {:?} to exist",
            resolved
        );
    }
}
