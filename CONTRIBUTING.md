# Contributing to ccp

Thanks for your interest in contributing!

## Getting started

```sh
git clone https://github.com/majiayu000/ccp.git
cd ccp
cargo build
```

## Run

```sh
cargo run -- serve          # web GUI on http://127.0.0.1:9847
cargo run -- presets        # list built-in provider presets
cargo run -- doctor         # sanity checks (exit 1 on problems)
```

`ccp` writes only under `~/.ccp/` (override with `$CCP_HOME`) and creates
profile homes as `~/.claude-<name>/`. For a fully sandboxed run, point
`$CCP_HOME` and `$HOME`-level experiments at a temp dir.

## Test & lint

Run these before opening a PR — CI enforces all of them:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Tests are in-memory (temp dirs, a fake secret store, an injected launcher) —
they never touch your real `~/.claude*` dirs or Keychain.

## Design rules

- The web UI is a single embedded `static/index.html` — no npm/build step,
  and we intend to keep it that way.
- `presets.toml` is data, not code; new providers go there (keep the
  cc-switch field shape so it can be synced from upstream).
- Never write into a profile home except template symlinks at creation.
  `~/.claude/settings.json` is never read or written.
- Tokens (`ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_API_KEY`) always go to the
  macOS Keychain; profile TOMLs only store the `@keychain` marker, files
  are written 0600, and APIs never return raw token values.
- macOS is the target platform (keyring apple-native, osascript launch).
  Linux support is best-effort.

## Pull requests

- Keep PRs focused; describe user-visible behavior changes in the body.
- English for code, comments, commit messages, and PR text.
- Squash merge is used; one logical change per PR.
