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
    BIN.get_or_init(|| {
        // 1) the user's login PATH (covers brew, custom installs, any shell)
        if let Ok(out) = std::process::Command::new("/bin/zsh")
            .args(["-lc", "command -v ollama"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() && std::path::Path::new(&s).exists() {
                return s;
            }
        }
        // 2) common install locations, incl. the Ollama.app bundle (GUI-only install)
        for p in [
            "/opt/homebrew/bin/ollama",
            "/usr/local/bin/ollama",
            "/usr/bin/ollama",
            "/opt/local/bin/ollama",
            "/Applications/Ollama.app/Contents/Resources/ollama",
        ] {
            if std::path::Path::new(p).exists() {
                return p.to_string();
            }
        }
        "ollama".to_string()
    })
    .as_str()
}
