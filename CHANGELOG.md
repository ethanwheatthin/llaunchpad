# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/draugvar/llaunchpad/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/draugvar/llaunchpad/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/draugvar/llaunchpad/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/draugvar/llaunchpad/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/draugvar/llaunchpad/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/draugvar/llaunchpad/releases/tag/v0.1.0
