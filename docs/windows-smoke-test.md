# Windows smoke test

After installing a new Windows build (`-rc.N`), paste this file into Claude Code and ask it to run the checks. Each check has a single expected signal — if any fail, stop and investigate before promoting to stable.

Run the commands from Git Bash unless a section says PowerShell. Paths below use the Windows layout added by the Windows support work: the bundled runtime lives under `%APPDATA%\Headroom\headroom\runtime\venv\Scripts` (`python.exe` / `headroom.exe`), RTK at `%APPDATA%\Headroom\headroom\bin\rtk.exe`, and the markitdown shim at `%APPDATA%\Headroom\headroom\bin\markitdown.cmd`.

## Setup

1. Quit and relaunch Headroom.
2. Confirm the tray icon appears in the system tray.
3. Open the dashboard window once (so the proxy is fully booted).
4. Confirm the dashboard shows the Windows preview banner and reports `runtimeStatus.platform === "windows"` with `supportTier === "experimental"`.

## Checks (Claude Code pass)

Run these from a Claude Code session and report PASS / FAIL with the observed value. Checks 1, 5, 7, 8, 9, and 10 are client-agnostic — run them once in either client. Codex has very different wiring (no RTK, no `%USERPROFILE%\.claude\settings.json`, pay-per-token), so its equivalents live in the **Codex pass** below; run that whole section from a Codex session.

### 1. Version matches the new build
```bash
reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall" /s /f Headroom 2>/dev/null | grep -i version
```
Expect: the `-rc.N` version you just installed.

### 2. Proxy is intercepting this conversation
Send a trivial prompt ("say hi"), then:
```bash
stat -c '%y' "$APPDATA/Headroom/config/activity-facts.json"
```
Expect: mtime within the last minute.

### 3. RTK is on PATH and reports savings (Claude Code only — RTK does not rewrite Codex)
```bash
"$APPDATA/Headroom/headroom/bin/rtk.exe" --version && "$APPDATA/Headroom/headroom/bin/rtk.exe" gain | head -5
```
Expect: a version line and a gain summary, no "command not found". Claude Code's Bash tool spawns a non-login shell, so a bare `rtk` may report `command not found` even on a healthy install; call the managed `rtk.exe` by its absolute path instead.

### 4. MCP retrieve tool is available (Claude Code only; only if memory tools are enabled)
First check whether the proxy was started with memory tools:
```bash
ls "$APPDATA/Headroom/headroom/logs/" | grep -E 'no-memory-tools' >/dev/null && echo 'memory tools DISABLED — skip this check' || echo 'memory tools enabled — run check'
```
If enabled, have Claude call `mcp__headroom__headroom_retrieve` with any small query and expect a tool result (not "No such tool available").

### 5. Tray → Dashboard renders
Click the tray icon, open the dashboard. Expect savings chart and per-client stats render without a blank/error state.

### 6. Pause / resume cleanly strips and restores interception
In Settings, toggle Pause then Resume. After Pause, `grep -c headroom-rtk-rewrite "$USERPROFILE/.claude/settings.json"` should return `0`; after Resume it should return `1`. This verifies the Claude Code config only. Codex deliberately keeps its stable `6891` route during Pause; check C4 confirms that its detached router switches to direct mode and back instead.

### 7. Proxy is actively optimizing this conversation (not just a heartbeat)
The proxy always runs in `token` mode now (`HEADROOM_MODE=token`, hardcoded). The compression policy is chosen per request by the auth-mode classifier from the client `User-Agent`: Claude Code subscription/OAuth traffic (UA `claude-code/`) is classified `SUBSCRIPTION` → conservative policy; pay-per-token API-key / Codex traffic is classified `PAYG`/`OAUTH` → aggressive policy, so `requests_compressed` and `total_tokens_removed` move directly.

Timing matters either way: a `Read` result becomes part of Claude's *next* outgoing prompt, not the one currently being composed. The baseline capture, the large Read, and the re-check cannot all happen in one turn.

**Claude Code subscription/OAuth traffic** (classified `SUBSCRIPTION`):
1. Capture the baseline:
   ```bash
   "$APPDATA/Headroom/headroom/bin/rtk.exe" proxy curl -s http://127.0.0.1:6867/stats | jq '{primary_model: .summary.primary_model, prefix_frozen: .summary.uncompressed_requests.prefix_frozen, requests_compressed: .summary.compression.requests_compressed, cache_savings_usd: .summary.cost.breakdown.cache_savings_usd, total_tokens_before: .summary.compression.total_tokens_before_with_cli_filtering}'
   ```
2. End the turn with a large Read in flight — ask Claude to read a long file (~1300-1500 lines).
3. On the *next* turn, re-run the same `jq` command.

Expect: `primary_model` is a `claude-*` model, `cache_savings_usd` is strictly greater, `total_tokens_before` jumped by at least the size of the Read, and `prefix_frozen` + `requests_compressed` together increased by at least 1.

**Pay-per-token API-key traffic** (classified `PAYG`/`OAUTH` — the branch Codex hits):
1. Capture the baseline:
   ```bash
   "$APPDATA/Headroom/headroom/bin/rtk.exe" proxy curl -s http://127.0.0.1:6867/stats | jq '.summary.compression.requests_compressed, .summary.compression.total_tokens_removed'
   ```
2. End the turn with the same large Read in flight (~1300-1500 lines clears the compression threshold).
3. On the *next* turn, re-run the same `jq` command.

