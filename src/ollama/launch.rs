use crate::ollama::Agent;
use crate::terminal::Terminal;
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

// ───────────────────────── shell safety ─────────────────────────

/// Strip everything that is not part of a plain base URL, so the result is safe
/// to interpolate into a shell line for `OLLAMA_HOST=...`.
fn shell_safe_url(url: &str) -> String {
    url.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "://.-_@[]".contains(*c))
        .collect()
}

/// Strip everything that is not safe inside a double-quoted shell string. We
/// use this for the `cd "<dir>"` prefix we hand to Terminal.app on macOS, so a
/// directory with `\"` or `;` can't break out of the quoting.
fn shell_safe_dir(dir: &str) -> String {
    dir.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "/._- ".contains(*c))
        .collect()
}

// ───────────────────────── directory picker ─────────────────────────

/// Open a native folder picker. The dialog is modal and blocks; callers
/// should run this off the UI thread.
pub fn pick_directory(start_dir: Option<&str>) -> Option<String> {
    let _start = start_dir.filter(|d| !d.is_empty());

    #[cfg(target_os = "macos")]
    {
        // `choose folder` returns an alias; convert to POSIX. We deliberately
        // don't seed a `default location`: a bad path makes osascript error.
        let script =
            "POSIX path of (choose folder with prompt \"Select working directory\")";
        let out = Command::new("osascript")
            .args(["-e", script])
            .output()
            .ok()?;
        if !out.status.success() {
            return None; // user pressed Cancel
        }
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return if p.is_empty() { None } else { Some(p) };
    }

    #[cfg(target_os = "windows")]
    {
        // Drive the Win32 folder browser through PowerShell. CREATE_NO_WINDOW
        // keeps a console from flashing up. The seed path is single-quoted with
        // `'` doubled so a path can't break out of the string literal.
        let seed = match start {
            Some(d) => format!(
                "$s = '{}'; if (Test-Path $s) {{ $dlg.SelectedPath = $s }}\n",
                d.replace('\'', "''")
            ),
            None => String::new(),
        };
        let ps = format!(
            "Add-Type -AssemblyName System.Windows.Forms | Out-Null\n\
             $dlg = New-Object System.Windows.Forms.FolderBrowserDialog\n\
             $dlg.Description = 'Select working directory'\n\
             {seed}\
             if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ \
             [Console]::Out.Write($dlg.SelectedPath) }}"
        );
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-STA", "-NonInteractive", "-Command", &ps]);
        c.creation_flags(super::CREATE_NO_WINDOW);
        let out = c.output().ok()?;
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return if p.is_empty() { None } else { Some(p) };
    }

    #[cfg(target_os = "linux")]
    {
        // Try zenity, then kdialog. zenity seeds via --filename (trailing slash
        // tells it the path is a directory).
        let mut zen = Command::new("zenity");
        zen.args(["--file-selection", "--directory", "--title=Select working directory"]);
        if let Some(d) = start {
            zen.arg(format!("--filename={}/", d.trim_end_matches('/')));
        }
        if let Ok(out) = zen.output() {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !p.is_empty() {
                    return Some(p);
                }
            }
        }
        let mut kde = Command::new("kdialog");
        kde.arg("--getexistingdirectory");
        kde.arg(start.unwrap_or("."));
        if let Ok(out) = kde.output() {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !p.is_empty() {
                    return Some(p);
                }
            }
        }
        return None;
    }
}

// ───────────────────────── terminal spawn ─────────────────────────

