# Support triage

A one-shot diagnostic to send a user who reports trouble. It is read-only (it
inspects config paths, proxy reachability, and recent log lines and changes
nothing) and tells you in one paste whether the problem is **routing**,
**detection**, or the **account gate**.

## Message to send

> Sorry you're hitting a snag, happy to get it sorted. Could you paste the
> output of this one command? It's read-only (it just checks your config paths,
> whether the proxy is reachable, and recent log lines, and changes nothing),
> and it tells me in one shot where the problem is.

## The command

Everything is inside `echo "..."` and one `{ ... } 2>&1` block, so zsh doesn't
choke on comments or `?`/`()` when the user pastes it.

```sh
{
  echo "=== Headroom version ==="
  defaults read /Applications/Headroom.app/Contents/Info CFBundleShortVersionString 2>/dev/null || echo "app not in /Applications"

  echo "=== Claude Code: config valid + routing ==="
  python3 -m json.tool < ~/.claude/settings.json >/dev/null 2>&1 && echo "settings.json OK" || echo "settings.json INVALID or missing"
  grep -i anthropic_base_url ~/.claude/settings.json ~/.zshrc ~/.zprofile ~/.bashrc 2>/dev/null
  echo "ANTHROPIC_BASE_URL env: ${ANTHROPIC_BASE_URL:-<unset>}"
  which claude || echo "claude NOT on PATH"
  claude --version 2>&1

  echo "=== Codex: config routing + guard ==="
  grep -iE "model_provider|base_url" ~/.codex/config.toml 2>/dev/null || echo "no ~/.codex/config.toml"
  echo "OPENAI_BASE_URL env: ${OPENAI_BASE_URL:-<unset>}"
  grep -o headroom-codex-guard ~/.codex/hooks.json 2>/dev/null && echo "codex guard registered" || echo "no codex guard"
  which codex || echo "codex NOT on PATH"
  codex --version 2>&1

  echo "=== Proxy reachable ==="
  curl -s -o /dev/null -w "6867 intercept: HTTP %{http_code}\n" http://127.0.0.1:6867/readyz || echo "6867 connection refused"
  curl -s http://127.0.0.1:6891/__headroom_codex_router_health 2>/dev/null \
    || echo "6891 Codex router unavailable"

  echo "=== Per-request proxy errors (~/.headroom/logs/proxy.log) ==="
  grep -iE "error|status=4|status=5| 401|refus|upstream" ~/.headroom/logs/proxy.log 2>/dev/null | tail -30

  echo "=== App + backend log errors ==="
  grep -iEr "error|fail|base_url|client setup|gate|401" ~/Library/Logs/Headroom/ "$HOME/Library/Application Support/Headroom/headroom/logs/" 2>/dev/null | tail -30
} 2>&1
```

## Reading the output

| Section | Healthy | Points to |
|---|---|---|
| version | matches current release | old build with a since-fixed bug |
| Claude config | `settings.json OK` + `ANTHROPIC_BASE_URL=http://127.0.0.1:6867` | `INVALID` = corrupt file blocks the write; wrong/absent URL = not routing, or a gateway (Bedrock/corp proxy) override |
| `which claude` | a real path | empty = installed via a PATH the GUI can't see, so the toggle is greyed out |
| Codex config | `model_provider = "headroom_local_community"` plus a `6891` router URL (`/v1` for an OpenAI API key or `/backend-api/codex` for ChatGPT OAuth) | wrong provider/base URL = Codex is not using the stable fail-open router |
| Proxy reachable | any HTTP code (200/503) | `connection refused` = app not running |
| logs | quiet | `client setup failed` = enable bug; `gate`/`401`/trial-ended = the account is gated, not broken |

The single most useful split: is it a **routing/detection** issue (top half) or an
**account gate** issue (log lines)? A user who is simply post-trial or over-cap
looks "broken" but every config check passes and the logs show the gate. Answer
that one with billing, not debugging.

## The three log locations

All three are current. Each has a distinct job.

| Path | Written by | Best for |
|---|---|---|
| `~/Library/Logs/Headroom/headroom-desktop.log` | Rust/Tauri app (`logging.rs`) | enable/`apply_client_setup` failures, pricing gate/bypass, runtime lifecycle |
| `~/Library/Application Support/Headroom/headroom/logs/headroom-proxy---port-6868---*.log` | desktop capturing the managed backend's stdout | startup banner, bootstrap |
| `~/.headroom/logs/proxy.log` | the backend's own always-on file logger (installed `headroom/proxy/server.py`) | **per-request failures: 401s, upstream errors, `status=`/`duration_ms=` for every request** |

For request-level problems (401s, upstream errors, "what did the proxy do with
my request"), `~/.headroom/logs/proxy.log` is the most useful of the three; it
rotates `proxy.log.1`..`.5` (~10 MB each) and is written for every user whenever
the backend runs. It is **not** legacy.
