# Security Policy

## Supported Versions

Only the latest commit on `main` receives security fixes. ccp has no
versioned releases yet.

## Reporting a Vulnerability

Please report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/majiayu000/ccp/security/advisories/new).

**Do not open a public issue** for security problems — especially anything
involving token handling, Keychain storage, file permissions, or the local
HTTP server.

You can expect an acknowledgement within a few days. If the report is
accepted, a fix lands on `main` and the advisory is published after.

## Scope notes

ccp runs a localhost-only HTTP server (127.0.0.1) that can read and write
files under `~/.ccp/` and launch terminal windows via osascript. API tokens
are stored in the macOS Keychain; profile files hold only an `@keychain`
marker and are written with 0600 permissions. Issues that weaken any of
these properties are in scope and treated as security bugs.