/// Run a shell command line in a new terminal window.
/// If `ollama_host` is provided it is forwarded as `OLLAMA_HOST` so the agent
/// connects to the right server. If `working_dir` is provided, the command
/// runs with that directory as its working directory.
fn spawn_in_terminal(
    cmd: &str,
    ollama_host: Option<&str>,
    working_dir: Option<&str>,
    terminal: &Terminal,
) -> Result<()> {
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

    // On macOS the Terminal.app command-string path lets us encode the
    // working directory with a `cd` prefix; Terminal.app doesn't inherit
    // our cwd, so a `cd` is the only way.
    //
    // On Linux/Windows the terminal implementations in `crate::terminal`
    // build their own Command and don't accept a working_dir; we re-do
    // the spawn here so we can set `current_dir` before invoking the
    // emulator. The emulator list mirrors what the Linux/Windows
    // platforms use in `crate::terminal`.
    #[cfg(target_os = "macos")]
    {
        let full: String = match working_dir {
            Some(dir) if !dir.is_empty() => {
                format!("cd \"{}\" && {cmd}", shell_safe_dir(dir))
            }
            _ => cmd.to_string(),
        };
        terminal.spawn(&full)
    }
    #[cfg(target_os = "linux")]
    {
        // Mirror crate::terminal's platform::spawn logic so we can set
        // current_dir on the emulator Command. We use the same hold-open
        // trick (\"cmd; exec $SHELL\") so the window stays after the
        // command exits.
        let hold = format!("{cmd}; exec ${{SHELL:-/bin/bash}}");
        let candidates: &[(&str, &[&str])] = &[
            ("x-terminal-emulator", &["-e", "bash", "-lc"]),
            ("gnome-terminal", &["--", "bash", "-lc"]),
            ("konsole", &["-e", "bash", "-lc"]),
            ("xfce4-terminal", &["-e", "bash", "-lc"]),
            ("xterm", &["-e", "bash", "-lc"]),
            ("alacritty", &["-e", "bash", "-lc"]),
            ("kitty", &["bash", "-lc"]),
        ];
        for (bin, args) in candidates {
            let mut c = Command::new(bin);
            c.args(*args).arg(&hold);
            if let Some(dir) = working_dir {
                if !dir.is_empty() {
                    c.current_dir(dir);
                }
            }
            if c.spawn().is_ok() {
                return Ok(());
            }
        }
        anyhow::bail!("no terminal emulator found (tried gnome-terminal, konsole, xterm…)")
    }
    #[cfg(target_os = "windows")]
    {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "cmd", "/K", cmd]);
        c.creation_flags(super::CREATE_NO_WINDOW);
        if let Some(dir) = working_dir {
            if !dir.is_empty() {
                c.current_dir(dir);
            }
        }
        c.spawn().context("failed to open cmd")?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        // Stub for any other target — `terminal` is intentionally
        // ignored, the caller should have filtered us out.
        let _ = (cmd, working_dir, terminal);
        anyhow::bail!("no terminal support on this platform")
    }
}

// ───────────────────────── GUI helpers ─────────────────────────

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
            if let Some(prov_body) = sections.get(&prov_header).cloned() {
                out.push('\n');
                out.push_str(&format!("[{prov_header}]\n"));
                for l in &prov_body {
                    if !l.trim().is_empty() {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
            }
        }
        let _ = std::fs::write(home.join(format!("{name}.config.toml")), out);
    }

    // strip the profiles from config.toml
    let mut new_cfg = String::new();
    let mut in_profile = false;
    let mut skip_provider = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("[profiles.") && t.ends_with(']') {
            in_profile = true;
            skip_provider = false;
            continue;
        }
        if in_profile {
            if t.starts_with('[') && t.ends_with(']') {
                in_profile = false;
                if t.starts_with("[model_providers.") {
                    skip_provider = true;
                    continue;
                }
            }
            continue;
        }
        if t.starts_with("profile") && t.contains('=') {
            continue;
        }
        if skip_provider {
            if t.starts_with('[') && t.ends_with(']') {
                skip_provider = false;
            } else {
                continue;
            }
        }
        new_cfg.push_str(line);
        new_cfg.push('\n');
    }
    let _ = std::fs::write(&cfg, new_cfg);
    Ok(())
}

// ───────────────────────── codex (GUI + CLI) ─────────────────────────

