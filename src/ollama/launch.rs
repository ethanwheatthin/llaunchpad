use crate::ollama::Agent;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

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

// ───────────────────────── install detection ─────────────────────────

/// What it takes for an agent integration to be considered "installed".
/// Either at least one binary is on PATH, or at least one macOS `.app`
/// bundle is found. Empty arrays disable that side of the check.
struct InstallSpec {
    bins: &'static [&'static str],
    bundles: &'static [&'static str],
}

/// Known per-agent install rules. Agents not in this table are reported as
/// installed: we'd rather let an unknown integration through than block a
/// legitimate launch with a false negative.
fn install_spec(name: &str) -> Option<InstallSpec> {
    match name {
        "codex-app" => Some(InstallSpec { bins: &[], bundles: &["Codex.app"] }),
        "vscode" => Some(InstallSpec {
            bins: &["code"],
            bundles: &["Visual Studio Code.app", "VSCodium.app"],
        }),
        "cursor" => Some(InstallSpec { bins: &["cursor"], bundles: &["Cursor.app"] }),
        "codex" => Some(InstallSpec { bins: &["codex"], bundles: &[] }),
        "claude" => Some(InstallSpec { bins: &["claude"], bundles: &[] }),
        "opencode" => Some(InstallSpec { bins: &["opencode"], bundles: &[] }),
        _ => None,
    }
}

/// PATH directories from the *login* shell, resolved once.
/// GUI apps on macOS get a minimal PATH from launchd (no Homebrew), so a bare
/// `code`/`cursor`/… lookup misses real installs. Mirror the trick already
/// used by `resolve_ollama` and pull PATH out of `zsh -lc`.
fn login_path_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        #[cfg(unix)]
        {
            for sh in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
                if !std::path::Path::new(sh).exists() {
                    continue;
                }
                if let Ok(out) = Command::new(sh)
                    .args(["-lc", "printf %s \"$PATH\""])
                    .output()
                {
                    let s = String::from_utf8_lossy(&out.stdout).into_owned();
                    if !s.is_empty() {
                        return s
                            .split(':')
                            .filter(|p| !p.is_empty())
                            .map(PathBuf::from)
                            .collect();
                    }
                }
            }
        }
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default()
    })
    .as_slice()
}

/// macOS bundle search roots: system + per-user Applications.
#[cfg(target_os = "macos")]
fn bundle_search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

#[cfg(not(target_os = "macos"))]
fn bundle_search_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// True if a binary `name` is found in any of `dirs`.
/// On Windows, common executable extensions are appended.
fn binary_in(name: &str, dirs: &[PathBuf]) -> bool {
    for d in dirs {
        if d.join(name).exists() {
            return true;
        }
        #[cfg(windows)]
        for ext in ["exe", "cmd", "bat"] {
            if d.join(format!("{name}.{ext}")).exists() {
                return true;
            }
        }
    }
    false
}

/// True if a `.app` bundle named `name` exists in any of `dirs`.
fn bundle_in(name: &str, dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|d| d.join(name).exists())
}

/// Pure check: spec is satisfied if any candidate binary is found in
/// `path_dirs` or any candidate bundle is found in `bundle_dirs`.
/// Spec with empty `bins` and `bundles` always returns false (no positive
/// evidence possible).
fn check_install_spec(spec: &InstallSpec, path_dirs: &[PathBuf], bundle_dirs: &[PathBuf]) -> bool {
    spec.bins.iter().any(|b| binary_in(b, path_dirs))
        || spec.bundles.iter().any(|b| bundle_in(b, bundle_dirs))
}

/// Whether the agent's required app or CLI is installed on this machine.
/// Agents without a rule are conservatively reported as installed.
pub fn agent_installed(name: &str) -> bool {
    let Some(spec) = install_spec(name) else { return true };
    check_install_spec(&spec, login_path_dirs(), &bundle_search_dirs())
}

