# ccp — Claude Code Profiles

[![CI](https://github.com/majiayu000/ccp/actions/workflows/ci.yml/badge.svg)](https://github.com/majiayu000/ccp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Run multiple Claude Code API providers side by side, each in its own isolated
config home. Switching tools like cc-switch mutate the global
`~/.claude/settings.json`, so every new or resumed session follows whichever
provider was configured last. ccp never touches the global config: each
profile is a top-level directory (`~/.claude-kimi`, `~/.claude-deepseek`, …)
used as `CLAUDE_CONFIG_DIR`, plus per-process env vars injected at launch.
Sessions, logins, and history stay pinned to their provider — an old chat
always resumes with the same source it started on.

## Features

- **Web GUI** (`ccp serve`) — create/edit/delete profiles from the browser;
  20+ built-in provider presets (Kimi, DeepSeek, Zhipu GLM, Bailian,
  OpenRouter, SiliconFlow, AtlasCloud, MiniMax, …) with pre-filled endpoints,
  or full custom env vars
- **Isolated homes** — profile `kimi` lives at `~/.claude-kimi`; the existing
  `~/.claude` is the built-in `default` profile, zero migration
- **Discovery & import** — finds existing `~/.claude-*` dirs and adopts them
- **One-click launch** — opens a new Terminal/iTerm window with the profile's
  env injected; resume any past session from its history list
- **Keychain tokens** — `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_API_KEY` are
  stored in the macOS Keychain; profile files only keep an `@keychain` marker
- **Session history** — per-profile conversation list with previews and
  one-click resume (lands back in the original project directory)
- **Usage stats** — per-day token usage and model breakdown parsed from local
  transcripts (no proxy, no network)
- **Connectivity test** — probes `{base}/v1/models` and reports latency +
  auth status
- **Export / import** — JSON backup of all profiles (tokens masked by default)
- **Shared env overlay** — env vars applied to every profile at launch
- **`ccp doctor`** — checks CLI presence, file permissions (0600), broken
  template symlinks, plaintext tokens, unmanaged dirs

## Install

```sh
cargo install --path .
ccp serve          # then open http://127.0.0.1:9847
```

Other commands: `ccp presets`, `ccp doctor`.

## Configuration

All ccp state lives under `~/.ccp/` (override with `$CCP_HOME`):

```
~/.ccp/
├── config.toml        # port = 9847 (default), terminal = "iterm" (default: Terminal.app)
├── shared.toml        # env applied to every profile (profile values win)
└── profiles/<name>.toml
```

`~/.ccp/config.toml`:

```toml
port = 9847            # overridden by $CCP_PORT and --port
terminal = "iterm"     # optional; default "terminal"
```

Profile file (`~/.ccp/profiles/kimi.toml`):

```toml
preset = "moonshot"
[env]
ANTHROPIC_BASE_URL = "https://api.moonshot.cn/anthropic"
ANTHROPIC_AUTH_TOKEN = "@keychain"   # real value lives in macOS Keychain
```

Any extra env var (model aliases, context limits, custom headers) can be set
per profile — the GUI has an advanced key-value editor.

New profiles automatically get symlinks to your global `CLAUDE.md`,
`agents/`, `skills/`, and a copy of your `mcpServers` from `~/.claude.json`.

## How it works

```
browser ──HTTP──> ccp serve (axum, 127.0.0.1 only)
                     ├─ profile CRUD under ~/.ccp/profiles/
                     ├─ tokens in macOS Keychain (service com.ccp.profiles)
                     ├─ launch: osascript → new Terminal/iTerm window with
                     │   env CLAUDE_CONFIG_DIR=~/.claude-<name> … claude
                     └─ history/usage: read-only scans of each home's
                         projects/**/*.jsonl
```

`~/.claude/settings.json` is never read or written by ccp.

## Development

```sh
cargo test                                   # 29 tests, no live server needed
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

Layout: `src/profile.rs` (store), `secret.rs` (keychain), `launch.rs`
(command builder + osascript), `sessions.rs` / `usage.rs` (transcript
scans), `connect.rs` (probe), `web.rs` (API), `static/index.html` (embedded
UI, no build step), `presets.toml` (provider data, cc-switch field shape).

See [SPEC.md](SPEC.md) for the design doc and milestone plan.

## License

MIT
