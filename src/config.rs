use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Last-used selection, persisted between runs.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Prefs {
    /// agent token (e.g. "codex-app")
    pub agent: String,
    /// launchable model name (e.g. "glm-4.6:cloud")
    pub model: String,
}

fn prefs_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join("Library/Application Support/Llaunchpad/prefs.json"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("APPDATA").ok()?;
        Some(PathBuf::from(base).join("Llaunchpad").join("prefs.json"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var("XDG_CONFIG_HOME")
            .unwrap_or_else(|_| format!("{}/.config", std::env::var("HOME").unwrap_or_default()));
        Some(PathBuf::from(base).join("llaunchpad").join("prefs.json"))
    }
}

pub fn load() -> Prefs {
    prefs_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(prefs: &Prefs) {
    let Some(path) = prefs_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(path, s);
    }
}
