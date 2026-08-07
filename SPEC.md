# ccp — Claude Code Profiles (Web GUI)

## Goal

A local web GUI to manage multiple Claude Code API configurations ("profiles"),
each running in a fully isolated `CLAUDE_CONFIG_DIR`. Switching providers must
never affect other running or resumable sessions.

Replaces the global-mutation model (ccswitch-style) with per-process isolation.

## Why

- Global config mutation (`~/.claude/settings.json`) makes every new/resumed
  session follow the latest provider — old chats break when the model changes.
  cc-switch explicitly relies on Claude Code hot-reloading global config,
  which is exactly the mechanism that breaks running sessions.
- `CLAUDE_CONFIG_DIR` + per-process env vars give true isolation: sessions,
  settings, and history live per profile.

## Differentiation vs cc-switch

cc-switch is a *switcher* (mutates global config, one active provider).
ccp is an *isolator* (profiles coexist; each session is pinned at launch).

Borrowed from cc-switch: its provider config model (settingsConfig.env with
tier-model vars), the preset table format, connectivity test, import/export,
usage stats (parsed from session jsonl — no proxy needed), MCP copy-on-create.

Explicitly rejected: local proxy/failover, tray hot-switching (ccp has no
"switch" concept, only "launch"), cloud sync, deep links, multi-tool bloat.

## Non-goals (MVP)

- No desktop app (Tauri), no menubar.
- No npm/frontend build step.
- Linux support is best-effort; macOS is the target.

## Architecture

Single Rust binary `ccp`, axum server, serving one embedded HTML page
(vanilla JS + fetch, no build step). The browser is the GUI; the server is
the only thing touching the filesystem.

```
browser ──HTTP──> ccp serve (axum, localhost)
                     ├─ reads/writes profile configs under the data dir
                     ├─ profile homes are TOP-LEVEL dirs: ~/.claude, ~/.claude-<name>
                     ├─ launches sessions via osascript (Terminal.app)
                     └─ lists past sessions by scanning profile projects/
```

## Data layout

**Profile homes** follow the `~/.claude*` convention so they stay usable
without ccp (`CLAUDE_CONFIG_DIR=~/.claude-kimi claude` works standalone):

```
~/.claude/                 # reserved "default" profile (existing dir, zero migration)
~/.claude-kimi/            # profile "kimi" — CLAUDE_CONFIG_DIR
~/.claude-deepseek/        # profile "deepseek"
```

**ccp's own data** (server config + profile env) lives separately:

Data dir: `$CCP_HOME` env, else `~/.ccp/`

```
~/.ccp/
├── config.toml                     # server config (port etc.)
├── shared.toml                     # optional env overlay applied to EVERY profile
└── profiles/
    ├── default.toml                # env overlay for ~/.claude (usually empty)
    ├── kimi.toml                   # env for ~/.claude-kimi
    └── deepseek.toml
```

Profile name rules: `[a-z0-9][a-z0-9-]*`, because it maps to the directory
name `~/.claude-<name>`. The name `default` is reserved and maps to
`~/.claude`; it always exists, cannot be deleted, only its env overlay can
be edited.

## Profile config model (cc-switch compatible)

Follows cc-switch's `settingsConfig.env` shape, so configs can be ported
back and forth. Profile TOML (`~/.ccp/profiles/kimi.toml`):

```toml
name = "kimi"
preset = "moonshot"        # optional, records which preset it came from
# home defaults to ~/.claude-<name>; explicit override allowed for imports:
# home = "/Users/x/custom-dir"

[env]
ANTHROPIC_BASE_URL = "https://api.moonshot.cn/anthropic"
ANTHROPIC_AUTH_TOKEN = "sk-..."
# Optional cc-switch-standard vars (only present when set):
# ANTHROPIC_MODEL = "kimi-k2-0905-preview"
# ANTHROPIC_DEFAULT_HAIKU_MODEL = "..."
# ANTHROPIC_DEFAULT_SONNET_MODEL = "..."
# ANTHROPIC_DEFAULT_OPUS_MODEL = "..."
# ANTHROPIC_SMALL_FAST_MODEL = "..."
# CLAUDE_CODE_MAX_CONTEXT_TOKENS = "262144"
# ...any other env var via the advanced KV editor
```

**Shared overlay** (`shared.toml`, cc-switch's "Shared Config Snippet"
equivalent): a `[env]` map merged under every profile at launch — profile
vars win on conflict. Use for things like
`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`.

## Provider presets

Preset table is **data, not code**: an embedded `presets.toml` (via
`include_str!`) with cc-switch's field shape so it can be synced from
upstream:

```toml
[[preset]]
key = "moonshot"
label = "Kimi (Moonshot)"
category = "cn_official"   # official | cn_official | third_party | aggregator | cloud_provider
website_url = "https://platform.moonshot.cn"
api_key_url = "https://platform.moonshot.cn/console/api-keys"
api_format = "anthropic"   # anthropic | openai_chat | gemini_native (MVP: anthropic only)
[preset.env]
ANTHROPIC_BASE_URL = "https://api.moonshot.cn/anthropic"
```

