# ccp — Claude Code Profiles

[![CI](https://github.com/majiayu000/ccp/actions/workflows/ci.yml/badge.svg)](https://github.com/majiayu000/ccp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Run multiple Claude Code API providers side by side, each in its own isolated
config home. This workflow uses separate profile homes instead of changing a
shared provider selection for all launches. ccp never touches the global config: each
profile is a top-level directory (`~/.claude-kimi`, `~/.claude-deepseek`, …)
used as `CLAUDE_CONFIG_DIR`, plus per-process env vars injected at launch.
Sessions, logins, and history stay in the selected profile home. Resuming a chat
uses that home and the profile's current launch settings; editing a profile's endpoint changes subsequent launches from that profile.

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
- **Export / import** — JSON export of all profiles (env redacted by default; confirmed plaintext export for restorable backups)
- **Shared env overlay** — env vars applied to every profile at launch
- **`ccp doctor`** — checks CLI presence, file permissions (0600), broken
  template symlinks, plaintext tokens, unmanaged dirs

## Install

Requires Rust, macOS (for Keychain and terminal launching), and Claude Code on
your `PATH`. See [Contributing](CONTRIBUTING.md) for the development setup.

```sh
git clone https://github.com/majiayu000/ccp.git
cd ccp
cargo install --path .
ccp serve          # then open http://127.0.0.1:9847
```

Other commands: `ccp presets`, `ccp doctor`.

## First workflow: two providers, two homes

1. Start `ccp serve`, then create a named profile in the web GUI. Choose a preset or configure an Anthropic-compatible endpoint and its provider-required model variables. Save its token through the GUI; the profile file keeps only the Keychain reference.
2. Use that profile's connectivity test, then launch it. The test probes `/v1/models`; it is not proof that chat, tools, or every model will work.
3. Create a second profile with a different name and launch it separately. Each terminal gets its own `CLAUDE_CONFIG_DIR` and provider environment.
4. To resume, open history on the original profile card and choose the conversation. ccp restores its recorded project directory and launches with that profile's current settings.

Keep the built-in `default` profile for your existing `~/.claude` login. Shared skills and instruction symlinks are reusable context; they do not combine profile session histories.

## Profile FAQ

**Can I use this on Windows or Linux?** The documented workflow targets macOS Keychain and Terminal/iTerm launching. The presence of Rust code does not establish another platform's support. See [Contributing](CONTRIBUTING.md).

**Why does a connection test pass but Claude Code fail?** The probe checks the endpoint's models route and authentication response. Confirm the provider supports the Anthropic API expected by Claude Code and that its model variables are valid; a generic OpenAI endpoint is not automatically compatible.

**Does isolation freeze a provider forever?** No. Histories are separated by home, while launches use the profile's current configuration. Use separate profiles when you need separate endpoints rather than editing one profile to represent two providers.

**Where do I start troubleshooting?** Run `ccp doctor` for CLI availability, file permissions, broken symlinks, and token-storage problems. Report reproducible failures in [Issues](https://github.com/majiayu000/ccp/issues), with the ccp/Claude Code versions and macOS architecture. Remove tokens and transcript content before sharing. Installation is from source; [Releases](https://github.com/majiayu000/ccp/releases) is the place to check any published artifacts, not a promise of prebuilt downloads.

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

Transcript framing and conversation projections use `agent-sessions`. History
previews still inspect only the first 64 KiB, keep the first user text block,
and show at most 100 characters. Profile roots and resumable filename filtering
remain local policies. Usage aggregation retains its existing message-ID dedup,
raw timestamp date labels, and zero treatment for missing or invalid counters.

## Development

```sh
cargo test                                   # unit/API tests, no live server needed
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
