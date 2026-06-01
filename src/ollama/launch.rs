use crate::ollama::Agent;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn codex_home() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".codex"))
}

// ───────────────────────── running detection ─────────────────────────

/// Running state for many agents in one process-table scan.
/// On macOS the GUI app is matched by its bundle executable path
/// (`<bundle>/Contents/MacOS/`) so it does not false-positive on the
/// `codex` CLI/app-server binary under `Contents/Resources`.
#[cfg(target_os = "macos")]
pub fn running_states(agents: &[Agent]) -> Vec<bool> {
    use sysinfo::System;
    fn bundle(agent: &str) -> Option<&'static str> {
        match agent {
            "codex-app" => Some("Codex.app"),
            "vscode" => Some("Visual Studio Code.app"),
            _ => None,
        }
    }
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let exe_paths: Vec<String> = sys
        .processes()
        .values()
        .filter_map(|p| p.exe().map(|e| e.to_string_lossy().to_string()))
        .collect();
    agents
        .iter()
        .map(|a| match bundle(&a.name) {
            Some(b) => {
                let needle = format!("{b}/Contents/MacOS/");
                exe_paths.iter().any(|p| p.contains(&needle))
            }
            None => false,
        })
        .collect()
}

/// Running detection is macOS-only for now; elsewhere report not-running.
#[cfg(not(target_os = "macos"))]
pub fn running_states(agents: &[Agent]) -> Vec<bool> {
    agents.iter().map(|_| false).collect()
}

pub fn agent_running(agent: &Agent) -> bool {
    running_states(std::slice::from_ref(agent))
        .first()
        .copied()
        .unwrap_or(false)
}

// ───────────────────────── platform helpers ─────────────────────────

/// Strip everything that is not part of a plain base URL, so the result is safe
/// to interpolate into a shell command line. The retained set covers
/// scheme/host/port plus IPv6 literals (`[::1]`) and optional userinfo (`@`).
/// Shell metacharacters (`& # ? % = \ | ; < > $ ` ` ` "` `'` space) are dropped —
/// they have no place in a base URL and `&`/`%` are command separators / env
/// expansions on cmd.exe and POSIX shells.
fn shell_safe_url(url: &str) -> String {
    url.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "://.-_@[]".contains(*c))
        .collect()
}

/// Run a shell command line in a new terminal window.
/// If `ollama_host` is provided it is forwarded as `OLLAMA_HOST` so the agent
/// connects to the right server.
fn spawn_in_terminal(cmd: &str, ollama_host: Option<&str>) -> Result<()> {
    // Prepend OLLAMA_HOST=<url> to the command string for each platform.
    // The host is sanitized before interpolation to guard against shell injection.
    let full_cmd: String;
    let cmd = if let Some(host) = ollama_host {
        let safe = shell_safe_url(host);
        // Quote the assignment so cmd.exe does not fold the space before `&&`
        // into the value (`set VAR=x ` would store a trailing space).
        #[cfg(target_os = "windows")]
        { full_cmd = format!("set \"OLLAMA_HOST={safe}\"&& {cmd}"); }
        #[cfg(not(target_os = "windows"))]
        { full_cmd = format!("OLLAMA_HOST={safe} {cmd}"); }
        full_cmd.as_str()
    } else {
        cmd
    };

    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell",
            cmd.replace('\\', "\\\\").replace('"', "\\\"")
        );
        Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn()
            .context("failed to open Terminal")?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let hold = format!("{cmd}; exec ${{SHELL:-/bin/bash}}");
        let candidates: &[(&str, &[&str])] = &[
            ("x-terminal-emulator", &["-e", "bash", "-lc"]),
            ("gnome-terminal", &["--", "bash", "-lc"]),
            ("konsole", &["-e", "bash", "-lc"]),
            ("xfce4-terminal", &["-e", "bash", "-lc"]),
            ("xterm", &["-e", "bash", "-lc"]),
        ];
        for (bin, args) in candidates {
            let mut c = Command::new(bin);
            c.args(*args).arg(&hold);
            if c.spawn().is_ok() {
                return Ok(());
            }
        }
        anyhow::bail!("no terminal emulator found (tried gnome-terminal, konsole, xterm…)");
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "cmd", "/K", cmd])
            .spawn()
            .context("failed to open cmd")?;
        return Ok(());
    }
}