/// Batched install states (mirrors `running_states`).
pub fn installed_states(agents: &[Agent]) -> Vec<bool> {
    agents.iter().map(|a| agent_installed(&a.name)).collect()
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
        let mut cmd_proc = Command::new("cmd");
        cmd_proc.args(["/C", "start", "cmd", "/K", cmd]);
        cmd_proc.creation_flags(super::CREATE_NO_WINDOW);
        cmd_proc.spawn().context("failed to open cmd")?;
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
    #[cfg(windows)]
    cfg_cmd.creation_flags(super::CREATE_NO_WINDOW);
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
            #[cfg(windows)]
            cmd.creation_flags(super::CREATE_NO_WINDOW);
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
    let mut restore_cmd = Command::new(crate::ollama::ollama_bin());
    restore_cmd.args(["launch", agent, "--restore", "-y"]);
    #[cfg(windows)]
    restore_cmd.creation_flags(super::CREATE_NO_WINDOW);
    let status = restore_cmd
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
    if !agent_installed(&agent.name) {
        anyhow::bail!("{} is not installed", agent.display);
    }

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
        #[cfg(windows)]
        cmd.creation_flags(super::CREATE_NO_WINDOW);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique per-test tempdir under the OS temp root, auto-cleaned on Drop.
    /// We avoid pulling in `tempfile` as a dev-dep — the project has no
    /// dev-dependencies and this keeps it that way.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "llaunchpad-test-{}-{}-{n}",
                tag,
                std::process::id()
            ));
            fs::create_dir_all(&p).expect("create tempdir");
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unknown_agent_is_assumed_installed() {
        // Agents we don't have a rule for must not be flagged as missing —
        // a false negative here would block a legitimate launch.
        assert!(agent_installed("totally-not-a-real-agent"));
    }

    #[test]
    fn known_agents_have_install_specs() {
        for name in ["codex-app", "codex", "vscode", "cursor", "claude", "opencode"] {
            assert!(install_spec(name).is_some(), "missing spec for `{name}`");
        }
    }

    #[test]
    fn install_spec_table_contents() {
        // The table content is the contract with the rest of the app.
        // A typo here silently makes a launch fail in the field — pin it.
        let codex_app = install_spec("codex-app").unwrap();
        assert_eq!(codex_app.bins, &[] as &[&str]);
        assert_eq!(codex_app.bundles, &["Codex.app"]);

        let vscode = install_spec("vscode").unwrap();
        assert_eq!(vscode.bins, &["code"]);
        assert_eq!(vscode.bundles, &["Visual Studio Code.app", "VSCodium.app"]);

        let cursor = install_spec("cursor").unwrap();
        assert_eq!(cursor.bins, &["cursor"]);
        assert_eq!(cursor.bundles, &["Cursor.app"]);

        let codex = install_spec("codex").unwrap();
        assert_eq!(codex.bins, &["codex"]);
        assert_eq!(codex.bundles, &[] as &[&str]);

        let claude = install_spec("claude").unwrap();
        assert_eq!(claude.bins, &["claude"]);
        assert_eq!(claude.bundles, &[] as &[&str]);

        let opencode = install_spec("opencode").unwrap();
        assert_eq!(opencode.bins, &["opencode"]);
        assert_eq!(opencode.bundles, &[] as &[&str]);
    }

    #[test]
    fn empty_spec_is_never_satisfied() {
        // A spec with no candidates has no way to produce positive evidence —
        // must return false regardless of the dirs we pass.
        let spec = InstallSpec { bins: &[], bundles: &[] };
        let dir = TempDir::new("empty");
        let dirs = vec![dir.path().to_path_buf()];
        assert!(!check_install_spec(&spec, &dirs, &dirs));
    }

    #[test]
    fn binary_match_satisfies_spec() {
        let path_dir = TempDir::new("bin");
        // On Windows binary_in also looks for .exe/.cmd/.bat; create the bare
        // name first so the test passes on every platform.
        let exe = path_dir.path().join("foo");
        fs::write(&exe, b"#!/bin/sh\n").unwrap();
        let spec = InstallSpec { bins: &["foo"], bundles: &["Nope.app"] };
        let bundle_dir = TempDir::new("nob");
        assert!(check_install_spec(
            &spec,
            &[path_dir.path().to_path_buf()],
            &[bundle_dir.path().to_path_buf()],
        ));
    }

    #[test]
    fn bundle_match_satisfies_spec() {
        let bundle_dir = TempDir::new("bun");
        // `.app` is just a directory on macOS — for the purpose of `Path::exists`
        // any directory with the matching name works on every platform.
        fs::create_dir(bundle_dir.path().join("Demo.app")).unwrap();
        let spec = InstallSpec { bins: &["nope"], bundles: &["Demo.app"] };
        let path_dir = TempDir::new("nop");
        assert!(check_install_spec(
            &spec,
            &[path_dir.path().to_path_buf()],
            &[bundle_dir.path().to_path_buf()],
        ));
    }

    #[test]
    fn neither_match_fails_spec() {
        let path_dir = TempDir::new("pd");
        let bundle_dir = TempDir::new("bd");
        let spec = InstallSpec { bins: &["ghost"], bundles: &["Ghost.app"] };
        assert!(!check_install_spec(
            &spec,
            &[path_dir.path().to_path_buf()],
            &[bundle_dir.path().to_path_buf()],
        ));
    }

    #[test]
    fn binary_only_match_satisfies_or_spec() {
        // OR semantics: binary present but bundle missing must still satisfy.
        let path_dir = TempDir::new("orbin");
        fs::write(path_dir.path().join("bar"), b"").unwrap();
        let bundle_dir = TempDir::new("orbinb");
        let spec = InstallSpec { bins: &["bar"], bundles: &["Missing.app"] };
        assert!(check_install_spec(
            &spec,
            &[path_dir.path().to_path_buf()],
            &[bundle_dir.path().to_path_buf()],
        ));
    }

    #[test]
    fn bundle_only_match_satisfies_or_spec() {
        // OR semantics: bundle present but binary missing must still satisfy.
        let path_dir = TempDir::new("orbun");
        let bundle_dir = TempDir::new("orbunb");
        fs::create_dir(bundle_dir.path().join("Only.app")).unwrap();
        let spec = InstallSpec { bins: &["nope"], bundles: &["Only.app"] };
        assert!(check_install_spec(
            &spec,
            &[path_dir.path().to_path_buf()],
            &[bundle_dir.path().to_path_buf()],
        ));
    }

    #[test]
    fn binary_in_searches_all_dirs() {
        let d1 = TempDir::new("first");
        let d2 = TempDir::new("second");
        // place the binary only in the second dir — must still be found
        fs::write(d2.path().join("tool"), b"").unwrap();
        assert!(binary_in(
            "tool",
            &[d1.path().to_path_buf(), d2.path().to_path_buf()]
        ));
    }

    #[test]
    fn binary_in_missing_returns_false() {
        let d = TempDir::new("none");
        assert!(!binary_in("ghost", &[d.path().to_path_buf()]));
    }

    #[test]
    fn bundle_in_missing_returns_false() {
        let d = TempDir::new("nobnd");
        assert!(!bundle_in("Ghost.app", &[d.path().to_path_buf()]));
    }

    #[test]
    fn installed_states_length_matches_agents() {
        // Batch invariant: one bool per input agent, in order.
        let agents = vec![
            Agent { name: "totally-fake-1".into(), display: "A".into(), is_gui: false },
            Agent { name: "totally-fake-2".into(), display: "B".into(), is_gui: false },
            Agent { name: "totally-fake-3".into(), display: "C".into(), is_gui: false },
        ];
        let v = installed_states(&agents);
        assert_eq!(v.len(), agents.len());
        // unknowns are reported installed
        assert!(v.iter().all(|x| *x));
    }

    #[cfg(windows)]
    #[test]
    fn binary_in_finds_windows_extensions() {
        let d = TempDir::new("winext");
        // Windows executables typically end in .exe / .cmd / .bat — verify
        // each suffix is picked up by the lookup.
        for (name, ext) in [("foo", "exe"), ("bar", "cmd"), ("baz", "bat")] {
            fs::write(d.path().join(format!("{name}.{ext}")), b"").unwrap();
            assert!(
                binary_in(name, &[d.path().to_path_buf()]),
                "missing `{name}.{ext}`"
            );
        }
    }
}
