# Headroom-desktop (free)

> GitHub 项目：[`OneBigMoon/Headroom-desktop`](https://github.com/OneBigMoon/Headroom-desktop)

Headroom Local Community is an unofficial, local-only desktop edition derived
from the MIT-licensed
[`gglucass/headroom-desktop`](https://github.com/gglucass/headroom-desktop)
project and other open-source components. It is not affiliated with or endorsed
by any upstream paid Headroom product.

## 下载

从 [GitHub Releases](https://github.com/OneBigMoon/Headroom-desktop/releases/latest) 下载：macOS 使用稳定版 universal DMG；Windows 11 x64 preview（预览版）使用同一 Release 中的 NSIS `.exe` 安装包。

This fork keeps the open-source local proxy, client connectors, token and
savings dashboard, RTK, MarkItDown, and local add-on management. It does not
provide or require a Headroom account, email login, trial, subscription,
pricing, checkout, cancellation flow, marketing telemetry, or official update
service.

## Isolation from the upstream paid app

Community uses its own runtime identity and never installs over the official
application:

| Resource | Community value |
|---|---|
| Product name | `Headroom Local Community` |
| Bundle ID | `org.headroomlocal.community` |
| App data | `HeadroomLocalCommunity` |
| headroom-ai workspace | `~/.headroom-local-community` |
| Intercept proxy | `127.0.0.1:6867` |
| Managed backend range | `6868-6890` |
| Codex provider / MCP server | `headroom_local_community` |
| Managed markers | `headroom-local-community:*` |

The application does not delete the paid app's bundle data, keychain entries,
`~/.headroom` workspace, managed markers, MCP entry, backups, or shared
HuggingFace cache. The sole read-only exception is
`~/.headroom/mcp_installs.json`, which may be read for the Serena ownership
fingerprint; the official ledger is never modified. Official routing is never
overwritten, and user-managed Serena entries are left untouched.

Codex, Claude Code, OpenCode, and other clients retain their own upstream API
key or OAuth authentication. The proxy and headroom-ai state stay local.

## How it works

```text
Claude Code / OpenCode
  | ANTHROPIC_BASE_URL=http://127.0.0.1:6867
  | provider base URL=http://127.0.0.1:6867/v1
  v
Rust intercept proxy :6867
  v
Managed headroom-ai backend :6868-6890
  v
The same upstream API selected by the coding client

Codex (OpenAI API key)
  | stable provider URL=http://127.0.0.1:6891/v1
Codex (ChatGPT OAuth)
  | stable provider URL=http://127.0.0.1:6891/backend-api/codex
  v
Detached Codex router :6891
  | Headroom running + heartbeat -> Rust intercept :6867
  | Headroom paused/closed/crashed -> native OpenAI or ChatGPT Codex API
```

Codex keeps the same loopback provider URL throughout this switch. The
detached router changes only its next hop: a healthy Headroom heartbeat enables
optimization through `:6867`; a paused, closed, or crashed Headroom process
leaves the router running in direct mode, so an already-running Codex session
does not need its configuration rewritten.

### Codex plugin boundary and controls

Headroom's Codex integration is a provider/router connector, not a standard
Codex plugin. Codex plugins are designed to bundle Skills, MCP servers, and
lifecycle hooks; they do not sit in front of Codex's model-provider request
transport. The routing switch therefore lives in Headroom's connector control
and the detached `:6891` router:

| State | Codex route | Headroom process |
|---|---|---|
| Connector enabled and heartbeat healthy | `:6891` → Headroom intercept `:6867` | running |
| Paused, disconnected, closed, or heartbeat expired | `:6891` → native OpenAI/ChatGPT endpoint | not required |
| Explicit uninstall | managed provider, shell export, guard hook, and router removed | removed |

Headroom's marketplace plugin cards use the same control model as Codex's
plugin browser. Enabling or disabling a Codex plugin edits its registered
`[plugins."plugin@marketplace"]` `enabled` value and keeps the installed cache;
uninstalling is the separate action that removes the bundle. Codex loads plugin
Skills and tools for a new chat or session, so an already-running conversation
keeps the capabilities it already resolved. See the [Codex plugin
documentation](https://learn.chatgpt.com/docs/plugins), [MCP configuration
documentation](https://learn.chatgpt.com/docs/extend/mcp), and [provider
configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

Anonymous headroom-ai telemetry, Sentry, Aptabase, account APIs, and the
official updater are disabled in this edition.

## Local components

| Component | License | Purpose |
|---|---|---|
| Headroom Desktop upstream | MIT | Tauri desktop shell and connectors |
| headroom-ai | Apache-2.0 | Local prompt optimization proxy |
| RTK | Apache-2.0 | Compact terminal command output |
| MarkItDown | MIT | Convert supported documents to Markdown |

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for attribution and
license details.

## Build on macOS

Requirements: macOS 14 or newer, Node.js/npm, Rust, and Tauri prerequisites.

```bash
npm ci
npm run build:mac:local
```

Artifacts are written under `src-tauri/target/release/bundle`. The local
script validates the frontend and Rust build, creates an ad-hoc signed `.app`
and DMG, and does not install, publish, or replace another application.

## Build on Windows

Requirements for the supported preview: Windows 11 x64, Node.js/npm, Rust,
Microsoft C++ Build Tools, WebView2, and the Tauri Windows prerequisites.
Windows 10 has not completed the required smoke test and is not currently a
supported compatibility promise.

```powershell
npm ci
npm run tauri build -- --bundles nsis --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

The release workflow builds a Windows x64 preview NSIS installer alongside the
stable macOS DMG and publishes both under one version and one `latest.json`
updater manifest. Promote Windows beyond preview only after a clean Win11 x64
install, first-run, update, and uninstall smoke test passes.

The preview installer is not Authenticode-signed, so Windows SmartScreen may
show an unknown-publisher warning. The Tauri updater signature verifies update
artifacts but does not replace Authenticode trust for the NSIS installer.

## Data written by Community

Depending on enabled features, Community may write its own app data, logs,
preferences, caches, `~/.headroom-local-community`, Community-managed fenced
blocks in client configuration, Serena usage hints in detected
`~/.codex/AGENTS.md` or `~/.claude/CLAUDE.md`, Community-named MCP entries and
guard hooks, and timestamped `.headroom-local-community-backup-*` files before
editing client configuration. Uninstall removes only Community-owned resources
and reverses only Community-managed client changes.

## License

This repository remains available under the upstream MIT license. Preserve the
upstream copyright notice, license, and applicable third-party notices in
redistributions. “Headroom Local Community” must be presented as an unofficial
community edition, not as the upstream paid product.

The project does not provide Homebrew installation. Use the stable macOS DMG or
the Windows 11 x64 preview NSIS installer from GitHub Releases.