/// Quit a running GUI app (best effort, per platform).
fn quit_gui(app_name: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(format!("tell application \"{app_name}\" to quit"))
            .status();
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let _ = Command::new("killall").arg(app_name).status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("pkill").args(["-f", app_name]).status();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(["/IM", &format!("{app_name}.exe"), "/F"])
            .status();
    }
}

/// Open a GUI app by macOS application name (no-op shape elsewhere).
#[cfg(target_os = "macos")]
fn open_macos_app(app_name: &str) -> Result<()> {
    Command::new("open")
        .args(["-a", app_name])
        .spawn()
        .with_context(|| format!("failed to open {app_name}"))?;
    Ok(())
}

// ───────────────────────── codex profile migration ─────────────────────────

/// Read a `key = value` (string) from a section body.
fn read_val(body: &[String], key: &str) -> Option<String> {
    for l in body {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix(key) {
            if let Some(v) = rest.trim_start().strip_prefix('=') {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// Current Codex rejects legacy `profile = "..."` selectors and `[profiles.X]`
/// tables in `config.toml`; profiles must live in `<X>.config.toml`.
/// `ollama launch` (0.24) still writes the legacy form, so we migrate after it:
/// move each `[profiles.X]` table (plus its provider block and the chosen model)
/// into `~/.codex/X.config.toml`, then strip the table and selector from config.toml.
fn migrate_codex_profiles(model: &str) -> Result<()> {
    let Some(home) = codex_home() else { return Ok(()) };
    let cfg = home.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&cfg) else { return Ok(()) };

    // split into ordered sections ("" = preamble before first table)
    let mut order: Vec<String> = vec![String::new()];
    let mut sections: BTreeMap<String, Vec<String>> = BTreeMap::new();
    sections.insert(String::new(), Vec::new());
    let mut cur = String::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') && !t.starts_with("[[") {
            cur = t[1..t.len() - 1].to_string();
            if !sections.contains_key(&cur) {
                order.push(cur.clone());
                sections.insert(cur.clone(), Vec::new());
            }
        } else {
            sections.get_mut(&cur).unwrap().push(line.to_string());
        }
    }

    let profiles: Vec<String> = order
        .iter()
        .filter(|h| h.starts_with("profiles."))
        .cloned()
        .collect();

    // write each profile into its own <name>.config.toml
    for ph in &profiles {
        let name = ph.trim_start_matches("profiles.").to_string();
        let body = sections.get(ph).cloned().unwrap_or_default();
        let mut out = String::new();
        if read_val(&body, "model").is_none() {
            out.push_str(&format!("model = \"{model}\"\n"));
        }
        for l in &body {
            if !l.trim().is_empty() {
                out.push_str(l);
                out.push('\n');
            }
        }
        if let Some(prov) = read_val(&body, "model_provider") {
            let prov_header = format!("model_providers.{prov}");
            if let Some(pbody) = sections.get(&prov_header) {
                out.push_str(&format!("\n[model_providers.{prov}]\n"));
                for l in pbody {
                    if !l.trim().is_empty() {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
            }
        }
        let _ = std::fs::write(home.join(format!("{name}.config.toml")), out);
    }

    if profiles.is_empty() {
        // still drop a stray `profile =` selector if present
        if !content.lines().any(|l| l.trim_start().starts_with("profile =")) {
            return Ok(());
        }
    }

    // rebuild config.toml without profile tables and without `profile =` selector
    let mut rebuilt = String::new();
    for h in &order {
        if h.starts_with("profiles.") {
            continue;
        }
        let body = &sections[h];
        if h.is_empty() {
            for l in body {
                if l.trim_start().starts_with("profile =") {
                    continue;
                }
                rebuilt.push_str(l);
                rebuilt.push('\n');
            }
        } else {
            rebuilt.push_str(&format!("[{h}]\n"));
            for l in body {
                rebuilt.push_str(l);
                rebuilt.push('\n');
            }
        }
    }
    std::fs::write(&cfg, rebuilt).context("failed to rewrite codex config.toml")?;
    Ok(())
}

/// Codex (GUI app or CLI): `ollama launch` writes a legacy profile config that
/// current Codex rejects. Configure first (`--config`), migrate the profile into
/// its own file, then launch Codex ourselves.
fn launch_codex(agent: &str, is_gui: bool, model: &str, ollama_host: Option<&str>) -> Result<()> {
    // close the GUI app if it is already open (relaunch)
    #[cfg(target_os = "macos")]
    if is_gui {
        let probe = Agent { name: agent.to_string(), display: String::new(), is_gui: true };
        if agent_running(&probe) {
            quit_gui("Codex");
        }
    }

    // configure only — writes the (legacy) profile; ignore its exit status
    let mut cfg_cmd = Command::new(crate::ollama::ollama_bin());
    cfg_cmd.args(["launch", agent, "--model", model, "--config", "-y"]);
    if let Some(host) = ollama_host {
        cfg_cmd.env("OLLAMA_HOST", host);
    }
    let _ = cfg_cmd.status();

    migrate_codex_profiles(model)?;

    if is_gui {
        #[cfg(target_os = "macos")]
        {
            open_macos_app("Codex")?;
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut cmd = Command::new(crate::ollama::ollama_bin());
            cmd.args(["launch", agent, "--model", model, "-y"]);
            if let Some(host) = ollama_host {
                cmd.env("OLLAMA_HOST", host);
            }
            cmd.spawn().context("failed to launch codex-app")?;
        }
    } else {
        // CLI: run codex against the migrated profile in a terminal
        spawn_in_terminal("codex --profile ollama-launch", ollama_host)?;
    }
    Ok(())
}

// ───────────────────────── restore ─────────────────────────

/// An agent can be restored if `ollama` saved a backup for it
/// (`~/.ollama/launch/<agent>-restore.json`).
pub fn restore_available(agent: &str) -> bool {
    home_dir()
        .map(|h| {
            h.join(".ollama/launch")
                .join(format!("{agent}-restore.json"))
                .exists()
        })
        .unwrap_or(false)
}

/// Restore an agent to its original (pre-Ollama) profile.
pub fn restore_agent(agent: &str) -> Result<()> {
    let status = Command::new(crate::ollama::ollama_bin())
        .args(["launch", agent, "--restore", "-y"])
        .status()
        .with_context(|| format!("failed to restore `{agent}`"))?;
    if !status.success() {
        anyhow::bail!("restore of `{agent}` failed");
    }
    Ok(())
}

// ───────────────────────── public entry point ─────────────────────────

/// Launch (or relaunch) an agent with the given model via `ollama launch`.
/// `ollama_host` is forwarded as `OLLAMA_HOST` when set, routing the agent
/// to a custom Ollama server instead of the default localhost.
pub fn launch_agent(agent: &Agent, model: &str, ollama_host: Option<&str>) -> Result<()> {
    match agent.name.as_str() {
        "codex-app" => return launch_codex("codex-app", true, model, ollama_host),
        "codex" => return launch_codex("codex", false, model, ollama_host),
        _ => {}
    }

    if agent.is_gui {
        #[cfg(target_os = "macos")]
        if agent.name == "vscode" && agent_running(agent) {
            quit_gui("Visual Studio Code");
        }
        // ollama launch configures the integration and opens the app
        let mut cmd = Command::new(crate::ollama::ollama_bin());
        cmd.args(["launch", &agent.name, "--model", model, "-y"]);
        if let Some(host) = ollama_host {
            cmd.env("OLLAMA_HOST", host);
        }
        cmd.spawn().with_context(|| format!("failed to launch `{}`", agent.name))?;
    } else {
        // CLI agent: run inside a terminal (absolute path: GUI PATH is minimal)
        let cmd = format!(
            "{} launch {} --model {} -y",
            crate::ollama::ollama_bin(),
            agent.name,
            model
        );
        spawn_in_terminal(&cmd, ollama_host)?;
    }
    Ok(())
}
