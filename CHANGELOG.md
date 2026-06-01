# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-06-01

### Added
- **"Not installed" detection** — agents whose required app or CLI is missing on
  the machine show a peach **not installed** badge in the dropdown, with the
  badge/text dimmed. The Launch button is disabled and a `● not installed`
  hint is shown next to it. The launch path runs a pre-flight check that
  fails with a clear `<Agent> is not installed` error instead of returning a
  false ✓. Detection is rule-based for known agents (Codex App, VS Code,
  Cursor, Codex, Claude Code, opencode); unknown integrations are
  conservatively reported as installed so an unrecognised agent is never
  blocked.
- Install state is refreshed alongside running state on every poll (5s) and
  is included in the change-detection signature, so the badge updates the
  moment the missing app appears in `/Applications` or the binary lands in
  PATH.

### Changed
- Agent dropdown popup auto-expands beyond the field width (min 320px) so the
  `not installed` badge fits alongside long agent names without overflowing
  the panel. The name `Text` now elides with `…` when space is tight; the
  popup container and each row clip overflow at their rounded border.

### Internal
- Install detection is split into pure helpers (`check_install_spec`,
  `binary_in`, `bundle_in`) with 11 new unit tests covering the rule table,
  OR semantics, missing/empty specs, multi-directory lookup, the batch
  invariant, and the Windows `.exe` / `.cmd` / `.bat` extension fallback.

## [0.4.0] - 2026-06-01

### Added
- **Custom Ollama base URL** — point Llaunchpad at any Ollama server (local or remote).
  A new `Ollama host` field plus a **Test** button probe `/api/version` and `/api/tags`,
  report the server version, and persist the URL in prefs.
- **Local models in the dropdown** — after a successful test, the server's local models
  are listed first with a teal **local** badge and a teal dot in the selector. Local
  entries take precedence over cloud entries with the same name.
- **Dismissible status banner** — the success/error banner at the bottom now has a close
  button (×) so it can be cleared without waiting for the next event.

### Changed
- **CI** now triggers on pull requests as well as `main`, cancels superseded runs on the
  same ref, and gates the release job on a **new** `version` in `Cargo.toml` — pushing
  a build with an already-tagged version fails the workflow.
- Model list refresh preserves the currently selected model by name when the list is
  rebuilt, so the highlight doesn't drop just because the list shape changed.

### Fixed
- **Windows**: font-based icons (`▾`, `×`, etc.) render as glyphs instead of boxes —
  explicit `font-family` fallback list on the affected `Text` elements.
- **Shell command safety**: custom Ollama host URLs are validated/escaped before being
  used in spawned shell commands.
- `ollama launch` argument order fixed so Codex / Claude Code pick up the selected
  model reliably.

## [0.3.0] - 2026-05-31

### Added
- **Restore button** next to Launch — runs `ollama launch <agent> --restore` to return an
  agent to its original (pre-Ollama) profile. Enabled only when Ollama has a restore backup
  for that agent (`~/.ollama/launch/<agent>-restore.json`); greyed out otherwise.

### Changed
- Dropdowns open downward from the field with the selected item at the head and highlighted.

### Fixed
- Background refresh no longer rebuilds the lists unless their content actually changes,
  so an open dropdown keeps mouse hover and selection.

## [0.2.1] - 2026-05-31

### Fixed
- Status bar is no longer flush against the window bottom — restored breathing room below it.

### Changed
- README and repo description updated to reflect cross-platform support.

## [0.2.0] - 2026-05-31

### Added
- **Cross-platform**: builds and runs on macOS, Linux, and Windows. CI now produces a
  release artifact for each (`*-macos-universal.tar.gz`, `*-linux-x86_64.tar.gz`,
  `*-windows-x86_64.zip`).
- App version shown in the bottom-right of the window.

### Fixed
- **Codex CLI/App**: `ollama launch` (0.24) writes a legacy `[profiles.…]` table that current
  Codex rejects. Llaunchpad now migrates the profile into its own `~/.codex/<name>.config.toml`
  and strips the legacy table/selector from `config.toml`, then launches Codex.

### Changed
- Platform-specific launch paths: terminal spawning, GUI quit/relaunch, prefs location, and
  `ollama` binary resolution are now adapted per OS.

## [0.1.3] - 2026-05-31

### Fixed
- `ollama` binary resolution now probes the user's login-shell PATH first, then common
  locations including the `Ollama.app` bundle. Works whether Ollama was installed via
  Homebrew, the macOS app, or any custom PATH — not just brew.

## [0.1.2] - 2026-05-31

### Fixed
- Bundled app now resolves the absolute `ollama` path (Homebrew/usr/local), so it works
  when launched from Finder/Dock where GUI apps get a minimal `PATH`. Previously failed with
  "failed to run `ollama launch --help`".

### Changed
- Homebrew cask strips the quarantine attribute via `postflight`, so the app opens without
  the Gatekeeper prompt — no right-click needed.

## [0.1.1] - 2026-05-31

### Added
- GitHub Actions release pipeline: universal macOS build, `.app` packaging, GitHub Release, and Homebrew tap update — all triggered by a version bump on `main`.
- Homebrew install: `brew install --cask draugvar/llaunchpad/llaunchpad`.
- Optional (secret-gated) Developer ID signing + notarization step in CI.

### Changed
- `bundle.sh` now reads the version from `Cargo.toml` instead of hardcoding it.

## [0.1.0] - 2026-05-31

### Added
- Native macOS GUI (Rust + Slint) wrapping `ollama launch <agent> --model <model>`.
- Live agent list parsed from `ollama launch --help`.
- Full Ollama Cloud model catalog fetched from `ollama.com/v1/models`, refreshed in the background.
- Inline command preview with agent + model dropdowns.
- Colored initials badges per agent for at-a-glance recognition.
- Automatic cloud model name normalization (`glm-4.6` → `glm-4.6:cloud`, `gpt-oss:120b` → `gpt-oss:120b-cloud`).
- GUI agents are quit and relaunched cleanly; CLI agents spawn in Terminal.
- Codex App launch fix: strips the legacy `profile =` line current Codex rejects.
- Accurate "running" detection via the app's bundle executable path.
- Persisted last-used agent + model across runs.
- App icon, banner, and `bundle.sh` to assemble `Llaunchpad.app`.

[Unreleased]: https://github.com/draugvar/llaunchpad/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/draugvar/llaunchpad/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/draugvar/llaunchpad/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/draugvar/llaunchpad/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/draugvar/llaunchpad/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/draugvar/llaunchpad/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/draugvar/llaunchpad/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/draugvar/llaunchpad/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/draugvar/llaunchpad/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/draugvar/llaunchpad/releases/tag/v0.1.0
