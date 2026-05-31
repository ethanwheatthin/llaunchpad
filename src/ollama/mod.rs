pub mod agents;
pub mod models;
pub mod launch;

pub use agents::{list_agents, Agent};
pub use models::list_cloud_models;
pub use launch::{launch_agent, running_states};

use std::sync::OnceLock;

/// Absolute path to the `ollama` binary.
///
/// GUI apps launched from Finder/Dock get a minimal `PATH`
/// (`/usr/bin:/bin:/usr/sbin:/sbin`) that excludes Homebrew, so a bare
/// `ollama` lookup fails. Resolve common install locations, then fall back
/// to a login shell, then to the bare name.
pub fn ollama_bin() -> &'static str {
    static BIN: OnceLock<String> = OnceLock::new();
    BIN.get_or_init(resolve_ollama).as_str()
}

#[cfg(unix)]
fn resolve_ollama() -> String {
    // 1) the user's login PATH (covers brew, custom installs, any shell)
    for sh in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if !std::path::Path::new(sh).exists() {
            continue;
        }
        if let Ok(out) = std::process::Command::new(sh)
            .args(["-lc", "command -v ollama"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() && std::path::Path::new(&s).exists() {
                return s;
            }
        }
    }
    // 2) common locations (macOS + Linux), incl. the Ollama.app bundle
    let home = std::env::var("HOME").unwrap_or_default();
    let mut candidates = vec![
        "/opt/homebrew/bin/ollama".to_string(),
        "/usr/local/bin/ollama".to_string(),
        "/usr/bin/ollama".to_string(),
        "/opt/local/bin/ollama".to_string(),
        "/Applications/Ollama.app/Contents/Resources/ollama".to_string(),
        "/snap/bin/ollama".to_string(),
    ];
    if !home.is_empty() {
        candidates.push(format!("{home}/.local/bin/ollama"));
    }
    for p in candidates {
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    "ollama".to_string()
}

#[cfg(windows)]
fn resolve_ollama() -> String {
    if let Ok(out) = std::process::Command::new("where").arg("ollama").output() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(first) = s.lines().next() {
            let first = first.trim();
            if !first.is_empty() && std::path::Path::new(first).exists() {
                return first.to_string();
            }
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = format!(r"{local}\Programs\Ollama\ollama.exe");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    "ollama.exe".to_string()
}