/// Codex (GUI app or CLI): `ollama launch` writes a legacy profile config that
/// current Codex rejects. Configure first (`--config`), migrate the profile into
/// its own file, then launch Codex ourselves.
fn launch_codex(
    agent: &str,
    is_gui: bool,
    model: &str,
    ollama_host: Option<&str>,
    working_dir: Option<&str>,
    terminal: &Terminal,
) -> Result<()> {
    // close the GUI app if it is already open (relaunch)
    #[cfg(target_os = "macos")]
    if is_gui {
        let probe = Agent { name: agent.to_string(), display: String::new(), is_gui: true, logo: String::new() };
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
            if let Some(dir) = working_dir {
                if !dir.is_empty() {
                    cmd.current_dir(dir);
                }
            }
            cmd.spawn().context("failed to launch codex-app")?;
        }
    } else {
        // CLI: run codex against the migrated profile in a terminal
        spawn_in_terminal("codex --profile ollama-launch", ollama_host, working_dir, terminal)?;
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
        })
        .map(|p| p.exists())
        .unwrap_or(false)
}

pub fn restore_agent(agent: &str) -> Result<()> {
    let mut cmd = Command::new(crate::ollama::ollama_bin());
    cmd.args(["launch", "--restore", agent]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.status().context("failed to run `ollama launch --restore`")?;
    Ok(())
}

// ───────────────────────── public entry point ─────────────────────────

/// Launch (or relaunch) an agent with the given model via `ollama launch`.
/// `ollama_host` is forwarded as `OLLAMA_HOST` when set, routing the agent
/// to a custom Ollama server instead of the default localhost.
/// `working_dir` is the directory the agent should run in (None = inherit).
pub fn launch_agent(
    agent: &Agent,
    model: &str,
    ollama_host: Option<&str>,
    working_dir: Option<&str>,
    terminal: &Terminal,
) -> Result<()> {
    if !agent_installed(&agent.name) {
        anyhow::bail!("{} is not installed", agent.display);
    }

    match agent.name.as_str() {
        "codex-app" => {
            return launch_codex("codex-app", true, model, ollama_host, working_dir, terminal)
        }
        "codex" => {
            return launch_codex("codex", false, model, ollama_host, working_dir, terminal)
        }
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
        if let Some(dir) = working_dir {
            if !dir.is_empty() {
                cmd.current_dir(dir);
            }
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
        spawn_in_terminal(&cmd, ollama_host, working_dir, terminal)?;
    }
    Ok(())
}

// ───────────────────────── tests ─────────────────────────

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
        fn new(prefix: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "{}-{}-{}-{}",
                prefix,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                n,
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &PathBuf {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn empty_spec_is_never_satisfied() {
        let spec = InstallSpec { bins: &[], bundles: &[] };
        let dir = TempDir::new("empty");
        let dirs = vec![dir.path().to_path_buf()];
        assert!(!check_install_spec(&spec, &dirs, &dirs));
    }

    #[test]
    fn binary_match_satisfies_spec() {
        let path_dir = TempDir::new("bin");
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
        let agents = vec![
            Agent { name: "totally-fake-1".into(), display: "A".into(), is_gui: false, logo: String::new() },
            Agent { name: "totally-fake-2".into(), display: "B".into(), is_gui: false, logo: String::new() },
            Agent { name: "totally-fake-3".into(), display: "C".into(), is_gui: false, logo: String::new() },
        ];
        let v = installed_states(&agents);
        assert_eq!(v.len(), agents.len());
        assert!(v.iter().all(|x| *x));
    }

    #[cfg(windows)]
    #[test]
    fn binary_in_finds_windows_extensions() {
        let d = TempDir::new("winext");
        for (name, ext) in [("foo", "exe"), ("bar", "cmd"), ("baz", "bat")] {
            fs::write(d.path().join(format!("{name}.{ext}")), b"").unwrap();
            assert!(
                binary_in(name, &[d.path().to_path_buf()]),
                "missing `{name}.{ext}`"
            );
        }
    }
}
