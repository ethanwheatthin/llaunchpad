use anyhow::{Context, Result};
use tokio::process::Command;

#[derive(Clone, Debug)]
pub struct Agent {
    /// token passed to `ollama launch <name>`
    pub name: String,
    /// human label, e.g. "Codex App"
    pub display: String,
    /// GUI app (open + quit via app name) vs CLI (spawn in Terminal)
    pub is_gui: bool,
    /// Logo key matching an entry in `assets/logos/*.png`. Empty
    /// means "use the colored-initials fallback in the badge".
    /// Computed by `logo_for_agent` so the parsing helper stays
    /// dumb (it just reads `ollama launch --help`).
    pub logo: String,
}

/// GUI integrations: launched as desktop apps. Others run in a terminal.
fn is_gui(name: &str) -> bool {
    matches!(name, "codex-app" | "vscode")
}

/// Fetch the supported integrations by parsing `ollama launch --help`.
pub async fn list_agents() -> Result<Vec<Agent>> {
    let mut cmd = Command::new(crate::ollama::ollama_bin());
    cmd.args(["launch", "--help"]);
    #[cfg(windows)]
    cmd.creation_flags(super::CREATE_NO_WINDOW);
    let out = cmd
        .output()
        .await
        .context("failed to run `ollama launch --help` (is ollama installed?)")?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_agents(&text))
}

/// Parse the block between "Supported integrations:" and the next section.
fn parse_agents(help: &str) -> Vec<Agent> {
    let mut agents = Vec::new();
    let mut in_block = false;
    for line in help.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("Supported integrations:") {
            in_block = true;
            continue;
        }
        if in_block {
            // block ends at a blank line or a new section header (no leading spaces)
            if trimmed.is_empty() || !line.starts_with(' ') {
                break;
            }
            // line looks like:  "  codex-app       Codex App (aliases: codex-desktop, ...)"
            let cols = trimmed.trim_start();
            let mut it = cols.splitn(2, char::is_whitespace);
            let name = it.next().unwrap_or("").trim().to_string();
            if name.is_empty() {
                continue;
            }
            // Default display: capitalised name. Real display strings come
            // from ollama's help output (e.g. "Codex App"); we only fall
            // back to a name-derived display when the parser didn't find one.
            let mut display = it.next().unwrap_or("").trim().to_string();
            // drop the "(aliases: ...)" suffix from the label
            if let Some(idx) = display.find("(aliases:") {
                display = display[..idx].trim().to_string();
            }
            if display.is_empty() {
                display = name.clone();
            }
            let is_gui = is_gui(&name);
            let logo = logo_for_agent(&name);
            agents.push(Agent { name, display, is_gui, logo });
        }
    }
    agents
}

/// Map an agent launch token to its logo key (matches AgentBadge in app.slint).
/// Returns "" for unknown agents so the initials badge is used as fallback.
pub fn logo_for_agent(name: &str) -> String {
    let key: &'static str = match name {
        "claude" | "claude-code"                        => "claude-code",
        "codex-app" | "codex-desktop" | "codex-gui"
        | "codex"                                       => "codex",
        "opencode"                                      => "opencode",
        "hermes" | "hermes-agent"                       => "hermes",
        "openclaw"                                      => "openclaw",
        "cursor"                                        => "cursor",
        "windsurf"                                      => "windsurf",
        "copilot" | "github-copilot"                    => "copilot",
        "cline"                                         => "cline",
        "amp"                                           => "amp",
        "goose"                                         => "goose",
        "vscode" | "code"                               => "vscode",
        _                                               => "",
    };
    key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integrations() {
        let help = "\
Launch the Ollama interactive menu.

Supported integrations:
  claude          Claude Code
  codex-app       Codex App (aliases: codex-desktop, codex-gui)
  codex           Codex
  vscode          VS Code (aliases: code)

Examples:
  ollama launch
";
        let a = parse_agents(help);
        assert_eq!(a.len(), 4);
        assert_eq!(a[0].name, "claude");
        assert_eq!(a[0].display, "Claude Code");
        assert_eq!(a[1].name, "codex-app");
        assert_eq!(a[1].display, "Codex App");
        assert!(a[1].is_gui);
        assert!(!a[2].is_gui);
        assert!(a[3].is_gui);
    }
}