Initial set (base URLs ported from cc-switch's preset source, verified at
implementation time): anthropic-official, moonshot, moonshot-coding,
deepseek, zhipu (cn/en), bailian, openrouter, siliconflow, atlascloud,
minimax, volcengine-ark, baidu-qianfan, modelscope, stepfun, longcat,
xiaomi-mimo, aihubmix, dmxapi, packycode — plus `custom` (blank).
The web form offers presets grouped by category; selecting one pre-fills
URL + tier-model defaults so the user only pastes a token.

Server config (`config.toml`): `port = 9847` default, overridable by
`CCP_PORT`. No hardcoded ports in code.

## Discovery / import

- `~/.claude` is always shown as the `default` profile.
- On startup (and via a UI "scan" button), ccp scans `$HOME` for unmanaged
  `~/.claude-*` directories and offers one-click import: creates
  `~/.ccp/profiles/<name>.toml` pointing at the existing dir, leaving its
  contents untouched.

## HTTP API

| Method | Path | Purpose |
|---|---|---|
| GET | `/` | Embedded single-page UI |
| GET | `/api/presets` | List presets (grouped by category) |
| GET | `/api/profiles` | List managed profiles + unmanaged `~/.claude-*` dirs found |
| POST | `/api/profiles` | Create profile `{name, preset?, env: {K: V}}` — writes profile TOML (0600), creates `~/.claude-<name>/`, applies template symlinks |
| POST | `/api/profiles/import` | Adopt an unmanaged `~/.claude-<name>` dir `{name}` |
| PUT | `/api/profiles/{name}` | Update env vars (merge; empty value deletes key) |
| DELETE | `/api/profiles/{name}` | Unmanage profile (requires `?confirm=true`; the `~/.claude-<name>` dir is kept unless `?purge=true`; `default` cannot be deleted) |
| GET/PUT | `/api/shared` | Read/update the shared env overlay (values masked on read) |
| GET | `/api/profiles/{name}/sessions` | Scan `home/projects/**/*.jsonl`, return recent sessions (id, cwd, first user message preview, mtime) |
| POST | `/api/profiles/{name}/launch` | Body `{resume?: sessionId, cwd?: string}` → spawn Terminal window with env + `claude [--resume id]` |
| POST | `/api/profiles/{name}/test` | M3: connectivity check — timed request to the profile's base URL with its token; returns latency + auth ok/fail |
| GET/POST | `/api/export`, `/api/import` | M3: JSON export of all profiles (tokens masked unless `?include_secrets=true`), import with conflict policy `skip|overwrite` |

Token values are never returned by GET APIs (only key names + masked value).

## Launch mechanism (macOS)

Server merges `shared.toml` env under the profile env, then runs:

```
osascript -e 'tell application "Terminal" to do script "env CLAUDE_CONFIG_DIR=$HOME/.claude-kimi ANTHROPIC_BASE_URL=... ... claude --resume <id>"'
```

Env values are single-quote escaped. iTerm supported via
`terminal = "iterm"` in config.toml (implemented in M4).

## Template inheritance

On profile creation (NOT for default or imports), symlink from `~/.claude/`
into the new profile home: `CLAUDE.md`, `agents/`, `skills/` (skip if
missing). Configurable later.

MCP copy-on-create (implemented in M4): `mcpServers` from the default
profile's `~/.claude.json` is copied into the new home on creation (each
profile has isolated config, so MCP servers must be provisioned per profile;
copying saves re-setup).

## Milestones

- **M1** ✅: axum skeleton, presets (embedded presets.toml), profile CRUD + discovery/import API, shared overlay, embedded page with preset dropdown + tier-model fields + advanced KV editor
- **M2** ✅: `/launch` via osascript, sessions list + resume button in UI
- **M3** ✅: `/test` connectivity check, export/import JSON, `ccp doctor` subcommand (CLI support check for CLAUDE_CONFIG_DIR, env file perms, broken symlinks)
- **M4** ✅: keychain storage via `keyring` crate, usage stats parsed from session jsonl (per-profile tokens, trend chart), MCP copy-on-create, iTerm adapter
- **M5** (phase 3, exploratory, not started): same profile abstraction for Codex (`~/.codex-<name>` via `CODEX_HOME`) and Gemini (`~/.gemini-<name>`)

## Done-when (M1+M2 acceptance)

1. `ccp serve` starts; browser page shows the existing `~/.claude` as `default` profile with its real session history
2. Create a profile from a preset (e.g. moonshot) entirely from the web form → `~/.claude-kimi/` appears at top level
3. Launch both default and kimi from the page → two Terminal windows, each `echo $CLAUDE_CONFIG_DIR` shows the right dir
4. Chat in kimi, close it, find it in kimi's session list, click resume → same conversation continues with the same provider
5. `~/.claude` contents untouched except by claude itself; ccp never writes into any profile home except template symlinks at creation

## Testing

- Rust: unit tests for profile store (CRUD, perms, masking, name validation, import), preset parsing, shared-overlay merge precedence, osascript command builder (escaping), session scanner (fixture jsonl)
- API: integration tests via `tower::ServiceExt` in-memory, no live server
- `cargo clippy -- -D warnings` + `cargo fmt` clean before each commit