Expect: `requests_compressed` increased by at least 1, and `total_tokens_removed` is strictly greater.

### 8. Bundled runtime is healthy
```bash
"$APPDATA/Headroom/headroom/runtime/venv/Scripts/headroom.exe" --version && \
  "$APPDATA/Headroom/headroom/runtime/venv/Scripts/python.exe" -c "import headroom; print(headroom.__file__)"
```
Expect: a `headroom, version X.Y.Z` line and a path under `...\runtime\venv\Lib\site-packages\headroom\__init__.py`. No `ModuleNotFoundError`, no `pydantic-core` mismatch traceback.

### 9. Keychain round-trip via Windows Credential Manager
The session token lives in Windows Credential Manager (the `keyring` crate, `windows-native` feature), service `com.extraheadroom.headroom.account`, account `session-token`.
```powershell
cmdkey /list | findstr /i headroom
```
Expect: a stored credential entry for Headroom. Sign out and back in, then re-run the same command: the old entry is removed and a new one appears.

### 10. Autostart run key is managed
```powershell
reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v Headroom
```
Expect: the value points at the installed Headroom executable. Toggle autostart off in Settings and confirm the value disappears; toggle back on and confirm it returns.

### 11. Claude Code integration is intact
```bash
grep -c 'headroom-rtk-rewrite' "$USERPROFILE/.claude/settings.json"
grep -c 'markitdown' "$USERPROFILE/.claude/settings.json"
ls "$USERPROFILE/.claude/hooks/" 2>/dev/null
```
Expect: non-zero hook references for RTK rewrite and markitdown, and the hook scripts exist under `~/.claude/hooks/` (the guard hook and markitdown shim run through Git Bash's `bash` on Windows). Sending a prompt that triggers a `Read` should route through markitdown and report compressed input.

## Codex checks (Codex pass)

Run these from a Codex CLI session. Codex uses a stable local router at `127.0.0.1:6891`; while Headroom is healthy it relays to the Rust intercept on `6867`, and when Headroom is paused, closed, or crashed it forwards to the native provider. The router is configured via an `OPENAI_BASE_URL` shell export plus a managed provider block in `%USERPROFILE%\.codex\config.toml`, and its traffic is pay-per-token, so the proxy runs it in `token` mode.

### C1. Codex is configured to route through Headroom
```bash
grep -q 'model_provider = "headroom_local_community"' "$USERPROFILE/.codex/config.toml" && \
  grep -Eq 'openai_base_url = "http://127.0.0.1:6891/(v1|backend-api/codex)"' "$USERPROFILE/.codex/config.toml" && \
  grep -qF '[model_providers.headroom_local_community]' "$USERPROFILE/.codex/config.toml" && \
  grep -q 'supports_websockets = false' "$USERPROFILE/.codex/config.toml" && \
  echo PASS || echo FAIL
```
Expect: `PASS`. An OpenAI API-key profile uses `/v1`; a ChatGPT OAuth profile uses `/backend-api/codex`.

### C2. Codex traffic is actively optimized (token mode)
1. Capture the baseline:
   ```bash
   "$APPDATA/Headroom/headroom/bin/rtk.exe" proxy curl -s http://127.0.0.1:6867/stats | jq '{mode: .summary.mode, primary_model: .summary.primary_model, requests_compressed: .summary.compression.requests_compressed, total_tokens_removed: .summary.compression.total_tokens_removed}'
   ```
2. End the turn with a large file read in flight from Codex (~1300-1500 lines).
3. On the next turn, re-run the same command.

Expect: `mode` is `token`, `primary_model` is a `gpt-*` model, `requests_compressed` increased by at least 1, and `total_tokens_removed` is strictly greater.

### C3. Codex savings are attributed on the dashboard
Open the dashboard and confirm a **Codex** group appears in the per-provider savings with non-zero values. Provider `openai` maps to the Codex group.

### C4. Pause / resume keeps the stable Codex route and switches its mode
In Settings, toggle Pause then Resume, checking after each:
```bash
grep -c 'headroom-local-community:codex_cli' "$USERPROFILE/.codex/config.toml"
grep -nE 'openai_base_url = "http://127.0.0.1:6891/(v1|backend-api/codex)"' "$USERPROFILE/.codex/config.toml"
curl -s http://127.0.0.1:6891/__headroom_codex_router_health
```
Expect: the marker and `6891` URL remain present; after Pause the health endpoint prints `direct`, and after Resume it prints `proxy` once 6867/readyz is healthy. An explicit Disconnect Codex action removes the managed config.

## Inspecting the proxy directly

When inspecting the running proxy by hand (e.g. checking `/stats`), wrap `curl` with `rtk proxy` to bypass RTK's output filtering — otherwise large JSON responses get summarized into a type-shape view that looks like a broken endpoint:

```bash
"$APPDATA/Headroom/headroom/bin/rtk.exe" proxy curl -s http://127.0.0.1:6867/stats | jq .summary
```

## When something fails

- Proxy log silent → check `%APPDATA%\Headroom\headroom\logs\` for a newer log file or a crash file.
- RTK missing → check `%APPDATA%\Headroom\headroom\bin\rtk.exe` exists; the managed blocks in `%USERPROFILE%\.claude\settings.json` / `%USERPROFILE%\.codex\config.toml` are intact.
- MCP tool missing → restart Claude Code; the MCP server registration happens at session start.
- Credential Manager entries missing → re-run sign-in; verify the app is the release (non-debug) build, since the Windows keyring module is only compiled in release builds.
