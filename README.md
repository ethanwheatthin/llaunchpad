<p align="center">
  <img src="assets/banner.png" alt="Llaunchpad" width="100%">
</p>

<p align="center">
  <a href="https://github.com/draugvar/llaunchpad/stargazers"><img src="https://img.shields.io/github/stars/draugvar/llaunchpad?style=for-the-badge&logo=github&color=89b4fa&labelColor=181825" alt="Stars"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-cba6f7?style=for-the-badge&labelColor=181825" alt="License"></a>
  <img src="https://img.shields.io/badge/platform-macOS-a6e3a1?style=for-the-badge&logo=apple&labelColor=181825" alt="Platform">
  <img src="https://img.shields.io/badge/made%20with-Rust%20%2B%20Slint-fab387?style=for-the-badge&logo=rust&labelColor=181825" alt="Rust + Slint">
</p>

<p align="center">
  <b>A native macOS launcher that wires any Ollama coding agent to any cloud model — in one click.</b>
</p>

---

## What is this?

[Ollama](https://ollama.com) ships `ollama launch <agent> --model <model>` to point coding
agents (Codex, Claude Code, Cursor, …) at models you run locally or in the
[Ollama Cloud](https://ollama.com/cloud). Doing it by hand means remembering agent tokens,
the exact cloud model names, and re-typing the command every time.

**Llaunchpad** is a tiny, good-looking GUI around that command:

- 🧩 **Pick an agent** from a dropdown — populated live from `ollama launch --help`.
- ☁️ **Pick a cloud model** from a dropdown — the full Ollama Cloud catalog, fetched in the background.
- 🚀 **One click** builds and runs `ollama launch <agent> --model <model>` for you.
- 🔁 Already running? It **closes and relaunches** the app cleanly.
- 💾 Remembers your **last selection** between runs.

<p align="center">
  <img src="assets/screenshot.png" alt="Llaunchpad screenshot" width="720">
</p>

## Features

| | |
|---|---|
| **Live agent list** | Parsed from `ollama launch --help` — always in sync with your Ollama version. |
| **Full cloud catalog** | Every model from `ollama.com/v1/models`, refreshed in the background every 5s. |
| **Correct model names** | Cloud ids are auto-normalized to launchable refs (`glm-4.6` → `glm-4.6:cloud`, `gpt-oss:120b` → `gpt-oss:120b-cloud`). |
| **GUI & CLI agents** | GUI apps (Codex, VS Code) are opened/relaunched; CLI agents spawn in Terminal. |
| **Codex App fix** | Strips the legacy `profile =` line modern Codex rejects, so launches just work. |
| **Smart "running" badge** | Detects the real GUI process by bundle path — no false positives from background helpers. |
| **Persisted state** | Your last agent + model are restored on the next launch. |
| **Native & light** | Pure Rust + [Slint](https://slint.dev), single ~14 MB binary, no Electron. |

## Install

### Prerequisites
- macOS 11+
- [Ollama](https://ollama.com/download) installed and signed in (`ollama signin`) for cloud models.

### Homebrew (recommended)
```bash
brew install --cask draugvar/llaunchpad/llaunchpad
```
Homebrew strips the quarantine flag, so the app opens normally — no right-click dance.

### Download
Or grab `llaunchpad-macos-universal.tar.gz` from the [Releases](https://github.com/draugvar/llaunchpad/releases)
page, unzip, move `Llaunchpad.app` to `/Applications`.

> The build is not notarized (no Apple Developer Program). On a direct download,
> first launch needs right-click → **Open**. Installing via Homebrew avoids this.

### Build from source
```bash
git clone https://github.com/draugvar/llaunchpad.git
cd llaunchpad
cargo build --release
./bundle.sh            # produces Llaunchpad.app
open Llaunchpad.app
```

## Usage

1. Open Llaunchpad.
2. Choose an **agent** and a **cloud model** from the inline dropdowns.
3. Hit **Launch**. The status bar confirms what was started.

That's it — Llaunchpad runs `ollama launch <agent> --model <model> -y` under the hood and
brings the agent up configured against your chosen model.

## How it works

```
┌─────────────┐   ollama launch --help   ┌──────────────┐
│   agents    │ ◀──────────────────────  │              │
├─────────────┤                          │  Llaunchpad  │
│   models    │ ◀── ollama.com/v1/models │   (Rust +    │
└─────────────┘                          │    Slint)    │
        │  ollama launch <agent>         │              │
        └──── --model <model> -y ──────▶ └──────────────┘
```

- **Agents** come from the *Supported integrations* block of `ollama launch --help`.
- **Models** come from the Ollama Cloud catalog and are normalized to runnable refs.
- **Launching** spawns `ollama launch`; for GUI agents the running app is quit first, then reopened.

## Supported agents

`claude` · `codex-app` · `codex` · `vscode` · `cursor` · `opencode` · `copilot` · `droid` ·
`kimi` · `cline` · `hermes` · `openclaw` · `pi` · `pool`

*(whatever your installed `ollama` reports — the list is dynamic.)*

> ⚠️ Not every cloud model supports agentic tool-calling. For coding agents prefer
> `qwen3-coder`, `deepseek`, `glm-*`, `kimi-k2*`, `gpt-oss`, `minimax-m2*`. Small/preview/vision
> models may return `Invalid tool type`.

## Development

```bash
cargo run                 # debug run
cargo test                # unit tests (agent parsing, model naming)
cargo build --release && ./bundle.sh
```

Project layout:
```
src/
  main.rs            UI wiring, background refresh, state persistence
  config.rs          last-used selection (prefs.json)
  ollama/
    agents.rs        parse `ollama launch --help`
    models.rs        fetch + normalize cloud catalog
    launch.rs        spawn / quit / relaunch, Codex config fix
ui/app.slint         Slint UI + theme
assets/              icon, banner, screenshot
bundle.sh            assemble Llaunchpad.app
```

## Contributing

Issues and PRs welcome. Good first contributions: more agent ⇄ process mappings,
Linux/Windows support, a model capability filter.

## License

MIT © [draugvar](https://github.com/draugvar) — see [LICENSE](LICENSE).

<sub>Not affiliated with Ollama. Built with ❤️ and a lot of `cargo build`.</sub>
