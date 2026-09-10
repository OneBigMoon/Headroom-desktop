use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock,
};

use parking_lot::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, NaiveDate, Utc};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::backend_port::{self, AllForeign, SelectedFallback};
use crate::models::{ManagedTool, RtkTodayStats, ToolStatus};

/// Pinned headroom-ai version. Upgrade logic is disabled; this exact version
/// will be installed if the currently-installed version differs.
///
/// 0.20.x–0.24.x upstream shipped a maturin/Rust-native wheel that was both
/// per-Python-version and per-platform (e.g. `cp312-cp312-macosx_11_0_arm64`,
/// upstream #355). Starting with 0.25.0 the native module is built against the
/// CPython stable ABI (abi3, upstream #516), so a single `cp310-abi3` wheel per
/// platform now covers every CPython >= 3.10 — the pin below
/// (`cp310-abi3-macosx_11_0_arm64`) installs cleanly on our bundled cp312 and
/// stays valid if `PYTHON_STANDALONE_RELEASE` later moves to 3.13+. Only the
/// per-platform axis still matters, which `headroom_wheel_artifact` handles —
/// when bumping this pin, re-pick every platform's wheel URL/sha256 from
/// https://pypi.org/pypi/headroom-ai/<version>/json.
pub(crate) const HEADROOM_PINNED_VERSION: &str = "0.36.5";
const HEADROOM_SMOKE_TEST_TIMEOUT: Duration = Duration::from_secs(15);
/// markitdown's `--help` cold-imports a much heavier converter stack
/// (onnxruntime, magika, pdfminer, …) than the core `import headroom`. On
/// macOS 26 the first run of freshly-installed *unsigned* wheel binaries is
/// scanned by Gatekeeper/EDR, which routinely pushes that first import past
/// 15s and trips the smoke-test SIGKILL (RUST-22). It is warn-only, so a too
/// tight bound just produces Sentry noise and false "addon broken" signals.
const MARKITDOWN_SMOKE_TEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound on the one-time `learn --verbosity` baseline seed run before
/// proxy start. Typical runs are a few seconds (a ~100MB transcript project
/// seeds in ~3s); the cap only trips on pathological corpora, after which the
/// proxy starts anyway and seeding retries next launch.
const HEADROOM_BASELINE_SEED_TIMEOUT: Duration = Duration::from_secs(30);
/// Index of pre-built wheels for sdist-only PyPI packages (e.g. hnswlib).
/// GitHub's expanded_assets endpoint serves HTML anchors pip can consume via --find-links.
const VENDOR_WHEELS_INDEX_URL: &str =
    "https://github.com/gglucass/headroom-desktop/releases/expanded_assets/vendor-wheels-v1";
/// Never let pip build a dependency from source. A user's machine is not
/// guaranteed to have Xcode Command Line Tools (nor the Rust toolchain
/// pydantic-core's sdist needs), and requiring either to install a desktop app
/// is not a bargain we get to make. Every pin we install has a wheel for every
/// platform we ship -- the one sdist-only dependency, hnswlib, is served as a
/// prebuilt wheel from `VENDOR_WHEELS_INDEX_URL` -- so in the healthy case this
/// flag changes nothing. It only bites when a wheel is *missing*: without it
/// pip silently falls back to the sdist and spends minutes compiling before
/// dying on a clang/cargo error no user can act on; with it pip fails at once
/// with "no matching distribution found", which `classify_bootstrap_failure`
/// already recognises and explains. Deliberately NOT applied to the optional
/// markitdown/serena addons: their transitive sets are unpinned and unaudited,
/// and a failed addon leaves a working Headroom behind.
const PIP_ONLY_BINARY: &str = "--only-binary=:all:";
// headroom binds on the backend port chosen at spawn time (default 6868);
// the intercept layer on 6867 forwards to it. The backend port is dynamic
// because something else on the machine (e.g. rapportd) can claim 6868 at
// login — see `backend_port` for the selection logic.
fn headroom_proxy_port() -> String {
    backend_port::get().to_string()
}

/// HTTPX 0.28.1 turns IPv6 CIDR entries from NO_PROXY into invalid URL
/// patterns and aborts client construction (for example, `fd00::/8` becomes
/// an "Invalid port" error). HTTPX does not support CIDR matching here anyway,
/// so omit only those incompatible entries from the managed runtime child.
fn httpx_compatible_no_proxy(raw: &str) -> (String, usize) {
    let mut dropped = 0usize;
    let entries = raw.split(',').filter_map(|entry| {
        let entry = entry.trim();
        let is_ipv6_cidr = entry
            .rsplit_once('/')
            .map(|(address, _)| {
                address
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::Ipv6Addr>()
                    .is_ok()
            })
            .unwrap_or(false);
        if is_ipv6_cidr {
            dropped += 1;
            None
        } else {
            Some(entry)
        }
    });
    (entries.collect::<Vec<_>>().join(","), dropped)
}

fn apply_httpx_compatible_proxy_env(command: &mut Command) {
    for key in ["NO_PROXY", "no_proxy"] {
        let Some(raw) = std::env::var_os(key) else {
            continue;
        };
        let (value, dropped) = httpx_compatible_no_proxy(&raw.to_string_lossy());
        if dropped > 0 {
            command.env(key, value);
            log::info!(
                "[tool_manager] omitted {dropped} HTTPX-incompatible IPv6 CIDR entr{} from {key}",
                if dropped == 1 { "y" } else { "ies" }
            );
        }
    }
}

const HEADROOM_PROXY_URL: &str = "http://127.0.0.1:6867";
const MCP_METHOD_CLAUDE_CLI: &str = "claude_cli";
const MCP_METHOD_FALLBACK_JSON: &str = "fallback_json";
const MCP_METHOD_DIRECT_CLAUDE_JSON: &str = "direct_claude_json";
const MCP_METHOD_COMMUNITY_REGISTRY: &str = "community_registry";

const COMMUNITY_MCP_INSTALL_HELPER: &str = r#"
import sys
from headroom.mcp_registry.base import ServerSpec
from headroom.mcp_registry.install import get_all_registrars

entrypoint, proxy_url, workspace_dir, config_dir = sys.argv[1:5]
spec = ServerSpec(
    name="headroom_local_community",
    command=entrypoint,
    args=("mcp", "serve"),
    env={
        "HEADROOM_PROXY_URL": proxy_url,
        "HEADROOM_WORKSPACE_DIR": workspace_dir,
        "HEADROOM_CONFIG_DIR": config_dir,
        "HEADROOM_TELEMETRY": "off",
        "HEADROOM_BEACON": "0",
    },
)

attempted = 0
failures = []
for registrar in get_all_registrars():
    if not registrar.detect():
        continue
    attempted += 1
    result = registrar.register_server(spec, force=False)
    if not result.ok:
        failures.append(f"{registrar.name}: {result.status.value}: {result.detail or ''}")

if failures:
    raise SystemExit("; ".join(failures))
if attempted == 0:
    raise SystemExit("no supported MCP clients detected")
"#;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum McpInstallMethod {
    ClaudeCli,
    FallbackJson,
    DirectClaudeJson,
    CommunityRegistry,
}

impl McpInstallMethod {
    fn as_str(self) -> &'static str {
        match self {
            McpInstallMethod::ClaudeCli => MCP_METHOD_CLAUDE_CLI,
            McpInstallMethod::FallbackJson => MCP_METHOD_FALLBACK_JSON,
            McpInstallMethod::DirectClaudeJson => MCP_METHOD_DIRECT_CLAUDE_JSON,
            McpInstallMethod::CommunityRegistry => MCP_METHOD_COMMUNITY_REGISTRY,
        }
    }
}
const HEADROOM_STARTUP_POLL_MS: u64 = 250;
const HEADROOM_STARTUP_TIMEOUT_MS: u64 = 300_000;

const HEADROOM_REQUIREMENTS_LOCK: &str = include_str!("../python/headroom-requirements.lock");
const HEADROOM_LINUX_REQUIREMENTS_LOCK: &str =
    include_str!("../python/headroom-linux-requirements.lock");
const HEADROOM_WINDOWS_REQUIREMENTS_LOCK: &str =
    include_str!("../python/headroom-windows-requirements.lock");

/// Full-file SHA-256 values of historical headroom-requirements.lock shipments
/// whose pinned versions are byte-for-byte identical to the current lock.
/// Receipts holding one of these shas are treated as up-to-date; the receipt is
/// silently migrated to the comment-insensitive sha on next launch so the
/// entry can be dropped the next time the lock file actually changes.
///
/// When modifying the lock, re-evaluate: compare the stripped (comments and
/// blank lines removed) form of each legacy lock against the stripped current
/// lock. Drop any entry that no longer matches — those users need a real
/// reinstall.
const LEGACY_REQUIREMENTS_LOCK_SHAS: &[&str] = &[
    // 0.28.0 re-resolved the lock from scratch ([all,vector], torch 2.12.1,
    // benchmark tail dropped, broad point-version churn), so its stripped sha
    // differs from every prior shipment. The 0.27.0 cohort's lock receipt is now
    // genuinely stale and SHOULD trigger a dep reinstall — do not whitelist the
    // old sha here. The list stays empty until a future no-op cosmetic lock edit
    // (comments/blank lines only, no pin moves) needs to be treated as
    // up-to-date.
];

/// Receipts strictly below this version cannot be safely upgraded in place to
/// the currently bundled headroom-ai — pip's in-place upgrade leaves stale
/// `.so`/`.dylib` files from old native-extension pins (onnxruntime,
/// tokenizers, cryptography, mmh3, py_rust_stemmers, uvloop/httptools)
/// alongside the new ones, which surfaces as "smoke test passes, boot
/// validation fails with no log lines and no port bound" — the python
/// process segfaults on import before reaching logging setup.
///
/// Bumping this floor is a release-by-release decision: when a new lock
/// adds native deps or bumps native pins ABI-incompatibly, raise the floor
/// to the previous bundled version. When the new lock only churns pure-Python
/// pins, leave the floor where it is.
///
/// Floor history:
/// - 0.3.7: set to 0.10.0. 0.3.6's lock jump from 0.8.2 → 0.19.0 added
///   fastembed/mmh3/py_rust_stemmers and bumped tokenizers/cryptography/
///   uvicorn; the failing Sentry users all had `fallback: 0.8.2` (from
///   0.2.50-era desktop). 0.3.0-rc.26 onward shipped headroom-ai 0.10.x
///   against the same lock as 0.8.2 — these users have the same dep set
///   on disk and have not produced upgrade-failure events, so we let them
///   take the cheap in-place path. If 0.10.x fallbacks start appearing in
///   Sentry, raise the floor.
/// - 0.3.8: a single `fallback: 0.10.12` boot-validation stall appeared in
///   Sentry, but a clean-VM 0.3.5 → 0.3.7 upgrade reproduced the same
///   0.10.12 → 0.19.0 in-place delta and succeeded. The N=1 failure looks
///   environmental, not universal to the 0.10.x cohort. With the new
///   "Retry with full rebuild" button as a recovery path for the
///   environmental cases, we keep the floor at 0.10.0 rather than penalize
///   the (probably ~99%) of 0.10.x users who succeed in-place. Re-evaluate
///   if multi-machine 0.10.x failures show up in 0.3.8 telemetry.
/// - 0.4.0: raised to 0.20.0. Upstream 0.20.x switched headroom-ai to a
///   maturin/Rust-native single-wheel build (upstream #355) — wheels are now
///   per-Python-version and per-platform and ship a compiled `headroom_core`
///   `.so`. 0.19.0 venvs were built against a `py3-none-any` wheel with no
///   native extension; an in-place pip upgrade onto the new wheel would
///   layer the new `.so` on top of stale transitive native pins from the
///   old lock, which is the exact segfault-on-import pattern this floor
///   exists to prevent. Atomic rebuild is the only safe path for the
///   0.10.x–0.19.x cohort on this bump.
/// - 0.4.2 (0.24.0 → 0.25.0 bundle): floor stays at 0.20.0. The lock delta is
///   two pure-Python pins (litellm, importlib-metadata); no native pin moves
///   and no native dep is added or removed. The headroom-ai wheel itself goes
///   from a per-version `cp312-cp312` native wheel to a stable-ABI `cp310-abi3`
///   wheel (upstream #516), but pip uninstalls the old wheel (clearing its
///   `cpython-312` `.so` via RECORD) before unpacking the new `abi3` `.so`, so
///   no stale headroom_core extension is layered. The 0.20.x+ cohort — which
///   includes every 0.24.0-shipping (desktop 0.4.1) user — upgrades in place
///   with no big-wheel rebuilds. Raise the floor only if a future lock adds or
///   ABI-bumps a native transitive dep.
/// - 0.4.x (0.25.0 → 0.26.0 bundle): floor stays at 0.20.0. headroom-ai's
///   `requires_dist` is byte-identical between 0.25.0 and 0.26.0, so the lock
///   is reused unchanged — no pin moves at all. pip uninstalls the 0.25.0 abi3
///   wheel (clearing its `.so` via RECORD) and unpacks the 0.26.0 abi3 wheel;
///   no stale native extension is layered. The 0.20.x+ cohort upgrades in
///   place with no wheel rebuilds.
/// - 0.4.x (0.26.0 → 0.27.0 bundle): floor stays at 0.20.0. The lock moves three
///   pins: tree-sitter-language-pack 1.8.1 → 0.13.0 (native, ships compiled
///   grammar `.so`s) and the new spreadsheet extra (et-xmlfile/openpyxl/xlrd,
///   pure-Python). The language-pack move is a version *change*, so pip
///   uninstalls 1.8.1 via its RECORD (removing the old grammar `.so`s) before
///   unpacking 0.13.0 — no stale native extension is layered, unlike the
///   same-version in-place rebuilds this floor guards against. headroom-ai's own
///   abi3 wheel is likewise uninstalled-then-reinstalled. The 0.20.x+ cohort
///   upgrades in place. Raise the floor only if a future lock adds or
///   ABI-bumps a native transitive dep without a version bump.
/// - 0.5.x (0.27.0 → 0.28.0 bundle): floor stays at 0.20.0. Native pin moves are
///   all version *changes* (torch 2.11.0 → 2.12.1, onnxruntime 1.23.2 → 1.27.0),
///   so pip uninstalls the old wheel via RECORD (clearing its `.so`) before
///   unpacking the new one — no same-version relayering. The new tree-sitter
///   grammar packages (tree-sitter-c-sharp/embedded-template/yaml) are fresh
///   installs, not in-place rebuilds, so they carry no stale-`.so` risk either.
///   hnswlib stays pinned at 0.8.0 (unchanged). The benchmark-tail packages
///   dropped from the lock are pure-Python orphans left on disk by the in-place
///   `--upgrade` (pip does not prune removed requirements); harmless. The
///   0.20.x+ cohort upgrades in place.
/// - 0.7.x (0.32.1 → 0.33.0 bundle): floor stays at 0.20.0. The lock is
///   unchanged — 0.33.0's requires_dist only *tightens* three existing
///   constraints (ast-grep-cli !=0.44.1, mcp >=1.28.1,<2.0.0 on the mcp/proxy
///   extras, ruff on the dev extra), and the shipped lock already satisfies all
///   of them (ast-grep-cli==0.44.0, mcp==1.28.1; dev isn't installed). No
///   package is added or removed from the core/extra set we install, so the
///   in-place path is a single `pip install --no-deps --force-reinstall` of the
///   abi3 wheel with zero dep churn. The new Rust ports (CodeCompressor,
///   Kompress — upstream #1154/#1153) live inside headroom-ai's own
///   `headroom_core` abi3 extension, which pip uninstalls via RECORD before
///   unpacking the replacement, so no stale `.so` is layered.
/// - 0.7.x (0.33.0 → 0.34.0 bundle): floor stays at 0.20.0. requires_dist is
///   byte-identical to 0.33.0. The lock tops up two transitive pins to match
///   upstream's #2753 CVE clearance (aiohttp 3.14.1 → 3.14.3, cryptography
///   49.0.0 → 50.0.0). Both are version *changes* of native wheels, so pip
///   uninstalls the old wheel via RECORD before unpacking the new one — no
///   same-version relayering. The lock-sha change triggers a dep top-up for
///   existing users, which is intended (they need the CVE-fixed wheels).
/// - 0.8.x (0.34.0 → 0.35.0 bundle): floor stays at 0.20.0. requires_dist
///   only moves the ruff dev pin (dev extra isn't installed). The lock tops
///   up one transitive pin to match upstream's #2839 CVE clearance: h2
///   4.3.0 → 4.4.1 (CVE-2026-71554). h2 is pure-Python, and its hpack/
///   hyperframe requirements are already satisfied by the shipped pins, so
///   the top-up is a wheel swap with no native or resolution churn.
const ATOMIC_REBUILD_FLOOR_VERSION: (u32, u32, u32) = (0, 20, 0);

/// Parse the leading `major.minor.patch` from a version string, tolerating
/// pre-release/build suffixes (`-rc.1`, `+build`, `.dev0`, etc.). Returns
/// None when the prefix isn't a numeric `major.minor`. `patch` defaults to
/// 0 when missing or unparseable, so `"0.19"` and `"0.19.0"` compare equal.
/// Injected onto the backend's PYTHONPATH so the `site` module imports it at
/// interpreter startup. `faulthandler.register` needs Python-side code — the
/// PYTHONFAULTHANDLER env only covers fatal signals — and the upstream proxy
/// has no dump hook of its own.
const SITECUSTOMIZE_PY: &str = r#""""Headroom Desktop injection (managed -- do not edit).

Registers SIGUSR1 to dump all Python thread stacks to stderr (the proxy
log). The desktop watchdog sends SIGUSR1 before force-killing a wedged
backend so the log shows what the event loop was stuck on.

Also keeps user-turn text out of lossy compression: from headroom-ai
0.34.0 the "coding" persona sets compress_user_messages=True, and the
profile kwargs force it on per request (HEADROOM_COMPRESS_USER_MESSAGES
can only force-enable, never disable), so the persona field itself is
flipped back. Verbatim user turns are deliberate desktop posture, not a
bug workaround: user messages carry the coding working set (code, errors,
paths) and Claude Code's <system-reminder> blocks (CLAUDE.md), which the
model must see verbatim. The 0.34.0 tag-split bug that made this urgent
(open/close tags landing in different router sections, so the pair never
matched and CLAUDE.md arrived word-dropped) was fixed upstream in 0.35.0
(#2887), but that only protects TAGGED blocks -- plain user text would
still compress -- so the flip stays. tool_result blocks compress
regardless of this flag (the role gate only guards text blocks), so the
coding token mass is unaffected.

Also ports four fixes owed upstream (remove each once a wheel ships it),
gated on HEADROOM_SDK=headroom-local-community-proxy so only the backend process
pays the proxy import cost:
Context-limit guard (upstream PR #2942): compression under-reports
usage to the client, so Claude Code's proactive auto-compaction never
fires; once even the compressed request exceeds the model's real
window, every turn 400s with "prompt is too long" and the client
force-compacts on each error -- a compact-every-other-prompt loop
(churn report 2026-08-12). The guard nudges the reported message_start
input_tokens once the forwarded total nears the real window, learns
the real window from prompt-too-long 400 bodies, and makes
get_context_limit honor the context-1m beta so 1M sessions stop being
max-crushed by compression pressure. Kill switch:
HEADROOM_CONTEXT_GUARD=0.
Response-cache poisoning guard: SemanticCache.set stores any body gated
only on status_code == 200, so an empty/unparseable/error body is
replayed for the whole TTL (1h) and only a proxy restart clears it. The
guard refuses to store anything that is not a JSON object or that
carries an error payload. Kill switch: HEADROOM_RESPONSE_CACHE_GUARD=0.
Responses savings denominator guard (upstream PR #3106): /v1/responses
HTTP outcomes derive optimized = max(0, original - saved) from a
messages-only original while tokens_saved includes tool-schema
compaction measured against the tools array, so schema-heavy turns
clamp optimized to 0 and record savings rates above 100% ("4,436 -> 0,
208.4%" in the desktop feed, 2026-08-18). The guard counts the
request's tools schema once per compressed HTTP request and widens the
recorded pair at the outcome funnel so original - optimized == saved.
Kill switch: HEADROOM_RESPONSES_DENOMINATOR_GUARD=0.
Codex additional_tools guard (upstream issue #3185): Codex CLI 0.149.0
stopped sending a top-level tools array on /v1/responses for models its
capability cache flags (gpt-5.6-sol, its default) -- tool definitions
ride inside input as items of type "additional_tools". Every tools
consumer in the proxy (tool_schema_compaction, the output-shaper
stratum, the tools token counts, the denominator guard above) reads
only payload["tools"], so those requests classify "notools" and record
zero savings (2026-08-21: 0/13 afternoon signups and 42/54 active
codex users with frozen savings). The guard lifts the items' tools into
a top-level array before compression and puts the compacted definitions
back into the carrier before the payload is forwarded. The lift is
internal only: tools is a per-request parameter while additional_tools
is an input item, so forwarding the lifted shape costs a stateful
session (Codex TUI/app-server over WS) its whole tool surface after
turn one -- upstream #3194, the 0.36.3 regression from the same lift.
No version gate: it self-neutralizes when payload["tools"] is already
present, and 0.36.2 has no additional_tools support either. Kill
switch: HEADROOM_ADDITIONAL_TOOLS_GUARD=0.
"""
import faulthandler
import signal

# No chain=True: SIGUSR1's default disposition is terminate, and chaining
# falls through to it after the dump -- the process must survive the dump so
# the watchdog controls the actual kill.
try:
    faulthandler.register(signal.SIGUSR1, all_threads=True)
except Exception:
    pass

# Protect user-turn text (CLAUDE.md system-reminders) from lossy Kompress:
# flip the coding persona back to compress_user_messages=False. Guarded so
# fallback runtimes without the persona (< 0.30.0) and half-installed venvs
# no-op cleanly.
try:
    from dataclasses import replace as _hd_replace

    import headroom.agent_savings as _hd_savings

    _hd_coding = _hd_savings._PROFILES.get("coding")
    if _hd_coding is not None and _hd_coding.compress_user_messages:
        _hd_savings._PROFILES["coding"] = _hd_replace(
            _hd_coding, compress_user_messages=False
        )
except Exception:
    pass

import os as _hd_os

if _hd_os.environ.get("HEADROOM_SDK") == "headroom-local-community-proxy":
    # Context-limit guard (upstream PR #2942; remove once a wheel ships it).
    # See the module docstring for the failure mode this breaks.
    try:
        if _hd_os.environ.get("HEADROOM_CONTEXT_GUARD", "1").strip().lower() not in (
            "0",
            "false",
            "no",
            "off",
        ):
            import contextvars as _hd_cg_cvars
            import json as _hd_cg_json
            import logging as _hd_cg_logging
            import re as _hd_cg_re

            import headroom.providers.anthropic as _hd_cg_prov
            import headroom.proxy.handlers.anthropic as _hd_cg_anth
            import headroom.proxy.handlers.streaming as _hd_cg_stream

            _hd_cg_log = _hd_cg_logging.getLogger("headroom.proxy")
            # Beta header of the in-flight request; set by the entry wrapper
            # so the get_context_limit patch can see it.
            _hd_cg_beta = _hd_cg_cvars.ContextVar("headroom_cg_beta", default=None)
            # (model, has_1m_beta) -> real window learned from a 400 body.
            _hd_cg_learned = {}
            _HD_CG_RE = _hd_cg_re.compile(
                r"prompt is too long:\s*(\d+)\s*tokens?\s*>\s*(\d+)\s*maximum",
                _hd_cg_re.IGNORECASE,
            )
            # Nudge arms at 90% of the real window; reports 95% of the window
            # the client believes in (Claude Code compacts around 92%).
            _HD_CG_TRIGGER = 0.90
            _HD_CG_REPORT = 0.95

            def _hd_cg_has_1m(beta):
                if not beta:
                    return False
                return any(
                    t.strip().lower().startswith("context-1m") for t in beta.split(",")
                )

            def _hd_cg_believed(base, beta):
                return max(base, 1_000_000) if _hd_cg_has_1m(beta) else base

            def _hd_cg_effective(model, base, beta):
                believed = _hd_cg_believed(base, beta)
                learned = _hd_cg_learned.get((model, _hd_cg_has_1m(beta)))
                return min(believed, learned) if learned is not None else believed

            def _hd_cg_learn(model, beta, text):
                if isinstance(text, (bytes, bytearray)):
                    text = bytes(text).decode("utf-8", errors="replace")
                m = _HD_CG_RE.search(text or "")
                if m:
                    _hd_cg_learned[(model, _hd_cg_has_1m(beta))] = int(m.group(2))
                    _hd_cg_log.warning(
                        "event=context_guard_learned_limit model=%s context_1m=%s limit=%s",
                        model,
                        _hd_cg_has_1m(beta),
                        m.group(2),
                    )

            _hd_cg_orig_handle = _hd_cg_anth.AnthropicHandlerMixin.handle_anthropic_messages

            async def _hd_cg_handle(self, request, *args, **kwargs):
                token = None
                try:
                    token = _hd_cg_beta.set(request.headers.get("anthropic-beta"))
                except Exception:
                    token = None
                try:
                    return await _hd_cg_orig_handle(self, request, *args, **kwargs)
                finally:
                    if token is not None:
                        _hd_cg_beta.reset(token)

            _hd_cg_anth.AnthropicHandlerMixin.handle_anthropic_messages = _hd_cg_handle

            # Compression pressure sees the window the request is really
            # subject to (context-1m raised, learned-capped) instead of a
            # flat 200k that pins context_pressure at 1.0 for 1M sessions.
            _hd_cg_orig_limit = _hd_cg_prov.AnthropicProvider.get_context_limit

            def _hd_cg_limit(self, model):
                base = _hd_cg_orig_limit(self, model)
                try:
                    return _hd_cg_effective(model, base, _hd_cg_beta.get())
                except Exception:
                    return base

            _hd_cg_prov.AnthropicProvider.get_context_limit = _hd_cg_limit

            class _HdCgGuard:
                # Mirrors upstream StreamUsageGuard (PR #2942): rewrite the
                # first message_start's input_tokens when the forwarded total
                # nears the real window, AND the final cumulative-usage
                # message_delta -- verified live 2026-08-12 that Claude Code
                # merges the latter over the former, so nudging only the
                # first event loses the merge. Below the trigger the guard
                # goes inert on the first event (steady-state untouched).
                def __init__(self, believed, effective):
                    self.believed = believed
                    self.effective = effective
                    self.buf = bytearray()
                    self.seen_start = False
                    self.target = None
                    self.start_cr = 0
                    self.start_cw = 0
                    self.done = believed <= 0 or effective <= 0

                def flush(self):
                    self.done = True
                    out = bytes(self.buf)
                    self.buf = bytearray()
                    return out

                def feed(self, chunk):
                    if self.done:
                        return chunk
                    self.buf.extend(chunk)
                    if len(self.buf) > 262144:
                        return self.flush()
                    out = bytearray()
                    while not self.done:
                        cut = self.buf.find(b"\n\n")
                        if cut == -1:
                            break
                        event = bytes(self.buf[: cut + 2])
                        del self.buf[: cut + 2]
                        out += self._event(event)
                    if self.done:
                        out += self.flush()
                    return bytes(out)

                def _event(self, event):
                    if b"event: ping" in event or event.strip() == b"":
                        return event
                    if not self.seen_start:
                        self.seen_start = True
                        if b"message_start" not in event:
                            self.done = True
                            return event
                        try:
                            rewritten = self._rewrite_start(event)
                        except Exception:
                            self.done = True
                            return event
                        if self.target is None:
                            self.done = True
                        return rewritten
                    if b"message_delta" not in event:
                        return event
                    self.done = True
                    try:
                        return self._rewrite_delta(event)
                    except Exception:
                        return event

                def _rewrite_start(self, event):
                    lines = event.split(b"\n")
                    for i, line in enumerate(lines):
                        if not line.startswith(b"data:"):
                            continue
                        payload = _hd_cg_json.loads(line[5:].strip())
                        if payload.get("type") != "message_start":
                            return event
                        usage = payload.get("message", {}).get("usage")
                        if not isinstance(usage, dict):
                            return event
                        cr = int(usage.get("cache_read_input_tokens") or 0)
                        cw = int(usage.get("cache_creation_input_tokens") or 0)
                        total = int(usage.get("input_tokens") or 0) + cr + cw
                        if total < _HD_CG_TRIGGER * self.effective:
                            return event
                        target = int(self.believed * _HD_CG_REPORT)
                        if total >= target:
                            return event
                        self.target = target
                        self.start_cr = cr
                        self.start_cw = cw
                        usage["input_tokens"] = target - cr - cw
                        _hd_cg_log.warning(
                            "event=context_guard_nudge forwarded_total=%s "
                            "effective_limit=%s reported_total=%s believed_limit=%s",
                            total,
                            self.effective,
                            target,
                            self.believed,
                        )
                        lines[i] = b"data: " + _hd_cg_json.dumps(
                            payload, separators=(",", ":")
                        ).encode()
                        return b"\n".join(lines)
                    return event

                def _rewrite_delta(self, event):
                    lines = event.split(b"\n")
                    for i, line in enumerate(lines):
                        if not line.startswith(b"data:"):
                            continue
                        payload = _hd_cg_json.loads(line[5:].strip())
                        if payload.get("type") != "message_delta":
                            return event
                        usage = payload.get("usage")
                        # A delta without cumulative input usage cannot
                        # override the already-nudged message_start.
                        if not isinstance(usage, dict) or "input_tokens" not in usage:
                            return event
                        cr = int(usage.get("cache_read_input_tokens", self.start_cr) or 0)
                        cw = int(
                            usage.get("cache_creation_input_tokens", self.start_cw) or 0
                        )
                        new_input = self.target - cr - cw
                        if int(usage.get("input_tokens") or 0) >= new_input:
                            return event
                        usage["input_tokens"] = new_input
                        lines[i] = b"data: " + _hd_cg_json.dumps(
                            payload, separators=(",", ":")
                        ).encode()
                        return b"\n".join(lines)
                    return event

            _hd_cg_orig_stream = _hd_cg_stream.StreamingMixin._stream_response

            async def _hd_cg_stream_response(
                self, url, headers, body, provider, model, request_id, *args, **kwargs
            ):
                resp = await _hd_cg_orig_stream(
                    self, url, headers, body, provider, model, request_id, *args, **kwargs
                )
                if provider != "anthropic":
                    return resp
                try:
                    beta = (
                        headers.get("anthropic-beta") if isinstance(headers, dict) else None
                    )
                    if getattr(resp, "status_code", 200) == 400:
                        _hd_cg_learn(model, beta, getattr(resp, "body", b"") or b"")
                        return resp
                    inner = getattr(resp, "body_iterator", None)
                    if inner is None:
                        return resp
                    base = _hd_cg_orig_limit(self.anthropic_provider, model)
                    guard = _HdCgGuard(
                        _hd_cg_believed(base, beta),
                        _hd_cg_effective(model, base, beta),
                    )

                    async def _hd_cg_guarded():
                        async for chunk in inner:
                            if guard.done:
                                yield chunk
                                continue
                            if not isinstance(chunk, (bytes, bytearray)):
                                tail = guard.flush()
                                if tail:
                                    yield tail
                                yield chunk
                                continue
                            out = guard.feed(bytes(chunk))
                            if out:
                                yield out
                        tail = guard.flush()
                        if tail:
                            yield tail

                    resp.body_iterator = _hd_cg_guarded()
                except Exception:
                    pass
                return resp

            _hd_cg_stream.StreamingMixin._stream_response = _hd_cg_stream_response
    except Exception:
        pass

    # Response-cache poisoning guard (owed upstream). SemanticCache.set is
    # gated only on `status_code == 200`, so an empty body, an unparseable
    # one, or an error payload that some path returned as 200 is stored and
    # replayed verbatim for the full TTL (default 1h) to every matching
    # non-streaming request. Observed 2026-08-13: three /v1/messages replays
    # served in 6-10ms from one poisoned entry, and the only recovery was
    # restarting the proxy. Provider-generic (Anthropic and OpenAI both use a
    # top-level "error"), so it protects every handler, not just the CCR path
    # that surfaced it. Kill switch: HEADROOM_RESPONSE_CACHE_GUARD=0.
    try:
        if _hd_os.environ.get("HEADROOM_RESPONSE_CACHE_GUARD", "1").strip().lower() not in (
            "0",
            "false",
            "no",
            "off",
        ):
            import json as _hd_sc_json
            import logging as _hd_sc_logging

            import headroom.proxy.semantic_cache as _hd_sc

            _hd_sc_log = _hd_sc_logging.getLogger("headroom.proxy")
            _hd_sc_orig_set = _hd_sc.SemanticCache.set

            def _hd_sc_cacheable(body):
                if not body:
                    return False
                try:
                    parsed = _hd_sc_json.loads(body)
                except Exception:
                    return False
                if not isinstance(parsed, dict):
                    return False
                # An error body stored under a 200 is the worst case: every
                # replay re-serves it and the client never retries.
                return not (parsed.get("type") == "error" or "error" in parsed)

            async def _hd_sc_set(self, messages, model, response_body, *args, **kwargs):
                if not _hd_sc_cacheable(response_body):
                    _hd_sc_log.warning(
                        "event=response_cache_store_refused model=%s bytes=%d",
                        model,
                        len(response_body or b""),
                    )
                    return None
                return await _hd_sc_orig_set(
                    self, messages, model, response_body, *args, **kwargs
                )

            _hd_sc.SemanticCache.set = _hd_sc_set
    except Exception:
        pass

    # Responses savings denominator guard (upstream PR #3106; remove once a
    # wheel ships it -- see the module docstring for the failure mode). WS
    # outcomes already build original as optimized + saved and are excluded
    # twice over: the stash skips executor calls carrying the WS-only timeout
    # kwarg (both WS call sites pass it on 0.35.0, the HTTP site does not),
    # and the repair refuses endpoint=responses_ws. Version-gated below 0.36
    # so a wheel that ships #3106 natively cannot be widened twice. Kill
    # switch: HEADROOM_RESPONSES_DENOMINATOR_GUARD=0.
    try:
        if _hd_os.environ.get(
            "HEADROOM_RESPONSES_DENOMINATOR_GUARD", "1"
        ).strip().lower() not in ("0", "false", "no", "off"):
            import asyncio as _hd_rd_asyncio
            import collections as _hd_rd_collections
            import dataclasses as _hd_rd_dc
            import importlib.metadata as _hd_rd_meta

            import headroom.proxy.handlers.openai as _hd_rd_openai
            import headroom.proxy.server as _hd_rd_server

            _hd_rd_ver = tuple(
                int(p) for p in _hd_rd_meta.version("headroom-ai").split(".")[:2]
            )
            if _hd_rd_ver < (0, 36):
                # request_id -> pre-compression tools-schema token count.
                # Entries pop at the outcome funnel; stragglers (upstream
                # errors, killed requests) age out FIFO.
                _hd_rd_tools = _hd_rd_collections.OrderedDict()
                _HD_RD_MAX = 512

                _hd_rd_orig_compress = (
                    _hd_rd_openai.OpenAIHandlerMixin._compress_openai_responses_payload_in_executor
                )

                async def _hd_rd_compress(self, payload, **kwargs):
                    result = await _hd_rd_orig_compress(self, payload, **kwargs)
                    try:
                        # The pass is copy-on-write, so `payload` still holds
                        # the pre-compression tools. result[1] = modified.
                        tools = (
                            payload.get("tools") if isinstance(payload, dict) else None
                        )
                        if tools and "timeout" not in kwargs and result[1]:
                            counter = self.openai_provider.get_token_counter(
                                kwargs.get("model")
                            )
                            count = await _hd_rd_asyncio.to_thread(
                                counter.count_text,
                                _hd_rd_openai._json_debug_dumps(tools),
                            )
                            rid = kwargs.get("request_id")
                            if rid and count > 0:
                                _hd_rd_tools[rid] = int(count)
                                while len(_hd_rd_tools) > _HD_RD_MAX:
                                    _hd_rd_tools.popitem(last=False)
                    except Exception:
                        pass
                    return result

                _hd_rd_openai.OpenAIHandlerMixin._compress_openai_responses_payload_in_executor = (
                    _hd_rd_compress
                )

                _hd_rd_orig_record = (
                    _hd_rd_server.HeadroomProxy._record_request_outcome
                )

                async def _hd_rd_record(self, outcome):
                    try:
                        tools_tokens = _hd_rd_tools.pop(outcome.request_id, None)
                        # Repair only the old HTTP derivation shape; anything
                        # else (WS deltas, future wheels) passes untouched.
                        if (
                            tools_tokens
                            and outcome.tokens_saved > 0
                            and (outcome.tags or {}).get("endpoint") != "responses_ws"
                            and outcome.optimized_tokens
                            == max(0, outcome.original_tokens - outcome.tokens_saved)
                        ):
                            widened = outcome.original_tokens + tools_tokens
                            outcome = _hd_rd_dc.replace(
                                outcome,
                                original_tokens=widened,
                                optimized_tokens=max(0, widened - outcome.tokens_saved),
                            )
                    except Exception:
                        pass
                    return await _hd_rd_orig_record(self, outcome)

                _hd_rd_server.HeadroomProxy._record_request_outcome = _hd_rd_record
    except Exception:
        pass

    # Codex additional_tools guard (upstream issue #3185; remove once a wheel
    # ships support -- see the module docstring for the failure mode). Applied
    # AFTER the denominator guard so this wrapper is outermost: the lift runs
    # first at runtime and every inner layer (denominator widening included)
    # sees the classic top-level tools shape. Symmetric: the lift is in place
    # for the compression pass only, and the carrier goes back on the wire
    # before the payload is forwarded (see _hd_at_restore -- forwarding the
    # lifted shape is upstream's 0.36.3 regression, #3194). Kill switch:
    # HEADROOM_ADDITIONAL_TOOLS_GUARD=0.
    try:
        if _hd_os.environ.get(
            "HEADROOM_ADDITIONAL_TOOLS_GUARD", "1"
        ).strip().lower() not in ("0", "false", "no", "off"):
            import logging as _hd_at_logging

            import headroom.proxy.handlers.openai as _hd_at_openai

            _hd_at_log = _hd_at_logging.getLogger("headroom.proxy")
            _hd_at_announced = False

            _hd_at_warned = False

            def _hd_at_lift(payload, plan):
                # Lift additional_tools input items into a top-level tools
                # array, in place, recording the carrier in `plan` so the
                # forwarded payload can be put back the way Codex sent it.
                # No-op (0) unless the request uses the Codex >= 0.149.0
                # encoding and has no top-level tools.
                global _hd_at_announced
                if not isinstance(payload, dict) or payload.get("tools"):
                    return 0
                items = payload.get("input")
                if not isinstance(items, list):
                    return 0
                lifted = []
                kept = []
                carrier = None
                for item in items:
                    if (
                        isinstance(item, dict)
                        and item.get("type") == "additional_tools"
                        and isinstance(item.get("tools"), list)
                        and item["tools"]
                    ):
                        if carrier is None:
                            # Restore template: the carrier's own fields
                            # minus the definitions, plus its position among
                            # the items that survive the lift. Codex sends a
                            # single carrier; extra carriers merge into this
                            # one rather than being split on a boundary that
                            # compaction may have invalidated.
                            carrier = (
                                {k: v for k, v in item.items() if k != "tools"},
                                len(kept),
                            )
                        lifted.extend(item["tools"])
                    else:
                        kept.append(item)
                if not lifted:
                    return 0
                payload["tools"] = lifted
                payload["input"] = kept
                plan.append((carrier[0], carrier[1], list(lifted)))
                if not _hd_at_announced:
                    _hd_at_announced = True
                    _hd_at_log.info(
                        "event=codex_additional_tools_lifted tools=%d "
                        "(codex >= 0.149.0 encoding; desktop guard active)",
                        len(lifted),
                    )
                return len(lifted)

            def _hd_at_restore(payload, plan):
                # Put the definitions back into the carrier Codex sent them
                # in, post-compaction, and drop the top-level array. The lift
                # is an internal normalization for the tools consumers; the
                # forwarded shape must match what the client sent. `tools` is
                # a per-request parameter while additional_tools is an input
                # item and therefore part of the transcript, so a stateful
                # session (Codex TUI/app-server over WS) declares its tools
                # once and relies on the transcript for every later turn --
                # forwarding the lifted shape leaves that transcript
                # tool-less: turn one works, then all shell/filesystem access
                # vanishes for the rest of the session (upstream #3194).
                if not isinstance(payload, dict) or not plan:
                    return 0
                items = payload.get("input")
                if not isinstance(items, list):
                    return 0
                if any(
                    isinstance(item, dict)
                    and item.get("type") == "additional_tools"
                    and item.get("tools")
                    for item in items
                ):
                    # Already in carrier form; restoring again would
                    # duplicate the definitions. Keeps the restore idempotent.
                    return 0
                template, position, original = plan[0]
                tools = payload.get("tools")
                if not isinstance(tools, list) or not tools:
                    # A consumer emptied the array. Restoring what Codex sent
                    # is strictly safer than forwarding a tool-less payload.
                    tools = original
                if not tools:
                    return 0
                carrier = dict(template)
                carrier["type"] = "additional_tools"
                carrier["tools"] = list(tools)
                restored = list(items)
                restored.insert(min(max(position, 0), len(restored)), carrier)
                payload["input"] = restored
                payload.pop("tools", None)
                return len(carrier["tools"])

            _hd_at_orig_compress = (
                _hd_at_openai.OpenAIHandlerMixin._compress_openai_responses_payload_in_executor
            )

            async def _hd_at_compress(self, payload, **kwargs):
                global _hd_at_warned
                plan = []
                result = None
                try:
                    _hd_at_lift(payload, plan)
                except Exception:
                    # The plan is deliberately kept: a lift that raised after
                    # mutating the payload is undone by the restore below.
                    pass
                try:
                    result = await _hd_at_orig_compress(self, payload, **kwargs)
                finally:
                    if plan:
                        targets = [payload]
                        try:
                            # result[0] is what the call sites forward; the caller
                            # payload is restored too, for the paths that forward
                            # the object they passed in when nothing was modified.
                            out = result[0] if isinstance(result, tuple) and result else None
                            if isinstance(out, dict) and out is not payload:
                                targets.append(out)
                            failed = [t for t in targets if not _hd_at_restore(t, plan)]
                        except Exception:
                            failed = targets
                        if failed and not _hd_at_warned:
                            # Logged once: the lifted shape is about to go out and
                            # a stateful client will lose its tools after this
                            # turn. Never silent.
                            _hd_at_warned = True
                            _hd_at_log.warning(
                                "event=codex_additional_tools_restore_failed "
                                "(forwarding lifted shape; set "
                                "HEADROOM_ADDITIONAL_TOOLS_GUARD=0 to opt out)"
                            )
                return result

            _hd_at_openai.OpenAIHandlerMixin._compress_openai_responses_payload_in_executor = (
                _hd_at_compress
            )
    except Exception:
        pass
"#;

/// Pre-upstream concurrency passed to the backend: 2x logical cores,
/// clamped to [8, 64]. See the spawn-site comment for why the proxy's own
/// 8-cap is safe to exceed under the desktop's env. An explicit
/// `HEADROOM_ANTHROPIC_PRE_UPSTREAM_CONCURRENCY` in the environment wins, so a
/// power user with 30+ concurrent sessions can raise it without a rebuild.
fn pre_upstream_concurrency() -> usize {
    if let Some(n) = std::env::var("HEADROOM_ANTHROPIC_PRE_UPSTREAM_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
    {
        return n;
    }
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    (cores * 2).clamp(8, 64)
}

/// Interval estimate of OpenAI's prompt-cache TTL from the backend's
/// `cache_ttl_observations.jsonl` (written under HEADROOM_CACHE_TTL_LEARN):
/// hit idles bound the TTL from below (cache proven alive), `ttl_expiry` miss
/// idles bound it from above (cache proven dead). Returns the smallest
/// observed death-idle beyond the largest observed life-idle, so an estimation
/// error can only skip a recompaction, never bust a warm cache; None when the
/// bounds overlap or samples are thin (< 3 hits or < 3 expiry misses).
/// Mirrors the upstream `headroom-cache-ttl` estimator (headroom PR #2670);
/// delete once a wheel ships it and a scheduled run replaces this.
fn learned_openai_ttl_seconds(obs_path: &Path) -> Option<u64> {
    let data = std::fs::read_to_string(obs_path).ok()?;
    let mut hit_idles: Vec<f64> = Vec::new();
    let mut expiry_idles: Vec<f64> = Vec::new();
    for line in data.lines() {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if row.get("provider").and_then(|v| v.as_str()) != Some("openai") {
            continue;
        }
        let idle = row
            .get("idle_seconds")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if idle <= 0.0 {
            continue;
        }
        if row.get("is_miss").and_then(|v| v.as_bool()) == Some(true) {
            if row.get("reason").and_then(|v| v.as_str()) == Some("ttl_expiry") {
                expiry_idles.push(idle);
            }
        } else {
            hit_idles.push(idle);
        }
    }
    if hit_idles.len() < 3 || expiry_idles.len() < 3 {
        return None;
    }
    let max_hit_idle = hit_idles.into_iter().fold(0.0f64, f64::max);
    let learned = expiry_idles
        .into_iter()
        .filter(|&idle| idle > max_hit_idle)
        .fold(f64::INFINITY, f64::min);
    if !learned.is_finite() {
        return None;
    }
    Some((learned as u64).clamp(300, 3600))
}

fn parse_major_minor_patch(s: &str) -> Option<(u32, u32, u32)> {
    let head = s.split(|c: char| c == '-' || c == '+').next()?;
    let mut parts = head.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// True when the previously-installed receipt is too old to safely apply an
/// in-place pip upgrade against — caller should fall through to the atomic
/// venv rebuild path. Unparseable versions are treated as too old (be
/// conservative: a rebuild is always safe, an unsafe in-place is not).
fn receipt_requires_atomic_rebuild(previous_version: &str) -> bool {
    match parse_major_minor_patch(previous_version) {
        Some(v) => v < ATOMIC_REBUILD_FLOOR_VERSION,
        None => true,
    }
}
const RTK_VERSION: &str = "0.47.0";
static RTK_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static RTK_INSTALL_NONCE: AtomicU64 = AtomicU64::new(0);
static PACKAGE_UPDATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const MARKITDOWN_PINNED_VERSION: &str = "0.1.7";
const SERENA_PINNED_VERSION: &str = "1.7.0";
const CONTEXT7_PINNED_VERSION: &str = "4.0.4";

pub(crate) fn stable_package_version(version: &str) -> Result<&str> {
    if version.len() >= 5
        && version.split('.').count() == 3
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        Ok(version)
    } else {
        bail!("invalid package version {version:?}; expected stable x.y.z")
    }
}
/// First run downloads the package into the npx cache; slow networks need
/// headroom over the usual smoke-test budget.
const CONTEXT7_INSTALL_TIMEOUT: Duration = Duration::from_secs(180);
const CODEBASE_MEMORY_VERSION: &str = "0.10.8";
static CODEBASE_MEMORY_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Durable phase metadata for a codebase-memory replacement. The marker is a
/// rollback boundary, so a valid JSON object with missing flags must not be
/// interpreted as `false`: doing so could delete a live binary or receipt that
/// the interrupted transaction never backed up.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CodebaseMemoryPending {
    had_destination: Option<bool>,
    had_receipt: Option<bool>,
}

const CODEBASE_MEMORY_SHA256_MACOS_AARCH64: &str =
    "9bd840dfb3ec7eaef4f310382057adaa5b0e904df883104d03ffcf39836afd07";
const CODEBASE_MEMORY_SHA256_MACOS_X86_64: &str =
    "2b193085410af3801634a522f4b17dcd6699695e015a068393c87817c1d260d4";
const CODEBASE_MEMORY_SHA256_LINUX_AARCH64: &str =
    "e2804a20f5a6fc392af361525a232703e351b7d1aacb81b88eef806eec5959fa";
const CODEBASE_MEMORY_SHA256_LINUX_X86_64: &str =
    "e5cba4cad6ca8254a85f45041fc8a831908d7d5cb64f98fc3f8eb70a58671793";
/// Serena's CLI cold-imports its full LSP stack; first run on a slow disk can
/// take tens of seconds.
const SERENA_SMOKE_TEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Registers/unregisters the managed Serena MCP entry with every detected
/// agent, reusing the bundled headroom package's registrars and install
/// ledger instead of reimplementing Claude Code JSON / Codex TOML handling in
/// Rust. Mirrors upstream `_setup_serena_mcp` / `_remove_headroom_installed_serena_mcp`
/// (headroom.cli.wrap): only entries the ledger proves Headroom installed are
/// ever overwritten or removed — a user-managed serena entry is left alone.
/// argv: `register <serena-bin> [legacy-ledger]` | `unregister`.
const SERENA_MCP_HELPER: &str = r#"
import sys
from pathlib import Path

from headroom.mcp_registry import (
    ClaudeRegistrar,
    CodexRegistrar,
    GrokRegistrar,
    OpencodeRegistrar,
    ServerSpec,
)
from headroom.mcp_registry.base import RegisterStatus
from headroom.mcp_registry.ledger import (
    clear_install,
    headroom_installed_matching,
    record_install,
)

action = sys.argv[1]
legacy_ledger = Path(sys.argv[3]) if len(sys.argv) > 3 else None
failures = []

def registrar_call(registrar, label, call, *args, **kwargs):
    """Run one registrar call, downgrading an unusable agent CLI to a skip.

    A detected CLI that exec() refuses -- Homebrew leaves a `claude` shim
    behind when its native binary never downloaded, and a wrong-architecture
    build or a missing interpreter fails the same way -- raises OSError out of
    subprocess. Letting it escape aborted the whole addon operation, and the
    user saw only a cause-free "installation failed". One unusable agent must
    not block the others.
    """
    try:
        return call(*args, **kwargs)
    except OSError as exc:
        print(f"{registrar.name}: {label} skipped, CLI is not runnable ({exc})")
        return None

def headroom_owns_serena(registrar, current):
    if headroom_installed_matching(registrar.name, current):
        return True
    return legacy_ledger is not None and headroom_installed_matching(
        registrar.name, current, path=legacy_ledger
    )

# Claude/Codex only: serena's --context values are named profiles and no
# grok/opencode context has been validated against serena yet.
registrars = ((ClaudeRegistrar(), "claude-code"), (CodexRegistrar(), "codex"))

if action == "register":
    for registrar, _ in registrars:
        if not registrar.detect():
            continue
        current = registrar.get_server("serena")
        if current is not None and not headroom_owns_serena(registrar, current):
            failures.append(f"{registrar.name}: serena entry conflicts with user-managed configuration")
    if failures:
        sys.exit("; ".join(failures))

for registrar, context in registrars:
    if not registrar.detect():
        print(f"{registrar.name}: not detected, skipping")
        continue
    if action == "register":
        spec = ServerSpec(
            name="serena",
            command=sys.argv[2],
            args=(
                "start-mcp-server",
                "--project-from-cwd",
                "--context",
                context,
                "--open-web-dashboard",
                "False",
            ),
        )
        result = registrar_call(registrar, "register", registrar.register_server, spec)
        if result is None:
            continue
        if result.status == RegisterStatus.MISMATCH:
            current = registrar.get_server("serena")
            if headroom_owns_serena(registrar, current):
                result = registrar_call(
                    registrar, "register", registrar.register_server, spec, force=True
                )
                if result is None:
                    continue
        if result.status == RegisterStatus.REGISTERED:
            record_install(registrar.name, spec)
        elif result.status == RegisterStatus.MISMATCH:
            failures.append(f"{registrar.name}: serena entry conflicts with user-managed configuration")
        elif result.status == RegisterStatus.FAILED:
            failures.append(f"{registrar.name}: {result.detail}")
        print(f"{registrar.name}: {result.status.value}")
    else:
        current = registrar.get_server("serena")
        if current is None:
            print(f"{registrar.name}: no serena entry")
            continue
        if not headroom_installed_matching(registrar.name, current):
            print(f"{registrar.name}: serena entry is user-managed, leaving it")
            continue
        removed = registrar_call(
            registrar, "unregister", registrar.unregister_server, "serena"
        )
        if removed is None:
            # The agent CLI cannot run, so removal cannot be confirmed.
            # Report nothing rather than a failure nobody can act on.
            continue
        if removed:
            clear_install(registrar.name, "serena")
            print(f"{registrar.name}: removed")
        else:
            failures.append(f"{registrar.name}: removal failed")
if failures:
    sys.exit("; ".join(failures))
"#;

fn legacy_headroom_mcp_ledger_path() -> PathBuf {
    crate::client_adapters::home_dir()
        .join(".headroom")
        .join("mcp_installs.json")
}

/// Same ledger-guarded register/unregister flow as `SERENA_MCP_HELPER`, for
/// the Context7 MCP entry. The registered command is a bare `npx` (resolved
/// from the agent session's own PATH, so nvm version switches don't strand an
/// absolute path) running the pinned package. argv: `register <package-spec>`
/// | `unregister`.
const CONTEXT7_MCP_HELPER: &str = r#"
import sys
from pathlib import Path

from headroom.mcp_registry import (
    ClaudeRegistrar,
    CodexRegistrar,
    GrokRegistrar,
    OpencodeRegistrar,
    ServerSpec,
)
from headroom.mcp_registry.base import RegisterStatus
from headroom.mcp_registry.ledger import (
    clear_install,
    headroom_installed_matching,
    record_install,
)

action = sys.argv[1]
legacy_ledger = Path(sys.argv[3]) if len(sys.argv) > 3 else None
failures = []

def registrar_call(registrar, label, call, *args, **kwargs):
    """Run one registrar call, downgrading an unusable agent CLI to a skip.

    A detected CLI that exec() refuses -- Homebrew leaves a `claude` shim
    behind when its native binary never downloaded, and a wrong-architecture
    build or a missing interpreter fails the same way -- raises OSError out of
    subprocess. Letting it escape aborted the whole addon operation, and the
    user saw only a cause-free "installation failed". One unusable agent must
    not block the others.
    """
    try:
        return call(*args, **kwargs)
    except OSError as exc:
        print(f"{registrar.name}: {label} skipped, CLI is not runnable ({exc})")
        return None

def headroom_owns(registrar, current):
    if headroom_installed_matching(registrar.name, current):
        return True
    return legacy_ledger is not None and headroom_installed_matching(
        registrar.name, current, path=legacy_ledger
    )

for registrar in (ClaudeRegistrar(), CodexRegistrar(), GrokRegistrar(), OpencodeRegistrar()):
    if not registrar.detect():
        print(f"{registrar.name}: not detected, skipping")
        continue
    if action == "register":
        spec = ServerSpec(
            name="context7",
            command="npx",
            args=("-y", sys.argv[2]),
        )
        result = registrar_call(registrar, "register", registrar.register_server, spec)
        if result is None:
            continue
        if result.status == RegisterStatus.MISMATCH:
            current = registrar.get_server("context7")
            if headroom_owns(registrar, current):
                result = registrar_call(
                    registrar, "register", registrar.register_server, spec, force=True
                )
                if result is None:
                    continue
        if result.status == RegisterStatus.REGISTERED:
            record_install(registrar.name, spec)
        elif result.status == RegisterStatus.MISMATCH:
            failures.append(f"{registrar.name}: context7 entry conflicts with user-managed configuration")
        elif result.status == RegisterStatus.FAILED:
            failures.append(f"{registrar.name}: {result.detail}")
        print(f"{registrar.name}: {result.status.value}")
    else:
        current = registrar.get_server("context7")
        if current is None:
            print(f"{registrar.name}: no context7 entry")
            continue
        if not headroom_owns(registrar, current):
            print(f"{registrar.name}: context7 entry is user-managed, leaving it")
            continue
        removed = registrar_call(
            registrar, "unregister", registrar.unregister_server, "context7"
        )
        if removed is None:
            # The agent CLI cannot run, so removal cannot be confirmed.
            # Report nothing rather than a failure nobody can act on.
            continue
        if removed:
            clear_install(registrar.name, "context7")
            print(f"{registrar.name}: removed")
        else:
            failures.append(f"{registrar.name}: removal failed")
if failures:
    sys.exit("; ".join(failures))
"#;
/// Same ledger-guarded register/unregister flow as `SERENA_MCP_HELPER`, for
/// the codebase-memory MCP entry. `CBM_CACHE_DIR` points its index databases
/// into Headroom's managed tools dir so uninstalling removes them too.
/// argv: `register <binary> <cache-dir>` | `unregister`.
const CODEBASE_MEMORY_MCP_HELPER: &str = r#"
import sys
from pathlib import Path

from headroom.mcp_registry import (
    ClaudeRegistrar,
    CodexRegistrar,
    GrokRegistrar,
    OpencodeRegistrar,
    ServerSpec,
)
from headroom.mcp_registry.base import RegisterStatus
from headroom.mcp_registry.ledger import (
    clear_install,
    headroom_installed_matching,
    record_install,
)

action = sys.argv[1]
legacy_ledger = Path(sys.argv[4]) if len(sys.argv) > 4 else None
failures = []

def registrar_call(registrar, label, call, *args, **kwargs):
    """Run one registrar call, downgrading an unusable agent CLI to a skip.

    A detected CLI that exec() refuses -- Homebrew leaves a `claude` shim
    behind when its native binary never downloaded, and a wrong-architecture
    build or a missing interpreter fails the same way -- raises OSError out of
    subprocess. Letting it escape aborted the whole addon operation, and the
    user saw only a cause-free "installation failed". One unusable agent must
    not block the others.
    """
    try:
        return call(*args, **kwargs)
    except OSError as exc:
        print(f"{registrar.name}: {label} skipped, CLI is not runnable ({exc})")
        return None

def headroom_owns(registrar, current):
    if headroom_installed_matching(registrar.name, current):
        return True
    return legacy_ledger is not None and headroom_installed_matching(
        registrar.name, current, path=legacy_ledger
    )

for registrar in (ClaudeRegistrar(), CodexRegistrar(), GrokRegistrar(), OpencodeRegistrar()):
    if not registrar.detect():
        print(f"{registrar.name}: not detected, skipping")
        continue
    if action == "register":
        spec = ServerSpec(
            name="codebase-memory",
            command=sys.argv[2],
            env={"CBM_CACHE_DIR": sys.argv[3]},
        )
        result = registrar_call(registrar, "register", registrar.register_server, spec)
        if result is None:
            continue
        if result.status == RegisterStatus.MISMATCH:
            current = registrar.get_server("codebase-memory")
            if headroom_owns(registrar, current):
                result = registrar_call(
                    registrar, "register", registrar.register_server, spec, force=True
                )
                if result is None:
                    continue
        if result.status == RegisterStatus.REGISTERED:
            record_install(registrar.name, spec)
        elif result.status == RegisterStatus.MISMATCH:
            failures.append(f"{registrar.name}: codebase-memory entry conflicts with user-managed configuration")
        elif result.status == RegisterStatus.FAILED:
            failures.append(f"{registrar.name}: {result.detail}")
        print(f"{registrar.name}: {result.status.value}")
    else:
        current = registrar.get_server("codebase-memory")
        if current is None:
            print(f"{registrar.name}: no codebase-memory entry")
            continue
        if not headroom_owns(registrar, current):
            print(f"{registrar.name}: codebase-memory entry is user-managed, leaving it")
            continue
        removed = registrar_call(
            registrar, "unregister", registrar.unregister_server, "codebase-memory"
        )
        if removed is None:
            # The agent CLI cannot run, so removal cannot be confirmed.
            # Report nothing rather than a failure nobody can act on.
            continue
        if removed:
            clear_install(registrar.name, "codebase-memory")
            print(f"{registrar.name}: removed")
        else:
            failures.append(f"{registrar.name}: removal failed")
if failures:
    sys.exit("; ".join(failures))
"#;
/// A marketplace plugin addon installed through the host CLIs' own
/// `<cli> plugin ...` managers. The install/enable/uninstall plumbing is
/// shared; addons differ only in these identifiers.
struct PluginAddon {
    id: &'static str,
    /// `owner/repo` passed to `plugin marketplace add`.
    marketplace: &'static str,
    /// Marketplace name passed to `plugin marketplace remove`.
    marketplace_name: &'static str,
    /// `plugin@marketplace` ref used by install/enable/disable/uninstall.
    plugin_ref: &'static str,
    /// Plugin directory inside the Codex marketplace checkout.
    codex_local_path: &'static str,
    /// Host CLIs this plugin supports.
    hosts: &'static [PluginHost],
    source_url: &'static str,
    /// Paths Codex should sparse-checkout when the repository also ships a
    /// Claude marketplace with the same name.
    codex_sparse_paths: &'static [&'static str],
}

static PLUGIN_ADDONS: [PluginAddon; 10] = [
    PluginAddon {
        id: "ponytail",
        marketplace: "DietrichGebert/ponytail",
        marketplace_name: "ponytail",
        plugin_ref: "ponytail@ponytail",
        codex_local_path: ".",
        hosts: &[PluginHost::ClaudeCode, PluginHost::Codex],
        source_url: "https://github.com/DietrichGebert/ponytail",
        codex_sparse_paths: &[],
    },
    PluginAddon {
        id: "caveman",
        marketplace: "JuliusBrussee/caveman",
        marketplace_name: "caveman",
        plugin_ref: "caveman@caveman",
        codex_local_path: "./plugins/caveman",
        hosts: &[PluginHost::ClaudeCode, PluginHost::Codex],
        source_url: "https://github.com/JuliusBrussee/caveman",
        codex_sparse_paths: &[],
    },
    PluginAddon {
        id: "allinluna",
        marketplace: "zenx0x/allinluna",
        marketplace_name: "allinluna",
        plugin_ref: "allinluna@allinluna",
        codex_local_path: "./plugins/allinluna",
        hosts: &[PluginHost::Codex],
        source_url: "https://github.com/zenx0x/allinluna",
        codex_sparse_paths: &[],
    },
    PluginAddon {
        id: "openspec",
        marketplace: "Fission-AI/OpenSpec",
        marketplace_name: "openspec",
        plugin_ref: "openspec@openspec",
        codex_local_path: ".",
        hosts: &[PluginHost::Codex],
        source_url: "https://github.com/Fission-AI/OpenSpec",
        codex_sparse_paths: &[],
    },
    PluginAddon {
        id: "superpowers",
        marketplace: "openai-curated",
        marketplace_name: "openai-curated",
        plugin_ref: "superpowers@openai-curated",
        codex_local_path: "./plugins/superpowers",
        hosts: &[PluginHost::Codex],
        source_url: "https://github.com/obra/superpowers",
        codex_sparse_paths: &[],
    },
    PluginAddon {
        id: "gstack",
        marketplace: "garrytan/gstack",
        marketplace_name: "gstack",
        plugin_ref: "gstack@gstack",
        codex_local_path: ".",
        hosts: &[PluginHost::Codex],
        source_url: "https://github.com/garrytan/gstack",
        codex_sparse_paths: &[],
    },
    PluginAddon {
        id: "ralph-loop",
        marketplace: "SantanderAI/ralph",
        marketplace_name: "ralph",
        plugin_ref: "ralph-loop@ralph",
        codex_local_path: ".",
        hosts: &[PluginHost::Codex],
        source_url: "https://github.com/SantanderAI/ralph",
        codex_sparse_paths: &[],
    },
    PluginAddon {
        id: "stop-that-shit",
        marketplace: "lennney/stop-that-shit",
        marketplace_name: "stop-that-shit",
        plugin_ref: "stop-that-shit@stop-that-shit",
        codex_local_path: ".",
        hosts: &[PluginHost::ClaudeCode, PluginHost::Codex],
        source_url: "https://github.com/lennney/stop-that-shit",
        codex_sparse_paths: &[
            ".agents",
            ".codex-plugin",
            ".hermes-plugin",
            "assets",
            "hooks",
            "opencode",
            "pi",
            "scripts",
            "skills",
            "src",
            "test",
        ],
    },
    PluginAddon {
        id: "agent-guard",
        marketplace: "JeongJaeSoon/agent-guard",
        marketplace_name: "agent-guard",
        plugin_ref: "agent-guard@agent-guard",
        codex_local_path: "./plugins/agent-guard",
        hosts: &[PluginHost::ClaudeCode, PluginHost::Codex],
        source_url: "https://github.com/JeongJaeSoon/agent-guard",
        codex_sparse_paths: &[".agents", "plugins/agent-guard"],
    },
    PluginAddon {
        id: "grill-me",
        marketplace: "joshuawheelock/grill-me",
        marketplace_name: "grill-me",
        plugin_ref: "grill-me@grill-me",
        codex_local_path: ".",
        hosts: &[PluginHost::ClaudeCode, PluginHost::Codex],
        source_url: "https://github.com/joshuawheelock/grill-me",
        codex_sparse_paths: &[],
    },
];
const PLUGIN_DISPLAY_VERSION: &str = "latest";
const GSTACK_BUN_VERSION: &str = "1.3.14";

const PONYTAIL_SUPPORTED_MODES: &[&str] = &["lite", "full", "ultra"];
const CAVEMAN_SUPPORTED_MODES: &[&str] = &[
    "lite",
    "full",
    "ultra",
    "wenyan-lite",
    "wenyan-full",
    "wenyan-ultra",
];

#[derive(Clone, Copy)]
struct AddonModeSpec {
    id: &'static str,
    supported_modes: &'static [&'static str],
}

const ADDON_MODE_SPECS: &[AddonModeSpec] = &[
    AddonModeSpec {
        id: "ponytail",
        supported_modes: PONYTAIL_SUPPORTED_MODES,
    },
    AddonModeSpec {
        id: "caveman",
        supported_modes: CAVEMAN_SUPPORTED_MODES,
    },
];

fn addon_mode_spec(id: &str) -> Option<&'static AddonModeSpec> {
    ADDON_MODE_SPECS.iter().find(|spec| spec.id == id)
}

fn canonical_addon_mode(spec: &AddonModeSpec, raw: &str) -> Option<&'static str> {
    let normalized = raw.trim().to_ascii_lowercase();
    spec.supported_modes
        .iter()
        .copied()
        .find(|mode| *mode == normalized)
}

fn canonical_addon_mode_from_config(spec: &AddonModeSpec, raw: &str) -> Option<&'static str> {
    if spec.id == "caveman" && raw.trim().eq_ignore_ascii_case("wenyan") {
        return Some("wenyan-full");
    }
    canonical_addon_mode(spec, raw)
}

fn addon_home_from_environment(
    home_from_env: Option<PathBuf>,
    system_home: Option<PathBuf>,
) -> Option<PathBuf> {
    home_from_env
        .filter(|home| !home.as_os_str().is_empty())
        .or(system_home)
}

/// Resolves the user-level configuration path used by both plugins. This
/// deliberately does not consider environment-variable or repository-local
/// defaults, which take precedence inside the plugins themselves.
fn addon_user_config_path(id: &str) -> Result<PathBuf> {
    #[cfg(test)]
    {
        let _ = id;
        // Unit tests must never inspect a developer's real plugin settings.
        bail!("user-level addon configuration is unavailable in unit tests");
    }

    #[cfg(not(test))]
    {
        let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
        let home = addon_home_from_environment(
            std::env::var_os("HOME").map(PathBuf::from),
            dirs::home_dir(),
        );
        addon_user_config_path_from(
            id,
            xdg_config_home.as_deref(),
            app_data.as_deref(),
            home.as_deref(),
            cfg!(target_os = "windows"),
        )
    }
}

fn addon_user_config_path_from(
    id: &str,
    xdg_config_home: Option<&Path>,
    app_data: Option<&Path>,
    home: Option<&Path>,
    is_windows: bool,
) -> Result<PathBuf> {
    let spec = addon_mode_spec(id).ok_or_else(|| anyhow!("unknown addon mode: {id}"))?;
    let base =
        if let Some(config_home) = xdg_config_home.filter(|path| !path.as_os_str().is_empty()) {
            config_home.to_path_buf()
        } else if is_windows {
            app_data
                .filter(|path| !path.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .or_else(|| {
                    home.filter(|path| !path.as_os_str().is_empty())
                        .map(|home| home.join("AppData").join("Roaming"))
                })
                .ok_or_else(|| anyhow!("cannot locate {id} config: APPDATA and HOME are unset"))?
        } else {
            home.filter(|path| !path.as_os_str().is_empty())
                .map(|home| home.join(".config"))
                .ok_or_else(|| anyhow!("cannot locate {id} config: HOME is unset"))?
        };

    Ok(base.join(spec.id).join("config.json"))
}

fn parse_addon_config_object(
    bytes: &[u8],
    config_path: &Path,
    spec: &AddonModeSpec,
) -> Result<serde_json::Map<String, Value>> {
    let raw = std::str::from_utf8(bytes)
        .with_context(|| format!("reading UTF-8 configuration {}", config_path.display()))?;
    // Ponytail explicitly accepts a UTF-8 BOM, commonly added by Windows
    // editors. Accept it here too so the dashboard reports the same mode.
    let raw = if spec.id == "ponytail" {
        raw.strip_prefix('\u{feff}').unwrap_or(raw)
    } else {
        raw
    };
    let config: Value = serde_json::from_str(raw)
        .with_context(|| format!("parsing configuration {}", config_path.display()))?;
    config.as_object().cloned().ok_or_else(|| {
        anyhow!(
            "configuration {} must be a JSON object",
            config_path.display()
        )
    })
}

fn read_addon_default_mode_from_path(
    config_path: &Path,
    spec: &AddonModeSpec,
) -> Result<Option<String>> {
    let bytes = match std::fs::read(config_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading configuration {}", config_path.display()))
        }
    };
    let config = parse_addon_config_object(&bytes, config_path, spec)?;
    Ok(config
        .get("defaultMode")
        .and_then(Value::as_str)
        .and_then(|mode| canonical_addon_mode_from_config(spec, mode))
        .map(str::to_owned))
}

fn write_addon_default_mode_to_path(
    config_path: &Path,
    spec: &AddonModeSpec,
    mode: &str,
) -> Result<()> {
    let mode = canonical_addon_mode(spec, mode).ok_or_else(|| {
        anyhow!(
            "unsupported {} default mode {mode:?}; supported modes: {}",
            spec.id,
            spec.supported_modes.join(", ")
        )
    })?;
    let mut config = match std::fs::read(config_path) {
        Ok(bytes) => parse_addon_config_object(&bytes, config_path, spec)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading configuration {}", config_path.display()))
        }
    };
    config.insert("defaultMode".to_string(), Value::String(mode.to_string()));

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating configuration directory {}", parent.display()))?;
    }
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(config))
        .context("serializing addon configuration")?;
    bytes.push(b'\n');
    crate::client_adapters::atomic_write(config_path, &bytes)
        .with_context(|| format!("writing configuration {}", config_path.display()))
}

fn addon_default_mode_or_full(config_path: &Path, spec: &AddonModeSpec) -> String {
    read_addon_default_mode_from_path(config_path, spec)
        .ok()
        .flatten()
        .unwrap_or_else(|| "full".to_string())
}

fn addon_mode_fields(id: &str) -> (Option<String>, Vec<String>) {
    let Some(spec) = addon_mode_spec(id) else {
        return (None, Vec::new());
    };
    let default_mode = addon_user_config_path(spec.id)
        .map(|path| addon_default_mode_or_full(&path, spec))
        .unwrap_or_else(|_| "full".to_string());
    let supported_modes = spec
        .supported_modes
        .iter()
        .map(|mode| (*mode).to_string())
        .collect();
    (Some(default_mode), supported_modes)
}

fn plugin_addon(id: &str) -> Option<&'static PluginAddon> {
    PLUGIN_ADDONS.iter().find(|plugin| plugin.id == id)
}

fn plugin_uses_existing_marketplace(plugin: &PluginAddon) -> bool {
    plugin.id == "superpowers"
}

/// True when a host CLI reported that the plugin is missing from a marketplace
/// it was asked to install from. Codex says this both when our own marketplace
/// registration did not take and when Codex has not yet materialized its
/// built-in marketplace.
fn is_marketplace_plugin_missing(err: &anyhow::Error) -> bool {
    format!("{err:#}")
        .to_ascii_lowercase()
        .contains("not found in marketplace")
}

/// Add the cause and the fix to a failure that carries neither.
///
/// Superpowers lives in Codex's own `openai-curated` marketplace, which Codex
/// creates itself the first time the app starts. An update that runs before
/// that first sync fails with the bare CLI line "plugin `x` was not found in
/// marketplace `y`" -- which reads as a permanent failure, offers no action,
/// and repeats on every retry (the user's report: "I clicked update and it
/// still errors"). Keep the CLI text because it is what
/// [`plugin_install_failure_category`] matches on, but lead with the cause and
/// the one step that resolves it.
fn explain_host_marketplace_missing_plugin(
    plugin: &PluginAddon,
    err: anyhow::Error,
) -> anyhow::Error {
    if !plugin_uses_existing_marketplace(plugin) || !is_marketplace_plugin_missing(&err) {
        return err;
    }
    err.context(format!(
        "Codex has not finished preparing its built-in `{}` marketplace, so `{}` is not available from it yet. Open the Codex app once, wait for it to finish loading, then retry.",
        plugin.marketplace_name, plugin.id
    ))
}

pub(crate) fn is_plugin_addon(id: &str) -> bool {
    plugin_addon(id).is_some()
}

/// Whether the card offers an Update action, and what it would move to.
///
/// The pinned version is a *minimum*, not a target: a user already ahead of the
/// pin (they updated the plugin themselves, or a pin has not caught up yet) is
/// current, and must never be prompted into a downgrade. An unparseable version
/// on either side is treated as current for the same reason — better a missed
/// prompt than a wrong one.
///
/// `None` means no Update button: nothing installed, already at or past the
/// pin. Headroom uses the dedicated transactional runtime-upgrade action. RTK
/// is also refreshed at launch, but remains user-updatable here so the Tools screen
/// never reports an update without offering an action.
fn pending_addon_update(id: &str, installed: Option<&str>, pinned: &str) -> Option<String> {
    let installed = installed?;
    match id {
        // Plugins track a moving marketplace, not a pin, so local state cannot
        // prove that a newer version exists. The upstream check adds Update only
        // after it confirms a newer marketplace version.
        _ if plugin_addon(id).is_some() => None,
        "headroom" | "rtk" | "markitdown" | "serena" | "context7" | "codebase-memory" => {
            let on_disk = parse_major_minor_patch(installed)?;
            let target = parse_major_minor_patch(pinned)?;
            (on_disk < target).then(|| pinned.to_string())
        }
        _ => None,
    }
}
const RTK_SHA256_MACOS_AARCH64: &str =
    "3617f9a95d536e0b6bb9e2c6d121d81c3d8fd8ef04c5b2a605f5e7b0309a47d2";
const RTK_SHA256_MACOS_X86_64: &str =
    "4a6d9268a3dbf3e9d82872a4a3ac444548de613debe16a1458fc2862d1d941b6";
const RTK_SHA256_LINUX_AARCH64: &str =
    "960ceb5f1f5f0b0939b32b5b1d41dec6d9a7113137b0703c68dca0d169a260fc";
const RTK_SHA256_LINUX_X86_64: &str =
    "7c0175d867f96c4f8f788479af82ca8f0990ea944226268834d224a525186fb7";
const RTK_SHA256_WINDOWS_X86_64: &str =
    "26401cf663797bfcfd0d7fbf3acfa5d81a6fb384e8cac188506d69c330d37596";
const PYTHON_STANDALONE_RELEASE: &str = "20251014";
const PYTHON_SHA256_MACOS_AARCH64: &str =
    "84cb7acbf75264982c8bdd818bfa1ff0f1eb76007b48a5f3e01d28633b46afdf";
const PYTHON_SHA256_MACOS_X86_64: &str =
    "f76a921e71e9c8954cccd00f176b7083041527b3b4223670d05bbb2f51209d3f";
const PYTHON_SHA256_LINUX_X86_64: &str =
    "c74addcd1b033a6e4d60ead3ab47fcc995569027e01d3061c4a934f363c4a0cf";
const PYTHON_SHA256_LINUX_AARCH64: &str =
    "d2a6c0d4ceea088f635b309a59d5d700a256656423225f96ddfb71d532adb1aa";
const PYTHON_SHA256_WINDOWS_X86_64: &str =
    "3c8b9b10a933909c98b9916297e2093b24a9c2abaa23df1c2622c2bfe052cb94";

/// Venv layout differs per platform: Unix venvs place interpreters and
/// console-script entrypoints in `bin/` (python3, no extension); Windows venvs
/// use `Scripts/` with `.exe`-suffixed names. The standalone python extracted
/// by `install_python_distribution` is the exception: on Windows the
/// interpreter is `python.exe` at the extraction root.
fn python_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "python.exe"
    } else {
        "python3"
    }
}

fn pip_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "pip.exe"
    } else {
        "pip"
    }
}

fn bin_subdir() -> &'static str {
    if cfg!(target_os = "windows") {
        "Scripts"
    } else {
        "bin"
    }
}

#[derive(Debug, Clone)]
pub struct BootstrapStepUpdate {
    pub step: &'static str,
    pub message: String,
    pub eta_seconds: u64,
    pub percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedRuntime {
    pub root_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub python_dir: PathBuf,
    pub venv_dir: PathBuf,
    pub tools_dir: PathBuf,
    pub downloads_dir: PathBuf,
}

impl ManagedRuntime {
    pub fn bootstrap_root(base_dir: &Path) -> Self {
        let root_dir = base_dir.join("headroom");
        let runtime_dir = root_dir.join("runtime");
        let bin_dir = root_dir.join("bin");
        let python_dir = runtime_dir.join("python");
        let venv_dir = runtime_dir.join("venv");
        let tools_dir = root_dir.join("tools");
        let downloads_dir = root_dir.join("downloads");

        Self {
            root_dir,
            runtime_dir,
            bin_dir,
            python_dir,
            venv_dir,
            tools_dir,
            downloads_dir,
        }
    }

    pub fn ensure_layout(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root_dir)
            .with_context(|| format!("creating {}", self.root_dir.display()))?;
        std::fs::create_dir_all(&self.runtime_dir)
            .with_context(|| format!("creating {}", self.runtime_dir.display()))?;
        std::fs::create_dir_all(&self.bin_dir)
            .with_context(|| format!("creating {}", self.bin_dir.display()))?;
        std::fs::create_dir_all(&self.tools_dir)
            .with_context(|| format!("creating {}", self.tools_dir.display()))?;
        std::fs::create_dir_all(&self.downloads_dir)
            .with_context(|| format!("creating {}", self.downloads_dir.display()))?;
        Ok(())
    }

    pub fn standalone_python(&self) -> PathBuf {
        if cfg!(target_os = "windows") {
            self.python_dir.join("python.exe")
        } else {
            self.python_dir.join("bin").join("python3")
        }
    }

    pub fn managed_python(&self) -> PathBuf {
        self.venv_dir.join(bin_subdir()).join(python_exe_name())
    }

    pub fn managed_pip(&self) -> PathBuf {
        self.venv_dir.join(bin_subdir()).join(pip_exe_name())
    }

    pub fn ready_flag(&self) -> PathBuf {
        self.venv_dir.join("READY")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root_dir.join("logs")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedToolManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub runtime: String,
    pub category: String,
    pub workflow_group: Option<String>,
    pub activation_scope: String,
    pub source_url: String,
    pub version: String,
    pub checksum: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct ToolManager {
    runtime: ManagedRuntime,
    manifests: Vec<ManagedToolManifest>,
    log_marker_cache: Arc<Mutex<Option<ToolLogMarkerCache>>>,
    serena_calls_cache: Arc<Mutex<Option<SerenaCallsCache>>>,
    serena_live_stats_cache: Arc<Mutex<Option<(Instant, Option<(u64, Option<Instant>)>)>>>,
    /// False once this app process has tried to start the backend at least
    /// once. See its use in `start_headroom_background`.
    first_backend_start: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Clone)]
struct ToolLogMarkerCache {
    tool_id: String,
    path: PathBuf,
    modified: std::time::SystemTime,
    result: Option<bool>,
}

/// Cached result of scanning today's serena logs for tool-call lines.
#[derive(Debug)]
struct SerenaCallsCache {
    day: String,
    at: Instant,
    count: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct RtkDailyGainOutput {
    #[serde(default)]
    summary: Option<RtkGainSummary>,
    #[serde(default)]
    daily: Vec<RtkDailyEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct RtkDailyEntry {
    date: String,
    #[serde(default)]
    commands: u64,
    #[serde(default)]
    saved_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RtkGainSummary {
    pub total_commands: u64,
    pub total_saved: u64,
    pub avg_savings_pct: f64,
}

#[derive(Debug, Clone)]
struct HeadroomLearnMetadata {
    learned_at: Option<String>,
    pattern_count: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct HeadroomLearnProjectSummary {
    pub last_run_at: Option<String>,
    pub has_persisted_learnings: bool,
    pub pattern_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadroomLearnPreviousRun {
    pub(crate) run_key: String,
    pub(crate) finished_at: DateTime<Utc>,
    pub(crate) success: bool,
    pub(crate) error: Option<String>,
}

fn parse_headroom_learn_previous_run(
    content: &str,
    fallback_finished_at: DateTime<Utc>,
) -> Option<HeadroomLearnPreviousRun> {
    const LLM_FAILURE_MARKER: &str = "LLM analysis failed:";

    let header = content.lines().next()?;
    let run_key = header
        .rsplit_once("(target=")?
        .1
        .strip_suffix(')')?
        .to_string();
    let finished_at = header
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .and_then(|(timestamp, _)| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .unwrap_or(fallback_finished_at);
    let status = content
        .lines()
        .find_map(|line| line.strip_prefix("status: "))?
        .trim();
    let llm_failures: Vec<String> = content
        .lines()
        .filter_map(|line| {
            line.split_once(LLM_FAILURE_MARKER)
                .map(|(_, reason)| format!("{LLM_FAILURE_MARKER} {}", reason.trim()))
        })
        .collect();
    let process_succeeded = matches!(status, "exit status: 0" | "exit code: 0");
    let success = process_succeeded && llm_failures.is_empty();
    let error = if !llm_failures.is_empty() {
        Some(llm_failures.join("\n"))
    } else if success {
        None
    } else {
        let stderr = content
            .split_once("--- stderr ---")
            .map(|(_, stderr)| stderr)
            .and_then(|stderr| stderr.lines().map(str::trim).find(|line| !line.is_empty()))
            .map(|line| line.chars().take(500).collect::<String>());
        Some(stderr.unwrap_or_else(|| format!("Previous scan failed ({status}).")))
    };

    Some(HeadroomLearnPreviousRun {
        run_key,
        finished_at,
        success,
        error,
    })
}

/// HuggingFace hub cache directory name for the Kompress model. Uninstall
/// cleanup sweeps the whole `models--chopratejas--` prefix instead of this one
/// name, so the two can drift apart without leaking.
pub(crate) const KOMPRESS_HF_MODEL_DIR: &str = "models--chopratejas--kompress-v2-base";

/// HuggingFace hub cache directory for the current edition. Community must not
/// inspect, reuse, or delete an inherited/shared HuggingFace cache; all model
/// reads and cleanup stay below its workspace. Non-Community builds retain
/// `huggingface_hub`'s normal cache-resolution precedence.
///
/// Reading it from our own env is correct by construction: the bundled runtime
/// is spawned as our child and inherits this env, so wherever we resolve to is
/// where it actually writes. Hardcoding the default bit us twice — the prefetch
/// guard re-downloaded an already-cached model, and uninstall left it behind.
///
/// Two deliberate deviations from python: no `$VAR` expansion inside the values
/// (`os.path.expandvars`), and an empty value is treated as unset where
/// `os.getenv` would return `""` and resolve to a CWD-relative path. Both cases
/// are already broken upstream; falling back to the default beats guessing.
/// ponytail: add expansion if anyone reports a `$`-containing value.
pub(crate) fn hf_hub_cache_dir() -> Option<PathBuf> {
    if crate::edition::LOCAL_COMMUNITY {
        return Some(crate::edition::huggingface_hub_cache_dir());
    }

    hf_hub_cache_dir_from_env()
}

fn hf_hub_cache_dir_from_env() -> Option<PathBuf> {
    fn var(key: &str) -> Option<PathBuf> {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    var("HF_HUB_CACHE")
        .or_else(|| var("HUGGINGFACE_HUB_CACHE"))
        .or_else(|| var("HF_HOME").map(|home| home.join("hub")))
        .or_else(|| {
            // `HOME` before `dirs::home_dir()`: on Windows the dirs crate reads the
            // profile known folder and ignores `$HOME`, so a redirected home (tests,
            // Git Bash) would resolve the sweep against the REAL profile instead.
            let cache = var("XDG_CACHE_HOME").or_else(|| {
                var("HOME")
                    .or_else(dirs::home_dir)
                    .map(|h| h.join(".cache"))
            })?;
            Some(cache.join("huggingface").join("hub"))
        })
}

/// Result of a best-effort kompress model prefetch.
pub enum KompressPrefetchOutcome {
    /// Model successfully downloaded and cached.
    Downloaded,
    /// Subprocess exited non-zero. `cause` is a coarse category plus the last
    /// meaningful line of `kompress-prefetch.log`, suitable for Sentry.
    Failed { cause: String },
}

/// Build a short, Sentry-friendly cause from the tail of the prefetch log.
/// The leading `[category]` keeps related failures grouped; the trailing line
/// carries the specific error for triage.
/// httpx (used by huggingface_hub 1.x) reads `SSL_CERT_FILE`/`SSL_CERT_DIR` but
/// ignores `REQUESTS_CA_BUNDLE`. When a user behind TLS inspection has set
/// `REQUESTS_CA_BUNDLE` but not `SSL_CERT_FILE`, mirror it so the model download
/// trusts their corporate root. No-op if `SSL_CERT_FILE` is already set or no
/// bundle is configured -- the child otherwise inherits the parent env unchanged.
fn httpx_ca_bundle_bridge() -> Vec<(String, String)> {
    httpx_ca_bundle_bridge_from(
        std::env::var_os("SSL_CERT_FILE").is_some(),
        std::env::var("REQUESTS_CA_BUNDLE").ok().as_deref(),
    )
}

fn httpx_ca_bundle_bridge_from(
    ssl_cert_file_set: bool,
    requests_ca_bundle: Option<&str>,
) -> Vec<(String, String)> {
    if ssl_cert_file_set {
        return Vec::new();
    }
    match requests_ca_bundle {
        Some(bundle) if !bundle.trim().is_empty() => {
            vec![("SSL_CERT_FILE".to_string(), bundle.to_string())]
        }
        _ => Vec::new(),
    }
}

fn summarize_kompress_prefetch_failure(log_path: &Path) -> String {
    let contents = std::fs::read_to_string(log_path).unwrap_or_default();
    let lines: Vec<&str> = contents.lines().collect();
    let start = lines.len().saturating_sub(40);
    let tail = lines[start..].join("\n");

    let category = classify_kompress_prefetch_failure(&tail);
    let detail: String = tail
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        .unwrap_or("(no output in kompress-prefetch.log)")
        .chars()
        .take(200)
        .collect();

    // The auto backend tries ONNX first and only then falls back to PyTorch, so
    // when both are broken the last line is the SECOND failure and the ONNX
    // cause that actually explains it scrolls past unreported. RUST-75 arrived
    // as a torch `c10.dll` load error with no hint that onnxruntime -- which
    // needs the same MSVC redistributable -- had already failed for the same
    // reason. Carry the first cause too when the loader logged one.
    let onnx_cause: Option<String> = tail
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| line.contains("ONNX load failed"))
        .map(|line| line.chars().take(160).collect());

    // The Sentry fingerprint is the bracketed category, so widening the detail
    // does not regroup the issue.
    match onnx_cause {
        Some(onnx) => format!("[{category}] {detail} (after {onnx})"),
        None => format!("[{category}] {detail}"),
    }
}

/// Bucket a prefetch-log tail into a coarse, stable failure category.
fn classify_kompress_prefetch_failure(tail: &str) -> &'static str {
    let t = tail.to_lowercase();
    if t.is_empty() {
        "no output"
    } else if t.contains("sigabrt") || t.contains("aborted") {
        "native abort"
    } else if t.contains("no space left") || t.contains("disk full") || t.contains("errno 28") {
        "disk full"
    } else if t.contains("connection")
        || t.contains("timed out")
        || t.contains("timeout")
        || t.contains("name resolution")
        || t.contains("failed to resolve")
        || t.contains("max retries exceeded")
        || t.contains("ssl")
        || t.contains("httperror")
        // huggingface_hub 1.x transient: shared httpx client torn down mid-pull
        // (RUST-3C). A fresh subprocess gets a fresh client, so it's retriable.
        || t.contains("client has been closed")
    {
        "network"
    } else if t.contains("permission denied") {
        "permission denied"
    // torch's DLLs need the MSVC redistributable, which a fresh Windows box
    // (notably Server 2022) does not ship. Nothing retriable and nothing the
    // app can repair, but it must not sit in the "other" grab-bag: that bucket
    // is what made RUST-3C/RUST-45 unresolvable.
    //
    // 126 is "the DLL is absent", 1114 is "it is there and its init routine
    // failed" (RUST-75, c10.dll) -- different Windows errnos, same broken
    // native stack, same non-answer for us, so one bucket. Match the number,
    // not the sentence after it: Windows localizes that text, and 1114 landed
    // in the grab-bag as Korean.
    } else if t.contains("winerror 126")
        || t.contains("winerror 1114")
        || t.contains("importerror: dll load failed")
    {
        "missing native dep"
    } else {
        "other"
    }
}

impl ToolManager {
    pub fn new(runtime: ManagedRuntime) -> Self {
        let rtk_checksum = rtk_distribution_artifact()
            .ok()
            .and_then(|artifact| artifact.sha256.map(str::to_owned));
        let manifests = vec![
            ManagedToolManifest {
                id: "headroom".into(),
                name: "Headroom".into(),
                description: "Default optimizer stage for every supported client.".into(),
                runtime: "python".into(),
                category: "core".into(),
                workflow_group: None,
                activation_scope: "immediate".into(),
                source_url: "https://pypi.org/project/headroom-ai/".into(),
                version: HEADROOM_PINNED_VERSION.into(),
                checksum: None,
                required: true,
            },
            ManagedToolManifest {
                id: "rtk".into(),
                name: "RTK".into(),
                description:
                    "Token-optimized shell command proxy for your coding agent and your terminal.".into(),
                runtime: "binary".into(),
                category: "efficiency".into(),
                workflow_group: None,
                activation_scope: "immediate".into(),
                source_url: "https://github.com/rtk-ai/rtk".into(),
                version: RTK_VERSION.into(),
                checksum: rtk_checksum,
                required: false,
            },
            ManagedToolManifest {
                id: "markitdown".into(),
                name: "MarkItDown".into(),
                description:
                    "Converts PDF and Office documents to Markdown so they cost far fewer tokens when your agent reads them."
                        .into(),
                runtime: "python".into(),
                category: "documents".into(),
                workflow_group: None,
                activation_scope: "immediate".into(),
                source_url: "https://github.com/microsoft/markitdown".into(),
                version: MARKITDOWN_PINNED_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "serena".into(),
                name: "Serena".into(),
                description:
                    "MCP server that gives your agent symbol-level code tools, so it reads one function instead of a whole file. Saves most in large repos; adds its tool definitions to every request."
                        .into(),
                runtime: "python".into(),
                category: "code_intelligence".into(),
                workflow_group: None,
                activation_scope: "client_restart".into(),
                source_url: "https://github.com/oraios/serena".into(),
                version: SERENA_PINNED_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "codebase-memory".into(),
                name: "Codebase Memory".into(),
                description:
                    "MCP server that indexes your codebase into a persistent knowledge graph - call chains, classes, routes - so your agent answers structure questions from the graph instead of re-reading files. Complements Serena: pre-built map vs live symbol tools."
                        .into(),
                runtime: "binary".into(),
                category: "code_intelligence".into(),
                workflow_group: None,
                activation_scope: "client_restart".into(),
                source_url: "https://github.com/DeusData/codebase-memory-mcp".into(),
                version: CODEBASE_MEMORY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "context7".into(),
                name: "Context7".into(),
                description:
                    "MCP server that fetches current, version-specific documentation for the libraries you use, so your agent stops burning tokens on guessed or outdated APIs. Requires Node.js 20.18.1 or newer on PATH."
                        .into(),
                runtime: "node".into(),
                category: "code_intelligence".into(),
                workflow_group: None,
                activation_scope: "client_restart".into(),
                source_url: "https://github.com/upstash/context7".into(),
                version: CONTEXT7_PINNED_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "ponytail".into(),
                name: "Ponytail".into(),
                description:
                    "Plugin that nudges the agent to write the least code possible. Installs into Claude Code and Codex. Requires their CLI and Node.js on PATH."
                        .into(),
                runtime: "plugin".into(),
                category: "efficiency".into(),
                workflow_group: None,
                activation_scope: "new_session".into(),
                source_url: "https://github.com/DietrichGebert/ponytail".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "caveman".into(),
                name: "Caveman".into(),
                description:
                    "Plugin that makes the agent reply in terse caveman-speak, cutting output tokens while keeping code, commands, and errors exact. Installs into Claude Code and Codex. Requires their CLI and Node.js on PATH."
                        .into(),
                runtime: "plugin".into(),
                category: "efficiency".into(),
                workflow_group: None,
                activation_scope: "new_session".into(),
                source_url: "https://github.com/JuliusBrussee/caveman".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "allinluna".into(),
                name: "All in Luna".into(),
            description: "Codex-only plugin for coordinating multi-agent work with persistent goals, execution lanes, resource policies, and verification evidence. Uses its bundled local Python runtime. Thanks to All in Luna author @zenx0x for the open-source contribution."
                .into(),
                runtime: "plugin".into(),
                category: "automation".into(),
                workflow_group: Some("execution_engine".into()),
                activation_scope: "new_session".into(),
                source_url: "https://github.com/zenx0x/allinluna".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "openspec".into(),
                name: "OpenSpec".into(),
                description: "Specification-driven workflow for changes with explicit requirements, artifacts, and acceptance checks. Includes the OpenSpec CLI and Codex skills. Thanks to Fission AI and contributors.".into(),
                runtime: "plugin".into(),
                category: "workflow".into(),
                workflow_group: Some("primary_workflow".into()),
                activation_scope: "new_session".into(),
                source_url: "https://github.com/Fission-AI/OpenSpec".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "superpowers".into(),
                name: "Superpowers".into(),
                description: "Structured coding workflow covering brainstorming, planning, TDD, debugging, review, and delivery. Thanks to Jesse Vincent and contributors.".into(),
                runtime: "plugin".into(),
                category: "workflow".into(),
                workflow_group: Some("primary_workflow".into()),
                activation_scope: "new_session".into(),
                source_url: "https://github.com/obra/superpowers".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "gstack".into(),
                name: "gstack".into(),
                description: "Full engineering workflow with product, design, browser, QA, review, and shipping skills. Requires Bun and Node.js. Thanks to Garry Tan and contributors.".into(),
                runtime: "plugin".into(),
                category: "workflow".into(),
                workflow_group: Some("primary_workflow".into()),
                activation_scope: "new_session".into(),
                source_url: "https://github.com/garrytan/gstack".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "ralph-loop".into(),
                name: "Ralph Loop".into(),
                description: "Bounded autonomous loop that starts a fresh coding-agent session each iteration and stops on explicit completion signals. Thanks to Santander AI and contributors.".into(),
                runtime: "plugin".into(),
                category: "automation".into(),
                workflow_group: Some("execution_engine".into()),
                activation_scope: "new_session".into(),
                source_url: "https://github.com/SantanderAI/ralph".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "stop-that-shit".into(),
                name: "Stop That Shit".into(),
                description: "Guardrail plugin that helps agents stop unsafe or unwanted behavior. Thanks to @lennney and contributors.".into(),
                runtime: "plugin".into(),
                category: "guardrails".into(),
                workflow_group: None,
                activation_scope: "new_session".into(),
                source_url: "https://github.com/lennney/stop-that-shit".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "agent-guard".into(),
                name: "Agent Guard".into(),
                description: "Guardrail plugin for safer agent execution. Thanks to @JeongJaeSoon and contributors.".into(),
                runtime: "plugin".into(),
                category: "guardrails".into(),
                workflow_group: None,
                activation_scope: "new_session".into(),
                source_url: "https://github.com/JeongJaeSoon/agent-guard".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
            ManagedToolManifest {
                id: "grill-me".into(),
                name: "Grill Me".into(),
                description: "Learning plugin that stress-tests plans and assumptions. Thanks to Joshua Wheelock.".into(),
                runtime: "plugin".into(),
                category: "learning".into(),
                workflow_group: None,
                activation_scope: "new_session".into(),
                source_url: "https://github.com/joshuawheelock/grill-me".into(),
                version: PLUGIN_DISPLAY_VERSION.into(),
                checksum: None,
                required: false,
            },
        ];

        Self {
            runtime,
            manifests,
            log_marker_cache: Arc::new(Mutex::new(None)),
            serena_calls_cache: Arc::new(Mutex::new(None)),
            serena_live_stats_cache: Arc::new(Mutex::new(None)),
            first_backend_start: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    pub fn list_tools(&self) -> Vec<ManagedTool> {
        self.manifests
            .iter()
            .map(|manifest| {
                let installed = self.installed_addon_version(&manifest.id);
                let enabled = self.tool_enabled(&manifest.id);
                // Never offer Update on a disabled addon: every installer
                // writes `enabled: true`, so the update would silently switch
                // it back on. Enable first, then update.
                let pending = enabled
                    .then(|| {
                        pending_addon_update(&manifest.id, installed.as_deref(), &manifest.version)
                    })
                    .flatten();
                let update_available = pending.is_some();
                let (default_mode, supported_modes) = addon_mode_fields(&manifest.id);
                ManagedTool {
                    id: manifest.id.clone(),
                    name: manifest.name.clone(),
                    description: manifest.description.clone(),
                    runtime: manifest.runtime.clone(),
                    category: manifest.category.clone(),
                    workflow_group: manifest.workflow_group.clone(),
                    activation_scope: manifest.activation_scope.clone(),
                    required: manifest.required,
                    enabled,
                    status: self.detect_status(&manifest.id),
                    source_url: manifest.source_url.clone(),
                    version: installed.unwrap_or_else(|| manifest.version.clone()),
                    supported_version: manifest.version.clone(),
                    checksum: manifest.checksum.clone(),
                    savings_label: self.tool_savings_label(&manifest.id),
                    update_available,
                    available_version: pending.filter(|version| !version.is_empty()),
                    unavailable_reason: addon_unavailable_reason(&manifest.id),
                    default_mode,
                    supported_modes,
                }
            })
            .collect()
    }

    /// The version actually on disk, from whichever record the addon's own
    /// installer writes: the headroom receipt, the host CLI's plugin registry,
    /// or the `<id>.json` tool receipt every other installer writes. None when
    /// the addon is not installed.
    fn installed_addon_version(&self, tool_id: &str) -> Option<String> {
        if tool_id == "headroom" {
            return self.installed_headroom_version();
        }
        if let Some(plugin) = plugin_addon(tool_id) {
            return installed_plugin_version(plugin).or_else(|| {
                self.read_tool_receipt(tool_id)?
                    .get("version")?
                    .as_str()
                    .map(str::to_string)
            });
        }
        if !self.addon_installed(tool_id) {
            return None;
        }
        self.read_tool_receipt(tool_id)?
            .get("version")?
            .as_str()
            .map(str::to_string)
    }

    /// Whether an installed addon has the artifact its receipt claims. A
    /// receipt without its payload (interrupted install, user-deleted venv) is
    /// not an install, and must not produce a version or an update prompt.
    fn addon_installed(&self, tool_id: &str) -> bool {
        match tool_id {
            "rtk" => self.rtk_installed(),
            "markitdown" => self.markitdown_installed(),
            "serena" => self.serena_installed(),
            "context7" => self.context7_installed(),
            "codebase-memory" => self.codebase_memory_installed(),
            _ => false,
        }
    }

    /// Chip text for the Addons tab. markitdown and serena are measured
    /// (shim counter / serena's logs plus its live dashboard stats); ponytail
    /// and caveman are the plugins' published benchmark medians — their skills
    /// forbid inventing per-repo figures, so the labels say "benchmark". rtk's
    /// figure comes from `rtk gain` via RuntimeStatus, not from here.
    fn tool_savings_label(&self, tool_id: &str) -> Option<String> {
        match tool_id {
            "markitdown" => self.markitdown_conversion_count().map(|count| {
                if count == 1 {
                    "1 doc converted".to_string()
                } else {
                    format!("{count} docs converted")
                }
            }),
            "serena" => {
                let live = self.serena_live_stats().map(|(tokens, session_start)| {
                    (tokens, session_start.map(|start| start.elapsed()))
                });
                serena_savings_label(self.serena_tool_calls_local_today(), live)
            }
            "ponytail" => Some("47-77% lower cost (benchmark)".to_string()),
            "caveman" => Some("~65% fewer output tokens (benchmark)".to_string()),
            _ => None,
        }
    }

    /// Serena slot for the Activity-tab feed: the same two measures as the
    /// Addons-tab chip, pre-formatted by the shared `serena_savings_parts`.
    /// None when serena is not installed or neither measure has data yet.
    pub fn serena_today_stats(&self) -> Option<crate::models::SerenaTodayStats> {
        if !self.serena_installed() {
            return None;
        }
        let live = self
            .serena_live_stats()
            .map(|(tokens, session_start)| (tokens, session_start.map(|start| start.elapsed())));
        let (calls_line, tokens_line) =
            serena_savings_parts(self.serena_tool_calls_local_today(), live);
        if calls_line.is_none() && tokens_line.is_none() {
            return None;
        }
        Some(crate::models::SerenaTodayStats {
            calls_line,
            tokens_line,
        })
    }

    /// Estimated tokens serena's tools have returned in the live session(s),
    /// summed from each running MCP process's dashboard `/get_tool_stats`
    /// (v1.7.0 records unconditionally, CHAR_COUNT estimator; our registration
    /// only suppresses the browser popup, not the dashboard server). In-memory
    /// upstream, so the figure resets when the session ends — by design.
    /// Paired with the oldest matching MCP process's start (from `ps` etime),
    /// so the chip can say over what span those tokens accumulated.
    /// 60s cache; closed local ports refuse instantly, so a miss is cheap.
    fn serena_live_stats(&self) -> Option<(u64, Option<Instant>)> {
        if !self.serena_installed() {
            return None;
        }
        {
            let cache = self.serena_live_stats_cache.lock();
            if let Some((at, stats)) = cache.as_ref() {
                if at.elapsed() < Duration::from_secs(60) {
                    return *stats;
                }
            }
        }
        // Serena assigns the first free port above its base. Discover current
        // listeners so later sessions remain visible even after the first four
        // ports are occupied (or older sessions leave gaps).
        let stats = fetch_serena_output_tokens_from_ports(&discover_serena_dashboard_ports()).map(
            |total| {
                let session_start = self
                    .serena_oldest_session_age()
                    .and_then(|age| Instant::now().checked_sub(age));
                (total, session_start)
            },
        );
        *self.serena_live_stats_cache.lock() = Some((Instant::now(), stats));
        stats
    }

    /// Age of the oldest running serena MCP session, from `ps` elapsed time.
    /// Matched on our managed entrypoint path plus the `start-mcp-server`
    /// subcommand — same argv-identity idea as `pid_is_headroom_backend`.
    /// With several sessions the tokens above span all of them, so the oldest
    /// is the honest window. `-ww`: unlimited width, argv must not truncate.
    fn serena_oldest_session_age(&self) -> Option<Duration> {
        let output = crate::proc::command("/bin/ps")
            .args(["-axww", "-o", "etime=,args="])
            .output()
            .ok()?;
        let marker = self.serena_entrypoint().display().to_string();
        oldest_serena_session_age(&String::from_utf8_lossy(&output.stdout), &marker)
    }

    /// Today's serena tool calls, counted from `~/.serena/logs/<local day>/`.
    /// Serena's usage stats are in-memory only (analytics.py), so its log
    /// files are the only persisted trace: one line containing
    /// "; session_id: " per tool application (tools_base.py, pinned v1.7.0).
    /// Local day matches serena's own log-dir bucketing (datetime.now()).
    /// 60s cache: Result lines make these files large enough that scanning on
    /// every dashboard poll would be wasteful.
    fn serena_tool_calls_local_today(&self) -> Option<u64> {
        let day = Local::now().format("%Y-%m-%d").to_string();
        {
            let cache = self.serena_calls_cache.lock();
            if let Some(cached) = cache.as_ref() {
                if cached.day == day && cached.at.elapsed() < Duration::from_secs(60) {
                    return cached.count;
                }
            }
        }
        // ponytail: default ~/.serena only; honor SERENA_HOME if a user ever
        // actually overrides it.
        let count = dirs::home_dir()
            .map(|home| home.join(".serena").join("logs").join(&day))
            .and_then(|dir| count_serena_tool_calls_in_dir(&dir));
        *self.serena_calls_cache.lock() = Some(SerenaCallsCache {
            day,
            at: Instant::now(),
            count,
        });
        count
    }

    pub fn python_runtime_installed(&self) -> bool {
        // The base interpreter counts too, not just the venv. A venv's
        // `Scripts/python.exe` is a redirector stub that execs the interpreter
        // recorded in `pyvenv.cfg`; deleting `runtime/python` (AV quarantine,
        // disk cleanup) leaves the stub and the READY flag on disk, so a
        // venv-only gate reads "installed" while every spawn dies with exit 103
        // / `No Python at '...'` (RUST-8E). That matters because this gate is
        // what routes a missing runtime back to setup, and bootstrap already
        // re-downloads the distribution and rebuilds the venv from it -- the
        // only thing standing between the user and a self-repair was this
        // check. Same blind spot as RUST-66/6M one level down.
        self.runtime.ready_flag().exists()
            && self.runtime.managed_python().exists()
            && self.runtime.standalone_python().exists()
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.runtime.logs_dir()
    }

    pub fn headroom_entrypoint(&self) -> PathBuf {
        let name = if cfg!(target_os = "windows") {
            "headroom.exe"
        } else {
            "headroom"
        };
        self.runtime.venv_dir.join(bin_subdir()).join(name)
    }

    pub fn managed_python(&self) -> PathBuf {
        self.runtime.managed_python()
    }

    pub fn rtk_entrypoint(&self) -> PathBuf {
        let name = if cfg!(target_os = "windows") {
            "rtk.exe"
        } else {
            "rtk"
        };
        self.runtime.bin_dir.join(name)
    }

    pub fn allinluna_entrypoint(&self) -> PathBuf {
        let name = if cfg!(target_os = "windows") {
            "allinluna.cmd"
        } else {
            "allinluna"
        };
        self.runtime.bin_dir.join(name)
    }

    fn allinluna_runtime_path(&self) -> Option<PathBuf> {
        let home = crate::client_adapters::home_dir();
        let codex_home = crate::client_adapters::codex_home();
        [
            codex_home.join(".tmp/marketplaces/allinluna/plugins/allinluna/runtime"),
            home.join(".claude/plugins/marketplaces/allinluna/plugins/allinluna/runtime"),
        ]
        .into_iter()
        .find(|path| path.join("allinluna_runtime/__main__.py").is_file())
    }

    fn ensure_allinluna_launcher(&self) -> Result<()> {
        let launcher = self.allinluna_entrypoint();
        let managed_python = self.runtime.managed_python();

        #[cfg(windows)]
    let body = format!(
        "@echo off\r\nif defined CODEX_HOME (set \"CODEX_ROOT=%CODEX_HOME%\") else (set \"CODEX_ROOT=%USERPROFILE%\\.codex\")\r\nset \"PLUGIN_RUNTIME=%CODEX_ROOT%\\.tmp\\marketplaces\\allinluna\\plugins\\allinluna\\runtime\"\r\nif not exist \"%PLUGIN_RUNTIME%\\allinluna_runtime\\__main__.py\" set \"PLUGIN_RUNTIME=%USERPROFILE%\\.claude\\plugins\\marketplaces\\allinluna\\plugins\\allinluna\\runtime\"\r\nif not exist \"%PLUGIN_RUNTIME%\\allinluna_runtime\\__main__.py\" (\r\n echo All in Luna requires installed marketplace runtime Headroom-managed Python ^>= 3.11. Install or enable All in Luna again. 1>&2\r\n exit /b 1\r\n)\r\nif defined PYTHONPATH (\r\n set \"PYTHONPATH=%PLUGIN_RUNTIME%;%PYTHONPATH%\"\r\n) else (\r\n set \"PYTHONPATH=%PLUGIN_RUNTIME%\"\r\n)\r\nset \"PYTHONNOUSERSITE=1\"\r\n\"{}\" -m allinluna_runtime %*\r\n",
        managed_python.display()
    );
        #[cfg(not(windows))]
    let body = format!(
        "#!/bin/sh\nset -eu\ncodex_root=\"${{CODEX_HOME:-$HOME/.codex}}\"\nfor plugin_runtime in \\\n \"$codex_root/.tmp/marketplaces/allinluna/plugins/allinluna/runtime\" \\\n \"$HOME/.claude/plugins/marketplaces/allinluna/plugins/allinluna/runtime\"; do\n if [ -f \"$plugin_runtime/allinluna_runtime/__main__.py\" ]; then\n  PYTHONPATH=\"$plugin_runtime${{PYTHONPATH:+:$PYTHONPATH}}\"\n  export PYTHONPATH\n  export PYTHONNOUSERSITE=1\n  exec '{}' -m allinluna_runtime \"$@\"\n fi\ndone\necho 'All in Luna requires installed marketplace runtime Headroom-managed Python >= 3.11.' >&2\nexit 1\n",
        managed_python.display().to_string().replace('\'', "'\\''")
    );

        crate::client_adapters::atomic_write(&launcher, body.as_bytes())
            .with_context(|| format!("writing {}", launcher.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&launcher)
                .with_context(|| format!("reading permissions of {}", launcher.display()))?
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&launcher, permissions)
                .with_context(|| format!("marking {} executable", launcher.display()))?;
        }

        Ok(())
    }

    fn remove_allinluna_launcher(&self) -> Result<()> {
        let launcher = self.allinluna_entrypoint();
        if launcher.exists() {
            std::fs::remove_file(&launcher)
                .with_context(|| format!("removing {}", launcher.display()))?;
        }

        Ok(())
    }

    fn allinluna_launcher_healthy(&self) -> bool {
        self.python_runtime_installed()
            && self.allinluna_entrypoint().is_file()
            && self.allinluna_runtime_path().is_some()
    }

    fn codex_plugin_source_path(&self, plugin: &PluginAddon) -> PathBuf {
        self.marketplace_package_root(plugin)
            .join(plugin.codex_local_path.trim_start_matches("./"))
    }

    fn marketplace_package_root(&self, plugin: &PluginAddon) -> PathBuf {
        crate::client_adapters::codex_home()
            .join(".tmp")
            .join("marketplaces")
            .join(plugin.marketplace_name)
    }
    fn openspec_entrypoint(&self) -> PathBuf {
        self.runtime.bin_dir.join(if cfg!(target_os = "windows") {
            "openspec.cmd"
        } else {
            "openspec"
        })
    }

    fn ralph_entrypoint(&self) -> PathBuf {
        self.runtime.bin_dir.join(if cfg!(target_os = "windows") {
            "ralph-loop.cmd"
        } else {
            "ralph-loop.sh"
        })
    }

    fn ensure_openspec_launcher(&self, plugin: &PluginAddon) -> Result<()> {
        let runtime = self
            .marketplace_package_root(plugin)
            .join("bin/openspec.js");
        if !runtime.is_file() {
            bail!("OpenSpec runtime was not built at {}", runtime.display());
        }
        let launcher = self.openspec_entrypoint();
        #[cfg(windows)]
        let body = format!("@echo off\r\nnode \"{}\" %*\r\n", runtime.display());
        #[cfg(not(windows))]
        let body = format!(
            "#!/bin/sh\nexec node '{}' \"$@\"\n",
            runtime.display().to_string().replace('\'', "'\\''")
        );
        crate::client_adapters::atomic_write(&launcher, body.as_bytes())
            .with_context(|| format!("writing {}", launcher.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&launcher)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&launcher, permissions)?;
        }
        Ok(())
    }

    fn ensure_ralph_launcher(&self, plugin: &PluginAddon) -> Result<()> {
        let source_root = self.marketplace_package_root(plugin);
        let source = if cfg!(target_os = "windows") {
            source_root.join("ralph-loop.ps1")
        } else {
            source_root.join("ralph-loop.sh")
        };
        let bytes = std::fs::read(&source)
            .with_context(|| format!("reading Ralph Loop runtime {}", source.display()))?;
        let launcher = self.ralph_entrypoint();
        #[cfg(windows)]
        let bytes = format!(
            "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" %*\r\n",
            source.display()
        )
        .into_bytes();
        crate::client_adapters::atomic_write(&launcher, &bytes)
            .with_context(|| format!("writing {}", launcher.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&launcher)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&launcher, permissions)?;
        }
        Ok(())
    }

    fn ensure_plugin_runtime(&self, plugin: &PluginAddon) -> Result<()> {
        match plugin.id {
            "allinluna" => self.ensure_allinluna_launcher(),
            "openspec" => self.ensure_openspec_launcher(plugin),
            "ralph-loop" => self.ensure_ralph_launcher(plugin),
            _ => Ok(()),
        }
    }

    fn remove_plugin_runtime(&self, plugin: &PluginAddon) -> Result<()> {
        let launcher = match plugin.id {
            "allinluna" => return self.remove_allinluna_launcher(),
            "openspec" => Some(self.openspec_entrypoint()),
            "ralph-loop" => Some(self.ralph_entrypoint()),
            _ => None,
        };
        if let Some(launcher) = launcher {
            if launcher.exists() {
                std::fs::remove_file(&launcher)
                    .with_context(|| format!("removing {}", launcher.display()))?;
            }
        }
        Ok(())
    }

    fn plugin_runtime_healthy(&self, plugin: &PluginAddon) -> bool {
        let package_root = self.marketplace_package_root(plugin);
        let source_root = self.codex_plugin_source_path(plugin);
        match plugin.id {
            "allinluna" => self.allinluna_launcher_healthy(),
            "openspec" => {
                self.openspec_entrypoint().is_file()
                    && package_root.join("dist/cli/index.js").is_file()
            }
            "gstack" => {
                let browse = if cfg!(target_os = "windows") {
                    package_root.join("browse/dist/browse.exe")
                } else {
                    package_root.join("browse/dist/browse")
                };
                browse.is_file() && source_root.join(".agents/skills").is_dir()
            }
            "ralph-loop" => {
                self.ralph_entrypoint().is_file() && package_root.join("ralph-loop.sh").is_file()
            }
            _ => true,
        }
    }

    /// Seed the output-shaper savings baseline by mining the user's Claude Code
    /// transcripts once. The proxy's `/stats` `output_shaping` estimate stays
    /// `available: false` until this baseline exists, so without it the
    /// dashboard would never show an output-reduction number. Heuristic-only
    /// (no `--llm-judge`), so it needs no API key or network, and writes the
    /// baseline into the Community workspace's `output_savings.json` (the same
    /// `workspace_dir` the proxy's recorder reads).
    ///
    /// Targets a single transcript-rich project rather than `--all`: upstream's
    /// `_run_verbosity` writes the ledger *inside* its per-project loop
    /// (last-project-wins), so `--apply --all` overwrites the baseline with
    /// whatever project sorts last — often a near-empty one. We instead pick the
    /// project with the most transcript bytes and pass its real path via
    /// `--project`. Baseline strata (model / turn kind / size / tools) are
    /// project-independent, so one busy project yields a usable baseline. (A
    /// proper cross-project aggregate belongs upstream; tracked separately.)
    ///
    /// Best-effort and idempotent: skips when a baseline is already present.
    ///
    /// MUST run *before* the proxy starts. The proxy's `SavingsRecorder` loads
    /// the baseline once at boot and, on its periodic flush, writes its
    /// in-memory ledger back to disk — so a baseline written after the proxy is
    /// running is both invisible (never reloaded) and eventually clobbered by an
    /// empty-baseline flush. Seeding first means the recorder boots with the
    /// real baseline and the number shows without an app relaunch. Synchronous
    /// but bounded by `HEADROOM_BASELINE_SEED_TIMEOUT`; callers already run it on
    /// a background thread, so the one-time ~3s scan never blocks the UI.
    ///
    /// The learned verbosity level it also writes is intentionally ignored: the
    /// proxy spawn pins `HEADROOM_VERBOSITY_LEVEL=2`, the manual-override tier.
    pub fn seed_verbosity_baseline_if_needed(&self) {
        if verbosity_baseline_present() {
            log::debug!("verbosity baseline seeding skipped: baseline already present");
            return;
        }
        let entrypoint = self.headroom_entrypoint();
        if !entrypoint.exists() {
            log::debug!(
                "verbosity baseline seeding skipped: entrypoint not yet installed at {}",
                entrypoint.display()
            );
            return;
        }
        log::info!("seeding output-shaper verbosity baseline (no baseline present yet)");
        let Some(project_cwd) = busiest_claude_project_cwd() else {
            log::info!("verbosity baseline seeding skipped: no Claude transcripts found");
            return;
        };
        let args = [
            "learn",
            "--verbosity",
            "--apply",
            "--project",
            project_cwd.as_str(),
        ];
        // Bounded so a pathological transcript corpus can never hang launch:
        // typical runs are a few seconds; the cap only trips on outliers, after
        // which we proceed and retry next launch.
        match run_command_with_timeout(
            &entrypoint,
            &args,
            &self.runtime.root_dir,
            HEADROOM_BASELINE_SEED_TIMEOUT,
        ) {
            Ok(()) => log::info!("seeded output-shaper verbosity baseline from {project_cwd}"),
            Err(err) => log::info!("verbosity baseline seeding failed: {err:#}"),
        }
    }

    pub fn headroom_learn_log_path(&self, project_path: &str) -> PathBuf {
        let logs_dir = self.runtime.logs_dir();
        let project_name = Path::new(project_path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("project");
        let safe_name: String = project_name
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        let mut hasher = Sha256::new();
        hasher.update(project_path.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        let short_hash = &digest[..12];
        logs_dir.join(format!("headroom-learn-{safe_name}-{short_hash}.log"))
    }

    pub(crate) fn latest_headroom_learn_run(&self) -> Option<HeadroomLearnPreviousRun> {
        std::fs::read_dir(self.runtime.logs_dir())
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with("headroom-learn-") && name.ends_with(".log")
                })
            })
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .max_by_key(|(modified, _)| *modified)
            .and_then(|(modified, path)| {
                let fallback_finished_at: DateTime<Utc> = modified.into();
                let content = std::fs::read_to_string(path).ok()?;
                parse_headroom_learn_previous_run(&content, fallback_finished_at)
            })
    }

    pub fn headroom_learn_last_run_at(&self, project_path: &str) -> Option<String> {
        let path = self.headroom_learn_log_path(project_path);
        if let Ok(modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) {
            let timestamp: DateTime<Utc> = modified.into();
            return Some(timestamp.to_rfc3339());
        }

        self.headroom_learn_metadata(project_path)
            .and_then(|metadata| metadata.learned_at)
    }

    /// Bundled metadata used to populate a `ClaudeCodeProject` row. Reads
    /// CLAUDE.md + MEMORY.md once instead of three times, which collapses
    /// 6 file reads per project down to 2 during the project list scan.
    pub fn headroom_learn_project_summary(
        &self,
        project_path: &str,
    ) -> HeadroomLearnProjectSummary {
        let metadata = self.headroom_learn_metadata(project_path);
        let log_last_run_at = std::fs::metadata(self.headroom_learn_log_path(project_path))
            .and_then(|meta| meta.modified())
            .ok()
            .map(|m| {
                let t: DateTime<Utc> = m.into();
                t.to_rfc3339()
            });
        HeadroomLearnProjectSummary {
            last_run_at: log_last_run_at
                .or_else(|| metadata.as_ref().and_then(|m| m.learned_at.clone())),
            has_persisted_learnings: metadata.is_some(),
            pattern_count: metadata.and_then(|m| m.pattern_count),
        }
    }

    /// `reclaim_healthy_orphan`: forwarded to `reclaim_orphan_proxy` so an
    /// upgrade boot validation replaces even a still-healthy old proxy squatting
    /// on 6868. Pass `false` for normal launch (leave a live backend alone).
    pub fn start_headroom_background(&self, reclaim_healthy_orphan: bool) -> Result<Child> {
        // First backend start of this app process: we hold no Child handle, so
        // a backend already on 6868 that `pid_is_headroom_backend` vouches for
        // is an orphan from a previous instance -- classically the one the
        // Windows updater leaves running when it exits the old app with no
        // teardown at all. Take the port back even though the orphan answers
        // /readyz: leaving it alone bails the whole start with "already
        // running", so the new build serves its traffic through the OLD
        // version's backend behind a startup error. Blast radius is the same
        // one the upgrade path already accepts -- one port, one process that
        // had to pass the identity gate.
        let first_start = self
            .first_backend_start
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        let mut allow_repair = true;
        'attempt: loop {
            let python = self.managed_python();
            if !python.exists() {
                bail!("headroom managed python not found at {}", python.display());
            }

            let entrypoint = self.headroom_entrypoint();

            let mut failures: Vec<HeadroomStartupFailure> = Vec::new();
            let logs_dir = self.runtime.logs_dir();
            std::fs::create_dir_all(&logs_dir)
                .with_context(|| format!("creating {}", logs_dir.display()))?;

            // Pre-flight: 6868 may already be held. Three cases:
            //   * Free → spawn on 6868.
            //   * HeadroomRunning → an orphaned proxy from a prior session is
            //     squatting on the port (a healthy one would have satisfied
            //     `is_headroom_proxy_reachable` upstream, so `ensure_headroom_running`
            //     would never have reached the spawn path). Reclaim the port by
            //     terminating it, then spawn the fresh runtime. Only bail if it
            //     turns out to be genuinely health-serving or we can't free it.
            //   * ForeignOccupant → try to fall back to a port in
            //     6869..=6890. Only bail if every fallback is also taken.
            // The chosen port is stored in `backend_port` so the intercept,
            // health probes, and spawn args all pick it up.
            let initial_state = diagnose_proxy_port_settled(backend_port::DEFAULT_BACKEND_PORT);
            match initial_state {
                PortState::Free => {
                    backend_port::set(backend_port::DEFAULT_BACKEND_PORT);
                }
                PortState::HeadroomRunning => {
                    reclaim_orphan_proxy(
                        backend_port::DEFAULT_BACKEND_PORT,
                        reclaim_healthy_orphan || first_start,
                    )?;
                    backend_port::set(backend_port::DEFAULT_BACKEND_PORT);
                }
                PortState::ForeignOccupant(detail) => {
                    let pid = parse_pid_from_lsof_detail(&detail);
                    let try_bind = |port: u16| TcpListener::bind(("127.0.0.1", port)).is_ok();
                    match backend_port::select_fallback(detail.clone(), pid, try_bind) {
                        Ok(SelectedFallback {
                            port,
                            original_occupant,
                            original_pid,
                        }) => {
                            backend_port::set(port);
                            log::warn!(
                                "[backend_port] {} held by {}; falling back to {}",
                                backend_port::DEFAULT_BACKEND_PORT,
                                original_occupant,
                                port,
                            );
                            sentry::with_scope(
                                |scope| {
                                    scope.set_tag("flow", "backend_port_fallback");
                                    scope.set_tag(
                                        "occupant_cmd",
                                        original_occupant
                                            .split(" pid ")
                                            .next()
                                            .unwrap_or("unknown"),
                                    );
                                    scope.set_extra(
                                        "original_port",
                                        backend_port::DEFAULT_BACKEND_PORT.into(),
                                    );
                                    scope.set_extra("chosen_port", port.into());
                                    if let Some(p) = original_pid {
                                        scope.set_extra("occupant_pid", p.into());
                                    }
                                },
                                || {
                                    sentry::capture_message(
                                        &format!(
                                            "backend_port_fallback: {} held by {}, using {}",
                                            backend_port::DEFAULT_BACKEND_PORT,
                                            original_occupant,
                                            port,
                                        ),
                                        sentry::Level::Info,
                                    );
                                },
                            );
                        }
                        Err(AllForeign {
                            original_occupant,
                            fallback_range,
                            ..
                        }) => {
                            bail!(
                                "{}",
                                format_all_foreign_bail(
                                    backend_port::DEFAULT_BACKEND_PORT,
                                    &original_occupant,
                                    fallback_range,
                                )
                            );
                        }
                    }
                }
            }

            // Construct spawn variants AFTER pre-flight so `--port` reflects any
            // fallback chosen above. The arg helpers read `backend_port::get()`
            // eagerly; building them earlier bakes in the stale default and the
            // proxy ends up trying to bind the foreign-held port.
            // Use the console_scripts entrypoint when available to avoid the Python
            // -m double-import RuntimeWarning. Fall back to -m if missing.
            let startup_variants: Vec<(PathBuf, Vec<String>)> = if entrypoint.exists() {
                vec![
                    (
                        entrypoint,
                        headroom_entrypoint_startup_args(
                            self.installed_headroom_version().as_deref(),
                            !crate::client_adapters::is_auto_learn_disabled(),
                        ),
                    ),
                    (python.clone(), headroom_python_startup_args()),
                ]
            } else {
                vec![(python.clone(), headroom_python_startup_args())]
            };

            for (executable, args) in &startup_variants {
                let variant = if args.is_empty() {
                    "default".to_string()
                } else {
                    sanitize_log_variant(&args.join("-"))
                };
                let log_path = logs_dir.join(format!("headroom-{variant}.log"));
                rotate_log_if_large(&log_path);
                let log_file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .with_context(|| format!("opening {}", log_path.display()))?;

                // SIGUSR1 -> faulthandler dump of all Python threads into the
                // proxy log (see SITECUSTOMIZE_PY). The dir holds nothing but
                // sitecustomize.py, so PYTHONPATH can't shadow real imports.
                // Best-effort: a failed write only costs the wedge diagnostics.
                let inject_dir = self.runtime.root_dir.join("pyinject");
                if let Err(err) = std::fs::create_dir_all(&inject_dir).and_then(|_| {
                    crate::client_adapters::atomic_write(
                        &inject_dir.join("sitecustomize.py"),
                        SITECUSTOMIZE_PY.as_bytes(),
                    )
                    .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err.to_string()))
                }) {
                    log::warn!("[tool_manager] writing pyinject/sitecustomize.py failed: {err}");
                }

                // Drop control samples left by the abandoned 1% holdout before
                // the 3% one starts filling the arm. One shot, stamped: from
                // here on the control arm is live data and clearing it every
                // spawn would empty it as fast as it fills.
                purge_legacy_output_savings_control_arm_once();

                // Cross-turn dedup + cold-prefix recompaction (headroom-ai
                // 0.33.0; older fallback runtimes ignore unknown envs).
                // DEDUPE is prefix-monotonic/cache-safe in every handler, so
                // it is unconditional. COLD_RECOMPACT rewrites the prefix only
                // when the prompt cache is confirmed dead. The anthropic
                // handler reads Claude Code's real TTL from cache_control; the
                // openai-format handler (Codex/OpenCode/Grok) would fall back
                // to a static 300s guess that busts still-warm caches on
                // 6-60min resumes, so we seed its learned-TTL table (below)
                // with OpenAI's documented worst-case instead of gating the
                // flag off for those connectors. CACHE_TTL_LEARN is
                // observation-only (size-capped local JSONL) and feeds the
                // future TTL learner that can replace the seed with measured
                // values. Connector state is read once per spawn; toggles
                // apply on the next backend restart.
                let cold_recompact = crate::client_adapters::is_claude_code_enabled();
                let cache_ttl_learn = crate::client_adapters::is_codex_enabled();

                // Seed value: a TTL learned from this user's own cache
                // observations when enough have accumulated, else 3600s =
                // OpenAI's "caches are always evicted within 1h" upper bound.
                // Either way Codex-path recompaction only fires on a provably
                // dead cache. Best-effort like sitecustomize.py: if the write
                // fails, resolve_learned_ttl returns None and the proxy uses
                // its 300s static guess — recompact still works, just with
                // the aggressive threshold.
                let ttl_seed_path = self.runtime.root_dir.join("cache_ttl_seed.json");
                let openai_ttl = learned_openai_ttl_seconds(
                    &crate::edition::workspace_dir().join("cache_ttl_observations.jsonl"),
                )
                .unwrap_or(3600);
                if let Err(err) = crate::client_adapters::atomic_write(
                    &ttl_seed_path,
                    format!(r#"{{"openai": {{"ttl_seconds": {openai_ttl}}}}}"#).as_bytes(),
                ) {
                    log::warn!("[tool_manager] writing cache_ttl_seed.json failed: {err}");
                }

                // Runs at default priority, deliberately. This used to be
                // wrapped in `nice` (+5, then +2 after the first round of
                // starvation) on the theory that the backend is background
                // work that should yield to foreground apps. It isn't: every
                // Claude Code request blocks on it, so deprioritizing it
                // deprioritizes exactly what the user is waiting for. Worse,
                // it inverted under load -- the case it existed for. A niced
                // backend on a contended machine misses the watchdog's health
                // probes, gets force-killed, truncates in-flight SSE streams,
                // and the restart costs more CPU than the nicing ever saved
                // (2026-08-17: 34 restarts in one morning at load 55 on 8
                // cores). The Windows path never niced at all.
                let mut command = {
                    let mut c = crate::proc::command(executable);
                    c.args(args);
                    c
                };
                command.current_dir(&self.runtime.root_dir);
                crate::edition::apply_runtime_env(&mut command);
                apply_httpx_compatible_proxy_env(&mut command);
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    command.process_group(0);
                }
                let mut child = command
                    .env("PYTHONNOUSERSITE", "1")
                    .env("PYTHONPATH", &inject_dir)
                    .env("PYTHONUNBUFFERED", "1")
                    .env("PYTHONFAULTHANDLER", "1")
                    .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
                    .env("PIP_NO_INPUT", "1")
                    // Force huggingface_hub off the native `hf_xet` downloader.
                    // Its Rust extension can SIGABRT ("Fatal Python error: Aborted")
                    // inside xet_get while pulling kompress-int8.onnx during
                    // eager_load_compressors, killing the interpreter before it
                    // binds the port (Sentry: never opened port within timeout).
                    // The SIGABRT is uncatchable in Python; disabling xet falls
                    // back to the stable HTTPS download path.
                    .env("HF_HUB_DISABLE_XET", "1")
                    // Persistent vocab cache. tiktoken defaults to
                    // $TMPDIR/data-gym-cache, which macOS purges, so the backend
                    // re-downloads vocab files; the fetch (requests.get, no
                    // timeout) can stall on a blocked network and wedge backend
                    // boot on the main thread (RUST-5D). A stable dir survives
                    // reboots and runtime reinstalls; prefetch_tiktoken_encodings
                    // seeds it so boot never needs the network for vocab.
                    .env("TIKTOKEN_CACHE_DIR", self.tiktoken_cache_dir())
                    .env("HEADROOM_SDK", "headroom-local-community-proxy")
                    // Community keeps upstream collection disabled. Dashboard
                    // stats come from the local interceptor, never a beacon.
                    .env("HEADROOM_TELEMETRY", "off")
                    // Explicitly disable headroom-ai's upstream session beacon.
                    .env("HEADROOM_BEACON", "0")
                    .env("HEADROOM_HTTP2", "false")
                    // Disable the HTTP/1.1 keep-alive pool for the upstream
                    // (proxy -> api.anthropic.com) client. Claude Code cancels
                    // streaming requests constantly (ESC, aborted tool calls,
                    // subagent cancellations), which can leave a pooled TLS
                    // connection desynced; reusing it surfaces as
                    // "SSLV3_ALERT_BAD_RECORD_MAC" on the next request. The
                    // proxy's retry path does not catch SSL/RemoteProtocolError,
                    // so the raw error leaks back to the client. Fresh
                    // connection per request avoids reuse of a poisoned socket.
                    .env("HEADROOM_MAX_KEEPALIVE", "0")
                    // Optimization mode. token: compress the frozen tool_result
                    // history for real raw-token savings. cache mode was tested and
                    // reverted -- on Claude Code subscription traffic it is ~a no-op
                    // (CacheAligner ships enabled=False, so the prefix-cache discount
                    // is 100% client-driven; cache mode only avoids busting it and
                    // adds no compression), leaving the savings chart flat.
                    .env("HEADROOM_MODE", "token")
                    .env("HEADROOM_DEDUPE", "1")
                    .env(
                        "HEADROOM_COLD_RECOMPACT",
                        if cold_recompact { "1" } else { "0" },
                    )
                    .env(
                        "HEADROOM_CACHE_TTL_LEARN",
                        if cache_ttl_learn { "1" } else { "0" },
                    )
                    .env("HEADROOM_CACHE_TTL_LEARNED_PATH", &ttl_seed_path)
                    // Off-path background compression (#1171). The Kompress ML pass
                    // over the stable prefix is CPU-bound Rust that releases the GIL,
                    // so 3+ concurrent Claude Code sessions run their passes in true
                    // parallel and saturate CPU. Each pass then stretches past the 30s
                    // COMPRESSION_TIMEOUT; asyncio cancels the awaiter but the Rust
                    // worker is non-preemptible and keeps burning a core to completion
                    // (a "leaked thread"), so the machine never drains and every
                    // session stalls ~30s/request -> apparent freeze. With this on, a
                    // cold-start-large request forwards uncompressed immediately and
                    // compresses on a dedicated single-thread background pool that
                    // never contends with the request path, breaking the cascade.
                    // Costs first-turn savings per session; steady state recovers once
                    // the prefix is compressed once.
                    .env("HEADROOM_BACKGROUND_COMPRESSION", "1")
                    // Pin per-request auth-mode policy enforcement ON. This is what
                    // keeps OAuth/subscription traffic (Claude Code, classified
                    // SUBSCRIPTION by User-Agent) on the conservative
                    // live-zone-only + cache-aligner-off policy so prior-turn
                    // compression never rewrites the frozen, already-cached prefix
                    // and busts Anthropic's prefix cache. Upstream defaults this to
                    // "enabled", but we set it explicitly so a future headroom-ai
                    // bump can't silently flip the default and fall subscription
                    // traffic back to the PAYG-aggressive policy (resolve_policy()
                    // returns policy_default_payg() when enforcement is off) -- a
                    // net loss on cache-billed subscription sessions.
                    .env("HEADROOM_PROXY_AUTH_MODE_POLICY_ENFORCEMENT", "enabled")
                    // User-message text compression is intentionally OFF: user
                    // turns carry the coding working set (code, errors, paths)
                    // and Claude Code's <system-reminder> blocks (CLAUDE.md),
                    // which the model must see verbatim; the token mass is
                    // tool_results, which compress regardless. As of headroom-ai
                    // 0.34.0 the "coding" persona flipped to
                    // compress_user_messages=True and the profile kwargs force
                    // it on per request (HEADROOM_COMPRESS_USER_MESSAGES can
                    // only force-ENABLE, never disable), so the OFF posture is
                    // enforced by SITECUSTOMIZE_PY flipping the persona field
                    // back. (The 0.34.0 tag-split bug that also mangled
                    // CLAUDE.md system-reminders was fixed upstream in 0.35.0
                    // by #2887; the flip stays because plain user text would
                    // still compress.) Trade-off: gives up the modest
                    // Codex/OpenAI user-text savings (0.5 read-discount, 0.0
                    // write-penalty) that HEADROOM_COMPRESS_USER_MESSAGES=1 enabled.
                    // Output-token shaping (new in headroom-ai 0.27.0). The proxy
                    // never emits output tokens, so this works request-side: it
                    // appends a byte-stable verbosity instruction to the TAIL of
                    // the system prompt (after the cache_control breakpoint, so the
                    // provider prefix cache is preserved) and lowers an
                    // already-present output_config.effort on mechanically-classified
                    // turns. Off by default upstream; enabled here. Effort router
                    // and mechanical-effort use upstream defaults (on, "low"). The
                    // shaper only ever lowers an effort the client already sent and
                    // never toggles thinking.type, so it cannot 400 a model that
                    // lacks effort support.
                    // headroom-ai 0.36+ rollout-gates this beta feature before
                    // consulting HEADROOM_OUTPUT_SHAPER; without the channel,
                    // /stats reports requested=true but blocked_by_channel.
                    .env("HEADROOM_ROLLOUT_CHANNEL", "beta")
                    .env("HEADROOM_OUTPUT_SHAPER", "1")
                    // Pin the steering level explicitly. An explicit env is the
                    // manual-override tier in the shaper's level resolution, so it
                    // wins over the per-user learned level written to verbosity.json
                    // by the baseline-seeding `learn --verbosity` run. That keeps
                    // steering uniform/predictable across users while the seeded
                    // baseline still feeds the /stats savings estimate. Level 2 =
                    // skip pre/postamble, don't restate in-context code/tool output.
                    .env("HEADROOM_VERBOSITY_LEVEL", "2")
                    // 3% of conversations run unshaped, as the control arm of a
                    // standing A/B. This is the only way the output-shaping
                    // number ever stops being a counterfactual: the seeded
                    // baseline is frozen at install time and cannot be relearned
                    // (every transcript written since is already shaped, so a
                    // re-learn would collapse the baseline onto the treatment
                    // mean and report ~0 savings). Control conversations are the
                    // only unshaped replies we will ever see again.
                    //
                    // Assignment is per conversation (`assign_arm` hashes the
                    // conversation key), so a conversation never flips mid-stream
                    // and the prefix cache stays stable.
                    //
                    // 1% was tried first and abandoned, but the fraction was
                    // never the bug: `best_estimate` prefers the measured number
                    // as soon as ONE stratum holds a sample in both arms, which
                    // is how three stale control samples reported -1439.9%. The
                    // desktop no longer reads that figure -- `output_savings`
                    // recomputes both estimators from the ledger and only shows
                    // the measured one once it covers real traffic at a usable
                    // band -- so collecting the arm is now safe. At 1% a heavy
                    // user needs ~90 days to reach a +/-13pp band and a light one
                    // never gets there; 3% reaches the same precision in ~30 days
                    // and still costs only 3% of conversations.
                    //
                    // Invisible to the compression figures: the arm gates the
                    // `shape_request` call alone, so control conversations are
                    // compressed, memory-augmented and cache-aligned exactly like
                    // any other, and the input-savings rate is priced off
                    // `cost.total_input_cost_usd`, which no output token enters.
                    .env("HEADROOM_OUTPUT_HOLDOUT", "0.03")
                    // Agent savings persona (new in headroom-ai 0.30.0). The
                    // `proxy` entrypoint reads HEADROOM_SAVINGS_PROFILE into
                    // config.savings_profile, and proxy_pipeline_kwargs() applies
                    // the persona's compression knobs per request across all
                    // handlers. The "coding" persona holds the active code working
                    // set verbatim (protect_recent=2, protect_analysis_context,
                    // smart_crusher_with_compaction) with a low min_tokens so
                    // compression stays visible, and target_ratio unset so savings
                    // emerge from lossless + relevance rather than a forced keep.
                    // The persona set compress_user_messages=False through
                    // 0.33.x; 0.34.0 flipped it to True, so user-turn
                    // protection is now restored by the SITECUSTOMIZE_PY
                    // persona patch (see that constant).
                    // Version-gated: "coding" only exists in _PROFILES from 0.30.0.
                    // When 0.30.0 boot-validation times out the app falls back to
                    // 0.28.0, whose profile set is {agent-90, balanced} only —
                    // passing "coding" there makes get_agent_savings_profile raise
                    // and the proxy exit 1 before opening the port (Sentry RUST-1M).
                    // Fall back to the runtime default that exists in every version.
                    .env(
                        "HEADROOM_SAVINGS_PROFILE",
                        savings_profile_for_runtime(self.installed_headroom_version().as_deref()),
                    )
                    // cc-switch reconciler. Off upstream by default; the desktop
                    // opts in so a user who points Claude Code at a third-party
                    // Anthropic-compatible endpoint (Kimi, DeepSeek, GLM) keeps
                    // Headroom in the path instead of dropping out of it. The
                    // reconciler captures that endpoint as the proxy upstream and
                    // rewrites env.ANTHROPIC_BASE_URL back to us, leaving the
                    // user's token untouched -- so their traffic is still
                    // compressed, and token-priced providers are exactly where
                    // compression is worth the most. Version-gated: see
                    // cc_switch_reconcile_for_runtime.
                    .env(
                        "HEADROOM_CC_SWITCH_RECONCILE",
                        cc_switch_reconcile_for_runtime(
                            self.installed_headroom_version().as_deref(),
                        ),
                    )
                    // Pre-upstream concurrency. The proxy's own auto is
                    // max(2, min(8, cpu_count)) — hard-capped at 8 to protect the
                    // event loop from CPU-bound compression. The desktop runs with
                    // HEADROOM_BACKGROUND_COMPRESSION=1 (above), which moves that
                    // CPU work off the request path, so the semaphore slots are
                    // mostly I/O-bound and the 8-cap just queues heavy multi-agent
                    // load (30+ sessions) until acquire timeouts degrade /readyz
                    // and the watchdog force-kills. Scale with cores instead.
                    .env(
                        "HEADROOM_ANTHROPIC_PRE_UPSTREAM_CONCURRENCY",
                        pre_upstream_concurrency().to_string(),
                    )
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(
                        log_file
                            .try_clone()
                            .with_context(|| format!("cloning {}", log_path.display()))?,
                    ))
                    .stderr(Stdio::from(log_file))
                    .spawn()
                    .with_context(|| {
                        format!(
                            "starting headroom background process: {} {}",
                            executable.display(),
                            args.join(" ")
                        )
                    })?;

                let mut startup_ok = false;
                let mut reason: Option<String> = None;

                let startup_polls = (HEADROOM_STARTUP_TIMEOUT_MS / HEADROOM_STARTUP_POLL_MS).max(1);
                for _ in 0..startup_polls {
                    thread::sleep(Duration::from_millis(HEADROOM_STARTUP_POLL_MS));
                    if is_local_proxy_reachable() {
                        startup_ok = true;
                        break;
                    }

                    match child.try_wait() {
                        Ok(Some(status)) => {
                            reason = Some(format!(
                                "exited with status {} before opening port {}",
                                status,
                                headroom_proxy_port()
                            ));
                            break;
                        }
                        Ok(None) => {}
                        Err(err) => {
                            reason = Some(format!("wait check failed: {}", err));
                            break;
                        }
                    }
                }

                if startup_ok {
                    return Ok(child);
                }

                // Timeout path (process still alive, port never opened): send SIGABRT
                // so PYTHONFAULTHANDLER=1 dumps all-thread tracebacks to the log file
                // before the process dies. Skip if the process already exited on its own.
                if reason.is_none() {
                    let _ = crate::proc::command("/bin/kill")
                        .arg("-ABRT")
                        .arg(child.id().to_string())
                        .status();
                    thread::sleep(Duration::from_millis(500));
                }

                let _ = child.kill();
                let _ = child.wait();

                let reason = reason.unwrap_or_else(|| {
                    format!(
                        "never opened port {} within {}ms",
                        headroom_proxy_port(),
                        HEADROOM_STARTUP_TIMEOUT_MS
                    )
                });
                failures.push(HeadroomStartupFailure {
                    program: executable.display().to_string(),
                    args: args.iter().map(|s| s.to_string()).collect(),
                    log_path: log_path.display().to_string(),
                    log_tail: tail_log_file(&log_path, 80),
                    reason,
                });
            }

            // All variants failed. If the proxy crashed because the venv has a
            // pydantic / pydantic-core skew (e.g. a partial upgrade left
            // pydantic-core ahead of pydantic), pin pydantic-core back to the
            // version pydantic asks for and retry once. The error message itself
            // tells us the required version — see extract_required_pydantic_core_version.
            if allow_repair {
                if let Some(target) = failures
                    .iter()
                    .find_map(|f| extract_required_pydantic_core_version(&f.log_tail))
                {
                    log::warn!(
                        "headroom proxy failed with pydantic-core/pydantic skew; \
                     reinstalling pydantic-core=={target} and retrying"
                    );
                    match self.repair_pydantic_core(&target) {
                        Ok(()) => {
                            log::warn!("pydantic-core repair succeeded; retrying headroom startup");
                            allow_repair = false;
                            continue 'attempt;
                        }
                        Err(repair_err) => {
                            log::error!("pydantic-core repair failed: {repair_err:#}");
                        }
                    }
                }
            }

            // Report the variant that actually captured a log tail (a traceback)
            // as the error whose tail is carried into Sentry `extra`. Otherwise
            // an empty-tailed variant can be reported as `last` while a prior
            // variant holds the real traceback — which `prior_summary` then drops
            // (it keeps only program/args/reason). Fall back to the last attempt.
            let chosen = failures
                .iter()
                .rposition(|f| !f.log_tail.is_empty())
                .unwrap_or(failures.len() - 1);
            let last = failures.remove(chosen);
            let prior_summary = if failures.is_empty() {
                String::new()
            } else {
                let joined = failures
                    .iter()
                    .map(|f| format!("{} {} {}", f.program, f.args.join(" "), f.reason))
                    .collect::<Vec<_>>()
                    .join("; ");
                format!(" (prior attempts: {})", joined)
            };
            return Err(anyhow::Error::from(last).context(format!(
                "unable to keep headroom running in background{}",
                prior_summary
            )));
        }
    }

    pub fn latest_tool_log_path(&self, tool_id: &str) -> Option<PathBuf> {
        let logs_dir = self.runtime.logs_dir();
        let entries = std::fs::read_dir(&logs_dir).ok()?;
        let prefix = format!("{tool_id}-");
        let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with(&prefix) && name.ends_with(".log"))
                    .unwrap_or(false)
            })
            .filter_map(|path| {
                let modified = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok()?;
                Some((modified, path))
            })
            .collect();

        candidates.sort_by_key(|(modified, _)| *modified);
        candidates.last().map(|(_, path)| path.clone())
    }

    pub fn read_headroom_log_tail(&self, max_lines: usize) -> Result<Vec<String>> {
        self.read_tool_log_tail("headroom", max_lines)
    }

    pub fn read_rtk_activity(&self, max_lines: usize) -> Result<Vec<String>> {
        if !self.rtk_installed() {
            return Ok(vec!["RTK is not installed yet.".into()]);
        }

        let output = crate::proc::command(self.rtk_entrypoint())
            .arg("session")
            .current_dir(&self.runtime.root_dir)
            .output()
            .with_context(|| format!("starting {} session", self.rtk_entrypoint().display()))?;

        if !output.status.success() {
            return Err(anyhow!(
                "command failed: {} session\nstdout:\n{}\nstderr:\n{}",
                self.rtk_entrypoint().display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines: Vec<String> = stdout.lines().map(|line| line.to_string()).collect();
        if lines.len() > max_lines {
            lines = lines.split_off(lines.len() - max_lines);
        }
        Ok(lines)
    }

    pub fn read_tool_log_tail(&self, tool_id: &str, max_lines: usize) -> Result<Vec<String>> {
        let Some(path) = self.latest_tool_log_path(tool_id) else {
            return Ok(Vec::new());
        };

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let lines = content
            .lines()
            .rev()
            .take(max_lines)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|line| line.to_string())
            .collect();
        Ok(lines)
    }

    fn latest_tool_log_marker_state(
        &self,
        tool_id: &str,
        enabled_markers: &[&str],
        disabled_markers: &[&str],
    ) -> Option<bool> {
        let path = self.latest_tool_log_path(tool_id)?;
        self.scan_file_for_marker_state_cached(tool_id, &path, enabled_markers, disabled_markers)
    }

    fn scan_file_for_marker_state_cached(
        &self,
        cache_key: &str,
        path: &Path,
        enabled_markers: &[&str],
        disabled_markers: &[&str],
    ) -> Option<bool> {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;

        {
            let cache = self.log_marker_cache.lock();
            if let Some(cached) = cache.as_ref() {
                if cached.tool_id == cache_key && cached.path == path && cached.modified == modified
                {
                    return cached.result;
                }
            }
        }

        let content = std::fs::read_to_string(path).ok()?;

        let mut result: Option<bool> = None;
        for line in content.lines().rev() {
            let lowered = line.to_ascii_lowercase();
            if enabled_markers
                .iter()
                .any(|marker| lowered.contains(marker))
            {
                result = Some(true);
                break;
            }
            if disabled_markers
                .iter()
                .any(|marker| lowered.contains(marker))
            {
                result = Some(false);
                break;
            }
        }

        let mut cache = self.log_marker_cache.lock();
        *cache = Some(ToolLogMarkerCache {
            tool_id: cache_key.to_string(),
            path: path.to_path_buf(),
            modified,
            result,
        });

        result
    }

    pub fn headroom_mcp_configured(&self) -> Option<bool> {
        self.read_headroom_receipt()?
            .get("mcp")?
            .get("configured")?
            .as_bool()
    }

    pub fn headroom_mcp_error(&self) -> Option<String> {
        self.read_headroom_receipt()?
            .get("mcp")?
            .get("error")?
            .as_str()
            .map(|value| value.to_string())
    }

    pub fn headroom_mcp_install_method(&self) -> Option<String> {
        self.read_headroom_receipt()?
            .get("mcp")?
            .get("installMethod")?
            .as_str()
            .map(|value| value.to_string())
    }

    pub fn headroom_ml_installed(&self) -> Option<bool> {
        self.read_headroom_receipt()?
            .get("ml")?
            .get("installed")?
            .as_bool()
    }

    pub fn headroom_kompress_enabled(&self) -> Option<bool> {
        // The `headroom` Python package attaches a RotatingFileHandler to its
        // `headroom` root logger with `propagate = False` (see helpers.py:
        // `_setup_file_logging`). Proxy-logger INFO lines — including the
        // `Kompress: ENABLED/not installed/disabled` startup markers — go to
        // the Community workspace's `logs/proxy.log` only, never to the stderr stream that
        // our Tauri-spawned log captures. Probe that file first; fall back to
        // the spawn-time tool log (covers older headroom versions that do
        // propagate to stderr).
        // Positive markers: startup reports either `ENABLED` or `DEFERRED`
        // (configured and ready to load on first use), followed by legacy
        // lazy-load success lines or the 0.36+ per-request backend line. The
        // scan returns the most recent marker, so both cold and warm runtimes
        // report the feature as enabled.
        const KOMPRESS_ENABLED_MARKERS: &[&str] = &[
            "kompress: enabled",
            "kompress: deferred",
            "kompress onnx loaded",
            "kompress pytorch loaded",
            "kompress slow compress backend=",
        ];
        const KOMPRESS_DISABLED_MARKERS: &[&str] =
            &["kompress: not installed", "kompress: disabled"];
        if let Some(path) = headroom_propagated_proxy_log_path() {
            if let Some(state) = self.scan_file_for_marker_state_cached(
                "headroom-proxy-log",
                &path,
                KOMPRESS_ENABLED_MARKERS,
                KOMPRESS_DISABLED_MARKERS,
            ) {
                return Some(state);
            }
        }

        self.latest_tool_log_marker_state(
            "headroom",
            KOMPRESS_ENABLED_MARKERS,
            KOMPRESS_DISABLED_MARKERS,
        )
    }

    /// True if the Kompress model snapshot is already present in the
    /// HuggingFace hub cache (`<hf_hub_cache_dir>/<KOMPRESS_HF_MODEL_DIR>/
    /// snapshots/<rev>`). Used as the prefetch idempotency guard so we never
    /// re-download an existing model.
    pub fn kompress_model_cached(&self) -> bool {
        let Some(hub) = hf_hub_cache_dir() else {
            return false;
        };
        let snapshots = hub.join(KOMPRESS_HF_MODEL_DIR).join("snapshots");
        std::fs::read_dir(&snapshots)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
    }

    /// Download the Kompress model (~850MB measured, not the ~260MB this doc
    /// used to claim) into the HF cache by running the
    /// bundled venv python's loader with network enabled. Blocks until the
    /// download finishes — call this on a background thread. Output is captured
    /// to `logs/kompress-prefetch.log`.
    ///
    /// This front-loads the download the proxy would otherwise do lazily on
    /// first request, so a fresh install has ML compression ready before any
    /// traffic. It is best-effort: on failure the proxy's own lazy-load path
    /// still downloads on first use. On a non-zero exit the returned
    /// [`KompressPrefetchOutcome::Failed`] carries a short, Sentry-friendly
    /// cause read from the tail of the prefetch log.
    pub fn prefetch_kompress_model(&self) -> Result<KompressPrefetchOutcome> {
        let python = self.managed_python();
        if !python.exists() {
            bail!("headroom managed python not found at {}", python.display());
        }
        let logs_dir = self.runtime.logs_dir();
        std::fs::create_dir_all(&logs_dir)
            .with_context(|| format!("creating {}", logs_dir.display()))?;
        let log_path = logs_dir.join("kompress-prefetch.log");

        // hf_hub_download resumes from its `.incomplete` blob via range
        // requests, so re-invoking the subprocess continues a partial pull
        // rather than restarting it. Retry only network-category failures
        // (RUST-3C): a dropped connection or TLS blip on the ~315MB model is
        // exactly what a second attempt fixes; permission/other failures won't
        // improve on retry.
        //
        // Why retries at the subprocess level at all: huggingface_hub 1.21.0's
        // own in-process backoff self-destructs on the first ConnectError --
        // `_http_backoff_base` caches `client = get_session()` before its retry
        // loop, the ConnectError handler calls `close_session()`, and the next
        // iteration reuses the closed client, raising "RuntimeError: Cannot
        // send a request, as the client has been closed" (the exact RUST-3C
        // message). A fresh subprocess gets a fresh client and resumes the
        // blob. ponytail: 5 attempts, linear backoff -- raised from 3 after
        // 0.5.9 telemetry showed hosts burning all 3 (RUST-45 -> RUST-3C).
        const MAX_ATTEMPTS: u32 = 5;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.run_kompress_prefetch_once(&python, &log_path)? {
                KompressPrefetchOutcome::Downloaded => {
                    return Ok(KompressPrefetchOutcome::Downloaded);
                }
                KompressPrefetchOutcome::Failed { cause } => {
                    if !cause.starts_with("[network]") || attempt == MAX_ATTEMPTS {
                        return Ok(KompressPrefetchOutcome::Failed { cause });
                    }
                    // Info: a retried transient is not a fleet signal (RUST-45
                    // spam); the final Failed outcome carries the cause.
                    log::info!(
                        "kompress prefetch attempt {attempt}/{MAX_ATTEMPTS} failed (retrying): {cause}"
                    );
                    std::thread::sleep(std::time::Duration::from_secs(3 * attempt as u64));
                }
            }
        }
        unreachable!("kompress prefetch loop returns on the final attempt")
    }

    /// Runs a single kompress preload subprocess, appending its output to
    /// `kompress-prefetch.log`. Retry/backoff across network failures lives in
    /// the caller, [`Self::prefetch_kompress_model`].
    fn run_kompress_prefetch_once(
        &self,
        python: &Path,
        log_path: &Path,
    ) -> Result<KompressPrefetchOutcome> {
        rotate_log_if_large(log_path);
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .with_context(|| format!("opening {}", log_path.display()))?;

        let mut command = crate::proc::command(python);
        let status = crate::edition::apply_runtime_env(&mut command)
            .arg("-c")
            .arg(
                "from headroom.transforms.kompress_compressor import KompressCompressor; \
                 KompressCompressor().preload(allow_download=True)",
            )
            .current_dir(&self.runtime.root_dir)
            .env("PYTHONNOUSERSITE", "1")
            .env("PYTHONUNBUFFERED", "1")
            // Same xet guard as the proxy spawn: the native hf_xet downloader
            // can SIGABRT mid-pull; the HTTPS fallback is stable.
            .env("HF_HUB_DISABLE_XET", "1")
            // huggingface_hub 1.x downloads over httpx, which reads SSL_CERT_FILE
            // but NOT REQUESTS_CA_BUNDLE. Users behind corporate TLS inspection
            // who set REQUESTS_CA_BUNDLE (per our bootstrap remediation) got pip
            // working but the model pull still failed cert verification (Sentry
            // RUST-3C). Bridge their bundle into SSL_CERT_FILE so httpx honors it.
            .envs(httpx_ca_bundle_bridge())
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log_file
                    .try_clone()
                    .with_context(|| format!("cloning {}", log_path.display()))?,
            ))
            .stderr(Stdio::from(log_file))
            .status()
            .with_context(|| format!("running kompress prefetch via {}", python.display()))?;

        if status.success() {
            Ok(KompressPrefetchOutcome::Downloaded)
        } else {
            Ok(KompressPrefetchOutcome::Failed {
                cause: summarize_kompress_prefetch_failure(log_path),
            })
        }
    }

    /// Stable on-disk tiktoken vocab cache, passed to the backend as
    /// TIKTOKEN_CACHE_DIR (see the proxy spawn for why the default tmp
    /// location is not good enough).
    pub fn tiktoken_cache_dir(&self) -> PathBuf {
        self.runtime.root_dir.join("tiktoken-cache")
    }

    /// Best-effort pre-download of the tiktoken vocabularies the backend
    /// loads at startup (RUST-5D: tiktoken's vocab fetch has no network
    /// timeout, so a stalled download wedges backend boot until the watchdog
    /// auto-pauses). Seeds [`Self::tiktoken_cache_dir`] once; with the cache
    /// populated the backend never touches the network for vocab. Output goes
    /// to `logs/tiktoken-prefetch.log`. The subprocess is killed after a
    /// deadline for the same no-timeout reason.
    pub fn prefetch_tiktoken_encodings(&self) -> Result<()> {
        const DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
        let cache_dir = self.tiktoken_cache_dir();
        // ponytail: coarse skip — any cached entry counts as seeded. A partial
        // cache (one of two vocabs) still self-completes lazily because the
        // backend writes to the same persistent dir.
        if std::fs::read_dir(&cache_dir)
            .map(|mut dir| dir.next().is_some())
            .unwrap_or(false)
        {
            return Ok(());
        }
        let python = self.managed_python();
        if !python.exists() {
            bail!("headroom managed python not found at {}", python.display());
        }
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("creating {}", cache_dir.display()))?;

        let logs_dir = self.runtime.logs_dir();
        std::fs::create_dir_all(&logs_dir)
            .with_context(|| format!("creating {}", logs_dir.display()))?;
        let log_path = logs_dir.join("tiktoken-prefetch.log");
        rotate_log_if_large(&log_path);
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("opening {}", log_path.display()))?;

        let mut command = crate::proc::command(&python);
        let mut child = crate::edition::apply_runtime_env(&mut command)
            .arg("-c")
            // cl100k_base: the proxy's default/fallback encoding.
            // o200k_base: current OpenAI model family, loaded for codex traffic.
            .arg(
                "import tiktoken; \
                 tiktoken.get_encoding('cl100k_base'); \
                 tiktoken.get_encoding('o200k_base')",
            )
            .current_dir(&self.runtime.root_dir)
            .env("PYTHONNOUSERSITE", "1")
            .env("PYTHONUNBUFFERED", "1")
            .env("TIKTOKEN_CACHE_DIR", &cache_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log_file
                    .try_clone()
                    .with_context(|| format!("cloning {}", log_path.display()))?,
            ))
            .stderr(Stdio::from(log_file))
            .spawn()
            .with_context(|| format!("running tiktoken prefetch via {}", python.display()))?;

        let started = std::time::Instant::now();
        loop {
            match child.try_wait().context("waiting for tiktoken prefetch")? {
                Some(status) if status.success() => return Ok(()),
                Some(status) => {
                    let tail = log_tail(&log_path, 1024);
                    bail!("tiktoken prefetch exited with {status}: {tail}");
                }
                None if started.elapsed() >= DEADLINE => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let tail = log_tail(&log_path, 1024);
                    bail!(
                        "tiktoken prefetch timed out after {}s (stalled vocab download): {tail}",
                        DEADLINE.as_secs()
                    );
                }
                None => std::thread::sleep(std::time::Duration::from_millis(500)),
            }
        }
    }

    fn read_headroom_receipt(&self) -> Option<Value> {
        let path = self.runtime.tools_dir.join("headroom.json");
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn read_rtk_receipt(&self) -> Option<Value> {
        let path = self.runtime.tools_dir.join("rtk.json");
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn headroom_learn_metadata(&self, project_path: &str) -> Option<HeadroomLearnMetadata> {
        let mut candidates = self
            .headroom_learn_memory_paths(project_path)
            .into_iter()
            .filter_map(|path| read_headroom_learn_metadata_from_path(&path))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.sort_key.cmp(&left.sort_key));
        candidates
            .into_iter()
            .next()
            .map(|candidate| candidate.metadata)
    }

    fn headroom_learn_memory_paths(&self, project_path: &str) -> Vec<PathBuf> {
        vec![
            // Current learn versions write CLAUDE.local.md; older ones wrote
            // CLAUDE.md. The freshest candidate wins, so list both.
            Path::new(project_path).join("CLAUDE.local.md"),
            Path::new(project_path).join("CLAUDE.md"),
            claude_project_memory_file(project_path),
        ]
    }

    /// Returns the installed Headroom version from the tool receipt, if any.
    pub fn installed_headroom_version(&self) -> Option<String> {
        self.read_headroom_receipt()?
            .get("version")?
            .as_str()
            .map(|v| v.to_string())
    }

    fn installed_requirements_lock_sha(&self) -> Option<String> {
        self.read_headroom_receipt()?
            .get("artifact")?
            .get("requirementsLockSha256")?
            .as_str()
            .map(|v| v.to_string())
    }

    pub fn rtk_installed(&self) -> bool {
        self.rtk_entrypoint().exists() && self.runtime.tools_dir.join("rtk.json").exists()
    }

    pub fn installed_rtk_version(&self) -> Option<String> {
        self.read_rtk_receipt()?
            .get("version")?
            .as_str()
            .map(|v| v.to_string())
    }

    pub fn rtk_needs_install(&self) -> bool {
        if !self.rtk_entrypoint().exists() {
            return true;
        }
        match self.installed_rtk_version().as_deref() {
            Some(version) => match (
                parse_major_minor_patch(version),
                parse_major_minor_patch(RTK_VERSION),
            ) {
                (Some(current), Some(pin)) => current < pin,
                _ => true,
            },
            None => true,
        }
    }

    /// Refresh an *already installed* rtk to the pinned version. Never creates a
    /// fresh install: RTK is opt-in, so a missing binary means the user has not
    /// installed it (or uninstalled it) and launch must leave it absent.
    /// Returns Ok(true) if work was done, Ok(false) if already current or absent.
    pub fn ensure_rtk_current(&self) -> Result<bool> {
        let pending = self.runtime.downloads_dir.join("rtk-install.pending");
        if pending.exists() {
            self.install_rtk()?;
            return Ok(true);
        }
        if !self.rtk_entrypoint().exists() {
            return Ok(false);
        }
        if !self.rtk_needs_install() {
            return Ok(false);
        }
        self.install_rtk()?;
        Ok(true)
    }

    fn rtk_gain_output(&self) -> Option<RtkDailyGainOutput> {
        if !self.rtk_installed() {
            return None;
        }
        let output = crate::proc::command(self.rtk_entrypoint())
            .args(["gain", "--daily", "--format", "json"])
            .current_dir(&self.runtime.root_dir)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        serde_json::from_slice(&output.stdout).ok()
    }

    pub fn rtk_gain_summary(&self) -> Option<RtkGainSummary> {
        self.rtk_gain_output()?.summary
    }

    pub fn rtk_today_stats(&self) -> Option<RtkTodayStats> {
        let today = Local::now().date_naive().to_string();
        self.rtk_gain_output()?
            .daily
            .into_iter()
            .find(|entry| entry.date == today)
            .map(|entry| RtkTodayStats {
                date: entry.date,
                saved_tokens: entry.saved_tokens,
                commands: entry.commands,
            })
    }

    /// Returns the bundled release only when the installed version is below the pin.
    pub fn check_headroom_upgrade(&self) -> Option<HeadroomRelease> {
        let installed = parse_major_minor_patch(&self.installed_headroom_version()?)?;
        let pinned = parse_major_minor_patch(HEADROOM_PINNED_VERSION)?;
        (installed < pinned)
            .then(|| pinned_headroom_release().ok())
            .flatten()
    }

    pub fn check_headroom_upgrade_for_version(
        &self,
        target_version: &str,
    ) -> Option<HeadroomRelease> {
        if !self.headroom_upgrade_target_is_newer(target_version) {
            return None;
        }
        if target_version == HEADROOM_PINNED_VERSION {
            Some(pinned_headroom_release().ok()?)
        } else {
            Some(fetch_headroom_release(target_version).ok()?)
        }
    }

    pub fn headroom_upgrade_target_is_newer(&self, target_version: &str) -> bool {
        let Some(installed) = self.installed_headroom_version() else {
            return false;
        };
        match (
            parse_major_minor_patch(&installed),
            parse_major_minor_patch(target_version),
        ) {
            (Some(current), Some(target)) => target > current,
            _ => false,
        }
    }

    /// Returns true if the compiled requirements lock differs from what was
    /// used during the last headroom install.
    ///
    /// As a side effect, if the stored sha is a known legacy value whose
    /// pinned versions are byte-identical to the current lock, rewrites the
    /// receipt with the new-format sha and returns false. This avoids a
    /// purely cosmetic reinstall on the 0.2.50 → 0.3.0 jump.
    pub fn requirements_are_stale(&self) -> bool {
        let Some(stored) = self.installed_requirements_lock_sha() else {
            return true;
        };
        let current = requirements_lock_sha(bootstrap_requirements_lock());
        if stored == current {
            return false;
        }
        if LEGACY_REQUIREMENTS_LOCK_SHAS.contains(&stored.as_str()) {
            if let Err(err) = self.write_requirements_lock_sha_to_receipt(&current) {
                log::warn!("failed to migrate legacy requirementsLockSha256: {err}");
            }
            return false;
        }
        true
    }

    fn write_requirements_lock_sha_to_receipt(&self, sha: &str) -> Result<()> {
        let receipt_path = self.runtime.tools_dir.join("headroom.json");
        let bytes = std::fs::read(&receipt_path)
            .with_context(|| format!("reading {}", receipt_path.display()))?;
        let mut receipt: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", receipt_path.display()))?;
        if let Some(artifact) = receipt.get_mut("artifact").and_then(|a| a.as_object_mut()) {
            artifact.insert("requirementsLockSha256".into(), json!(sha));
        } else {
            return Ok(());
        }
        crate::client_adapters::atomic_write(&receipt_path, &serde_json::to_vec(&receipt)?)
            .with_context(|| format!("writing {}", receipt_path.display()))?;
        Ok(())
    }

    pub fn repair_stale_requirements_with_progress<F>(&self, mut progress: F) -> Result<()>
    where
        F: FnMut(BootstrapStepUpdate),
    {
        let requirements_lock = bootstrap_requirements_lock();
        let lock_path = self.write_headroom_requirements_lock(requirements_lock)?;

        progress(BootstrapStepUpdate {
            step: "Repairing dependencies",
            message: "Repairing Headroom's bundled dependencies.".into(),
            eta_seconds: 60,
            percent: 40,
        });

        let deps_start = Instant::now();
        let progress_ref = std::cell::RefCell::new(&mut progress);
        let mut dep_counter: u32 = 0;
        run_pip_install_with_retries_streaming(
            &self.runtime.managed_python(),
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                PIP_ONLY_BINARY,
                "--find-links",
                VENDOR_WHEELS_INDEX_URL,
                "--extra-index-url",
                "https://pypi.org/simple",
                "--upgrade",
                "--requirement",
                lock_path.to_string_lossy().as_ref(),
            ],
            &self.runtime.root_dir,
            |line| {
                if let Some(update) =
                    pip_line_to_progress(line, deps_start.elapsed(), &mut dep_counter, 40, 82)
                {
                    if let Ok(mut cb) = progress_ref.try_borrow_mut() {
                        (cb)(BootstrapStepUpdate {
                            step: "Repairing dependencies",
                            message: update.message,
                            eta_seconds: update.eta_seconds,
                            percent: update.percent,
                        });
                    }
                }
            },
        )
        .context("repairing stale headroom requirements")?;

        progress(BootstrapStepUpdate {
            step: "Configuring integrations",
            message: "Setting up Headroom MCP integration.".into(),
            eta_seconds: 5,
            percent: 88,
        });

        let mcp_install = match self.install_headroom_mcp() {
            Ok(method) => json!({
                "configured": true,
                "proxyUrl": HEADROOM_PROXY_URL,
                "installMethod": method.as_str(),
            }),
            Err(err) => {
                log::info!("headroom MCP setup skipped during repair: {err:#}");
                json!({ "configured": false, "proxyUrl": HEADROOM_PROXY_URL, "error": err.to_string() })
            }
        };

        self.update_headroom_receipt_after_requirements_repair(
            requirements_lock_sha(requirements_lock),
            mcp_install,
        )?;

        progress(BootstrapStepUpdate {
            step: "Repair complete",
            message: "Headroom dependency repair finished.".into(),
            eta_seconds: 0,
            percent: 95,
        });

        Ok(())
    }

    pub fn bootstrap_all_with_progress<F>(&self, mut progress: F) -> Result<ManagedRuntime>
    where
        F: FnMut(BootstrapStepUpdate),
    {
        // Milestone logs mirror the UI progress steps into the file log. The
        // bootstrap is otherwise near-silent on disk (only the codesign line),
        // so a stuck/slow first install is hard to diagnose from logs alone.
        log::info!("bootstrap: starting managed runtime install");
        progress(BootstrapStepUpdate {
            step: "Preparing install",
            message: "Setting up managed directories.".into(),
            eta_seconds: 3,
            percent: 5,
        });
        self.runtime.ensure_layout()?;

        if !self.runtime.standalone_python().exists() {
            log::info!("bootstrap: downloading standalone Python runtime");
            progress(BootstrapStepUpdate {
                step: "Downloading Python",
                message: "Fetching pinned standalone Python runtime.".into(),
                eta_seconds: 75,
                percent: 18,
            });
            self.install_python_distribution(|update| progress(update))?;
        } else {
            progress(BootstrapStepUpdate {
                step: "Python runtime",
                message: "Pinned Python runtime already available locally.".into(),
                eta_seconds: 3,
                percent: 18,
            });
        }

        if !self.managed_venv_has_pip() {
            log::info!("bootstrap: creating managed virtualenv");
            progress(BootstrapStepUpdate {
                step: "Creating environment",
                message: "Creating isolated Headroom virtual environment.".into(),
                eta_seconds: 25,
                percent: 35,
            });
            self.create_managed_venv()?;
        } else {
            progress(BootstrapStepUpdate {
                step: "Environment",
                message: "Isolated runtime already present.".into(),
                eta_seconds: 3,
                percent: 35,
            });
        }

        log::info!("bootstrap: installing Headroom + dependencies via pip");
        progress(BootstrapStepUpdate {
            step: "Installing Headroom",
            message: "Installing Headroom and required dependencies.".into(),
            eta_seconds: 95,
            percent: 58,
        });
        // Forward pip's per-package chatter instead of dropping it. Without
        // this the whole multi-minute dependency install sits on the single
        // static frame above, which reads as a hang once its ETA lapses.
        // Clamped so the sub-step percents (which start at 40 for the
        // standalone upgrade path) never walk the bar backwards from 58.
        self.install_headroom(|update| {
            progress(BootstrapStepUpdate {
                percent: update.percent.clamp(58, 89),
                ..update
            })
        })?;

        // RTK is opt-in: bootstrap no longer installs it. Users add it from the
        // Addons tab, which calls install_addon("rtk").
        progress(BootstrapStepUpdate {
            step: "Finalizing",
            message: "Writing managed runtime receipts and completion markers.".into(),
            eta_seconds: 6,
            percent: 90,
        });
        self.write_ready_flag()?;
        self.write_bootstrap_receipt()?;
        log::info!("bootstrap: managed runtime install complete (ready flag written)");
        progress(BootstrapStepUpdate {
            step: "Install complete",
            message: "Headroom runtime installed successfully.".into(),
            eta_seconds: 0,
            percent: 100,
        });
        Ok(self.runtime.clone())
    }

    /// Download-only warm-up for the consented bootstrap: pulls the two big
    /// network artifacts (standalone Python tarball, pinned headroom wheel)
    /// into `downloads_dir` while the user is still in signup/client-setup.
    /// Nothing is extracted or installed and nothing outside `downloads_dir`
    /// changes — `python_runtime_installed()` and every other installed-gate
    /// stays false, so consent semantics remain with bootstrap. Destinations
    /// and sha checks are identical to bootstrap's own download calls, which
    /// therefore skip instantly when they find these files.
    /// ponytail: pip's dependency downloads (the ~60-90s "Updating
    /// dependencies" step) can't be prefetched without extracting Python
    /// first — revisit only if the remaining install time still hurts.
    pub fn prefetch_bootstrap_artifacts(&self) -> Result<()> {
        self.runtime.ensure_layout()?;
        if !self.runtime.standalone_python().exists() {
            let artifact = python_distribution_artifact()?;
            let archive_path = self.runtime.downloads_dir.join("python-standalone.tar.gz");
            download_to_path(&artifact.url, &archive_path, artifact.sha256)?;
        }
        if !self.runtime.managed_python().exists() {
            let release = pinned_headroom_release()?;
            download_to_path(
                &release.wheel_url,
                &self.wheel_download_path(&release.wheel_url),
                Some(&release.sha256),
            )?;
        }
        Ok(())
    }

    /// Shared by the prefetch and the install/upgrade paths so a prefetched
    /// wheel always lands exactly where the installer looks for it. Keeps
    /// PyPI's own filename (platform tags and all) so pip's "not a supported
    /// wheel on this platform" check still backstops a mis-picked wheel — a
    /// `py3-none-any` rename made pip install a macOS wheel on Windows.
    fn wheel_download_path(&self, wheel_url: &str) -> PathBuf {
        self.runtime.downloads_dir.join(
            wheel_url
                .rsplit('/')
                .next()
                .filter(|name| name.ends_with(".whl"))
                .unwrap_or("headroom_ai.whl"),
        )
    }

    /// Run a directory operation that Windows can transiently deny.
    ///
    /// `DeleteFile`/`RemoveDirectory` only MARK a name for deletion: it stays
    /// on disk until the last handle closes, and a rename onto it meanwhile
    /// returns ACCESS_DENIED. Defender opens every file we just unpacked for
    /// the same second or two. RUST-8K is that race hitting `rename` on a
    /// fresh 0.8.6 install (os error 5, localized so the text is not even
    /// greppable) -- and it dead-ends the bootstrap, so the user never gets a
    /// runtime at all.
    ///
    /// Retries every error, not a classified subset: on a genuinely broken
    /// install this costs ~2.5s on a path that already failed, and the alt is
    /// a per-platform errno table (5/32/33 mean something else entirely on
    /// Unix) guarding a branch no test on a Mac can reach.
    /// ponytail: linear backoff, no jitter -- one process, no contention.
    fn retry_fs<T>(what: &str, mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
        const ATTEMPTS: u32 = 5;
        let mut attempt = 1;
        loop {
            match op() {
                Ok(value) => return Ok(value),
                Err(err) if attempt < ATTEMPTS => {
                    // info, not warn: a retry that then succeeds is a
                    // non-event, and warn bridges to Sentry. The caller
                    // reports the final failure as bootstrap_failed.
                    log::info!("{what} failed ({err}), retry {attempt}/{ATTEMPTS}");
                    std::thread::sleep(std::time::Duration::from_millis(250 * u64::from(attempt)));
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn install_python_distribution<F>(&self, mut emit_step: F) -> Result<()>
    where
        F: FnMut(BootstrapStepUpdate),
    {
        let archive_path = self.runtime.downloads_dir.join("python-standalone.tar.gz");
        let artifact = python_distribution_artifact()?;
        // Sub-progress maps the download to bootstrap percents 18..=34 (next
        // step starts at 35). Keeps the progress bar moving on slow networks
        // so users don't assume the app has frozen.
        let started_at = Instant::now();
        download_to_path_with_progress(
            &artifact.url,
            &archive_path,
            artifact.sha256,
            |downloaded, total| {
                let downloaded_mb = downloaded as f64 / 1_048_576.0;
                let (message, percent, eta_seconds) = match total {
                    Some(total) if total > 0 => {
                        let total_mb = total as f64 / 1_048_576.0;
                        let frac = (downloaded as f64 / total as f64).clamp(0.0, 1.0);
                        let percent = (18.0 + frac * 16.0).round().clamp(18.0, 34.0) as u8;
                        let elapsed = started_at.elapsed().as_secs_f64().max(0.1);
                        let rate = downloaded as f64 / elapsed;
                        let remaining = (total.saturating_sub(downloaded)) as f64;
                        let eta = if rate > 1.0 {
                            (remaining / rate).ceil() as u64
                        } else {
                            75
                        };
                        (
                            format!(
                                "Downloading Python runtime: {:.1} / {:.1} MB",
                                downloaded_mb, total_mb
                            ),
                            percent,
                            eta,
                        )
                    }
                    _ => (
                        format!("Downloading Python runtime: {:.1} MB", downloaded_mb),
                        18,
                        75,
                    ),
                };
                emit_step(BootstrapStepUpdate {
                    step: "Downloading Python",
                    message,
                    eta_seconds,
                    percent,
                });
            },
        )?;

        let file = std::fs::File::open(&archive_path)
            .with_context(|| format!("opening {}", archive_path.display()))?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);
        // Extract to a staging dir and rename into place. Unpacking straight
        // into runtime_dir used to be a permanent trap: a crash mid-unpack
        // after bin/python3 landed made every later launch's `exists()` gate
        // read "installed" while the stdlib was incomplete — and no repair
        // path covers runtime/python, so it stayed broken until manual
        // deletion.
        let staging_dir = self.runtime.runtime_dir.join("python.extracting");
        if staging_dir.exists() {
            Self::retry_fs("clearing stale staging dir", || {
                std::fs::remove_dir_all(&staging_dir)
            })
            .with_context(|| format!("clearing stale {}", staging_dir.display()))?;
        }
        std::fs::create_dir_all(&staging_dir)
            .with_context(|| format!("creating {}", staging_dir.display()))?;
        archive
            .unpack(&staging_dir)
            .with_context(|| format!("extracting into {}", staging_dir.display()))?;

        // The tarball's single root is `python/`. On Windows the interpreter is
        // `python.exe` at the root; on Unix it is `bin/python3`.
        let extracted_root = staging_dir.join("python");
        let expected_python = if cfg!(target_os = "windows") {
            extracted_root.join("python.exe")
        } else {
            extracted_root.join("bin").join("python3")
        };
        if !expected_python.exists() {
            bail!(
                "standalone python extraction completed but {} was not found",
                expected_python.display()
            );
        }
        if self.runtime.python_dir.exists() {
            Self::retry_fs("removing partial python dir", || {
                std::fs::remove_dir_all(&self.runtime.python_dir)
            })
            .with_context(|| format!("removing partial {}", self.runtime.python_dir.display()))?;
        }
        Self::retry_fs("publishing extracted python", || {
            std::fs::rename(&extracted_root, &self.runtime.python_dir)
        })
        .with_context(|| {
            format!(
                "publishing extracted python into {}",
                self.runtime.python_dir.display()
            )
        })?;
        let _ = std::fs::remove_dir_all(&staging_dir);

        if !self.runtime.standalone_python().exists() {
            bail!(
                "standalone python extraction completed but {} was not found",
                self.runtime.standalone_python().display()
            );
        }

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            let python = self.runtime.standalone_python();
            if let Ok(metadata) = std::fs::metadata(&python) {
                let mut perms = metadata.permissions();
                if perms.mode() & 0o111 == 0 {
                    perms.set_mode(0o755);
                    let _ = std::fs::set_permissions(&python, perms);
                }
            }
        }

        // Strip the quarantine attribute from the extracted runtime so macOS
        // Gatekeeper doesn't scan it on first execution (which can hang the
        // machine for 20-30 seconds).
        #[cfg(target_os = "macos")]
        {
            let _ = crate::proc::command("xattr")
                .args([
                    "-rd",
                    "com.apple.quarantine",
                    self.runtime.runtime_dir.to_string_lossy().as_ref(),
                ])
                .output();
        }

        Ok(())
    }

    /// `python -m venv` copies the interpreter into place *before* it runs
    /// ensurepip, so an attempt killed mid-way (quit, reboot, AV quarantine)
    /// leaves a `python` with no pip. Gating bootstrap on the interpreter alone
    /// made that state permanent: `create_managed_venv` — which carries the pip
    /// verification — was skipped on every later launch and the install died
    /// with "No module named pip" forever. Re-running venv creation re-runs
    /// ensurepip, so treating a pip-less venv as absent self-heals it.
    fn managed_venv_has_pip(&self) -> bool {
        self.runtime.managed_python().exists()
            && run_python_command(
                &self.runtime.managed_python(),
                &["-m", "pip", "--version"],
                &self.runtime.root_dir,
            )
            .is_ok()
    }

    /// Creates the venv, then installs pip into it as a separate step.
    ///
    /// `python -m venv` runs ensurepip through `subprocess.check_output` and
    /// re-raises only the exit status, so the child's own stderr is captured
    /// into an exception venv never prints. A failure there reached us as the
    /// bare string "Command '[... -m ensurepip ...]' returned non-zero exit
    /// status 1" -- no cause, no keyword for `pip_failure_category` to match,
    /// so it landed in the `other` grab-bag (Sentry RUST-82, an install-blocking
    /// dead end on 0.8.4 with nothing in it to act on).
    ///
    /// Running the two steps ourselves is what venv does internally, but now
    /// ensurepip's stderr comes back as a first-class `CommandFailure` that the
    /// categoriser can bucket. `build_command` already clears
    /// PYTHONHOME/PYTHONPATH and sets PYTHONNOUSERSITE, which is the isolation
    /// venv applies to this call, so the explicit invocation is equivalent.
    ///
    /// A venv left pip-less by a failure here is not a dead end: it fails
    /// `managed_venv_has_pip`, so the next launch treats it as absent and
    /// re-runs this whole function.
    fn create_managed_venv(&self) -> Result<()> {
        run_python_command(
            &self.runtime.standalone_python(),
            &[
                "-m",
                "venv",
                "--without-pip",
                self.runtime.venv_dir.to_string_lossy().as_ref(),
            ],
            &self.runtime.root_dir,
        )
        .context("creating Headroom-managed virtualenv")?;

        run_python_command(
            &self.runtime.managed_python(),
            &["-m", "ensurepip", "--upgrade", "--default-pip"],
            &self.runtime.root_dir,
        )
        .context("installing pip into the Headroom-managed virtualenv")?;

        run_python_command(
            &self.runtime.managed_python(),
            &["-m", "pip", "--version"],
            &self.runtime.root_dir,
        )
        .context("verifying Headroom-managed pip is available")?;

        Ok(())
    }

    /// Bootstrap path: installs the pinned headroom release.
    fn install_headroom<F>(&self, progress: F) -> Result<()>
    where
        F: FnMut(BootstrapStepUpdate),
    {
        // Bootstrap path runs at first launch where there is no boot
        // validation yet — no caller will read the captured pip output, so
        // skip the buffer to avoid allocating it.
        self.install_headroom_release(&pinned_headroom_release()?, progress, None)
    }

    fn install_headroom_release<F>(
        &self,
        release: &HeadroomRelease,
        mut progress: F,
        pip_capture: Option<&std::cell::RefCell<PipOutputCapture>>,
    ) -> Result<()>
    where
        F: FnMut(BootstrapStepUpdate),
    {
        let requirements_lock = bootstrap_requirements_lock();
        let lock_path = self.write_headroom_requirements_lock(requirements_lock)?;
        let wheel_path = self.wheel_download_path(&release.wheel_url);

        progress(BootstrapStepUpdate {
            step: "Downloading update",
            message: "Fetching Headroom update bundle.".into(),
            eta_seconds: 15,
            percent: 40,
        });

        // Try direct wheel download (with retries). If the transfer fails,
        // fall back to the PyPI index; a checksum mismatch fails hard instead
        // — the fallback has no hash verification, so downgrading on mismatch
        // would bypass the integrity check exactly when it fires.
        let use_wheel =
            match download_to_path(&release.wheel_url, &wheel_path, Some(&release.sha256)) {
                Ok(()) => true,
                Err(download_err) if is_checksum_mismatch(&download_err) => {
                    return Err(download_err.context(
                        "Headroom wheel failed checksum verification; refusing unverified fallback",
                    ));
                }
                Err(download_err) => {
                    log::warn!(
                    "headroom wheel download failed (will fall back to pip index): {download_err}"
                );
                    false
                }
            };

        progress(BootstrapStepUpdate {
            step: "Updating dependencies",
            message: "Updating Headroom's bundled dependencies.".into(),
            eta_seconds: 90,
            percent: 55,
        });

        // Stream pip's stdout/stderr and translate noteworthy lines into
        // user-facing step updates so the progress UI actually changes
        // during the ~60-90s dependency install instead of staring at a
        // single "Updating dependencies" frame. Also funnel each line into
        // the diagnostic capture so a later boot-validation failure can
        // forensic the pip run that produced the broken venv.
        let deps_start = std::time::Instant::now();
        let deps_progress_ref = std::cell::RefCell::new(&mut progress);
        let mut dep_counter: u32 = 0;
        run_pip_install_with_retries_streaming(
            &self.runtime.managed_python(),
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                PIP_ONLY_BINARY,
                "--find-links",
                VENDOR_WHEELS_INDEX_URL,
                "--extra-index-url",
                "https://pypi.org/simple",
                "--upgrade",
                "--requirement",
                lock_path.to_string_lossy().as_ref(),
            ],
            &self.runtime.root_dir,
            |line| {
                if let Some(cap) = pip_capture {
                    cap.borrow_mut().push(line);
                }
                if let Some(update) =
                    pip_line_to_progress(line, deps_start.elapsed(), &mut dep_counter, 55, 80)
                {
                    if let Ok(mut cb) = deps_progress_ref.try_borrow_mut() {
                        (cb)(update);
                    }
                }
            },
        )
        .context("installing locked Headroom dependencies into Headroom-managed virtualenv")?;

        progress(BootstrapStepUpdate {
            step: "Applying update",
            message: "Applying the Headroom update.".into(),
            eta_seconds: 15,
            percent: 80,
        });

        let headroom_spec = format!("headroom-ai=={}", release.version);
        let headroom_arg = if use_wheel {
            wheel_path.to_string_lossy().into_owned()
        } else {
            headroom_spec.clone()
        };
        run_pip_install_with_retries_streaming(
            &self.runtime.managed_python(),
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                PIP_ONLY_BINARY,
                "--extra-index-url",
                "https://pypi.org/simple",
                "--no-deps",
                &headroom_arg,
            ],
            &self.runtime.root_dir,
            |line| {
                if let Some(cap) = pip_capture {
                    cap.borrow_mut().push(line);
                }
            },
        )
        .with_context(|| {
            if use_wheel {
                "installing verified Headroom wheel into Headroom-managed virtualenv".into()
            } else {
                format!("installing {headroom_spec} from PyPI into Headroom-managed virtualenv")
            }
        })?;

        // Ad-hoc sign every native extension pip just dropped into the venv.
        // PyPI wheels are unsigned; some EDR tooling stalls or blocks on
        // first-execution of unsigned binaries. Best-effort — failures are
        // logged and ignored, the smoke test downstream is the real gate.
        self.ad_hoc_sign_venv_natives();

        progress(BootstrapStepUpdate {
            step: "Configuring integrations",
            message: "Setting up Headroom MCP integration.".into(),
            eta_seconds: 5,
            percent: 90,
        });

        let mcp_install = match self.install_headroom_mcp() {
            Ok(method) => json!({
                "configured": true,
                "proxyUrl": HEADROOM_PROXY_URL,
                "installMethod": method.as_str(),
            }),
            Err(err) => {
                log::info!("headroom MCP setup skipped: {err:#}");
                json!({
                    "configured": false,
                    "proxyUrl": HEADROOM_PROXY_URL,
                    "error": err.to_string()
                })
            }
        };

        self.write_tool_receipt(
            "headroom",
            json!({
                "status": "healthy",
                "installedBy": "Headroom",
                "scope": "self-contained",
                "runtime": "python",
                "pythonExecutable": self.runtime.managed_python(),
                "pipExecutable": self.runtime.managed_pip(),
                "entrypoint": self.runtime.venv_dir.join("bin").join("headroom"),
                "source": self.manifests[0].source_url,
                "version": release.version,
                "artifact": {
                    "url": release.wheel_url,
                    "sha256": release.sha256,
                    "requirementsLockSha256": requirements_lock_sha(requirements_lock)
                },
                "mcp": mcp_install,
                "ml": {
                    "installed": true,
                    "engine": "kompress"
                }
            }),
        )
    }

    fn update_headroom_receipt_after_requirements_repair(
        &self,
        requirements_lock_sha256: String,
        mcp_install: Value,
    ) -> Result<()> {
        let receipt_path = self.runtime.tools_dir.join("headroom.json");
        if let Ok(bytes) = std::fs::read(&receipt_path) {
            if let Ok(mut receipt) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(artifact) = receipt.get_mut("artifact").and_then(|a| a.as_object_mut())
                {
                    artifact.insert(
                        "requirementsLockSha256".into(),
                        json!(requirements_lock_sha256),
                    );
                }
                receipt["mcp"] = mcp_install;
                crate::client_adapters::atomic_write(&receipt_path, &serde_json::to_vec(&receipt)?)
                    .with_context(|| format!("writing {}", receipt_path.display()))?;
            }
        }
        Ok(())
    }

    /// Cheap post-install sanity check: can the new venv import the top-level
    /// headroom package and its proxy entrypoint? Catches import errors, syntax
    /// errors, and missing transitive dependencies introduced by a new version
    /// before we try to actually boot the proxy.
    ///
    /// If the failure is a pydantic / pydantic-core skew (pip's `--upgrade -r
    /// lock` left the two out of sync), reinstall pydantic-core at the version
    /// pydantic asks for and retry the smoke test once. Mirrors the
    /// proxy-startup repair in `start_headroom_proxy_with_repair` so the same
    /// recoverable failure doesn't fail an in-flight upgrade and force a
    /// rollback.
    pub fn smoke_test_headroom(&self) -> Result<()> {
        match self.smoke_test_headroom_with_timeout(HEADROOM_SMOKE_TEST_TIMEOUT) {
            Ok(()) => Ok(()),
            Err(err) => {
                let target = err
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<CommandFailure>())
                    .and_then(|f| extract_required_pydantic_core_version(&f.stderr));
                let Some(target) = target else {
                    return Err(err);
                };
                log::warn!(
                    "smoke test failed with pydantic-core/pydantic skew; \
                     reinstalling pydantic-core=={target} and retrying"
                );
                if let Err(repair_err) = self.repair_pydantic_core(&target) {
                    log::error!("pydantic-core repair failed: {repair_err:#}");
                    return Err(err);
                }
                self.smoke_test_headroom_with_timeout(HEADROOM_SMOKE_TEST_TIMEOUT)
            }
        }
    }

    fn smoke_test_headroom_with_timeout(&self, timeout: Duration) -> Result<()> {
        let python = self.runtime.managed_python();
        if let Err(err) = run_command_with_timeout(
            &python,
            &["-c", "import headroom; import headroom.proxy.server"],
            &self.runtime.root_dir,
            timeout,
        )
        .with_context(|| format!("running smoke test with {}", python.display()))
        {
            return Err(anyhow::Error::new(CommandFailure {
                program: python.display().to_string(),
                args: vec![
                    "-c".into(),
                    "import headroom; import headroom.proxy.server".into(),
                ],
                stdout: err
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<CommandFailure>())
                    .map(|failure| failure.stdout.clone())
                    .unwrap_or_default(),
                stderr: err
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<CommandFailure>())
                    .map(|failure| failure.stderr.clone())
                    .unwrap_or_else(|| format!("{err:#}")),
                exit_code: err
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<CommandFailure>())
                    .and_then(|failure| failure.exit_code),
                signal: err
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<CommandFailure>())
                    .and_then(|failure| failure.signal),
            }))
            .context("Headroom smoke test failed — the new version cannot be imported");
        }
        Ok(())
    }

    /// Verifies the managed `markitdown` console script actually executes (its
    /// base converters and their dependencies import). No-op when the addon
    /// isn't installed, so it can be called unconditionally from a smoke pass.
    pub fn smoke_test_markitdown(&self) -> Result<()> {
        // First execution after an upgrade pays Gatekeeper/EDR scanning of the
        // freshly-written venv plus cold imports, which can blow the 60s
        // timeout (RUST-22). A retry runs with warm caches; only a repeat
        // failure is a real signal.
        if self
            .smoke_test_markitdown_with_timeout(MARKITDOWN_SMOKE_TEST_TIMEOUT)
            .is_ok()
        {
            return Ok(());
        }
        self.smoke_test_markitdown_with_timeout(MARKITDOWN_SMOKE_TEST_TIMEOUT)
    }

    fn smoke_test_markitdown_with_timeout(&self, timeout: Duration) -> Result<()> {
        if !self.markitdown_installed() {
            return Ok(());
        }
        let bin = self.markitdown_entrypoint();
        run_command_with_timeout(&bin, &["--help"], &self.runtime.root_dir, timeout)
            .with_context(|| format!("running markitdown smoke test with {}", bin.display()))?;
        Ok(())
    }

    /// Plugin addons are host plugins, not binaries we own, so "smoke test"
    /// means confirming the plugin is still registered with a host's plugin
    /// registry. No-op when our receipt says it was never installed, or when
    /// it says the user disabled it. A disabled Codex plugin intentionally
    /// remains registered with `enabled = false`, so it can be re-enabled
    /// without reinstalling its marketplace checkout.
    pub fn smoke_test_plugin(&self, id: &str) -> Result<()> {
        let plugin = plugin_addon(id).with_context(|| format!("unknown plugin addon: {id}"))?;
        let Some(receipt) = self.read_plugin_receipt(plugin)? else {
            let registered = plugin
                .hosts
                .iter()
                .map(|host| host.plugin_present_checked(plugin))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .any(|present| present);
            let runtime_residue = match plugin.id {
                "allinluna" => self.allinluna_runtime_path().is_some(),
                "openspec" => self.openspec_entrypoint().exists(),
                "ralph-loop" => self.ralph_entrypoint().exists(),
                _ => false,
            };
            if registered || runtime_residue {
                bail!("{id} partial/orphan install: host registration or managed runtime exists without receipt");
            }
            return Ok(());
        };
        let enabled = receipt
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !enabled {
            return Ok(());
        }
        let registered = plugin
            .hosts
            .iter()
            .map(|host| host.plugin_present_checked(plugin))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .any(|present| present);
        if !registered {
            // The plugin was removed behind our back (host-native `/plugin`
            // uninstall or a host registry migration). Drop the stale receipt
            // so this warns once instead of on every future upgrade;
            // `detect_status` already reports NotInstalled for this state.
            let receipt_path = self.runtime.tools_dir.join(format!("{}.json", plugin.id));
            let removed = std::fs::remove_file(&receipt_path).is_ok();
            bail!(
                "{id} receipt exists but the plugin is no longer registered \
                 with any host{}",
                if removed {
                    " (stale receipt removed)"
                } else {
                    " (stale receipt could not be removed)"
                }
            );
        }
        if !self.plugin_runtime_healthy(plugin) && plugin.id == "allinluna" {
            self.ensure_plugin_runtime(plugin)?;
        }
        if !self.plugin_runtime_healthy(plugin) {
            bail!("{id} is registered but its managed runtime is incomplete");
        }
        Ok(())
    }

    /// Apply an ad-hoc codesign signature to every native extension (.so /
    /// .dylib) under the venv's site-packages. PyPI wheels arrive unsigned,
    /// and some endpoint protection (EDR) tooling either blocks unsigned
    /// freshly-extracted binaries outright or makes them slower to load on
    /// first execution. An ad-hoc signature (`codesign --force --sign -`)
    /// satisfies macOS Gatekeeper's "signed" check and clears at least one
    /// class of EDR heuristic without us shipping a Developer ID at runtime.
    ///
    /// Best-effort: failures are logged and ignored. The install must not
    /// fail because codesign couldn't sign one file — the smoke test that
    /// follows is the real gate.
    fn ad_hoc_sign_venv_natives(&self) -> usize {
        if !cfg!(target_os = "macos") {
            return 0;
        }
        let site_packages = self
            .runtime
            .venv_dir
            .join("lib")
            .join("python3.12")
            .join("site-packages");
        if !site_packages.exists() {
            return 0;
        }
        let mut native_paths: Vec<PathBuf> = Vec::new();
        if let Err(err) = collect_native_extensions(&site_packages, &mut native_paths) {
            log::warn!(
                "ad-hoc codesign skipped: failed to walk {}: {err:#}",
                site_packages.display()
            );
            return 0;
        }
        if native_paths.is_empty() {
            return 0;
        }
        let total = native_paths.len();
        // One codesign invocation can accept many file arguments; ARG_MAX
        // (~256KB on macOS) is well above what we'd hit even with 1000+
        // long paths, so we avoid the per-file fork-exec overhead.
        let output = crate::proc::command("codesign")
            .args(["--force", "--sign", "-"])
            .args(&native_paths)
            .output();
        match output {
            Ok(out) if out.status.success() => {
                log::info!("ad-hoc codesign signed {total} venv native extensions");
                total
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                log::warn!(
                    "ad-hoc codesign exited {:?} for {total} files: {}",
                    out.status.code(),
                    stderr.trim()
                );
                0
            }
            Err(err) => {
                log::warn!("ad-hoc codesign failed to spawn: {err:#}");
                0
            }
        }
    }

    fn venv_backup_dir(&self) -> PathBuf {
        let mut dir = self.runtime.venv_dir.clone();
        let file_name = format!(
            "{}.backup",
            dir.file_name().and_then(|n| n.to_str()).unwrap_or("venv")
        );
        dir.set_file_name(file_name);
        dir
    }

    fn headroom_receipt_path(&self) -> PathBuf {
        self.runtime.tools_dir.join("headroom.json")
    }

    fn headroom_receipt_backup_path(&self) -> PathBuf {
        self.runtime.tools_dir.join("headroom.json.backup")
    }

    fn upgrade_marker_path(&self) -> PathBuf {
        self.runtime.runtime_dir.join("upgrade.in_progress.json")
    }

    fn write_upgrade_marker(
        &self,
        target_version: &str,
        in_place_previous_version: Option<&str>,
        in_place_previous_lock_backup: Option<&Path>,
    ) -> Result<()> {
        let marker = self.upgrade_marker_path();
        let mut body = json!({
            "target_version": target_version,
            "started_at": Utc::now().to_rfc3339(),
        });
        if let Some(previous) = in_place_previous_version {
            body["in_place"] = json!(true);
            body["previous_version"] = json!(previous);
        }
        if let Some(backup) = in_place_previous_lock_backup {
            body["previous_lock_backup"] = json!(backup);
        }
        crate::client_adapters::atomic_write(&marker, &serde_json::to_vec_pretty(&body)?)
            .with_context(|| format!("writing {}", marker.display()))?;
        Ok(())
    }

    /// Strictly parse the in-progress upgrade marker and, if it records an
    /// in-place upgrade, return (previous_version, target_version,
    /// previous_lock_backup). A malformed marker is an unresolved recovery
    /// boundary, so callers must preserve it instead of falling through to the
    /// full-rebuild path and clearing it.
    fn read_in_place_marker_checked(&self) -> Result<Option<(String, String, Option<PathBuf>)>> {
        let path = self.upgrade_marker_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let body: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing upgrade marker {}", path.display()))?;
        let object = body
            .as_object()
            .ok_or_else(|| anyhow!("upgrade marker {} must be a JSON object", path.display()))?;
        let Some(in_place) = object.get("in_place") else {
            return Ok(None);
        };
        if !in_place.is_boolean() {
            bail!(
                "upgrade marker {} has a non-boolean in_place field",
                path.display()
            );
        }
        if !in_place.as_bool().unwrap_or(false) {
            return Ok(None);
        }
        let previous = object
            .get("previous_version")
            .and_then(Value::as_str)
            .filter(|version| !version.trim().is_empty())
            .ok_or_else(|| anyhow!("upgrade marker {} has no previous_version", path.display()))?
            .to_string();
        let target = object
            .get("target_version")
            .and_then(Value::as_str)
            .filter(|version| !version.trim().is_empty())
            .ok_or_else(|| anyhow!("upgrade marker {} has no target_version", path.display()))?
            .to_string();
        let lock_backup = match object.get("previous_lock_backup") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .filter(|path| !path.trim().is_empty())
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        anyhow!(
                            "upgrade marker {} has an invalid previous_lock_backup",
                            path.display()
                        )
                    })?,
            ),
        };
        Ok(Some((previous, target, lock_backup)))
    }

    /// Compatibility wrapper retained for focused tests and non-recovery
    /// callers. It logs malformed markers and returns `None`; recovery entry
    /// points use `read_in_place_marker_checked` so they can stop without
    /// clearing an unresolved marker.
    fn read_in_place_marker(&self) -> Option<(String, String, Option<PathBuf>)> {
        match self.read_in_place_marker_checked() {
            Ok(marker) => marker,
            Err(err) => {
                log::warn!("read_in_place_marker: malformed marker preserved for retry: {err:#}");
                None
            }
        }
    }

    fn clear_upgrade_marker(&self) {
        let _ = std::fs::remove_file(self.upgrade_marker_path());
    }

    /// Inspect disk state for the signature of an interrupted previous upgrade
    /// and restore the backup venv as the live venv if so.
    ///
    /// Interrupted = upgrade marker file present. The backup venv is treated
    /// as the canonical "old, working" one; the live venv (if any) is whatever
    /// partial state was left behind. Safe to call at every upgrade entry.
    ///
    /// Returns true if recovery was performed.
    pub fn recover_from_interrupted_upgrade(&self) -> bool {
        let marker = self.upgrade_marker_path();
        if !marker.exists() {
            return false;
        }

        // In-place recovery: pip install is atomic per-package, so the venv
        // may be in a mixed state (some packages at target pins, others at
        // previous pins). Restore deps from the lock snapshot (if the
        // interrupted upgrade took that path) and force-reinstall the prior
        // headroom-ai so the next launch starts from a known-good state.
        // `check_headroom_upgrade` will then retry the swap fresh.
        let in_place_marker = match self.read_in_place_marker_checked() {
            Ok(marker) => marker,
            Err(err) => {
                log::error!(
                    "recover_from_interrupted_upgrade: malformed marker preserved for manual/retry recovery: {err:#}"
                );
                return false;
            }
        };
        if let Some((previous_version, _target, previous_lock_backup)) = in_place_marker {
            log::info!(
                "recover_from_interrupted_upgrade: in-place upgrade was in progress; \
                 reinstalling previous headroom-ai {previous_version}"
            );
            // Both pip helpers need PyPI. Offline (laptop died mid-upgrade,
            // reopened on a plane) they fail — and discarding those failures
            // used to clear the marker anyway, leaving a mixed venv (new dep
            // pins, unknown headroom-ai version) that the restored receipt
            // declared healthy. On failure keep the marker AND the lock
            // backup untouched so the next launch retries recovery; mirror
            // `rollback_headroom_upgrade`, which propagates the same errors.
            if let Some(ref backup) = previous_lock_backup {
                if let Err(err) = self.pip_restore_deps_from_backup(backup) {
                    log::warn!(
                        "recover_from_interrupted_upgrade: dep restore failed ({err:#}); \
                         keeping upgrade marker for retry"
                    );
                    return false;
                }
            }
            if let Err(err) = self.pip_force_reinstall_headroom_version(&previous_version) {
                log::warn!(
                    "recover_from_interrupted_upgrade: reinstalling headroom-ai \
                     {previous_version} failed ({err:#}); keeping upgrade marker for retry"
                );
                return false;
            }
            if let Some(ref backup) = previous_lock_backup {
                if let Err(err) = std::fs::copy(backup, self.active_lock_path()) {
                    log::error!(
                        "recover_from_interrupted_upgrade: restoring lock snapshot {} failed: {err}",
                        backup.display()
                    );
                    return false;
                }
                if let Err(err) = std::fs::remove_file(backup) {
                    log::error!(
                        "recover_from_interrupted_upgrade: removing consumed lock snapshot {} failed: {err}",
                        backup.display()
                    );
                    return false;
                }
            }
            let receipt_backup = self.headroom_receipt_backup_path();
            let receipt_path = self.headroom_receipt_path();
            if receipt_backup.exists() {
                if let Err(err) = std::fs::copy(&receipt_backup, &receipt_path) {
                    log::error!(
                        "recover_from_interrupted_upgrade: restoring receipt {} failed: {err}",
                        receipt_backup.display()
                    );
                    return false;
                }
                if let Err(err) = std::fs::remove_file(&receipt_backup) {
                    log::error!(
                        "recover_from_interrupted_upgrade: removing consumed receipt backup {} failed: {err}",
                        receipt_backup.display()
                    );
                    return false;
                }
            }
            self.clear_upgrade_marker();
            return true;
        }

        let backup_dir = self.venv_backup_dir();
        let venv_dir = &self.runtime.venv_dir;
        let receipt_backup = self.headroom_receipt_backup_path();
        let receipt_path = self.headroom_receipt_path();

        log::info!(
            "recover_from_interrupted_upgrade: found stale marker at {}; restoring backup",
            marker.display()
        );

        if backup_dir.exists() {
            // The live venv (if present) is a partial/unknown new install.
            // Blow it away and put the backup back in its place.
            if venv_dir.exists() {
                if let Err(err) = std::fs::remove_dir_all(venv_dir) {
                    log::error!(
                        "recover_from_interrupted_upgrade: failed to remove partial venv at {}: {err}",
                        venv_dir.display()
                    );
                    // Leave everything in place; clearing the marker would be
                    // worse than leaving it for a later manual intervention.
                    return false;
                }
            }
            if let Err(err) = std::fs::rename(&backup_dir, venv_dir) {
                log::error!(
                    "recover_from_interrupted_upgrade: failed to restore venv from {}: {err}",
                    backup_dir.display()
                );
                return false;
            }
            if receipt_backup.exists() {
                if let Err(err) = std::fs::copy(&receipt_backup, &receipt_path) {
                    log::error!(
                        "recover_from_interrupted_upgrade: restoring receipt {} failed: {err}",
                        receipt_backup.display()
                    );
                    return false;
                }
                if let Err(err) = std::fs::remove_file(&receipt_backup) {
                    log::error!(
                        "recover_from_interrupted_upgrade: removing consumed receipt backup {} failed: {err}",
                        receipt_backup.display()
                    );
                    return false;
                }
            }
        } else {
            // No backup to restore from. Rare — the user (or a script) deleted
            // the backup dir while the marker was still live. Best we can do
            // is clear the marker so we don't loop on this state.
            log::warn!(
                "recover_from_interrupted_upgrade: no backup at {}; keeping marker for manual/retry recovery",
                backup_dir.display()
            );
            return false;
        }
        self.clear_upgrade_marker();
        true
    }

    /// Atomic runtime upgrade. Moves the current venv aside, creates a fresh
    /// venv at the original path, installs the new release, runs a smoke test.
    ///
    /// On success: returns `InstalledPendingValidation` — the backup is **still
    /// on disk** and the caller must call either [`commit_headroom_upgrade`] (if
    /// the new proxy boots) or [`rollback_headroom_upgrade`] (if it doesn't).
    ///
    /// On failure in any install step: rolls back internally, restoring the
    /// previous venv + receipt byte-for-byte, and returns `InstallFailed`.
    ///
    /// `force_rebuild` skips the in-place upgrade attempt and goes straight
    /// to the move-aside-and-rebuild path. Used by the user-facing "Retry
    /// with full rebuild" recovery flow when an in-place upgrade installed
    /// cleanly but boot validation failed (typically an ABI mismatch in
    /// native deps that pip can't detect).
    pub fn atomic_upgrade_headroom<F>(
        &self,
        release: &HeadroomRelease,
        mut progress: F,
        force_rebuild: bool,
    ) -> UpgradeOutcome
    where
        F: FnMut(BootstrapStepUpdate),
    {
        progress(BootstrapStepUpdate {
            step: "Preparing update",
            message: "Checking for previous upgrade state.".into(),
            eta_seconds: 2,
            percent: 5,
        });

        // Windows: anything still running from the venv (IDE-spawned MCP
        // servers, stray pythons) holds file locks that fail both the wheel
        // install and its rollback with permission errors (RUST-6Z/70).
        crate::state::kill_venv_lock_holders(&self.runtime.venv_dir);

        // If a prior upgrade was interrupted (process killed between
        // move-aside and success-commit), the backup is the REAL venv.
        // Restore it before doing anything destructive.
        let _recovered = self.recover_from_interrupted_upgrade();

        // In-place path: mutate the live venv rather than rebuilding it.
        // Covers both the wheel-only case (lock unchanged) and the lock-churn
        // case (`pip install --upgrade -r lock` reinstalls only the pins that
        // actually differ). Skipped when `force_rebuild` is set (user-
        // initiated recovery from a botched in-place upgrade) or when
        // `prepare_in_place_upgrade` decides the receipt isn't safe to
        // mutate in-place.
        if !force_rebuild {
            if let Some(ctx) = self.prepare_in_place_upgrade() {
                return self.in_place_upgrade_headroom(release, ctx, progress);
            }
        }

        let venv_dir = self.runtime.venv_dir.clone();
        let backup_dir = self.venv_backup_dir();
        let receipt_path = self.headroom_receipt_path();
        let receipt_backup = self.headroom_receipt_backup_path();

        // Best-effort: purge any leftover backup from a cleanly-completed
        // previous upgrade. recover_from_interrupted_upgrade above has
        // already handled any backup that belongs to an in-flight upgrade.
        if backup_dir.exists() {
            if let Err(err) = std::fs::remove_dir_all(&backup_dir) {
                return UpgradeOutcome::InstallFailed {
                    restored: false,
                    error: anyhow!(
                        "failed to remove stale venv backup at {}: {err}",
                        backup_dir.display()
                    ),
                };
            }
        }
        let _ = std::fs::remove_file(&receipt_backup);

        // Disk-space pre-check: building a fresh venv doubles space usage
        // momentarily. Refuse if less than 1 GB is free on the root volume.
        if let Some(avail) = available_disk_bytes(&self.runtime.root_dir) {
            const ONE_GB: u64 = 1_024 * 1_024 * 1_024;
            if avail < ONE_GB {
                return UpgradeOutcome::InstallFailed {
                    restored: false,
                    error: anyhow!(
                        "insufficient disk space for runtime upgrade: {} MB free, 1024 MB required",
                        avail / (1024 * 1024)
                    ),
                };
            }
        }

        // Move current venv + receipt aside. Write the in-progress marker
        // FIRST so that if we're killed between the rename and
        // commit/rollback, the next launch can recognize and recover.
        let had_live_venv = venv_dir.exists();
        if had_live_venv {
            if let Err(err) = self.write_upgrade_marker(&release.version, None, None) {
                return UpgradeOutcome::InstallFailed {
                    restored: false,
                    error: err.context("writing upgrade-in-progress marker"),
                };
            }
            if let Err(err) = std::fs::rename(&venv_dir, &backup_dir) {
                self.clear_upgrade_marker();
                return UpgradeOutcome::InstallFailed {
                    restored: false,
                    error: anyhow!("failed to move {} aside: {err}", venv_dir.display()),
                };
            }
        }
        let had_receipt = receipt_path.exists();
        if had_receipt {
            if let Err(err) = std::fs::copy(&receipt_path, &receipt_backup) {
                let restored = self.restore_venv_from_backup(had_live_venv);
                return UpgradeOutcome::InstallFailed {
                    restored,
                    error: anyhow!("failed to snapshot {}: {err}", receipt_path.display()),
                };
            }
        }

        progress(BootstrapStepUpdate {
            step: "Creating environment",
            message: "Creating isolated Headroom virtual environment.".into(),
            eta_seconds: 20,
            percent: 15,
        });

        if let Err(err) = self.create_managed_venv() {
            let restored = self.rollback_partial_upgrade(had_live_venv, had_receipt);
            return UpgradeOutcome::InstallFailed {
                restored,
                error: err.context("creating replacement Headroom virtualenv"),
            };
        }

        // install_headroom_release emits its own granular progress from ~40-90%.
        let pip_capture = std::cell::RefCell::new(PipOutputCapture::new(100));
        if let Err(err) = self.install_headroom_release(release, &mut progress, Some(&pip_capture))
        {
            let restored = self.rollback_partial_upgrade(had_live_venv, had_receipt);
            return UpgradeOutcome::InstallFailed {
                restored,
                error: err,
            };
        }

        progress(BootstrapStepUpdate {
            step: "Verifying install",
            message: "Running Headroom import smoke test.".into(),
            eta_seconds: 3,
            percent: 95,
        });

        if let Err(err) = self.smoke_test_headroom() {
            let restored = self.rollback_partial_upgrade(had_live_venv, had_receipt);
            return UpgradeOutcome::InstallFailed {
                restored,
                error: err,
            };
        }

        // Re-stamp the READY flag on the fresh venv. Without this,
        // `python_runtime_installed()` returns false (the flag lives inside
        // venv_dir, which was replaced during the swap), which would make
        // `ensure_headroom_running()` early-return without spawning the
        // new proxy — silently breaking boot validation.
        if let Err(err) = self.write_ready_flag() {
            let restored = self.rollback_partial_upgrade(had_live_venv, had_receipt);
            return UpgradeOutcome::InstallFailed {
                restored,
                error: err.context("writing READY flag on upgraded venv"),
            };
        }

        progress(BootstrapStepUpdate {
            step: "Install complete",
            message: "Install finished. Verifying Headroom boot…".into(),
            eta_seconds: 0,
            percent: 97,
        });

        UpgradeOutcome::InstalledPendingValidation {
            pip_output_tail: pip_capture.into_inner().into_string(),
        }
    }

    /// Tear down the new venv and restore the previous one. Called by the
    /// `state.rs` upgrade coordinator when boot validation fails.
    /// Idempotent — no-op if no backup exists.
    pub fn rollback_headroom_upgrade(&self) -> Result<()> {
        // Windows: clear venv file locks first — this path pip-reinstalls
        // (in-place) or renames the venv (swap), and both fail on locked
        // files (RUST-6Z/70).
        crate::state::kill_venv_lock_holders(&self.runtime.venv_dir);
        // In-place rollback: no venv backup. Restore deps from the lock
        // snapshot (if the upgrade touched the lock), then pip-reinstall the
        // previous headroom-ai and restore the receipt.
        let in_place_marker = self
            .read_in_place_marker_checked()
            .context("reading in-place upgrade marker during rollback")?;
        if let Some((previous_version, _target, previous_lock_backup)) = in_place_marker {
            if let Some(ref backup) = previous_lock_backup {
                self.pip_restore_deps_from_backup(backup).with_context(|| {
                    format!(
                        "rollback failed — could not restore dependencies from {}",
                        backup.display()
                    )
                })?;
                std::fs::copy(backup, self.active_lock_path())
                    .with_context(|| format!("restoring active lock from {}", backup.display()))?;
                std::fs::remove_file(backup).with_context(|| {
                    format!("removing consumed lock snapshot {}", backup.display())
                })?;
            }
            self.pip_force_reinstall_headroom_version(&previous_version)
                .with_context(|| {
                    format!(
                        "rollback failed — could not reinstall previous Headroom version {previous_version}"
                    )
                })?;
            let receipt_backup = self.headroom_receipt_backup_path();
            let receipt_path = self.headroom_receipt_path();
            if receipt_backup.exists() {
                std::fs::copy(&receipt_backup, &receipt_path)
                    .with_context(|| format!("restoring {}", receipt_path.display()))?;
                let _ = std::fs::remove_file(&receipt_backup);
            }
            self.clear_upgrade_marker();
            return Ok(());
        }

        let backup_dir = self.venv_backup_dir();
        if !backup_dir.exists() {
            return Ok(());
        }
        let had_live_venv = true; // by definition, if we have a backup
        let had_receipt = self.headroom_receipt_backup_path().exists();
        let restored = self.rollback_partial_upgrade(had_live_venv, had_receipt);
        if !restored {
            bail!(
                "rollback failed — venv.backup is present but could not be restored to {}",
                self.runtime.venv_dir.display()
            );
        }
        Ok(())
    }

    /// Returns true if the bundled requirements lock's dep pins differ from
    /// what was installed (ignoring comment/whitespace churn). Conservative:
    /// if we can't determine the installed sha, assume it differs.
    fn lock_pins_differ_from_installed(&self) -> bool {
        let Some(stored) = self.installed_requirements_lock_sha() else {
            return true;
        };
        let current = requirements_lock_sha(bootstrap_requirements_lock());
        stored != current && !LEGACY_REQUIREMENTS_LOCK_SHAS.contains(&stored.as_str())
    }

    fn active_lock_path(&self) -> PathBuf {
        self.runtime
            .downloads_dir
            .join("headroom-requirements.lock")
    }

    fn lock_backup_path(&self) -> PathBuf {
        self.runtime
            .downloads_dir
            .join("headroom-requirements.lock.backup")
    }

    /// Prepare to upgrade the runtime in place (no venv rebuild). Returns
    /// `None` when the caller should fall back to the full atomic rebuild:
    /// either there is no prior install to upgrade, the previously-installed
    /// version is below `ATOMIC_REBUILD_FLOOR_VERSION` (in-place pip across
    /// that delta leaves stale native libs), or the lock churned but the
    /// active lock file is missing on disk so we can't safely snapshot for
    /// rollback.
    ///
    /// When `Some`, the caller owns `previous_lock_backup` (if set): on
    /// success, `commit_headroom_upgrade` deletes it; on failure, rollback
    /// uses it to restore the prior pin set.
    fn prepare_in_place_upgrade(&self) -> Option<InPlaceUpgradeContext> {
        let previous_version = self.installed_headroom_version()?;
        if receipt_requires_atomic_rebuild(&previous_version) {
            log::info!(
                "prepare_in_place_upgrade: receipt {} predates atomic-rebuild floor {:?}; \
                 forcing full venv rebuild",
                previous_version,
                ATOMIC_REBUILD_FLOOR_VERSION
            );
            return None;
        }
        let previous_lock_backup = if self.lock_pins_differ_from_installed() {
            let active = self.active_lock_path();
            if !active.exists() {
                return None;
            }
            let backup = self.lock_backup_path();
            if let Err(err) = std::fs::remove_file(&backup) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::warn!(
                        "prepare_in_place_upgrade: could not remove stale lock backup {}: {err}",
                        backup.display()
                    );
                    return None;
                }
            }
            if let Err(err) = std::fs::copy(&active, &backup) {
                log::warn!(
                    "prepare_in_place_upgrade: could not snapshot lock {} to {}: {err}",
                    active.display(),
                    backup.display()
                );
                return None;
            }
            Some(backup)
        } else {
            None
        };
        Some(InPlaceUpgradeContext {
            previous_version,
            previous_lock_backup,
        })
    }

    /// In-place upgrade: mutate the live venv rather than rebuilding it.
    /// When `ctx.previous_lock_backup` is set, runs
    /// `pip install --upgrade -r lock` first so only churned dep pins are
    /// reinstalled (pip skips packages already at the pinned version). Then
    /// force-reinstalls the new `headroom-ai` wheel.
    ///
    /// On any failure, attempts to restore the prior version and (if
    /// applicable) the prior lock via the same pip mechanism.
    fn in_place_upgrade_headroom<F>(
        &self,
        release: &HeadroomRelease,
        ctx: InPlaceUpgradeContext,
        mut progress: F,
    ) -> UpgradeOutcome
    where
        F: FnMut(BootstrapStepUpdate),
    {
        let receipt_path = self.headroom_receipt_path();
        let receipt_backup = self.headroom_receipt_backup_path();

        // Purge stale receipt backup from any cleanly-completed prior upgrade.
        let _ = std::fs::remove_file(&receipt_backup);

        // Snapshot the receipt so rollback can restore the old artifact pointers.
        if receipt_path.exists() {
            if let Err(err) = std::fs::copy(&receipt_path, &receipt_backup) {
                if let Some(ref p) = ctx.previous_lock_backup {
                    let _ = std::fs::remove_file(p);
                }
                return UpgradeOutcome::InstallFailed {
                    restored: true,
                    error: anyhow!("failed to snapshot {}: {err}", receipt_path.display()),
                };
            }
        }

        // Write marker so an interrupted upgrade can be recovered on next launch.
        if let Err(err) = self.write_upgrade_marker(
            &release.version,
            Some(&ctx.previous_version),
            ctx.previous_lock_backup.as_deref(),
        ) {
            let _ = std::fs::remove_file(&receipt_backup);
            if let Some(ref p) = ctx.previous_lock_backup {
                let _ = std::fs::remove_file(p);
            }
            return UpgradeOutcome::InstallFailed {
                restored: true,
                error: err.context("writing upgrade-in-progress marker"),
            };
        }

        // Bounded ring buffer collecting pip stdout/stderr across both
        // install steps. Attached to the boot-validation Sentry event when
        // it later fails — pip can return exit 0 while leaving the venv
        // broken (skipped packages, ABI-mismatched native deps), and the
        // tail is the only forensic record of what pip actually did.
        let pip_capture = std::cell::RefCell::new(PipOutputCapture::new(100));

        // Dep-lock upgrade (only when pins changed).
        if ctx.previous_lock_backup.is_some() {
            progress(BootstrapStepUpdate {
                step: "Updating dependencies",
                message: "Updating Headroom's bundled dependencies.".into(),
                eta_seconds: 45,
                percent: 15,
            });

            let requirements_lock = bootstrap_requirements_lock();
            let lock_path = match self.write_headroom_requirements_lock(requirements_lock) {
                Ok(p) => p,
                Err(err) => {
                    let restored = self.rollback_in_place_upgrade_inner(&ctx);
                    return UpgradeOutcome::InstallFailed {
                        restored,
                        error: err,
                    };
                }
            };

            let deps_start = std::time::Instant::now();
            let deps_progress_ref = std::cell::RefCell::new(&mut progress);
            let mut dep_counter: u32 = 0;
            if let Err(err) = run_pip_install_with_retries_streaming(
                &self.runtime.managed_python(),
                &[
                    "-m",
                    "pip",
                    "install",
                    "--timeout",
                    "180",
                    "--retries",
                    "10",
                    PIP_ONLY_BINARY,
                    "--find-links",
                    VENDOR_WHEELS_INDEX_URL,
                    "--extra-index-url",
                    "https://pypi.org/simple",
                    "--upgrade",
                    "--requirement",
                    lock_path.to_string_lossy().as_ref(),
                ],
                &self.runtime.root_dir,
                |line| {
                    pip_capture.borrow_mut().push(line);
                    if let Some(update) =
                        pip_line_to_progress(line, deps_start.elapsed(), &mut dep_counter, 15, 55)
                    {
                        if let Ok(mut cb) = deps_progress_ref.try_borrow_mut() {
                            (cb)(update);
                        }
                    }
                },
            ) {
                let restored = self.rollback_in_place_upgrade_inner(&ctx);
                return UpgradeOutcome::InstallFailed {
                    restored,
                    error: err.context("upgrading Headroom's bundled dependencies in place"),
                };
            }
        }

        progress(BootstrapStepUpdate {
            step: "Downloading update",
            message: "Fetching Headroom update bundle.".into(),
            eta_seconds: 10,
            percent: 60,
        });

        let wheel_path = self
            .runtime
            .downloads_dir
            .join(format!("headroom_ai-{}-py3-none-any.whl", release.version));
        let use_wheel =
            match download_to_path(&release.wheel_url, &wheel_path, Some(&release.sha256)) {
                Ok(()) => true,
                Err(download_err) if is_checksum_mismatch(&download_err) => {
                    // Never downgrade a failed integrity check to an
                    // unverified pip-index install.
                    let restored = self.rollback_in_place_upgrade_inner(&ctx);
                    return UpgradeOutcome::InstallFailed {
                    restored,
                    error: download_err.context(
                        "Headroom wheel failed checksum verification; refusing unverified fallback",
                    ),
                };
                }
                Err(download_err) => {
                    log::warn!(
                    "headroom wheel download failed (will fall back to pip index): {download_err}"
                );
                    false
                }
            };

        progress(BootstrapStepUpdate {
            step: "Applying update",
            message: "Installing the new Headroom wheel.".into(),
            eta_seconds: 10,
            percent: 75,
        });

        let headroom_spec = format!("headroom-ai=={}", release.version);
        let headroom_arg = if use_wheel {
            wheel_path.to_string_lossy().into_owned()
        } else {
            headroom_spec.clone()
        };
        if let Err(err) = run_pip_install_with_retries_streaming(
            &self.runtime.managed_python(),
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                PIP_ONLY_BINARY,
                "--extra-index-url",
                "https://pypi.org/simple",
                "--no-deps",
                "--force-reinstall",
                &headroom_arg,
            ],
            &self.runtime.root_dir,
            |line| {
                pip_capture.borrow_mut().push(line);
            },
        ) {
            let restored = self.rollback_in_place_upgrade_inner(&ctx);
            let context_msg = if use_wheel {
                "installing verified Headroom wheel into Headroom-managed virtualenv"
            } else {
                "installing headroom-ai from PyPI into Headroom-managed virtualenv"
            };
            return UpgradeOutcome::InstallFailed {
                restored,
                error: err.context(context_msg),
            };
        }

        // Ad-hoc sign every native extension pip just dropped in. Failures
        // are logged and ignored; the smoke test below is the real gate.
        self.ad_hoc_sign_venv_natives();

        progress(BootstrapStepUpdate {
            step: "Verifying install",
            message: "Running Headroom import smoke test.".into(),
            eta_seconds: 3,
            percent: 85,
        });

        if let Err(err) = self.smoke_test_headroom() {
            let restored = self.rollback_in_place_upgrade_inner(&ctx);
            return UpgradeOutcome::InstallFailed {
                restored,
                error: err,
            };
        }

        // Optional addon: an in-place upgrade keeps the venv, so markitdown
        // should still run. Warn-only — a broken optional addon must not fail
        // the core Headroom upgrade.
        if let Err(err) = self.smoke_test_markitdown() {
            log::warn!("markitdown smoke test failed after upgrade: {err:#}");
        }
        for plugin in &PLUGIN_ADDONS {
            if let Err(err) = self.smoke_test_plugin(plugin.id) {
                log::warn!("{} smoke test failed after upgrade: {err:#}", plugin.id);
            }
        }

        progress(BootstrapStepUpdate {
            step: "Configuring integrations",
            message: "Setting up Headroom MCP integration.".into(),
            eta_seconds: 5,
            percent: 92,
        });

        let mcp_install = match self.install_headroom_mcp() {
            Ok(method) => json!({
                "configured": true,
                "proxyUrl": HEADROOM_PROXY_URL,
                "installMethod": method.as_str(),
            }),
            Err(err) => {
                log::info!("headroom MCP setup skipped: {err:#}");
                json!({
                    "configured": false,
                    "proxyUrl": HEADROOM_PROXY_URL,
                    "error": err.to_string(),
                })
            }
        };

        if let Err(err) = self.update_headroom_receipt_after_in_place_upgrade(release, mcp_install)
        {
            let restored = self.rollback_in_place_upgrade_inner(&ctx);
            return UpgradeOutcome::InstallFailed {
                restored,
                error: err,
            };
        }

        progress(BootstrapStepUpdate {
            step: "Install complete",
            message: "Install finished. Verifying Headroom boot…".into(),
            eta_seconds: 0,
            percent: 97,
        });

        UpgradeOutcome::InstalledPendingValidation {
            pip_output_tail: pip_capture.into_inner().into_string(),
        }
    }

    fn pip_force_reinstall_headroom_version(&self, version: &str) -> Result<()> {
        let spec = format!("headroom-ai=={version}");
        run_pip_install_with_retries(
            &self.runtime.managed_python(),
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                PIP_ONLY_BINARY,
                "--extra-index-url",
                "https://pypi.org/simple",
                "--no-deps",
                "--force-reinstall",
                &spec,
            ],
            &self.runtime.root_dir,
        )
        .with_context(|| format!("reinstalling Headroom version {version}"))
    }

    /// Recover from a pydantic / pydantic-core version skew by reinstalling
    /// pydantic-core at the version pydantic wants. Triggered when the proxy
    /// log shows the SystemError pydantic raises during import. `--no-deps`
    /// keeps the rest of the venv untouched.
    fn repair_pydantic_core(&self, target_version: &str) -> Result<()> {
        // Reinstall pydantic itself first (no version pin) to rewrite its
        // dist-info. A failed prior upgrade can leave two `pydantic-X.Y.dist-info`
        // dirs in site-packages; `importlib.metadata.metadata('pydantic')` then
        // returns either one non-deterministically, producing flip-flopping
        // "requires N.N.N" errors across attempts. Force-reinstalling pydantic
        // collapses the duplicates so the next pin we apply actually matches
        // what pydantic asks for.
        run_pip_install_with_retries(
            &self.runtime.managed_python(),
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                PIP_ONLY_BINARY,
                "--extra-index-url",
                "https://pypi.org/simple",
                "--no-deps",
                "--force-reinstall",
                "pydantic",
            ],
            &self.runtime.root_dir,
        )
        .with_context(|| "reinstalling pydantic to clear duplicate dist-info")?;

        let spec = format!("pydantic-core=={target_version}");
        run_pip_install_with_retries(
            &self.runtime.managed_python(),
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                PIP_ONLY_BINARY,
                "--extra-index-url",
                "https://pypi.org/simple",
                "--no-deps",
                "--force-reinstall",
                &spec,
            ],
            &self.runtime.root_dir,
        )
        .with_context(|| format!("reinstalling pydantic-core=={target_version}"))
    }

    /// Restore deps from `previous_lock_backup` via
    /// `pip install --upgrade -r <backup>` — packages already at the old pin
    /// are skipped by pip, only packages that were actually churned by the
    /// failed upgrade get reinstalled.
    fn pip_restore_deps_from_backup(&self, backup_lock: &Path) -> Result<()> {
        run_pip_install_with_retries(
            &self.runtime.managed_python(),
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                PIP_ONLY_BINARY,
                "--find-links",
                VENDOR_WHEELS_INDEX_URL,
                "--extra-index-url",
                "https://pypi.org/simple",
                "--upgrade",
                "--requirement",
                backup_lock.to_string_lossy().as_ref(),
            ],
            &self.runtime.root_dir,
        )
        .with_context(|| {
            format!(
                "restoring Headroom dependencies from {}",
                backup_lock.display()
            )
        })
    }

    fn rollback_in_place_upgrade_inner(&self, ctx: &InPlaceUpgradeContext) -> bool {
        // Re-kill venv lock holders: an IDE can respawn its MCP server
        // between the failed install and this rollback, and a rollback that
        // hits the same Windows file locks reports restored=false and leaves
        // the runtime bricked (RUST-70).
        crate::state::kill_venv_lock_holders(&self.runtime.venv_dir);
        // Restore deps first so headroom-ai lands on a consistent dep set.
        let deps_ok = match ctx.previous_lock_backup.as_deref() {
            Some(backup) => {
                let pip_ok = match self.pip_restore_deps_from_backup(backup) {
                    Ok(()) => true,
                    Err(err) => {
                        log::error!(
                            "rollback_in_place_upgrade: restoring dependencies from {} failed: {err:#}",
                            backup.display()
                        );
                        false
                    }
                };
                let mut ok = pip_ok;
                let active = self.active_lock_path();
                if let Err(err) = std::fs::copy(backup, &active) {
                    log::error!(
                        "rollback_in_place_upgrade: restoring lock snapshot {} failed: {err}",
                        backup.display()
                    );
                    ok = false;
                } else if pip_ok {
                    if let Err(err) = std::fs::remove_file(backup) {
                        log::error!(
                            "rollback_in_place_upgrade: removing consumed lock snapshot {} failed: {err}",
                            backup.display()
                        );
                        ok = false;
                    }
                }
                if !pip_ok {
                    log::error!(
                        "rollback_in_place_upgrade: retaining lock snapshot {} because dependency restore failed",
                        backup.display()
                    );
                }
                ok
            }
            None => true,
        };
        let wheel_ok = self
            .pip_force_reinstall_headroom_version(&ctx.previous_version)
            .is_ok();
        let receipt_backup = self.headroom_receipt_backup_path();
        let receipt_path = self.headroom_receipt_path();
        let receipt_ok = if receipt_backup.exists() {
            match std::fs::copy(&receipt_backup, &receipt_path) {
                Ok(_) => match std::fs::remove_file(&receipt_backup) {
                    Ok(()) => true,
                    Err(err) => {
                        log::error!(
                            "rollback_in_place_upgrade: removing consumed receipt backup {} failed: {err}",
                            receipt_backup.display()
                        );
                        false
                    }
                },
                Err(err) => {
                    log::error!(
                        "rollback_in_place_upgrade: restoring receipt {} failed: {err}",
                        receipt_backup.display()
                    );
                    false
                }
            }
        } else {
            true
        };
        let restored = deps_ok && wheel_ok && receipt_ok;
        if restored {
            self.clear_upgrade_marker();
        } else {
            log::error!(
                "rollback_in_place_upgrade: recovery incomplete; retaining marker and any backups for retry"
            );
        }
        restored
    }

    fn update_headroom_receipt_after_in_place_upgrade(
        &self,
        release: &HeadroomRelease,
        mcp_install: Value,
    ) -> Result<()> {
        let receipt_path = self.headroom_receipt_path();
        let bytes = std::fs::read(&receipt_path)
            .with_context(|| format!("reading {}", receipt_path.display()))?;
        let mut receipt: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", receipt_path.display()))?;
        receipt["version"] = json!(release.version);
        if let Some(artifact) = receipt.get_mut("artifact").and_then(|a| a.as_object_mut()) {
            artifact.insert("url".into(), json!(release.wheel_url));
            artifact.insert("sha256".into(), json!(release.sha256));
            artifact.insert(
                "requirementsLockSha256".into(),
                json!(requirements_lock_sha(bootstrap_requirements_lock())),
            );
        }
        receipt["mcp"] = mcp_install;
        crate::client_adapters::atomic_write(&receipt_path, &serde_json::to_vec_pretty(&receipt)?)
            .with_context(|| format!("writing {}", receipt_path.display()))?;
        Ok(())
    }

    /// Finalize a successful atomic upgrade. Deletes the backup venv and
    /// receipt snapshot. Non-fatal if cleanup fails — a future upgrade's
    /// "purge stale backup" step will clean up whatever we left behind.
    pub fn commit_headroom_upgrade(&self) -> Result<()> {
        let backup_dir = self.venv_backup_dir();
        if backup_dir.exists() {
            if let Err(err) = std::fs::remove_dir_all(&backup_dir) {
                log::warn!(
                    "commit_headroom_upgrade: non-fatal: failed to remove {}: {err}",
                    backup_dir.display()
                );
            }
        }
        let _ = std::fs::remove_file(self.headroom_receipt_backup_path());
        let _ = std::fs::remove_file(self.lock_backup_path());
        // Clear the in-progress marker last, so a mid-commit crash (e.g.,
        // between the remove_dir_all of the backup and the marker cleanup)
        // still looks like an interrupted upgrade on the next launch and
        // triggers recovery rather than a potentially-unsafe purge.
        self.clear_upgrade_marker();
        Ok(())
    }

    /// Restore both venv + receipt from their backups. Used from the atomic
    /// upgrade failure path and from the post-boot-validation rollback path.
    /// Returns true if the restore succeeded.
    fn rollback_partial_upgrade(&self, had_live_venv: bool, had_receipt: bool) -> bool {
        // Remove any partial new venv.
        if self.runtime.venv_dir.exists() {
            if let Err(err) = std::fs::remove_dir_all(&self.runtime.venv_dir) {
                log::error!(
                    "rollback: failed to remove partial venv at {}: {err}",
                    self.runtime.venv_dir.display()
                );
                return false;
            }
        }
        let venv_restored = self.restore_venv_from_backup(had_live_venv);
        if !venv_restored {
            return false;
        }
        if had_receipt {
            let receipt_path = self.headroom_receipt_path();
            let receipt_backup = self.headroom_receipt_backup_path();
            if let Err(err) = std::fs::copy(&receipt_backup, &receipt_path) {
                log::error!(
                    "rollback: failed to restore {}: {err}",
                    receipt_path.display()
                );
                return false;
            }
            if let Err(err) = std::fs::remove_file(&receipt_backup) {
                log::error!(
                    "rollback: failed to remove consumed receipt backup {}: {err}",
                    receipt_backup.display()
                );
                return false;
            }
        }
        // Rollback complete — clear the marker so we don't trigger recovery
        // on the next launch.
        self.clear_upgrade_marker();
        true
    }

    fn restore_venv_from_backup(&self, had_live_venv: bool) -> bool {
        if !had_live_venv {
            return true;
        }
        let backup_dir = self.venv_backup_dir();
        if !backup_dir.exists() {
            return true;
        }
        match std::fs::rename(&backup_dir, &self.runtime.venv_dir) {
            Ok(()) => true,
            Err(err) => {
                log::error!(
                    "rollback: failed to restore venv from {}: {err}",
                    backup_dir.display()
                );
                false
            }
        }
    }

    /// Runs MCP install if the receipt shows it is not configured, or was
    /// configured via the legacy `~/.claude/mcp.json` fallback (which Claude
    /// Code ≥2.x ignores). Safe to call at every launch — no-ops when the
    /// server is already registered via `claude mcp add` or direct json write.
    ///
    /// If the install fails with a Python `ModuleNotFoundError`/`ImportError`
    /// in stderr, the venv is missing one or more pinned dependencies despite
    /// the receipt's `requirementsLockSha256` saying otherwise (seen on
    /// upgrades from very-old desktop versions where a partial install left
    /// the receipt stamped but the venv incomplete). Self-heal by running the
    /// requirements repair, which re-installs the full lock file and retries
    /// MCP install internally.
    pub fn ensure_mcp_configured(&self) -> Result<()> {
        if self.headroom_mcp_configured() == Some(true)
            && matches!(
                self.headroom_mcp_install_method().as_deref(),
                Some(MCP_METHOD_CLAUDE_CLI)
                    | Some(MCP_METHOD_DIRECT_CLAUDE_JSON)
                    | Some(MCP_METHOD_COMMUNITY_REGISTRY)
            )
        {
            return Ok(());
        }
        let method = match self.install_headroom_mcp() {
            Ok(method) => method,
            Err(err) if looks_like_corrupt_venv_error(&err) => {
                log::warn!(
                    "MCP install hit a Python import error; running requirements repair: {err:#}"
                );
                sentry::capture_message(
                    "MCP install hit corrupt-venv signal; auto-running requirements repair",
                    sentry::Level::Info,
                );
                self.repair_stale_requirements_with_progress(|_| {})
                    .context("auto-repairing venv after MCP install import error")?;
                // repair_stale_requirements_with_progress runs install_headroom_mcp
                // and writes the mcp section of the receipt itself, so we're done.
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        let receipt_path = self.runtime.tools_dir.join("headroom.json");
        let bytes = match std::fs::read(&receipt_path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("reading {} after MCP install", receipt_path.display())
                })
            }
        };
        let mut receipt: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {} after MCP install", receipt_path.display()))?;
        receipt["mcp"] = json!({
            "configured": true,
            "proxyUrl": HEADROOM_PROXY_URL,
            "installMethod": method.as_str(),
        });
        crate::client_adapters::atomic_write(&receipt_path, &serde_json::to_vec(&receipt)?)
            .with_context(|| {
                format!("recording MCP configuration in {}", receipt_path.display())
            })?;
        Ok(())
    }

    fn install_headroom_mcp(&self) -> Result<McpInstallMethod> {
        let entrypoint = self.headroom_entrypoint();
        let detected_claude = crate::claude_cli::detect_claude_cli();

        if crate::edition::LOCAL_COMMUNITY {
            let entrypoint_arg = entrypoint.to_string_lossy().into_owned();
            let workspace_arg = crate::edition::workspace_dir()
                .to_string_lossy()
                .into_owned();
            let config_arg = crate::edition::config_dir().to_string_lossy().into_owned();
            self.run_mcp_helper(&[
                "-c",
                COMMUNITY_MCP_INSTALL_HELPER,
                &entrypoint_arg,
                HEADROOM_PROXY_URL,
                &workspace_arg,
                &config_arg,
            ])
            .context("registering isolated Community MCP server")?;

            let _ = crate::client_adapters::pin_codex_mcp_command(&entrypoint);
            let _ = crate::client_adapters::pin_grok_mcp_command(&entrypoint);
            return Ok(McpInstallMethod::CommunityRegistry);
        }

        // GUI apps launched from Finder/Dock inherit a minimal PATH that
        // excludes /opt/homebrew/bin, /usr/local/bin, ~/.claude/local/bin,
        // etc. Without augmentation, `shutil.which("claude")` inside the
        // Python CLI returns None and it falls back to writing
        // ~/.claude/mcp.json — a legacy path Claude Code ≥2.x does not read.
        let run_install = |force: bool| -> Result<(std::process::Output, Vec<&'static str>)> {
            let mut args: Vec<&'static str> =
                vec!["mcp", "install", "--proxy-url", HEADROOM_PROXY_URL];
            if force {
                args.push("--force");
            }
            let mut cmd = build_command(&entrypoint, &args[..], &self.runtime.root_dir);
            if let Some(claude_path) = detected_claude.as_ref() {
                let existing = std::env::var_os("PATH");
                cmd.env(
                    "PATH",
                    crate::claude_cli::path_with_binary_dir(claude_path, existing.as_deref()),
                );
            }
            let output = cmd
                .output()
                .with_context(|| format!("starting {} {}", entrypoint.display(), args.join(" ")))
                .context("configuring Headroom MCP integration")?;
            Ok((output, args))
        };

        // Always pass --force so that stale entrypoints left over from a
        // previous Headroom version (e.g. venv python3 path → headroom CLI)
        // are overwritten without a separate retry. --force is a no-op when
        // the config is already correct or absent. Desktop owns this config.
        let (output, args) = run_install(true)?;

        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let exit_code = output.status.code();
            let signal = exit_status_signal(&output.status);

            // If the Python CLI exited non-zero only because no supported tool
            // (claude, codex, etc.) was detected on PATH, it wrote nothing --
            // but the direct JSON write path below can still configure the
            // integration. Fall through instead of surfacing a Sentry warning.
            if !stdout.contains("not detected on this system") {
                let detected = detected_claude
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<not detected>".into());
                sentry::with_scope(
                    |scope| {
                        scope.set_extra("claude_cli_detected", detected.clone().into());
                        scope.set_extra(
                            "exit_code",
                            exit_code
                                .map(|c| c.into())
                                .unwrap_or(serde_json::Value::Null),
                        );
                        scope.set_extra(
                            "signal",
                            signal.map(|s| s.into()).unwrap_or(serde_json::Value::Null),
                        );
                        scope.set_extra(
                            "stdout_tail",
                            stdout[stdout.char_indices().rev().nth(2047).map_or(0, |(i, _)| i)..]
                                .into(),
                        );
                        scope.set_extra(
                            "stderr_tail",
                            stderr[stderr.char_indices().rev().nth(2047).map_or(0, |(i, _)| i)..]
                                .into(),
                        );
                    },
                    || {
                        sentry::capture_message(
                            "Headroom MCP install exited non-zero",
                            sentry::Level::Warning,
                        );
                    },
                );
                return Err(anyhow::Error::new(CommandFailure {
                    program: entrypoint.display().to_string(),
                    args: args.iter().map(|s| s.to_string()).collect(),
                    stdout,
                    stderr,
                    exit_code,
                    signal,
                }))
                .context("configuring Headroom MCP integration");
            }
        }

        // Codex stores the MCP server command as a bare `headroom` (PATH-based,
        // via ~/.local/bin/headroom). When the managed runtime relocates that
        // symlink dangles and Codex fails to start the server. Pin it to the
        // absolute entrypoint so it survives runtime moves. Best-effort: a
        // failure here must not break the Claude integration below.
        let _ = crate::client_adapters::pin_codex_mcp_command(&entrypoint);
        let _ = crate::client_adapters::pin_grok_mcp_command(&entrypoint);

        // Ground truth: did Claude Code actually see the server? The Python
        // CLI's fallback branch writes ~/.claude/mcp.json (legacy, ignored by
        // Claude Code ≥2.x) and exits 0, so the subprocess succeeding is not
        // a reliable proxy for "integration works". Read the file Claude Code
        // actually reads and confirm the registration landed there.
        if claude_code_has_headroom_mcp_server() {
            return Ok(McpInstallMethod::ClaudeCli);
        }

        // The Python CLI couldn't find `claude` (e.g. GUI launch with bare
        // PATH) and wrote ~/.claude/mcp.json instead. Write the entry
        // directly to ~/.claude.json, which is what Claude Code ≥2.x reads.
        if let Ok(()) = write_headroom_to_claude_json(&entrypoint, HEADROOM_PROXY_URL) {
            if claude_code_has_headroom_mcp_server() {
                return Ok(McpInstallMethod::DirectClaudeJson);
            }
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detected = detected_claude
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<not detected>".into());
        sentry::with_scope(
            |scope| {
                scope.set_extra("claude_cli_detected", detected.clone().into());
                scope.set_extra(
                    "stdout_tail",
                    stdout[stdout.char_indices().rev().nth(511).map_or(0, |(i, _)| i)..].into(),
                );
                scope.set_extra(
                    "stderr_tail",
                    stderr[stderr.char_indices().rev().nth(511).map_or(0, |(i, _)| i)..].into(),
                );
            },
            || {
                sentry::capture_message(
                    "Headroom MCP install exited 0 but Claude Code does not see the server \
                     (fell back to ~/.claude/mcp.json which Claude Code ≥2.x ignores).",
                    sentry::Level::Warning,
                );
            },
        );
        Ok(McpInstallMethod::FallbackJson)
    }

    fn recover_rtk_pending(&self) -> Result<()> {
        let marker = self.runtime.downloads_dir.join("rtk-install.pending");
        if !marker.exists() {
            return Ok(());
        }
        let pending: Value = serde_json::from_slice(
            &std::fs::read(&marker).with_context(|| format!("reading {}", marker.display()))?,
        )
        .with_context(|| format!("parsing {}", marker.display()))?;
        let destination = pending
            .get("destination")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| self.rtk_entrypoint());
        let staged = pending
            .get("staged")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let backup = pending
            .get("backup")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let receipt = self.runtime.tools_dir.join("rtk.json");
        let receipt_backup = self.runtime.tools_dir.join("rtk.json.backup");

        // Restore the old binary/receipt when a commit had already moved them
        // aside. If no backup exists, the marker may have been written before
        // the first rename; leave the live installation untouched.
        if let Some(backup) = backup.filter(|p| p.exists()) {
            if destination.exists() {
                std::fs::remove_file(&destination)
                    .with_context(|| format!("removing incomplete {}", destination.display()))?;
            }
            std::fs::rename(&backup, &destination)
                .with_context(|| format!("restoring {}", destination.display()))?;
        }
        if receipt_backup.exists() {
            if receipt.exists() {
                std::fs::remove_file(&receipt)
                    .with_context(|| format!("removing incomplete {}", receipt.display()))?;
            }
            std::fs::rename(&receipt_backup, &receipt)
                .with_context(|| format!("restoring {}", receipt.display()))?;
        }
        if let Some(staged) = staged.filter(|p| p.exists()) {
            let _ = std::fs::remove_file(staged);
        }
        std::fs::remove_file(&marker).with_context(|| format!("removing {}", marker.display()))?;
        Ok(())
    }

    fn install_rtk_release(&self, version: &str, url: &str, sha256: &str) -> Result<()> {
        let _lock = RTK_INSTALL_LOCK.get_or_init(|| Mutex::new(())).lock();
        self.recover_rtk_pending()?;
        let nonce = RTK_INSTALL_NONCE.fetch_add(1, Ordering::Relaxed);
        let extension = if cfg!(target_os = "windows") {
            "zip"
        } else {
            "tar.gz"
        };
        let archive_path = self.runtime.downloads_dir.join(format!(
            "rtk-v{}-{}-{}.{}",
            version,
            std::env::consts::OS,
            std::env::consts::ARCH,
            extension
        ));
        download_to_path(url, &archive_path, Some(sha256))?;

        let extract_dir = self.runtime.downloads_dir.join("rtk-extract");
        if extract_dir.exists() {
            std::fs::remove_dir_all(&extract_dir)
                .with_context(|| format!("removing {}", extract_dir.display()))?;
        }
        std::fs::create_dir_all(&extract_dir)
            .with_context(|| format!("creating {}", extract_dir.display()))?;

        #[cfg(target_os = "windows")]
        {
            let file = std::fs::File::open(&archive_path)
                .with_context(|| format!("opening {}", archive_path.display()))?;
            let mut archive = zip::ZipArchive::new(file)
                .with_context(|| format!("reading zip {}", archive_path.display()))?;
            archive
                .extract(&extract_dir)
                .with_context(|| format!("extracting into {}", extract_dir.display()))?;
        }
        #[cfg(not(target_os = "windows"))]
        {
            let file = std::fs::File::open(&archive_path)
                .with_context(|| format!("opening {}", archive_path.display()))?;
            let decoder = GzDecoder::new(file);
            let mut archive = Archive::new(decoder);
            archive
                .unpack(&extract_dir)
                .with_context(|| format!("extracting into {}", extract_dir.display()))?;
        }

        let binary_name = if cfg!(target_os = "windows") {
            "rtk.exe"
        } else {
            "rtk"
        };
        let extracted_binary = extract_dir.join(binary_name);
        if !extracted_binary.exists() {
            bail!(
                "rtk extraction completed but {} was not found",
                extracted_binary.display()
            );
        }

        // Stage next to the destination, then rename into place: rtk is
        // exec'd by the PreToolUse hook on nearly every agent Bash command,
        // so copying over the live binary opens a truncated-exec window (and
        // fails with ETXTBSY on Linux while an rtk is running). Rename is
        // atomic for exec.
        let destination = self.rtk_entrypoint();
        let staged = {
            let mut s = destination.as_os_str().to_os_string();
            s.push(format!(".new.{}.{}", std::process::id(), nonce));
            PathBuf::from(s)
        };
        std::fs::copy(&extracted_binary, &staged)
            .with_context(|| format!("writing {}", staged.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&staged)
                .with_context(|| format!("reading {}", staged.display()))?
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&staged, permissions)
                .with_context(|| format!("chmod {}", staged.display()))?;
        }

        // Prove the downloaded binary starts before replacing the working
        // copy. A failed update must leave the existing RTK hook executable.
        run_command_with_timeout(
            &staged,
            &["--version"],
            &self.runtime.root_dir,
            Duration::from_secs(15),
        )
        .context("RTK update failed its smoke test")?;
        let smoke = Command::new(&staged)
            .arg("--version")
            .current_dir(&self.runtime.root_dir)
            .output()
            .context("reading RTK staged version")?;
        let reported = String::from_utf8_lossy(&smoke.stdout);
        if !rtk_version_output_matches(&reported, version) {
            bail!("RTK staged binary reported {reported:?}, expected {version}");
        }

        let backup = {
            let mut path = destination.as_os_str().to_os_string();
            path.push(format!(".old.{}.{}", std::process::id(), nonce));
            PathBuf::from(path)
        };
        let receipt = self.runtime.tools_dir.join("rtk.json");
        let receipt_backup = self.runtime.tools_dir.join("rtk.json.backup");
        if receipt_backup.exists() {
            bail!(
                "stale RTK receipt backup exists without a pending marker; refusing to overwrite recovery data"
            );
        }
        let pending = self.runtime.downloads_dir.join("rtk-install.pending");
        let pending_bytes = serde_json::to_vec(&json!({
            "version": version,
            "staged": staged,
            "destination": destination,
            "backup": backup,
            "hadReceipt": receipt.exists(),
        }))?;
        crate::client_adapters::atomic_write(&pending, &pending_bytes)
            .context("marking RTK install pending")?;
        if receipt.exists() {
            std::fs::rename(&receipt, &receipt_backup)
                .with_context(|| format!("backing up {}", receipt.display()))?;
        }
        if destination.exists() {
            std::fs::rename(&destination, &backup)
                .with_context(|| format!("backing up {}", destination.display()))?;
        }
        if let Err(replace_err) = std::fs::rename(&staged, &destination) {
            let restore = if backup.exists() {
                std::fs::rename(&backup, &destination).err()
            } else {
                None
            };
            return match restore {
                Some(restore_err) => Err(anyhow!(
                    "RTK replacement failed: {replace_err}; restoring previous binary failed: {restore_err}; backup retained at {}",
                    backup.display()
                )),
                None => Err(anyhow!("RTK replacement failed: {replace_err}")),
            };
        }

        let receipt_result = self.write_tool_receipt(
            "rtk",
            json!({
                "status": "healthy",
                "installedBy": "Headroom",
                "scope": "self-contained",
                "runtime": "binary",
                "entrypoint": destination,
                "source": "https://github.com/rtk-ai/rtk",
                "version": version,
                "artifact": {
                    "url": url,
                    "sha256": sha256
                }
            }),
        );
        if let Err(err) = receipt_result {
            return match self.recover_rtk_pending() {
                Ok(()) => {
                    Err(err.context("RTK receipt write failed; previous installation restored"))
                }
                Err(rollback_err) => Err(anyhow!(
                    "RTK receipt write failed: {err:#}; rollback also failed: {rollback_err:#}"
                )),
            };
        }
        for path in [&backup, &receipt_backup, &pending] {
            if let Err(err) = std::fs::remove_file(path) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::warn!(
                        "RTK installed, but cleanup of {} failed: {err}",
                        path.display()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn install_rtk(&self) -> Result<()> {
        let artifact = rtk_distribution_artifact()?;
        self.install_rtk_release(
            RTK_VERSION,
            &artifact.url,
            artifact.sha256.unwrap_or_default(),
        )
    }

    pub fn install_rtk_version(&self, version: Option<&str>) -> Result<()> {
        match version {
            None => self.install_rtk(),
            Some(version) => {
                let release = fetch_rtk_release(version)?;
                if let Some(current) = self.installed_rtk_version().as_deref() {
                    if rtk_version_is_downgrade(current, &release.version) {
                        bail!(
                            "refusing to downgrade RTK from {current} to {}",
                            release.version
                        );
                    }
                }
                self.install_rtk_release(&release.version, &release.url, &release.sha256)
            }
        }
    }

    fn write_headroom_requirements_lock(&self, contents: &str) -> Result<PathBuf> {
        let lock_path = self
            .runtime
            .downloads_dir
            .join("headroom-requirements.lock");
        crate::client_adapters::atomic_write(&lock_path, contents.as_bytes())
            .with_context(|| format!("writing {}", lock_path.display()))?;
        Ok(lock_path)
    }

    fn write_bootstrap_receipt(&self) -> Result<()> {
        let receipt = self.runtime.root_dir.join("bootstrap-receipt.json");
        // Receipts/flags gate bootstrap and upgrade decisions: a truncated one
        // reads as "not installed" and triggers a multi-minute rebuild, so all
        // of them go through tmp+rename.
        crate::client_adapters::atomic_write(
            &receipt,
            &serde_json::to_vec_pretty(&json!({
                "managedBy": "Headroom",
                "runtime": "python",
                "scope": "self-contained",
                "downloadsDir": self.runtime.downloads_dir,
                "managedBinDir": self.runtime.bin_dir,
                "pythonDistribution": self.runtime.standalone_python(),
                "managedPython": self.runtime.managed_python(),
                "managedPip": self.runtime.managed_pip(),
                "toolsDir": self.runtime.tools_dir
            }))
            .context("serializing bootstrap receipt")?,
        )
        .with_context(|| format!("writing {}", receipt.display()))?;
        Ok(())
    }

    fn write_ready_flag(&self) -> Result<()> {
        let ready_flag = self.runtime.ready_flag();
        crate::client_adapters::atomic_write(
            &ready_flag,
            json!({
                "managedPython": self.runtime.managed_python(),
                "managedPip": self.runtime.managed_pip(),
                "scope": "self-contained"
            })
            .to_string()
            .as_bytes(),
        )
        .with_context(|| format!("writing {}", ready_flag.display()))?;
        Ok(())
    }

    fn write_tool_receipt(&self, tool_id: &str, payload: serde_json::Value) -> Result<()> {
        let path = self.runtime.tools_dir.join(format!("{tool_id}.json"));
        crate::client_adapters::atomic_write(
            &path,
            &serde_json::to_vec_pretty(&payload).context("serializing managed tool receipt")?,
        )
        .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    fn plugin_receipt_path(&self, plugin: &PluginAddon) -> PathBuf {
        self.runtime.tools_dir.join(format!("{}.json", plugin.id))
    }

    /// Read and validate a plugin receipt before using it as an ownership
    /// boundary. The dashboard's best-effort status reader intentionally keeps
    /// its `Option` API, but destructive plugin operations must never interpret
    /// malformed JSON (or a wrong-shaped value) as proof that Headroom owns a
    /// host registration.
    fn read_plugin_receipt(&self, plugin: &PluginAddon) -> Result<Option<Value>> {
        let path = self.plugin_receipt_path(plugin);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing managed plugin receipt {}", path.display()))?;
        let object = value.as_object().ok_or_else(|| {
            anyhow!(
                "managed plugin receipt {} must be a JSON object",
                path.display()
            )
        })?;
        if object
            .get("version")
            .and_then(Value::as_str)
            .is_none_or(|version| version.trim().is_empty())
        {
            bail!(
                "managed plugin receipt {} has no non-empty string version",
                path.display()
            );
        }
        if let Some(enabled) = object.get("enabled") {
            if !enabled.is_boolean() {
                bail!(
                    "managed plugin receipt {} has a non-boolean enabled field",
                    path.display()
                );
            }
        }
        if let Some(managed_by) = object.get("managedBy") {
            if managed_by.as_str() != Some("Headroom") {
                bail!(
                    "managed plugin receipt {} has unexpected managedBy value",
                    path.display()
                );
            }
        }
        if let Some(plugin_id) = object.get("pluginId") {
            if plugin_id.as_str() != Some(plugin.id) {
                bail!(
                    "managed plugin receipt {} belongs to a different plugin",
                    path.display()
                );
            }
        }
        Ok(Some(value))
    }

    fn require_plugin_receipt(&self, plugin: &PluginAddon) -> Result<Value> {
        self.read_plugin_receipt(plugin)?.ok_or_else(|| {
            anyhow!(
                "{} is not installed (Headroom ownership receipt is missing)",
                plugin.id
            )
        })
    }

    /// Legacy receipts written before ownership metadata was introduced are
    /// still readable for dashboard/status purposes, but they are not enough
    /// evidence for a destructive host mutation. A version string alone can
    /// be copied into the runtime directory by a user or another installer.
    fn receipt_proves_plugin_ownership(plugin: &PluginAddon, receipt: &Value) -> bool {
        receipt.as_object().is_some_and(|object| {
            object.get("managedBy").and_then(Value::as_str) == Some("Headroom")
                && object.get("pluginId").and_then(Value::as_str) == Some(plugin.id)
        })
    }

    fn require_owned_plugin_receipt(&self, plugin: &PluginAddon) -> Result<Value> {
        let receipt = self.require_plugin_receipt(plugin)?;
        if !Self::receipt_proves_plugin_ownership(plugin, &receipt) {
            bail!(
                "refusing to mutate {} because its legacy receipt lacks explicit Headroom ownership metadata; reinstall it from Headroom to create a managed receipt",
                plugin.id
            );
        }
        Ok(receipt)
    }

    fn plugin_receipt_payload(plugin: &PluginAddon, version: &str, enabled: bool) -> Value {
        json!({
            "managedBy": "Headroom",
            "pluginId": plugin.id,
            "version": version,
            "enabled": enabled
        })
    }

    fn codex_marketplace_recovery_path(&self, plugin: &PluginAddon) -> PathBuf {
        self.runtime
            .tools_dir
            .join(format!("{}-codex-marketplace-recovery.json", plugin.id))
    }

    /// The only checkout path that Headroom may move aside while preparing a
    /// Codex adapter. Keeping the expected sibling deterministic lets recovery
    /// reject a hand-edited marker that points at an unrelated user path.
    fn codex_marketplace_recovery_backup_path(&self, plugin: &PluginAddon) -> PathBuf {
        self.marketplace_package_root(plugin)
            .with_file_name(format!("{}.headroom-legacy", plugin.marketplace_name))
    }

    fn write_codex_marketplace_recovery(
        &self,
        plugin: &PluginAddon,
        recovery: &CodexMarketplaceRecovery,
    ) -> Result<()> {
        let path = self.codex_marketplace_recovery_path(plugin);
        let bytes = serde_json::to_vec_pretty(recovery)
            .context("serializing Codex marketplace recovery metadata")?;
        crate::client_adapters::atomic_write(&path, &bytes)
            .with_context(|| format!("writing {}", path.display()))
    }

    fn read_codex_marketplace_recovery(
        &self,
        plugin: &PluginAddon,
    ) -> Result<Option<CodexMarketplaceRecovery>> {
        let path = self.codex_marketplace_recovery_path(plugin);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let recovery = serde_json::from_slice::<CodexMarketplaceRecovery>(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        if recovery.backup_path.as_os_str().is_empty() {
            bail!("{} has no Codex marketplace backup path", path.display());
        }
        let expected_backup = self.codex_marketplace_recovery_backup_path(plugin);
        if !paths_refer_to_same_location(&recovery.backup_path, &expected_backup) {
            bail!(
                "{} points outside the expected Codex marketplace recovery path {}",
                path.display(),
                expected_backup.display()
            );
        }
        Ok(Some(recovery))
    }

    fn clear_codex_marketplace_recovery(&self, plugin: &PluginAddon) -> Result<()> {
        let path = self.codex_marketplace_recovery_path(plugin);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        Ok(())
    }

    /// Clear a recovery marker that was published for a migration which then
    /// returned `Ok(None)` because another process changed the checkout. This
    /// is deliberately conservative: only an unchanged, unmarked checkout
    /// whose Git remote still matches the recorded host registration proves
    /// that no move occurred. Ambiguous states retain the marker so the next
    /// recovery attempt can inspect them instead of deleting user data.
    fn clear_unmoved_codex_marketplace_recovery_if_safe(
        &self,
        plugin: &PluginAddon,
        registration: &CodexMarketplaceRegistration,
    ) -> Result<bool> {
        if self.codex_marketplace_recovery_backup_path(plugin).exists() {
            return Ok(false);
        }
        let CodexMarketplaceRegistration::Git(expected_source) = registration else {
            return Ok(false);
        };
        let destination = self.marketplace_package_root(plugin);
        if !destination.join(".git").is_dir()
            || destination
                .join(".headroom-local-community-adapter.json")
                .exists()
            || expected_source.trim().is_empty()
            || !git_checkout_matches_source(&destination, expected_source)
        {
            return Ok(false);
        }
        self.clear_codex_marketplace_recovery(plugin)?;
        Ok(true)
    }

    /// Restore a user Git marketplace moved aside for the Codex adapter. The
    /// marker is removed only after both the checkout and the original CLI
    /// registration are back, so a failed recovery remains retryable.
    fn restore_codex_marketplace_recovery(&self, plugin: &PluginAddon, cli: &Path) -> Result<()> {
        let Some(recovery) = self.read_codex_marketplace_recovery(plugin)? else {
            return Ok(());
        };
        let destination = self.marketplace_package_root(plugin);

        let registration =
            codex_marketplace_registration(cli, &self.runtime.root_dir, plugin.marketplace_name)?;
        let original_source = recovery
            .original_git_source
            .as_deref()
            .filter(|source| !source.trim().is_empty());
        let backup_exists = recovery.backup_path.exists();

        // A moved checkout can be restored safely only if its original Git
        // source is known. Without it, a missing host registration would be
        // silently left absent after consuming the backup and clearing the
        // marker. Keep both artifacts intact so a later retry (or manual
        // repair) can supply the missing registration metadata.
        if matches!(registration, CodexMarketplaceRegistration::Missing)
            && original_source.is_none()
        {
            bail!(
                "Codex marketplace recovery for {} has no original Git source; refusing to consume the backup without registration metadata",
                plugin.marketplace_name
            );
        }

        // If the backup has already been consumed, the only safe recovery
        // candidate is an unmarked Git checkout whose remote still matches the
        // recorded source. This covers both a crash before the rename
        // (`moved=false`) and a crash after restoring the checkout but before
        // re-registering it (`moved=true`). Never delete a registration before
        // establishing this identity proof.
        let destination_is_original_checkout = !backup_exists
            && destination.join(".git").is_dir()
            && !destination
                .join(".headroom-local-community-adapter.json")
                .exists()
            && original_source
                .is_some_and(|source| git_checkout_matches_source(&destination, source));
        if !backup_exists && !destination_is_original_checkout {
            bail!(
                "Codex marketplace recovery backup {} is missing; refusing to infer recovery from {}",
                recovery.backup_path.display(),
                destination.display()
            );
        }

        if destination_is_original_checkout {
            match registration {
                CodexMarketplaceRegistration::Missing => {
                    let source = original_source.ok_or_else(|| {
                        anyhow!(
                            "Codex marketplace {} has no original Git source to re-register",
                            plugin.marketplace_name
                        )
                    })?;
                    self.run_plugin_cmd(
                        plugin,
                        cli,
                        PluginHost::Codex,
                        &["plugin", "marketplace", "add", source],
                    )?;
                    ensure_codex_git_registration(
                        cli,
                        &self.runtime.root_dir,
                        plugin.marketplace_name,
                        source,
                    )?;
                }
                CodexMarketplaceRegistration::Git(source)
                    if original_source == Some(source.as_str()) => {}
                CodexMarketplaceRegistration::Git(_) => {
                    bail!(
                        "refusing to replace user-managed Codex marketplace {} during recovery",
                        plugin.marketplace_name
                    );
                }
                CodexMarketplaceRegistration::Local(path) => {
                    bail!(
                        "refusing to replace user-managed Codex marketplace {} at {} during recovery",
                        plugin.marketplace_name,
                        path.display()
                    );
                }
                CodexMarketplaceRegistration::Other(_) => {
                    bail!(
                        "refusing to replace user-managed Codex marketplace {} during recovery",
                        plugin.marketplace_name
                    );
                }
            }
            return self.clear_codex_marketplace_recovery(plugin);
        }

        // Current Codex disables keep the plugin registration and only flip its
        // `enabled` field. This legacy recovery path can still encounter an
        // older install that removed the registration, so handle both shapes:
        // remove only the exact local adapter we created; any other source is
        // user-managed and must remain intact.
        // Validate the ownership marker before this destructive host mutation.
        if destination.exists() && !self.codex_adapter_marketplace_is_owned(plugin)? {
            bail!(
                "refusing to remove unmarked Codex marketplace replacement {} during recovery",
                destination.display()
            );
        }
        let mut original_registration_preserved = false;
        match registration {
            CodexMarketplaceRegistration::Missing => {}
            CodexMarketplaceRegistration::Local(path)
                if paths_refer_to_same_location(&path, &destination) =>
            {
                self.run_plugin_cmd(
                    plugin,
                    cli,
                    PluginHost::Codex,
                    &PluginHost::Codex.marketplace_remove_args(plugin),
                )?;
            }
            CodexMarketplaceRegistration::Local(path) => {
                bail!(
                    "refusing to replace user-managed Codex marketplace {} at {} during recovery",
                    plugin.marketplace_name,
                    path.display()
                );
            }
            CodexMarketplaceRegistration::Git(source) => {
                // A crash can occur after the backup is moved and the marker
                // is written but before the original registration is removed.
                // Preserve that registration only when it is the exact source
                // recorded in the marker; any other Git checkout remains
                // user-managed and is never replaced.
                if original_source == Some(source.as_str()) {
                    original_registration_preserved = true;
                } else {
                    bail!(
                        "refusing to replace user-managed Codex marketplace {} during recovery",
                        plugin.marketplace_name
                    );
                }
            }
            CodexMarketplaceRegistration::Other(_) => {
                bail!(
                    "refusing to replace user-managed Codex marketplace {} during recovery",
                    plugin.marketplace_name
                );
            }
        }
        restore_codex_marketplace_backup(&recovery.backup_path, &destination)?;
        if !original_registration_preserved {
            let source = original_source.ok_or_else(|| {
                anyhow!(
                    "Codex marketplace {} has no original Git source to re-register",
                    plugin.marketplace_name
                )
            })?;
            self.run_plugin_cmd(
                plugin,
                cli,
                PluginHost::Codex,
                &["plugin", "marketplace", "add", source],
            )?;
            ensure_codex_git_registration(
                cli,
                &self.runtime.root_dir,
                plugin.marketplace_name,
                source,
            )?;
        }
        self.clear_codex_marketplace_recovery(plugin)
    }

    /// Validate the marker written at the root of a Codex adapter checkout.
    /// Only a marker with the exact addon id and source URL proves that the
    /// directory is ours; a merely present or hand-edited marker must never
    /// authorize deleting a user's marketplace.
    fn codex_adapter_marketplace_is_owned(&self, plugin: &PluginAddon) -> Result<bool> {
        if !uses_codex_plugin_adapter(plugin, PluginHost::Codex) {
            return Ok(false);
        }
        codex_adapter_marker_is_owned(&self.marketplace_package_root(plugin), plugin)
    }

    /// A marketplace may contain another plugin that the user installed. In
    /// that case removing the marketplace while uninstalling one addon would
    /// delete unrelated capability and registry state, so keep the checkout.
    fn codex_marketplace_has_other_plugin_registrations(
        &self,
        plugin: &PluginAddon,
    ) -> Result<bool> {
        let path = crate::client_adapters::codex_home().join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let root: toml::Value = toml::from_str(&text)
            .with_context(|| format!("parsing Codex config {}", path.display()))?;
        let Some(plugins) = root.get("plugins") else {
            return Ok(false);
        };
        let plugins = plugins.as_table().ok_or_else(|| {
            anyhow!(
                "Codex config {} has a non-table plugins value",
                path.display()
            )
        })?;
        let suffix = format!("@{}", plugin.marketplace_name);
        Ok(plugins
            .keys()
            .any(|key| key.ends_with(&suffix) && key != plugin.plugin_ref))
    }

    fn marketplace_removal_allowed(&self, plugin: &PluginAddon, host: PluginHost) -> Result<bool> {
        if !matches!(host, PluginHost::Codex) {
            // Claude marketplace ownership is not exposed consistently across
            // supported CLI versions. Keep it registered unless a future
            // receipt records explicit ownership; removing it heuristically
            // could delete another Claude plugin's source.
            return Ok(false);
        }
        if !self.codex_adapter_marketplace_is_owned(plugin)? {
            return Ok(false);
        }
        Ok(!self.codex_marketplace_has_other_plugin_registrations(plugin)?)
    }

    fn remove_owned_codex_adapter_marketplace(&self, plugin: &PluginAddon) -> Result<()> {
        if !self.codex_adapter_marketplace_is_owned(plugin)? {
            return Ok(());
        }
        if self.codex_marketplace_has_other_plugin_registrations(plugin)? {
            log::info!(
                "keeping Codex marketplace {} because another plugin is still registered",
                plugin.marketplace_name
            );
            return Ok(());
        }
        let root = self.marketplace_package_root(plugin);
        if root.exists() {
            std::fs::remove_dir_all(&root).with_context(|| {
                format!("removing managed Codex marketplace {}", root.display())
            })?;
        }
        Ok(())
    }

    fn read_tool_receipt(&self, tool_id: &str) -> Option<Value> {
        let path = self.runtime.tools_dir.join(format!("{tool_id}.json"));
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Optional tools persist an `enabled` flag in their receipt. Required core
    /// tools (headroom, rtk) are always enabled. Missing flag defaults to true.
    fn tool_enabled(&self, tool_id: &str) -> bool {
        self.read_tool_receipt(tool_id)
            .and_then(|receipt| receipt.get("enabled").and_then(Value::as_bool))
            .unwrap_or(true)
    }

    pub fn markitdown_entrypoint(&self) -> PathBuf {
        let name = if cfg!(target_os = "windows") {
            "markitdown.exe"
        } else {
            "markitdown"
        };
        self.markitdown_venv_dir().join(bin_subdir()).join(name)
    }

    fn markitdown_venv_dir(&self) -> PathBuf {
        self.runtime.root_dir.join("markitdown-venv")
    }

    /// Shim in the Headroom-managed bin dir. The Office nudge and the Bash
    /// permission both reference this absolute path, so it works whether or not
    /// the bin dir is on PATH (RTK, which exports it, is now opt-in).
    pub fn markitdown_shim_path(&self) -> PathBuf {
        let name = if cfg!(target_os = "windows") {
            "markitdown.cmd"
        } else {
            "markitdown"
        };
        self.runtime.bin_dir.join(name)
    }

    fn markitdown_conversion_counter_path(&self) -> PathBuf {
        self.runtime.tools_dir.join("markitdown-conversions")
    }

    /// Lifetime conversions recorded by the shim. None until the first one.
    pub fn markitdown_conversion_count(&self) -> Option<u64> {
        let raw = std::fs::read_to_string(self.markitdown_conversion_counter_path()).ok()?;
        raw.trim().parse::<u64>().ok().filter(|count| *count > 0)
    }

    /// Wrapper script (previously a bare symlink) so each real conversion bumps
    /// a counter the Addons tab can show. Flag-only invocations (--help) are
    /// not counted. Re-run on every launch so pre-wrapper installs pick it up.
    pub fn ensure_markitdown_shim(&self) -> Result<()> {
        let shim = self.markitdown_shim_path();
        if shim.exists() || shim.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(&shim);
        }
        #[cfg(unix)]
        {
            // ponytail: single-quoting is enough - both paths live under
            // Application Support (spaces, no quotes).
            let script = format!(
                "#!/bin/sh\n\
                 # Headroom-managed markitdown shim. Counts conversions, then runs the real binary.\n\
                 case \"$1\" in\n\
                   \"\"|-*) ;;\n\
                   *)\n\
                     C='{counter}'\n\
                     n=$(cat \"$C\" 2>/dev/null)\n\
                     case \"$n\" in ''|*[!0-9]*) n=0;; esac\n\
                     printf '%s' $((n+1)) > \"$C.tmp\" 2>/dev/null && mv -f \"$C.tmp\" \"$C\" 2>/dev/null\n\
                     ;;\n\
                 esac\n\
                 exec '{real}' \"$@\"\n",
                counter = self.markitdown_conversion_counter_path().display(),
                real = self.markitdown_entrypoint().display(),
            );
            crate::client_adapters::atomic_write(&shim, script.as_bytes())
                .with_context(|| format!("writing markitdown shim {}", shim.display()))?;
            let mut perms = std::fs::metadata(&shim)?.permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(&shim, perms).with_context(|| {
                format!("marking markitdown shim executable {}", shim.display())
            })?;
        }
        #[cfg(target_os = "windows")]
        {
            let script = format!(
                "@echo off\r\n\
                 setlocal\r\n\
                 rem Headroom-managed markitdown shim. Counts conversions, then runs the real binary.\r\n\
                 if \"%~1\"==\"\" goto :run\r\n\
                 if \"%~1\"==\"--help\" goto :run\r\n\
                 set \"C={counter}\"\r\n\
                 set /p n=<\"%C%\" 2>nul\r\n\
                 if not defined n set n=0\r\n\
                 set /a n+=1 >nul 2>nul\r\n\
                 >\"%C%.tmp\" echo %n%\r\n\
                 move /y \"%C%.tmp\" \"%C%\" >nul 2>nul\r\n\
                 :run\r\n\
                 \"{real}\" %*\r\n",
                counter = self.markitdown_conversion_counter_path().display(),
                real = self.markitdown_entrypoint().display(),
            );
            crate::client_adapters::atomic_write(&shim, script.as_bytes())
                .with_context(|| format!("writing markitdown shim {}", shim.display()))?;
        }
        Ok(())
    }

    pub fn markitdown_installed(&self) -> bool {
        self.runtime.tools_dir.join("markitdown.json").exists()
            && self.markitdown_entrypoint().exists()
    }

    pub fn install_markitdown(&self) -> Result<()> {
        self.install_markitdown_version(None)
    }

    pub fn install_markitdown_version(&self, version: Option<&str>) -> Result<()> {
        let _guard = PACKAGE_UPDATE_LOCK.get_or_init(|| Mutex::new(())).lock();
        let version = version
            .map(stable_package_version)
            .transpose()?
            .unwrap_or(MARKITDOWN_PINNED_VERSION);
        if let Some(current) = self
            .read_tool_receipt("markitdown")
            .and_then(|r| r.get("version").and_then(Value::as_str).map(str::to_owned))
            .and_then(|v| parse_major_minor_patch(&v))
        {
            if parse_major_minor_patch(version).is_some_and(|target| target < current) {
                bail!("refusing to downgrade MarkItDown to {version}");
            }
        }
        let final_venv = self.markitdown_venv_dir();
        let staging = self
            .runtime
            .root_dir
            .join(format!("markitdown-venv.staging-{}", uuid::Uuid::new_v4()));
        if let Err(err) = run_command_with_timeout(
            &self.runtime.standalone_python(),
            &["-m", "venv", "--clear", &staging.to_string_lossy()],
            &self.runtime.root_dir,
            Duration::from_secs(120),
        ) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err).context("creating MarkItDown staging venv");
        }
        let python = staging.join(bin_subdir()).join(python_exe_name());
        let package = format!("markitdown[all]=={version}");
        if let Err(err) = run_pip_install_with_retries_streaming(
            &python,
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                &package,
            ],
            &self.runtime.root_dir,
            |line| log::info!("markitdown pip: {line}"),
        ) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err).context("installing MarkItDown into staging venv");
        }
        let staging_entrypoint = staging
            .join(bin_subdir())
            .join(if cfg!(target_os = "windows") {
                "markitdown.exe"
            } else {
                "markitdown"
            });
        if !staging_entrypoint.exists() {
            let _ = std::fs::remove_dir_all(&staging);
            bail!(
                "markitdown install completed but {} was not found",
                staging_entrypoint.display()
            );
        }
        if let Err(err) = run_command_with_timeout(
            &staging_entrypoint,
            &["--help"],
            &self.runtime.root_dir,
            HEADROOM_SMOKE_TEST_TIMEOUT,
        )
        .context("markitdown installed but failed its smoke test")
        {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        let backup = self
            .runtime
            .root_dir
            .join(format!("markitdown-venv.backup-{}", uuid::Uuid::new_v4()));
        let had_old = final_venv.exists();
        if had_old {
            std::fs::rename(&final_venv, &backup).context("staging existing MarkItDown venv")?;
        }
        if let Err(err) = std::fs::rename(&staging, &final_venv)
            .context("activating MarkItDown venv")
            .and_then(|_| self.ensure_markitdown_shim())
            .and_then(|_| {
                let enabled = self.tool_enabled("markitdown");
                self.write_tool_receipt(
                    "markitdown",
                    json!({ "version": version, "enabled": enabled }),
                )
            })
        {
            let activation_error = err.to_string();
            let _ = std::fs::remove_dir_all(&final_venv);
            if had_old {
                if let Err(restore_err) = std::fs::rename(&backup, &final_venv) {
                    return Err(anyhow!(
                    "{activation_error}; additionally failed to restore MarkItDown venv: {restore_err}"
                ));
                }
                if let Err(shim_err) = self.ensure_markitdown_shim() {
                    return Err(anyhow!(
                    "{activation_error}; additionally failed to restore MarkItDown shim: {shim_err}"
                ));
                }
            } else {
                let shim = self.markitdown_shim_path();
                if shim.symlink_metadata().is_ok() {
                    if let Err(shim_err) = std::fs::remove_file(&shim) {
                        return Err(anyhow!(
                        "{activation_error}; additionally failed to remove MarkItDown shim: {shim_err}"
                    ));
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        if had_old {
            if let Err(err) = std::fs::remove_dir_all(&backup) {
                log::warn!(
                    "updated MarkItDown, but could not remove backup {}: {err}",
                    backup.display()
                );
            }
        }
        Ok(())
    }

    pub fn set_markitdown_enabled(&self, enabled: bool) -> Result<()> {
        if !self.markitdown_installed() {
            bail!("markitdown is not installed");
        }
        let stored_version = self.read_tool_receipt("markitdown").and_then(|receipt| {
            receipt
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let version = stored_version
            .as_deref()
            .unwrap_or(MARKITDOWN_PINNED_VERSION);
        self.write_tool_receipt(
            "markitdown",
            json!({ "version": version, "enabled": enabled }),
        )?;
        Ok(())
    }

    pub fn uninstall_markitdown(&self) -> Result<()> {
        let _ = run_command_streaming(
            &self
                .markitdown_venv_dir()
                .join(bin_subdir())
                .join(python_exe_name()),
            &["-m", "pip", "uninstall", "-y", "markitdown"],
            &self.runtime.root_dir,
            &mut |line: &str| log::info!("markitdown pip uninstall: {line}"),
        );
        let shim = self.markitdown_shim_path();
        if shim.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(&shim);
        }
        let receipt = self.runtime.tools_dir.join("markitdown.json");
        if receipt.exists() {
            std::fs::remove_file(&receipt)
                .with_context(|| format!("removing {}", receipt.display()))?;
        }
        Ok(())
    }

    /// Serena lives in its own venv: its LSP dependency tree must never
    /// up/downgrade headroom-ai's pins in the shared venv (a broken optional
    /// addon must not take down the proxy). Outside runtime/ so in-place and
    /// atomic runtime upgrades can't wipe it.
    pub fn serena_venv_dir(&self) -> PathBuf {
        self.runtime.root_dir.join("serena-venv")
    }

    pub fn serena_entrypoint(&self) -> PathBuf {
        self.serena_entrypoint_in(&self.serena_venv_dir())
    }

    fn serena_entrypoint_in(&self, venv: &Path) -> PathBuf {
        let name = if cfg!(target_os = "windows") {
            "serena.exe"
        } else {
            "serena"
        };
        venv.join(bin_subdir()).join(name)
    }

    pub fn serena_installed(&self) -> bool {
        self.runtime.tools_dir.join("serena.json").exists() && self.serena_entrypoint().exists()
    }

    pub fn serena_enabled(&self) -> bool {
        self.serena_installed() && self.tool_enabled("serena")
    }

    pub fn install_serena(&self) -> Result<()> {
        self.install_serena_version(None)
    }

    pub fn install_serena_version(&self, version: Option<&str>) -> Result<()> {
        let _guard = PACKAGE_UPDATE_LOCK.get_or_init(|| Mutex::new(())).lock();
        let version = version
            .map(stable_package_version)
            .transpose()?
            .unwrap_or(SERENA_PINNED_VERSION);
        if let (Some(current), Some(target)) = (
            self.read_tool_receipt("serena")
                .and_then(|r| r.get("version").and_then(Value::as_str).map(str::to_owned))
                .as_deref(),
            parse_major_minor_patch(version),
        ) {
            if parse_major_minor_patch(current).is_some_and(|current| target < current) {
                bail!("refusing to downgrade Serena from {current} to {version}");
            }
        }
        let staging = self
            .runtime
            .root_dir
            .join(format!("serena-venv.staging-{}", uuid::Uuid::new_v4()));
        if let Err(err) = run_command_with_timeout(
            &self.runtime.standalone_python(),
            &["-m", "venv", "--clear", &staging.to_string_lossy()],
            &self.runtime.root_dir,
            Duration::from_secs(120),
        ) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err).context("creating Serena staging venv");
        }
        let staging_python = staging.join(bin_subdir()).join(python_exe_name());
        if let Err(err) = run_pip_install_with_retries_streaming(
            &staging_python,
            &[
                "-m",
                "pip",
                "install",
                "--timeout",
                "180",
                "--retries",
                "10",
                &format!("serena-agent=={version}"),
            ],
            &self.runtime.root_dir,
            |line| log::info!("serena pip: {line}"),
        ) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err).context("installing Serena into staging venv");
        }
        let staging_entrypoint = self.serena_entrypoint_in(&staging);
        if !staging_entrypoint.exists() {
            let _ = std::fs::remove_dir_all(&staging);
            bail!(
                "serena install completed but {} was not found",
                staging_entrypoint.display()
            );
        }
        if let Err(err) = run_command_with_timeout(
            &staging_entrypoint,
            &["--help"],
            &self.runtime.root_dir,
            SERENA_SMOKE_TEST_TIMEOUT,
        )
        .context("serena installed but failed its smoke test")
        {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        let final_venv = self.serena_venv_dir();
        let backup = self
            .runtime
            .root_dir
            .join(format!("serena-venv.backup-{}", uuid::Uuid::new_v4()));
        let had_old = final_venv.exists();
        if had_old {
            std::fs::rename(&final_venv, &backup).context("staging existing Serena venv")?;
        }
        let activate = || -> Result<()> {
            std::fs::rename(&staging, &final_venv).context("activating Serena venv")?;
            self.register_serena_mcp()?;
            set_serena_global_gitignore(true);
            let enabled = self.tool_enabled("serena");
            self.write_tool_receipt("serena", json!({ "version": version, "enabled": enabled }))?;
            Ok(())
        }();
        if let Err(err) = activate {
            let mut recovery = Vec::new();
            if let Err(restore_err) = std::fs::remove_dir_all(&final_venv) {
                if final_venv.exists() {
                    recovery.push(format!(
                        "failed to remove partial Serena venv: {restore_err}"
                    ));
                }
            }
            if had_old {
                if let Err(restore_err) = std::fs::rename(&backup, &final_venv) {
                    recovery.push(format!("failed to restore Serena venv: {restore_err}"));
                } else {
                    let mcp_result = if self.tool_enabled("serena") {
                        self.register_serena_mcp()
                    } else {
                        self.unregister_serena_mcp()
                    };
                    if let Err(mcp_err) = mcp_result {
                        recovery.push(format!(
                            "failed to restore Serena MCP registration: {mcp_err}"
                        ));
                    }
                }
            } else {
                if let Err(mcp_err) = self.unregister_serena_mcp() {
                    recovery.push(format!(
                        "failed to remove Serena MCP registration: {mcp_err}"
                    ));
                }
                set_serena_global_gitignore(false);
            }
            let _ = std::fs::remove_dir_all(&staging);
            if recovery.is_empty() {
                return Err(err);
            }
            return Err(err.context(format!("recovery also failed: {}", recovery.join("; "))));
        }
        if had_old {
            if let Err(err) = std::fs::remove_dir_all(&backup) {
                log::warn!(
                    "updated Serena, but could not remove backup {}: {err}",
                    backup.display()
                );
            }
        }
        Ok(())
    }

    pub fn set_serena_enabled(&self, enabled: bool) -> Result<()> {
        if !self.serena_installed() {
            bail!("serena is not installed");
        }
        if enabled {
            self.register_serena_mcp()?;
        } else {
            self.unregister_serena_mcp()?;
        }
        let stored_version = self.read_tool_receipt("serena").and_then(|receipt| {
            receipt
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let version = stored_version.as_deref().unwrap_or(SERENA_PINNED_VERSION);
        self.write_tool_receipt("serena", json!({ "version": version, "enabled": enabled }))?;
        Ok(())
    }

    /// Reconcile an already-enabled install on app launch. This is what lets a
    /// Community upgrade safely replace a stale official-Headroom registration
    /// after the legacy ownership ledger proves that entry was Headroom-managed.
    pub fn ensure_serena_configured(&self) -> Result<()> {
        if !self.serena_enabled() {
            return Ok(());
        }
        self.register_serena_mcp()
    }

    pub fn uninstall_serena(&self) -> Result<()> {
        // Unregister before deleting the venv, and fail if it doesn't land: a
        // leftover MCP entry pointing at a deleted binary would make every new
        // agent session spawn a failing server.
        self.unregister_serena_mcp()?;
        set_serena_global_gitignore(false);
        let venv = self.serena_venv_dir();
        if venv.exists() {
            // Retrying helper, not a bare remove_dir_all: it clears read-only
            // bits across the tree, which is the half of Windows "Access is
            // denied" we can actually fix (Sentry RUST-6T).
            crate::client_adapters::remove_dir_all_retry(&venv).map_err(|err| {
                // The other half is an open handle -- an agent session still
                // running serena's MCP server out of this venv. We will not kill
                // a user's editor to win a delete, so name the cause instead: a
                // bare "Access is denied" leaves them with nothing to act on.
                if err.kind() == std::io::ErrorKind::PermissionDenied {
                    anyhow!(
                        "removing {} was denied. Serena may still be running as an MCP server \
                         in an open Claude Code or Codex session -- close those and uninstall \
                         again. Underlying error: {err}",
                        venv.display()
                    )
                } else {
                    anyhow::Error::new(err).context(format!("removing {}", venv.display()))
                }
            })?;
        }
        let receipt = self.runtime.tools_dir.join("serena.json");
        if receipt.exists() {
            std::fs::remove_file(&receipt)
                .with_context(|| format!("removing {}", receipt.display()))?;
        }
        Ok(())
    }

    fn register_serena_mcp(&self) -> Result<()> {
        set_serena_browser_dashboard();
        let entrypoint = self.serena_entrypoint().to_string_lossy().into_owned();
        let legacy_ledger = legacy_headroom_mcp_ledger_path()
            .to_string_lossy()
            .into_owned();
        let result = self.run_mcp_helper(&[
            "-c",
            SERENA_MCP_HELPER,
            "register",
            &entrypoint,
            &legacy_ledger,
        ]);
        result.context("registering serena MCP server")
    }

    fn unregister_serena_mcp(&self) -> Result<()> {
        self.run_mcp_helper(&["-c", SERENA_MCP_HELPER, "unregister"])
            .context("unregistering serena MCP server")
    }

    pub fn context7_installed(&self) -> bool {
        self.runtime.tools_dir.join("context7.json").exists()
    }

    /// No managed venv or binary: the agent runs the pinned package through
    /// its own `npx`. Install just proves that works once (warming the npx
    /// cache), then registers the MCP entry — a broken entry would make every
    /// new agent session spawn a failing server.
    pub fn install_context7(&self) -> Result<()> {
        self.install_context7_version(None)
    }

    pub fn install_context7_version(&self, version: Option<&str>) -> Result<()> {
        let _guard = PACKAGE_UPDATE_LOCK.get_or_init(|| Mutex::new(())).lock();
        let version = version
            .map(stable_package_version)
            .transpose()?
            .unwrap_or(CONTEXT7_PINNED_VERSION);
        if let Some(current) = self
            .read_tool_receipt("context7")
            .and_then(|r| r.get("version").and_then(Value::as_str).map(str::to_owned))
            .as_deref()
            .and_then(parse_major_minor_patch)
        {
            if parse_major_minor_patch(version).is_some_and(|target| target < current) {
                bail!("refusing to downgrade Context7 from {current:?} to {version}");
            }
        }
        let npx = crate::claude_cli::detect_npx().context(
            "npx was not found. Context7 runs through Node.js -- install Node.js, then try again.",
        )?;
        run_command_with_timeout(
            &npx,
            &["-y", &context7_package_spec_for(version), "--help"],
            &self.runtime.root_dir,
            CONTEXT7_INSTALL_TIMEOUT,
        )
        // ponytail: no separate Node version probe. Context7 4.x declares
        // node >=20.18.1 but npm only warns on an engines mismatch, so the
        // --help run above is what actually proves this Node can run it.
        .context(
            "context7 failed its smoke test (npx download or startup). Context7 needs Node.js 20.18.1 or newer",
        )?;
        let previous = self.read_tool_receipt("context7");
        self.register_context7_mcp(version)?;
        let enabled = self.tool_enabled("context7");
        if let Err(err) = self.write_tool_receipt(
            "context7",
            json!({ "version": version, "enabled": enabled }),
        ) {
            let restore = previous
                .as_ref()
                .and_then(|r| r.get("version").and_then(Value::as_str))
                .map(|old| self.register_context7_mcp(old))
                .unwrap_or_else(|| self.unregister_context7_mcp());
            if let Err(restore_err) = restore {
                return Err(err.context(format!(
                    "writing Context7 receipt failed; restoring previous MCP registration also failed: {restore_err}"
                )));
            }
            return Err(err).context("writing Context7 receipt");
        }
        Ok(())
    }

    pub fn set_context7_enabled(&self, enabled: bool) -> Result<()> {
        if !self.context7_installed() {
            bail!("context7 is not installed");
        }
        if enabled {
            let stored_version = self.read_tool_receipt("context7").and_then(|receipt| {
                receipt
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            let version = stored_version.as_deref().unwrap_or(CONTEXT7_PINNED_VERSION);
            self.register_context7_mcp(version)?;
        } else {
            self.unregister_context7_mcp()?;
        }
        let stored_version = self.read_tool_receipt("context7").and_then(|receipt| {
            receipt
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let version = stored_version.as_deref().unwrap_or(CONTEXT7_PINNED_VERSION);
        self.write_tool_receipt(
            "context7",
            json!({ "version": version, "enabled": enabled }),
        )?;
        Ok(())
    }

    pub fn uninstall_context7(&self) -> Result<()> {
        self.unregister_context7_mcp()?;
        let receipt = self.runtime.tools_dir.join("context7.json");
        if receipt.exists() {
            std::fs::remove_file(&receipt)
                .with_context(|| format!("removing {}", receipt.display()))?;
        }
        Ok(())
    }

    fn register_context7_mcp(&self, version: &str) -> Result<()> {
        let legacy_ledger = legacy_headroom_mcp_ledger_path()
            .to_string_lossy()
            .into_owned();
        self.run_mcp_helper(&[
            "-c",
            CONTEXT7_MCP_HELPER,
            "register",
            &context7_package_spec_for(version),
            &legacy_ledger,
        ])
        .context("registering context7 MCP server")
    }

    fn unregister_context7_mcp(&self) -> Result<()> {
        self.run_mcp_helper(&["-c", CONTEXT7_MCP_HELPER, "unregister"])
            .context("unregistering context7 MCP server")
    }

    pub fn codebase_memory_entrypoint(&self) -> PathBuf {
        self.runtime.bin_dir.join("codebase-memory-mcp")
    }

    /// Index databases live here (via `CBM_CACHE_DIR`) instead of the
    /// binary's default `~/.cache/codebase-memory-mcp`, so uninstalling the
    /// addon or Headroom removes them too.
    fn codebase_memory_cache_dir(&self) -> PathBuf {
        self.runtime.tools_dir.join("codebase-memory-cache")
    }

    fn codebase_memory_pending_marker_path(&self) -> PathBuf {
        self.runtime.tools_dir.join("codebase-memory.pending.json")
    }

    pub(crate) fn recover_codebase_memory_pending(&self) -> Result<()> {
        let marker = self.codebase_memory_pending_marker_path();
        if !marker.exists() {
            return Ok(());
        }
        let pending: CodebaseMemoryPending = serde_json::from_slice(
            &std::fs::read(&marker).with_context(|| format!("reading {}", marker.display()))?,
        )
        .with_context(|| format!("parsing {}", marker.display()))?;
        let had_destination = pending.had_destination.ok_or_else(|| {
            anyhow!(
                "{} is missing required hadDestination rollback flag; refusing to guess",
                marker.display()
            )
        })?;
        let had_receipt = pending.had_receipt.ok_or_else(|| {
            anyhow!(
                "{} is missing required hadReceipt rollback flag; refusing to guess",
                marker.display()
            )
        })?;
        let backup = self.runtime.bin_dir.join("codebase-memory-mcp.backup");
        let destination = self.codebase_memory_entrypoint();
        let receipt = self.runtime.tools_dir.join("codebase-memory.json");
        let receipt_backup = self.runtime.tools_dir.join("codebase-memory.json.backup");
        let rollback = (|| -> Result<()> {
            if had_destination {
                if backup.exists() {
                    if destination.exists() {
                        std::fs::remove_file(&destination).with_context(|| {
                            format!("removing incomplete {}", destination.display())
                        })?;
                    }
                    std::fs::rename(&backup, &destination)
                        .with_context(|| format!("restoring {}", destination.display()))?;
                } else if !destination.exists() {
                    bail!("codebase-memory rollback is missing its binary backup");
                }
            } else if destination.exists() {
                std::fs::remove_file(&destination)
                    .with_context(|| format!("removing incomplete {}", destination.display()))?;
            }

            if had_receipt {
                if receipt_backup.exists() {
                    if receipt.exists() {
                        std::fs::remove_file(&receipt).with_context(|| {
                            format!("removing incomplete {}", receipt.display())
                        })?;
                    }
                    std::fs::rename(&receipt_backup, &receipt)
                        .with_context(|| format!("restoring {}", receipt.display()))?;
                } else if !receipt.exists() {
                    bail!("codebase-memory rollback is missing its receipt backup");
                }
            } else if receipt.exists() {
                std::fs::remove_file(&receipt)
                    .with_context(|| format!("removing incomplete {}", receipt.display()))?;
            }

            if self.codebase_memory_installed() && self.tool_enabled("codebase-memory") {
                self.register_codebase_memory_mcp()?;
            } else {
                self.unregister_codebase_memory_mcp()?;
            }
            Ok(())
        })();
        match rollback {
            Ok(()) => {
                std::fs::remove_file(&marker)
                    .with_context(|| format!("removing {}", marker.display()))?;
                Ok(())
            }
            Err(err) => Err(err.context(format!(
                "recovering interrupted codebase-memory update; marker retained at {}",
                marker.display()
            ))),
        }
    }

    pub fn codebase_memory_installed(&self) -> bool {
        self.runtime.tools_dir.join("codebase-memory.json").exists()
            && self.codebase_memory_entrypoint().exists()
    }

    pub fn install_codebase_memory(&self) -> Result<()> {
        self.install_codebase_memory_version(None)
    }

    pub fn install_codebase_memory_version(&self, version: Option<&str>) -> Result<()> {
        let _guard = CODEBASE_MEMORY_INSTALL_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock();
        self.recover_codebase_memory_pending()?;
        let artifact = match version {
            None => codebase_memory_distribution_artifact()?,
            Some(version) => fetch_codebase_memory_release(version)?,
        };
        let target_version = version.unwrap_or(CODEBASE_MEMORY_VERSION);
        if !valid_codebase_memory_version(target_version) {
            bail!("invalid codebase-memory version: {target_version}");
        }
        if let Some(current) = self
            .read_tool_receipt("codebase-memory")
            .and_then(|receipt| {
                receipt
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        {
            if let (Some(current), Some(target)) = (
                parse_major_minor_patch(&current),
                parse_major_minor_patch(target_version),
            ) {
                if target < current {
                    bail!("refusing to downgrade codebase-memory from {current:?} to {target_version}");
                }
            }
        }
        let archive_path = self.runtime.downloads_dir.join(format!(
            "codebase-memory-mcp-v{}-{}-{}.tar.gz",
            target_version,
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        download_to_path(&artifact.url, &archive_path, artifact.sha256)?;

        let extract_dir = self.runtime.downloads_dir.join("codebase-memory-extract");
        if extract_dir.exists() {
            std::fs::remove_dir_all(&extract_dir)
                .with_context(|| format!("removing {}", extract_dir.display()))?;
        }
        std::fs::create_dir_all(&extract_dir)
            .with_context(|| format!("creating {}", extract_dir.display()))?;

        let file = std::fs::File::open(&archive_path)
            .with_context(|| format!("opening {}", archive_path.display()))?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);
        archive
            .unpack(&extract_dir)
            .with_context(|| format!("extracting into {}", extract_dir.display()))?;

        let extracted_binary = extract_dir.join("codebase-memory-mcp");
        if !extracted_binary.exists() {
            bail!(
                "codebase-memory extraction completed but {} was not found",
                extracted_binary.display()
            );
        }

        // Stage then rename: live agent sessions may be running the old
        // binary as an MCP server, and copying over it risks ETXTBSY /
        // truncated-exec. Rename is atomic.
        let destination = self.codebase_memory_entrypoint();
        let staged = {
            let mut s = destination.as_os_str().to_os_string();
            s.push(format!(".new.{}", uuid::Uuid::new_v4()));
            PathBuf::from(s)
        };
        std::fs::rename(&extracted_binary, &staged)
            .with_context(|| format!("staging {}", staged.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&staged)
                .with_context(|| format!("reading permissions of {}", staged.display()))?
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&staged, permissions)
                .with_context(|| format!("marking {} executable", staged.display()))?;
        }
        // Validate the staged binary first. If an upstream artifact is
        // incompatible, the currently installed MCP executable stays intact.
        run_command_with_timeout(
            &staged,
            &["--version"],
            &self.runtime.root_dir,
            Duration::from_secs(15),
        )
        .context("codebase-memory update failed its smoke test")?;
        let reported = Command::new(&staged)
            .arg("--version")
            .current_dir(&self.runtime.root_dir)
            .output()
            .context("reading staged codebase-memory version")?;
        let stdout = String::from_utf8_lossy(&reported.stdout);
        let stderr = String::from_utf8_lossy(&reported.stderr);
        if !rtk_version_output_matches(&stdout, target_version)
            && !rtk_version_output_matches(&stderr, target_version)
        {
            bail!(
                "codebase-memory staged binary reported stdout={stdout:?} stderr={stderr:?}, expected {target_version}"
            );
        }

        let marker = self.codebase_memory_pending_marker_path();
        let backup = self.runtime.bin_dir.join("codebase-memory-mcp.backup");
        let receipt = self.runtime.tools_dir.join("codebase-memory.json");
        let receipt_backup = self.runtime.tools_dir.join("codebase-memory.json.backup");
        if backup.exists() || receipt_backup.exists() {
            bail!(
                "stale codebase-memory update backup exists without a pending marker; refusing to overwrite recovery data"
            );
        }
        let had_destination = destination.exists();
        let had_receipt = receipt.exists();
        let enabled = self.tool_enabled("codebase-memory");
        crate::client_adapters::atomic_write(
            &marker,
            &serde_json::to_vec_pretty(&json!({
                "version": target_version,
                "hadDestination": had_destination,
                "hadReceipt": had_receipt,
            }))
            .context("serializing codebase-memory pending marker")?,
        )
        .with_context(|| format!("writing {}", marker.display()))?;

        let commit = (|| -> Result<()> {
            if had_receipt {
                std::fs::rename(&receipt, &receipt_backup)
                    .with_context(|| format!("backing up {}", receipt.display()))?;
            }
            if had_destination {
                std::fs::rename(&destination, &backup)
                    .with_context(|| format!("backing up {}", destination.display()))?;
            }
            std::fs::rename(&staged, &destination)
                .with_context(|| format!("installing {}", destination.display()))?;
            std::fs::create_dir_all(self.codebase_memory_cache_dir()).with_context(|| {
                format!("creating {}", self.codebase_memory_cache_dir().display())
            })?;
            if enabled {
                self.register_codebase_memory_mcp()?;
            } else {
                self.unregister_codebase_memory_mcp()?;
            }
            self.write_tool_receipt(
                "codebase-memory",
                json!({ "version": target_version, "enabled": enabled }),
            )?;
            Ok(())
        })();

        if let Err(err) = commit {
            // The cause chain is what makes this actionable; without it the
            // user (and support) only ever sees the sentence below. Log it
            // before the rollback swallows the error into a context string.
            log::error!("codebase-memory install/update failed: {err:#}");
            // A first install has nothing to restore, so promising the user
            // their "previous installation" came back is a lie they cannot
            // check.
            let restored = if had_destination || had_receipt {
                "codebase-memory update failed; restored previous installation"
            } else {
                "codebase-memory install failed; nothing was installed"
            };
            return match self.recover_codebase_memory_pending() {
                Ok(()) => Err(err.context(restored)),
                Err(rollback_err) => Err(anyhow!(
                    "codebase-memory update failed: {err:#}; rollback also failed: {rollback_err:#}"
                )),
            };
        }

        if let Err(err) = std::fs::remove_file(&marker) {
            return match self.recover_codebase_memory_pending() {
                Ok(()) => Err(anyhow!(
                    "committing codebase-memory update failed while removing {}: {err}; restored previous installation",
                    marker.display()
                )),
                Err(rollback_err) => Err(anyhow!(
                    "committing codebase-memory update failed while removing {}: {err}; rollback also failed: {rollback_err:#}",
                    marker.display()
                )),
            };
        }
        for old in [&backup, &receipt_backup] {
            if old.exists() {
                if let Err(err) = std::fs::remove_file(old) {
                    log::warn!(
                        "could not remove committed codebase-memory backup {}: {err}",
                        old.display()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn set_codebase_memory_enabled(&self, enabled: bool) -> Result<()> {
        if !self.codebase_memory_installed() {
            bail!("codebase-memory is not installed");
        }
        let previous = self.read_tool_receipt("codebase-memory");
        let version = previous
            .as_ref()
            .and_then(|receipt| {
                receipt
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| CODEBASE_MEMORY_VERSION.to_owned());
        let previous_enabled = previous
            .as_ref()
            .and_then(|receipt| receipt.get("enabled").and_then(Value::as_bool))
            .unwrap_or(false);
        if enabled {
            self.register_codebase_memory_mcp()?;
        } else {
            self.unregister_codebase_memory_mcp()?;
        }
        if let Err(err) = self.write_tool_receipt(
            "codebase-memory",
            json!({ "version": version, "enabled": enabled }),
        ) {
            let restore = if previous_enabled {
                self.register_codebase_memory_mcp()
            } else {
                self.unregister_codebase_memory_mcp()
            };
            return match restore {
                Ok(()) => Err(err.context("writing codebase-memory receipt; previous MCP state restored")),
                Err(restore_err) => Err(anyhow!(
                    "writing codebase-memory receipt failed: {err:#}; restoring previous MCP state also failed: {restore_err:#}"
                )),
            };
        }
        Ok(())
    }

    pub fn uninstall_codebase_memory(&self) -> Result<()> {
        // Unregister first: a leftover MCP entry pointing at a deleted binary
        // would make every new agent session spawn a failing server.
        self.unregister_codebase_memory_mcp()?;
        let binary = self.codebase_memory_entrypoint();
        if binary.exists() {
            std::fs::remove_file(&binary)
                .with_context(|| format!("removing {}", binary.display()))?;
        }
        let cache = self.codebase_memory_cache_dir();
        if cache.exists() {
            std::fs::remove_dir_all(&cache)
                .with_context(|| format!("removing {}", cache.display()))?;
        }
        let receipt = self.runtime.tools_dir.join("codebase-memory.json");
        if receipt.exists() {
            std::fs::remove_file(&receipt)
                .with_context(|| format!("removing {}", receipt.display()))?;
        }
        Ok(())
    }

    fn register_codebase_memory_mcp(&self) -> Result<()> {
        let binary = self
            .codebase_memory_entrypoint()
            .to_string_lossy()
            .into_owned();
        let cache_dir = self
            .codebase_memory_cache_dir()
            .to_string_lossy()
            .into_owned();
        let legacy_ledger = legacy_headroom_mcp_ledger_path()
            .to_string_lossy()
            .into_owned();
        self.run_mcp_helper(&[
            "-c",
            CODEBASE_MEMORY_MCP_HELPER,
            "register",
            &binary,
            &cache_dir,
            &legacy_ledger,
        ])
        .context("registering codebase-memory MCP server")
    }

    fn unregister_codebase_memory_mcp(&self) -> Result<()> {
        self.run_mcp_helper(&["-c", CODEBASE_MEMORY_MCP_HELPER, "unregister"])
            .context("unregistering codebase-memory MCP server")
    }

    fn run_mcp_helper(&self, args: &[&str]) -> Result<()> {
        // ClaudeRegistrar may shell out to the `claude` CLI, which can take a
        // few seconds per agent; 60s covers both registrars comfortably.
        run_command_with_timeout(
            &self.managed_python(),
            args,
            &self.runtime.root_dir,
            Duration::from_secs(60),
        )
    }

    /// Remove the managed rtk binary and its receipt. Shell PATH and Claude Code
    /// hook teardown is handled separately by `client_adapters::set_rtk_enabled`.
    pub fn uninstall_rtk(&self) -> Result<()> {
        let binary = self.rtk_entrypoint();
        if binary.exists() {
            std::fs::remove_file(&binary)
                .with_context(|| format!("removing {}", binary.display()))?;
        }
        let receipt = self.runtime.tools_dir.join("rtk.json");
        if receipt.exists() {
            std::fs::remove_file(&receipt)
                .with_context(|| format!("removing {}", receipt.display()))?;
        }
        Ok(())
    }

    /// A plugin install is genuine only when our receipt exists AND at least
    /// one host (Claude Code or Codex) still has the plugin registered, so a
    /// user who removes it via `/plugin` doesn't leave the card stuck on
    /// "Enabled".
    fn plugin_installed(&self, id: &str) -> bool {
        let Some(plugin) = plugin_addon(id) else {
            return false;
        };
        self.plugin_receipt_exists(plugin)
            && plugin.hosts.iter().any(|host| host.plugin_present(plugin))
    }

    /// Persists the user-level default only. Plugin environment variables and
    /// repository-local configuration can still take precedence, and running
    /// agent sessions keep the mode they already resolved.
    pub fn set_addon_mode(&self, id: &str, mode: &str) -> Result<()> {
        let spec = addon_mode_spec(id).ok_or_else(|| anyhow!("unknown addon mode: {id}"))?;
        canonical_addon_mode(spec, mode).ok_or_else(|| {
            anyhow!(
                "unsupported {} default mode {mode:?}; supported modes: {}",
                spec.id,
                spec.supported_modes.join(", ")
            )
        })?;
        if !self.plugin_installed(id) {
            bail!("{id} plugin is not installed");
        }
        if !self.tool_enabled(id) {
            bail!("{id} plugin is disabled");
        }

        let config_path = addon_user_config_path(id)?;
        write_addon_default_mode_to_path(&config_path, spec, mode)
    }

    fn plugin_receipt_exists(&self, plugin: &PluginAddon) -> bool {
        self.read_plugin_receipt(plugin).ok().flatten().is_some()
    }

    fn conflicting_plugin_ids(&self, id: &str) -> Vec<String> {
        let Some(group) = self
            .manifests
            .iter()
            .find(|manifest| manifest.id == id)
            .and_then(|manifest| manifest.workflow_group.as_deref())
        else {
            return Vec::new();
        };
        self.manifests
            .iter()
            .filter(|manifest| {
                manifest.id != id
                    && manifest.workflow_group.as_deref() == Some(group)
                    && plugin_addon(&manifest.id).is_some()
            })
            .map(|manifest| manifest.id.clone())
            .collect()
    }

    fn enforce_exclusive_plugin_group(&self, id: &str) -> Result<()> {
        let peers: Vec<String> = self
            .conflicting_plugin_ids(id)
            .into_iter()
            .filter(|peer| {
                plugin_addon(peer).is_some_and(|plugin| self.plugin_receipt_exists(plugin))
                    && self.tool_enabled(peer)
            })
            .collect();
        let mut disabled: Vec<String> = Vec::new();
        for peer in peers {
            if let Err(err) = self.set_plugin_enabled_inner(&peer, false) {
                let mut rollback_errors = Vec::new();
                if let Err(rollback_err) = self.set_plugin_enabled_inner(id, false) {
                    rollback_errors.push(format!("disabling {id}: {rollback_err:#}"));
                }
                for restored in disabled.iter().rev() {
                    if let Err(rollback_err) = self.set_plugin_enabled_inner(restored, true) {
                        rollback_errors.push(format!("restoring {restored}: {rollback_err:#}"));
                    }
                }
                let rollback = if rollback_errors.is_empty() {
                    String::new()
                } else {
                    format!("; rollback issues: {}", rollback_errors.join("; "))
                };
                bail!("could not disable conflicting workflow {peer}: {err:#}{rollback}");
            }
            disabled.push(peer);
        }
        Ok(())
    }

    fn run_plugin_cmd(
        &self,
        plugin: &PluginAddon,
        cli: &Path,
        host: PluginHost,
        args: &[&str],
    ) -> Result<()> {
        let id = plugin.id;
        let label = host.label();
        let result = run_command_streaming(cli, args, &self.runtime.root_dir, &mut |line: &str| {
            log::info!("{id} [{label}]: {line}")
        });
        if result.is_ok() && matches!(host, PluginHost::Codex) {
            // The host command has already committed its registry change. A
            // cache normalization failure is a post-action maintenance issue,
            // not evidence that the CLI command failed. Returning it as the
            // command error makes callers roll back a successful install or
            // uninstall and can leave an orphaned Codex registration. Keep
            // the successful host result, log the degraded cache state, and
            // retry normalization on the next plugin operation.
            if let Err(err) = normalize_codex_plugin_cache(plugin) {
                log::warn!(
                    "{id} Codex command succeeded but plugin-cache normalization was deferred: {err:#}"
                );
            }
        }
        result
    }

    /// Registers the marketplace (best-effort) and installs the plugin into a
    /// single host. Used for first install, recovery of a missing registration,
    /// and Update. Codex enable/disable normally edits its persistent `enabled`
    /// flag without refreshing or removing the installation.
    ///
    /// Already present on this host means this is an Update: both hosts install
    /// from a local marketplace checkout, so the snapshot has to be refreshed
    /// first or the "update" reinstalls the same commit. Plain `install`/`add`
    /// on an installed plugin is a no-op, which is why Update cannot just be
    /// the install path replayed.
    fn install_plugin_into(&self, plugin: &'static PluginAddon, host: PluginHost) -> Result<()> {
        let cli = host.cli().context("CLI not found on PATH")?;
        if uses_codex_plugin_adapter(plugin, host) {
            self.install_codex_adapter_plugin(plugin, &cli)?;
            set_codex_plugin_enabled(plugin, true)?;
            return Ok(());
        }
        let plugin_present = host.plugin_present_checked(plugin)?;
        if plugin_present {
            if !plugin_uses_existing_marketplace(plugin) {
                self.run_plugin_cmd(plugin, &cli, host, &host.marketplace_update_args(plugin))
                    .with_context(|| {
                        format!(
                            "refreshing {} marketplace before update",
                            plugin.marketplace_name
                        )
                    })?;
                host.prepare_marketplace_snapshot(plugin)?;
            }
            self.run_plugin_cmd(plugin, &cli, host, &host.update_args(plugin))
                .map_err(|err| explain_host_marketplace_missing_plugin(plugin, err))?;
        } else {
            // Superpowers ships in the host's own openai-curated marketplace,
            // which Codex materializes the first time the app starts, so there
            // is no add step for it. Every other addon
            // must successfully add/refresh its marketplace before install;
            // continuing after an add failure would let the host resolve a
            // stale or user-managed checkout and silently install the wrong
            // source.
            if !plugin_uses_existing_marketplace(plugin) {
                self.run_plugin_cmd(plugin, &cli, host, &host.marketplace_add_args(plugin))
                    .with_context(|| {
                        format!(
                            "adding {} marketplace before installing {}",
                            plugin.marketplace_name, plugin.id
                        )
                    })?;
                host.prepare_marketplace_snapshot(plugin)?;
            }
            self.run_plugin_cmd(plugin, &cli, host, &host.install_args(plugin))
                .map_err(|err| explain_host_marketplace_missing_plugin(plugin, err))?;
        }
        if !host.plugin_present_checked(plugin)? {
            bail!("install completed but the plugin was not registered");
        }
        if matches!(host, PluginHost::Codex) {
            // `plugin add` may preserve an existing disabled flag. Install and
            // Update are explicit enable actions in Headroom, so make the
            // resulting Codex state unambiguous without another CLI refresh.
            set_codex_plugin_enabled(plugin, true)?;
        }
        Ok(())
    }

    fn install_codex_adapter_plugin(&self, plugin: &'static PluginAddon, cli: &Path) -> Result<()> {
        let adapter_source = crate::client_adapters::codex_home()
            .join(".tmp")
            .join("marketplaces")
            .join(plugin.marketplace_name);
        let registration =
            codex_marketplace_registration(cli, &self.runtime.root_dir, plugin.marketplace_name)?;
        if let CodexMarketplaceRegistration::Local(path) = &registration {
            if !paths_refer_to_same_location(path, &adapter_source) {
                bail!(
                    "refusing to replace user-managed Codex marketplace {} at {}",
                    plugin.marketplace_name,
                    path.display()
                );
            }
        } else if matches!(registration, CodexMarketplaceRegistration::Other(_)) {
            bail!(
                "refusing to replace user-managed Codex marketplace {}",
                plugin.marketplace_name
            );
        }

        let original_git_source = match &registration {
            CodexMarketplaceRegistration::Git(source) if !source.is_empty() => Some(source.clone()),
            _ => None,
        };
        let recovery_marker_preexisting = self.read_codex_marketplace_recovery(plugin)?.is_some();
        let migration_decision = codex_adapter_source_decision(
            &adapter_source,
            &adapter_source.join(".headroom-local-community-adapter.json"),
        );
        let mut pending_marker_created = false;
        if let CodexAdapterSourceDecision::MigrateGit { backup } = &migration_decision {
            if !matches!(registration, CodexMarketplaceRegistration::Git(_)) {
                bail!(
                    "{} is an unregistered Git checkout; refusing to move user-managed Codex marketplace",
                    adapter_source.display()
                );
            }
            if backup.exists() {
                bail!(
                    "refusing to overwrite recoverable Codex marketplace backup {}",
                    backup.display()
                );
            }
            if recovery_marker_preexisting {
                bail!(
                    "{} has a recovery marker but no managed adapter marker; refusing to migrate it again",
                    adapter_source.display()
                );
            }
            // Publish the recovery boundary before moving the checkout. If the
            // process dies between the rename and the phase update, the
            // marker still points at the deterministic backup and recovery can
            // restore it safely.
            self.write_codex_marketplace_recovery(
                plugin,
                &CodexMarketplaceRecovery {
                    backup_path: backup.clone(),
                    original_git_source: original_git_source.clone(),
                    moved: false,
                },
            )?;
            pending_marker_created = true;
        }
        let migrated_backup = match migrate_unmanaged_codex_marketplace(plugin, &registration) {
            Ok(backup) => backup,
            Err(err) => {
                if pending_marker_created {
                    match self.clear_unmoved_codex_marketplace_recovery_if_safe(
                        plugin,
                        &registration,
                    ) {
                        Ok(true) => log::warn!(
                            "Codex marketplace migration failed before moving the checkout; cleared the pending recovery marker"
                        ),
                        Ok(false) => log::warn!(
                            "Codex marketplace migration failed after its recovery marker was published; retaining the marker because the checkout state is ambiguous"
                        ),
                        Err(clear_err) => {
                            return Err(merge_codex_marketplace_recovery_error(
                                err,
                                Err(clear_err),
                            ));
                        }
                    }
                }
                return Err(err);
            }
        };
        if pending_marker_created && migrated_backup.is_none() {
            // The preflight saw an unmanaged Git checkout, but the migration
            // helper no longer did. Another process changed the marketplace
            // between those two observations. Never continue with a
            // moved=false marker that has no backup: it would make a later
            // uninstall infer a recovery that did not happen. Clear the
            // marker only when the original checkout is still present,
            // unmarked, and proves the same registered Git source. If the
            // path disappeared or changed shape, retain the marker as an
            // unresolved recovery boundary for a safe retry.
            let marker_cleared =
                self.clear_unmoved_codex_marketplace_recovery_if_safe(plugin, &registration)?;
            if marker_cleared {
                bail!(
                    "Codex marketplace {} changed while its recovery marker was being created; migration did not occur and the pending marker was cleared",
                    plugin.marketplace_name
                );
            }
            bail!(
                "Codex marketplace {} changed while its recovery marker was being created; refusing to continue with no migration backup (pending marker retained for recovery)",
                plugin.marketplace_name
            );
        }
        let registration_removed = matches!(registration, CodexMarketplaceRegistration::Git(_));
        let replacement_registration_attempted = std::cell::Cell::new(false);
        let restore_migrated = || -> Result<()> {
            let mut failures = Vec::new();
            if registration_removed
                || replacement_registration_attempted.get()
                || migrated_backup.is_some()
            {
                if registration_removed || replacement_registration_attempted.get() {
                    if let Err(err) = self.run_plugin_cmd(
                        plugin,
                        cli,
                        PluginHost::Codex,
                        &PluginHost::Codex.marketplace_remove_args(plugin),
                    ) {
                        failures.push(format!(
                            "removing replacement marketplace registration: {err:#}"
                        ));
                    }
                }
                if let Some(backup) = migrated_backup.as_deref() {
                    if let Err(err) = restore_codex_marketplace_backup(backup, &adapter_source) {
                        failures.push(format!(
                            "restoring Codex marketplace backup {}: {err:#}",
                            backup.display()
                        ));
                    }
                }
                if let Some(source) = original_git_source.as_deref() {
                    if let Err(err) = self.run_plugin_cmd(
                        plugin,
                        cli,
                        PluginHost::Codex,
                        &["plugin", "marketplace", "add", source],
                    ) {
                        failures.push(format!(
                            "re-registering original Codex marketplace {source}: {err:#}"
                        ));
                    }
                }
            }
            // If this invocation created the recovery marker, remove it only
            // after the original checkout and registration are both restored.
            // A pre-existing marker belongs to an already-installed adapter and
            // must survive an update failure for a later uninstall retry.
            if failures.is_empty() && migrated_backup.is_some() && !recovery_marker_preexisting {
                if let Err(err) = self.clear_codex_marketplace_recovery(plugin) {
                    failures.push(format!(
                        "clearing Codex marketplace recovery marker: {err:#}"
                    ));
                }
            }
            if failures.is_empty() {
                Ok(())
            } else {
                bail!(failures.join("; "))
            }
        };
        if let Some(backup) = migrated_backup.as_ref() {
            let recovery = CodexMarketplaceRecovery {
                backup_path: backup.clone(),
                original_git_source: original_git_source.clone(),
                moved: true,
            };
            if let Err(err) = self.write_codex_marketplace_recovery(plugin, &recovery) {
                // The checkout has already moved. Try to put it back before
                // returning; if that fails, the marker (when it was written)
                // remains as the explicit retry boundary.
                let recovery_err = restore_migrated();
                return Err(merge_codex_marketplace_recovery_error(
                    err.context("recording Codex marketplace recovery metadata"),
                    recovery_err,
                ));
            }
        }
        if registration_removed {
            if let Err(err) = self.run_plugin_cmd(
                plugin,
                cli,
                PluginHost::Codex,
                &PluginHost::Codex.marketplace_remove_args(plugin),
            ) {
                let err = err.context(format!(
                    "removing existing Codex marketplace {}",
                    plugin.marketplace_name
                ));
                return Err(merge_codex_marketplace_recovery_error(
                    err,
                    restore_migrated(),
                ));
            }
        }
        let source = match prepare_codex_adapter_marketplace(plugin) {
            Ok(source) => source,
            Err(err) => {
                return Err(merge_codex_marketplace_recovery_error(
                    err,
                    restore_migrated(),
                ));
            }
        };
        let source_text = source.to_string_lossy().into_owned();
        replacement_registration_attempted.set(true);
        let marketplace_err = self
            .run_plugin_cmd(
                plugin,
                cli,
                PluginHost::Codex,
                &["plugin", "marketplace", "add", &source_text],
            )
            .err();
        let marketplace_registration = match codex_marketplace_registration(
            cli,
            &self.runtime.root_dir,
            plugin.marketplace_name,
        ) {
            Ok(value) => value,
            Err(err) => {
                return Err(merge_codex_marketplace_recovery_error(
                    err,
                    restore_migrated(),
                ));
            }
        };
        let marketplace_is_local = matches!(
            marketplace_registration,
            CodexMarketplaceRegistration::Local(path)
                if paths_refer_to_same_location(&path, &source)
        );
        if !marketplace_is_local {
            let err = anyhow!(
                "Codex marketplace {} was not registered as the expected local marketplace",
                plugin.marketplace_name
            );
            return Err(merge_codex_marketplace_recovery_error(
                err,
                restore_migrated(),
            ));
        }
        let install_result = self
            .run_plugin_cmd(
                plugin,
                cli,
                PluginHost::Codex,
                &PluginHost::Codex.install_args(plugin),
            )
            .map_err(|err| match marketplace_err {
                Some(add_err) => {
                    err.context(format!("local marketplace add failed first: {add_err:#}"))
                }
                None => err,
            });
        if let Err(err) = install_result {
            return Err(merge_codex_marketplace_recovery_error(
                err,
                restore_migrated(),
            ));
        }
        let codex_registered = match PluginHost::Codex.plugin_present_checked(plugin) {
            Ok(registered) => registered,
            Err(err) => {
                return Err(merge_codex_marketplace_recovery_error(
                    err.context("checking Codex plugin registration after install"),
                    restore_migrated(),
                ));
            }
        };
        if !codex_registered {
            return Err(merge_codex_marketplace_recovery_error(
                anyhow!("install completed but the Codex adapter plugin was not registered"),
                restore_migrated(),
            ));
        }
        Ok(())
    }

    /// Installs a plugin addon into every host that has a CLI on PATH. Returns
    /// `Ok(true)` when at least one host succeeded but Codex was skipped because
    /// it is too old to support `plugin add` -- the caller nudges the user to
    /// update Codex. A too-old Codex is not a real error (no Sentry warning); it
    /// is a version skew the user can only fix by updating Codex.
    pub fn install_plugin(&self, id: &str) -> Result<bool> {
        // Serialize plugin registry, marketplace, receipt, and runtime
        // mutations so concurrent addon operations cannot lose updates.
        let _guard = PACKAGE_UPDATE_LOCK.get_or_init(|| Mutex::new(())).lock();
        let plugin = plugin_addon(id).with_context(|| format!("unknown plugin addon: {id}"))?;
        // A host registration without a valid Headroom receipt is user-owned
        // state. Refuse to update or adopt it before invoking any host CLI so a
        // failed multi-host install cannot mutate one existing host and leave
        // another untouched. A recovery marker is an interrupted Headroom
        // transaction: finish its safe rollback first, then re-run the normal
        // ownership preflight instead of leaving a crash state permanently
        // unrecoverable.
        let receipt = self.read_plugin_receipt(plugin)?;
        if let Some(receipt) = receipt.as_ref() {
            if !Self::receipt_proves_plugin_ownership(plugin, receipt) {
                bail!(
                    "refusing to mutate {} because its legacy receipt lacks explicit Headroom ownership metadata; reinstall it from Headroom to create a managed receipt",
                    plugin.id
                );
            }
        }
        let recovery_marker_present = self.read_codex_marketplace_recovery(plugin)?.is_some();
        if receipt.is_none() && recovery_marker_present {
            let cli = PluginHost::Codex.cli().context(
                "Codex marketplace recovery is pending, but the Codex CLI was not found on PATH",
            )?;
            self.restore_codex_marketplace_recovery(plugin, &cli)
                .with_context(|| format!("recovering interrupted {} installation", plugin.id))?;
        }
        // Read every host registry before filtering on CLI availability. A
        // missing CLI must not hide a pre-existing user plugin in that host's
        // config and allow a partial install to take it over elsewhere.
        let all_host_presence: Vec<(PluginHost, bool)> = plugin
            .hosts
            .iter()
            .copied()
            .map(|host| {
                host.plugin_present_checked(plugin)
                    .with_context(|| format!("checking {} plugin registry", host.label()))
                    .map(|present| (host, present))
            })
            .collect::<Result<Vec<_>>>()?;
        if all_host_presence
            .iter()
            .all(|(host, _)| host.cli().is_none())
            && plugin.hosts.len() == 1
            && matches!(plugin.hosts[0], PluginHost::Codex)
        {
            bail!("Codex CLI ('codex') was not found on PATH. Install Codex, then try again.");
        }
        if all_host_presence
            .iter()
            .all(|(host, _)| host.cli().is_none())
        {
            bail!(
                "Neither the Claude Code CLI ('claude') nor the Codex CLI ('codex') was found on PATH. Install one, then try again."
            );
        }
        let previous_version = receipt.as_ref().and_then(|receipt| {
            receipt
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let preexisting: Vec<&str> = if receipt.is_none() {
            all_host_presence
                .iter()
                .filter_map(|(host, present)| present.then_some(host.label()))
                .collect()
        } else {
            Vec::new()
        };
        let mut hosts: Vec<(PluginHost, bool)> = all_host_presence
            .into_iter()
            .filter(|(host, _)| host.cli().is_some())
            .collect();
        if !preexisting.is_empty() {
            bail!(
                "refusing to take over existing {} plugin registration on {} without a Headroom ownership receipt",
                plugin.id,
                preexisting.join(", ")
            );
        }
        // Install missing hosts before touching existing ones. If a new host
        // fails, hosts that were already working remain unchanged.
        hosts.sort_by_key(|(_, was_registered)| *was_registered);
        let mut errors: Vec<String> = Vec::new();
        let mut installed_any = false;
        let mut newly_registered_hosts = Vec::new();
        let mut codex_outdated = false;
        for (host, was_registered) in hosts {
            match self.install_plugin_into(plugin, host) {
                Ok(()) => {
                    installed_any = true;
                    if !was_registered {
                        newly_registered_hosts.push(host);
                    }
                }
                Err(err) if matches!(host, PluginHost::Codex) && is_outdated_codex(&err) => {
                    codex_outdated = true;
                }
                Err(err) => errors.push(format!("{}: {err:#}", host.label())),
            }
        }
        let rollback_hosts = |manager: &Self| {
            for host in newly_registered_hosts.iter().copied() {
                if let Some(cli) = host.cli() {
                    if let Err(err) =
                        manager.run_plugin_cmd(plugin, &cli, host, &host.uninstall_args(plugin))
                    {
                        log::error!(
                            "{} rollback: removing {} from {} failed: {err:#}",
                            plugin.id,
                            plugin.id,
                            host.label()
                        );
                    }
                    match manager.marketplace_removal_allowed(plugin, host) {
                        Ok(true) => {
                            if let Err(err) = manager.run_plugin_cmd(
                                plugin,
                                &cli,
                                host,
                                &host.marketplace_remove_args(plugin),
                            ) {
                                log::error!(
                                    "{} rollback: removing marketplace {} failed: {err:#}",
                                    plugin.id,
                                    plugin.marketplace_name
                                );
                            }
                        }
                        Ok(false) => {}
                        Err(err) => log::error!(
                            "{} rollback: could not verify marketplace ownership: {err:#}",
                            plugin.id
                        ),
                    }
                    if matches!(host, PluginHost::Codex) {
                        if let Err(err) = manager.restore_codex_marketplace_recovery(plugin, &cli) {
                            // Keep the recovery marker when restoration fails;
                            // uninstall/retry can make another explicit attempt.
                            log::warn!(
                                "{} Codex marketplace rollback deferred: {err:#}",
                                plugin.id
                            );
                        }
                        if !manager.codex_marketplace_recovery_path(plugin).exists() {
                            if let Err(err) = manager.remove_owned_codex_adapter_marketplace(plugin)
                            {
                                log::error!(
                                    "{} rollback: removing managed Codex marketplace failed: {err:#}",
                                    plugin.id
                                );
                            }
                        }
                    }
                }
            }
        };
        if !installed_any {
            rollback_hosts(self);
            if codex_outdated && errors.is_empty() {
                bail!(
                    "Your Codex CLI is too old to install the {id} plugin. Update Codex, then try again."
                );
            }
            bail!("installing the {id} plugin failed: {}", errors.join("; "));
        }
        if !errors.is_empty() {
            rollback_hosts(self);
            let detail = errors.join("; ");
            // Explicit per-category fingerprint; the bridged warn is local-only
            // (skip_sentry rule) so this doesn't double-report. Mirrors the pip
            // install path -- see `plugin_install_failure_category`.
            let category = plugin_install_failure_category(&detail);
            sentry::with_scope(
                |scope| {
                    scope.set_fingerprint(Some(&["plugin-install-partial", category]));
                },
                || {
                    sentry::capture_message(
                        &format!(
                            "{id} installed for some hosts but not all [{category}]: {detail}"
                        ),
                        sentry::Level::Warning,
                    );
                },
            );
            log::warn!("{id} installed for some hosts but not all: {detail}");
        }
        if !errors.is_empty() {
            return Err(anyhow!(
                "installing {id} plugin failed on hosts: {}",
                errors.join("; ")
            ));
        }
        let version = installed_plugin_version(plugin)
            .or(previous_version)
            .unwrap_or_else(|| PLUGIN_DISPLAY_VERSION.into());
        if let Err(err) = self.ensure_plugin_runtime(plugin) {
            rollback_hosts(self);
            let _ = self.remove_plugin_runtime(plugin);
            return Err(err);
        }
        if let Err(err) = self.write_tool_receipt(
            plugin.id,
            Self::plugin_receipt_payload(plugin, &version, true),
        ) {
            rollback_hosts(self);
            let _ = self.remove_plugin_runtime(plugin);
            return Err(err);
        }
        if let Err(err) = self.enforce_exclusive_plugin_group(plugin.id) {
            rollback_hosts(self);
            let _ = self.remove_plugin_runtime(plugin);
            let _ =
                std::fs::remove_file(self.runtime.tools_dir.join(format!("{}.json", plugin.id)));
            return Err(err);
        }
        Ok(codex_outdated)
    }

    pub fn set_plugin_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let _guard = PACKAGE_UPDATE_LOCK.get_or_init(|| Mutex::new(())).lock();
        self.set_plugin_enabled_inner(id, enabled)?;
        if enabled {
            self.enforce_exclusive_plugin_group(id)?;
        }
        Ok(())
    }

    fn set_plugin_enabled_inner(&self, id: &str, enabled: bool) -> Result<()> {
        let plugin = plugin_addon(id).with_context(|| format!("unknown plugin addon: {id}"))?;
        // The receipt is the ownership boundary. A host registration without
        // this proof may belong to the user and must never be toggled by us.
        let receipt = self.require_owned_plugin_receipt(plugin)?;
        let previous_version = receipt
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut errors: Vec<String> = Vec::new();
        for &host in plugin.hosts {
            let present = match host.plugin_present_checked(plugin) {
                Ok(present) => present,
                Err(err) => {
                    errors.push(format!("{} registry: {err:#}", host.label()));
                    continue;
                }
            };
            // Codex exposes plugin activation as an `enabled` field in
            // config.toml. Toggle that field in place so Disable keeps the
            // install/cache and Enable does not need a network refresh. Only a
            // missing registration on Enable falls back to `plugin add`.
            let result = if matches!(host, PluginHost::Codex) {
                if present {
                    set_codex_plugin_enabled(plugin, enabled)
                } else if enabled {
                    self.install_plugin_into(plugin, host)
                } else {
                    continue;
                }
            } else {
                let Some(cli) = host.cli() else {
                    if present {
                        errors.push(format!(
                            "{} CLI is unavailable while {} is still registered",
                            host.label(),
                            plugin.id
                        ));
                    }
                    continue;
                };
                if enabled {
                    self.install_plugin_into(plugin, host)
                } else if present {
                    self.run_plugin_cmd(
                        plugin,
                        &cli,
                        host,
                        &["plugin", "disable", plugin.plugin_ref],
                    )
                } else {
                    continue;
                }
            };
            if let Err(err) = result {
                errors.push(format!("{}: {err:#}", host.label()));
            }
        }
        if !errors.is_empty() {
            bail!("toggling {id} failed: {}", errors.join("; "));
        }
        let version = installed_plugin_version(plugin)
            .or(previous_version)
            .unwrap_or_else(|| PLUGIN_DISPLAY_VERSION.into());
        self.write_tool_receipt(
            plugin.id,
            Self::plugin_receipt_payload(plugin, &version, enabled),
        )?;
        if enabled {
            self.ensure_plugin_runtime(plugin)?;
        } else {
            self.remove_plugin_runtime(plugin)?;
        }
        Ok(())
    }

    pub fn uninstall_plugin(&self, id: &str) -> Result<()> {
        let _guard = PACKAGE_UPDATE_LOCK.get_or_init(|| Mutex::new(())).lock();
        let plugin = plugin_addon(id).with_context(|| format!("unknown plugin addon: {id}"))?;
        // No receipt means Headroom never installed it. Don't touch the user's
        // plugin config or marketplace registration (which they may own).
        let recovery_marker_present = self.read_codex_marketplace_recovery(plugin)?.is_some();
        let _receipt = match self.read_plugin_receipt(plugin) {
            Ok(Some(receipt)) if Self::receipt_proves_plugin_ownership(plugin, &receipt) => receipt,
            Ok(Some(_)) => {
                bail!(
                    "refusing to uninstall {} because its legacy receipt lacks explicit Headroom ownership metadata; reinstall it from Headroom to create a managed receipt",
                    plugin.id
                );
            }
            Ok(None) if recovery_marker_present => {
                // The marker itself is Headroom's durable ownership evidence
                // for an interrupted migration. Recover the user's checkout
                // and registration, then leave all user plugin state alone.
                let cli = PluginHost::Codex.cli().context(
                    "Codex marketplace recovery is pending, but the Codex CLI was not found on PATH",
                )?;
                self.restore_codex_marketplace_recovery(plugin, &cli)
                    .with_context(|| {
                        format!("recovering interrupted {} installation", plugin.id)
                    })?;
                return Ok(());
            }
            Ok(None) => return Ok(()),
            Err(err) => {
                bail!(
                    "refusing to uninstall {} because its ownership receipt is invalid: {err:#}",
                    plugin.id
                );
            }
        };
        let mut uninstall_errors = Vec::new();
        for &host in plugin.hosts {
            let present = match host.plugin_present_checked(plugin) {
                Ok(present) => present,
                Err(err) => {
                    uninstall_errors.push(format!("{} registry: {err:#}", host.label()));
                    continue;
                }
            };
            if !present {
                continue;
            }
            let Some(cli) = host.cli() else {
                uninstall_errors.push(format!(
                    "{} CLI is unavailable while {} is still registered",
                    host.label(),
                    plugin.id
                ));
                continue;
            };
            let mut host_uninstall_succeeded = true;
            if let Err(err) = self.run_plugin_cmd(plugin, &cli, host, &host.uninstall_args(plugin))
            {
                host_uninstall_succeeded = false;
                uninstall_errors.push(format!("{}: {err:#}", host.label()));
            }
            if host_uninstall_succeeded {
                match self.marketplace_removal_allowed(plugin, host) {
                    Ok(true) => {
                        if let Err(err) = self.run_plugin_cmd(
                            plugin,
                            &cli,
                            host,
                            &host.marketplace_remove_args(plugin),
                        ) {
                            uninstall_errors.push(format!("{} marketplace: {err:#}", host.label()));
                        }
                    }
                    Ok(false) => {}
                    Err(err) => uninstall_errors
                        .push(format!("{} marketplace ownership: {err:#}", host.label())),
                }
            }
        }
        // All in Luna can move a user Git marketplace aside while preparing its
        // local Codex adapter. Restore that checkout and its original
        // registration before deleting our receipt. If recovery fails, keep the
        // marker and receipt so a later explicit retry can finish safely.
        if uninstall_errors.is_empty() && self.codex_marketplace_recovery_path(plugin).exists() {
            match PluginHost::Codex.cli() {
                Some(cli) => {
                    if let Err(err) = self.restore_codex_marketplace_recovery(plugin, &cli) {
                        uninstall_errors.push(format!("Codex marketplace recovery: {err:#}"));
                    }
                }
                None => uninstall_errors
                    .push("Codex marketplace recovery requires the Codex CLI on PATH".to_string()),
            }
        }
        if !uninstall_errors.is_empty() {
            bail!(
                "uninstalling {id} plugin failed: {}",
                uninstall_errors.join("; ")
            );
        }
        if !self.codex_marketplace_recovery_path(plugin).exists() {
            self.remove_owned_codex_adapter_marketplace(plugin)?;
        }
        let receipt = self.plugin_receipt_path(plugin);
        self.remove_plugin_runtime(plugin)?;
        if receipt.exists() {
            std::fs::remove_file(&receipt)
                .with_context(|| format!("removing {}", receipt.display()))?;
        }
        Ok(())
    }

    fn detect_status(&self, tool_id: &str) -> ToolStatus {
        if let Some(plugin) = plugin_addon(tool_id) {
            let receipt = match self.read_plugin_receipt(plugin) {
                Ok(Some(receipt)) => receipt,
                Ok(None) => return ToolStatus::NotInstalled,
                Err(err) => {
                    log::warn!(
                        "could not validate {} ownership receipt; reporting degraded: {err:#}",
                        plugin.id
                    );
                    return ToolStatus::Degraded;
                }
            };
            // Intentionally disabled via the app. The ownership receipt keeps
            // the card in its installed state while each host retains its
            // registration with activation turned off.
            let enabled = receipt
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !enabled {
                return ToolStatus::Healthy;
            }
            // Enabled per our receipt: require it still be registered with a host,
            // so a manual `/plugin` removal surfaces as not-installed.
            return if plugin.hosts.iter().any(|host| host.plugin_present(plugin)) {
                if !self.plugin_runtime_healthy(plugin) {
                    ToolStatus::Degraded
                } else {
                    ToolStatus::Healthy
                }
            } else {
                ToolStatus::NotInstalled
            };
        }
        let installed_path = self.runtime.tools_dir.join(format!("{tool_id}.json"));
        if installed_path.exists() && self.python_runtime_installed() {
            if crate::client_adapters::is_codex_enabled()
                && matches!(tool_id, "context7" | "codebase-memory")
            {
                let context7_version = self
                    .read_tool_receipt("context7")
                    .and_then(|receipt| {
                        receipt
                            .get("version")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| CONTEXT7_PINNED_VERSION.to_string());
                let context7_package = context7_package_spec_for(&context7_version);
                let registration = crate::client_adapters::codex_headroom_mcp_registration_health(
                    &context7_package,
                    &self.codebase_memory_entrypoint(),
                    &self.codebase_memory_cache_dir(),
                );
                let healthy = match registration {
                    Ok(health) if tool_id == "context7" => {
                        health.context7
                            == crate::client_adapters::ManagedMcpRegistrationState::Healthy
                    }
                    Ok(health) => {
                        health.codebase_memory
                            == crate::client_adapters::ManagedMcpRegistrationState::Healthy
                    }
                    Err(err) => {
                        log::warn!("checking Codex MCP registration for {tool_id}: {err:#}");
                        false
                    }
                };
                if !healthy {
                    return ToolStatus::Degraded;
                }
            }
            ToolStatus::Healthy
        } else {
            ToolStatus::NotInstalled
        }
    }
}

/// Plugin addons ship marketplace plugins that both Claude Code and Codex can
/// install through their own `<cli> plugin ...` managers. Codex activation is
/// persisted in config.toml rather than exposed as a CLI verb; removal remains
/// an explicit uninstall operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginHost {
    ClaudeCode,
    Codex,
}

impl PluginHost {
    fn label(self) -> &'static str {
        match self {
            PluginHost::ClaudeCode => "Claude Code",
            PluginHost::Codex => "Codex",
        }
    }

    fn cli(self) -> Option<PathBuf> {
        match self {
            PluginHost::ClaudeCode => crate::claude_cli::detect_claude_cli(),
            PluginHost::Codex => crate::claude_cli::detect_codex_cli(),
        }
    }

    fn marketplace_add_args(self, plugin: &PluginAddon) -> Vec<&'static str> {
        let mut args = vec!["plugin", "marketplace", "add", plugin.marketplace];
        if matches!(self, PluginHost::Codex) {
            for path in plugin.codex_sparse_paths {
                args.extend(["--sparse", *path]);
            }
        }
        args
    }

    fn prepare_marketplace_snapshot(self, plugin: &PluginAddon) -> Result<()> {
        if matches!(self, PluginHost::Codex) {
            let root = crate::client_adapters::codex_home()
                .join(".tmp")
                .join("marketplaces");
            write_codex_compat_marketplace_manifest_at(&root, plugin)?;
            prepare_codex_plugin_adapter_at(&root, plugin)?;
            let plugin_root = codex_marketplace_plugin_root(&root, plugin);
            normalize_codex_plugin_manifest_at(&plugin_root)?;
            // All in Luna ships Research Routes as a sibling Codex plugin in
            // the same marketplace checkout; normalize both in one pass.
            if plugin.id == "allinluna" {
                normalize_codex_plugin_manifest_at(
                    &root
                        .join(plugin.marketplace_name)
                        .join("plugins/research-routes"),
                )?;
            }
        }
        Ok(())
    }

    fn marketplace_remove_args(self, plugin: &PluginAddon) -> Vec<&'static str> {
        vec!["plugin", "marketplace", "remove", plugin.marketplace_name]
    }

    /// Pull the marketplace's newest commit into the host's local snapshot.
    /// Claude calls it `update`, Codex calls it `upgrade`.
    fn marketplace_update_args(self, plugin: &PluginAddon) -> Vec<&'static str> {
        match self {
            PluginHost::ClaudeCode => {
                vec!["plugin", "marketplace", "update", plugin.marketplace_name]
            }
            PluginHost::Codex => vec!["plugin", "marketplace", "upgrade", plugin.marketplace_name],
        }
    }

    /// Move an installed plugin onto the refreshed snapshot. Codex has no
    /// update verb; re-adding from the upgraded snapshot is the equivalent.
    fn update_args(self, plugin: &PluginAddon) -> Vec<&'static str> {
        match self {
            PluginHost::ClaudeCode => vec!["plugin", "update", plugin.plugin_ref],
            PluginHost::Codex => vec!["plugin", "add", plugin.plugin_ref],
        }
    }

    fn install_args(self, plugin: &PluginAddon) -> Vec<&'static str> {
        match self {
            // No `--scope user`: it is Claude Code's default, and CLIs older
            // than the flag reject the whole command with "unknown option".
            PluginHost::ClaudeCode => vec!["plugin", "install", plugin.plugin_ref],
            PluginHost::Codex => vec!["plugin", "add", plugin.plugin_ref],
        }
    }

    fn uninstall_args(self, plugin: &PluginAddon) -> Vec<&'static str> {
        match self {
            PluginHost::ClaudeCode => vec!["plugin", "uninstall", plugin.plugin_ref],
            PluginHost::Codex => vec!["plugin", "remove", plugin.plugin_ref],
        }
    }

    fn plugin_present(self, plugin: &PluginAddon) -> bool {
        match self {
            PluginHost::ClaudeCode => claude_plugin_present(plugin),
            PluginHost::Codex => codex_plugin_present(plugin),
        }
    }

    /// Ownership-sensitive operations must distinguish "not registered" from
    /// "the host registry could not be read or parsed". The best-effort bool
    /// above remains useful for dashboard health, but install/uninstall paths
    /// use this checked variant so malformed user config cannot be treated as
    /// permission to adopt or delete a plugin.
    fn plugin_present_checked(self, plugin: &PluginAddon) -> Result<bool> {
        match self {
            PluginHost::ClaudeCode => claude_plugin_present_checked(plugin),
            PluginHost::Codex => codex_plugin_present_checked(plugin),
        }
    }
}

fn write_codex_compat_marketplace_manifest_at(root: &Path, plugin: &PluginAddon) -> Result<()> {
    let manifest = root
        .join(plugin.marketplace_name)
        .join(".agents")
        .join("plugins")
        .join("marketplace.json");
    let parent = manifest
        .parent()
        .context("Codex compatibility marketplace manifest has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let plugins = if plugin.id == "allinluna" {
        vec![
            json!({
                "name": "allinluna",
                "source": { "source": "local", "path": "./plugins/allinluna" },
                "policy": { "installation": "AVAILABLE", "authentication": "ON_INSTALL" },
                "category": "Productivity"
            }),
            json!({
                "name": "research-routes",
                "source": { "source": "local", "path": "./plugins/research-routes" },
                "policy": { "installation": "AVAILABLE", "authentication": "ON_INSTALL" },
                "category": "Productivity"
            }),
        ]
    } else {
        vec![json!({
            "name": plugin.id,
            "source": { "source": "local", "path": plugin.codex_local_path },
            "policy": { "installation": "AVAILABLE", "authentication": "ON_INSTALL" },
            "category": "Productivity"
        })]
    };
    let body = json!({
        "name": plugin.marketplace_name,
        "interface": { "displayName": plugin.id },
        "plugins": plugins
    });
    crate::client_adapters::atomic_write(
        &manifest,
        &serde_json::to_vec_pretty(&body).context("serializing Codex compatibility marketplace")?,
    )
    .with_context(|| format!("writing {}", manifest.display()))
}

/// Normalize third-party Codex manifests at the adapter boundary. Codex
/// rejects oversized default prompts and warns about icon fields whose paths
/// do not resolve from the plugin root. Keep all unrelated upstream fields.
fn normalize_codex_plugin_manifest_at(plugin_root: &Path) -> Result<()> {
    let manifest = plugin_root.join(".codex-plugin").join("plugin.json");
    if !manifest.is_file() {
        return Ok(());
    }
    let mut body: Value = serde_json::from_slice(
        &std::fs::read(&manifest).with_context(|| format!("reading {}", manifest.display()))?,
    )
    .with_context(|| format!("parsing {}", manifest.display()))?;
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let Some(interface) = body.get_mut("interface").and_then(Value::as_object_mut) else {
        return Ok(());
    };

    let prompts: Option<&[&str]> = match name.as_str() {
        "allinluna" => Some(&[
            "Advisory default only: for complex or parallelizable goals, use All in Luna to compile a task graph; keep simple tasks direct.",
            "System, developer, security, and permission controls plus explicit user/task scope take precedence.",
            "Follow project instructions; execute host actions only within authorized scope and retain verified handoff evidence.",
        ]),
        "research-routes" => Some(&[
            "Advisory default only: use Research Routes to map evidence-backed routes when useful.",
            "System, developer, security, and permission controls plus explicit user/task scope take precedence; follow project instructions.",
            "Keep claims, evidence, contradictions, unknowns, failures, and provenance separate; ask only when scope expands.",
        ]),
        "stop-that-shit" => Some(&[
            "Advisory only: follow system, developer, security, permission, user, and project instructions; never block authorized work.",
            "Review the diff and report findings; edit when the requested fix is within existing authorization.",
            "Keep the requested change direct; delegate only when useful and within authorized scope.",
        ]),
        _ => None,
    };
    if let Some(prompts) = prompts {
        interface.insert(
            "defaultPrompt".to_string(),
            Value::Array(
                prompts
                    .iter()
                    .map(|prompt| Value::String((*prompt).into()))
                    .collect(),
            ),
        );
    }

    // `composerIcon` and `logo` are not accepted by Codex when their relative
    // paths cannot be resolved. Remove only invalid icon fields; valid assets
    // and all other interface metadata remain untouched.
    for key in ["composerIcon", "logo"] {
        let invalid = interface
            .get(key)
            .and_then(Value::as_str)
            .filter(|path| path.starts_with("./"))
            .is_some_and(|path| !plugin_root.join(path.trim_start_matches("./")).is_file());
        if invalid {
            interface.remove(key);
        }
    }
    if interface.get("icons").is_some_and(Value::is_null) {
        interface.remove("icons");
    }

    crate::client_adapters::atomic_write(
        &manifest,
        &serde_json::to_vec_pretty(&body)
            .context("serializing normalized Codex plugin manifest")?,
    )
    .with_context(|| format!("writing {}", manifest.display()))
}

#[cfg(test)]
mod codex_manifest_compat_tests {
    use super::{normalize_codex_plugin_cache_at, normalize_codex_plugin_manifest_at};
    use serde_json::json;
    use std::fs;

    fn write_manifest(root: &std::path::Path, name: &str, prompts: serde_json::Value) {
        let path = root.join(".codex-plugin");
        fs::create_dir_all(&path).expect("manifest directory");
        fs::write(
            path.join("plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "name": name,
                "version": "9.9.9",
                "interface": {
                    "defaultPrompt": prompts,
                    "shortDescription": "preserve me",
                    "logo": "./missing/icon.svg"
                },
                "custom": {"keep": true}
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");
    }

    #[test]
    fn normalizes_prompts_icons_and_preserves_unrelated_fields_idempotently() {
        let root = tempfile::tempdir().expect("tempdir");
        write_manifest(root.path(), "allinluna", json!(["oversized"]));
        normalize_codex_plugin_manifest_at(root.path()).expect("normalize manifest");
        let path = root.path().join(".codex-plugin/plugin.json");
        let first = fs::read(&path).expect("read normalized manifest");
        let value: serde_json::Value = serde_json::from_slice(&first).expect("valid JSON");
        let prompts = value["interface"]["defaultPrompt"]
            .as_array()
            .expect("prompt array");
        assert_eq!(prompts.len(), 3);
        assert!(prompts
            .iter()
            .all(|p| p.as_str().unwrap().chars().count() <= 128));
        assert_eq!(value["interface"]["shortDescription"], "preserve me");
        assert_eq!(value["custom"]["keep"], true);
        assert!(value["interface"].get("logo").is_none());

        normalize_codex_plugin_manifest_at(root.path()).expect("normalize twice");
        assert_eq!(
            first,
            fs::read(&path).expect("read normalized manifest twice")
        );
    }

    #[test]
    fn normalizes_nested_versioned_cache_and_research_routes_sibling() {
        let cache = tempfile::tempdir().expect("cache root");
        let allinluna = cache.path().join("allinluna/allinluna/2.0.0-rc.7");
        let research_routes = cache.path().join("allinluna/research-routes/0.3.0-rc.7");
        write_manifest(&allinluna, "allinluna", json!(["old"]));
        write_manifest(&research_routes, "research-routes", json!(["old"]));
        let plugin = super::PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");

        normalize_codex_plugin_cache_at(cache.path(), plugin).expect("normalize cache");

        for root in [&allinluna, &research_routes] {
            let value: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join(".codex-plugin/plugin.json")).expect("read manifest"),
            )
            .expect("valid manifest");
            let prompts = value["interface"]["defaultPrompt"]
                .as_array()
                .expect("prompt array");
            assert_eq!(prompts.len(), 3);
            assert!(prompts
                .iter()
                .all(|prompt| prompt.as_str().unwrap().chars().count() <= 128));
            assert!(value["interface"].get("logo").is_none());
        }
    }

    #[test]
    fn stop_that_shit_is_limited_to_three_natural_language_prompts() {
        let root = tempfile::tempdir().expect("tempdir");
        write_manifest(
            root.path(),
            "stop-that-shit",
            json!(["one", "two", "three", "four"]),
        );
        normalize_codex_plugin_manifest_at(root.path()).expect("normalize manifest");
        let bytes = fs::read(root.path().join(".codex-plugin/plugin.json")).expect("read manifest");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
        let prompts = value["interface"]["defaultPrompt"]
            .as_array()
            .expect("prompt array");
        assert_eq!(prompts.len(), 3);
        assert!(prompts.iter().all(|p| {
            let text = p.as_str().unwrap();
            text.chars().count() <= 128 && !text.contains('$')
        }));
    }

    #[test]
    fn research_routes_prompts_are_normalized_under_allinluna_marketplace() {
        let root = tempfile::tempdir().expect("tempdir");
        write_manifest(
            root.path(),
            "research-routes",
            json!(["one", "two", "three", "four"]),
        );
        normalize_codex_plugin_manifest_at(root.path()).expect("normalize manifest");
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(root.path().join(".codex-plugin/plugin.json")).expect("read manifest"),
        )
        .expect("valid JSON");
        let prompts = value["interface"]["defaultPrompt"]
            .as_array()
            .expect("prompt array");
        assert_eq!(prompts.len(), 3);
        assert!(prompts
            .iter()
            .all(|prompt| prompt.as_str().unwrap().chars().count() <= 128));
    }

    #[test]
    fn openspec_adapter_metadata_matches_current_cli_version() {
        let plugin = super::PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "openspec")
            .expect("OpenSpec addon");
        assert_eq!(super::codex_plugin_adapter(plugin).unwrap().0, "1.12.0");
    }

    #[test]
    fn allinluna_adapter_is_selected_for_codex_install() {
        let plugin = super::PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");

        let (version, _description, skills) =
            super::codex_plugin_adapter(plugin).expect("All in Luna adapter");
        assert_eq!(version, "2.0.0-rc.7");
        assert_eq!(skills, "./skills/");
        assert!(super::uses_codex_plugin_adapter(
            plugin,
            super::PluginHost::Codex
        ));
        assert!(!super::uses_codex_plugin_adapter(
            plugin,
            super::PluginHost::ClaudeCode
        ));
    }
}

fn codex_plugin_adapter(
    plugin: &PluginAddon,
) -> Option<(&'static str, &'static str, &'static str)> {
    match plugin.id {
        "allinluna" => Some((
            "2.0.0-rc.7",
            "Compile goals, plans, active runs, or research routes into executable task graphs",
            "./skills/",
        )),
        "openspec" => Some((
            // OpenSpec's bundled CLI/package currently resolves to 1.12.0.
            // Keep the Codex adapter metadata aligned with the executable.
            "1.12.0",
            "Specification-driven change workflow with OpenSpec CLI support",
            "./skills/",
        )),
        "gstack" => Some((
            "1.79.0",
            "Product, design, browser, QA, review, and shipping workflow",
            "./.agents/skills/",
        )),
        "ralph-loop" => Some((
            "0.1.0",
            "Bounded autonomous coding-agent loop with explicit stop signals",
            "./skills/",
        )),
        _ => None,
    }
}

fn uses_codex_plugin_adapter(plugin: &PluginAddon, host: PluginHost) -> bool {
    matches!(host, PluginHost::Codex) && codex_plugin_adapter(plugin).is_some()
}

fn codex_marketplace_plugin_root(root: &Path, plugin: &PluginAddon) -> PathBuf {
    root.join(plugin.marketplace_name)
        .join(plugin.codex_local_path.trim_start_matches("./"))
}

/// Codex copies marketplace checkouts into a versioned cache before loading
/// them. Normalize the copied manifests too; normalizing only `.tmp` leaves
/// Codex validating the original oversized prompts and invalid icon paths.
fn normalize_codex_plugin_cache(plugin: &PluginAddon) -> Result<()> {
    let cache = crate::client_adapters::codex_home()
        .join("plugins")
        .join("cache");
    normalize_codex_plugin_cache_at(&cache, plugin)
}

fn normalize_codex_plugin_cache_at(cache: &Path, plugin: &PluginAddon) -> Result<()> {
    let marketplace = cache.join(plugin.marketplace_name);
    normalize_codex_plugin_cache_plugin_at(&marketplace.join(plugin.id))?;

    // All in Luna installs Research Routes as a sibling plugin in the same
    // marketplace cache, so its copied manifest needs the same treatment.
    if plugin.id == "allinluna" {
        normalize_codex_plugin_cache_plugin_at(&marketplace.join("research-routes"))?;
    }
    Ok(())
}

fn normalize_codex_plugin_cache_plugin_at(plugin_root: &Path) -> Result<()> {
    if !plugin_root.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(plugin_root)
        .with_context(|| format!("reading Codex plugin cache {}", plugin_root.display()))?
    {
        let entry = entry.with_context(|| {
            format!("reading Codex plugin cache entry {}", plugin_root.display())
        })?;
        let version_root = entry.path();
        if version_root.is_dir() {
            normalize_codex_plugin_manifest_at(&version_root)?;
        }
    }
    Ok(())
}

fn prepare_codex_adapter_marketplace(plugin: &PluginAddon) -> Result<PathBuf> {
    let root = crate::client_adapters::codex_home()
        .join(".tmp")
        .join("marketplaces");
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let source = root.join(plugin.marketplace_name);
    let marker = source.join(".headroom-local-community-adapter.json");
    let mut fresh_clone = false;

    if source.join(".git").is_dir() {
        if !codex_adapter_marker_is_owned(&source, plugin)? {
            bail!(
                "{} already exists but is not marked as a Headroom-managed adapter marketplace",
                source.display()
            );
        }
        let source_text = source.to_string_lossy().into_owned();
        run_codex_adapter_command("git", &["-C", &source_text, "pull", "--ff-only"], &root)?;
    } else {
        if source.exists() {
            bail!(
                "{} already exists without a Git checkout; remove the stale directory before retrying",
                source.display()
            );
        }
        let url = format!("https://github.com/{}.git", plugin.marketplace);
        let source_text = source.to_string_lossy().into_owned();
        run_codex_adapter_command("git", &["clone", "--depth", "1", &url, &source_text], &root)?;
        fresh_clone = true;
    }

    let preparation = (|| -> Result<()> {
        if fresh_clone {
            crate::client_adapters::atomic_write(
                &marker,
                &serde_json::to_vec_pretty(&json!({
                    "id": plugin.id,
                    "source": plugin.source_url
                }))
                .context("serializing Codex adapter ownership marker")?,
            )
            .with_context(|| format!("writing {}", marker.display()))?;
        }
        write_codex_compat_marketplace_manifest_at(&root, plugin)?;
        prepare_codex_plugin_adapter_at(&root, plugin)?;
        normalize_codex_plugin_manifest_at(&codex_marketplace_plugin_root(&root, plugin))?;
        if plugin.id == "allinluna" {
            normalize_codex_plugin_manifest_at(
                &root
                    .join(plugin.marketplace_name)
                    .join("plugins/research-routes"),
            )?;
        }
        Ok(())
    })();
    if let Err(err) = preparation {
        if fresh_clone {
            if let Err(cleanup_err) = cleanup_fresh_codex_adapter_checkout(&source, plugin) {
                return Err(anyhow!(
                    "{err:#}; cleaning fresh Codex adapter checkout {} also failed: {cleanup_err:#}",
                    source.display()
                ));
            }
        }
        return Err(err);
    }
    Ok(source)
}

/// Remove a checkout created by the current adapter preparation only when the
/// exact ownership marker proves that the path is still ours. If marker
/// publication failed or another process replaced the path, leave it in place
/// so an error path never deletes user-managed data.
fn cleanup_fresh_codex_adapter_checkout(source: &Path, plugin: &PluginAddon) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("inspecting {}", source.display())),
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to remove fresh Codex adapter path {} after it became a symlink",
            source.display()
        );
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    let owned = match codex_adapter_marker_is_owned(source, plugin) {
        Ok(owned) => owned,
        Err(err) => {
            log::warn!(
                "leaving failed Codex adapter checkout {} in place because its ownership marker is invalid: {err:#}",
                source.display()
            );
            false
        }
    };
    if !owned {
        return Ok(());
    }
    std::fs::remove_dir_all(source).with_context(|| {
        format!(
            "removing failed fresh Codex adapter checkout {}",
            source.display()
        )
    })
}

/// Return whether a Codex adapter checkout carries the exact ownership marker
/// Headroom wrote for `plugin`. A present marker with another id/source is an
/// ownership violation, so report it as an error instead of treating it as a
/// missing marker and mutating the checkout.
fn codex_adapter_marker_is_owned(root: &Path, plugin: &PluginAddon) -> Result<bool> {
    let marker = root.join(".headroom-local-community-adapter.json");
    let bytes = match std::fs::read(&marker) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("reading {}", marker.display())),
    };
    let value: Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", marker.display()))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", marker.display()))?;
    let id = object.get("id").and_then(Value::as_str);
    let source = object.get("source").and_then(Value::as_str);
    if id != Some(plugin.id) || source != Some(plugin.source_url) {
        bail!(
            "Codex adapter ownership marker {} does not match {}",
            marker.display(),
            plugin.id
        );
    }
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexAdapterSourceDecision {
    Create,
    ReuseManaged,
    MigrateGit { backup: PathBuf },
    Refuse,
}

/// Recovery metadata for the one Codex adapter path that can temporarily move
/// a user's Git marketplace aside. Keep this outside the marketplace checkout:
/// the checkout is replaced by the adapter and may be refreshed or deleted on
/// uninstall, while this small marker must survive every failure boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CodexMarketplaceRecovery {
    backup_path: PathBuf,
    #[serde(default)]
    original_git_source: Option<String>,
    /// False while the durable marker has been published but the checkout has
    /// not yet moved; true after the backup rename succeeds. Writing the
    /// marker first closes the crash window that previously orphaned a user's
    /// checkout without recovery metadata.
    moved: bool,
}

/// Recovery markers written by the first implementation did not carry a phase
/// bit. That writer emitted the marker only after the checkout had been moved,
/// so treating the missing field as `true` preserves the old, recoverable
/// meaning instead of reopening a crash window during upgrade.
impl Default for CodexMarketplaceRecovery {
    fn default() -> Self {
        Self {
            backup_path: PathBuf::new(),
            original_git_source: None,
            moved: true,
        }
    }
}

fn codex_adapter_source_decision(source: &Path, marker: &Path) -> CodexAdapterSourceDecision {
    if source.join(".git").is_dir() {
        if marker.is_file() {
            CodexAdapterSourceDecision::ReuseManaged
        } else {
            CodexAdapterSourceDecision::MigrateGit {
                backup: source.with_file_name(format!(
                    "{}.headroom-legacy",
                    source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("marketplace")
                )),
            }
        }
    } else if source.exists() {
        CodexAdapterSourceDecision::Refuse
    } else {
        CodexAdapterSourceDecision::Create
    }
}

fn migrate_unmanaged_codex_marketplace(
    plugin: &PluginAddon,
    registration: &CodexMarketplaceRegistration,
) -> Result<Option<PathBuf>> {
    if plugin.id != "allinluna" {
        return Ok(None);
    }
    let root = crate::client_adapters::codex_home()
        .join(".tmp")
        .join("marketplaces");
    let source = root.join(plugin.marketplace_name);
    let marker = source.join(".headroom-local-community-adapter.json");
    // A checkout with no matching host registration is simply a user's local
    // repository. Installing an adapter must never move it aside merely
    // because it happens to use the same directory name. A registered Git
    // marketplace is the narrow case where the user explicitly selected this
    // checkout through Codex and the recovery sidecar can preserve it.
    if source.join(".git").is_dir()
        && !marker.is_file()
        && !matches!(registration, CodexMarketplaceRegistration::Git(_))
    {
        bail!(
            "{} is an unregistered Git checkout; refusing to move user-managed Codex marketplace without a matching registration",
            source.display()
        );
    }
    let CodexAdapterSourceDecision::MigrateGit { backup } =
        codex_adapter_source_decision(&source, &marker)
    else {
        return Ok(None);
    };
    let CodexMarketplaceRegistration::Git(registered_source) = registration else {
        // The caller normally performs this guard before reaching here. Keep
        // it local as well so this helper cannot ever move an unregistered
        // checkout if another call site is added later.
        bail!(
            "{} is not registered as a Git marketplace; refusing to move user-managed checkout",
            source.display()
        );
    };
    if registered_source.trim().is_empty()
        || !git_checkout_matches_source(&source, registered_source)
    {
        bail!(
            "Codex marketplace checkout {} does not match its registered Git source; refusing to move user-managed checkout",
            source.display()
        );
    }
    if backup.exists() {
        bail!(
            "refusing to overwrite recoverable Codex marketplace backup {}",
            backup.display()
        );
    }
    std::fs::rename(&source, &backup).with_context(|| {
        format!(
            "moving unmanaged Codex marketplace to recoverable backup {}",
            backup.display()
        )
    })?;
    Ok(Some(backup))
}

fn git_checkout_matches_source(path: &Path, expected: &str) -> bool {
    let path_text = path.to_string_lossy().into_owned();
    let Ok(output) = crate::proc::command("git")
        .args(["-C", &path_text, "config", "--get", "remote.origin.url"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let actual = String::from_utf8_lossy(&output.stdout);
    normalize_git_source(&actual) == normalize_git_source(expected)
}

fn normalize_git_source(source: &str) -> String {
    source
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

fn restore_codex_marketplace_backup(backup: &Path, destination: &Path) -> Result<()> {
    if !backup.exists() {
        bail!(
            "Codex marketplace backup {} is missing; refusing to infer recovery from destination {}",
            backup.display(),
            destination.display()
        );
    }
    if destination.exists() {
        std::fs::remove_dir_all(destination).with_context(|| {
            format!(
                "removing replacement Codex marketplace {}",
                destination.display()
            )
        })?;
    }
    std::fs::rename(backup, destination)
        .with_context(|| format!("restoring Codex marketplace backup {}", backup.display()))
}

fn merge_codex_marketplace_recovery_error(
    primary: anyhow::Error,
    recovery: Result<()>,
) -> anyhow::Error {
    match recovery {
        Ok(()) => primary,
        Err(recovery) => {
            anyhow!("{primary:#}; restoring previous Codex marketplace failed: {recovery:#}")
        }
    }
}

fn run_codex_adapter_command(command: &str, args: &[&str], cwd: &Path) -> Result<()> {
    let binary = match command {
        // GUI launches do not reliably inherit the user's shell PATH. Resolve
        // npm through the same known-path/login-shell probes used by CLI tools.
        "npm" => crate::claude_cli::detect_npm_cli()
            .with_context(|| "npm was not found; install Node.js/npm and retry")?,
        _ => PathBuf::from(command),
    };
    run_codex_adapter_binary(&binary, args, cwd)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexMarketplaceRegistration {
    Missing,
    Local(PathBuf),
    Git(String),
    Other(String),
}

fn paths_refer_to_same_location(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn codex_marketplace_registration(
    cli: &Path,
    cwd: &Path,
    marketplace_name: &str,
) -> Result<CodexMarketplaceRegistration> {
    // Use the shared GUI-safe command builder so script CLIs such as
    // `/opt/homebrew/bin/codex` can resolve their `#!/usr/bin/env node`
    // interpreter even when Headroom was launched from Finder.
    let output = build_command(cli, &["plugin", "marketplace", "list", "--json"], cwd)
        .output()
        .with_context(|| format!("listing Codex marketplaces with {}", cli.display()))?;
    if !output.status.success() {
        bail!(
            "Codex marketplace list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).context("parsing Codex marketplace list JSON")?;
    if !json_marketplace_list_shape_is_supported(&value) {
        bail!(
            "Codex marketplace list JSON has an unsupported schema; refusing to treat it as an empty registry"
        );
    }
    Ok(json_marketplace_registration(&value, marketplace_name)
        .unwrap_or(CodexMarketplaceRegistration::Missing))
}

/// Return whether the marketplace-list payload has a shape that this client
/// understands. A schema drift must be an error, never `Missing`, because the
/// latter authorizes replacing or deleting a user marketplace during install
/// and recovery. The recursive form accepts the nested `marketplaces` object
/// used by current Codex builds and the array/direct-object forms covered by
/// older builds.
fn json_marketplace_list_shape_is_supported(value: &Value) -> bool {
    match value {
        Value::Array(items) => items
            .iter()
            .all(|item| matches!(item, Value::Object(_) | Value::Array(_))),
        Value::Object(object) => {
            if let Some(marketplaces) = object.get("marketplaces") {
                return matches!(marketplaces, Value::Array(items)
                    if items
                        .iter()
                        .all(|item| matches!(item, Value::Object(_) | Value::Array(_))));
            }
            if let Some(name) = object.get("name").or_else(|| object.get("marketplace")) {
                return name.is_string();
            }
            object.values().any(|child| {
                matches!(child, Value::Object(_) | Value::Array(_))
                    && json_marketplace_list_shape_is_supported(child)
            })
        }
        _ => false,
    }
}

fn ensure_codex_git_registration(
    cli: &Path,
    cwd: &Path,
    marketplace_name: &str,
    expected_source: &str,
) -> Result<()> {
    match codex_marketplace_registration(cli, cwd, marketplace_name)? {
        CodexMarketplaceRegistration::Git(actual)
            if normalize_git_source(&actual) == normalize_git_source(expected_source) => Ok(()),
        other => bail!(
            "Codex marketplace {} was not restored as the expected Git source {}; observed {other:?}",
            marketplace_name,
            expected_source
        ),
    }
}

fn json_marketplace_registration(
    value: &Value,
    marketplace_name: &str,
) -> Option<CodexMarketplaceRegistration> {
    match value {
        Value::Array(items) => items
            .iter()
            .find_map(|item| json_marketplace_registration(item, marketplace_name)),
        Value::Object(object) => {
            let name_matches = object
                .get("name")
                .or_else(|| object.get("marketplace"))
                .and_then(Value::as_str)
                == Some(marketplace_name);
            if name_matches {
                let metadata = object
                    .get("marketplaceSource")
                    .and_then(Value::as_object)
                    .unwrap_or(object);
                let source_type = metadata
                    .get("sourceType")
                    .or_else(|| metadata.get("source_type"))
                    .or_else(|| object.get("sourceType"))
                    .or_else(|| object.get("source_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let source = metadata
                    .get("source")
                    .or_else(|| object.get("source"))
                    .or_else(|| object.get("root"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                return Some(match source_type.as_str() {
                    "local" => CodexMarketplaceRegistration::Local(PathBuf::from(source)),
                    "git" => CodexMarketplaceRegistration::Git(source),
                    other => CodexMarketplaceRegistration::Other(other.to_string()),
                });
            }
            object
                .values()
                .find_map(|item| json_marketplace_registration(item, marketplace_name))
        }
        _ => None,
    }
}

fn codex_marketplace_is_local(cli: &Path, cwd: &Path, marketplace_name: &str) -> Result<bool> {
    Ok(matches!(
        codex_marketplace_registration(cli, cwd, marketplace_name)?,
        CodexMarketplaceRegistration::Local(_)
    ))
}

fn json_marketplace_is_local(value: &Value, marketplace_name: &str) -> bool {
    match value {
        Value::Array(items) => items
            .iter()
            .any(|item| json_marketplace_is_local(item, marketplace_name)),
        Value::Object(object) => {
            let name_matches = object
                .get("name")
                .or_else(|| object.get("marketplace"))
                .and_then(Value::as_str)
                == Some(marketplace_name);
            let source_is_local = object
                .get("sourceType")
                .or_else(|| object.get("source_type"))
                .and_then(Value::as_str)
                .is_some_and(|source| source.eq_ignore_ascii_case("local"));
            (name_matches && source_is_local)
                || object
                    .values()
                    .any(|item| json_marketplace_is_local(item, marketplace_name))
        }
        _ => false,
    }
}

fn run_codex_adapter_binary(binary: &Path, args: &[&str], cwd: &Path) -> Result<()> {
    run_command_streaming(binary, args, cwd, &mut |line: &str| {
        let command = binary.display();
        log::info!("codex plugin adapter [{command}]: {line}")
    })
    .with_context(|| {
        format!(
            "running {} {} in {}",
            binary.display(),
            args.join(" "),
            cwd.display()
        )
    })
}

fn gstack_bun_binary(plugin_root: &Path) -> Result<PathBuf> {
    if crate::proc::command("bun")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return Ok(PathBuf::from("bun"));
    }

    let install_root = plugin_root.join(".headroom-bun");
    let executable =
        install_root
            .join("node_modules")
            .join(".bin")
            .join(if cfg!(target_os = "windows") {
                "bun.cmd"
            } else {
                "bun"
            });
    if !executable.is_file() {
        let prefix = install_root.to_string_lossy().into_owned();
        let cache = plugin_root
            .join(".headroom-npm-cache")
            .to_string_lossy()
            .into_owned();
        let package = format!("bun@{GSTACK_BUN_VERSION}");
        run_codex_adapter_command(
            "npm",
            &[
                "install",
                "--prefix",
                &prefix,
                "--cache",
                &cache,
                "--no-audit",
                "--no-fund",
                &package,
            ],
            plugin_root,
        )?;
    }
    if !executable.is_file() {
        bail!(
            "private Bun {GSTACK_BUN_VERSION} install completed but {} was not created",
            executable.display()
        );
    }
    Ok(executable)
}

fn prepare_codex_plugin_adapter_at(root: &Path, plugin: &PluginAddon) -> Result<()> {
    let Some((version, description, skills)) = codex_plugin_adapter(plugin) else {
        return Ok(());
    };
    let plugin_root = codex_marketplace_plugin_root(root, plugin);
    match plugin.id {
        "openspec" => {
            if !plugin_root.join("dist/cli/index.js").is_file() {
                let npm_cache = plugin_root
                    .join(".headroom-npm-cache")
                    .to_string_lossy()
                    .into_owned();
                run_codex_adapter_command(
                    "npm",
                    &[
                        "install",
                        "--ignore-scripts",
                        "--cache",
                        &npm_cache,
                        "--no-audit",
                        "--no-fund",
                    ],
                    &plugin_root,
                )?;
                run_codex_adapter_command(
                    "npm",
                    &["--cache", &npm_cache, "run", "build"],
                    &plugin_root,
                )?;
            }
        }
        "gstack" => {
            let bun = gstack_bun_binary(&plugin_root)?;
            let browse = if cfg!(target_os = "windows") {
                plugin_root.join("browse/dist/browse.exe")
            } else {
                plugin_root.join("browse/dist/browse")
            };
            if !browse.is_file() || !plugin_root.join(".agents/skills").is_dir() {
                run_codex_adapter_binary(&bun, &["install", "--ignore-scripts"], &plugin_root)?;
                run_codex_adapter_binary(&bun, &["run", "build"], &plugin_root)?;
                run_codex_adapter_binary(
                    &bun,
                    &["run", "gen:skill-docs", "--host", "codex"],
                    &plugin_root,
                )?;
            }
            rewrite_gstack_plugin_paths(&plugin_root.join(".agents/skills"))?;
        }
        "ralph-loop" => {
            if !plugin_root.join("ralph-loop.sh").is_file() {
                bail!("Ralph Loop marketplace snapshot is missing ralph-loop.sh");
            }
        }
        _ => {}
    }

    let manifest = plugin_root.join(".codex-plugin/plugin.json");
    let parent = manifest
        .parent()
        .context("Codex plugin adapter manifest has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let body = json!({
        "name": plugin.id,
        "version": version,
        "description": description,
        "skills": skills,
        "hooks": {},
        "interface": {
            "displayName": plugin.id,
            "shortDescription": description,
            "developerName": "Headroom Local Community adapter",
            "category": "Developer Tools",
            "websiteURL": plugin.source_url,
            "capabilities": ["Interactive", "Read", "Write"]
        }
    });
    crate::client_adapters::atomic_write(
        &manifest,
        &serde_json::to_vec_pretty(&body).context("serializing Codex plugin adapter")?,
    )
    .with_context(|| format!("writing {}", manifest.display()))
}

fn rewrite_gstack_plugin_paths(root: &Path) -> Result<()> {
    if !root.is_dir() {
        bail!(
            "gstack Codex skills were not generated at {}",
            root.display()
        );
    }
    for entry in std::fs::read_dir(root).with_context(|| format!("reading {}", root.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            rewrite_gstack_plugin_paths(&path)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
            let original = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let updated = original
                .replace(
                    "${CODEX_HOME:-$HOME/.codex}/skills/gstack",
                    "$CODEX_PLUGIN_ROOT",
                )
                .replace("$HOME/.codex/skills/gstack", "$CODEX_PLUGIN_ROOT")
                .replace("~/.codex/skills/gstack", "$CODEX_PLUGIN_ROOT");
            if updated != original {
                crate::client_adapters::atomic_write(&path, updated.as_bytes())
                    .with_context(|| format!("rewriting {}", path.display()))?;
            }
        }
    }
    Ok(())
}

pub(crate) fn claude_installed_plugins() -> Option<Value> {
    // Not `dirs::home_dir()`: on Windows that reads the profile known folder
    // and ignores `$HOME`, so a redirected home (tests, Git Bash) resolves
    // against the REAL profile. Every OSS-plugin test failed on Windows CI
    // that way -- reading the runner's own ~/.claude and finding no plugin.
    let path = crate::client_adapters::home_dir()
        .join(".claude")
        .join("plugins")
        .join("installed_plugins.json");
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Claude Code records installs in `~/.claude/plugins/installed_plugins.json`
/// under `plugins["<plugin>@<marketplace>"]` as a non-empty array of install
/// records.
fn claude_plugin_present(plugin: &PluginAddon) -> bool {
    claude_installed_plugins()
        .and_then(|v| v.get("plugins")?.get(plugin.plugin_ref).cloned())
        .and_then(|entry| entry.as_array().map(|installs| !installs.is_empty()))
        .unwrap_or(false)
}

fn claude_plugin_present_checked(plugin: &PluginAddon) -> Result<bool> {
    let path = crate::client_adapters::home_dir()
        .join(".claude")
        .join("plugins")
        .join("installed_plugins.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let value: Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    let root = value
        .as_object()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", path.display()))?;
    let Some(plugins) = root.get("plugins") else {
        return Ok(false);
    };
    let plugins = plugins
        .as_object()
        .ok_or_else(|| anyhow!("{}.plugins must be a JSON object", path.display()))?;
    let Some(entry) = plugins.get(plugin.plugin_ref) else {
        return Ok(false);
    };
    let installs = entry.as_array().ok_or_else(|| {
        anyhow!(
            "{}.plugins[{}] must be a JSON array",
            path.display(),
            plugin.plugin_ref
        )
    })?;
    Ok(!installs.is_empty())
}

/// Codex records installs in `~/.codex/config.toml` under a
/// `[plugins."<plugin>@<marketplace>"]` table. A disabled plugin remains
/// registered with `enabled = false`; that distinction lets uninstall remove
/// only a plugin that is actually installed while the app's Disable action
/// keeps the cache and marketplace checkout intact.
fn codex_plugin_present(plugin: &PluginAddon) -> bool {
    codex_plugin_present_checked(plugin).unwrap_or(false)
}

fn codex_plugin_present_checked(plugin: &PluginAddon) -> Result<bool> {
    let path = crate::client_adapters::codex_home().join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let root: toml::Value = toml::from_str(&text)
        .with_context(|| format!("parsing Codex config {}", path.display()))?;
    let Some(plugins) = root.get("plugins") else {
        return Ok(false);
    };
    let plugins = plugins.as_table().ok_or_else(|| {
        anyhow!(
            "Codex config {} has a non-table plugins value",
            path.display()
        )
    })?;
    let Some(entry) = plugins.get(plugin.plugin_ref) else {
        return Ok(false);
    };
    if !entry.is_table() {
        bail!(
            "Codex config {} has a non-table plugin entry for {}",
            path.display(),
            plugin.plugin_ref
        );
    }
    Ok(true)
}

/// Return the activation state encoded by one Codex plugin table. Codex treats
/// a missing `enabled` key as enabled, but Headroom writes an explicit value on
/// every transition so the dashboard and Codex agree after a restart.
fn codex_plugin_enabled_from_text(content: &str, plugin_ref: &str) -> Result<Option<bool>> {
    let root: toml::Value = toml::from_str(content).context("parsing Codex config TOML")?;
    let Some(plugins) = root.get("plugins") else {
        return Ok(None);
    };
    let plugins = plugins
        .as_table()
        .context("Codex config plugins value must be a table")?;
    let Some(entry) = plugins.get(plugin_ref) else {
        return Ok(None);
    };
    let entry = entry
        .as_table()
        .with_context(|| format!("Codex plugin entry {plugin_ref} must be a table"))?;
    match entry.get("enabled") {
        Some(value) => Ok(Some(value.as_bool().with_context(|| {
            format!("Codex plugin entry {plugin_ref}.enabled must be a boolean")
        })?)),
        None => Ok(Some(true)),
    }
}

fn toml_line_body(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

/// Find a TOML comment marker without treating a `#` inside a quoted string as
/// a comment. This is intentionally small: it only supports the string forms
/// needed to preserve an `enabled = true # comment` line and table headers.
fn toml_comment_start(line: &str) -> Option<usize> {
    let mut basic = false;
    let mut literal = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if basic && ch == '\\' {
            escaped = true;
            continue;
        }
        match ch {
            '"' if !literal => basic = !basic,
            '\'' if !basic => literal = !literal,
            '#' if !basic && !literal => return Some(index),
            _ => {}
        }
    }
    None
}

fn toml_line_code(line: &str) -> &str {
    let (body, _) = toml_line_body(line);
    let end = toml_comment_start(body).unwrap_or(body.len());
    body[..end].trim()
}

fn codex_plugin_header_matches(line: &str, plugin_ref: &str) -> bool {
    let code = toml_line_code(line);
    code == format!("[plugins.\"{plugin_ref}\"]") || code == format!("[plugins.'{plugin_ref}']")
}

fn toml_table_header(line: &str) -> bool {
    let code = toml_line_code(line);
    code.starts_with('[') && code.ends_with(']')
}

/// Replace only the boolean value on a direct `enabled = ...` line. The
/// caller has already parsed the complete document, so a non-boolean value is
/// reported as an error before this function is reached. Keeping the original
/// prefix, spacing, comment, and line ending makes the edit safe for configs
/// users format by hand.
fn replace_codex_enabled_line(line: &str, enabled: bool) -> Option<String> {
    let (body, ending) = toml_line_body(line);
    let code_end = toml_comment_start(body).unwrap_or(body.len());
    let code = &body[..code_end];
    let equals = code.find('=')?;
    let key = code[..equals].trim();
    let key = key
        .strip_prefix('"')
        .and_then(|key| key.strip_suffix('"'))
        .or_else(|| {
            key.strip_prefix('\'')
                .and_then(|key| key.strip_suffix('\''))
        })
        .unwrap_or(key);
    if key != "enabled" {
        return None;
    }
    let value_start = equals + 1;
    let rhs = &code[value_start..];
    let leading = rhs.len() - rhs.trim_start().len();
    let trailing = rhs.trim_end().len();
    if leading >= trailing {
        return None;
    }
    let start = value_start + leading;
    let end = value_start + trailing;
    let mut updated = String::with_capacity(line.len() + 1);
    updated.push_str(&body[..start]);
    updated.push_str(if enabled { "true" } else { "false" });
    updated.push_str(&body[end..]);
    updated.push_str(ending);
    Some(updated)
}

/// Render a Codex config with one managed plugin's activation flag changed.
/// This function never creates a missing plugin table: callers must first
/// prove ownership and registration, so an unexpected config shape fails
/// closed instead of mutating a similarly named user entry.
fn render_codex_plugin_enabled(
    content: &str,
    plugin_ref: &str,
    enabled: bool,
) -> Result<Option<String>> {
    let current = codex_plugin_enabled_from_text(content, plugin_ref)?;
    let Some(_current) = current else {
        bail!("Codex plugin {plugin_ref} is not registered");
    };
    // An omitted key has Codex's enabled-by-default meaning. Still materialize
    // `enabled = true` so a later Disable transition has a stable line to edit.

    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let target_start = lines
        .iter()
        .position(|line| codex_plugin_header_matches(line, plugin_ref))
        .ok_or_else(|| {
            anyhow!("Codex plugin {plugin_ref} is registered in an unsupported inline-table form")
        })?;
    let target_end = lines
        .iter()
        .enumerate()
        .skip(target_start + 1)
        .find_map(|(index, line)| toml_table_header(line).then_some(index))
        .unwrap_or(lines.len());

    let mut out = String::with_capacity(content.len() + 24);
    let mut replaced = false;
    for (index, line) in lines.iter().enumerate() {
        if index == target_start {
            out.push_str(line);
            continue;
        }
        if index > target_start && index < target_end {
            if let Some(updated) = replace_codex_enabled_line(line, enabled) {
                out.push_str(&updated);
                replaced = true;
                continue;
            }
        }
        out.push_str(line);
    }

    if !replaced {
        let insertion = if target_start < lines.len() {
            let (_, ending) = toml_line_body(lines[target_start]);
            if ending.is_empty() {
                format!("\nenabled = {}", if enabled { "true" } else { "false" })
            } else {
                format!(
                    "enabled = {}{ending}",
                    if enabled { "true" } else { "false" }
                )
            }
        } else {
            unreachable!("target_start came from lines");
        };
        let mut rebuilt = String::with_capacity(out.len() + insertion.len());
        for (index, line) in lines.iter().enumerate() {
            if index == target_start {
                rebuilt.push_str(line);
                rebuilt.push_str(&insertion);
            } else {
                rebuilt.push_str(line);
            }
        }
        out = rebuilt;
    }

    if out == content {
        return Ok(None);
    }
    let parsed: toml::Value =
        toml::from_str(&out).context("rewritten Codex config is invalid TOML")?;
    let parsed_enabled = parsed
        .get("plugins")
        .and_then(toml::Value::as_table)
        .and_then(|plugins| plugins.get(plugin_ref))
        .and_then(toml::Value::as_table)
        .and_then(|entry| entry.get("enabled"))
        .and_then(toml::Value::as_bool);
    if parsed_enabled != Some(enabled) {
        bail!("rewritten Codex plugin {plugin_ref} did not receive enabled = {enabled}");
    }
    Ok(Some(out))
}

/// Atomically persist one Codex plugin activation transition. Codex may write
/// config.toml while the desktop is changing it, so re-read before publishing
/// and verify after the rename; a changed file is merged and retried rather
/// than blindly overwriting a concurrent user update.
fn set_codex_plugin_enabled(plugin: &PluginAddon, enabled: bool) -> Result<()> {
    const MAX_ATTEMPTS: usize = 3;
    let path = crate::client_adapters::codex_home().join("config.toml");
    for attempt in 0..MAX_ATTEMPTS {
        let existing = std::fs::read_to_string(&path)
            .with_context(|| format!("reading Codex config {}", path.display()))?;
        let Some(updated) = render_codex_plugin_enabled(&existing, plugin.plugin_ref, enabled)?
        else {
            return Ok(());
        };
        // Detect a Codex/user write that happened during rendering. The final
        // verify below closes the remaining rename race and causes a retry.
        if std::fs::read_to_string(&path)
            .with_context(|| format!("checking Codex config {}", path.display()))?
            != existing
        {
            continue;
        }
        let _ = crate::client_adapters::backup_if_exists(&path)?;
        crate::client_adapters::atomic_write(&path, updated.as_bytes())
            .with_context(|| format!("writing Codex config {}", path.display()))?;
        let verify = std::fs::read_to_string(&path)
            .with_context(|| format!("verifying Codex config {}", path.display()))?;
        if codex_plugin_enabled_from_text(&verify, plugin.plugin_ref)? == Some(enabled) {
            return Ok(());
        }
        if attempt + 1 == MAX_ATTEMPTS {
            break;
        }
    }
    bail!(
        "Codex config {} changed while toggling plugin {}; please retry",
        path.display(),
        plugin.id
    )
}

/// One serena tool application logs exactly one line containing this marker
/// (`tools_base.py` `_log_tool_application`, stable through pinned v1.7.0).
const SERENA_TOOL_CALL_LOG_MARKER: &str = "; session_id: ";

/// Serena's dashboard API binds the first free port scanning upward from here
/// (`constants.py` 0x5EDA, `dashboard.py` `_find_first_free_port`).
const SERENA_DASHBOARD_BASE_PORT: u16 = 24282;
const SERENA_DASHBOARD_PORT_WINDOW: u16 = 64;

/// Discover listening Serena dashboard candidates once per live-stats cache
/// miss. We only query IPv4/IPv6 loopback and still require the dashboard's
/// exact JSON shape. Keep candidates inside a bounded observation window so
/// unrelated high-port local services are never probed. When listener
/// enumeration is unavailable, scan the same window rather than regressing to
/// the former four-port limit.
fn discover_serena_dashboard_ports() -> Vec<u16> {
    let mut ports = Vec::new();

    for lsof in ["/usr/sbin/lsof", "/usr/bin/lsof"] {
        let Ok(output) = crate::proc::command(lsof)
            .args(["-nP", "-a", "-iTCP", "-sTCP:LISTEN"])
            .output()
        else {
            continue;
        };
        if output.status.success() {
            ports.extend(parse_lsof_listener_ports(&String::from_utf8_lossy(
                &output.stdout,
            )));
            break;
        }
    }

    #[cfg(target_os = "linux")]
    if ports.is_empty() {
        if let Ok(output) = crate::proc::command("ss").arg("-ltn").output() {
            if output.status.success() {
                ports.extend(parse_ss_listener_ports(&String::from_utf8_lossy(
                    &output.stdout,
                )));
            }
        }
    }

    #[cfg(windows)]
    if ports.is_empty() {
        if let Ok(output) = crate::proc::command("netstat")
            .args(["-ano", "-p", "tcp"])
            .output()
        {
            if output.status.success() {
                ports.extend(parse_netstat_listener_ports(&String::from_utf8_lossy(
                    &output.stdout,
                )));
            }
        }
    }

    ports.sort_unstable();
    ports.dedup();
    if ports.is_empty() {
        ports.extend(
            (0..SERENA_DASHBOARD_PORT_WINDOW)
                .filter_map(|offset| SERENA_DASHBOARD_BASE_PORT.checked_add(offset)),
        );
    }
    ports
}

fn parse_dashboard_listener_port(endpoint: &str) -> Option<u16> {
    let (host, port) = endpoint.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']);
    if !matches!(
        host,
        "127.0.0.1" | "localhost" | "0.0.0.0" | "*" | "::1" | "::"
    ) {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    let offset = port.checked_sub(SERENA_DASHBOARD_BASE_PORT)?;
    (offset < SERENA_DASHBOARD_PORT_WINDOW).then_some(port)
}

fn parse_lsof_listener_ports(text: &str) -> Vec<u16> {
    text.lines()
        .skip(1)
        .filter(|line| line.trim_end().ends_with("(LISTEN)"))
        .filter_map(|line| {
            line.split_whitespace()
                .find_map(parse_dashboard_listener_port)
        })
        .collect()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_ss_listener_ports(text: &str) -> Vec<u16> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 4 || !fields[0].eq_ignore_ascii_case("LISTEN") {
                return None;
            }
            parse_dashboard_listener_port(fields[3])
        })
        .collect()
}

#[cfg_attr(not(windows), allow(dead_code))]
fn parse_netstat_listener_ports(text: &str) -> Vec<u16> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 5
                || !fields[0].eq_ignore_ascii_case("TCP")
                || !fields[3].eq_ignore_ascii_case("LISTENING")
            {
                return None;
            }
            parse_dashboard_listener_port(fields[1])
        })
        .collect()
}

fn fetch_serena_output_tokens_from_ports(ports: &[u16]) -> Option<u64> {
    let mut total = 0_u64;
    let mut any_responder = false;
    for port in ports {
        let tokens = fetch_serena_output_tokens(&format!("http://127.0.0.1:{port}"))
            .or_else(|| fetch_serena_output_tokens(&format!("http://[::1]:{port}")));
        if let Some(tokens) = tokens {
            any_responder = true;
            total = total.saturating_add(tokens);
        }
    }
    (any_responder && total > 0).then_some(total)
}

/// GET `{base_url}/get_tool_stats` and sum `output_tokens` across tools.
/// Response shape: `{"stats": {tool: {num_times_called, input_tokens,
/// output_tokens}}}`; empty stats yield Some(0), no/invalid responder None.
fn fetch_serena_output_tokens(base_url: &str) -> Option<u64> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
        .ok()?;
    let value: Value = client
        .get(format!("{base_url}/get_tool_stats"))
        .send()
        .ok()?
        .json()
        .ok()?;
    serena_output_tokens_from_value(&value)
}

fn serena_output_tokens_from_value(value: &Value) -> Option<u64> {
    let stats = value.get("stats")?.as_object()?;
    let mut total: u64 = 0;
    for entry in stats.values() {
        let entry = entry.as_object()?;
        entry.get("num_times_called")?.as_u64()?;
        entry.get("input_tokens")?.as_u64()?;
        total = total.saturating_add(entry.get("output_tokens")?.as_u64()?);
    }
    Some(total)
}

/// "231 tool calls today, ~48k tokens returned in 2h 14m" — either half may
/// be absent (and the duration within the second), None only when both are.
/// `(calls_line, tokens_line)` — the two independently-optional halves of the
/// serena activity phrasing, shared by the Addons-tab chip and the feed tile.
fn serena_savings_parts(
    calls_today: Option<u64>,
    live: Option<(u64, Option<Duration>)>,
) -> (Option<String>, Option<String>) {
    let calls_line = calls_today.map(|count| {
        if count == 1 {
            "1 tool call today".to_string()
        } else {
            format!("{count} tool calls today")
        }
    });
    let tokens_line = match live {
        Some((tokens, Some(age))) => Some(format!(
            "~{} tokens returned in {}",
            compact_token_count(tokens),
            compact_duration(age)
        )),
        // No session age (e.g. serena running from a non-managed install):
        // still name the window, or the figure reads as all-time.
        Some((tokens, None)) => Some(format!(
            "~{} tokens returned this session",
            compact_token_count(tokens)
        )),
        None => None,
    };
    (calls_line, tokens_line)
}

fn serena_savings_label(
    calls_today: Option<u64>,
    live: Option<(u64, Option<Duration>)>,
) -> Option<String> {
    let (calls_line, tokens_line) = serena_savings_parts(calls_today, live);
    let parts: Vec<String> = [calls_line, tokens_line].into_iter().flatten().collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// Parse `ps` etime, `[[dd-]hh:]mm:ss`.
fn parse_ps_etime(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    let (days, clock) = match raw.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, raw),
    };
    let fields: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [m, s] => (0, m.parse::<u64>().ok()?, s.parse::<u64>().ok()?),
        [h, m, s] => (
            h.parse::<u64>().ok()?,
            m.parse::<u64>().ok()?,
            s.parse::<u64>().ok()?,
        ),
        _ => return None,
    };
    Some(Duration::from_secs(
        ((days * 24 + hours) * 60 + minutes) * 60 + seconds,
    ))
}

/// Oldest `start-mcp-server` session matching our entrypoint in
/// `ps -axww -o etime=,args=` output.
fn oldest_serena_session_age(ps_output: &str, entrypoint_marker: &str) -> Option<Duration> {
    ps_output
        .lines()
        .filter_map(|line| {
            let (etime, argv) = line.trim_start().split_once(char::is_whitespace)?;
            (argv.contains(entrypoint_marker) && argv.contains("start-mcp-server"))
                .then(|| parse_ps_etime(etime))
                .flatten()
        })
        .max()
}

fn compact_duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    if minutes >= 60 {
        let hours = minutes / 60;
        let rest = minutes % 60;
        if rest == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {rest}m")
        }
    } else if minutes == 0 {
        "under a minute".to_string()
    } else {
        format!("{minutes}m")
    }
}

fn compact_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.0}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Count tool-call lines across every `*.txt` log in one day's serena log dir.
/// None when the dir is missing or holds no calls, so the chip stays hidden.
// ponytail: a tool result that itself quotes serena log text would inflate the
// count; switch to per-line prefix matching if that ever shows up in practice.
fn count_serena_tool_calls_in_dir(dir: &Path) -> Option<u64> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut count: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("txt") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        count += String::from_utf8_lossy(&bytes)
            .matches(SERENA_TOOL_CALL_LOG_MARKER)
            .count() as u64;
    }
    (count > 0).then_some(count)
}

/// Serena writes a `.serena/` dir (config, cache, memories) into the root of
/// every project it is pointed at. It self-ignores the noisy parts but leaves
/// `project.yml` and its own `.gitignore` tracked, so every repo the user
/// touches picks up two unrequested files.
const SERENA_GITIGNORE_MARKER: &str =
    "# Headroom-managed: serena writes .serena/ into every project root";
const SERENA_GITIGNORE_PATTERN: &str = ".serena/";

/// Git reads `$XDG_CONFIG_HOME/git/ignore` (default `~/.config/git/ignore`)
/// when `core.excludesfile` is unset, so the pattern can be added without
/// rewriting the user's global git config.
fn global_git_excludes_path() -> Option<PathBuf> {
    let configured = crate::proc::command("git")
        .args(["config", "--global", "--get", "core.excludesfile"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|path| !path.is_empty());
    let home = crate::client_adapters::home_dir();
    Some(match configured {
        Some(path) => match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(path),
        },
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("git")
            .join("ignore"),
    })
}

/// `None` when the file already says what it should - nothing to write.
/// Removal only strips the block Headroom added: a hand-written `.serena/`
/// with no marker above it is the user's, not ours.
fn apply_serena_gitignore(existing: &str, present: bool) -> Option<String> {
    if present {
        if existing
            .lines()
            .any(|line| line.trim() == SERENA_GITIGNORE_PATTERN)
        {
            return None;
        }
        let mut updated = existing.to_string();
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(SERENA_GITIGNORE_MARKER);
        updated.push('\n');
        updated.push_str(SERENA_GITIGNORE_PATTERN);
        updated.push('\n');
        return Some(updated);
    }
    if !existing.lines().any(|line| line == SERENA_GITIGNORE_MARKER) {
        return None;
    }
    let mut updated = String::new();
    let mut after_marker = false;
    for line in existing.lines() {
        if line == SERENA_GITIGNORE_MARKER {
            after_marker = true;
            continue;
        }
        if after_marker {
            after_marker = false;
            if line.trim() == SERENA_GITIGNORE_PATTERN {
                continue;
            }
        }
        updated.push_str(line);
        updated.push('\n');
    }
    Some(updated)
}

/// Best-effort: a user with an unwritable git config still gets a working
/// serena, so failures are logged at info (never Sentry-escalated) and the
/// install continues.
fn set_serena_global_gitignore(present: bool) {
    let Some(path) = global_git_excludes_path() else {
        return;
    };
    let existing = match std::fs::read_to_string(&path) {
        Ok(existing) => existing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            // Never replace an unreadable excludes file with an empty one: it
            // may contain user-owned patterns that we cannot safely recover.
            log::warn!(
                "serena: reading global git excludes {} failed; leaving it untouched: {err:#}",
                path.display()
            );
            return;
        }
    };
    let Some(updated) = apply_serena_gitignore(&existing, present) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            log::info!("serena: creating {} failed: {err:#}", parent.display());
            return;
        }
    }
    if path.exists() {
        if let Err(err) = crate::client_adapters::backup_if_exists(&path) {
            log::warn!(
                "serena: preserving global git excludes {} failed; leaving it untouched: {err:#}",
                path.display()
            );
            return;
        }
    }
    if let Err(err) = crate::client_adapters::atomic_write(&path, updated.as_bytes()) {
        log::info!("serena: updating {} failed: {err:#}", path.display());
    } else {
        log::info!(
            "serena: {} .serena/ in {}",
            if present { "ignoring" } else { "un-ignoring" },
            path.display()
        );
    }
}

/// Serena 1.7 defaults its dashboard "interface" on macOS to a menu-bar tray
/// app (`DashboardManager.Mode.from_platform` returns tray_manager on Darwin).
/// `--open-web-dashboard False` only suppresses the browser tab, and there is
/// no CLI flag or env var for the interface, so the config key is the only
/// lever. `browser` keeps the dashboard HTTP API up (our savings chip polls
/// it) while spawning nothing visible. An explicit user-chosen interface is
/// left alone; only the unset platform default is replaced. `None` = nothing
/// to write.
fn apply_serena_dashboard_interface(existing: &str) -> Option<String> {
    const KEY: &str = "web_dashboard_interface:";
    const WANT: &str = "web_dashboard_interface: browser";
    let mut out = String::new();
    let mut found = false;
    for line in existing.lines() {
        if !found && line.starts_with(KEY) {
            found = true;
            let value = line[KEY.len()..].split('#').next().unwrap_or("").trim();
            match value {
                "" | "null" | "~" => out.push_str(WANT),
                _ => return None,
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !found {
        if existing.trim().is_empty() {
            // A file serena has never written: `projects` is the one key its
            // loader hard-requires; everything else falls back to defaults
            // (serena fills the rest in on its next config migration save).
            out.push_str("projects: []\n");
        }
        out.push_str(WANT);
        out.push('\n');
    }
    Some(out)
}

/// Best-effort like the gitignore nudge above: serena works either way, the
/// tray icon is just noise, so failures log at info and never block install.
fn set_serena_browser_dashboard() {
    let home_dir = std::env::var_os("SERENA_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(crate::client_adapters::home_dir().join(".serena")));
    let Some(dir) = home_dir else {
        return;
    };
    let path = dir.join("serena_config.yml");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            // An existing file we cannot read must not be clobbered with a
            // minimal one - that would drop the user's projects list.
            log::info!("serena: reading {} failed: {err:#}", path.display());
            return;
        }
    };
    let Some(updated) = apply_serena_dashboard_interface(&existing) else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(&dir) {
        log::info!("serena: creating {} failed: {err:#}", dir.display());
        return;
    }
    if path.exists() {
        if let Err(err) = crate::client_adapters::backup_if_exists(&path) {
            log::warn!(
                "serena: preserving {} failed; leaving it untouched: {err:#}",
                path.display()
            );
            return;
        }
    }
    if let Err(err) = crate::client_adapters::atomic_write(&path, updated.as_bytes()) {
        log::info!("serena: updating {} failed: {err:#}", path.display());
    } else {
        log::info!(
            "serena: set web_dashboard_interface to browser in {}",
            path.display()
        );
    }
}

fn installed_plugin_version(plugin: &PluginAddon) -> Option<String> {
    if let Some(version) = claude_installed_plugins().and_then(|plugins| {
        plugins
            .get("plugins")?
            .get(plugin.plugin_ref)?
            .as_array()?
            .first()?
            .get("version")?
            .as_str()
            .map(str::to_string)
    }) {
        return Some(version);
    }

    if !codex_plugin_present(plugin) {
        return None;
    }

    let codex_tmp = crate::client_adapters::codex_home().join(".tmp");
    plugin_version_from_codex_cache(plugin, &codex_tmp)
}

fn plugin_version_from_codex_cache(plugin: &PluginAddon, codex_tmp: &Path) -> Option<String> {
    let local_path = plugin.codex_local_path.trim_start_matches("./");
    [
        codex_tmp
            .join("marketplaces")
            .join(plugin.marketplace_name)
            .join(local_path),
        codex_tmp.join("plugins").join(local_path),
    ]
    .into_iter()
    .find_map(|root| plugin_version_from_root(&root))
}

fn plugin_version_from_root(root: &Path) -> Option<String> {
    for path in [
        root.join(".codex-plugin/plugin.json"),
        root.join("package.json"),
        root.join("VERSION"),
    ] {
        if let Ok(bytes) = std::fs::read(&path) {
            if path.file_name().and_then(|name| name.to_str()) == Some("VERSION") {
                if let Some(version) = String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                {
                    return Some(version.to_string());
                }
            } else if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                let version = value
                    .get("version")
                    .and_then(Value::as_str)
                    .or_else(|| value.pointer("/package/version").and_then(Value::as_str));
                if let Some(version) = version {
                    return Some(version.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod plugin_version_tests {
    use super::{plugin_version_from_codex_cache, plugin_version_from_root, PLUGIN_ADDONS};
    use serde_json::json;
    use std::fs;

    #[test]
    fn reads_manifest_then_package_then_version_file() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(
            root.path().join("package.json"),
            serde_json::to_vec(&json!({ "version": "1.79.0", "name": "gstack" })).unwrap(),
        )
        .unwrap();
        assert_eq!(
            plugin_version_from_root(root.path()).as_deref(),
            Some("1.79.0")
        );

        fs::create_dir_all(root.path().join(".codex-plugin")).unwrap();
        fs::write(
            root.path().join(".codex-plugin/plugin.json"),
            serde_json::to_vec(&json!({ "version": "2.0.0-rc.7" })).unwrap(),
        )
        .unwrap();
        assert_eq!(
            plugin_version_from_root(root.path()).as_deref(),
            Some("2.0.0-rc.7")
        );

        fs::write(root.path().join(".codex-plugin/plugin.json"), b"not json").unwrap();
        fs::remove_file(root.path().join("package.json")).unwrap();
        fs::write(root.path().join("VERSION"), b"0.3.0\n").unwrap();
        assert_eq!(
            plugin_version_from_root(root.path()).as_deref(),
            Some("0.3.0")
        );
    }

    #[test]
    fn reads_curated_codex_plugin_version_from_shared_cache() {
        let root = tempfile::tempdir().expect("tempdir");
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "superpowers")
            .expect("superpowers addon");
        let manifest = root
            .path()
            .join("plugins/plugins/superpowers/.codex-plugin/plugin.json");
        fs::create_dir_all(manifest.parent().expect("manifest parent")).unwrap();
        fs::write(
            &manifest,
            serde_json::to_vec(&json!({ "version": "6.3.0" })).unwrap(),
        )
        .unwrap();

        assert_eq!(
            plugin_version_from_codex_cache(plugin, root.path()).as_deref(),
            Some("6.3.0")
        );
    }
}

/// Claude Code ≥2.x stores user-scope MCP servers in `~/.claude.json` under
/// `mcpServers.<name>`. The legacy `~/.claude/mcp.json` path written by our
/// Python CLI's fallback branch is ignored. Reading the file Claude Code
/// actually reads is the only reliable way to confirm the registration
/// landed where `/mcp` and `claude mcp list` will see it.
fn claude_code_has_headroom_mcp_server() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let path = home.join(".claude.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    value
        .get("mcpServers")
        .and_then(|v| v.get(crate::edition::MCP_SERVER_NAME))
        .is_some()
}

/// Writes the headroom MCP server entry directly to `~/.claude.json`.
/// Used when `claude mcp add` is unavailable (e.g. bare GUI PATH). Preserves
/// all existing keys; only merges `mcpServers.headroom_local_community`.
fn write_headroom_to_claude_json(entrypoint: &Path, proxy_url: &str) -> Result<()> {
    let Some(home) = dirs::home_dir() else {
        anyhow::bail!("home directory not available");
    };
    write_headroom_to_claude_json_at(&home.join(".claude.json"), entrypoint, proxy_url)
}

fn write_headroom_to_claude_json_at(path: &Path, entrypoint: &Path, proxy_url: &str) -> Result<()> {
    let desired = json!({
        "command": entrypoint,
        "args": ["mcp", "serve"],
        "env": {
            "HEADROOM_PROXY_URL": proxy_url,
            "HEADROOM_WORKSPACE_DIR": crate::edition::workspace_dir(),
            "HEADROOM_CONFIG_DIR": crate::edition::config_dir(),
            "HEADROOM_TELEMETRY": "off",
            "HEADROOM_BEACON": "0"
        },
    });

    let modified_time = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();

    // ~/.claude.json holds OAuth state and per-project settings, and Claude
    // Code rewrites it frequently — often while this runs (bootstrap,
    // upgrade, requirements repair). Two defenses against reverting a
    // concurrent Claude Code write with our stale snapshot: skip the publish
    // entirely when our entry is already present and correct (the common
    // case on every repair), and re-check the file's mtime just before the
    // rename, retrying the whole read-modify-write if it moved.
    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 0..MAX_ATTEMPTS {
        let seen_modified = modified_time(path);

        // A read or parse failure (e.g. a mid-write partial file) must never
        // degrade to an empty object, or the write below replaces the user's
        // entire config with just our entry.
        let mut config: Value = if path.exists() {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            if bytes.iter().all(|b| b.is_ascii_whitespace()) {
                json!({})
            } else {
                serde_json::from_slice(&bytes).with_context(|| {
                    format!(
                        "parsing {} failed; refusing to overwrite potentially valid user config",
                        path.display()
                    )
                })?
            }
        } else {
            json!({})
        };

        if config
            .get("mcpServers")
            .and_then(|servers| servers.get(crate::edition::MCP_SERVER_NAME))
            == Some(&desired)
        {
            return Ok(());
        }

        let root = config
            .as_object_mut()
            .context("~/.claude.json root is not a JSON object")?;

        root.entry("mcpServers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("~/.claude.json mcpServers is not a JSON object")?
            .insert(crate::edition::MCP_SERVER_NAME.into(), desired.clone());

        let _ = crate::client_adapters::backup_if_exists(path)?;

        // Publish atomically (tmp + rename) so a crash mid-write can never
        // leave a truncated ~/.claude.json behind. Give each writer its own
        // temporary name: a fixed sibling lets two concurrent repairs rename
        // one another's file or fail with ENOENT.
        static MCP_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = MCP_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut tmp_os = path.as_os_str().to_os_string();
        tmp_os.push(format!(
            ".headroom-local-community-tmp.{}.{}",
            std::process::id(),
            n
        ));
        let tmp = PathBuf::from(tmp_os);
        let serialized = serde_json::to_vec_pretty(&config)?;
        crate::client_adapters::atomic_write(&tmp, &serialized)
            .with_context(|| format!("writing {}", tmp.display()))?;

        if modified_time(path) != seen_modified && attempt + 1 < MAX_ATTEMPTS {
            // Claude Code wrote while we worked — merge against the new
            // contents instead of reverting them.
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        return std::fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} into place", tmp.display()));
    }
    unreachable!("loop always returns")
}

fn is_local_proxy_reachable() -> bool {
    // Check headroom's actual backend port, not the intercept port (6867),
    // because the intercept starts before headroom and would always be reachable.
    let address: SocketAddr = ([127, 0, 0, 1], backend_port::get()).into();
    TcpStream::connect_timeout(&address, Duration::from_millis(180)).is_ok()
}

enum PortState {
    Free,
    HeadroomRunning,
    ForeignOccupant(String),
}

/// Occupant string used when the port is held but no owning pid can be found.
/// `diagnose_proxy_port_settled` keys off this exact shape, so it is a const
/// rather than three loose copies of the same literal.
const UNKNOWN_OCCUPANT: &str = "unknown process";

fn diagnose_proxy_port(port: u16) -> PortState {
    // If we can bind the port, nothing is there.
    if TcpListener::bind(("127.0.0.1", port)).is_ok() {
        return PortState::Free;
    }

    // Port is held. Probe it: headroom's proxy speaks HTTP and, for an
    // unrecognized path, responds with an HTTP status line. A foreign
    // non-HTTP service (SSH, Redis, etc.) will not.
    let headroom_like = probe_headroom_http(port, Duration::from_millis(400));
    if headroom_like {
        // "Speaks HTTP" alone is not identity. HeadroomRunning occupants get
        // killed by the reclaim path, so verify the pid's argv actually looks
        // like our managed backend — an unrelated local HTTP server (dev
        // server, docker forward) squatting the port must route to the
        // foreign fallback-port path instead of being SIGKILLed.
        match listener_process(port) {
            Some((_, pid)) if pid_is_headroom_backend(pid) => PortState::HeadroomRunning,
            Some((command, pid)) => PortState::ForeignOccupant(format!("{command} pid {pid}")),
            None => PortState::ForeignOccupant(UNKNOWN_OCCUPANT.into()),
        }
    } else {
        PortState::ForeignOccupant(listener_detail(port).unwrap_or_else(|| UNKNOWN_OCCUPANT.into()))
    }
}

/// [`diagnose_proxy_port`], but waits out a socket the process we just replaced
/// left closing.
///
/// An updater relaunch tears down the old backend and starts the new one
/// immediately. For a few seconds the kernel still holds :6868 while nothing
/// accepts on it and no pid owns it -- a shape `diagnose_proxy_port` can only
/// read as a foreign occupant, so the backend abandoned its default port for
/// 6770 on every update (RUST-7F: one event per release, i.e. one per update,
/// not one per launch). Only that exact unowned shape is waited on; a named
/// occupant is a real conflict and falls back immediately.
fn diagnose_proxy_port_settled(port: u16) -> PortState {
    settle_unowned_port(|| diagnose_proxy_port(port), 6, Duration::from_millis(500))
}

/// Who holds `port`, as a short label for boot-validation failure diagnostics.
///
/// `proxy_port_bound=true` on its own cannot tell "our freshly spawned child
/// is wedged and never answered /livez" apart from "a foreign process squats
/// the port so the child never got it": both burn the full boot budget and
/// look identical in Sentry (RUST-7Y, RUST-4A). Called once, on the failure
/// path, before `stop_headroom` tears the occupant down.
pub(crate) fn describe_proxy_port_occupant(port: u16) -> String {
    match diagnose_proxy_port(port) {
        PortState::Free => "free".to_string(),
        PortState::HeadroomRunning => "headroom".to_string(),
        PortState::ForeignOccupant(detail) => format!("foreign: {detail}"),
    }
}

/// Re-`diagnose` while the port reads as held-by-nobody, up to `attempts`
/// times. Split from [`diagnose_proxy_port_settled`] so the retry rule is
/// testable without binding real sockets.
fn settle_unowned_port(
    mut diagnose: impl FnMut() -> PortState,
    attempts: usize,
    pause: Duration,
) -> PortState {
    let mut last = diagnose();
    for _ in 1..attempts.max(1) {
        match last {
            PortState::ForeignOccupant(ref detail) if detail == UNKNOWN_OCCUPANT => {
                std::thread::sleep(pause);
                last = diagnose();
            }
            settled => return settled,
        }
    }
    last
}

/// True when `pid`'s full command line looks like Headroom's managed backend
/// (venv python under the Headroom app-support tree, or anything
/// headroom-branded). Guards port reclaim from killing an unrelated process
/// that merely answers HTTP on our port.
fn pid_is_headroom_backend(pid: u32) -> bool {
    // Windows has no `ps`, and `tasklist` reports only the image name, which is
    // a bare `python.exe` for every venv on the machine. The executable PATH is
    // both obtainable and a stricter claim: anything running out of our managed
    // runtime is ours by construction, whatever its argv says.
    #[cfg(windows)]
    {
        let Ok(output) = crate::proc::command("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("(Get-Process -Id {pid} -ErrorAction SilentlyContinue).Path"),
            ])
            .output()
        else {
            return false;
        };
        let path = String::from_utf8_lossy(&output.stdout);
        let runtime_dir =
            ManagedRuntime::bootstrap_root(&crate::storage::app_data_dir()).runtime_dir;
        return exe_path_is_under(&path, &runtime_dir);
    }
    #[cfg(not(windows))]
    {
        let Ok(output) = crate::proc::command("/bin/ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        let argv = String::from_utf8_lossy(&output.stdout).to_lowercase();
        // A bare "headroom" substring also matches unrelated dev processes whose
        // path merely contains it (e.g. `python /Users/x/headroom/serve.py 6868`).
        // Require the `proxy` subcommand as well: every version of the managed
        // backend runs as `... headroom proxy ...` (or `-m headroom.proxy.server`),
        // so this still recognizes old orphans the upgrade path must reclaim while
        // excluding a random headroom-pathed process. Only ever consulted for the
        // exact port being reclaimed, so the blast radius is one port either way.
        argv.contains("headroom") && argv.contains("proxy")
    }
}

/// True when `exe_path` sits inside `runtime_dir`.
///
/// The Windows half of [`pid_is_headroom_backend`], split out so the rule is
/// testable without a live pid. Match the runtime DIRECTORY rather than
/// substrings: on Windows the backend runs as the base interpreter
/// (`runtime\python\python.exe`), NOT the venv one
/// (`runtime\venv\Scripts\python.exe`) that `managed_python()` composes, so a
/// first cut of this requiring "venv" in the path vouched for neither and the
/// identity gate could never pass. Both layouts live under `runtime`.
#[cfg_attr(not(windows), allow(dead_code))]
fn exe_path_is_under(exe_path: &str, runtime_dir: &Path) -> bool {
    let exe = exe_path.trim();
    // Empty means the pid was gone by the time PowerShell looked, or it is a
    // process whose image we may not read. Either way: not provably ours.
    if exe.is_empty() {
        return false;
    }
    let Some(runtime) = runtime_dir.to_str().filter(|r| !r.is_empty()) else {
        return false;
    };
    // Windows paths are case-insensitive and PowerShell need not echo our
    // casing back.
    let exe = exe.to_lowercase();
    let mut prefix = runtime.to_lowercase();
    // The prefix has to end on a separator, or `...\runtime-old\python.exe`
    // would pass for `...\runtime` and the kill would land on a stranger.
    if !prefix.ends_with('\\') && !prefix.ends_with('/') {
        prefix.push('\\');
    }
    exe.starts_with(&prefix)
}

fn probe_headroom_http(port: u16, timeout: Duration) -> bool {
    use std::io::{Read, Write};
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 16];
    match stream.read(&mut buf) {
        Ok(n) if n >= 5 => buf[..5].eq_ignore_ascii_case(b"HTTP/"),
        _ => false,
    }
}

/// The process listening on `port`, as `(command, pid)`.
///
/// lsof lives in /usr/sbin on macOS but /usr/bin on Linux, and Debian-family
/// images often omit it entirely, so try both paths and then fall back to `ss`
/// (iproute2, present on every Linux we ship to).
///
/// `None` means "could not identify the listener" — never "nothing is
/// listening" and never "it is not ours". Both the port-reclaim kill path and
/// the stale-argv check hang off this, so neither may treat None as evidence.
fn listener_process(port: u16) -> Option<(String, u32)> {
    for lsof in ["/usr/sbin/lsof", "/usr/bin/lsof"] {
        // Only `-iTCP:{port}` — a bare `-iTCP` here would OR with the port
        // selector (lsof ORs `-i` options) and match every listening socket on
        // the machine, so the first row would be an unrelated daemon.
        let Ok(output) = crate::proc::command(lsof)
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
            .output()
        else {
            continue; // not installed at this path
        };
        if !output.status.success() {
            continue;
        }
        if let Some(found) = parse_lsof_listener(&String::from_utf8_lossy(&output.stdout)) {
            return Some(found);
        }
    }
    #[cfg(target_os = "linux")]
    return ss_listener(port);
    #[cfg(windows)]
    return windows_listener(port);
    #[cfg(not(any(target_os = "linux", windows)))]
    return None;
}

/// Windows has neither `lsof` nor `ss`, so `listener_process` returned `None`
/// for every port -- which is why `diagnose_proxy_port` could only ever say
/// "unknown process" there (RUST-7F) and why `reclaim_orphan_proxy` bailed
/// before it could reclaim anything. `netstat` and `tasklist` ship with every
/// Windows since XP and need no elevation for our own processes.
#[cfg(windows)]
fn windows_listener(port: u16) -> Option<(String, u32)> {
    let output = crate::proc::command("netstat")
        .args(["-ano"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let pid = parse_netstat_listener(&String::from_utf8_lossy(&output.stdout), port)?;

    // The image name is cosmetic (it goes into the occupant string); a pid we
    // could not name is still a pid worth reporting and gating a kill on.
    let image = crate::proc::command("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| parse_tasklist_image(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_else(|| "unnamed process".to_string());
    Some((image, pid))
}

/// The pid LISTENING on `port` in `netstat -ano` output.
///
/// Matches the port exactly rather than by suffix: `:16868` ends with `6868`,
/// and picking that row would point a kill at an unrelated process.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_netstat_listener(text: &str, port: u16) -> Option<u32> {
    text.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Proto, Local Address, Foreign Address, State, PID
        if fields.len() < 5 || !fields[0].eq_ignore_ascii_case("TCP") {
            return None;
        }
        if !fields[3].eq_ignore_ascii_case("LISTENING") {
            return None;
        }
        // rsplit: IPv6 rows are `[::1]:6868`, so only the last colon separates
        // the port.
        let (_, found) = fields[1].rsplit_once(':')?;
        (found.parse::<u16>().ok()? == port).then(|| fields[4].parse().ok())?
    })
}

/// The image name from one `tasklist /NH /FO CSV` row (`"python.exe","123",...`).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_tasklist_image(text: &str) -> Option<String> {
    let row = text.lines().find(|line| line.starts_with('"'))?;
    let name = row.strip_prefix('"')?.split('"').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// `(command, pid)` from the first data row of `lsof -nP -iTCP -sTCP:LISTEN`,
/// whose columns start `COMMAND PID`. Row 0 is the header.
fn parse_lsof_listener(text: &str) -> Option<(String, u32)> {
    let mut fields = text.lines().nth(1)?.split_whitespace();
    let command = fields.next()?.to_string();
    let pid = fields.next()?.parse().ok()?;
    Some((command, pid))
}

/// lsof-less fallback. `ss` only fills the `users:` field for processes the
/// caller owns, which covers the case that matters — our own backend. Someone
/// else's daemon on our port stays "unknown process", which is the honest
/// answer and routes to the fallback port instead of the kill path.
#[cfg(target_os = "linux")]
fn ss_listener(port: u16) -> Option<(String, u32)> {
    let output = crate::proc::command("ss").arg("-ltnp").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ss_listener(&String::from_utf8_lossy(&output.stdout), port)
}

/// Pick the LISTEN row whose local address ends in `:{port}` and pull
/// `(command, pid)` from its `users:(("python",pid=1234,fd=7))` field.
///
/// Filtered here rather than with an `ss` filter expression: the column layout
/// is stable across iproute2 versions, and a filter expression we got subtly
/// wrong would hand a *different* process's pid to the kill path.
// Only called on Linux, but compiled everywhere so the parser stays testable.
#[allow(dead_code)]
fn parse_ss_listener(text: &str, port: u16) -> Option<(String, u32)> {
    let suffix = format!(":{port}");
    let row = text.lines().find(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some("LISTEN") && fields.nth(2).is_some_and(|addr| addr.ends_with(&suffix))
    })?;
    let users = row.split("users:((").nth(1)?;
    let command = users.strip_prefix('"')?.split('"').next()?.to_string();
    let pid = users
        .split("pid=")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((command, pid))
}

/// `listener_process` formatted as the `"cmd pid 1234"` detail string the
/// port-conflict marker and its bail messages carry.
fn listener_detail(port: u16) -> Option<String> {
    listener_process(port).map(|(command, pid)| format!("{command} pid {pid}"))
}

/// Extract the numeric pid from a `"cmd pid 1234"` string returned by
/// [`listener_detail`]. Returns None for the `"unknown process"` placeholder
/// or any unparseable shape. Companion to `port_conflict::parse_occupant`,
/// which works on the full bail string instead of the occupant detail.
fn parse_pid_from_lsof_detail(detail: &str) -> Option<u32> {
    let idx = detail.rfind(" pid ")?;
    detail[idx + " pid ".len()..].trim().parse().ok()
}

/// Bail message when a previous (still-alive) headroom proxy holds the port.
/// Extracted as a function so the exact format is testable against
/// `port_conflict::is_port_conflict` and `state::classify_startup_error`.
fn format_already_running_bail(port: u16) -> String {
    format!(
        "headroom proxy already running on port {port} (likely a stale process from a prior session). \
         Run `lsof -iTCP:{port} -sTCP:LISTEN` to find and kill it, then retry."
    )
}

/// True when `/readyz` on the backend `port` answers with a 2xx — i.e. a
/// genuinely healthy headroom proxy is serving there. Used to avoid killing a
/// live backend during port reclaim. Short timeout: a hung orphan won't answer
/// in time and a healthy one answers in milliseconds.
fn probe_backend_readyz_ok(port: u16) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
    else {
        return false;
    };
    matches!(
        client.get(format!("http://127.0.0.1:{port}/readyz")).send(),
        Ok(resp) if resp.status().is_success()
    )
}

/// Poll until `port` is bindable or `timeout` elapses. Returns true once the
/// port is free. A killed listener's socket is released as soon as the owning
/// process dies, so this normally returns within a couple of poll intervals.
fn wait_for_port_free(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Reclaim `port` from an orphaned headroom proxy left behind by a prior
/// session. We only get here from the `HeadroomRunning` spawn pre-flight, which
/// is unreachable when a healthy proxy is up (its `/readyz` would have
/// satisfied `is_headroom_proxy_reachable` and short-circuited
/// `ensure_headroom_running`). Still, re-confirm health on the backend port
/// directly before killing — if it answers 2xx the backend is live (e.g. the
/// 6867 intercept is wedged while 6868 is fine) and we leave it alone. On any
/// failure to reclaim (no pid, refuses to die, healthy) we fall back to the
/// original bail so the caller's classification and user guidance are
/// unchanged.
///
/// `force_unhealthy_too`: during upgrade boot validation the orphan on 6868 is
/// the *old* version we are replacing — a still-healthy old worker (left when
/// `stop_headroom`'s argv pattern-kill missed the real socket holder) must be
/// killed anyway, or the new venv can't bind and the upgrade rolls back as
/// `not_started`. When set, skip the readyz health guard and reclaim regardless.
/// Signal a single pid, gracefully or forcefully.
///
/// Deliberately not `state::terminate_process_tree`: that one signals a process
/// GROUP on unix and refuses any pid we did not spawn, which is exactly what an
/// orphan from a previous app instance is. Callers must have passed
/// `pid_is_headroom_backend` first.
fn kill_pid(pid: u32, force: bool) {
    #[cfg(windows)]
    {
        // `/T` takes the subtree with it: the backend spawns helpers, and a
        // survivor holds the port just as well as its parent did.
        let mut command = crate::proc::command("taskkill");
        command.args(["/PID", &pid.to_string(), "/T"]);
        if force {
            command.arg("/F");
        }
        let _ = command.status();
    }
    #[cfg(not(windows))]
    {
        let mut command = crate::proc::command("/bin/kill");
        if force {
            command.arg("-KILL");
        }
        let _ = command.arg(pid.to_string()).status();
    }
}

fn reclaim_orphan_proxy(port: u16, force_unhealthy_too: bool) -> Result<()> {
    if !force_unhealthy_too && probe_backend_readyz_ok(port) {
        bail!("{}", format_already_running_bail(port));
    }
    let Some((_, pid)) = listener_process(port) else {
        bail!("{}", format_already_running_bail(port));
    };
    // Belt-and-braces identity gate (diagnose_proxy_port already filters, but
    // the upgrade flow reaches here with force set): never kill a pid that
    // doesn't look like our own backend.
    if !pid_is_headroom_backend(pid) {
        bail!("{}", format_already_running_bail(port));
    }

    log::warn!("[backend_port] reclaiming orphaned headroom proxy pid {pid} on port {port}");
    kill_pid(pid, false);
    if !wait_for_port_free(port, Duration::from_secs(3)) {
        kill_pid(pid, true);
        if !wait_for_port_free(port, Duration::from_secs(2)) {
            bail!("{}", format_already_running_bail(port));
        }
    }

    sentry::with_scope(
        |scope| {
            scope.set_tag("flow", "orphan_proxy_reclaimed");
            scope.set_extra("port", port.into());
            scope.set_extra("occupant_pid", pid.into());
        },
        || {
            sentry::capture_message(
                &format!("orphan_proxy_reclaimed: killed pid {pid} holding port {port}"),
                sentry::Level::Info,
            );
        },
    );
    Ok(())
}

/// Bail message when 6868 is foreign-held AND every port in the fallback
/// range is also taken. Must contain `"is occupied by a non-headroom process"`
/// so `port_conflict::is_port_conflict` continues to match, and the
/// `(occupant)` parenthetical so `port_conflict::parse_occupant` can extract
/// the cmd/pid for the persistent-conflict marker.
fn format_all_foreign_bail(default_port: u16, occupant: &str, range: (u16, u16)) -> String {
    let (start, end) = range;
    format!(
        "port {default_port} is occupied by a non-headroom process ({occupant}) and fallback ports {start}-{end} are also unavailable; cannot start proxy. \
         Reboot to clear stuck listeners, then relaunch Headroom."
    )
}

pub(crate) fn tail_log_file(path: &Path, max_lines: usize) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut lines: std::collections::VecDeque<String> =
        std::collections::VecDeque::with_capacity(max_lines);
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if lines.len() == max_lines {
            lines.pop_front();
        }
        lines.push_back(redact_sensitive(&line));
    }
    lines.into_iter().collect::<Vec<_>>().join("\n")
}

/// Strip Anthropic API keys and bearer tokens from log content before it gets
/// handed to Sentry. Without this, Sentry's default PII scrubber sees one
/// `sk-ant-…` and replaces the entire `proxy_log_tail` field with `[Filtered]`,
/// which is the single most diagnostic field in `proxy_unreachable_post_boot`.
/// Pre-redact so the rest of the line survives the scrubber.
fn redact_sensitive(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &line[i..];
        if let Some(consumed) = match_redactable(rest) {
            out.push_str("[REDACTED]");
            i += consumed;
        } else {
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// If `rest` starts with a redactable token, return the byte length to skip.
fn match_redactable(rest: &str) -> Option<usize> {
    if let Some(after) = rest.strip_prefix("sk-ant-") {
        let token_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
            .count();
        return Some("sk-ant-".len() + token_len);
    }
    for prefix in ["Bearer ", "bearer "] {
        if let Some(after) = rest.strip_prefix(prefix) {
            let token_len = after
                .bytes()
                .take_while(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(*b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
                })
                .count();
            if token_len >= 8 {
                return Some(prefix.len() + token_len);
            }
        }
    }
    None
}

/// Newest `headroom-proxy*.log` in the logs directory, if any.
pub(crate) fn newest_proxy_log_path(logs_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(logs_dir).ok()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("headroom-proxy") || !name_str.ends_with(".log") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                let path = entry.path();
                newest = Some(match newest {
                    Some((prev_time, prev_path)) if prev_time > mtime => (prev_time, prev_path),
                    _ => (mtime, path),
                });
            }
        }
    }
    newest.map(|(_, p)| p)
}

fn headroom_python_startup_args() -> Vec<String> {
    // The `python -m headroom.proxy.server` argparse does NOT define the learn
    // flags (--learn, --no-memory-tools, --no-memory-context, --memory-db-path);
    // those live only on the `headroom proxy` click entrypoint. Passing them
    // here makes argparse exit 2, so the fallback would always fail and mask
    // the real entrypoint failure under spurious noise. Keep this variant to
    // server-supported flags only.
    vec![
        "-m".to_string(),
        "headroom.proxy.server".to_string(),
        "--port".to_string(),
        headroom_proxy_port(),
        "--no-http2".to_string(),
        "--log-messages".to_string(),
    ]
}

/// The `headroom proxy` click entrypoint only defines `--no-http2` from
/// 0.28.0 (upstream e06b6167); 0.26.0/0.27.0 exit 2 with "No such option",
/// which made boot validation on the 0.26.0 fallback runtime fail every
/// attempt and time out (Sentry RUST-4A). Unknown/unparseable version means
/// the receipt is from a current install, so assume the pinned (>= 0.28.0)
/// runtime and keep the flag.
fn runtime_supports_no_http2(installed_version: Option<&str>) -> bool {
    let Some(version) = installed_version else {
        return true;
    };
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= (0, 28),
        _ => true,
    }
}

/// The unified `--no-ccr` flag replaced the split `--no-ccr-marker` /
/// `--no-ccr-inject-tool` pair in headroom-ai 0.31.0 (upstream ecc93991);
/// 0.30.0 and earlier exit 2 with "No such option", which would fail boot
/// validation on the 0.28.0 fallback runtime exactly as --no-http2 did on
/// 0.26.0 (Sentry RUST-4A). Verified against the tagged CLI: v0.30.0 carries
/// the split pair, v0.31.0+ the unified flag. Unknown/unparseable version means
/// the receipt is from a current install, so assume the pinned runtime.
fn runtime_supports_no_ccr(installed_version: Option<&str>) -> bool {
    let Some(version) = installed_version else {
        return true;
    };
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= (0, 31),
        _ => true,
    }
}

/// The "coding" savings persona only exists in `agent_savings._PROFILES` from
/// headroom-ai 0.30.0. On the 0.28.0 fallback runtime (chosen when 0.30.0 boot
/// validation times out) the set is {agent-90, balanced}, so `coding` makes the
/// proxy raise on startup and exit before opening the port (Sentry RUST-1M).
/// Below 0.30.0, fall back to "agent-90" — the runtime's own default, valid in
/// every version. Unknown/unparseable version = current install, keep "coding".
fn savings_profile_for_runtime(installed_version: Option<&str>) -> &'static str {
    let Some(version) = installed_version else {
        return "coding";
    };
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) if (major, minor) < (0, 30) => "agent-90",
        _ => "coding",
    }
}

/// First bundled runtime carrying the cc-switch reconciler's Official-branch
/// upstream reset (upstream PR #3166). The reconciler itself has shipped since
/// 0.29.0, but before the reset a switch back to Claude Official left the
/// previously captured third-party endpoint live on
/// `HeadroomProxy.ANTHROPIC_API_URL` -- a process-wide class attr -- so every
/// Anthropic client on this proxy kept reaching e.g. api.deepseek.com while
/// sending Anthropic OAuth credentials. Bump this the release the fix lands in;
/// until then the gate below keeps the flag off.
const CC_SWITCH_RESET_MIN_VERSION: (u64, u64, u64) = (0, 36, 3);

/// Whether to opt this runtime into the cc-switch reconciler.
///
/// Defaults to off for an unreadable version and for the 0.28.0 fallback the
/// app drops to when boot validation times out: enabling the reconciler on a
/// runtime without the reset is worse than leaving it off, because it only
/// misroutes AFTER the user has already switched providers once.
fn cc_switch_reconcile_for_runtime(installed_version: Option<&str>) -> &'static str {
    let Some(version) = installed_version else {
        return "0";
    };
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    let parsed = (
        parts.next().flatten(),
        parts.next().flatten(),
        parts.next().flatten(),
    );
    match parsed {
        (Some(major), Some(minor), Some(patch)) => {
            if (major, minor, patch) >= CC_SWITCH_RESET_MIN_VERSION {
                "1"
            } else {
                "0"
            }
        }
        _ => "0",
    }
}

fn headroom_entrypoint_startup_args(
    installed_version: Option<&str>,
    learn_enabled: bool,
) -> Vec<String> {
    // HTTP/2 to upstream is disabled both ways: the explicit --no-http2 flag
    // AND the HEADROOM_HTTP2=false env var (set in the spawn env). Either alone
    // suffices, but older bundled runtimes ignored the env var and ran HTTP/2
    // unconditionally, which surfaced as SSLV3_ALERT_BAD_RECORD_MAC under
    // multi-tab concurrency. The flag is belt-and-suspenders against a future
    // runtime regressing on the env var — but only on runtimes whose click
    // entrypoint defines it (see runtime_supports_no_http2). --log-messages
    // stores full request/response bodies so the desktop's Activity tab can
    // render the live transformations feed.
    let mut args = vec![
        "proxy".to_string(),
        "--port".to_string(),
        headroom_proxy_port(),
    ];
    if runtime_supports_no_http2(installed_version) {
        args.push("--no-http2".to_string());
    }
    args.push("--log-messages".to_string());
    // CCR off: with headroom_retrieve injected into the tools array, every
    // stream:true turn is rewritten to a buffered stream:false upstream call
    // and re-synthesized as SSE (`buffered_stream_ccr`, upstream #1451/#2479).
    // That wrapper commits `http.response.start` with status 200 +
    // text/event-stream after a 1s keepalive, before the upstream outcome is
    // known; when the buffered call then returns anything that is not a
    // StreamingResponse (unparseable body, or a real 429/500/529) it can only
    // emit a bare `event: error` with no message_start, so the client sees
    // "API returned an empty or malformed response (HTTP 200)" and, because the
    // status is 200, never retries. Upstream's own help text names --no-ccr as
    // the right setting for streaming clients. Turning it off also restores
    // real streaming and puts those turns back through
    // StreamingMixin._stream_response, which is where SITECUSTOMIZE_PY's
    // context-limit guard (#2942) attaches — the buffered path bypassed it
    // entirely. Present since 0.33.0, so this is not a 0.35.0 regression.
    if runtime_supports_no_ccr(installed_version) {
        args.push("--no-ccr".to_string());
    }
    if learn_enabled {
        args.extend(headroom_learn_startup_args());
    }
    args
}

/// Flags whose presence in the running proxy's argv we treat as proof that it
/// was started by this build. If any of these are missing, the proxy was
/// spawned by an older desktop (or by something else) and we restart it.
/// With auto-learning off the learn flags are not passed, so they drop out of
/// the signature too.
fn expected_proxy_arg_signature(learn_enabled: bool) -> Vec<&'static str> {
    let mut flags = vec!["--port", "--log-messages"];
    if learn_enabled {
        flags.extend([
            "--learn",
            "--no-memory-tools",
            "--no-memory-context",
            "--memory-db-path",
        ]);
    }
    flags
}

/// Returns the full command line of whatever process is currently listening on
/// the proxy port, or `None` if we couldn't determine it.
pub fn running_proxy_argv() -> Option<String> {
    let (_, pid) = listener_process(backend_port::get())?;
    ps_command(pid)
}

/// Identity of whatever is listening on `port`, for diagnostics that need to
/// tell a foreign squatter from an orphaned old Headroom: a port that answers
/// HTTP without the backend's routes is invisible to the readyz gate (a 404
/// there deliberately counts as reachable), so the listener's name is the one
/// fact that resolves the ambiguity. Best-effort: `None` where lsof is
/// unavailable (Windows), and the caller's message must read fine without it.
pub(crate) fn listener_identity(port: u16) -> Option<String> {
    let (cmd, pid) = listener_process(port)?;
    Some(format_listener_identity(
        &cmd,
        pid,
        ps_command(pid).as_deref(),
    ))
}

fn format_listener_identity(cmd: &str, pid: u32, argv: Option<&str>) -> String {
    const MAX_ARGV: usize = 160;
    let argv = argv.map(str::trim).unwrap_or_default();
    if argv.is_empty() {
        return format!("{cmd} (pid {pid})");
    }
    let mut argv = argv.to_string();
    if argv.len() > MAX_ARGV {
        let cut = (0..=MAX_ARGV)
            .rev()
            .find(|i| argv.is_char_boundary(*i))
            .unwrap_or(0);
        argv.truncate(cut);
        argv.push_str("...");
    }
    format!("{cmd} (pid {pid}): {argv}")
}

/// True if the running proxy's argv contains every flag we expect this build
/// to pass. Used to detect proxies left over from an older desktop version.
pub fn running_proxy_matches_expected_args() -> bool {
    let Some(argv) = running_proxy_argv() else {
        // Fail open. The caller kills the backend when this is false, and
        // "we could not read the listener's argv" is not evidence that it is
        // stale. Returning false here meant every ensure-running pass on a
        // host we cannot introspect killed and respawned a healthy backend.
        return true;
    };
    proxy_argv_contains_expected_flags(&argv, !crate::client_adapters::is_auto_learn_disabled())
}

fn proxy_argv_contains_expected_flags(argv: &str, learn_enabled: bool) -> bool {
    // A proxy still carrying --learn after the user turned auto-learning off is
    // as stale as one missing a flag: restart it so the opt-out takes effect.
    if !learn_enabled && argv_contains_flag(argv, "--learn") {
        return false;
    }
    expected_proxy_arg_signature(learn_enabled)
        .iter()
        .all(|flag| argv_contains_flag(argv, flag))
}

/// Whitespace-aware containment check so `--port` doesn't match `--port-foo`
/// and `--learn` doesn't match `--no-learn`.
fn argv_contains_flag(argv: &str, flag: &str) -> bool {
    argv.split_whitespace().any(|tok| tok == flag)
}

fn ps_command(pid: u32) -> Option<String> {
    let output = crate::proc::command("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// If `log_tail` shows pydantic refusing to import because the installed
/// `pydantic-core` doesn't match what the bundled pydantic wants, return the
/// version pydantic wants. The error message is the source of truth — pydantic
/// prints the exact pinned version it expects.
///
/// Example line we match:
///     SystemError: The installed pydantic-core version (2.46.3) is
///     incompatible with the current pydantic version, which requires 2.41.5.
fn extract_required_pydantic_core_version(log_tail: &str) -> Option<String> {
    if !log_tail.contains("pydantic-core") {
        return None;
    }
    let marker = "which requires ";
    let idx = log_tail.find(marker)?;
    let after = &log_tail[idx + marker.len()..];
    let version: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let trimmed = version.trim_end_matches('.');
    if trimmed.is_empty() || !trimmed.contains('.') {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Child-process logs are opened append-mode on every launch and otherwise
/// grow unbounded. Rename to `.log.old` (keeping one prior generation, same
/// scheme as logging.rs) once the file exceeds the cap, before reopening.
/// Read the last `max_bytes` of a log file, for folding into an error message.
/// Returns an empty string if the file is missing or unreadable — best-effort
/// diagnostics, never a hard failure.
fn log_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max_bytes {
        use std::io::Seek;
        let _ = f.seek(std::io::SeekFrom::Start(len - max_bytes));
    }
    let mut buf = String::new();
    let _ = f.read_to_string(&mut buf);
    buf.trim().to_string()
}

fn rotate_log_if_large(path: &Path) {
    const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
    let too_big = std::fs::metadata(path)
        .map(|m| m.len() > MAX_LOG_BYTES)
        .unwrap_or(false);
    if too_big {
        let backup = path.with_extension("log.old");
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::rename(path, &backup);
    }
}

/// Make a string safe to use as part of a filename: replace path separators
/// (`/`, `\`) and other characters that have meaning to the filesystem with
/// `_`, then truncate so absurdly long argv strings don't blow past
/// per-component name limits (255 bytes on most filesystems).
fn sanitize_log_variant(raw: &str) -> String {
    const MAX_LEN: usize = 80;
    let mut out: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' | '\n' | '\r' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    if out.len() > MAX_LEN {
        out.truncate(MAX_LEN);
    }
    out
}

/// Args that enable passive learning: the proxy extracts patterns from live
/// traffic into the memory store, but does not inject memory tools or context
/// into requests (so the model's view of the conversation is unchanged).
fn headroom_learn_startup_args() -> Vec<String> {
    vec![
        "--learn".to_string(),
        "--no-memory-tools".to_string(),
        "--no-memory-context".to_string(),
        "--memory-db-path".to_string(),
        crate::headroom_memory_db_path().display().to_string(),
    ]
}

fn headroom_propagated_proxy_log_path() -> Option<PathBuf> {
    let path = crate::edition::workspace_dir()
        .join("logs")
        .join("proxy.log");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

struct DownloadArtifact {
    url: String,
    sha256: Option<&'static str>,
}

/// Metadata for a specific headroom-ai release fetched from PyPI.
pub(crate) struct HeadroomRelease {
    version: String,
    wheel_url: String,
    sha256: String,
}

impl HeadroomRelease {
    pub fn version(&self) -> &str {
        &self.version
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeMaintenanceKind {
    Upgrade,
    RequirementsRepair,
}

/// Outcome of [`ToolManager::atomic_upgrade_headroom`].
///
/// `InstalledPendingValidation` means install + smoke test succeeded but the
/// backup is still on disk. The caller must either commit or rollback.
pub enum UpgradeOutcome {
    InstalledPendingValidation {
        /// Last ~100 lines of pip stdout/stderr from this install. Attached
        /// to the boot-validation Sentry event when it later fails — pip
        /// can return exit 0 while leaving the venv in a broken state
        /// (skipped packages, downgraded native deps with mismatched ABI,
        /// etc.), and without the tail there's no record of what actually
        /// happened. Empty string when capture was skipped (e.g., bootstrap).
        pip_output_tail: String,
    },
    InstallFailed {
        /// True if we successfully restored the old venv + receipt.
        restored: bool,
        error: anyhow::Error,
    },
}

/// Bounded ring buffer collecting pip stdout/stderr lines for post-mortem
/// diagnostics. Keeps the LAST `max_lines` (drops oldest when full) so
/// warnings, "Skipping X", "Successfully installed ..." lines that pip
/// prints near the end of a run survive. Sentry extras cap at ~16KB; 100
/// lines at the typical ~120-char pip line averages ~12KB.
pub(crate) struct PipOutputCapture {
    lines: std::collections::VecDeque<String>,
    max_lines: usize,
}

impl PipOutputCapture {
    pub(crate) fn new(max_lines: usize) -> Self {
        Self {
            lines: std::collections::VecDeque::with_capacity(max_lines),
            max_lines,
        }
    }

    pub(crate) fn push(&mut self, line: &str) {
        if self.lines.len() >= self.max_lines {
            self.lines.pop_front();
        }
        self.lines.push_back(line.to_string());
    }

    pub(crate) fn into_string(self) -> String {
        let parts: Vec<String> = self.lines.into_iter().collect();
        parts.join("\n")
    }
}

/// State required to perform (and roll back) an in-place upgrade — i.e. an
/// upgrade that mutates the live venv instead of rebuilding it. When
/// `previous_lock_backup` is `Some`, the dep lock has churned and the file at
/// that path is the pre-upgrade lock content, used by rollback and recovery
/// to `pip install --upgrade -r <backup>` back to the prior pin set.
pub(crate) struct InPlaceUpgradeContext {
    pub(crate) previous_version: String,
    pub(crate) previous_lock_backup: Option<PathBuf>,
}

/// Best-effort free-bytes query for the volume backing `path`. Returns None
/// on error — callers should treat that as "don't block on disk space".
#[cfg(unix)]
fn available_disk_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret != 0 {
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(windows)]
fn available_disk_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let existing = path
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();

    let mut free_bytes_available: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            existing.as_ptr(),
            &mut free_bytes_available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    Some(free_bytes_available)
}

/// Pinned headroom-ai wheel for the running platform. The wheel carries a
/// native `_core` extension, so a macOS wheel installed on Windows/Linux
/// yields `ModuleNotFoundError: No module named 'headroom._core'` at proxy
/// start (RUST-6E: every 0.7.7 Windows install). Keep this in step with
/// `python_distribution_artifact`'s platform matrix.
#[derive(Debug, Clone, Deserialize)]
struct PyPiFile {
    filename: String,
    url: String,
    digests: PyPiDigests,
    packagetype: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PyPiDigests {
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct PyPiReleaseResponse {
    info: PyPiReleaseInfo,
    urls: Vec<PyPiFile>,
}

#[derive(Debug, Deserialize)]
struct PyPiReleaseInfo {
    version: String,
}

fn headroom_wheel_platform_tag() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("macosx_11_0_arm64"),
        ("macos", "x86_64") => Ok("macosx_10_12_x86_64"),
        ("linux", "aarch64") => Ok("manylinux_2_28_aarch64"),
        ("linux", "x86_64") => Ok("manylinux_2_28_x86_64"),
        ("windows", "x86_64") => Ok("win_amd64"),
        (os, arch) => bail!("unsupported headroom-ai wheel target: {os}/{arch}"),
    }
}

fn parse_headroom_release(value: &Value, requested_version: &str) -> Result<HeadroomRelease> {
    let requested = stable_package_version(requested_version)?;
    let response: PyPiReleaseResponse = serde_json::from_value(value.clone())?;
    let published = stable_package_version(&response.info.version)?;
    if published != requested {
        bail!("PyPI release version does not match requested version");
    }
    let platform = headroom_wheel_platform_tag()?;
    let prefix = format!("headroom_ai-{requested}-");
    let mut wheels = response.urls.into_iter().filter(|file| {
        file.packagetype == "bdist_wheel"
            && file.filename.starts_with(&prefix)
            && file.filename.ends_with(".whl")
            && file.filename.contains(platform)
    });
    let file = wheels
        .clone()
        .find(|file| file.filename.contains("cp312-cp312-"))
        .or_else(|| wheels.find(|file| file.filename.contains("cp312-abi3-")))
        .or_else(|| wheels.find(|file| file.filename.contains("abi3-")))
        .ok_or_else(|| anyhow!("PyPI has no compatible headroom-ai wheel for this platform"))?;
    let parsed = reqwest::Url::parse(&file.url).context("invalid PyPI wheel URL")?;
    if parsed.scheme() != "https"
        || !matches!(
            parsed.host_str(),
            Some("pypi.org") | Some("files.pythonhosted.org")
        )
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.path().starts_with("/packages/")
            && parsed.host_str() == Some("files.pythonhosted.org")
    {
        bail!("PyPI wheel URL is not trusted");
    }
    if file.digests.sha256.len() != 64
        || !file.digests.sha256.chars().all(|c| c.is_ascii_hexdigit())
    {
        bail!("PyPI wheel is missing a valid sha256 digest");
    }
    Ok(HeadroomRelease {
        version: requested.into(),
        wheel_url: file.url,
        sha256: file.digests.sha256.to_ascii_lowercase(),
    })
}

fn fetch_headroom_release(version: &str) -> Result<HeadroomRelease> {
    stable_package_version(version)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Headroom Desktop")
        .build()?;
    let value: Value = client
        .get(format!("https://pypi.org/pypi/headroom-ai/{version}/json"))
        .send()?
        .error_for_status()?
        .json()?;
    parse_headroom_release(&value, version)
}

fn pinned_headroom_release() -> Result<HeadroomRelease> {
    let (url, sha256) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("https://files.pythonhosted.org/packages/1b/99/410b64a578f36d249b76915d733873192537e986b2bc911373e6d72839e9/headroom_ai-0.36.5-cp310-abi3-macosx_11_0_arm64.whl", "0190c55f022760d49f6268f3c0970438f9bdd5a72bc0288e2788ff7e8b8ce730"),
        ("macos", "x86_64") => ("https://files.pythonhosted.org/packages/3a/79/8db10dd06c45c942c2f293b3748d70c8ac53ba048b7796b947f78fcdf952/headroom_ai-0.36.5-cp310-abi3-macosx_10_12_x86_64.whl", "c62f18e261909a3de43c32113ec76c9ad6580e8fac6dc83347a71403d66094bf"),
        ("linux", "aarch64") => ("https://files.pythonhosted.org/packages/68/58/97538fcca4505a130e7f65e5a736e1672f3b339f34dde0b86f07a9220edc/headroom_ai-0.36.5-cp310-abi3-manylinux_2_28_aarch64.whl", "378ac86ea3d188014be8c98e5af2d5b64b4ac6ad566bcdc95714dcdf02851932"),
        ("linux", "x86_64") => ("https://files.pythonhosted.org/packages/15/c9/650195df8133b0f2ae5156bd9780fd70e17d8cead361016983e72d629697/headroom_ai-0.36.5-cp310-abi3-manylinux_2_28_x86_64.whl", "56954b76cd10b5312061725e7470575598a4bde3fa7f80f77d82a08a71fceae8"),
        ("windows", "x86_64") => ("https://files.pythonhosted.org/packages/87/55/4f68afd415361200c9339ac98ee30ed2ceae573136d852b17f1b276e1c36/headroom_ai-0.36.5-cp310-abi3-win_amd64.whl", "f842990c69e39d967982ef9bd1da20c12b9a7f39507e4f76d638b44f93114986"),
        (os, arch) => bail!("unsupported headroom-ai wheel target: {os}/{arch}"),
    };
    Ok(HeadroomRelease {
        version: HEADROOM_PINNED_VERSION.into(),
        wheel_url: url.into(),
        sha256: sha256.into(),
    })
}

fn python_distribution_artifact() -> Result<DownloadArtifact> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok(DownloadArtifact {
            url: format!(
                "https://github.com/astral-sh/python-build-standalone/releases/download/{}/cpython-3.12.12+20251014-aarch64-apple-darwin-install_only_stripped.tar.gz",
                PYTHON_STANDALONE_RELEASE
            ),
            sha256: Some(PYTHON_SHA256_MACOS_AARCH64),
        }),
        ("macos", "x86_64") => Ok(DownloadArtifact {
            url: format!(
                "https://github.com/astral-sh/python-build-standalone/releases/download/{}/cpython-3.12.12+20251014-x86_64-apple-darwin-install_only_stripped.tar.gz",
                PYTHON_STANDALONE_RELEASE
            ),
            sha256: Some(PYTHON_SHA256_MACOS_X86_64),
        }),
        ("linux", "x86_64") => Ok(DownloadArtifact {
            url: format!(
                "https://github.com/astral-sh/python-build-standalone/releases/download/{}/cpython-3.12.12+20251014-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz",
                PYTHON_STANDALONE_RELEASE
            ),
            sha256: Some(PYTHON_SHA256_LINUX_X86_64),
        }),
        ("linux", "aarch64") => Ok(DownloadArtifact {
            url: format!(
                "https://github.com/astral-sh/python-build-standalone/releases/download/{}/cpython-3.12.12+20251014-aarch64-unknown-linux-gnu-install_only_stripped.tar.gz",
                PYTHON_STANDALONE_RELEASE
            ),
            sha256: Some(PYTHON_SHA256_LINUX_AARCH64),
        }),
        ("windows", "x86_64") => Ok(DownloadArtifact {
            url: format!(
                "https://github.com/astral-sh/python-build-standalone/releases/download/{}/cpython-3.12.12+20251014-x86_64-pc-windows-msvc-install_only_stripped.tar.gz",
                PYTHON_STANDALONE_RELEASE
            ),
            sha256: Some(PYTHON_SHA256_WINDOWS_X86_64),
        }),
        (os, arch) => bail!("unsupported Headroom managed Python target: {os}/{arch}"),
    }
}

fn rtk_distribution_artifact() -> Result<DownloadArtifact> {
    let (target, sha256, extension) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("aarch64-apple-darwin", RTK_SHA256_MACOS_AARCH64, "tar.gz"),
        ("macos", "x86_64") => ("x86_64-apple-darwin", RTK_SHA256_MACOS_X86_64, "tar.gz"),
        ("linux", "aarch64") => (
            "aarch64-unknown-linux-gnu",
            RTK_SHA256_LINUX_AARCH64,
            "tar.gz",
        ),
        ("linux", "x86_64") => (
            "x86_64-unknown-linux-musl",
            RTK_SHA256_LINUX_X86_64,
            "tar.gz",
        ),
        ("windows", "x86_64") => ("x86_64-pc-windows-msvc", RTK_SHA256_WINDOWS_X86_64, "zip"),
        (os, arch) => bail!("unsupported RTK target: {os}/{arch}"),
    };

    Ok(DownloadArtifact {
        url: format!(
            "https://github.com/rtk-ai/rtk/releases/download/v{}/rtk-{}.{}",
            RTK_VERSION, target, extension
        ),
        sha256: Some(sha256),
    })
}

#[derive(Debug, Deserialize)]
struct RtkReleaseAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RtkReleaseResponse {
    tag_name: String,
    assets: Vec<RtkReleaseAsset>,
}

struct RtkReleaseArtifact {
    version: String,
    url: String,
    sha256: String,
}

fn valid_rtk_version(version: &str) -> bool {
    let version = version.strip_prefix('v').unwrap_or(version);
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn rtk_version_output_matches(output: &str, version: &str) -> bool {
    output
        .split_whitespace()
        .any(|token| token == version || token.strip_prefix('v') == Some(version))
}

fn rtk_version_is_downgrade(current: &str, target: &str) -> bool {
    matches!(
        (parse_major_minor_patch(current), parse_major_minor_patch(target)),
        (Some(current), Some(target)) if target < current
    )
}

fn rtk_release_asset_name() -> Result<String> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => bail!("unsupported RTK target: {os}/{arch}"),
    };
    let extension = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    };
    Ok(format!("rtk-{target}.{extension}"))
}

fn parse_rtk_release(value: &Value, requested_version: &str) -> Result<RtkReleaseArtifact> {
    let requested = requested_version
        .strip_prefix('v')
        .unwrap_or(requested_version);
    if !valid_rtk_version(requested) {
        bail!("invalid RTK version: {requested_version}");
    }
    let release: RtkReleaseResponse = serde_json::from_value(value.clone())?;
    let tag_version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    if tag_version != requested || !valid_rtk_version(tag_version) {
        bail!("RTK release tag does not match requested version");
    }
    let asset_name = rtk_release_asset_name()?;
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| anyhow!("RTK release has no asset for this platform"))?;
    if !asset
        .browser_download_url
        .starts_with("https://github.com/rtk-ai/rtk/releases/download/")
    {
        bail!("RTK release asset URL is not trusted");
    }
    let parsed_url = reqwest::Url::parse(&asset.browser_download_url)
        .context("invalid RTK release asset URL")?;
    let expected_path = format!("/rtk-ai/rtk/releases/download/v{requested}/{asset_name}");
    if parsed_url.scheme() != "https"
        || parsed_url.host_str() != Some("github.com")
        || parsed_url.query().is_some()
        || parsed_url.fragment().is_some()
        || parsed_url.path() != expected_path
    {
        bail!("RTK release asset URL not trusted");
    }
    let digest = asset.digest.as_deref().unwrap_or("");
    let sha256 = digest.strip_prefix("sha256:").unwrap_or("");
    if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("RTK release asset is missing a valid sha256 digest");
    }
    Ok(RtkReleaseArtifact {
        version: requested.to_string(),
        url: asset.browser_download_url,
        sha256: sha256.to_ascii_lowercase(),
    })
}

fn fetch_rtk_release(version: &str) -> Result<RtkReleaseArtifact> {
    if !valid_rtk_version(version) {
        bail!("invalid RTK version: {version}");
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent("Headroom")
        .timeout(Duration::from_secs(30))
        .build()?;
    let value: Value = client
        .get(format!(
            "https://api.github.com/repos/rtk-ai/rtk/releases/tags/v{}",
            version.strip_prefix('v').unwrap_or(version)
        ))
        .send()?
        .error_for_status()?
        .json()?;
    parse_rtk_release(&value, version)
}

/// Why this addon cannot be installed on the current OS/arch, in one sentence
/// the Addons tab shows in place of an Install button that could only ever
/// error. Keyed off the same artifact resolvers the installers call, so a newly
/// published target re-enables the card with no edit here.
fn addon_unavailable_reason(id: &str) -> Option<String> {
    let platform = match std::env::consts::OS {
        "windows" => "Windows",
        "linux" => "Linux",
        "macos" => "macOS",
        other => other,
    };
    match id {
        "rtk" if rtk_distribution_artifact().is_err() => Some(format!(
            "Not available on {platform} {}: RTK publishes no build for this architecture yet.",
            std::env::consts::ARCH
        )),
        "codebase-memory" if codebase_memory_distribution_artifact().is_err() => Some(format!(
            "Not available on {platform}: codebase-memory publishes macOS and Linux binaries only."
        )),
        _ => None,
    }
}

fn codebase_memory_distribution_artifact() -> Result<DownloadArtifact> {
    let (target, sha256) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("darwin-arm64", CODEBASE_MEMORY_SHA256_MACOS_AARCH64),
        ("macos", "x86_64") => ("darwin-amd64", CODEBASE_MEMORY_SHA256_MACOS_X86_64),
        ("linux", "aarch64") => ("linux-arm64", CODEBASE_MEMORY_SHA256_LINUX_AARCH64),
        ("linux", "x86_64") => ("linux-amd64", CODEBASE_MEMORY_SHA256_LINUX_X86_64),
        (os, arch) => bail!("unsupported codebase-memory target: {os}/{arch}"),
    };

    Ok(DownloadArtifact {
        url: format!(
            "https://github.com/DeusData/codebase-memory-mcp/releases/download/v{}/codebase-memory-mcp-{}.tar.gz",
            CODEBASE_MEMORY_VERSION, target
        ),
        sha256: Some(sha256),
    })
}

#[derive(Debug, Deserialize)]
struct CodebaseMemoryReleaseAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodebaseMemoryReleaseResponse {
    tag_name: String,
    assets: Vec<CodebaseMemoryReleaseAsset>,
}

fn valid_codebase_memory_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn codebase_memory_release_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("darwin-arm64"),
        ("macos", "x86_64") => Ok("darwin-amd64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("linux", "x86_64") => Ok("linux-amd64"),
        (os, arch) => bail!("unsupported codebase-memory target: {os}/{arch}"),
    }
}

fn parse_codebase_memory_release(
    value: &Value,
    requested_version: &str,
) -> Result<DownloadArtifact> {
    if !valid_codebase_memory_version(requested_version) {
        bail!("invalid codebase-memory version: {requested_version}");
    }
    let release: CodebaseMemoryReleaseResponse = serde_json::from_value(value.clone())?;
    let tag_version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    if tag_version != requested_version || !valid_codebase_memory_version(tag_version) {
        bail!("codebase-memory release tag does not match requested version");
    }
    let asset_name = format!(
        "codebase-memory-mcp-{}.tar.gz",
        codebase_memory_release_target()?
    );
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| anyhow!("codebase-memory release has no asset for this platform"))?;
    let parsed_url = reqwest::Url::parse(&asset.browser_download_url)
        .context("invalid codebase-memory release asset URL")?;
    let expected_path = format!(
        "/DeusData/codebase-memory-mcp/releases/download/v{requested_version}/{asset_name}"
    );
    if parsed_url.scheme() != "https"
        || parsed_url.host_str() != Some("github.com")
        || parsed_url.path() != expected_path
        || parsed_url.query().is_some()
        || parsed_url.fragment().is_some()
    {
        bail!("codebase-memory release asset URL not trusted");
    }
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow!("codebase-memory release asset has no valid SHA-256 digest"))?;
    Ok(DownloadArtifact {
        url: asset.browser_download_url,
        sha256: Some(Box::leak(sha256.to_ascii_lowercase().into_boxed_str())),
    })
}

fn fetch_codebase_memory_release(version: &str) -> Result<DownloadArtifact> {
    if !valid_codebase_memory_version(version) {
        bail!("invalid codebase-memory version: {version}");
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent("Headroom")
        .timeout(Duration::from_secs(30))
        .build()?;
    let value: Value = client
        .get(format!(
            "https://api.github.com/repos/DeusData/codebase-memory-mcp/releases/tags/v{}",
            version
        ))
        .send()?
        .error_for_status()?
        .json()?;
    parse_codebase_memory_release(&value, version)
}

fn download_to_path(url: &str, destination: &Path, expected_sha256: Option<&str>) -> Result<()> {
    download_to_path_with_progress(url, destination, expected_sha256, |_, _| {})
}

/// True when a `download_to_path` failure was a sha256 mismatch (see the
/// `bail!` in `download_to_path_with_progress`) rather than a transfer error.
/// A mismatch means the bytes were tampered with or corrupted in transit —
/// exactly the case the hash exists for — so callers must never respond to it
/// by falling back to an unverified pip-index install.
fn is_checksum_mismatch(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("checksum mismatch")
}

/// Download `url` to `destination` with an optional progress callback.
///
/// The callback receives `(downloaded_bytes, total_bytes)` and is called at
/// most every 250ms during a streaming download. `total_bytes` is `None` when
/// the server does not provide a Content-Length header.
fn download_to_path_with_progress<F>(
    url: &str,
    destination: &Path,
    expected_sha256: Option<&str>,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, Option<u64>),
{
    // One artifact download at a time, process-wide. The pre-consent prefetch
    // and the consented bootstrap can otherwise race the same `.partial`
    // file; holding the lock across the exists+sha check below means
    // whichever caller runs second sees the finished file and skips.
    // ponytail: a bootstrap that starts mid-prefetch blocks on a static
    // progress frame until the in-flight artifact completes; cancellation is
    // the upgrade path if that stall ever matters.
    static ARTIFACT_DOWNLOAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _download_guard = ARTIFACT_DOWNLOAD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if destination.exists() {
        if let Some(expected_sha256) = expected_sha256 {
            match verify_sha256_file(destination, expected_sha256) {
                Ok(()) => return Ok(()),
                Err(_) => {
                    std::fs::remove_file(destination)
                        .with_context(|| format!("removing {}", destination.display()))?;
                }
            }
        } else {
            return Ok(());
        }
    }

    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!(
            "headroom-local-community/",
            env!("CARGO_PKG_VERSION")
        ))
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(30 * 60))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .context("building download client")?;

    let tmp_path = destination.with_extension("partial");
    const MAX_ATTEMPTS: u32 = 5;
    let mut last_err = anyhow::anyhow!("no attempts made");

    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            // 2s, 4s, 8s, 16s between attempts.
            std::thread::sleep(Duration::from_secs(1u64 << attempt));
        }
        let _ = std::fs::remove_file(&tmp_path);

        let result = (|| -> Result<()> {
            let mut response = client
                .get(url)
                .send()
                .with_context(|| format!("downloading {}", url))?
                .error_for_status()
                .with_context(|| format!("downloading {}", url))?;

            let total_bytes = response.content_length();
            let mut file = std::fs::File::create(&tmp_path)
                .with_context(|| format!("creating {}", tmp_path.display()))?;
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut downloaded: u64 = 0;
            on_progress(0, total_bytes);
            let mut last_emit = Instant::now();

            loop {
                let n = response.read(&mut buf).context("reading download body")?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])
                    .with_context(|| format!("writing {}", tmp_path.display()))?;
                hasher.update(&buf[..n]);
                downloaded += n as u64;
                if last_emit.elapsed() >= Duration::from_millis(250) {
                    on_progress(downloaded, total_bytes);
                    last_emit = Instant::now();
                }
            }
            file.flush().context("flushing download")?;
            drop(file);
            on_progress(downloaded, total_bytes);

            if let Some(expected_sha256) = expected_sha256 {
                let actual_checksum = format!("{:x}", hasher.finalize());
                if actual_checksum != expected_sha256 {
                    bail!(
                        "checksum mismatch for {}: expected {}, got {}",
                        url,
                        expected_sha256,
                        actual_checksum
                    );
                }
            }

            std::fs::rename(&tmp_path, destination).with_context(|| {
                format!(
                    "renaming {} to {}",
                    tmp_path.display(),
                    destination.display()
                )
            })?;
            Ok(())
        })();

        match result {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }

    let _ = std::fs::remove_file(&tmp_path);
    Err(last_err)
}

fn verify_sha256_file(path: &Path, expected_sha256: &str) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let actual_checksum = sha256_bytes(&bytes);
    if actual_checksum != expected_sha256 {
        bail!(
            "checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected_sha256,
            actual_checksum
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct HeadroomLearnMetadataCandidate {
    metadata: HeadroomLearnMetadata,
    sort_key: Option<DateTime<Utc>>,
}

fn read_headroom_learn_metadata_from_path(path: &Path) -> Option<HeadroomLearnMetadataCandidate> {
    let content = std::fs::read_to_string(path).ok()?;
    let start = content.find("<!-- headroom:learn:start -->")?;
    let end = content.find("<!-- headroom:learn:end -->")?;
    if end <= start {
        return None;
    }

    let block = &content[start..end];
    let pattern_count = count_headroom_learn_patterns(block);
    let learned_at = parse_headroom_learn_timestamp(block);
    let modified_at = std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .map(DateTime::<Utc>::from);

    Some(HeadroomLearnMetadataCandidate {
        metadata: HeadroomLearnMetadata {
            learned_at: learned_at
                .map(|timestamp| timestamp.to_rfc3339())
                .or_else(|| modified_at.map(|timestamp| timestamp.to_rfc3339())),
            pattern_count,
        },
        sort_key: learned_at.or(modified_at),
    })
}

fn count_headroom_learn_patterns(block: &str) -> Option<usize> {
    let count = block
        .lines()
        .filter(|line| line.trim_start().starts_with("- "))
        .count();

    if count > 0 {
        Some(count)
    } else {
        None
    }
}

fn parse_headroom_learn_timestamp(block: &str) -> Option<DateTime<Utc>> {
    const PREFIX: &str = "*Auto-generated by `headroom learn` on ";

    block.lines().find_map(|line| {
        let trimmed = line.trim();
        let rest = trimmed.strip_prefix(PREFIX)?;
        let token: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || matches!(ch, '-' | ':' | 'T' | 'Z' | '+'))
            .collect();
        if token.is_empty() {
            return None;
        }

        DateTime::parse_from_rfc3339(&token)
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .ok()
            .or_else(|| {
                NaiveDate::parse_from_str(&token, "%Y-%m-%d")
                    .ok()
                    .and_then(|date| date.and_hms_opt(0, 0, 0))
                    .map(|timestamp| DateTime::<Utc>::from_naive_utc_and_offset(timestamp, Utc))
            })
    })
}

/// Parse sections and bullets inside the managed `<!-- headroom:learn -->`
/// block. Returns an empty Vec if no block is present.
pub fn parse_headroom_learn_block(file_content: &str) -> Vec<crate::models::AppliedSection> {
    use crate::models::AppliedSection;
    let Some(start) = file_content.find("<!-- headroom:learn:start -->") else {
        return Vec::new();
    };
    let Some(end_rel) = file_content[start..].find("<!-- headroom:learn:end -->") else {
        return Vec::new();
    };
    let block = &file_content[start..start + end_rel];

    let mut sections: Vec<AppliedSection> = Vec::new();
    let mut current: Option<AppliedSection> = None;

    for line in block.lines() {
        let trimmed = line.trim_start();
        if let Some(title) = trimmed.strip_prefix("### ") {
            if let Some(sec) = current.take() {
                sections.push(sec);
            }
            current = Some(AppliedSection {
                title: title.trim().to_string(),
                bullets: Vec::new(),
            });
        } else if let Some(sec) = current.as_mut() {
            if let Some(rest) = trimmed.strip_prefix("- ") {
                let bullet = rest.trim();
                if !bullet.is_empty() {
                    sec.bullets.push(bullet.to_string());
                }
            }
        }
    }
    if let Some(sec) = current {
        sections.push(sec);
    }
    sections
}

/// Delete one bullet from the managed block and return the updated file
/// contents. No-op (returns the original) when section or bullet is missing.
///
/// If a section's bullets are all removed, the whole `### <section>` block is
/// dropped. If the entire managed block becomes empty, the whole block
/// including its markers is removed.
pub fn delete_applied_bullet(file_content: &str, section_title: &str, bullet_text: &str) -> String {
    let Some(start) = file_content.find("<!-- headroom:learn:start -->") else {
        return file_content.to_string();
    };
    let end_marker = "<!-- headroom:learn:end -->";
    let Some(end_rel) = file_content[start..].find(end_marker) else {
        return file_content.to_string();
    };
    let end = start + end_rel + end_marker.len();

    let before = &file_content[..start];
    let block = &file_content[start..end];
    let after = &file_content[end..];

    let mut out_lines: Vec<String> = Vec::new();
    let mut current_section_start: Option<usize> = None;
    let mut current_section_has_bullets = false;
    let mut in_target_section = false;
    let mut bullet_removed = false;

    fn flush(
        out_lines: &mut Vec<String>,
        section_start: &mut Option<usize>,
        has_bullets: &mut bool,
    ) {
        if let Some(idx) = section_start.take() {
            if !*has_bullets {
                out_lines.truncate(idx);
            }
        }
        *has_bullets = false;
    }

    for line in block.lines() {
        // Skip the end-of-block marker so the section-flush truncation
        // can never drop it. We re-append it during reassembly below.
        if line.trim_end() == end_marker {
            continue;
        }

        let trimmed = line.trim_start();
        if let Some(title) = trimmed.strip_prefix("### ") {
            flush(
                &mut out_lines,
                &mut current_section_start,
                &mut current_section_has_bullets,
            );
            current_section_start = Some(out_lines.len());
            in_target_section = title.trim() == section_title;
            out_lines.push(line.to_string());
            continue;
        }

        if current_section_start.is_some() {
            if let Some(rest) = trimmed.strip_prefix("- ") {
                let bullet = rest.trim();
                if in_target_section && !bullet_removed && bullet == bullet_text {
                    bullet_removed = true;
                    continue;
                }
                if !bullet.is_empty() {
                    current_section_has_bullets = true;
                }
            }
        }

        out_lines.push(line.to_string());
    }
    flush(
        &mut out_lines,
        &mut current_section_start,
        &mut current_section_has_bullets,
    );

    if !bullet_removed {
        return file_content.to_string();
    }

    let any_sections = out_lines.iter().any(|l| l.trim_start().starts_with("### "));
    if !any_sections {
        let mut rewritten = String::with_capacity(before.len() + after.len());
        rewritten.push_str(before.trim_end_matches('\n'));
        let after_trimmed = after.trim_start_matches('\n');
        if !rewritten.is_empty() && !after_trimmed.is_empty() {
            rewritten.push_str("\n\n");
        }
        rewritten.push_str(after_trimmed);
        return rewritten;
    }

    // Drop trailing blank lines so removing the last bullet of the last
    // section doesn't leave a `\n\n<!-- end -->` gap behind.
    while out_lines
        .last()
        .map(|s| s.trim().is_empty())
        .unwrap_or(false)
    {
        out_lines.pop();
    }

    let mut rewritten = String::with_capacity(file_content.len());
    rewritten.push_str(before);
    rewritten.push_str(&out_lines.join("\n"));
    rewritten.push('\n');
    rewritten.push_str(end_marker);
    rewritten.push_str(after);
    rewritten
}

pub fn claude_project_memory_file(project_path: &str) -> PathBuf {
    let home = dirs::home_dir()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    home.join(".claude")
        .join("projects")
        .join(encode_claude_project_folder_name(project_path))
        .join("memory")
        .join("MEMORY.md")
}

fn encode_claude_project_folder_name(project_path: &str) -> String {
    format!(
        "-{}",
        project_path.trim_start_matches('/').replace('/', "-")
    )
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Recursively collect every `.so` / `.dylib` under `dir`. Used by
/// `ad_hoc_sign_venv_natives` to enumerate the native extensions pip
/// dropped into the venv. Symlinks are followed via `read_dir`'s default
/// behavior on macOS, but `file_type` is checked so we don't recurse into
/// non-directories. Errors propagate so the caller can log + skip.
fn collect_native_extensions(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_native_extensions(&path, out)?;
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if ext == "so" || ext == "dylib" {
                out.push(path);
            }
        }
    }
    Ok(())
}

/// Hash a requirements lock file ignoring comments and blank lines, so that
/// header/comment churn does not force a full `pip install` on upgrade.
fn requirements_lock_sha(lock: &str) -> String {
    let mut hasher = Sha256::new();
    for line in lock.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        hasher.update(trimmed.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

fn bootstrap_requirements_lock() -> &'static str {
    bootstrap_requirements_lock_for_target(std::env::consts::OS)
}

fn bootstrap_requirements_lock_for_target(os: &str) -> &'static str {
    match os {
        // Linux bootstrap only needs the proxy runtime. Installing the full
        // headroom-ai[all] stack pulls optional native packages like hnswlib
        // that fail on many fresh Linux systems.
        "linux" => HEADROOM_LINUX_REQUIREMENTS_LOCK,
        // Windows uses the full stack minus hnswlib (sdist-only on PyPI, no
        // vendored win_amd64 wheel; a source build needs MSVC and bricked
        // bootstrap — RUST-65/66). sqlite-vec covers the vector backend. The
        // pin set is its own file so a Windows-only resolution change never
        // perturbs the macOS lock.
        "windows" => HEADROOM_WINDOWS_REQUIREMENTS_LOCK,
        _ => HEADROOM_REQUIREMENTS_LOCK,
    }
}

fn run_python_command(python: &Path, args: &[&str], cwd: &Path) -> Result<()> {
    run_command(python, args, cwd)
}

/// Path to the output-shaper savings ledger, which `output_savings` also reads
/// to recompute the estimate the dashboard shows.
fn output_savings_ledger_path() -> Option<PathBuf> {
    crate::output_savings::ledger_path()
}

/// Core of [`purge_legacy_output_savings_control_arm_once`]: given the ledger bytes, return
/// rewritten bytes when a non-empty `control` arm was cleared, else `None`
/// (missing/empty control). Parse and serialization failures are returned so
/// the caller can leave the ledger and its once-only stamp untouched.
fn ledger_bytes_without_control(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut ledger =
        serde_json::from_slice::<Value>(bytes).context("parsing output_savings ledger")?;
    // A syntactically valid JSON scalar/array is still an invalid ledger
    // schema. Treat it as an unresolved persistence boundary so the caller
    // leaves the source and retry stamp untouched rather than recording a
    // successful purge against data it could not safely interpret.
    if !ledger.is_object() {
        bail!("output_savings ledger must be a JSON object");
    }
    let has_control = match ledger.get("control") {
        None => false,
        Some(control) => {
            let control = control.as_object().ok_or_else(|| {
                anyhow!("output_savings ledger control arm must be a JSON object")
            })?;
            !control.is_empty()
        }
    };
    if !has_control {
        return Ok(None);
    }
    ledger
        .as_object_mut()
        .ok_or_else(|| anyhow!("output_savings ledger must be a JSON object"))?
        .insert("control".to_string(), json!({}));
    Ok(Some(
        serde_json::to_vec(&ledger).context("serializing output_savings ledger")?,
    ))
}

/// Drop the output-shaper A/B control arm left over from the abandoned 1%
/// holdout, exactly once.
///
/// Those samples predate the current shaper and were collected under a policy
/// that never gathered enough of them to mean anything, so folding them into
/// the 3% arm would poison it from the first request. Clearing them on every
/// spawn is not an option either: the arm is live data now, and this runs each
/// time the proxy starts.
///
/// The stamp sits beside the ledger on purpose. A reset that removes
/// `~/.headroom` takes the control samples with it, so the stamp going too is
/// correct — there is nothing left to purge either way.
///
/// Best-effort throughout: never touch a missing or unparseable ledger, and
/// only rewrite when there is control data to drop. Uses `atomic_write` so a
/// crash mid-write cannot truncate the ledger.
fn purge_legacy_output_savings_control_arm_once() {
    let Some(path) = output_savings_ledger_path() else {
        return;
    };
    let stamp = path.with_file_name(".legacy-control-arm-purged");
    if stamp.exists() {
        return;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let rewritten = match ledger_bytes_without_control(&bytes) {
        Ok(rewritten) => rewritten,
        Err(err) => {
            log::warn!(
                "[tool_manager] refusing to mark output_savings control-arm purge complete: {err:#}"
            );
            return;
        }
    };
    if let Some(out) = rewritten {
        if let Err(err) = crate::client_adapters::atomic_write(&path, &out) {
            log::warn!("[tool_manager] purging output_savings control arm failed: {err}");
            // Leave the stamp unwritten so the next spawn retries; a failed
            // purge must not be recorded as a done one.
            return;
        }
        log::info!("[tool_manager] cleared legacy output-shaper control arm");
    }
    if let Err(err) = crate::client_adapters::atomic_write(&stamp, b"") {
        log::warn!("[tool_manager] stamping legacy control-arm purge failed: {err}");
    }
}

/// True once the verbosity baseline has been seeded (non-empty sample count).
/// The persisted baseline does not carry a `total_samples` field — that is a
/// computed property of the in-memory model. On disk the total observation
/// count lives in `baseline.glob.n` (the global accumulator), so that is what
/// we gate on. The proxy reports the savings estimate as unavailable until a
/// baseline with observations exists.
fn verbosity_baseline_present() -> bool {
    let Some(path) = output_savings_ledger_path() else {
        return false;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|json| {
            json.get("baseline")
                .and_then(|b| b.get("glob"))
                .and_then(|g| g.get("n"))
                .and_then(|n| n.as_u64())
        })
        .is_some_and(|n| n > 0)
}

/// Real project root (the transcript `cwd`) of the Claude Code project with the
/// most transcript bytes under `~/.claude/projects`. Reading `cwd` from a
/// transcript avoids lossily decoding the mangled `~/.claude/projects` dir name
/// — it is exactly the path headroom's plugin resolves to, so `--project <cwd>`
/// matches. Returns `None` when no non-empty transcript exists.
fn busiest_claude_project_cwd() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let projects_dir = PathBuf::from(home).join(".claude").join("projects");

    let mut best: Option<(u64, PathBuf)> = None;
    for entry in std::fs::read_dir(&projects_dir).ok()?.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let mut bytes = 0u64;
        if let Ok(files) = std::fs::read_dir(&dir) {
            for f in files.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    if let Ok(meta) = f.metadata() {
                        bytes += meta.len();
                    }
                }
            }
        }
        if bytes > 0 && best.as_ref().is_none_or(|(b, _)| bytes > *b) {
            best = Some((bytes, dir));
        }
    }

    project_cwd_from_transcript_dir(&best?.1)
}

/// Pull the `cwd` field from the first transcript line that has one. Reads at
/// most a few lines so a multi-hundred-MB transcript dir stays cheap.
fn project_cwd_from_transcript_dir(dir: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    for f in std::fs::read_dir(dir).ok()?.flatten() {
        let p = f.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(file) = std::fs::File::open(&p) else {
            continue;
        };
        for line in BufReader::new(file).lines().take(50).map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
                    if !cwd.is_empty() {
                        return Some(cwd.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Prepend a binary's own directory to PATH so an `#!/usr/bin/env node`
/// shebang (or similar) resolves the interpreter that nvm installs alongside
/// it. Falls back to the existing PATH when the binary has no parent.
fn context7_package_spec() -> String {
    context7_package_spec_for(CONTEXT7_PINNED_VERSION)
}

fn context7_package_spec_for(version: &str) -> String {
    format!("@upstash/context7-mcp@{version}")
}

fn build_command(binary: &Path, args: &[&str], cwd: &Path) -> Command {
    let mut command = crate::proc::command(binary);
    command
        .args(args)
        .current_dir(cwd)
        .env_remove("PYTHONHOME")
        // GUI apps inherit a minimal PATH lacking the nvm/homebrew bin dir, so a
        // CLI with a `#!/usr/bin/env node` shebang (e.g. codex) fails with exit
        // 127 / "env: node: No such file or directory". node lives alongside the
        // CLI in nvm's bin, so prepend the binary's own dir to PATH.
        .env(
            "PATH",
            crate::claude_cli::path_with_binary_dir(binary, std::env::var_os("PATH").as_deref()),
        )
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONSTARTUP")
        .env("PYTHONNOUSERSITE", "1")
        // `backslashreplace`, matching `proc::command` -- plain "utf-8" here
        // silently overrode it and re-armed the RUST-7C UnicodeEncodeError kill
        // for every child this builds (a lone surrogate must not be fatal).
        .env("PYTHONIOENCODING", "utf-8:backslashreplace")
        .env("LC_ALL", "C.UTF-8")
        .env("LANG", "C.UTF-8")
        .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
        .env("PIP_NO_INPUT", "1");
    crate::edition::apply_runtime_env(&mut command);
    command
}

/// Runs a pip install invocation with retries on transient failures.
///
/// pip's own `--retries` flag only covers connection establishment, not
/// mid-stream read timeouts, so a single TCP stall during a wheel download
/// can fail the whole bootstrap (see Sentry bootstrap_failed reports). We
/// retry the full invocation; pip's cachecontrol layer persists partial
/// responses so retries resume cheaply instead of redownloading from zero.
fn run_pip_install_with_retries(python: &Path, args: &[&str], cwd: &Path) -> Result<()> {
    run_pip_install_with_retries_streaming(python, args, cwd, |_| {})
}

/// Translate a pip stdout/stderr line into a progress update, or None for
/// noise. Counter-based monotonic advance inside `[base_percent, max_percent-1]`:
/// we don't know the final dep count up-front, so each interesting line nudges
/// the bar forward and it saturates just below the parent step's ceiling.
fn pip_line_to_progress(
    line: &str,
    elapsed: Duration,
    counter: &mut u32,
    base_percent: u8,
    max_percent: u8,
) -> Option<BootstrapStepUpdate> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let message = if let Some(rest) = trimmed.strip_prefix("Collecting ") {
        let spec = rest.split_whitespace().next().unwrap_or(rest);
        let pkg = spec
            .split(|c: char| matches!(c, '=' | '<' | '>' | '!' | '~' | ';' | '['))
            .next()
            .unwrap_or(spec);
        format!("Fetching {}...", pkg)
    } else if let Some(rest) = trimmed.strip_prefix("Downloading ") {
        let file = rest.split_whitespace().next().unwrap_or(rest);
        let name = file.rsplit('/').next().unwrap_or(file);
        let pkg = name.split('-').next().unwrap_or(name);
        format!("Downloading {}...", pkg)
    } else if trimmed.starts_with("Installing collected packages") {
        "Installing packages...".to_string()
    } else if let Some(rest) = trimmed.strip_prefix("Successfully installed ") {
        let count = rest.split_whitespace().count();
        format!("Installed {} packages.", count)
    } else {
        return None;
    };

    *counter = counter.saturating_add(1);
    let span = max_percent.saturating_sub(base_percent).max(1) as u32;
    let advance = (*counter).min(span.saturating_sub(1));
    let percent = (base_percent as u32 + advance).min(max_percent as u32 - 1) as u8;

    let remaining = 90_u64.saturating_sub(elapsed.as_secs()).max(5);
    Some(BootstrapStepUpdate {
        step: "Updating dependencies",
        message,
        eta_seconds: remaining,
        percent,
    })
}

// Compact representation of a pip-install failure for log/Sentry. The full
// CommandFailure Display dumps program + args + stdout + stderr, which the
// 400-char Sentry cap eats before any stderr lines appear. Pip's actual
// reason lives on stderr, so prefer the tail of stderr (or stdout if stderr
// is empty) plus exit code.
pub(crate) fn compact_pip_failure(err: &anyhow::Error) -> String {
    const TAIL_BUDGET: usize = 300;
    let Some(failure) = err.chain().find_map(|c| c.downcast_ref::<CommandFailure>()) else {
        // No CommandFailure means the command never ran (spawn failed).
        // `to_string()` prints only the top context ("starting <python> -m
        // pip ...") and drops the io cause — RUST-6S shipped 15 events of
        // exactly that, unreadable. `{:#}` keeps the whole chain.
        return format!("{err:#}");
    };
    let source = if !failure.stderr.trim().is_empty() {
        failure.stderr.as_str()
    } else {
        failure.stdout.as_str()
    };
    let trimmed = source.trim_end();
    // "The reason lives at the end" holds for pip run directly, but not when it
    // is reached through ensurepip: pip prints its `ERROR:` diagnosis, then a
    // Python traceback follows, so the tail is `CalledProcessError ... returned
    // non-zero exit status 1` -- the one line in the whole stream with no cause
    // in it. That is how RUST-82 reached triage: an install-blocking venv
    // failure classified `other`, with nothing in it to act on. Where pip named
    // a reason, start from it; otherwise the tail is still the best guess.
    let from_pip_error = if trimmed.starts_with("ERROR: ") {
        Some(trimmed)
    } else {
        trimmed.find("\nERROR: ").map(|i| &trimmed[i + 1..])
    };
    let tail = match from_pip_error {
        // Byte offsets, so walk to a char boundary before slicing: pip on a
        // non-English Windows locale emits multi-byte stderr and slicing
        // mid-character would panic.
        Some(head) if head.len() > TAIL_BUDGET => {
            let mut end = TAIL_BUDGET;
            while !head.is_char_boundary(end) {
                end -= 1;
            }
            &head[..end]
        }
        Some(head) => head,
        None if trimmed.len() > TAIL_BUDGET => {
            let mut start = trimmed.len() - TAIL_BUDGET;
            while !trimmed.is_char_boundary(start) {
                start += 1;
            }
            let aligned = trimmed[start..]
                .find('\n')
                .map(|i| start + i + 1)
                .unwrap_or(start);
            &trimmed[aligned..]
        }
        None => trimmed,
    };
    let exit = failure
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".into());
    format!("exit={exit}; stderr tail: {tail}")
}

/// Coarse cause class for a pip failure, used as the Sentry fingerprint.
///
/// The compact message embeds pip's stderr tail, and Sentry groups bridged log
/// lines by message text — so every distinct tail opened its own issue for what
/// is one failure (RUST-6M/6N/6P were all the same machine's half-built venv).
/// One flat bucket would be the opposite mistake: resolving a shipped fix would
/// regress the instant an unrelated cause reappeared (RUST-5Q). These classes
/// match the buckets triage already sorts these into by hand.
pub(crate) fn pip_failure_category(compact: &str) -> &'static str {
    let lower = compact.to_ascii_lowercase();
    if lower.contains("no module named pip") {
        "no-pip"
    } else if lower.contains("no matching distribution found")
        || lower.contains("could not find a version that satisfies")
    {
        // Our lock asked for a version PyPI has no wheel for on that
        // interpreter/platform (RUST-6S: onnxruntime==1.27.0 on Intel macOS,
        // where releases stop at 1.23.2). That is a bad pin in *our* lock, not
        // the user's machine -- it must never sit in the "other" grab-bag.
        "no-matching-dist"
    } else if lower.contains("no usable temporary directory") {
        "no-tempdir"
    } else if crate::is_disk_full_signal(&lower) {
        "disk-full"
    } else if lower.contains("permission denied")
        || lower.contains("check the permissions")
        || lower.contains("access is denied")
        || lower.contains("errno 13")
        // Windows LOCALIZES its error text: RUST-8K arrived as
        // "액세스가 거부되었습니다. (os error 5)" and fell through to the
        // grab-bag this function exists to prevent. The numeric code is the
        // one locale-independent handle. Not gated to Windows: errno 5 is EIO
        // on Unix, but a mislabelled bucket on an all-but-unseen pip EIO is a
        // cheaper bug than a Windows-only branch no test here can reach. Keep
        // the closing paren -- os error 50/51 are macOS network errors.
        || lower.contains("(os error 5)")
    {
        "permission"
    } else if lower.contains("no such file or directory") || lower.contains("errno 2") {
        "missing-file"
    } else if lower.contains("failed building wheel")
        || lower.contains("microsoft visual c++")
        || lower.contains("error: subprocess-exited-with-error")
    {
        "build"
    } else if lower.contains("could not fetch url")
        || lower.contains("connection")
        || lower.contains("timed out")
        || lower.contains("temporary failure in name resolution")
    {
        "network"
    } else {
        "other"
    }
}

/// Cause class for a partial plugin install, so each shape gets its own Sentry
/// issue instead of one grab-bag. Same rationale as [`pip_failure_category`]:
/// RUST-6K collected five unrelated causes (a missing marketplace, a config the
/// host CLI refuses to parse, no CLI on PATH, and CLI version skew) under one
/// title, which made it untriageable -- and unresolvable, since any resolve
/// regresses the moment a sibling shape reappears.
fn plugin_install_failure_category(compact: &str) -> &'static str {
    let lower = compact.to_ascii_lowercase();
    if lower.contains("not found in marketplace") {
        // Our marketplace registration did not take. Cause now travels with it
        // (see install_plugin_into), so this bucket carries the real reason.
        "marketplace-missing"
    } else if lower.contains("cli not found on path") {
        "cli-missing"
    } else if lower.contains("unknown option")
        || lower.contains("unrecognized subcommand")
        || lower.contains("is no longer supported")
    {
        "cli-version-skew"
    } else if lower.contains("failed to load configuration")
        || lower.contains("duplicate key")
        || lower.contains("toml parse error")
    {
        // The host CLI cannot read its own config, so nothing we do lands.
        "host-config-invalid"
    } else if crate::is_disk_full_signal(&lower) {
        "disk-full"
    } else if lower.contains("permission denied")
        || lower.contains("access is denied")
        || lower.contains("errno 13")
    {
        "permission"
    } else {
        "other"
    }
}

/// Streaming variant of `run_pip_install_with_retries`. Each line emitted by
/// pip on stdout/stderr is piped through `on_line` as it arrives, so callers
/// can translate noteworthy pip events ("Collecting X", "Downloading Y",
/// "Installing collected packages", "Successfully installed") into
/// user-facing progress updates instead of staring at a static message for
/// the 60–90 seconds a large pip install takes.
fn run_pip_install_with_retries_streaming<F>(
    python: &Path,
    args: &[&str],
    cwd: &Path,
    mut on_line: F,
) -> Result<()>
where
    F: FnMut(&str),
{
    const MAX_ATTEMPTS: u32 = 3;
    const BACKOFFS_SECS: &[u64] = &[2, 5];
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match run_command_streaming(python, args, cwd, &mut on_line) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt < MAX_ATTEMPTS {
                    log::info!(
                        "pip install attempt {}/{} failed (will retry): {}",
                        attempt,
                        MAX_ATTEMPTS,
                        err
                    );
                } else {
                    let compact = compact_pip_failure(&err);
                    if crate::is_disk_full_signal(&compact)
                        || crate::is_disk_full_signal(&format!("{err:#}"))
                    {
                        // ENOSPC is environmental and already surfaced + Sentry-
                        // suppressed by the caller's runtime_upgrade_failed /
                        // bootstrap_failed guard. Drop this per-attempt warn to
                        // info so the log->Sentry bridge doesn't recapture it
                        // (RUST-4C).
                        log::info!(
                            "pip install attempt {}/{} failed (final): disk full (ENOSPC)",
                            attempt,
                            MAX_ATTEMPTS
                        );
                    } else {
                        // Explicit per-category fingerprint; the bridged warn is
                        // local-only (skip_sentry rule) so this doesn't double-
                        // report. See `pip_failure_category`.
                        let category = pip_failure_category(&compact);
                        sentry::with_scope(
                            |scope| {
                                scope.set_fingerprint(Some(&["pip-install-failed", category]));
                            },
                            || {
                                sentry::capture_message(
                                    &format!(
                                        "pip install failed after {MAX_ATTEMPTS} attempts \
                                         [{category}]: {compact}"
                                    ),
                                    sentry::Level::Warning,
                                );
                            },
                        );
                        log::warn!(
                            "pip install attempt {}/{} failed (final): {}",
                            attempt,
                            MAX_ATTEMPTS,
                            compact
                        );
                    }
                }
                last_err = Some(err);
                if attempt < MAX_ATTEMPTS {
                    let idx = (attempt as usize - 1).min(BACKOFFS_SECS.len() - 1);
                    std::thread::sleep(std::time::Duration::from_secs(BACKOFFS_SECS[idx]));
                }
            }
        }
    }
    Err(last_err.expect("at least one attempt was made"))
}

/// Like `run_command` but streams stdout + stderr line-by-line through
/// `on_line` in real time. Captures everything for the structured failure
/// payload so error reporting is unchanged.
fn run_command_streaming<F>(binary: &Path, args: &[&str], cwd: &Path, on_line: &mut F) -> Result<()>
where
    F: FnMut(&str),
{
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;

    let mut cmd = build_command(binary, args, cwd);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {} {}", binary.display(), args.join(" ")))?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let (tx, rx) = mpsc::channel::<StreamedLine>();
    let tx_stdout = tx.clone();
    let tx_stderr = tx.clone();
    drop(tx);

    let stdout_handle = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx_stdout.send(StreamedLine {
                line,
                is_stderr: false,
            });
        }
    });
    let stderr_handle = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx_stderr.send(StreamedLine {
                line,
                is_stderr: true,
            });
        }
    });

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();

    while let Ok(streamed) = rx.recv() {
        on_line(&streamed.line);
        let sink = if streamed.is_stderr {
            &mut stderr_buf
        } else {
            &mut stdout_buf
        };
        sink.push_str(&streamed.line);
        sink.push('\n');
    }

    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    let status = child
        .wait()
        .with_context(|| format!("waiting for {} {}", binary.display(), args.join(" ")))?;

    if !status.success() {
        return Err(anyhow::Error::new(CommandFailure {
            program: binary.display().to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            stdout: stdout_buf,
            stderr: stderr_buf,
            exit_code: status.code(),
            signal: exit_status_signal(&status),
        }));
    }

    Ok(())
}

struct StreamedLine {
    line: String,
    is_stderr: bool,
}

fn run_command_with_timeout(
    binary: &Path,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Result<()> {
    use std::io::Read;
    use std::sync::mpsc;

    let mut cmd = build_command(binary, args, cwd);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {} {}", binary.display(), args.join(" ")))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>();
    let (stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>();
    let stdout_handle = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        let _ = stdout_tx.send(buf);
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stderr);
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        let _ = stderr_tx.send(buf);
    });

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().with_context(|| {
                        format!("waiting for {} {}", binary.display(), args.join(" "))
                    })?;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err).with_context(|| {
                    format!("waiting for {} {}", binary.display(), args.join(" "))
                });
            }
        }
    };

    let _ = stdout_handle.join();
    let _ = stderr_handle.join();
    let stdout = String::from_utf8_lossy(&stdout_rx.recv().unwrap_or_default()).into_owned();
    let mut stderr = String::from_utf8_lossy(&stderr_rx.recv().unwrap_or_default()).into_owned();

    if timed_out {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "command timed out after {}ms",
            timeout.as_millis()
        ));
        return Err(anyhow::Error::new(CommandFailure {
            program: binary.display().to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            stdout,
            stderr,
            exit_code: None,
            signal: exit_status_signal(&status),
        }));
    }

    if !status.success() {
        return Err(anyhow::Error::new(CommandFailure {
            program: binary.display().to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            stdout,
            stderr,
            exit_code: status.code(),
            signal: exit_status_signal(&status),
        }));
    }

    Ok(())
}

fn run_command(binary: &Path, args: &[&str], cwd: &Path) -> Result<()> {
    let output = build_command(binary, args, cwd)
        .output()
        .with_context(|| format!("starting {} {}", binary.display(), args.join(" ")))?;

    if !output.status.success() {
        return Err(anyhow::Error::new(CommandFailure {
            program: binary.display().to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
            signal: exit_status_signal(&output.status),
        }));
    }

    Ok(())
}

/// Structured failure from a shell-out. Carried through `anyhow::Error` so callers
/// can `.context()` as usual, and capture sites (e.g. Sentry) can downcast to pull
/// stdout/stderr into structured fields instead of a truncated message string.
#[derive(Debug)]
pub struct CommandFailure {
    pub program: String,
    pub args: Vec<String>,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    /// Unix signal number when the child was killed by a signal (`exit_code` is
    /// `None` in that case). Lets us tell SIGKILL (9 — likely parent shutdown,
    /// OOM, or launchd) from SIGTERM (15 — graceful kill) in failure reports.
    pub signal: Option<i32>,
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = match (self.exit_code, self.signal) {
            (Some(code), _) => format!("exit {}", code),
            (None, Some(sig)) => format!("killed by signal {}", sig),
            (None, None) => "killed by signal".to_string(),
        };
        write!(
            f,
            "command failed ({}): {} {}\nstdout:\n{}\nstderr:\n{}",
            status,
            self.program,
            self.args.join(" "),
            self.stdout,
            self.stderr
        )
    }
}

impl std::error::Error for CommandFailure {}

/// True when a failure is an older Codex CLI that predates `codex plugin add`
/// (`error: unrecognized subcommand 'add'`). Not retryable -- the user must
/// update Codex -- so install treats it as a soft skip + nudge, not an error.
fn is_outdated_codex(err: &anyhow::Error) -> bool {
    err.downcast_ref::<CommandFailure>()
        .is_some_and(|failure| failure.stderr.contains("unrecognized subcommand"))
}

/// Extract the Unix signal number that killed a child, or `None` on non-Unix
/// or when the process exited normally. Used to populate `CommandFailure.signal`
/// so failure reports distinguish SIGKILL from SIGTERM.
fn exit_status_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// Returns true when an `anyhow::Error` from a `headroom <subcommand>` shell-out
/// looks like the venv is missing pinned dependencies — i.e. Python died with
/// `ModuleNotFoundError` or `ImportError` before the CLI could run. This is the
/// recovery signal for a partial install that left the receipt's
/// `requirementsLockSha256` stamped but the venv contents incomplete.
fn looks_like_corrupt_venv_error(err: &anyhow::Error) -> bool {
    let Some(failure) = err.downcast_ref::<CommandFailure>() else {
        return false;
    };
    let stderr = failure.stderr.as_str();
    stderr.contains("ModuleNotFoundError") || stderr.contains("ImportError")
}

/// Structured error emitted when the headroom proxy subprocess fails to open
/// its port. Capture sites downcast to pull the log tail into Sentry `extra`
/// fields, which are not subject to the 8KB message cap.
#[derive(Debug)]
pub struct HeadroomStartupFailure {
    pub program: String,
    pub args: Vec<String>,
    pub log_path: String,
    pub log_tail: String,
    pub reason: String,
}

impl std::fmt::Display for HeadroomStartupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {} (log: {}){}",
            self.program,
            self.args.join(" "),
            self.reason,
            self.log_path,
            if self.log_tail.is_empty() {
                String::new()
            } else {
                format!("\n--- log tail ---\n{}\n--- end log ---", self.log_tail)
            }
        )
    }
}

impl std::error::Error for HeadroomStartupFailure {}

#[cfg(test)]
mod tests {
    use super::{
        codebase_memory_release_target, context7_package_spec_for, parse_codebase_memory_release,
        parse_rtk_release, rtk_release_asset_name, rtk_version_is_downgrade,
        rtk_version_output_matches, stable_package_version, valid_codebase_memory_version,
        valid_rtk_version,
    };
    use serde_json::json;
    use std::cell::Cell;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use chrono::Local;

    use super::log_tail;
    #[cfg(windows)]
    use super::python_distribution_artifact;
    use super::rotate_log_if_large;
    use super::{
        addon_unavailable_reason, apply_serena_dashboard_interface, apply_serena_gitignore,
        bootstrap_requirements_lock_for_target, build_command, cc_switch_reconcile_for_runtime,
        classify_kompress_prefetch_failure, codebase_memory_distribution_artifact,
        compact_pip_failure, describe_proxy_port_occupant, diagnose_proxy_port, exe_path_is_under,
        explain_host_marketplace_missing_plugin, extract_required_pydantic_core_version,
        format_all_foreign_bail,
        format_already_running_bail, headroom_entrypoint_startup_args,
        headroom_python_startup_args, httpx_ca_bundle_bridge_from, is_checksum_mismatch,
        is_outdated_codex, learned_openai_ttl_seconds, ledger_bytes_without_control,
        looks_like_corrupt_venv_error, parse_lsof_listener, parse_major_minor_patch,
        parse_netstat_listener, parse_pid_from_lsof_detail, parse_ss_listener,
        parse_tasklist_image, pending_addon_update, pinned_headroom_release, pip_failure_category,
        plugin_install_failure_category, pre_upstream_concurrency, probe_backend_readyz_ok,
        proxy_argv_contains_expected_flags, purge_legacy_output_savings_control_arm_once,
        read_headroom_learn_metadata_from_path, receipt_requires_atomic_rebuild,
        reclaim_orphan_proxy, redact_sensitive, render_codex_plugin_enabled, requirements_lock_sha,
        rtk_distribution_artifact, run_command, sanitize_log_variant, savings_profile_for_runtime,
        set_codex_plugin_enabled, settle_unowned_port, sha256_bytes,
        summarize_kompress_prefetch_failure, verify_sha256_file, wait_for_port_free,
        write_codex_compat_marketplace_manifest_at, CommandFailure, HeadroomRelease,
        ManagedRuntime, PipOutputCapture, PluginHost, PortState, ToolManager, UpgradeOutcome,
        ATOMIC_REBUILD_FLOOR_VERSION, HEADROOM_LINUX_REQUIREMENTS_LOCK, HEADROOM_PINNED_VERSION,
        HEADROOM_REQUIREMENTS_LOCK, HEADROOM_WINDOWS_REQUIREMENTS_LOCK, MARKITDOWN_PINNED_VERSION,
        PLUGIN_ADDONS, PLUGIN_DISPLAY_VERSION, RTK_VERSION, UNKNOWN_OCCUPANT,
    };
    use crate::backend_port;
    use crate::models::ManagedTool;
    use crate::port_conflict;
    use std::net::TcpListener;

    #[test]
    fn previous_learn_run_treats_exit_zero_llm_timeout_as_failure() {
        let log = r#"[2026-08-31T12:34:11Z] headroom learn --agent codex (target=codex)
status: exit status: 0

--- stdout ---
No actionable patterns found.

--- stderr ---
LLM analysis failed: `codex exec` did not respond within 300s.
"#;

        let run = super::parse_headroom_learn_previous_run(log, chrono::Utc::now())
            .expect("previous run");

        assert_eq!(run.run_key, "codex");
        assert!(!run.success);
        assert_eq!(run.finished_at.to_rfc3339(), "2026-08-31T12:34:11+00:00");
        assert_eq!(
            run.error.as_deref(),
            Some("LLM analysis failed: `codex exec` did not respond within 300s.")
        );
    }

    #[test]
    fn previous_learn_run_accepts_clean_cross_platform_exit_zero() {
        for status in ["exit status: 0", "exit code: 0"] {
            let log = format!(
                r#"[2026-09-01T08:00:00Z] headroom learn --agent codex (target=codex)
status: {status}

--- stdout ---
No actionable patterns found.

--- stderr ---
"#
            );

            let run = super::parse_headroom_learn_previous_run(&log, chrono::Utc::now())
                .expect("previous run");

            assert!(run.success, "{status}");
            assert_eq!(run.error, None, "{status}");
        }
    }

    fn write_ttl_obs(dir: &Path, rows: &[(&str, f64, bool, &str)]) -> PathBuf {
        let path = dir.join("cache_ttl_observations.jsonl");
        let mut lines: Vec<String> = rows
            .iter()
            .map(|(provider, idle, is_miss, reason)| {
                format!(
                    r#"{{"ts":1000,"provider":"{provider}","model":"gpt-5.5","reason":"{reason}","idle_seconds":{idle},"ttl_assumed":300,"is_miss":{is_miss},"cache_read":0,"expected_cached":1000}}"#
                )
            })
            .collect();
        lines.push("not-json".into());
        fs::write(&path, lines.join("\n")).expect("write obs");
        path
    }

    fn learned_openai_ttl_picks_smallest_death_beyond_largest_life() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_ttl_obs(
            dir.path(),
            &[
                ("openai", 120.0, false, "hit"),
                ("openai", 300.0, false, "hit"),
                ("openai", 480.0, false, "hit"),
                // Death below the max hit says nothing; 600 is the safe bound.
                ("openai", 400.0, true, "ttl_expiry"),
                ("openai", 600.0, true, "ttl_expiry"),
                ("openai", 900.0, true, "ttl_expiry"),
                // Non-openai rows and non-ttl_expiry misses are ignored.
                ("anthropic", 2000.0, false, "hit"),
                ("openai", 550.0, true, "prefix_change"),
            ],
        );
        assert_eq!(learned_openai_ttl_seconds(&path), Some(600));
    }

    #[test]
    fn learned_openai_ttl_none_without_safe_upper_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Every observed death overlaps the life range.
        let overlap = write_ttl_obs(
            dir.path(),
            &[
                ("openai", 120.0, false, "hit"),
                ("openai", 480.0, false, "hit"),
                ("openai", 900.0, false, "hit"),
                ("openai", 400.0, true, "ttl_expiry"),
                ("openai", 450.0, true, "ttl_expiry"),
                ("openai", 600.0, true, "ttl_expiry"),
            ],
        );
        assert_eq!(learned_openai_ttl_seconds(&overlap), None);
        // Thin samples (2 hits < 3) and a missing file also yield None.
        let thin = write_ttl_obs(
            dir.path(),
            &[
                ("openai", 120.0, false, "hit"),
                ("openai", 480.0, false, "hit"),
                ("openai", 600.0, true, "ttl_expiry"),
                ("openai", 700.0, true, "ttl_expiry"),
                ("openai", 800.0, true, "ttl_expiry"),
            ],
        );
        assert_eq!(learned_openai_ttl_seconds(&thin), None);
        assert_eq!(
            learned_openai_ttl_seconds(&dir.path().join("missing.jsonl")),
            None
        );
    }

    #[test]
    fn learned_openai_ttl_clamps_to_documented_bounds() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Observed death beyond OpenAI's documented 1h max: clamp to 3600.
        let path = write_ttl_obs(
            dir.path(),
            &[
                ("openai", 120.0, false, "hit"),
                ("openai", 300.0, false, "hit"),
                ("openai", 480.0, false, "hit"),
                ("openai", 5000.0, true, "ttl_expiry"),
                ("openai", 6000.0, true, "ttl_expiry"),
                ("openai", 7000.0, true, "ttl_expiry"),
            ],
        );
        assert_eq!(learned_openai_ttl_seconds(&path), Some(3600));
    }

    #[test]
    fn serena_gitignore_add_remove_roundtrip() {
        // Empty file -> block appended; missing trailing newline is repaired.
        let added = apply_serena_gitignore("", true).expect("append to empty");
        assert!(added.ends_with(".serena/\n"));
        let from_existing = apply_serena_gitignore("*.log", true).expect("append after content");
        assert!(from_existing.starts_with("*.log\n"));
        assert!(from_existing.ends_with(".serena/\n"));

        // Idempotent: already present -> no rewrite.
        assert!(apply_serena_gitignore(&added, true).is_none());
        assert!(apply_serena_gitignore(".serena/\n", true).is_none());

        // Removal restores the original bytes exactly.
        assert_eq!(
            apply_serena_gitignore(&from_existing, false).expect("remove block"),
            "*.log\n"
        );

        // A hand-written pattern with no marker is the user's - left alone.
        assert!(apply_serena_gitignore(".serena/\n*.log\n", false).is_none());
    }

    #[test]
    fn apply_serena_dashboard_interface_replaces_only_unset_default() {
        // Unset key (serena's generated default) -> forced to browser.
        assert_eq!(
            apply_serena_dashboard_interface("projects: []\nweb_dashboard_interface:\n")
                .expect("replace unset"),
            "projects: []\nweb_dashboard_interface: browser\n"
        );
        assert_eq!(
            apply_serena_dashboard_interface("web_dashboard_interface: null # default\n")
                .expect("replace null"),
            "web_dashboard_interface: browser\n"
        );

        // Missing file -> minimal config with the required projects key.
        assert_eq!(
            apply_serena_dashboard_interface("").expect("create minimal"),
            "projects: []\nweb_dashboard_interface: browser\n"
        );

        // Existing config without the key -> appended, projects not duplicated.
        assert_eq!(
            apply_serena_dashboard_interface("projects: []\nlog_level: 20\n")
                .expect("append to existing"),
            "projects: []\nlog_level: 20\nweb_dashboard_interface: browser\n"
        );

        // Explicit user choice (or already browser) -> left alone.
        assert!(apply_serena_dashboard_interface("web_dashboard_interface: app\n").is_none());
        assert!(apply_serena_dashboard_interface("web_dashboard_interface: browser\n").is_none());
        // Commented-out template line is not the key.
        assert_eq!(
            apply_serena_dashboard_interface("projects: []\n# web_dashboard_interface:\n")
                .expect("append despite comment"),
            "projects: []\n# web_dashboard_interface:\nweb_dashboard_interface: browser\n"
        );
    }

    #[test]
    fn ledger_purge_clears_nonempty_control_only() {
        // Non-empty control -> rewrite with control emptied; baseline/treatment kept.
        let with_control =
            br#"{"baseline":{"glob":{"n":5}},"treatment":{"k":{"n":3}},"control":{"k":{"n":2}}}"#;
        let out = ledger_bytes_without_control(with_control)
            .expect("parse/rewrite ledger")
            .expect("rewrite when control present");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["control"].as_object().unwrap().len(), 0);
        assert_eq!(v["treatment"]["k"]["n"], 3);
        assert_eq!(v["baseline"]["glob"]["n"], 5);

        // Empty control, absent control, and unparseable input -> no rewrite.
        assert!(ledger_bytes_without_control(br#"{"control":{}}"#)
            .expect("parse empty control")
            .is_none());
        assert!(
            ledger_bytes_without_control(br#"{"treatment":{"k":{"n":1}}}"#)
                .expect("parse missing control")
                .is_none()
        );
        assert!(ledger_bytes_without_control(b"{not json").is_err());
        // Valid JSON with the wrong root schema must not be treated as a
        // missing control arm: the purge caller must leave its retry stamp
        // untouched until the ledger can be repaired or replaced safely.
        assert!(ledger_bytes_without_control(br#"[]"#).is_err());
        assert!(ledger_bytes_without_control(br#"null"#).is_err());
        assert!(ledger_bytes_without_control(br#"{"control":[]}"#).is_err());
    }

    #[test]
    fn legacy_control_arm_is_purged_once_then_left_to_fill() {
        let root = std::env::temp_dir().join(format!("headroom-purge-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _guard = HomeGuard::new(&root);
        let ledger = root
            .join(".headroom-local-community")
            .join("output_savings.json");
        std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        let with_control =
            br#"{"baseline":{"glob":{"n":5}},"treatment":{"k":{"n":3}},"control":{"k":{"n":2}}}"#;

        // Samples from the abandoned 1% holdout go.
        std::fs::write(&ledger, with_control).unwrap();
        purge_legacy_output_savings_control_arm_once();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&ledger).unwrap()).unwrap();
        assert_eq!(v["control"].as_object().unwrap().len(), 0);

        // The 3% arm now fills on every proxy spawn. Purging again would empty
        // it as fast as it fills, so the stamp has to hold.
        std::fs::write(&ledger, with_control).unwrap();
        purge_legacy_output_savings_control_arm_once();
        assert_eq!(std::fs::read(&ledger).unwrap(), with_control);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_checksum_mismatch_detects_bail_through_context_layers() {
        // Pins the string coupling with download_to_path_with_progress's
        // bail! — reword one side and the unverified-fallback guard dies.
        let err = anyhow::anyhow!("checksum mismatch for https://x: expected aa, got bb")
            .context("downloading wheel");
        assert!(is_checksum_mismatch(&err));
        assert!(!is_checksum_mismatch(&anyhow::anyhow!(
            "connection reset by peer"
        )));
    }

    #[test]
    fn sitecustomize_registers_nonchaining_sigusr1_dump() {
        assert!(super::SITECUSTOMIZE_PY
            .contains("faulthandler.register(signal.SIGUSR1, all_threads=True)"));
        // chain=True would fall through to SIGUSR1's default terminate action.
        assert!(!super::SITECUSTOMIZE_PY.contains("chain=True)"));
    }

    #[test]
    fn sitecustomize_keeps_user_turn_text_out_of_lossy_compression() {
        // From headroom-ai 0.34.0 the coding persona compresses user-role
        // text blocks and no env can disable it. Verbatim user turns are
        // deliberate desktop posture (see the SITECUSTOMIZE_PY docstring);
        // the injection must flip the persona field back.
        assert!(super::SITECUSTOMIZE_PY.contains("compress_user_messages=False"));
        assert!(super::SITECUSTOMIZE_PY.contains(r#"_PROFILES.get("coding")"#));
        // Must stay guarded: fallback runtimes (< 0.30.0) lack the persona.
        assert!(super::SITECUSTOMIZE_PY.contains("except Exception:"));
    }

    #[test]
    fn sitecustomize_dropped_patches_upstreamed_in_0_35_0() {
        // The breakpoint-diagnostics / payload-preview / expansion-guard /
        // marker-mirroring patches shipped upstream in 0.35.0 (#2919);
        // keeping the desktop copies would double-log cache_breakpoints
        // and shadow the native implementations.
        assert!(!super::SITECUSTOMIZE_PY.contains("event=cache_breakpoints"));
        assert!(!super::SITECUSTOMIZE_PY.contains("HEADROOM_LOG_PAYLOAD_PREVIEW"));
        assert!(!super::SITECUSTOMIZE_PY.contains("normalize_message_cache_control"));
    }

    /// SemanticCache.set stores any body gated only on `status_code == 200`,
    /// so one empty/unparseable/error response is replayed for the full TTL
    /// (1h) and only a proxy restart clears it — the amplifier that turned a
    /// single bad upstream response into a wedged session on 2026-08-13.
    #[test]
    fn sitecustomize_guards_response_cache_against_poisoning() {
        let py = super::SITECUSTOMIZE_PY;
        // Must patch the class the proxy actually instantiates
        // (headroom.proxy.semantic_cache.SemanticCache, not cache.semantic).
        assert!(py.contains("import headroom.proxy.semantic_cache as _hd_sc"));
        assert!(py.contains("_hd_sc.SemanticCache.set = _hd_sc_set"));
        // The wrapper must stay async: callers `await self.cache.set(...)`.
        assert!(py.contains("async def _hd_sc_set("));
        assert!(py.contains("await _hd_sc_orig_set("));
        // Error payloads under a 200 are the worst case to cache.
        assert!(py.contains(r#"parsed.get("type") == "error""#));
        assert!(py.contains("HEADROOM_RESPONSE_CACHE_GUARD"));
        // Backend process only: it imports the proxy stack.
        assert!(py.contains(r#"environ.get("HEADROOM_SDK") == "headroom-local-community-proxy""#));
    }

    /// Upstream PR #3106: /v1/responses HTTP outcomes derive optimized from
    /// a messages-only original while tokens_saved includes tool-schema
    /// compaction, clamping optimized to 0 and recording >100% savings
    /// rates. Seams verified against the installed 0.35.0 wheel by
    /// scratchpad verify script on 2026-08-18 (both WS executor call sites
    /// pass `timeout=`, the HTTP site does not; streaming and buffered HTTP
    /// both record through HeadroomProxy._record_request_outcome).
    #[test]
    fn sitecustomize_ports_responses_denominator_guard() {
        let py = super::SITECUSTOMIZE_PY;
        // Both seams patched on the classes the proxy actually uses.
        assert!(py.contains(
            "_hd_rd_openai.OpenAIHandlerMixin._compress_openai_responses_payload_in_executor = ("
        ));
        assert!(py.contains("_hd_rd_server.HeadroomProxy._record_request_outcome = _hd_rd_record"));
        // WS exclusion, both layers: stash-time kwarg check + repair-time tag.
        assert!(py.contains(r#""timeout" not in kwargs"#));
        assert!(py.contains(r#"("endpoint") != "responses_ws""#));
        // Coherence precondition: only the old derivation shape is repaired,
        // so a wheel shipping #3106 natively (also version-gated) and WS
        // delta records pass through untouched.
        assert!(py.contains("== max(0, outcome.original_tokens - outcome.tokens_saved)"));
        assert!(py.contains("if _hd_rd_ver < (0, 36):"));
        assert!(py.contains("HEADROOM_RESPONSES_DENOMINATOR_GUARD"));
        // The stash must be bounded: entries for failed requests never pop.
        assert!(py.contains("_HD_RD_MAX"));
    }

    #[test]
    fn sitecustomize_lifts_codex_additional_tools() {
        // Upstream issue #3185: Codex CLI 0.149.0 moved tool definitions off
        // the top-level tools array into input items of type
        // "additional_tools" (gpt-5.6-sol default), so every tools consumer
        // classified those requests "notools" and savings recorded zero.
        // Verified live against the ChatGPT Codex backend on 2026-08-21:
        // lifted requests compact (608 tokens/turn) and tool calls execute.
        let py = super::SITECUSTOMIZE_PY;
        assert!(py.contains("HEADROOM_ADDITIONAL_TOOLS_GUARD"));
        assert!(py.contains(r#"item.get("type") == "additional_tools""#));
        // Self-neutralizes on classic-encoding requests: a present top-level
        // tools array must short-circuit the lift.
        assert!(py.contains(r#"or payload.get("tools"):"#));
        // The wrapper must be installed on the executor seam (the single
        // funnel for HTTP, WS, and passthrough responses compression)...
        assert!(py.contains(
            "_hd_at_openai.OpenAIHandlerMixin._compress_openai_responses_payload_in_executor = ("
        ));
        // ...and the lift must be symmetric. Upstream #3194: tools is a
        // per-request parameter, additional_tools is an input item and so
        // part of the transcript, so a stateful Codex session (TUI/app-server
        // over WS) declares its tools once and relies on the transcript
        // afterwards -- forwarding the lifted shape gives it tools on turn
        // one and none for the rest of the session. The carrier goes back,
        // carrying the compacted definitions, before the payload is
        // forwarded, and the top-level array is dropped.
        assert!(py.contains("def _hd_at_restore(payload, plan):"));
        assert!(py.contains(r#"payload.pop("tools", None)"#));
        assert!(py.contains("failed = [t for t in targets if not _hd_at_restore(t, plan)]"));
        // Both what the call sites forward (result[0]) and the payload the
        // caller passed in are restored.
        assert!(py.contains("out = result[0] if isinstance(result, tuple) and result else None"));
        assert!(py.contains("targets = [payload]"));
        // Restoring an already-restored payload must not duplicate the
        // definitions, and an emptied array falls back to what Codex sent
        // rather than forwarding a tool-less payload.
        assert!(py.contains("if not isinstance(tools, list) or not tools:"));
        assert!(py.contains("tools = original"));
        // A restore that cannot happen is logged, never silent.
        assert!(py.contains("event=codex_additional_tools_restore_failed"));
        let restore_pos = py.find("def _hd_at_restore").expect("restore present");
        let call_pos = py
            .find("failed = [t for t in targets")
            .expect("restore call present");
        let orig_pos = py
            .find("result = await _hd_at_orig_compress")
            .expect("inner compress call present");
        assert!(restore_pos < orig_pos && orig_pos < call_pos);
        // ...and AFTER the denominator guard's wrapper of the same method, so
        // the lift is outermost and the widening sees the lifted tools.
        let lift_pos = py
            .find("_hd_at_openai.OpenAIHandlerMixin")
            .expect("lift patch present");
        let denom_pos = py
            .find("_hd_rd_openai.OpenAIHandlerMixin")
            .expect("denominator patch present");
        assert!(denom_pos < lift_pos);
    }

    #[test]
    fn sitecustomize_restores_codex_additional_tools_when_compression_raises() {
        let py = super::SITECUSTOMIZE_PY;
        let guard = &py[py
            .find("    # Codex additional_tools guard")
            .expect("additional_tools guard present")..];
        let mut script = r#"
import asyncio
import copy
import os as _hd_os
import sys
import types

headroom = types.ModuleType("headroom")
headroom.__path__ = []
proxy = types.ModuleType("headroom.proxy")
proxy.__path__ = []
handlers = types.ModuleType("headroom.proxy.handlers")
handlers.__path__ = []
openai = types.ModuleType("headroom.proxy.handlers.openai")

class OpenAIHandlerMixin:
    async def _compress_openai_responses_payload_in_executor(self, payload, **kwargs):
        assert payload.get("tools"), payload
        raise TimeoutError("boom")

openai.OpenAIHandlerMixin = OpenAIHandlerMixin
headroom.proxy = proxy
proxy.handlers = handlers
handlers.openai = openai
sys.modules.update({
    "headroom": headroom,
    "headroom.proxy": proxy,
    "headroom.proxy.handlers": handlers,
    "headroom.proxy.handlers.openai": openai,
})
"#
        .to_string();
        script.push_str("if True:\n");
        script.push_str(guard);
        script.push_str(
            r#"
payload = {
    "input": [
        {"type": "message", "content": "before"},
        {
            "type": "additional_tools",
            "id": "carrier",
            "tools": [{"type": "function", "name": "shell"}],
        },
        {"type": "message", "content": "after"},
    ]
}
original = copy.deepcopy(payload)

async def verify():
    try:
        await OpenAIHandlerMixin()._compress_openai_responses_payload_in_executor(payload)
    except TimeoutError:
        pass
    else:
        raise AssertionError("compressor did not raise")
    assert payload == original, (payload, original)

asyncio.run(verify())
"#,
        );

        let python = if cfg!(windows) { "python" } else { "python3" };
        let out = match crate::proc::command(python).args(["-c", &script]).output() {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => panic!("spawn failed: {e}"),
        };
        assert!(
            out.status.success(),
            "additional_tools exception round-trip failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn sitecustomize_ports_context_limit_guard() {
        // Upstream PR #2942: without the guard, long sessions degrade into a
        // compact-every-other-prompt loop once the compressed request hits
        // the model's real window (churn report 2026-08-12). Verified against
        // installed 0.34.0 by scratchpad/verify_sitecustomize.py on 2026-08-12;
        // all three patched seams re-verified present with unchanged
        // signatures in the 0.35.0 wheel on 2026-08-13 (#2942 still open).
        let py = super::SITECUSTOMIZE_PY;
        // The guard imports the full proxy stack; it must only run in the
        // backend process, never in markitdown or other venv Pythons.
        assert!(py.contains(r#"environ.get("HEADROOM_SDK") == "headroom-local-community-proxy""#));
        // All three seams are patched...
        assert!(
            py.contains("_hd_cg_stream.StreamingMixin._stream_response = _hd_cg_stream_response")
        );
        assert!(py.contains("_hd_cg_prov.AnthropicProvider.get_context_limit = _hd_cg_limit"));
        assert!(py.contains(
            "_hd_cg_anth.AnthropicHandlerMixin.handle_anthropic_messages = _hd_cg_handle"
        ));
        // ...the real window is learned from prompt-too-long 400 bodies,
        // keyed by 1m-beta presence so a clamped account never poisons one
        // with real 1M access...
        assert!(py.contains("prompt is too long"));
        assert!(py.contains("_hd_cg_learned.get((model, _hd_cg_has_1m(beta)))"));
        // ...the nudge covers BOTH usage-bearing events -- Claude Code merges
        // the final cumulative-usage message_delta over message_start
        // (verified live 2026-08-12), so rewriting only the first loses the
        // merge...
        assert!(py.contains("_rewrite_start"));
        assert!(py.contains("_rewrite_delta"));
        // ...and the kill switch is honored.
        assert!(py.contains("HEADROOM_CONTEXT_GUARD"));
    }

    #[test]
    fn pre_upstream_concurrency_stays_within_bounds() {
        let value = pre_upstream_concurrency();
        assert!((8..=32).contains(&value), "got {value}");
    }

    #[test]
    fn httpx_ca_bundle_bridge_mirrors_requests_bundle() {
        // Bundle set, SSL_CERT_FILE unset -> mirror it for httpx.
        assert_eq!(
            httpx_ca_bundle_bridge_from(false, Some("/etc/corp/ca.pem")),
            vec![("SSL_CERT_FILE".to_string(), "/etc/corp/ca.pem".to_string())]
        );
        // SSL_CERT_FILE already set -> don't override.
        assert!(httpx_ca_bundle_bridge_from(true, Some("/etc/corp/ca.pem")).is_empty());
        // No bundle, or blank -> nothing to bridge.
        assert!(httpx_ca_bundle_bridge_from(false, None).is_empty());
        assert!(httpx_ca_bundle_bridge_from(false, Some("  ")).is_empty());
    }

    #[test]
    fn classify_kompress_prefetch_failure_buckets_known_causes() {
        assert_eq!(classify_kompress_prefetch_failure(""), "no output");
        assert_eq!(
            classify_kompress_prefetch_failure("python3 abort trap: 6 (SIGABRT)"),
            "native abort"
        );
        assert_eq!(
            classify_kompress_prefetch_failure("OSError: [Errno 28] No space left on device"),
            "disk full"
        );
        assert_eq!(
            classify_kompress_prefetch_failure(
                "requests.exceptions.ConnectionError: Max retries exceeded with url"
            ),
            "network"
        );
        assert_eq!(
            classify_kompress_prefetch_failure(
                "RuntimeError: Cannot send a request, as the client has been closed."
            ),
            "network"
        );
        assert_eq!(
            classify_kompress_prefetch_failure("PermissionError: [Errno 13] Permission denied"),
            "permission denied"
        );
        assert_eq!(
            classify_kompress_prefetch_failure(
                "OSError: [WinError 126] The specified module could not be found. \
                 Error loading \"...\\torch\\lib\\c10.dll\" or one of its dependencies"
            ),
            "missing native dep"
        );
        // RUST-75, verbatim from the event: only the errno survives the locale.
        assert_eq!(
            classify_kompress_prefetch_failure(
                "OSError: [WinError 1114] DLL 초기화 루틴을 실행할 수 없습니다. \
                 Error loading \"...\\torch\\lib\\c10.dll\" or one of its dependencies."
            ),
            "missing native dep"
        );
        assert_eq!(
            classify_kompress_prefetch_failure("ValueError: something unexpected"),
            "other"
        );
    }

    #[test]
    fn summarize_kompress_prefetch_failure_uses_last_meaningful_line() {
        let dir = std::env::temp_dir().join(format!(
            "kompress-prefetch-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("kompress-prefetch.log");
        fs::write(
            &log,
            "Downloading model...\nTraceback (most recent call last):\n  File x\nConnectionError: Max retries exceeded\n\n",
        )
        .unwrap();

        let cause = summarize_kompress_prefetch_failure(&log);
        assert_eq!(cause, "[network] ConnectionError: Max retries exceeded");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn summarize_kompress_prefetch_failure_handles_missing_log() {
        let cause = summarize_kompress_prefetch_failure(&PathBuf::from("/no/such/prefetch.log"));
        assert_eq!(cause, "[no output] (no output in kompress-prefetch.log)");
    }

    /// RUST-75 arrived as a bare torch DLL error. The ONNX failure that
    /// preceded it -- same missing MSVC redistributable, and the actual first
    /// cause -- was dropped, so the report pointed at the wrong library.
    #[test]
    fn summarize_kompress_prefetch_failure_carries_the_earlier_onnx_cause() {
        let dir = std::env::temp_dir().join(format!(
            "kompress-onnx-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("kompress-prefetch.log");
        fs::write(
            &log,
            "Downloading Kompress ONNX model ...\n             WARNING ONNX load failed for kompress-v2-base, trying PyTorch: DLL load failed while importing onnxruntime_pybind11_state\n             Traceback (most recent call last):\n             OSError: [WinError 126] The specified module could not be found. Error loading \"C:\\venv\\torch\\lib\\c10.dll\"\n",
        )
        .unwrap();

        let cause = summarize_kompress_prefetch_failure(&log);
        // Category still comes from the last line, so the fingerprint is stable.
        assert!(
            cause.starts_with("[missing native dep] OSError: [WinError 126]"),
            "{cause}"
        );
        // ...but the ONNX cause that explains it now rides along.
        assert!(cause.contains("(after WARNING ONNX load failed"), "{cause}");
        assert!(cause.contains("onnxruntime_pybind11_state"), "{cause}");

        fs::remove_dir_all(&dir).ok();
    }

    /// An updater relaunch leaves :6868 held by nobody for a moment. Treating
    /// that as a foreign occupant moved the backend to 6770 on every update
    /// (RUST-7F), so the unowned shape is waited out before falling back.
    #[test]
    fn settle_unowned_port_waits_for_a_closing_socket_then_takes_the_port() {
        let calls = std::cell::Cell::new(0);
        let state = settle_unowned_port(
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    PortState::ForeignOccupant(UNKNOWN_OCCUPANT.into())
                } else {
                    PortState::Free
                }
            },
            6,
            Duration::from_millis(0),
        );
        assert!(matches!(state, PortState::Free));
        assert_eq!(calls.get(), 3);
    }

    /// The boot-validation failure path reports the occupant, not just a
    /// bound/unbound bit. A free port and a held one must not read alike.
    #[test]
    fn describe_proxy_port_occupant_separates_free_from_held() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        assert_ne!(describe_proxy_port_occupant(port), "free");
        drop(listener);
        assert_eq!(describe_proxy_port_occupant(port), "free");
    }

    #[test]
    fn settle_unowned_port_does_not_wait_on_a_named_occupant() {
        let calls = std::cell::Cell::new(0);
        let state = settle_unowned_port(
            || {
                calls.set(calls.get() + 1);
                PortState::ForeignOccupant("rapportd pid 594".into())
            },
            6,
            Duration::from_millis(0),
        );
        // A real conflict must fall back immediately, not sit through the wait.
        assert!(matches!(state, PortState::ForeignOccupant(ref d) if d == "rapportd pid 594"));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn settle_unowned_port_gives_up_after_the_attempt_budget() {
        let calls = std::cell::Cell::new(0);
        let state = settle_unowned_port(
            || {
                calls.set(calls.get() + 1);
                PortState::ForeignOccupant(UNKNOWN_OCCUPANT.into())
            },
            6,
            Duration::from_millis(0),
        );
        assert!(matches!(state, PortState::ForeignOccupant(ref d) if d == UNKNOWN_OCCUPANT));
        assert_eq!(calls.get(), 6, "budget must be spent, not exceeded");
    }

    #[test]
    fn redact_sensitive_strips_anthropic_keys() {
        let line = "POST /v1/messages x-api-key: sk-ant-api03-AbCdEf-12_34 done";
        let out = redact_sensitive(line);
        assert!(!out.contains("sk-ant-"), "leak: {out}");
        assert!(out.contains("[REDACTED]"));
        assert!(out.contains("done"));
    }

    #[test]
    fn redact_sensitive_strips_bearer_tokens() {
        let line = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig trailing";
        let out = redact_sensitive(line);
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"), "leak: {out}");
        assert!(out.contains("[REDACTED]"));
        assert!(out.contains("trailing"));
    }

    #[test]
    fn redact_sensitive_passes_through_clean_lines() {
        let line = "2026-05-03T20:31:34Z proxy started on 127.0.0.1:6867";
        assert_eq!(redact_sensitive(line), line);
    }

    #[test]
    fn redact_sensitive_ignores_short_bearer_word() {
        // "Bearer" followed by something too short to be a real token shouldn't
        // be redacted — we don't want to nuke unrelated prose.
        let line = "the Bearer of the message is fine";
        assert_eq!(redact_sensitive(line), line);
    }

    fn cmd_failure_with_stderr(stderr: &str) -> anyhow::Error {
        anyhow::Error::new(CommandFailure {
            program: "/runtime/venv/bin/headroom".into(),
            args: vec!["mcp".into(), "install".into()],
            stdout: String::new(),
            stderr: stderr.into(),
            exit_code: Some(1),
            signal: None,
        })
    }

    #[test]
    fn looks_like_corrupt_venv_error_matches_module_not_found() {
        let err = cmd_failure_with_stderr(
            "Traceback (most recent call last):\n\
             ...\n\
             ModuleNotFoundError: No module named 'opentelemetry'\n",
        );
        assert!(looks_like_corrupt_venv_error(&err));
    }

    #[test]
    fn looks_like_corrupt_venv_error_matches_import_error() {
        let err = cmd_failure_with_stderr(
            "Traceback (most recent call last):\n\
             ImportError: cannot import name 'X' from partially initialized module 'Y'\n",
        );
        assert!(looks_like_corrupt_venv_error(&err));
    }

    #[test]
    fn looks_like_corrupt_venv_error_ignores_unrelated_failures() {
        let err = cmd_failure_with_stderr("error: invalid --proxy-url\n");
        assert!(!looks_like_corrupt_venv_error(&err));
    }

    #[test]
    fn looks_like_corrupt_venv_error_ignores_non_command_errors() {
        let err = anyhow::anyhow!("some other failure with ModuleNotFoundError in the message");
        // Only CommandFailure errors carry the structured stderr we trust as a
        // corrupt-venv signal — a bare anyhow message could be anything.
        assert!(!looks_like_corrupt_venv_error(&err));
    }

    #[test]
    fn looks_like_corrupt_venv_error_survives_anyhow_context() {
        use anyhow::Context as _;
        let err = cmd_failure_with_stderr("ModuleNotFoundError: No module named 'opentelemetry'\n");
        let wrapped = Err::<(), _>(err)
            .context("configuring Headroom MCP integration")
            .unwrap_err();
        assert!(looks_like_corrupt_venv_error(&wrapped));
    }

    #[test]
    fn requirements_lock_sha_ignores_comments_and_blank_lines() {
        let a = "# header one\n\nabsl-py==2.4.0\naiohttp==3.13.5\n";
        let b = "# header two — different\naiohttp==3.13.5\nabsl-py==2.4.0\n";
        let c = "absl-py==2.4.0\naiohttp==3.13.6\n";
        // Same pinned versions, different comments/whitespace → same hash.
        assert_eq!(requirements_lock_sha(a), requirements_lock_sha(a));
        // Order still matters (pip resolution order), so (a) and (b) differ.
        assert_ne!(requirements_lock_sha(a), requirements_lock_sha(b));
        // A real version bump changes the hash.
        assert_ne!(requirements_lock_sha(a), requirements_lock_sha(c));
        // Adding/removing a comment or blank line must not change the hash.
        let a_more_comments =
            "# header one\n# extra note\n\n\nabsl-py==2.4.0\n# inline\naiohttp==3.13.5\n";
        assert_eq!(
            requirements_lock_sha(a),
            requirements_lock_sha(a_more_comments)
        );
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn run_command_failure_carries_structured_output() {
        let tmp = std::env::temp_dir();
        let err = run_command(
            std::path::Path::new("/bin/sh"),
            &["-c", "echo hi-out; echo hi-err 1>&2; exit 7"],
            &tmp,
        )
        .expect_err("command should have failed");

        let failure = err
            .chain()
            .find_map(|e| e.downcast_ref::<CommandFailure>())
            .expect("CommandFailure should be in the error chain");

        assert_eq!(failure.exit_code, Some(7));
        assert!(
            failure.stdout.contains("hi-out"),
            "stdout: {}",
            failure.stdout
        );
        assert!(
            failure.stderr.contains("hi-err"),
            "stderr: {}",
            failure.stderr
        );
        assert_eq!(failure.program, "/bin/sh");
    }

    #[test]
    fn managed_python_paths_live_inside_headroom_root() {
        let root = std::env::temp_dir().join("headroom-tool-manager-test");
        let runtime = ManagedRuntime::bootstrap_root(&root);

        assert!(runtime.managed_python().starts_with(&runtime.root_dir));
        assert!(runtime.standalone_python().starts_with(&runtime.root_dir));
        assert!(runtime.managed_pip().starts_with(&runtime.root_dir));
        assert!(runtime.bin_dir.starts_with(&runtime.root_dir));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn managed_runtime_uses_windows_layout() {
        let runtime =
            ManagedRuntime::bootstrap_root(&std::env::temp_dir().join("headroom-layout-test"));
        assert!(runtime.standalone_python().ends_with("python\\python.exe"));
        assert!(runtime.managed_python().ends_with("Scripts\\python.exe"));
        assert!(runtime.managed_pip().ends_with("Scripts\\pip.exe"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn tool_manager_entrypoints_use_windows_layout() {
        let runtime =
            ManagedRuntime::bootstrap_root(&std::env::temp_dir().join("headroom-entrypoints-test"));
        let manager = ToolManager::new(runtime);
        assert!(manager
            .headroom_entrypoint()
            .ends_with("Scripts\\headroom.exe"));
        assert!(manager.rtk_entrypoint().ends_with("bin\\rtk.exe"));
        assert!(manager
            .markitdown_shim_path()
            .ends_with("bin\\markitdown.cmd"));
    }

    /// The wheel carries a native `_core` extension, so the platform tag in
    /// its filename must match the platform we're installing on. Runs on every
    /// target: a macOS wheel shipped to Windows is what broke RUST-6E.
    #[test]
    fn pinned_headroom_wheel_matches_running_platform() {
        let release = pinned_headroom_release().expect("pinned wheel for this platform");
        let expected_tag = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "macosx_11_0_arm64",
            ("macos", "x86_64") => "macosx_10_12_x86_64",
            ("linux", "aarch64") => "manylinux_2_28_aarch64",
            ("linux", "x86_64") => "manylinux_2_28_x86_64",
            ("windows", "x86_64") => "win_amd64",
            (os, arch) => panic!("test needs a tag for {os}/{arch}"),
        };
        assert!(
            release.wheel_url.ends_with(&format!("{expected_tag}.whl")),
            "wheel {} is not built for {}/{}",
            release.wheel_url,
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        assert!(
            release
                .wheel_url
                .contains(&format!("headroom_ai-{HEADROOM_PINNED_VERSION}-")),
            "wheel url {} does not carry the pinned version",
            release.wheel_url
        );
        assert_eq!(release.sha256.len(), 64, "sha256 must be pinned");
    }

    /// Downloads keep PyPI's filename so pip's own platform check stays a
    /// backstop; a `py3-none-any` rename is what let a macOS wheel install on
    /// Windows.
    #[test]
    fn wheel_download_path_keeps_platform_tagged_filename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = ToolManager::new(ManagedRuntime::bootstrap_root(dir.path()));
        let release = pinned_headroom_release().expect("pinned wheel for this platform");
        let path = manager.wheel_download_path(&release.wheel_url);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            release.wheel_url.rsplit('/').next()
        );
        assert!(!path.to_string_lossy().contains("py3-none-any"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn python_distribution_artifact_supports_windows_x86_64() {
        let artifact = python_distribution_artifact().expect("windows python target");
        assert!(artifact.url.contains("x86_64-pc-windows-msvc"));
        assert!(artifact.url.ends_with(".tar.gz"));
        assert!(
            artifact.sha256.is_some(),
            "python checksum should be pinned"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn rtk_distribution_artifact_supports_windows_x86_64() {
        let artifact = rtk_distribution_artifact().expect("windows rtk target");
        assert!(artifact.url.contains("x86_64-pc-windows-msvc"));
        assert!(artifact.url.ends_with(".zip"));
        assert!(artifact.sha256.is_some(), "rtk checksum should be pinned");
    }

    #[test]
    fn bootstrap_requirements_lock_targets_windows() {
        assert_eq!(
            bootstrap_requirements_lock_for_target("windows"),
            HEADROOM_WINDOWS_REQUIREMENTS_LOCK
        );
        assert_eq!(
            bootstrap_requirements_lock_for_target("linux"),
            HEADROOM_LINUX_REQUIREMENTS_LOCK
        );
        assert_eq!(
            bootstrap_requirements_lock_for_target("macos"),
            HEADROOM_REQUIREMENTS_LOCK
        );
        // hnswlib is sdist-only on PyPI with no vendored win_amd64 wheel; a
        // pin here bricks bootstrap on any Windows box without MSVC (RUST-65).
        assert!(!HEADROOM_WINDOWS_REQUIREMENTS_LOCK.contains("hnswlib=="));
        assert!(HEADROOM_WINDOWS_REQUIREMENTS_LOCK.contains("sqlite-vec=="));
    }

    #[test]
    fn serena_venv_lives_outside_runtime_dir_so_upgrades_keep_it() {
        let root = std::env::temp_dir().join("headroom-tool-manager-test");
        let runtime = ManagedRuntime::bootstrap_root(&root);
        let runtime_dir = runtime.runtime_dir.clone();
        let manager = ToolManager::new(runtime);

        assert!(manager
            .serena_venv_dir()
            .starts_with(&manager.runtime.root_dir));
        assert!(!manager.serena_venv_dir().starts_with(&runtime_dir));
        assert!(manager
            .serena_entrypoint()
            .starts_with(manager.serena_venv_dir()));
    }

    #[test]
    fn kompress_marker_scan_treats_deferred_and_lazy_load_as_enabled() {
        let (root, runtime, manager) = seed_test_runtime("kompress-marker");
        fs::create_dir_all(runtime.logs_dir()).expect("logs dir");
        let enabled: &[&str] = &[
            "kompress: enabled",
            "kompress: deferred",
            "kompress onnx loaded",
            "kompress pytorch loaded",
            "kompress slow compress backend=",
        ];
        let disabled: &[&str] = &["kompress: not installed", "kompress: disabled"];

        // Current headroom-ai defers native model initialization until the
        // first request. DEFERRED means the feature is configured and usable,
        // not unknown or disabled.
        let deferred_log = runtime.logs_dir().join("kompress-deferred.log");
        fs::write(
            &deferred_log,
            "2026-06-12 10:00:00 - headroom.proxy - INFO - Kompress: DEFERRED (model loads on first request)\n",
        )
        .expect("write deferred log");
        assert_eq!(
            manager.scan_file_for_marker_state_cached(
                "k-deferred",
                &deferred_log,
                enabled,
                disabled,
            ),
            Some(true),
            "a deferred startup should report enabled before first use"
        );

        // Cold-cache startup logs "not installed"; a later 0.36+ compression
        // logs the backend actually used. The most-recent marker wins, so the
        // desktop reports enabled without a restart.
        let log = runtime.logs_dir().join("kompress-lazy.log");
        fs::write(
            &log,
            "2026-06-12 10:00:00 - headroom.proxy - INFO - Kompress: not installed (pip install headroom-ai[ml])\n\
             2026-06-12 10:05:00 - headroom.transforms.kompress_compressor - INFO - Kompress slow compress backend=onnx input_tokens=4096 output_tokens=2048\n",
        )
        .expect("write lazy log");
        assert_eq!(
            manager.scan_file_for_marker_state_cached("k-lazy", &log, enabled, disabled),
            Some(true),
            "a compression line after a not-installed line should report enabled"
        );

        // A pure cold-cache log (no lazy load yet) still reports disabled.
        let log2 = runtime.logs_dir().join("kompress-cold.log");
        fs::write(
            &log2,
            "2026-06-12 10:00:00 - headroom.proxy - INFO - Kompress: not installed (pip install headroom-ai[ml])\n",
        )
        .expect("write cold log");
        assert_eq!(
            manager.scan_file_for_marker_state_cached("k-cold", &log2, enabled, disabled),
            Some(false),
            "a not-installed line with no later load should report disabled"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rtk_distribution_artifact_is_pinned_to_current_release_with_checksum() {
        assert_eq!(RTK_VERSION, "0.47.0");
        let artifact = rtk_distribution_artifact().expect("supported RTK target");

        assert!(artifact.url.contains(&format!("/v{RTK_VERSION}/")));
        assert!(
            artifact.sha256.is_some(),
            "RTK artifact checksum should be pinned"
        );
    }

    #[test]
    fn tool_manifest_exposes_platform_rtk_checksum() {
        let root = std::env::temp_dir().join("headroom-tool-manager-manifest-test");
        let runtime = ManagedRuntime::bootstrap_root(&root);
        let manager = ToolManager::new(runtime);

        let rtk = manager
            .list_tools()
            .into_iter()
            .find(|tool| tool.id == "rtk")
            .expect("rtk manifest should exist");
        assert_eq!(rtk.version, RTK_VERSION);
        assert!(rtk.checksum.is_some(), "RTK checksum should be exposed");
    }

    #[test]
    fn rtk_installed_requires_binary_and_receipt() {
        let (root, _runtime, manager) = seed_test_runtime("rtk-installed");

        assert!(!manager.rtk_installed(), "no binary or receipt yet");

        write_executable(&manager.rtk_entrypoint(), "#!/usr/bin/env bash\nexit 0\n");
        assert!(
            !manager.rtk_installed(),
            "binary alone should not count as installed"
        );

        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");
        assert!(manager.rtk_installed(), "binary + receipt should count");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_rtk_version_reads_receipt() {
        let (root, _runtime, manager) = seed_test_runtime("rtk-version");
        write_executable(&manager.rtk_entrypoint(), "#!/usr/bin/env bash\nexit 0\n");
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": "0.37.2-test" }))
            .expect("rtk receipt");

        assert_eq!(
            manager.installed_rtk_version().as_deref(),
            Some("0.37.2-test")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn headroom_upgrade_target_comparison_is_strict_and_local() {
        let (root, _runtime, manager) = seed_test_runtime("headroom-target-comparison");

        assert!(manager.headroom_upgrade_target_is_newer("0.0.2"));
        assert!(!manager.headroom_upgrade_target_is_newer("0.0.1"));
        assert!(!manager.headroom_upgrade_target_is_newer("latest"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rtk_needs_install_true_when_binary_missing() {
        let (root, _runtime, manager) = seed_test_runtime("rtk-needs-install-missing");
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");
        assert!(manager.rtk_needs_install());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rtk_needs_install_true_when_version_stale() {
        let (root, runtime, manager) = seed_test_runtime("rtk-needs-install-stale");
        write_executable(
            &runtime.bin_dir.join("rtk"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": "0.0.1-old" }))
            .expect("rtk receipt");
        assert!(manager.rtk_needs_install());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rtk_needs_install_false_when_current() {
        let (root, _runtime, manager) = seed_test_runtime("rtk-needs-install-current");
        write_executable(&manager.rtk_entrypoint(), "#!/usr/bin/env bash\nexit 0\n");
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");
        assert!(!manager.rtk_needs_install());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ensure_rtk_current_is_noop_when_already_current() {
        let (root, runtime, manager) = seed_test_runtime("rtk-ensure-current-noop");
        write_executable(
            &runtime.bin_dir.join("rtk"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");
        let did_work = manager.ensure_rtk_current().expect("ensure_rtk_current");
        assert!(!did_work, "should skip install when already current");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ensure_rtk_current_is_noop_when_binary_absent() {
        // RTK is opt-in: a missing binary means uninstalled/never-installed, so
        // launch must not create a fresh install.
        let (root, _runtime, manager) = seed_test_runtime("rtk-ensure-current-absent");
        let did_work = manager.ensure_rtk_current().expect("ensure_rtk_current");
        assert!(!did_work, "should not install rtk when binary is absent");
        assert!(!manager.rtk_installed(), "rtk must remain uninstalled");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_rtk_activity_reports_not_installed_when_missing() {
        let (root, _runtime, manager) = seed_test_runtime("rtk-not-installed");

        let lines = manager
            .read_rtk_activity(10)
            .expect("not-installed fallback");
        assert_eq!(lines, vec!["RTK is not installed yet.".to_string()]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // read_rtk_activity spawns `rtk session`; the fake is a shell script
    fn read_rtk_activity_returns_last_lines_from_session_output() {
        let (root, _runtime, manager) = seed_test_runtime("rtk-activity");
        write_executable(
            &manager.rtk_entrypoint(),
            "#!/usr/bin/env bash\nif [ \"$1\" = \"session\" ]; then\n  printf 'line-1\\nline-2\\nline-3\\nline-4\\n';\n  exit 0\nfi\nexit 9\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");

        let lines = manager.read_rtk_activity(2).expect("session output");
        assert_eq!(lines, vec!["line-3".to_string(), "line-4".to_string()]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // read_rtk_activity spawns `rtk session`; the fake is a shell script
    fn read_rtk_activity_surfaces_session_failures() {
        let (root, _runtime, manager) = seed_test_runtime("rtk-activity-fail");
        write_executable(
            &manager.rtk_entrypoint(),
            "#!/usr/bin/env bash\nif [ \"$1\" = \"session\" ]; then\n  echo 'session stdout';\n  echo 'session stderr' 1>&2;\n  exit 7\nfi\nexit 9\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");

        let err = manager
            .read_rtk_activity(10)
            .expect_err("failing session should surface an error");
        let msg = err.to_string();
        assert!(msg.contains("session stdout"), "stdout preserved: {msg}");
        assert!(msg.contains("session stderr"), "stderr preserved: {msg}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn rtk_gain_summary_parses_summary_json() {
        let (root, runtime, manager) = seed_test_runtime("rtk-gain-summary");
        write_executable(
            &runtime.bin_dir.join("rtk"),
            "#!/usr/bin/env bash\nif [ \"$1\" = \"gain\" ]; then\n  echo '{\"summary\":{\"total_commands\":7,\"total_saved\":1234,\"avg_savings_pct\":61.5},\"daily\":[]}';\n  exit 0\nfi\nexit 9\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");

        let summary = manager.rtk_gain_summary().expect("gain summary");
        assert_eq!(summary.total_commands, 7);
        assert_eq!(summary.total_saved, 1234);
        assert_eq!(summary.avg_savings_pct, 61.5);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn rtk_today_stats_returns_matching_daily_row() {
        let (root, runtime, manager) = seed_test_runtime("rtk-today");
        let today = Local::now().date_naive().to_string();
        let script = format!(
            "#!/usr/bin/env bash\nif [ \"$1\" = \"gain\" ]; then\n  cat <<'EOF'\n{{\"daily\":[{{\"date\":\"1999-01-01\",\"commands\":1,\"saved_tokens\":2}},{{\"date\":\"{today}\",\"commands\":7,\"saved_tokens\":1234}}]}}\nEOF\n  exit 0\nfi\nexit 9\n",
        );
        write_executable(&runtime.bin_dir.join("rtk"), &script);
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");

        let stats = manager.rtk_today_stats().expect("today stats");
        assert_eq!(stats.date, today);
        assert_eq!(stats.commands, 7);
        assert_eq!(stats.saved_tokens, 1234);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rtk_today_stats_returns_none_when_today_absent() {
        let (root, runtime, manager) = seed_test_runtime("rtk-today-missing");
        write_executable(
            &runtime.bin_dir.join("rtk"),
            "#!/usr/bin/env bash\nif [ \"$1\" = \"gain\" ]; then\n  echo '{\"daily\":[{\"date\":\"1999-01-01\",\"commands\":1,\"saved_tokens\":2}]}';\n  exit 0\nfi\nexit 9\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");

        assert!(manager.rtk_today_stats().is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rtk_gain_summary_returns_none_when_summary_absent() {
        let (root, runtime, manager) = seed_test_runtime("rtk-gain-missing");
        write_executable(
            &runtime.bin_dir.join("rtk"),
            "#!/usr/bin/env bash\nif [ \"$1\" = \"gain\" ]; then\n  echo '{\"daily\":[]}';\n  exit 0\nfi\nexit 9\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");

        assert!(manager.rtk_gain_summary().is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rtk_gain_summary_returns_none_on_command_failure() {
        let (root, runtime, manager) = seed_test_runtime("rtk-gain-fail");
        write_executable(
            &runtime.bin_dir.join("rtk"),
            "#!/usr/bin/env bash\nif [ \"$1\" = \"gain\" ]; then\n  echo 'boom' 1>&2;\n  exit 4\nfi\nexit 9\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");

        assert!(manager.rtk_gain_summary().is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rtk_gain_summary_returns_none_on_invalid_json() {
        let (root, runtime, manager) = seed_test_runtime("rtk-gain-invalid-json");
        write_executable(
            &runtime.bin_dir.join("rtk"),
            "#!/usr/bin/env bash\nif [ \"$1\" = \"gain\" ]; then\n  echo 'not-json';\n  exit 0\nfi\nexit 9\n",
        );
        manager
            .write_tool_receipt("rtk", serde_json::json!({ "version": RTK_VERSION }))
            .expect("rtk receipt");

        assert!(manager.rtk_gain_summary().is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bootstrap_all_installs_into_temp_root_when_enabled() {
        if std::env::var("HEADROOM_RUN_NETWORK_TESTS").is_err() {
            return;
        }

        let root = std::env::temp_dir().join(format!("headroom-e2e-{}", uuid::Uuid::new_v4()));
        let runtime = ManagedRuntime::bootstrap_root(&root);
        let manager = ToolManager::new(runtime.clone());

        manager
            .bootstrap_all_with_progress(|_| {})
            .expect("bootstrap succeeds");

        assert!(runtime.managed_python().exists());
        assert!(runtime.tools_dir.join("headroom.json").exists());
        assert!(runtime.bin_dir.join("rtk").exists());
    }

    #[test]
    fn listener_identity_formats_and_truncates_on_char_boundaries() {
        assert_eq!(
            super::format_listener_identity("nginx", 42, None),
            "nginx (pid 42)"
        );
        assert_eq!(
            super::format_listener_identity("python3.12", 7, Some("  headroom proxy --port 6867 ")),
            "python3.12 (pid 7): headroom proxy --port 6867"
        );
        // A multibyte char straddling the cap must not panic the truncate.
        let argv = format!("{}é", "x".repeat(159));
        let got = super::format_listener_identity("app", 1, Some(&argv));
        assert!(got.ends_with("..."), "{got}");
        assert!(got.len() < 200);
    }

    #[test]
    fn proxy_argv_matches_when_all_expected_flags_present() {
        let argv = "/Users/x/headroom proxy --port 6868 --log-messages \
                    --learn --no-memory-tools --no-memory-context --memory-db-path /tmp/m.db";
        assert!(proxy_argv_contains_expected_flags(argv, true));
    }

    #[test]
    fn proxy_argv_matches_legacy_nice_wrapped_orphan() {
        // Builds before 2026-08-17 spawned the backend under `nice`. Upgrading
        // users still have one of those running, and it must be recognized as
        // ours rather than treated as a foreign occupant of the port.
        let argv = "/usr/bin/nice -n 2 /Users/x/headroom proxy --port 6868 --log-messages \
                    --learn --no-memory-tools --no-memory-context --memory-db-path /tmp/m.db";
        assert!(proxy_argv_contains_expected_flags(argv, true));
    }

    #[test]
    fn proxy_argv_matches_without_learn_flags_when_auto_learn_off() {
        let argv = "/Users/x/headroom proxy --port 6868 --no-http2 --log-messages";
        assert!(proxy_argv_contains_expected_flags(argv, false));
    }

    #[test]
    fn proxy_argv_mismatch_when_learn_present_but_auto_learn_off() {
        // Leftover learn-enabled proxy from before the toggle flipped: restart.
        let argv = "/Users/x/headroom proxy --port 6868 --log-messages --learn \
                    --no-memory-tools --no-memory-context --memory-db-path /tmp/m.db";
        assert!(!proxy_argv_contains_expected_flags(argv, false));
    }

    #[test]
    fn proxy_argv_mismatch_when_log_messages_missing() {
        // The exact orphan-from-old-build case: a v0.2.x proxy still running
        // with just `proxy --port 6868`.
        let argv = "/Users/x/headroom proxy --port 6868";
        assert!(!proxy_argv_contains_expected_flags(argv, true));
    }

    #[test]
    fn proxy_argv_mismatch_when_learn_missing() {
        let argv = "headroom proxy --port 6868 --log-messages --no-memory-tools \
                    --no-memory-context --memory-db-path /tmp/m.db";
        assert!(!proxy_argv_contains_expected_flags(argv, true));
    }

    #[test]
    fn proxy_argv_match_does_not_get_fooled_by_negated_flag_substring() {
        // `--no-learn` contains `--learn` as a substring; whitespace tokenizing
        // ensures we don't false-positive on it.
        let argv = "headroom proxy --port 6868 --log-messages --no-learn \
                    --no-memory-tools --no-memory-context --memory-db-path /tmp/m.db";
        assert!(!proxy_argv_contains_expected_flags(argv, true));
    }

    #[test]
    fn proxy_argv_match_works_for_python_module_invocation() {
        let argv = "/Users/x/venv/bin/python3 -m headroom.proxy.server --port 6868 \
                    --no-http2 --log-messages --learn --no-memory-tools --no-memory-context \
                    --memory-db-path /tmp/m.db";
        assert!(proxy_argv_contains_expected_flags(argv, true));
    }

    #[test]
    fn sanitize_log_variant_replaces_path_separators() {
        let raw = "proxy---memory-db-path-/Users/x/Library/Application Support/Headroom/memory.db";
        let cleaned = sanitize_log_variant(raw);
        assert!(
            !cleaned.contains('/'),
            "expected no slashes, got: {cleaned}"
        );
        assert!(!cleaned.contains('\\'));
        assert!(cleaned.contains("memory-db-path"));
    }

    #[test]
    fn sanitize_log_variant_truncates_long_input() {
        let raw = "a".repeat(500);
        let cleaned = sanitize_log_variant(&raw);
        assert_eq!(cleaned.len(), 80);
    }

    #[test]
    fn sanitize_log_variant_keeps_short_safe_input_unchanged() {
        let raw = "proxy---port-6868---log-messages---learn";
        let cleaned = sanitize_log_variant(raw);
        assert_eq!(cleaned, raw);
    }

    #[test]
    fn prefetch_bootstrap_artifacts_is_a_no_op_when_runtime_is_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = ManagedRuntime::bootstrap_root(dir.path());
        for marker in [runtime.standalone_python(), runtime.managed_python()] {
            fs::create_dir_all(marker.parent().expect("parent")).expect("mkdir");
            fs::write(&marker, b"").expect("marker");
        }
        let downloads_dir = runtime.downloads_dir.clone();

        // Both gates satisfied: must succeed without attempting any network
        // download (an attempt would be a gate bug, and flaky offline).
        ToolManager::new(runtime)
            .prefetch_bootstrap_artifacts()
            .expect("no-op prefetch succeeds");

        let leftovers: Vec<_> = fs::read_dir(&downloads_dir)
            .expect("downloads dir exists")
            .collect();
        assert!(
            leftovers.is_empty(),
            "no-op prefetch must not write into downloads: {leftovers:?}"
        );
    }

    #[test]
    fn prefetch_tiktoken_encodings_gates_before_spawning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = ManagedRuntime::bootstrap_root(dir.path());
        let manager = ToolManager::new(runtime);

        // Empty cache but no managed python: must bail, not hang or succeed.
        let err = manager
            .prefetch_tiktoken_encodings()
            .expect_err("missing python must error");
        assert!(err.to_string().contains("managed python not found"));

        // Seeded cache: no-op success without spawning python (which does not
        // exist here) or touching the network.
        let cache = manager.tiktoken_cache_dir();
        fs::create_dir_all(&cache).expect("mkdir cache");
        fs::write(cache.join("seed"), b"x").expect("seed cache");
        manager
            .prefetch_tiktoken_encodings()
            .expect("seeded cache is a no-op");
    }

    #[test]
    fn rotate_log_if_large_rotates_only_past_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("headroom-default.log");

        fs::write(&log, b"small").expect("write small log");
        rotate_log_if_large(&log);
        assert!(log.exists(), "small log must not rotate");
        assert!(!log.with_extension("log.old").exists());

        fs::write(&log, vec![b'x'; 5 * 1024 * 1024 + 1]).expect("write big log");
        rotate_log_if_large(&log);
        assert!(!log.exists(), "oversized log must be renamed away");
        assert_eq!(
            fs::metadata(log.with_extension("log.old"))
                .expect("old gen")
                .len(),
            5 * 1024 * 1024 + 1
        );
    }

    #[test]
    fn log_tail_returns_last_bytes_and_handles_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("t.log");

        assert_eq!(log_tail(&log, 1024), "", "missing file -> empty");

        fs::write(&log, b"  hello  ").expect("write");
        assert_eq!(log_tail(&log, 1024), "hello", "trims whitespace");

        fs::write(&log, b"0123456789").expect("write");
        assert_eq!(log_tail(&log, 4), "6789", "seeks to last N bytes");
    }

    #[test]
    fn parse_pid_from_lsof_detail_extracts_numeric_pid() {
        assert_eq!(parse_pid_from_lsof_detail("rapportd pid 594"), Some(594));
        assert_eq!(
            parse_pid_from_lsof_detail("python3.12 pid 1073"),
            Some(1073)
        );
        assert_eq!(
            parse_pid_from_lsof_detail("Google Chrome Helper pid 4242"),
            Some(4242)
        );
    }

    /// The real path observed on a Windows box was
    /// `...\Headroom\headroom\runtime\python\python.exe` -- the BASE
    /// interpreter. A first cut of this gate required "venv" in the path, which
    /// that never matches, so the identity check could never pass and every
    /// reclaim stayed dead.
    #[test]
    fn exe_path_is_under_accepts_both_windows_python_layouts() {
        let runtime = PathBuf::from(r"C:\Users\garm\AppData\Local\Headroom\headroom\runtime");
        assert!(exe_path_is_under(
            r"C:\Users\garm\AppData\Local\Headroom\headroom\runtime\python\python.exe",
            &runtime
        ));
        assert!(exe_path_is_under(
            r"C:\Users\garm\AppData\Local\Headroom\headroom\runtime\venv\Scripts\headroom.exe",
            &runtime
        ));
        // PowerShell output arrives with a trailing newline and need not match
        // our casing.
        assert!(exe_path_is_under(
            "c:\\users\\garm\\appdata\\local\\headroom\\headroom\\runtime\\python\\python.exe\r\n",
            &runtime
        ));
    }

    /// A prefix match that does not land on a separator would point a kill at a
    /// stranger.
    #[test]
    fn exe_path_is_under_rejects_neighbours_and_strangers() {
        let runtime = PathBuf::from(r"C:\Users\garm\AppData\Local\Headroom\headroom\runtime");
        assert!(!exe_path_is_under(
            r"C:\Users\garm\AppData\Local\Headroom\headroom\runtime-old\python\python.exe",
            &runtime
        ));
        assert!(!exe_path_is_under(r"C:\Python312\python.exe", &runtime));
        assert!(!exe_path_is_under(
            r"C:\Users\garm\AppData\Local\Headroom\headroom-local-community.exe",
            &runtime
        ));
        // Pid already gone: PowerShell prints nothing. Never provably ours.
        assert!(!exe_path_is_under("", &runtime));
        assert!(!exe_path_is_under("   \r\n", &runtime));
    }

    /// Windows could not name a port holder at all before this (no lsof, no
    /// ss), so every occupant read as "unknown process" and no reclaim could
    /// pass its identity gate.
    #[test]
    fn parse_netstat_listener_finds_the_listening_pid() {
        let out = "\r\nActive Connections\r\n\r\n  Proto  Local Address          Foreign Address        State           PID\r\n  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1044\r\n  TCP    127.0.0.1:6868         0.0.0.0:0              LISTENING       9876\r\n  TCP    [::1]:6867             [::]:0                 LISTENING       4321\r\n";
        assert_eq!(parse_netstat_listener(out, 6868), Some(9876));
        // IPv6 rows are `[::1]:6867`; only the LAST colon separates the port.
        assert_eq!(parse_netstat_listener(out, 6867), Some(4321));
        assert_eq!(parse_netstat_listener(out, 7000), None);
    }

    /// Suffix matching would point a kill at whatever holds :16868.
    #[test]
    fn parse_netstat_listener_does_not_match_a_port_by_suffix() {
        let out = "  TCP    127.0.0.1:16868        0.0.0.0:0              LISTENING       5555\r\n";
        assert_eq!(parse_netstat_listener(out, 6868), None);
    }

    /// An ESTABLISHED row for the same port is a client, not the holder.
    #[test]
    fn parse_netstat_listener_ignores_non_listening_rows() {
        let out = "  TCP    127.0.0.1:6868         127.0.0.1:52100        ESTABLISHED     7777\r\n";
        assert_eq!(parse_netstat_listener(out, 6868), None);
    }

    #[test]
    fn parse_tasklist_image_reads_the_csv_image_name() {
        let out = "\"python.exe\",\"9876\",\"Console\",\"1\",\"45,678 K\"\r\n";
        assert_eq!(parse_tasklist_image(out), Some("python.exe".to_string()));
        // `/FI` with no match prints an INFO line, not a CSV row.
        assert_eq!(
            parse_tasklist_image("INFO: No tasks are running which match the specified criteria."),
            None
        );
    }

    #[test]
    fn parse_lsof_listener_reads_the_row_below_the_header() {
        let out = "COMMAND   PID USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\n\
                   python  12345 garm    7u  IPv4 0x1234      0t0  TCP 127.0.0.1:6868 (LISTEN)\n";
        assert_eq!(parse_lsof_listener(out), Some(("python".into(), 12345)));
        // Header only: lsof found nothing, which is not a listener.
        assert_eq!(
            parse_lsof_listener("COMMAND   PID USER   FD   TYPE DEVICE\n"),
            None
        );
    }

    #[test]
    fn parse_ss_listener_picks_the_row_for_the_requested_port() {
        let out = "State  Recv-Q Send-Q Local Address:Port  Peer Address:Port Process\n\
             LISTEN 0      4096       127.0.0.1:16868      0.0.0.0:*     users:((\"decoy\",pid=1,fd=3))\n\
             LISTEN 0      4096       127.0.0.1:6868       0.0.0.0:*     users:((\"python\",pid=12345,fd=7))\n";
        // :16868 must not satisfy a :6868 lookup.
        assert_eq!(parse_ss_listener(out, 6868), Some(("python".into(), 12345)));
        assert_eq!(parse_ss_listener(out, 16868), Some(("decoy".into(), 1)));
        assert_eq!(parse_ss_listener(out, 9999), None);
    }

    #[test]
    fn parse_ss_listener_returns_none_without_process_ownership() {
        // ss omits `users:` for processes the caller does not own. Guessing a
        // pid here would point the kill path at the wrong process.
        let out = "State  Recv-Q Send-Q Local Address:Port Peer Address:Port\n\
                   LISTEN 0      4096       0.0.0.0:6868       0.0.0.0:*\n";
        assert_eq!(parse_ss_listener(out, 6868), None);
    }

    #[test]
    fn parse_pid_from_lsof_detail_returns_none_for_unknown_or_malformed() {
        assert_eq!(parse_pid_from_lsof_detail("unknown process"), None);
        assert_eq!(parse_pid_from_lsof_detail(""), None);
        assert_eq!(
            parse_pid_from_lsof_detail("rapportd pid not-a-number"),
            None
        );
        // Missing the " pid " separator entirely.
        assert_eq!(parse_pid_from_lsof_detail("rapportd 594"), None);
    }

    /// Round-trip: the bail string produced by `format_all_foreign_bail` must
    /// be matched by `port_conflict::is_port_conflict` (so the persistent-
    /// conflict marker keeps tracking it) AND the occupant must be parseable
    /// by `port_conflict::parse_occupant` (so analytics/Sentry get the
    /// process name and pid).
    #[test]
    fn all_foreign_bail_round_trips_through_port_conflict_helpers() {
        let bail = format_all_foreign_bail(6868, "rapportd pid 594", (6869, 6890));
        assert!(
            port_conflict::is_port_conflict(&bail),
            "bail must match is_port_conflict so the marker keeps tracking; got: {bail}"
        );
        let (cmd, pid) = port_conflict::parse_occupant(&bail);
        assert_eq!(cmd.as_deref(), Some("rapportd"), "bail: {bail}");
        assert_eq!(pid, Some(594), "bail: {bail}");
    }

    /// Mirror round-trip for the unknown-occupant path (lsof returned nothing
    /// useful). `parse_occupant` should return None/None instead of inventing
    /// a fake cmd from "unknown process".
    #[test]
    fn all_foreign_bail_with_unknown_occupant_round_trips() {
        let bail = format_all_foreign_bail(6868, "unknown process", (6869, 6890));
        assert!(port_conflict::is_port_conflict(&bail));
        let (cmd, pid) = port_conflict::parse_occupant(&bail);
        assert!(cmd.is_none(), "got cmd: {cmd:?} from bail: {bail}");
        assert!(pid.is_none(), "got pid: {pid:?} from bail: {bail}");
    }

    /// The "stale headroom proxy holding the port" bail must NOT trigger
    /// the foreign-process port-conflict path — those are separate
    /// fingerprints in Sentry. Verifies the boundary stays intact.
    #[test]
    fn already_running_bail_is_not_classified_as_foreign_conflict() {
        let bail = format_already_running_bail(6868);
        assert!(
            !port_conflict::is_port_conflict(&bail),
            "stale-proxy bail must not match foreign-port classifier; got: {bail}"
        );
        // But the lib.rs port-conflict-failure classifier (which fingerprints
        // both shapes the same way) still catches it via its second condition.
        assert!(crate::is_port_conflict_failure(&bail));
    }

    #[test]
    fn wait_for_port_free_detects_release() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(
            !wait_for_port_free(port, Duration::from_millis(200)),
            "port held by a live listener must not report free"
        );
        drop(listener);
        assert!(
            wait_for_port_free(port, Duration::from_secs(2)),
            "port must report free shortly after the listener is dropped"
        );
    }

    /// Reproduces the upgrade-rollback scenario from the Sentry report: a
    /// *healthy* orphaned proxy (answers /readyz) squatting on the backend
    /// port. Normal launch (`force_unhealthy_too=false`) must leave it alone
    /// and bail; an upgrade boot validation (`true`) must reclaim it anyway so
    /// the new venv can bind. Ignored by default — spawns a child process,
    /// binds a port, and kills the process. Run locally:
    /// `cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored reclaim_orphan`
    #[test]
    #[ignore]
    fn reclaim_orphan_proxy_respects_upgrade_override() {
        let port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };

        // Stand-in for a live old-version proxy: answers 200 on every path so
        // `/readyz` reads healthy. argv is `python3 -c ...`, which deliberately
        // does NOT match `stop_headroom`'s pattern-kill — exactly the orphan
        // that survives into the spawn pre-flight.
        let script = r#"
import http.server, socketserver, sys
class S(socketserver.TCPServer):
    allow_reuse_address = True
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b'ok')
    def log_message(self, *a):
        pass
S(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
"#;

        // The trailing marker arg makes the stand-in pass the
        // pid_is_headroom_backend argv identity gate (needs both a "headroom"
        // and a "proxy" token), like a real orphan running as
        // `.../headroom proxy ...` from the app-support venv path would.
        let mut child = crate::proc::command("/usr/bin/python3")
            .arg("-c")
            .arg(script)
            .arg(port.to_string())
            .arg("--headroom-proxy-test-standin")
            .spawn()
            .expect("spawn stand-in proxy");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !probe_backend_readyz_ok(port) {
            assert!(
                std::time::Instant::now() < deadline,
                "stand-in proxy never became healthy on port {port}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        // Normal launch: a healthy occupant is left alone, reclaim bails.
        assert!(
            reclaim_orphan_proxy(port, false).is_err(),
            "force=false must bail on a healthy occupant"
        );
        assert!(
            probe_backend_readyz_ok(port),
            "force=false must NOT kill the healthy occupant"
        );

        // Upgrade validation: the healthy old proxy is reclaimed regardless.
        assert!(
            reclaim_orphan_proxy(port, true).is_ok(),
            "force=true must reclaim even a healthy occupant"
        );
        assert!(
            wait_for_port_free(port, Duration::from_secs(3)),
            "force=true must free the port"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn reclaim_orphan_proxy_never_kills_foreign_http_server() {
        let port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };

        // An unrelated local HTTP server (no headroom marker in argv) that
        // happens to hold the port: reclaim must bail — even with force — and
        // leave the process alive.
        let script = r#"
import http.server, socketserver, sys
class S(socketserver.TCPServer):
    allow_reuse_address = True
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b'ok')
    def log_message(self, *a):
        pass
S(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
"#;
        let mut child = crate::proc::command("/usr/bin/python3")
            .arg("-c")
            .arg(script)
            .arg(port.to_string())
            .spawn()
            .expect("spawn foreign http server");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !probe_backend_readyz_ok(port) {
            assert!(
                std::time::Instant::now() < deadline,
                "foreign server never came up on port {port}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        assert!(
            reclaim_orphan_proxy(port, true).is_err(),
            "reclaim must refuse to kill a non-headroom occupant"
        );
        assert!(
            probe_backend_readyz_ok(port),
            "foreign occupant must still be alive after reclaim bails"
        );
        assert!(matches!(
            diagnose_proxy_port(port),
            PortState::ForeignOccupant(_)
        ));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn probe_backend_readyz_ok_false_when_nothing_listening() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(
            !probe_backend_readyz_ok(port),
            "no server listening means /readyz is not healthy"
        );
    }

    #[test]
    fn extract_required_pydantic_core_version_pulls_pin_from_systemerror() {
        let log = "Traceback (most recent call last):\n  File \"<frozen runpy>\", line 189, in _run_module_as_main\n  ...\nSystemError: The installed pydantic-core version (2.46.3) is incompatible with the current pydantic version, which requires 2.41.5. If you encounter this error, make sure that you haven't upgraded pydantic-core manually.\n";
        assert_eq!(
            extract_required_pydantic_core_version(log),
            Some("2.41.5".into())
        );
    }

    #[test]
    fn extract_required_pydantic_core_version_returns_none_on_unrelated_traceback() {
        let log = "Traceback (most recent call last):\n  File \"x.py\", line 1, in <module>\nImportError: No module named 'foo'\n";
        assert!(extract_required_pydantic_core_version(log).is_none());
    }

    #[test]
    fn extract_required_pydantic_core_version_returns_none_when_marker_missing_version() {
        // Future-proof: if pydantic ever changes the message format and there's
        // no version after "which requires ", we must not return an empty pin.
        let log = "pydantic-core mismatch: which requires nothing useful here";
        assert!(extract_required_pydantic_core_version(log).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn managed_headroom_startup_uses_supported_proxy_args() {
        backend_port::reset_for_tests();
        let default_port = backend_port::DEFAULT_BACKEND_PORT.to_string();
        let entrypoint_args = headroom_entrypoint_startup_args(Some("0.28.0"), true);
        assert!(entrypoint_args.starts_with(&[
            "proxy".to_string(),
            "--port".to_string(),
            default_port.clone(),
            "--no-http2".to_string(),
            "--log-messages".to_string(),
        ]));
        assert!(entrypoint_args.contains(&"--learn".to_string()));
        assert!(entrypoint_args.contains(&"--no-memory-tools".to_string()));
        assert!(entrypoint_args.contains(&"--no-memory-context".to_string()));
        assert!(entrypoint_args.contains(&"--memory-db-path".to_string()));

        let python_args = headroom_python_startup_args();
        assert_eq!(
            python_args,
            vec![
                "-m".to_string(),
                "headroom.proxy.server".to_string(),
                "--port".to_string(),
                default_port,
                "--no-http2".to_string(),
                "--log-messages".to_string(),
            ]
        );
        // The python -m fallback must not pass learn flags; argparse on
        // headroom.proxy.server doesn't define them and would exit 2.
        assert!(!python_args.contains(&"--learn".to_string()));
        assert!(!python_args.contains(&"--no-memory-tools".to_string()));
        assert!(!python_args.contains(&"--no-memory-context".to_string()));
        assert!(!python_args.contains(&"--memory-db-path".to_string()));

        backend_port::reset_for_tests();
    }

    /// Regression (Sentry RUST-4A): the 0.26.0 fallback runtime's click
    /// entrypoint has no `--no-http2` option, so passing it made every boot
    /// validation attempt exit 2 and the upgrade time out. The flag must be
    /// gated on runtime >= 0.28.0; unknown versions assume the pinned runtime.
    #[test]
    #[serial_test::serial]
    fn entrypoint_args_gate_no_http2_on_runtime_version() {
        backend_port::reset_for_tests();

        assert!(!headroom_entrypoint_startup_args(Some("0.26.0"), true)
            .contains(&"--no-http2".to_string()));
        assert!(!headroom_entrypoint_startup_args(Some("0.27.0"), true)
            .contains(&"--no-http2".to_string()));
        assert!(headroom_entrypoint_startup_args(Some("0.28.0"), true)
            .contains(&"--no-http2".to_string()));
        assert!(headroom_entrypoint_startup_args(Some("1.0.0"), true)
            .contains(&"--no-http2".to_string()));
        // Unknown or malformed receipt version: assume pinned runtime.
        assert!(headroom_entrypoint_startup_args(None, true).contains(&"--no-http2".to_string()));
        assert!(headroom_entrypoint_startup_args(Some("garbage"), true)
            .contains(&"--no-http2".to_string()));
        // The python -m argparse variant has defined --no-http2 since 0.10.0
        // and must keep it unconditionally.
        assert!(headroom_python_startup_args().contains(&"--no-http2".to_string()));

        backend_port::reset_for_tests();
    }

    /// CCR tool injection forces every stream:true turn through the buffered
    /// non-stream path, whose keepalive wrapper commits HTTP 200 before the
    /// upstream outcome is known and then cannot report a real error — the
    /// client sees an empty/malformed 200 and cannot retry. The unified
    /// `--no-ccr` flag only exists from 0.31.0; on the 0.28.0 fallback runtime
    /// click would exit 2 and boot validation would fail like RUST-4A.
    #[test]
    #[serial_test::serial]
    fn entrypoint_args_gate_no_ccr_on_runtime_version() {
        backend_port::reset_for_tests();

        // 0.30.0 and earlier expose the split --no-ccr-marker /
        // --no-ccr-inject-tool pair, not the unified flag.
        assert!(!headroom_entrypoint_startup_args(Some("0.28.0"), true)
            .contains(&"--no-ccr".to_string()));
        assert!(!headroom_entrypoint_startup_args(Some("0.30.0"), true)
            .contains(&"--no-ccr".to_string()));
        assert!(headroom_entrypoint_startup_args(Some("0.31.0"), true)
            .contains(&"--no-ccr".to_string()));
        assert!(headroom_entrypoint_startup_args(Some("0.35.0"), true)
            .contains(&"--no-ccr".to_string()));
        // Unknown or malformed receipt version: assume pinned runtime.
        assert!(headroom_entrypoint_startup_args(None, true).contains(&"--no-ccr".to_string()));
        assert!(headroom_entrypoint_startup_args(Some("garbage"), true)
            .contains(&"--no-ccr".to_string()));
        // The `python -m headroom.proxy.server` argparse defines no CCR
        // option at all; passing it there would exit 2 on every fallback boot.
        assert!(!headroom_python_startup_args().contains(&"--no-ccr".to_string()));
        // Version-gated flags stay out of the staleness signature: a runtime
        // that cannot take the flag would otherwise never match and the
        // desktop would stop/restart the proxy on every check.
        assert!(!super::expected_proxy_arg_signature(true).contains(&"--no-ccr"));

        backend_port::reset_for_tests();
    }

    /// Regression (Sentry RUST-1M): the "coding" savings persona only exists in
    /// _PROFILES from 0.30.0. On the 0.28.0 fallback runtime, passing it makes
    /// the proxy raise on startup and exit before opening the port. Must gate on
    /// runtime >= 0.30.0; unknown versions assume the pinned (current) runtime.
    #[test]
    #[serial_test::serial]
    fn savings_profile_gated_on_runtime_version() {
        assert_eq!(savings_profile_for_runtime(Some("0.28.0")), "agent-90");
        assert_eq!(savings_profile_for_runtime(Some("0.29.9")), "agent-90");
        assert_eq!(savings_profile_for_runtime(Some("0.30.0")), "coding");
        assert_eq!(savings_profile_for_runtime(Some("1.0.0")), "coding");
        // Unknown or malformed receipt version: assume pinned runtime.
        assert_eq!(savings_profile_for_runtime(None), "coding");
        assert_eq!(savings_profile_for_runtime(Some("garbage")), "coding");
    }

    /// The cc-switch reconciler must stay off on any runtime without the
    /// Official-branch upstream reset (upstream PR #3166). Enabling it there
    /// leaves a captured third-party endpoint live process-wide after the user
    /// switches back to Claude Official, so Anthropic OAuth traffic goes to the
    /// old provider. Unlike the savings persona, an unreadable version fails
    /// CLOSED: the flag is an opt-in, and off is always the safe answer.
    #[test]
    #[serial_test::serial]
    fn cc_switch_reconcile_gated_on_runtime_version() {
        assert_eq!(cc_switch_reconcile_for_runtime(Some("0.35.0")), "0");
        assert_eq!(cc_switch_reconcile_for_runtime(Some("0.36.2")), "0");
        assert_eq!(cc_switch_reconcile_for_runtime(Some("0.36.3")), "1");
        assert_eq!(cc_switch_reconcile_for_runtime(Some("0.37.0")), "1");
        assert_eq!(cc_switch_reconcile_for_runtime(Some("1.0.0")), "1");
        // Unreadable version or the 0.28.0 boot-validation fallback: stay off.
        assert_eq!(cc_switch_reconcile_for_runtime(None), "0");
        assert_eq!(cc_switch_reconcile_for_runtime(Some("garbage")), "0");
        assert_eq!(cc_switch_reconcile_for_runtime(Some("0.36")), "0");
        // The currently pinned wheel is in the affected 0.36.3+ range.
        assert_eq!(
            cc_switch_reconcile_for_runtime(Some(HEADROOM_PINNED_VERSION)),
            "1"
        );
    }

    #[test]
    fn httpx_no_proxy_filter_drops_only_ipv6_cidrs() {
        let (value, dropped) = crate::tool_manager::httpx_compatible_no_proxy(
            "127.0.0.1,localhost,::1,10.0.0.0/8,fd00::/8,[fe80::]/10,.local",
        );

        assert_eq!(dropped, 2);
        assert_eq!(value, "127.0.0.1,localhost,::1,10.0.0.0/8,.local");
    }

    /// Regression: `start_headroom_background` previously built `startup_variants`
    /// before pre-flight ran, so when fallback called `backend_port::set(6869)`
    /// the variants still spawned with `--port 6868` and both failed with
    /// EADDRINUSE. The arg helpers read the atomic at call time, so as long as
    /// the helpers are invoked AFTER fallback has updated the atomic, the
    /// chosen fallback port flows through.
    #[test]
    #[serial_test::serial]
    fn startup_args_reflect_fallback_port_set_after_default() {
        backend_port::reset_for_tests();
        backend_port::set(6770);

        let entrypoint_args = headroom_entrypoint_startup_args(None, true);
        let port_idx = entrypoint_args
            .iter()
            .position(|a| a == "--port")
            .expect("entrypoint args contain --port");
        assert_eq!(entrypoint_args[port_idx + 1], "6770");

        let python_args = headroom_python_startup_args();
        let port_idx = python_args
            .iter()
            .position(|a| a == "--port")
            .expect("python args contain --port");
        assert_eq!(python_args[port_idx + 1], "6770");

        backend_port::reset_for_tests();
    }

    #[test]
    fn linux_bootstrap_requirements_skip_optional_memory_and_ml_packages() {
        let linux_requirements = bootstrap_requirements_lock_for_target("linux");

        assert!(linux_requirements.contains("ast-grep-cli=="));
        assert!(!linux_requirements.contains("hnswlib=="));
        assert!(linux_requirements.contains("opentelemetry-api=="));
        assert!(!linux_requirements.contains("torch=="));
        assert!(!linux_requirements.contains("sentence-transformers=="));
        assert!(linux_requirements.contains("mcp=="));
        assert!(linux_requirements.contains("onnxruntime=="));
        assert!(linux_requirements.contains("transformers=="));
    }

    #[test]
    fn parse_headroom_learn_timestamp_accepts_generated_date_lines() {
        let block = r#"
<!-- headroom:learn:start -->
## Headroom Learned Patterns
*Auto-generated by `headroom learn` on 2026-03-26 — do not edit manually*
- First pattern
<!-- headroom:learn:end -->
"#;

        let timestamp = super::parse_headroom_learn_timestamp(block).expect("timestamp");

        assert_eq!(timestamp.to_rfc3339(), "2026-03-26T00:00:00+00:00");
    }

    #[test]
    fn count_headroom_learn_patterns_counts_bullets_inside_block() {
        let block = r#"
<!-- headroom:learn:start -->
- First pattern
*Auto-generated by `headroom learn` on 2026-03-26 — do not edit manually*
- Second pattern
<!-- headroom:learn:end -->
"#;

        assert_eq!(super::count_headroom_learn_patterns(block), Some(2));
    }

    #[test]
    fn count_headroom_learn_patterns_returns_none_for_block_with_no_bullets() {
        let block = r#"
<!-- headroom:learn:start -->
*Auto-generated by `headroom learn` on 2026-03-26 — do not edit manually*
<!-- headroom:learn:end -->
"#;

        assert_eq!(super::count_headroom_learn_patterns(block), None);
    }

    #[test]
    fn count_headroom_learn_patterns_ignores_non_bullet_lines() {
        let block = r#"
<!-- headroom:learn:start -->
## Heading
Plain text without a dash
- Real pattern
<!-- headroom:learn:end -->
"#;

        assert_eq!(super::count_headroom_learn_patterns(block), Some(1));
    }

    #[test]
    fn parse_headroom_learn_timestamp_returns_none_when_no_timestamp_line() {
        let block = r#"
<!-- headroom:learn:start -->
- Some pattern
<!-- headroom:learn:end -->
"#;

        assert!(super::parse_headroom_learn_timestamp(block).is_none());
    }

    #[test]
    fn parse_headroom_learn_timestamp_accepts_rfc3339_datetime() {
        let block = r#"
<!-- headroom:learn:start -->
*Auto-generated by `headroom learn` on 2026-03-26T14:30:00Z — do not edit manually*
- Pattern
<!-- headroom:learn:end -->
"#;

        let timestamp = super::parse_headroom_learn_timestamp(block).expect("timestamp");

        assert_eq!(timestamp.to_rfc3339(), "2026-03-26T14:30:00+00:00");
    }

    #[test]
    fn parse_block_extracts_sections_and_bullets() {
        let content = r#"# Prior heading

<!-- headroom:learn:start -->
## Headroom Learned Patterns
*Auto-generated by `headroom learn` on 2026-04-22 — do not edit manually*

### Large Files
*~15,000 tokens/session saved*
- `src/App.tsx` is very large
- Also `lib.rs`

### Learned: environment
- Use uv run python
<!-- headroom:learn:end -->
"#;

        let sections = super::parse_headroom_learn_block(content);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "Large Files");
        assert_eq!(
            sections[0].bullets,
            vec!["`src/App.tsx` is very large", "Also `lib.rs`"]
        );
        assert_eq!(sections[1].title, "Learned: environment");
        assert_eq!(sections[1].bullets, vec!["Use uv run python"]);
    }

    #[test]
    fn parse_block_returns_empty_when_no_block_present() {
        let content = "Just some CLAUDE.md content without markers.\n- a bullet";
        assert!(super::parse_headroom_learn_block(content).is_empty());
    }

    #[test]
    fn delete_applied_bullet_removes_one_bullet() {
        let content = "\
before
<!-- headroom:learn:start -->
### Foo
- alpha
- beta
- gamma
<!-- headroom:learn:end -->
after
";
        let out = super::delete_applied_bullet(content, "Foo", "beta");
        assert!(out.contains("- alpha"));
        assert!(!out.contains("- beta"));
        assert!(out.contains("- gamma"));
        assert!(out.contains("### Foo"));
    }

    #[test]
    fn delete_applied_bullet_drops_section_when_last_bullet_removed() {
        let content = "\
<!-- headroom:learn:start -->
### Foo
- only
### Bar
- keep
<!-- headroom:learn:end -->
";
        let out = super::delete_applied_bullet(content, "Foo", "only");
        assert!(!out.contains("### Foo"));
        assert!(out.contains("### Bar"));
        assert!(out.contains("- keep"));
    }

    #[test]
    fn delete_applied_bullet_drops_last_section_and_keeps_end_marker() {
        // Regression: previously the final flush truncated the trailing
        // `<!-- headroom:learn:end -->` marker when the last section was
        // emptied, which left the block unparseable on the next read.
        let content = "\
<!-- headroom:learn:start -->
### Foo
- keep
### Bar
- removeme
<!-- headroom:learn:end -->
";
        let out = super::delete_applied_bullet(content, "Bar", "removeme");
        assert!(out.contains("### Foo"), "earlier section preserved");
        assert!(out.contains("- keep"), "earlier bullet preserved");
        assert!(!out.contains("### Bar"), "emptied last section dropped");
        assert!(!out.contains("- removeme"), "removed bullet absent");
        assert!(
            out.contains("<!-- headroom:learn:end -->"),
            "end marker preserved, got:\n{out}"
        );
        assert!(
            !super::parse_headroom_learn_block(&out).is_empty(),
            "block still parseable after deletion"
        );
    }

    #[test]
    fn delete_applied_bullet_removes_whole_block_when_empty() {
        let content = "prefix\n\n<!-- headroom:learn:start -->\n### Foo\n- only\n<!-- headroom:learn:end -->\n\nsuffix\n";
        let out = super::delete_applied_bullet(content, "Foo", "only");
        assert!(!out.contains("headroom:learn:start"));
        assert!(!out.contains("headroom:learn:end"));
        assert!(out.contains("prefix"));
        assert!(out.contains("suffix"));
    }

    #[test]
    fn delete_applied_bullet_is_noop_when_bullet_missing() {
        let content =
            "<!-- headroom:learn:start -->\n### Foo\n- alpha\n<!-- headroom:learn:end -->\n";
        let out = super::delete_applied_bullet(content, "Foo", "not-there");
        assert_eq!(out, content);
    }

    #[test]
    fn parse_headroom_learn_timestamp_returns_none_for_malformed_date() {
        let block = r#"
<!-- headroom:learn:start -->
*Auto-generated by `headroom learn` on not-a-date — do not edit manually*
- Pattern
<!-- headroom:learn:end -->
"#;

        assert!(super::parse_headroom_learn_timestamp(block).is_none());
    }

    #[test]
    fn encode_claude_project_folder_name_replaces_slashes_preserving_hyphens() {
        // Claude Code's on-disk encoding only substitutes '/' with '-'; literal
        // hyphens in the path are preserved verbatim. Verified against real
        // ~/.claude/projects/ folder names.
        assert_eq!(
            super::encode_claude_project_folder_name("/Users/alice/my-project"),
            "-Users-alice-my-project"
        );
    }

    #[test]
    fn encode_claude_project_folder_name_handles_root_slash() {
        assert_eq!(super::encode_claude_project_folder_name("/foo"), "-foo");
    }

    #[test]
    fn read_headroom_learn_metadata_from_path_falls_back_to_file_metadata() {
        let root = unique_temp_dir("headroom-learn-metadata");
        fs::create_dir_all(&root).expect("create root");
        let memory = root.join("MEMORY.md");
        fs::write(
            &memory,
            r#"
<!-- headroom:learn:start -->
- First pattern
- Second pattern
<!-- headroom:learn:end -->
"#,
        )
        .expect("write memory file");

        let metadata = read_headroom_learn_metadata_from_path(&memory).expect("metadata");

        assert_eq!(metadata.metadata.pattern_count, Some(2));
        assert!(metadata.metadata.learned_at.is_some());
        assert!(metadata.sort_key.is_some());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verify_sha256_file_accepts_matching_content_and_rejects_mismatches() {
        let root = unique_temp_dir("headroom-sha256");
        fs::create_dir_all(&root).expect("create root");
        let artifact = root.join("artifact.bin");
        fs::write(&artifact, b"headroom").expect("write artifact");

        let checksum = sha256_bytes(b"headroom");
        verify_sha256_file(&artifact, &checksum).expect("matching checksum");

        let err = verify_sha256_file(&artifact, "not-the-right-checksum")
            .expect_err("mismatched checksum should fail");
        assert!(err.to_string().contains("checksum mismatch"));

        let _ = fs::remove_dir_all(root);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }

    fn write_executable(path: &std::path::Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write script");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).expect("chmod");
        }
    }

    fn seed_test_runtime(prefix: &str) -> (PathBuf, ManagedRuntime, ToolManager) {
        let root = unique_temp_dir(prefix);
        let runtime = ManagedRuntime::bootstrap_root(&root);
        runtime.ensure_layout().expect("layout");
        fs::create_dir_all(&runtime.venv_dir).expect("venv dir");
        fs::write(runtime.venv_dir.join("marker"), b"live-v1").expect("marker");
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            br#"{"version":"0.0.1"}"#,
        )
        .expect("receipt");
        let manager = ToolManager::new(runtime.clone());
        (root, runtime, manager)
    }

    /// RUST-8E: a venv orphaned from its base interpreter keeps its redirector
    /// stub and READY flag, so the venv-only gate reported "installed" and the
    /// launch dead-ended in a Sentry start failure (exit 103, `No Python at`)
    /// instead of routing to setup, which re-downloads the runtime.
    #[test]
    fn python_runtime_installed_requires_the_standalone_base() {
        let (_root, runtime, manager) = seed_test_runtime("orphaned-venv-base");
        for marker in [runtime.ready_flag(), runtime.managed_python()] {
            fs::create_dir_all(marker.parent().expect("parent")).expect("mkdir");
            fs::write(&marker, b"").expect("marker");
        }
        assert!(
            !manager.python_runtime_installed(),
            "a venv whose standalone base is gone must not read as installed"
        );

        let base = runtime.standalone_python();
        fs::create_dir_all(base.parent().expect("parent")).expect("mkdir");
        fs::write(&base, b"").expect("base");
        assert!(
            manager.python_runtime_installed(),
            "all three markers present must read as installed"
        );
    }

    /// RUST-82: `python -m venv` runs ensurepip through `check_output` and
    /// re-raises only its exit status, so an install-blocking failure reached
    /// Sentry as a bare "returned non-zero exit status 1" with the cause
    /// discarded. Creating the venv without pip and running ensurepip ourselves
    /// is what venv does internally, but keeps ensurepip's stderr. If anyone
    /// folds these back into one step, that blind spot returns.
    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn create_managed_venv_runs_ensurepip_as_its_own_step() {
        let (root, runtime, manager) = seed_test_runtime("venv-ensurepip-split");
        let log = root.join("argv.log");
        let script = format!("#!/bin/sh\necho \"$@\" >> {}\nexit 0\n", log.display());
        write_executable(&runtime.standalone_python(), &script);
        write_executable(&runtime.managed_python(), &script);

        manager.create_managed_venv().expect("fake python succeeds");

        let calls = fs::read_to_string(&log).expect("argv log");
        let mut lines = calls.lines();
        assert_eq!(
            lines.next().map(str::to_string),
            Some(format!(
                "-m venv --without-pip {}",
                runtime.venv_dir.to_string_lossy()
            )),
            "venv is created without pip: {calls}"
        );
        assert_eq!(
            lines.next(),
            Some("-m ensurepip --upgrade --default-pip"),
            "ensurepip runs as its own command, so its stderr survives: {calls}"
        );
        assert_eq!(
            lines.next(),
            Some("-m pip --version"),
            "pip is still verified afterwards: {calls}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// `build_command` set plain "utf-8", silently overriding the
    /// `utf-8:backslashreplace` that `proc::command` applies on purpose -- which
    /// re-armed the RUST-7C UnicodeEncodeError kill for every python child
    /// built here (a lone surrogate in a log line must not be fatal).
    #[test]
    fn build_command_keeps_the_backslashreplace_stdio_encoding() {
        let cmd = build_command(Path::new("python3"), &["-V"], Path::new("."));
        let encoding = cmd
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("PYTHONIOENCODING"))
            .and_then(|(_, value)| value)
            .expect("PYTHONIOENCODING is set");
        assert_eq!(encoding, std::ffi::OsStr::new("utf-8:backslashreplace"));
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn managed_venv_has_pip_rejects_a_venv_whose_pip_is_missing() {
        // A venv interrupted during ensurepip keeps its interpreter but has no
        // pip. Bootstrap used to gate on the interpreter alone, so it skipped
        // venv creation forever and every install died with "No module named
        // pip" (RUST-66 / RUST-6M).
        let (_root, runtime, manager) = seed_test_runtime("venv-pip-probe");
        assert!(!manager.managed_venv_has_pip(), "no interpreter yet");

        write_executable(&runtime.managed_python(), "#!/bin/sh\nexit 1\n");
        assert!(
            !manager.managed_venv_has_pip(),
            "interpreter present but `-m pip --version` fails"
        );

        write_executable(&runtime.managed_python(), "#!/bin/sh\nexit 0\n");
        assert!(manager.managed_venv_has_pip(), "pip answers");
    }

    #[test]
    fn claude_plugin_install_args_omit_the_scope_flag() {
        // `--scope user` is Claude Code's default and older CLIs reject the
        // flag outright, failing the whole install (RUST-6K).
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|p| p.id == "caveman")
            .expect("caveman addon");
        assert_eq!(
            crate::tool_manager::PluginHost::ClaudeCode.install_args(plugin),
            vec!["plugin", "install", plugin.plugin_ref]
        );
    }

    #[test]
    fn codex_compat_marketplace_manifest_uses_local_plugin_paths() {
        let root = std::env::temp_dir().join(format!(
            "headroom-codex-marketplace-compat-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);

        for plugin in &PLUGIN_ADDONS {
            write_codex_compat_marketplace_manifest_at(&root, plugin)
                .expect("compatibility manifest");
            let path = root
                .join(plugin.marketplace_name)
                .join(".agents/plugins/marketplace.json");
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).expect("manifest bytes"))
                    .expect("valid JSON");
            assert_eq!(value["plugins"][0]["name"], plugin.id);
            assert_eq!(value["plugins"][0]["source"]["source"], "local");
            assert_eq!(
                value["plugins"][0]["source"]["path"],
                plugin.codex_local_path
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn allinluna_compat_marketplace_exposes_research_routes_and_preserves_adapter_choice() {
        let root = tempfile::tempdir().expect("tempdir");
        let plugin = super::PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");
        super::write_codex_compat_marketplace_manifest_at(root.path(), plugin)
            .expect("compatibility manifest");
        let path = root
            .path()
            .join(plugin.marketplace_name)
            .join(".agents/plugins/marketplace.json");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("manifest bytes")).expect("valid JSON");
        let plugins = value["plugins"].as_array().expect("plugin entries");
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0]["name"], "allinluna");
        assert_eq!(plugins[1]["name"], "research-routes");
        assert_eq!(plugins[1]["source"]["source"], "local");
        assert_eq!(plugins[1]["source"]["path"], "./plugins/research-routes");
    }

    #[test]
    fn codex_adapter_source_decision_is_safe_and_recoverable() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("allinluna");
        let marker = source.join(".headroom-local-community-adapter.json");
        assert_eq!(
            super::codex_adapter_source_decision(&source, &marker),
            super::CodexAdapterSourceDecision::Create
        );
        fs::create_dir_all(source.join(".git")).expect("git checkout");
        assert!(matches!(
            super::codex_adapter_source_decision(&source, &marker),
            super::CodexAdapterSourceDecision::MigrateGit { backup }
                if backup == root.path().join("allinluna.headroom-legacy")
        ));
        fs::write(&marker, b"{}").expect("ownership marker");
        assert_eq!(
            super::codex_adapter_source_decision(&source, &marker),
            super::CodexAdapterSourceDecision::ReuseManaged
        );
        fs::remove_dir_all(source.join(".git")).expect("remove git marker");
        assert_eq!(
            super::codex_adapter_source_decision(&source, &marker),
            super::CodexAdapterSourceDecision::Refuse
        );
    }

    #[test]
    fn codex_adapter_marker_requires_matching_plugin_identity() {
        let root = tempfile::tempdir().expect("marker root");
        let plugin = super::PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");
        let marker_root = root.path().join(plugin.marketplace_name);
        fs::create_dir_all(&marker_root).expect("marker directory");
        let marker = marker_root.join(".headroom-local-community-adapter.json");

        fs::write(
            &marker,
            serde_json::to_vec(&json!({
                "id": "another-plugin",
                "source": plugin.source_url
            }))
            .expect("serialize forged marker"),
        )
        .expect("write forged marker");
        let err = super::codex_adapter_marker_is_owned(&marker_root, plugin)
            .expect_err("forged marker must be rejected");
        assert!(format!("{err:#}").contains("does not match"));

        fs::write(
            &marker,
            serde_json::to_vec(&json!({
                "id": plugin.id,
                "source": plugin.source_url
            }))
            .expect("serialize valid marker"),
        )
        .expect("write valid marker");
        assert!(
            super::codex_adapter_marker_is_owned(&marker_root, plugin).expect("read valid marker")
        );
    }

    #[test]
    fn cleanup_fresh_codex_adapter_checkout_requires_owned_marker() {
        let root = tempfile::tempdir().expect("marker root");
        let plugin = super::PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");
        let checkout = root.path().join(plugin.marketplace_name);
        fs::create_dir_all(&checkout).expect("checkout directory");
        let marker = checkout.join(".headroom-local-community-adapter.json");

        fs::write(
            &marker,
            serde_json::to_vec(&json!({
                "id": "another-plugin",
                "source": plugin.source_url
            }))
            .expect("serialize forged marker"),
        )
        .expect("write forged marker");
        super::cleanup_fresh_codex_adapter_checkout(&checkout, plugin)
            .expect("forged marker must leave checkout untouched");
        assert!(checkout.exists());

        fs::write(
            &marker,
            serde_json::to_vec(&json!({
                "id": plugin.id,
                "source": plugin.source_url
            }))
            .expect("serialize owned marker"),
        )
        .expect("write owned marker");
        super::cleanup_fresh_codex_adapter_checkout(&checkout, plugin)
            .expect("owned checkout cleanup");
        assert!(!checkout.exists());
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)]
    fn codex_marketplace_migration_requires_matching_git_remote() {
        let (root, _runtime, _manager) = seed_test_runtime("plugin-migration-remote");
        let _home = HomeGuard::new(&root);
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");
        let source = crate::client_adapters::codex_home()
            .join(".tmp/marketplaces")
            .join(plugin.marketplace_name);
        fs::create_dir_all(&source).expect("marketplace directory");
        let source_text = source.to_string_lossy().into_owned();
        let init = crate::proc::command("git")
            .args(["init", "--quiet", &source_text])
            .output()
            .expect("git init");
        assert!(init.status.success(), "git init failed: {init:?}");
        let remote = crate::proc::command("git")
            .args([
                "-C",
                &source_text,
                "remote",
                "add",
                "origin",
                "https://github.com/another-owner/another-marketplace.git",
            ])
            .output()
            .expect("git remote add");
        assert!(remote.status.success(), "git remote add failed: {remote:?}");

        let registration = super::CodexMarketplaceRegistration::Git(plugin.source_url.into());
        let err = super::migrate_unmanaged_codex_marketplace(plugin, &registration)
            .expect_err("mismatched remote must not be moved");
        assert!(format!("{err:#}").contains("does not match"));
        assert!(source.exists(), "mismatched checkout must remain in place");
        assert!(
            !source.with_file_name("allinluna.headroom-legacy").exists(),
            "mismatched checkout must not create a recovery backup"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn codex_marketplace_backup_restore_moves_original_back() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("allinluna");
        let backup = root.path().join("allinluna.headroom-legacy");
        fs::create_dir_all(&destination).expect("replacement marketplace");
        fs::write(destination.join("source"), b"replacement").expect("replacement marker");
        fs::create_dir_all(&backup).expect("marketplace backup");
        fs::create_dir_all(backup.join(".git")).expect("original git marker");
        fs::write(backup.join("source"), b"original").expect("original marker");

        super::restore_codex_marketplace_backup(&backup, &destination)
            .expect("restore marketplace backup");
        assert_eq!(
            fs::read(destination.join("source")).expect("restored marker"),
            b"original"
        );
        assert!(!backup.exists());
    }

    #[test]
    fn codex_marketplace_backup_restore_rejects_missing_source_and_destination() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("allinluna");
        let backup = root.path().join("allinluna.headroom-legacy");

        let err = super::restore_codex_marketplace_backup(&backup, &destination)
            .expect_err("missing backup must not be reported as restored");
        assert!(format!("{err:#}").contains("is missing"));
    }

    #[test]
    fn codex_marketplace_backup_restore_rejects_managed_replacement_without_backup() {
        let root = tempfile::tempdir().expect("tempdir");
        let destination = root.path().join("allinluna");
        let backup = root.path().join("allinluna.headroom-legacy");
        fs::create_dir_all(destination.join(".git")).expect("replacement git marker");
        fs::write(
            destination.join(".headroom-local-community-adapter.json"),
            b"{}",
        )
        .expect("replacement ownership marker");

        super::restore_codex_marketplace_backup(&backup, &destination)
            .expect_err("managed replacement must not be mistaken for restored backup");
    }

    #[test]
    fn codex_marketplace_recovery_error_keeps_primary_and_recovery_failures() {
        let err = super::merge_codex_marketplace_recovery_error(
            anyhow::anyhow!("install failed"),
            Err(anyhow::anyhow!("restore failed")),
        );
        let message = format!("{err:#}");
        assert!(message.contains("install failed"));
        assert!(message.contains("restore failed"));
    }

    #[test]
    fn codex_marketplace_list_requires_local_source_type() {
        let local = json!({
            "marketplaces": [{"name": "allinluna", "sourceType": "local"}]
        });
        let git = json!({
            "marketplaces": [{"name": "allinluna", "sourceType": "git"}]
        });
        assert!(super::json_marketplace_is_local(&local, "allinluna"));
        assert!(!super::json_marketplace_is_local(&git, "allinluna"));
        assert!(!super::json_marketplace_is_local(&local, "other"));
    }

    #[test]
    fn codex_marketplace_registration_reads_nested_source_and_path() {
        let local = json!({
            "marketplaces": [{
                "name": "allinluna",
                "root": "/tmp/adapter",
                "marketplaceSource": {
                    "sourceType": "local",
                    "source": "/tmp/adapter"
                }
            }]
        });
        let git = json!({
            "marketplaces": [{
                "name": "allinluna",
                "marketplaceSource": {
                    "sourceType": "git",
                    "source": "https://github.com/zenx0x/allinluna.git"
                }
            }]
        });

        assert_eq!(
            super::json_marketplace_registration(&local, "allinluna"),
            Some(super::CodexMarketplaceRegistration::Local(PathBuf::from(
                "/tmp/adapter"
            )))
        );
        assert_eq!(
            super::json_marketplace_registration(&git, "allinluna"),
            Some(super::CodexMarketplaceRegistration::Git(
                "https://github.com/zenx0x/allinluna.git".into()
            ))
        );
        assert_eq!(
            super::json_marketplace_registration(&local, "missing"),
            None
        );
    }

    #[test]
    fn codex_marketplace_list_schema_drift_is_not_treated_as_empty() {
        assert!(super::json_marketplace_list_shape_is_supported(&json!({
            "marketplaces": []
        })));
        assert!(super::json_marketplace_list_shape_is_supported(&json!([
            {"name": "other", "sourceType": "git"}
        ])));
        assert!(!super::json_marketplace_list_shape_is_supported(&json!({
            "marketplaces": {}
        })));
        assert!(!super::json_marketplace_list_shape_is_supported(&json!({
            "unexpected": "schema"
        })));
        assert!(!super::json_marketplace_list_shape_is_supported(&json!(
            null
        )));
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn codex_marketplace_registration_resolves_node_for_gui_path() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("tempdir");
        let node = root.path().join("node");
        fs::write(&node, "#!/bin/sh\nprintf '%s\\n' '{\"marketplaces\":[]}'\n").expect("fake node");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).expect("node executable");

        let cli = root.path().join("codex");
        fs::write(&cli, "#!/usr/bin/env node\n").expect("fake codex");
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).expect("codex executable");

        let previous_path = std::env::var_os("PATH");
        std::env::set_var("PATH", "/usr/bin:/bin");
        let result = super::codex_marketplace_registration(&cli, root.path(), "openspec");
        match previous_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }

        assert!(
            result.is_ok(),
            "GUI-safe PATH should resolve node: {result:?}"
        );
    }

    #[test]
    fn managed_tool_metadata_and_native_plugin_registry_are_stable() {
        use std::collections::HashSet;

        let (_root, _runtime, manager) = seed_test_runtime("managed-tool-metadata");
        let tools = manager.list_tools();
        let ids: HashSet<_> = tools.iter().map(|tool| tool.id.as_str()).collect();
        assert_eq!(ids.len(), tools.len());

        for (id, category, activation_scope, workflow_group) in [
            ("headroom", "core", "immediate", None),
            ("rtk", "efficiency", "immediate", None),
            ("markitdown", "documents", "immediate", None),
            ("serena", "code_intelligence", "client_restart", None),
            (
                "codebase-memory",
                "code_intelligence",
                "client_restart",
                None,
            ),
            ("context7", "code_intelligence", "client_restart", None),
            ("ponytail", "efficiency", "new_session", None),
            ("caveman", "efficiency", "new_session", None),
            (
                "allinluna",
                "automation",
                "new_session",
                Some("execution_engine"),
            ),
            ("stop-that-shit", "guardrails", "new_session", None),
            ("agent-guard", "guardrails", "new_session", None),
            ("grill-me", "learning", "new_session", None),
        ] {
            let tool = listed_tool(&manager, id);
            assert_eq!(tool.category, category, "category for {id}");
            assert_eq!(tool.activation_scope, activation_scope, "scope for {id}");
            assert_eq!(
                tool.workflow_group.as_deref(),
                workflow_group,
                "workflow for {id}"
            );
        }

        for (id, marketplace, marketplace_name, plugin_ref, local_path, source_url) in [
            (
                "stop-that-shit",
                "lennney/stop-that-shit",
                "stop-that-shit",
                "stop-that-shit@stop-that-shit",
                ".",
                "https://github.com/lennney/stop-that-shit",
            ),
            (
                "agent-guard",
                "JeongJaeSoon/agent-guard",
                "agent-guard",
                "agent-guard@agent-guard",
                "./plugins/agent-guard",
                "https://github.com/JeongJaeSoon/agent-guard",
            ),
            (
                "grill-me",
                "joshuawheelock/grill-me",
                "grill-me",
                "grill-me@grill-me",
                ".",
                "https://github.com/joshuawheelock/grill-me",
            ),
        ] {
            let plugin = PLUGIN_ADDONS
                .iter()
                .find(|plugin| plugin.id == id)
                .expect("registered plugin");
            assert_eq!(plugin.marketplace, marketplace);
            assert_eq!(plugin.marketplace_name, marketplace_name);
            assert_eq!(plugin.plugin_ref, plugin_ref);
            assert_eq!(plugin.codex_local_path, local_path);
            assert_eq!(plugin.source_url, source_url);
            assert_eq!(plugin.hosts, &[PluginHost::ClaudeCode, PluginHost::Codex]);
        }
    }

    #[test]
    fn allinluna_launcher_uses_managed_python_and_marketplace_runtime() {
        let (_root, runtime, manager) = seed_test_runtime("allinluna-launcher");
        manager
            .ensure_allinluna_launcher()
            .expect("write All in Luna launcher");

        let launcher = manager.allinluna_entrypoint();
        let body = fs::read_to_string(&launcher).expect("launcher contents");
        assert!(body.contains(&runtime.managed_python().display().to_string()));
        if cfg!(target_os = "windows") {
            assert!(body.contains(r".codex\.tmp\marketplaces\allinluna"));
            assert!(body.contains(r".claude\plugins\marketplaces\allinluna"));
        } else {
            assert!(body.contains("CODEX_HOME"));
            assert!(body.contains("codex_root"));
            assert!(body.contains(".tmp/marketplaces/allinluna"));
            assert!(body.contains(".claude/plugins/marketplaces/allinluna"));
        }
        let required_python = if cfg!(target_os = "windows") {
            "Headroom-managed Python ^>= 3.11"
        } else {
            "Headroom-managed Python >= 3.11"
        };
        assert!(body.contains(required_python));
        assert!(body.contains("allinluna/runtime"));
        assert!(body.contains("allinluna_runtime/__main__.py"));
        assert!(body.contains("PYTHONPATH"));
        assert!(body.contains("PYTHONNOUSERSITE=1"));
        assert!(body.contains("-m allinluna_runtime"));
        assert!(!body.contains("allinluna_runtime.py"));
        assert!(!body.contains("/usr/bin/python3"));
    }

    #[test]
    fn tool_enabled_reads_receipt_flag_and_defaults_true() {
        let (_root, runtime, manager) = seed_test_runtime("tool-enabled");
        // No receipt -> default enabled.
        assert!(manager.tool_enabled("markitdown"));

        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            br#"{"version":"0.1.6","enabled":false}"#,
        )
        .expect("receipt");
        assert!(!manager.tool_enabled("markitdown"));

        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            br#"{"version":"0.1.6","enabled":true}"#,
        )
        .expect("receipt");
        assert!(manager.tool_enabled("markitdown"));
    }

    /// Every addon keeps its card on every platform; the ones with no artifact
    /// for this target carry a reason instead (codebase-memory ships no Windows
    /// binary, rtk none for windows-aarch64). A card without an artifact and
    /// without a reason would offer an Install button that only ever errors.
    #[test]
    fn addons_without_an_artifact_for_this_target_carry_a_reason() {
        let (_root, _runtime, manager) = seed_test_runtime("manifest-artifacts");
        let tools = manager.list_tools();
        let reason_for = |id: &str| {
            tools
                .iter()
                .find(|tool| tool.id == id)
                .unwrap_or_else(|| panic!("{id} card must exist on every platform"))
                .unavailable_reason
                .clone()
        };

        assert_eq!(
            reason_for("rtk").is_none(),
            rtk_distribution_artifact().is_ok(),
            "rtk is installable exactly when its artifact resolves"
        );
        assert_eq!(
            reason_for("codebase-memory").is_none(),
            codebase_memory_distribution_artifact().is_ok(),
            "codebase-memory is installable exactly when its artifact resolves"
        );
        // Nothing platform-gated about the Python addons: never gray them.
        assert!(reason_for("markitdown").is_none());
        assert!(reason_for("serena").is_none());
    }

    #[test]
    fn unavailable_reason_names_the_platform_and_the_reason() {
        // The message is user-facing copy on a grayed card, so it has to say
        // both which platform is missing and why, without a bare target triple.
        let Some(reason) = addon_unavailable_reason("codebase-memory") else {
            return; // this target has a build; nothing to phrase
        };
        assert!(reason.starts_with("Not available on "), "{reason}");
        assert!(reason.contains("codebase-memory"), "{reason}");
    }

    #[test]
    fn addon_mode_specs_match_the_real_plugin_tiers() {
        let ponytail = super::addon_mode_spec("ponytail").expect("ponytail mode spec");
        assert_eq!(ponytail.supported_modes, ["lite", "full", "ultra"]);

        let caveman = super::addon_mode_spec("caveman").expect("caveman mode spec");
        assert_eq!(
            caveman.supported_modes,
            [
                "lite",
                "full",
                "ultra",
                "wenyan-lite",
                "wenyan-full",
                "wenyan-ultra",
            ]
        );
        assert_eq!(
            super::canonical_addon_mode(ponytail, " ULTRA "),
            Some("ultra")
        );
        assert_eq!(
            super::canonical_addon_mode_from_config(caveman, " Wenyan "),
            Some("wenyan-full")
        );
        assert_eq!(super::canonical_addon_mode(ponytail, "wenyan"), None);
        assert_eq!(super::canonical_addon_mode(caveman, "wenyan"), None);
        assert_eq!(super::canonical_addon_mode(caveman, "off"), None);
    }

    #[test]
    fn addon_mode_paths_follow_plugin_user_config_rules() {
        let root = tempfile::tempdir().expect("tempdir");
        let xdg = root.path().join("xdg");
        let app_data = root.path().join("app-data");
        let home = root.path().join("home");

        assert_eq!(
            super::addon_user_config_path_from(
                "ponytail",
                Some(&xdg),
                Some(&app_data),
                Some(&home),
                false,
            )
            .expect("XDG path"),
            xdg.join("ponytail").join("config.json")
        );
        assert_eq!(
            super::addon_user_config_path_from(
                "caveman",
                None,
                Some(&app_data),
                Some(&home),
                true,
            )
            .expect("Windows APPDATA path"),
            app_data.join("caveman").join("config.json")
        );
        assert_eq!(
            super::addon_user_config_path_from("caveman", None, None, Some(&home), true)
                .expect("Windows home fallback"),
            home.join("AppData")
                .join("Roaming")
                .join("caveman")
                .join("config.json")
        );
        assert_eq!(
            super::addon_user_config_path_from("ponytail", None, None, Some(&home), false)
                .expect("Unix home path"),
            home.join(".config").join("ponytail").join("config.json")
        );

        let system_home = root.path().join("system-home");
        for home_from_env in [None, Some(PathBuf::new())] {
            let resolved_home =
                super::addon_home_from_environment(home_from_env, Some(system_home.clone()))
                    .expect("system home fallback");
            assert_eq!(
                super::addon_user_config_path_from(
                    "ponytail",
                    None,
                    None,
                    Some(&resolved_home),
                    false,
                )
                .expect("fallback Unix path"),
                system_home
                    .join(".config")
                    .join("ponytail")
                    .join("config.json")
            );
        }
    }

    #[test]
    fn addon_mode_config_read_write_is_normalized_and_preserves_fields() {
        let root = tempfile::tempdir().expect("tempdir");
        let ponytail = super::addon_mode_spec("ponytail").expect("ponytail mode spec");
        let ponytail_config = root.path().join("ponytail").join("config.json");
        fs::create_dir_all(ponytail_config.parent().expect("parent")).expect("create parent");
        fs::write(
            &ponytail_config,
            b"\xEF\xBB\xBF{\"defaultMode\":\" ULTRA \",\"other\":{\"kept\":true}}",
        )
        .expect("write Ponytail config with BOM");

        assert_eq!(
            super::read_addon_default_mode_from_path(&ponytail_config, ponytail)
                .expect("read Ponytail config"),
            Some("ultra".to_string())
        );
        super::write_addon_default_mode_to_path(&ponytail_config, ponytail, " lite ")
            .expect("write Ponytail mode");
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&ponytail_config).expect("read saved config"))
                .expect("saved JSON");
        assert_eq!(
            saved.get("defaultMode").and_then(serde_json::Value::as_str),
            Some("lite")
        );
        assert_eq!(saved["other"], serde_json::json!({ "kept": true }));

        let caveman = super::addon_mode_spec("caveman").expect("caveman mode spec");
        let caveman_config = root.path().join("caveman").join("config.json");
        fs::create_dir_all(caveman_config.parent().expect("parent")).expect("create parent");
        fs::write(&caveman_config, b"{\"defaultMode\":\" WENYAN \"}")
            .expect("write legacy Caveman config");
        assert_eq!(
            super::read_addon_default_mode_from_path(&caveman_config, caveman)
                .expect("read Caveman config"),
            Some("wenyan-full".to_string())
        );
    }

    #[test]
    fn addon_mode_write_never_overwrites_invalid_or_non_object_config() {
        let root = tempfile::tempdir().expect("tempdir");
        let ponytail = super::addon_mode_spec("ponytail").expect("ponytail mode spec");
        let invalid = root.path().join("invalid.json");
        fs::write(&invalid, b"{not valid json").expect("write invalid config");
        let original_invalid = fs::read(&invalid).expect("read invalid config");
        let err = super::write_addon_default_mode_to_path(&invalid, ponytail, "lite")
            .expect_err("invalid JSON must not be overwritten");
        assert!(err.to_string().contains(&invalid.display().to_string()));
        assert_eq!(
            fs::read(&invalid).expect("read invalid config"),
            original_invalid
        );
        assert_eq!(
            super::addon_default_mode_or_full(&invalid, ponytail),
            "full"
        );

        let non_object = root.path().join("array.json");
        fs::write(&non_object, b"[]").expect("write array config");
        let original_non_object = fs::read(&non_object).expect("read array config");
        let err = super::write_addon_default_mode_to_path(&non_object, ponytail, "full")
            .expect_err("non-object JSON must not be overwritten");
        assert!(err.to_string().contains(&non_object.display().to_string()));
        assert_eq!(
            fs::read(&non_object).expect("read array config"),
            original_non_object
        );

        let absent = root.path().join("new").join("config.json");
        let err = super::write_addon_default_mode_to_path(&absent, ponytail, "wenyan")
            .expect_err("unsupported Ponytail mode must be rejected");
        assert!(err
            .to_string()
            .contains("unsupported ponytail default mode"));
        assert!(
            !absent.exists(),
            "invalid mode must not create a config file"
        );

        let caveman = super::addon_mode_spec("caveman").expect("caveman mode spec");
        let legacy_write = root.path().join("caveman").join("config.json");
        let err = super::write_addon_default_mode_to_path(&legacy_write, caveman, "wenyan")
            .expect_err("legacy Caveman alias must only be accepted while reading");
        assert!(err.to_string().contains("unsupported caveman default mode"));
        assert!(
            !legacy_write.exists(),
            "legacy alias must not create a config file"
        );
    }

    fn listed_tool(manager: &ToolManager, id: &str) -> ManagedTool {
        manager
            .list_tools()
            .into_iter()
            .find(|tool| tool.id == id)
            .expect("tool in manifest")
    }

    #[test]
    fn list_tools_reports_the_installed_version_and_offers_the_pinned_one() {
        let (_root, runtime, manager) = seed_test_runtime("addon-update");

        // Not installed: the card shows what an install would give you.
        let absent = listed_tool(&manager, "markitdown");
        assert_eq!(absent.version, MARKITDOWN_PINNED_VERSION);
        assert!(!absent.update_available);
        assert!(absent.available_version.is_none());

        let entrypoint = manager.markitdown_entrypoint();
        fs::create_dir_all(entrypoint.parent().expect("bin parent")).expect("bin dir");
        fs::write(&entrypoint, b"#!/bin/sh\n").expect("entrypoint");
        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            br#"{"version":"0.1.5","enabled":true}"#,
        )
        .expect("receipt");

        // Installed and behind: report what is on disk, offer the pin.
        let stale = listed_tool(&manager, "markitdown");
        assert_eq!(stale.version, "0.1.5");
        assert!(stale.update_available);
        assert_eq!(
            stale.available_version.as_deref(),
            Some(MARKITDOWN_PINNED_VERSION)
        );

        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            format!(r#"{{"version":"{MARKITDOWN_PINNED_VERSION}","enabled":true}}"#).as_bytes(),
        )
        .expect("receipt");
        let current = listed_tool(&manager, "markitdown");
        assert_eq!(current.version, MARKITDOWN_PINNED_VERSION);
        assert!(!current.update_available);

        // The pin is a minimum: someone ahead of it is current, not overdue.
        // Prompting here would be an offer to downgrade.
        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            br#"{"version":"99.0.0","enabled":true}"#,
        )
        .expect("receipt");
        let ahead = listed_tool(&manager, "markitdown");
        assert_eq!(ahead.version, "99.0.0");
        assert!(!ahead.update_available);

        // An unreadable version is treated as current for the same reason.
        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            br#"{"version":"main","enabled":true}"#,
        )
        .expect("receipt");
        assert!(!listed_tool(&manager, "markitdown").update_available);

        // Disabled and behind: no Update button. Every installer writes
        // `enabled: true`, so updating here would switch the addon back on
        // behind the user's back.
        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            br#"{"version":"0.1.5","enabled":false}"#,
        )
        .expect("receipt");
        let disabled = listed_tool(&manager, "markitdown");
        assert_eq!(disabled.version, "0.1.5");
        assert!(!disabled.update_available);

        // A receipt whose payload is gone is not an install: no version claim,
        // no update prompt.
        fs::remove_file(&entrypoint).expect("remove entrypoint");
        let orphaned = listed_tool(&manager, "markitdown");
        assert_eq!(orphaned.version, MARKITDOWN_PINNED_VERSION);
        assert!(!orphaned.update_available);
    }

    #[test]
    fn pending_addon_update_treats_the_pin_as_a_minimum() {
        assert_eq!(
            pending_addon_update("serena", Some("1.6.1"), "1.7.0").as_deref(),
            Some("1.7.0")
        );
        assert_eq!(pending_addon_update("serena", Some("1.7.0"), "1.7.0"), None);
        // Ahead of the pin: current, not overdue. Never offer a downgrade.
        assert_eq!(pending_addon_update("serena", Some("1.8.0"), "1.7.0"), None);
        assert_eq!(pending_addon_update("serena", Some("2.0.0"), "1.7.0"), None);
        assert_eq!(pending_addon_update("serena", None, "1.7.0"), None);
        assert_eq!(
            pending_addon_update("serena", Some("nightly"), "1.7.0"),
            None
        );

        // Plugin updates require a confirmed upstream version; local state alone
        // must not create an optimistic Update button.
        assert_eq!(
            pending_addon_update("ponytail", Some("4.7.0"), PLUGIN_DISPLAY_VERSION),
            None
        );
        assert_eq!(
            pending_addon_update("caveman", None, PLUGIN_DISPLAY_VERSION),
            None
        );

        // Both runtime-managed tools stay manually actionable when stale.
        assert_eq!(
            pending_addon_update("rtk", Some("0.1.0"), RTK_VERSION).as_deref(),
            Some(RTK_VERSION)
        );
        assert_eq!(
            pending_addon_update("headroom", Some("0.1.0"), HEADROOM_PINNED_VERSION),
            Some(HEADROOM_PINNED_VERSION.to_string())
        );
    }

    #[test]
    fn stale_rtk_is_both_launch_refreshable_and_manually_updatable() {
        let (_root, runtime, manager) = seed_test_runtime("addon-update-rtk");
        fs::create_dir_all(&runtime.bin_dir).expect("bin dir");
        fs::write(manager.rtk_entrypoint(), b"#!/bin/sh\n").expect("entrypoint");
        fs::write(
            runtime.tools_dir.join("rtk.json"),
            br#"{"version":"0.1.0","enabled":true}"#,
        )
        .expect("receipt");

        let rtk = listed_tool(&manager, "rtk");
        assert_eq!(rtk.version, "0.1.0");
        assert!(manager.rtk_needs_install());
        assert!(rtk.update_available);
        assert_eq!(rtk.available_version.as_deref(), Some(RTK_VERSION));
    }

    #[test]
    fn markitdown_conversion_count_reads_counter_file() {
        let (_root, runtime, manager) = seed_test_runtime("markitdown-count");
        assert_eq!(manager.markitdown_conversion_count(), None);

        fs::write(runtime.tools_dir.join("markitdown-conversions"), b"7\n").expect("counter");
        assert_eq!(manager.markitdown_conversion_count(), Some(7));

        fs::write(runtime.tools_dir.join("markitdown-conversions"), b"junk").expect("counter");
        assert_eq!(manager.markitdown_conversion_count(), None);

        fs::write(runtime.tools_dir.join("markitdown-conversions"), b"0").expect("counter");
        assert_eq!(manager.markitdown_conversion_count(), None);
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn markitdown_shim_counts_file_conversions_but_not_flag_calls() {
        let (_root, _runtime, manager) = seed_test_runtime("markitdown-shim");
        write_executable(
            &manager.markitdown_entrypoint(),
            "#!/bin/sh\necho converted:$1\n",
        );
        manager.ensure_markitdown_shim().expect("shim");

        let run = |arg: &str| {
            let out = crate::proc::command(manager.markitdown_shim_path())
                .arg(arg)
                .output()
                .expect("run shim");
            assert!(out.status.success(), "shim exited non-zero for {arg}");
            String::from_utf8_lossy(&out.stdout).to_string()
        };

        assert!(run("/tmp/a.docx").contains("converted:/tmp/a.docx"));
        run("/tmp/b.xlsx");
        run("--help"); // flag-only invocation must not count
        assert_eq!(manager.markitdown_conversion_count(), Some(2));
    }

    #[test]
    fn count_serena_tool_calls_counts_marker_lines_in_txt_logs_only() {
        let dir = unique_temp_dir("serena-log-count");
        assert_eq!(
            super::count_serena_tool_calls_in_dir(&dir),
            None,
            "missing dir"
        );

        fs::create_dir_all(&dir).expect("log dir");
        fs::write(
            dir.join("mcp_20260723_1.txt"),
            "INFO find_symbol: {\"name\": \"foo\"}; session_id: abc\n\
             INFO Result: 42 lines\n\
             INFO read_file: {\"path\": \"x.rs\"}; session_id: abc\n",
        )
        .expect("log file");
        fs::write(dir.join("ignored.log"), "x: {}; session_id: zzz\n").expect("non-txt");
        assert_eq!(super::count_serena_tool_calls_in_dir(&dir), Some(2));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn serena_savings_label_combines_calls_live_tokens_and_timing() {
        assert_eq!(super::serena_savings_label(None, None), None);
        assert_eq!(
            super::serena_savings_label(Some(1), None).as_deref(),
            Some("1 tool call today")
        );
        assert_eq!(
            super::serena_savings_label(None, Some((500, None))).as_deref(),
            Some("~500 tokens returned this session")
        );
        let age = std::time::Duration::from_secs(2 * 3600 + 14 * 60 + 5);
        assert_eq!(
            super::serena_savings_label(Some(231), Some((48_200, Some(age)))).as_deref(),
            Some("231 tool calls today, ~48k tokens returned in 2h 14m")
        );
        assert_eq!(super::compact_token_count(1_234_000), "1.2M");
    }

    #[test]
    fn parse_ps_etime_handles_all_ps_formats() {
        use std::time::Duration;
        assert_eq!(
            super::parse_ps_etime("05:33"),
            Some(Duration::from_secs(333))
        );
        assert_eq!(
            super::parse_ps_etime(" 02:14:33"),
            Some(Duration::from_secs(2 * 3600 + 14 * 60 + 33))
        );
        assert_eq!(
            super::parse_ps_etime("3-01:02:03"),
            Some(Duration::from_secs(3 * 86400 + 3600 + 2 * 60 + 3))
        );
        assert_eq!(super::parse_ps_etime("garbage"), None);
    }

    #[test]
    fn oldest_serena_session_age_matches_entrypoint_and_subcommand_only() {
        let marker = "/tmp/hr/serena-venv/bin/serena";
        let ps = "\
  01:00 /usr/bin/python other-tool start-mcp-server\n\
  45:00 /tmp/hr/serena-venv/bin/python /tmp/hr/serena-venv/bin/serena start-mcp-server --context claude-code\n\
2-00:00:00 /tmp/hr/serena-venv/bin/serena --help\n\
  02:14:33 /tmp/hr/serena-venv/bin/serena start-mcp-server --context codex\n";
        assert_eq!(
            super::oldest_serena_session_age(ps, marker),
            Some(std::time::Duration::from_secs(2 * 3600 + 14 * 60 + 33)),
            "picks oldest matching session; ignores other tools and non-server invocations"
        );
        assert_eq!(super::oldest_serena_session_age("", marker), None);
    }

    #[test]
    fn compact_duration_formats() {
        use std::time::Duration;
        assert_eq!(
            super::compact_duration(Duration::from_secs(30)),
            "under a minute"
        );
        assert_eq!(super::compact_duration(Duration::from_secs(47 * 60)), "47m");
        assert_eq!(super::compact_duration(Duration::from_secs(2 * 3600)), "2h");
        assert_eq!(
            super::compact_duration(Duration::from_secs(2 * 3600 + 14 * 60)),
            "2h 14m"
        );
    }

    #[test]
    #[serial_test::serial]
    fn serena_registration_migrates_only_ledger_proven_legacy_entries() {
        let home = tempfile::tempdir().expect("temp home");
        let _home = HomeGuard::new(home.path());
        assert_eq!(
            super::legacy_headroom_mcp_ledger_path(),
            home.path().join(".headroom/mcp_installs.json")
        );
        assert!(super::SERENA_MCP_HELPER
            .contains("legacy_ledger = Path(sys.argv[3]) if len(sys.argv) > 3 else None"));
        assert!(super::SERENA_MCP_HELPER.contains(
            "headroom_installed_matching(\n        registrar.name, current, path=legacy_ledger"
        ));
        let preflight_exit = super::SERENA_MCP_HELPER
            .find("    if failures:\n        sys.exit(\"; \".join(failures))")
            .expect("registration preflight exits on conflicts");
        // The call is wrapped (`registrar_call`) so an agent CLI that cannot
        // be executed is a per-agent skip instead of a fatal traceback.
        let first_registration = super::SERENA_MCP_HELPER
            .find("registrar_call(registrar, \"register\", registrar.register_server, spec)")
            .expect("registration runs after preflight");
        assert!(preflight_exit < first_registration);
        assert!(super::SERENA_MCP_HELPER.contains(
            "if headroom_owns_serena(registrar, current):\n                result = registrar_call(\n                    registrar, \"register\", registrar.register_server, spec, force=True\n                )"
        ));
        assert!(super::SERENA_MCP_HELPER.contains(
            "elif result.status == RegisterStatus.MISMATCH:\n            failures.append(f\"{registrar.name}: serena entry conflicts with user-managed configuration\")"
        ));
        assert!(super::SERENA_MCP_HELPER
            .contains("if not headroom_installed_matching(registrar.name, current):"));
    }

    #[test]
    fn context7_and_codebase_memory_migrate_legacy_entries_and_fail_on_mismatch() {
        for (helper, tool, legacy_arg) in [
            (super::CONTEXT7_MCP_HELPER, "context7", "sys.argv[3]"),
            (
                super::CODEBASE_MEMORY_MCP_HELPER,
                "codebase-memory",
                "sys.argv[4]",
            ),
        ] {
            assert!(helper.contains(&format!(
                "legacy_ledger = Path({legacy_arg}) if len(sys.argv) >"
            )));
            assert!(helper.contains("headroom_installed_matching("));
            assert!(helper.contains("path=legacy_ledger"));
            assert!(helper.contains(&format!(
                "{tool} entry conflicts with user-managed configuration"
            )));
            assert!(helper.contains("elif result.status == RegisterStatus.MISMATCH:"));
        }
    }

    #[test]
    fn addon_helpers_skip_an_agent_cli_that_cannot_be_executed() {
        // Reproduced on macOS: Homebrew left /opt/homebrew/bin/claude as a
        // shim whose native binary never downloaded, so exec() failed with
        // ENOEXEC. ClaudeRegistrar.detect() still saw a CLI on PATH, the
        // helper died with a traceback, and the whole addon install aborted
        // as "codebase-memory update failed; restored previous installation"
        // with no cause. One unusable agent must not block the others.
        for (helper, tool) in [
            (super::SERENA_MCP_HELPER, "serena"),
            (super::CONTEXT7_MCP_HELPER, "context7"),
            (super::CODEBASE_MEMORY_MCP_HELPER, "codebase-memory"),
        ] {
            assert!(
                helper.contains("def registrar_call(registrar, label, call, *args, **kwargs):"),
                "{tool}: the guard must exist"
            );
            assert!(
                helper.contains("except OSError as exc:"),
                "{tool}: an unrunnable CLI raises OSError, not a RegisterResult"
            );
            assert!(
                helper.contains("CLI is not runnable"),
                "{tool}: the skip must be reported, not swallowed"
            );
            // Every CLI-backed call goes through the guard. A bare call site
            // is exactly what crashed.
            assert!(
                !helper.contains("result = registrar.register_server(spec)\n"),
                "{tool}: register_server must be wrapped"
            );
            assert!(
                !helper.contains("result = registrar.register_server(spec, force=True)\n"),
                "{tool}: the forced re-register must be wrapped too"
            );
            assert!(
                !helper.contains(&format!("if registrar.unregister_server(\"{tool}\"):\n")),
                "{tool}: unregister must be wrapped too"
            );
            assert!(
                helper.contains(&format!("registrar.unregister_server, \"{tool}\"")),
                "{tool}: unregister must still target {tool}"
            );
        }
    }

    #[test]
    fn serena_dashboard_listener_parsers_find_late_loopback_ports() {
        let lsof = "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n\
python3 10 user 4u IPv4 0x1 0t0 TCP 127.0.0.1:24282 (LISTEN)\n\
python3 11 user 4u IPv6 0x2 0t0 TCP [::1]:24295 (LISTEN)\n\
python3 12 user 4u IPv4 0x3 0t0 TCP 192.168.1.2:24296 (LISTEN)\n\
python3 13 user 4u IPv4 0x4 0t0 TCP 127.0.0.1:24281 (LISTEN)\n\
python3 14 user 4u IPv4 0x5 0t0 TCP 127.0.0.1:24346 (LISTEN)\n";
        assert_eq!(super::parse_lsof_listener_ports(lsof), vec![24282, 24295]);

        let ss = "State Recv-Q Send-Q Local Address:Port Peer Address:Port\n\
LISTEN 0 128 127.0.0.1:24290 0.0.0.0:*\n\
LISTEN 0 128 *:24296 *:*\n\
ESTAB 0 0 127.0.0.1:24297 127.0.0.1:1\n";
        assert_eq!(super::parse_ss_listener_ports(ss), vec![24290, 24296]);

        let netstat = "TCP 127.0.0.1:24291 0.0.0.0:0 LISTENING 44\n\
TCP [::1]:24298 [::]:0 LISTENING 45\n\
TCP 127.0.0.1:24299 127.0.0.1:50000 ESTABLISHED 46\n";
        assert_eq!(
            super::parse_netstat_listener_ports(netstat),
            vec![24291, 24298]
        );
    }

    #[test]
    fn fetch_serena_output_tokens_sums_stats_across_tools() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let body = r#"{"stats":{"find_symbol":{"num_times_called":3,"input_tokens":10,"output_tokens":4500},"read_file":{"num_times_called":1,"input_tokens":5,"output_tokens":500}}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write");
        });

        let total = super::fetch_serena_output_tokens(&format!("http://127.0.0.1:{port}"));
        handle.join().expect("server thread");
        assert_eq!(total, Some(5000));
        assert_eq!(
            super::serena_output_tokens_from_value(&serde_json::json!({
                "stats": {"foreign": {"output_tokens": 5000}}
            })),
            None,
            "a generic local JSON service must not look like Serena"
        );

        // Nothing listening: must be None, not Some(0).
        let unused = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let dead_port = unused.local_addr().expect("addr").port();
        drop(unused);
        assert_eq!(
            super::fetch_serena_output_tokens(&format!("http://127.0.0.1:{dead_port}")),
            None
        );
    }

    #[test]
    fn fetch_serena_output_tokens_sums_more_than_four_dashboards() {
        let mut ports = Vec::new();
        let mut handles = Vec::new();
        for tokens in [100_u64, 200, 300, 400, 500] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            ports.push(listener.local_addr().expect("addr").port());
            handles.push(std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0_u8; 1024];
                let _ = std::io::Read::read(&mut stream, &mut buf);
                let body = format!(
                    r#"{{"stats":{{"find_symbol":{{"num_times_called":1,"input_tokens":0,"output_tokens":{tokens}}}}}}}"#
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write");
            }));
        }

        assert_eq!(
            super::fetch_serena_output_tokens_from_ports(&ports),
            Some(1_500)
        );
        for handle in handles {
            handle.join().expect("server thread");
        }
    }

    #[test]
    fn list_tools_exposes_savings_labels() {
        let (_root, runtime, manager) = seed_test_runtime("savings-labels");
        let label_of = |id: &str| {
            manager
                .list_tools()
                .into_iter()
                .find(|tool| tool.id == id)
                .expect("tool listed")
                .savings_label
        };

        assert_eq!(label_of("markitdown"), None);
        fs::write(runtime.tools_dir.join("markitdown-conversions"), b"3").expect("counter");
        assert_eq!(label_of("markitdown").as_deref(), Some("3 docs converted"));

        let ponytail = label_of("ponytail").expect("ponytail label");
        assert!(
            ponytail.contains("benchmark"),
            "must cite benchmark: {ponytail}"
        );
        assert_eq!(label_of("rtk"), None, "rtk figure comes from RuntimeStatus");
    }

    #[test]
    fn commit_headroom_upgrade_removes_backup() {
        let (root, runtime, manager) = seed_test_runtime("commit-backup");
        let backup = manager.venv_backup_dir();
        fs::create_dir_all(&backup).expect("backup dir");
        fs::write(backup.join("old-marker"), b"old").expect("old marker");
        fs::write(
            manager.headroom_receipt_backup_path(),
            br#"{"version":"0.0.0"}"#,
        )
        .expect("receipt backup");

        manager.commit_headroom_upgrade().expect("commit ok");

        assert!(!backup.exists(), "backup should be removed");
        assert!(!manager.headroom_receipt_backup_path().exists());
        assert!(
            runtime.venv_dir.join("marker").exists(),
            "live venv untouched"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recover_rtk_pending_restores_binary_and_receipt() {
        let (root, runtime, manager) = seed_test_runtime("rtk-pending-recovery");
        fs::create_dir_all(&runtime.bin_dir).expect("bin dir");
        let destination = manager.rtk_entrypoint();
        let backup = destination.with_extension("old-test");
        let staged = destination.with_extension("new-test");
        fs::write(&backup, b"old-binary").expect("backup");
        fs::write(&destination, b"new-binary").expect("new binary");
        fs::write(&staged, b"staged").expect("staged");
        fs::write(
            runtime.tools_dir.join("rtk.json.backup"),
            br#"{"version":"0.45.0"}"#,
        )
        .expect("receipt backup");
        fs::write(
            runtime.tools_dir.join("rtk.json"),
            br#"{"version":"0.46.0"}"#,
        )
        .expect("new receipt");
        let marker = runtime.downloads_dir.join("rtk-install.pending");
        fs::write(
            &marker,
            serde_json::to_vec(&json!({
                "destination": destination,
                "backup": backup,
                "staged": staged,
                "hadReceipt": true,
            }))
            .expect("marker"),
        )
        .expect("write marker");

        manager.recover_rtk_pending().expect("recover");
        assert_eq!(fs::read(&destination).expect("destination"), b"old-binary");
        assert_eq!(
            fs::read(runtime.tools_dir.join("rtk.json")).expect("receipt"),
            br#"{"version":"0.45.0"}"#
        );
        assert!(!marker.exists());
        assert!(!staged.exists());
        assert!(!backup.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_headroom_upgrade_is_noop_without_backup() {
        let (root, _runtime, manager) = seed_test_runtime("commit-noop");
        manager.commit_headroom_upgrade().expect("noop ok");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn collect_native_extensions_walks_recursively_and_filters_by_extension() {
        let root = unique_temp_dir("collect-natives");
        let sp = root.join("site-packages");
        let pkg = sp.join("torch").join("_C");
        fs::create_dir_all(&pkg).expect("nested dirs");
        // Should be collected:
        fs::write(sp.join("mmh3.cpython-312-darwin.so"), b"").expect("so file");
        fs::write(pkg.join("libtorch_python.dylib"), b"").expect("dylib file");
        fs::write(
            sp.join("hnswlib").join("hnswlib.cpython-312-darwin.so"),
            b"",
        )
        .or_else(|_| {
            fs::create_dir_all(sp.join("hnswlib")).and_then(|_| {
                fs::write(
                    sp.join("hnswlib").join("hnswlib.cpython-312-darwin.so"),
                    b"",
                )
            })
        })
        .expect("nested so");
        // Should NOT be collected:
        fs::write(sp.join("README.md"), b"docs").expect("md file");
        fs::write(sp.join("module.py"), b"code").expect("py file");
        fs::write(pkg.join("_C.pyi"), b"stubs").expect("pyi file");

        let mut paths = Vec::new();
        super::collect_native_extensions(&sp, &mut paths).expect("walk ok");
        paths.sort();

        assert_eq!(paths.len(), 3, "expected 3 native files, got {paths:?}");
        assert!(paths
            .iter()
            .any(|p| p.ends_with("mmh3.cpython-312-darwin.so")));
        assert!(paths.iter().any(|p| p.ends_with("libtorch_python.dylib")));
        assert!(paths
            .iter()
            .any(|p| p.ends_with("hnswlib.cpython-312-darwin.so")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ad_hoc_sign_venv_natives_returns_zero_when_site_packages_missing() {
        // Fresh venv dir with no lib/python3.12/site-packages subtree — the
        // helper must silently return 0 rather than error. This is the path
        // exercised on every install before pip has populated the venv.
        let (root, _runtime, manager) = seed_test_runtime("codesign-no-sitepackages");
        assert_eq!(manager.ad_hoc_sign_venv_natives(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_headroom_upgrade_removes_lock_backup() {
        let (root, _runtime, manager) = seed_test_runtime("commit-lock-backup");
        let lock_backup = manager.lock_backup_path();
        fs::write(&lock_backup, b"old-lock==1.0\n").expect("seed lock backup");

        manager.commit_headroom_upgrade().expect("commit ok");

        assert!(
            !lock_backup.exists(),
            "in-place lock backup should be removed on commit"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_and_read_in_place_marker_roundtrip() {
        let (root, _runtime, manager) = seed_test_runtime("marker-roundtrip");
        let lock_backup = manager.lock_backup_path();
        fs::write(&lock_backup, b"old-lock\n").expect("seed lock backup");

        manager
            .write_upgrade_marker("0.11.0", Some("0.10.8"), Some(&lock_backup))
            .expect("marker");

        let (prev, target, backup) = manager
            .read_in_place_marker()
            .expect("marker should parse as in-place");
        assert_eq!(prev, "0.10.8");
        assert_eq!(target, "0.11.0");
        assert_eq!(backup.as_deref(), Some(lock_backup.as_path()));

        // Atomic-rebuild markers (no previous_version) must not parse as in-place.
        manager
            .write_upgrade_marker("0.11.0", None, None)
            .expect("atomic marker");
        assert!(manager.read_in_place_marker().is_none());

        // Wheel-only shape (previous_version but no lock backup).
        manager
            .write_upgrade_marker("0.10.8", Some("0.10.7"), None)
            .expect("wheel-only marker");
        let (prev, target, backup) = manager.read_in_place_marker().expect("parse");
        assert_eq!(prev, "0.10.7");
        assert_eq!(target, "0.10.8");
        assert!(backup.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_upgrade_marker_is_preserved_for_recovery() {
        let (root, _runtime, manager) = seed_test_runtime("marker-malformed");
        let marker = manager.upgrade_marker_path();
        fs::write(&marker, br#"["#).expect("malformed marker");

        assert!(
            !manager.recover_from_interrupted_upgrade(),
            "malformed marker must stop recovery rather than guessing"
        );
        assert!(marker.exists(), "malformed marker must remain for repair");
        assert!(
            manager.read_in_place_marker().is_none(),
            "compatibility reader reports no usable in-place marker"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn update_headroom_receipt_after_in_place_upgrade_rewrites_artifact() {
        // Guards the legacy-sha migration path. When LEGACY_REQUIREMENTS_LOCK_SHAS
        // is empty (current state after the 0.19.0 lock regen), there is no
        // legacy fixture to inject — re-enable when a future cosmetic-only lock
        // change re-populates the list.
        if super::LEGACY_REQUIREMENTS_LOCK_SHAS.is_empty() {
            return;
        }
        let (root, runtime, manager) = seed_test_runtime("receipt-rewrite");
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": "0.10.4",
                "artifact": {
                    "url": "https://old.example/headroom_ai-0.10.4.whl",
                    "sha256": "oldoldold",
                    "requirementsLockSha256": super::LEGACY_REQUIREMENTS_LOCK_SHAS[0],
                },
                "mcp": { "configured": false },
            }))
            .unwrap(),
        )
        .expect("seed receipt");

        let release = HeadroomRelease {
            version: "0.10.8".into(),
            wheel_url: "https://new.example/headroom_ai-0.10.8.whl".into(),
            sha256: "newnewnew".into(),
        };
        let mcp = serde_json::json!({ "configured": true, "proxyUrl": "http://127.0.0.1:6867" });
        manager
            .update_headroom_receipt_after_in_place_upgrade(&release, mcp.clone())
            .expect("receipt update ok");

        let receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(runtime.tools_dir.join("headroom.json")).expect("read receipt"),
        )
        .expect("parse receipt");
        assert_eq!(receipt["version"], "0.10.8");
        assert_eq!(receipt["artifact"]["url"], release.wheel_url);
        assert_eq!(receipt["artifact"]["sha256"], release.sha256);
        assert_eq!(
            receipt["artifact"]["requirementsLockSha256"],
            requirements_lock_sha(super::bootstrap_requirements_lock()),
            "legacy sha must be migrated to the comment-insensitive form"
        );
        assert_eq!(receipt["mcp"], mcp);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_headroom_upgrade_restores_from_backup() {
        // Simulate state after a boot-validation failure: a NEW venv is live
        // at venv_dir, the previous one is at venv_dir.backup, and the old
        // receipt is snapshotted.
        let (root, runtime, manager) = seed_test_runtime("rollback");
        let backup = manager.venv_backup_dir();

        // "Move" the current live venv to backup and create a fake "new" venv.
        fs::rename(&runtime.venv_dir, &backup).expect("move aside");
        fs::create_dir_all(&runtime.venv_dir).expect("new venv dir");
        fs::write(runtime.venv_dir.join("new-marker"), b"new").expect("new marker");
        fs::copy(
            runtime.tools_dir.join("headroom.json"),
            manager.headroom_receipt_backup_path(),
        )
        .expect("snapshot receipt");
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            br#"{"version":"9.9.9"}"#,
        )
        .expect("new receipt");

        manager
            .rollback_headroom_upgrade()
            .expect("rollback succeeds");

        // The live venv should now be the original (contains "marker", not "new-marker").
        assert!(
            runtime.venv_dir.join("marker").exists(),
            "restored marker present"
        );
        assert!(
            !runtime.venv_dir.join("new-marker").exists(),
            "new venv wiped"
        );
        assert!(!backup.exists(), "backup consumed");
        let receipt = fs::read(runtime.tools_dir.join("headroom.json")).expect("receipt");
        assert!(
            String::from_utf8_lossy(&receipt).contains("0.0.1"),
            "receipt restored to previous: {}",
            String::from_utf8_lossy(&receipt)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_headroom_upgrade_is_noop_without_backup() {
        let (root, _runtime, manager) = seed_test_runtime("rollback-noop");
        manager.rollback_headroom_upgrade().expect("noop ok");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recover_from_interrupted_upgrade_restores_backup_as_live() {
        // Simulate an interrupted upgrade: marker present, venv.backup has
        // the real old venv, venv has some partial new content.
        let (root, runtime, manager) = seed_test_runtime("interrupted");
        let backup = manager.venv_backup_dir();

        // Move original venv aside (as atomic_upgrade would).
        fs::rename(&runtime.venv_dir, &backup).expect("move aside");
        // Simulate a partial new venv left by an interrupted pip install.
        fs::create_dir_all(&runtime.venv_dir).expect("partial venv");
        fs::write(runtime.venv_dir.join("partial-marker"), b"interrupted").expect("partial");
        // Marker file and receipt backup (written by atomic_upgrade).
        manager
            .write_upgrade_marker("0.8.2", None, None)
            .expect("marker");
        fs::copy(
            runtime.tools_dir.join("headroom.json"),
            manager.headroom_receipt_backup_path(),
        )
        .expect("receipt snapshot");
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            br#"{"version":"9.9.9-partial"}"#,
        )
        .expect("new receipt");

        let recovered = manager.recover_from_interrupted_upgrade();
        assert!(recovered, "recovery should fire");

        // The live venv should be the restored original.
        assert!(
            runtime.venv_dir.join("marker").exists(),
            "original restored"
        );
        assert!(
            !runtime.venv_dir.join("partial-marker").exists(),
            "partial new venv discarded"
        );
        assert!(!backup.exists(), "backup consumed");
        assert!(
            !manager.upgrade_marker_path().exists(),
            "marker cleared after recovery"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recover_from_interrupted_upgrade_is_noop_without_marker() {
        let (root, _runtime, manager) = seed_test_runtime("interrupted-noop");
        assert!(!manager.recover_from_interrupted_upgrade());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recover_from_interrupted_upgrade_keeps_marker_when_pip_fails() {
        // Offline / broken-python recovery: pip fails, so the marker and the
        // receipt backup must survive for a retry on the next launch —
        // clearing them used to leave a mixed venv that the restored receipt
        // declared healthy.
        let (root, runtime, manager) = seed_test_runtime("recover-pip-fails");
        write_executable(&runtime.managed_python(), "#!/bin/sh\nexit 1\n");
        manager
            .write_upgrade_marker("0.10.8", Some("0.10.7"), None)
            .expect("marker");
        fs::write(
            manager.headroom_receipt_backup_path(),
            br#"{"version":"0.10.7"}"#,
        )
        .expect("receipt snapshot");

        assert!(!manager.recover_from_interrupted_upgrade());
        assert!(
            manager.upgrade_marker_path().exists(),
            "marker kept for retry when pip fails"
        );
        assert!(
            manager.headroom_receipt_backup_path().exists(),
            "receipt backup kept for retry when pip fails"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn recover_from_interrupted_upgrade_handles_wheel_only_marker() {
        // In-place marker without a lock backup (wheel-only interrupted
        // upgrade). A stub python stands in for a successful pip reinstall;
        // we assert the file-manipulation side: receipt restored, marker
        // cleared.
        let (root, runtime, manager) = seed_test_runtime("recover-wheel-only");
        write_executable(&runtime.managed_python(), "#!/bin/sh\nexit 0\n");
        manager
            .write_upgrade_marker("0.10.8", Some("0.10.7"), None)
            .expect("marker");
        fs::write(
            manager.headroom_receipt_backup_path(),
            br#"{"version":"0.10.7"}"#,
        )
        .expect("receipt snapshot");
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            br#"{"version":"0.10.8-partial"}"#,
        )
        .expect("partial receipt");

        assert!(manager.recover_from_interrupted_upgrade());

        assert!(
            !manager.upgrade_marker_path().exists(),
            "marker cleared after recovery"
        );
        assert!(
            !manager.headroom_receipt_backup_path().exists(),
            "receipt backup consumed"
        );
        let receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(runtime.tools_dir.join("headroom.json")).expect("read receipt"),
        )
        .expect("parse receipt");
        assert_eq!(receipt["version"], "0.10.7", "receipt restored to previous");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn recover_from_interrupted_upgrade_handles_in_place_marker_with_lock_backup() {
        // In-place marker with a lock backup. Recovery should: copy the lock
        // backup back to the active lock path, remove the backup, restore the
        // receipt, and clear the marker. A stub python stands in for
        // successful pip calls.
        let (root, runtime, manager) = seed_test_runtime("recover-lock-backup");
        write_executable(&runtime.managed_python(), "#!/bin/sh\nexit 0\n");
        let active_lock = manager.active_lock_path();
        let lock_backup = manager.lock_backup_path();
        fs::write(&active_lock, b"new-lock==2.0\n").expect("seed active lock");
        fs::write(&lock_backup, b"old-lock==1.0\n").expect("seed lock backup");

        manager
            .write_upgrade_marker("0.11.0", Some("0.10.8"), Some(&lock_backup))
            .expect("marker");
        fs::write(
            manager.headroom_receipt_backup_path(),
            br#"{"version":"0.10.8"}"#,
        )
        .expect("receipt snapshot");

        assert!(manager.recover_from_interrupted_upgrade());

        assert!(!manager.upgrade_marker_path().exists(), "marker cleared");
        assert!(!lock_backup.exists(), "lock backup consumed");
        assert_eq!(
            fs::read(&active_lock).expect("read active lock"),
            b"old-lock==1.0\n",
            "active lock rolled back to snapshot content"
        );
        assert!(!manager.headroom_receipt_backup_path().exists());
        let receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(runtime.tools_dir.join("headroom.json")).expect("read receipt"),
        )
        .expect("parse receipt");
        assert_eq!(receipt["version"], "0.10.8");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_upgrade_purges_stale_backup_and_reports_failure_without_python() {
        // Without a real standalone python available, create_managed_venv()
        // will fail. We still want to verify that a stale backup from a
        // previous aborted upgrade is removed before the attempt, and that
        // the live venv is restored byte-for-byte after the failure.
        let (root, runtime, manager) = seed_test_runtime("atomic-stale");

        // Pre-seed a stale backup (simulating a previous aborted upgrade).
        let stale_backup = manager.venv_backup_dir();
        fs::create_dir_all(&stale_backup).expect("stale backup");
        fs::write(stale_backup.join("stale-marker"), b"stale").expect("stale marker");

        // Fake release — bogus URL ensures download/install would fail even
        // if we somehow reached that step.
        let release = HeadroomRelease {
            version: "0.0.0-test".into(),
            wheel_url: "https://example.invalid/headroom.whl".into(),
            sha256: "deadbeef".into(),
        };

        let outcome = manager.atomic_upgrade_headroom(&release, |_| {}, false);

        match outcome {
            UpgradeOutcome::InstallFailed { restored, .. } => {
                assert!(restored, "old venv should be restored after failure");
            }
            UpgradeOutcome::InstalledPendingValidation { .. } => {
                panic!("unexpected success without python");
            }
        }

        // Live venv is back with its original content.
        assert!(
            runtime.venv_dir.join("marker").exists(),
            "original marker restored"
        );
        // Stale backup purged (either consumed during restore or cleaned at start).
        assert!(!stale_backup.exists(), "stale backup removed");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn requirements_are_stale_recognizes_legacy_sha_and_migrates_receipt() {
        if super::LEGACY_REQUIREMENTS_LOCK_SHAS.is_empty() {
            return;
        }
        let (root, runtime, manager) = seed_test_runtime("legacy-sha-migrate");
        let legacy_sha = super::LEGACY_REQUIREMENTS_LOCK_SHAS[0];
        let receipt_path = runtime.tools_dir.join("headroom.json");
        fs::write(
            &receipt_path,
            serde_json::to_vec(&serde_json::json!({
                "version": "0.2.50",
                "artifact": { "requirementsLockSha256": legacy_sha },
            }))
            .unwrap(),
        )
        .expect("receipt");

        assert!(
            !manager.requirements_are_stale(),
            "legacy sha should be treated as current"
        );

        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).expect("receipt read")).expect("json");
        assert_eq!(
            receipt["artifact"]["requirementsLockSha256"],
            requirements_lock_sha(super::bootstrap_requirements_lock()),
            "receipt should be migrated to the new-format sha"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn requirements_are_stale_flags_unknown_sha() {
        let (root, runtime, manager) = seed_test_runtime("unknown-sha");
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": "0.2.45",
                "artifact": { "requirementsLockSha256": "deadbeef".repeat(8) },
            }))
            .unwrap(),
        )
        .expect("receipt");

        assert!(
            manager.requirements_are_stale(),
            "unknown sha must force a reinstall"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_in_place_skips_lock_snapshot_when_sha_matches() {
        let (root, runtime, manager) = seed_test_runtime("in-place-current");
        let current_sha = requirements_lock_sha(super::bootstrap_requirements_lock());
        // Receipt must be ≥ ATOMIC_REBUILD_FLOOR_VERSION or `prepare_in_place_upgrade`
        // forces an atomic rebuild before reaching the lock-snapshot logic.
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": "0.20.0",
                "artifact": { "requirementsLockSha256": current_sha },
            }))
            .unwrap(),
        )
        .expect("receipt");

        let ctx = manager
            .prepare_in_place_upgrade()
            .expect("eligible for in-place");
        assert_eq!(ctx.previous_version, "0.20.0");
        assert!(
            ctx.previous_lock_backup.is_none(),
            "lock unchanged => no snapshot"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_in_place_skips_lock_snapshot_when_stored_sha_is_legacy() {
        if super::LEGACY_REQUIREMENTS_LOCK_SHAS.is_empty() {
            return;
        }
        let (root, runtime, manager) = seed_test_runtime("in-place-legacy");
        let legacy_sha = super::LEGACY_REQUIREMENTS_LOCK_SHAS[0];
        // Receipt must be ≥ ATOMIC_REBUILD_FLOOR_VERSION; this test exercises
        // the legacy-sha path, not the version-floor path.
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": "0.20.0",
                "artifact": { "requirementsLockSha256": legacy_sha },
            }))
            .unwrap(),
        )
        .expect("receipt");

        let ctx = manager
            .prepare_in_place_upgrade()
            .expect("eligible for in-place");
        assert_eq!(ctx.previous_version, "0.20.0");
        assert!(ctx.previous_lock_backup.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_in_place_snapshots_lock_when_pins_differ() {
        let (root, runtime, manager) = seed_test_runtime("in-place-lock-churn");
        // Receipt must be ≥ ATOMIC_REBUILD_FLOOR_VERSION; this test is about
        // the lock-snapshot path, not the version-floor path (covered by
        // `receipt_requires_atomic_rebuild_below_floor`).
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": "0.20.0",
                "artifact": { "requirementsLockSha256": "deadbeef".repeat(8) },
            }))
            .unwrap(),
        )
        .expect("receipt");
        fs::write(manager.active_lock_path(), b"old-lock-content==1.0\n")
            .expect("seed active lock");

        let ctx = manager
            .prepare_in_place_upgrade()
            .expect("eligible for in-place");
        assert_eq!(ctx.previous_version, "0.20.0");
        let backup = ctx
            .previous_lock_backup
            .as_ref()
            .expect("lock changed => snapshot taken");
        assert_eq!(
            fs::read(backup).expect("backup readable"),
            b"old-lock-content==1.0\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_in_place_falls_back_to_atomic_when_lock_missing() {
        // Lock pins differ AND the active lock is missing on disk => caller
        // should fall through to the full atomic rebuild so rollback stays
        // safe. Receipt must be ≥ ATOMIC_REBUILD_FLOOR_VERSION so the
        // version-floor early-return doesn't pre-empt the assertion target.
        let (root, runtime, manager) = seed_test_runtime("in-place-no-lock-on-disk");
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": "0.20.0",
                "artifact": { "requirementsLockSha256": "deadbeef".repeat(8) },
            }))
            .unwrap(),
        )
        .expect("receipt");
        // no active lock written
        assert!(manager.prepare_in_place_upgrade().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_in_place_skipped_without_installed_version() {
        let (root, runtime, manager) = seed_test_runtime("in-place-no-receipt");
        fs::remove_file(runtime.tools_dir.join("headroom.json")).expect("drop receipt");
        assert!(manager.prepare_in_place_upgrade().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_in_place_falls_back_to_atomic_when_receipt_predates_floor() {
        // 0.8.2 (shipped in headroom-local-community 0.2.50-rc.1 and the fallback
        // version on every Sentry boot-validation stall observed for 0.3.6)
        // is below the 0.10.0 floor. Force the rebuild even when the lock
        // snapshot is takeable.
        let (root, runtime, manager) = seed_test_runtime("in-place-pre-floor");
        let current_sha = requirements_lock_sha(super::bootstrap_requirements_lock());
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": "0.8.2",
                "artifact": { "requirementsLockSha256": current_sha },
            }))
            .unwrap(),
        )
        .expect("receipt");
        fs::write(manager.active_lock_path(), b"old-lock-content==1.0\n")
            .expect("seed active lock");
        assert!(
            manager.prepare_in_place_upgrade().is_none(),
            "0.8.2 receipt must force atomic rebuild even when lock snapshot is takeable"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Points $HOME and $CODEX_HOME at the test root for the test's lifetime.
    /// repair/bootstrap paths reach install_headroom_mcp, which writes MCP
    /// registrations into ~/.claude.json and ~/.codex/config.toml — without
    /// this guard a test run corrupts the developer's real agent configs with
    /// a temp-dir entrypoint that macOS later deletes.
    struct HomeGuard {
        prev_home: Option<std::ffi::OsString>,
        prev_codex: Option<std::ffi::OsString>,
        _env_lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeGuard {
        fn new(root: &Path) -> Self {
            let env_lock = crate::test_env_lock::lock_home();
            let prev_home = std::env::var_os("HOME");
            let prev_codex = std::env::var_os("CODEX_HOME");
            std::env::set_var("HOME", root);
            std::env::remove_var("CODEX_HOME");
            HomeGuard {
                prev_home,
                prev_codex,
                _env_lock: env_lock,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match self.prev_codex.take() {
                Some(v) => std::env::set_var("CODEX_HOME", v),
                None => std::env::remove_var("CODEX_HOME"),
            }
        }
    }

    /// Snapshots and clears every var `hf_hub_cache_dir` reads, so the
    /// precedence test is hermetic on a dev machine that has any of them set.
    struct HfEnvGuard {
        prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    const HF_CACHE_VARS: [&str; 4] = [
        "HF_HUB_CACHE",
        "HUGGINGFACE_HUB_CACHE",
        "HF_HOME",
        "XDG_CACHE_HOME",
    ];

    impl HfEnvGuard {
        fn new() -> Self {
            let prev = HF_CACHE_VARS
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
            for key in HF_CACHE_VARS {
                std::env::remove_var(key);
            }
            HfEnvGuard { prev }
        }
    }

    impl Drop for HfEnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.prev.drain(..) {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn hf_hub_cache_dir_from_env_follows_huggingface_precedence() {
        let root = std::env::temp_dir().join("headroom-hf-precedence");
        let _home = HomeGuard::new(&root);
        let _hf = HfEnvGuard::new();

        // Default: ${XDG_CACHE_HOME:-$HOME/.cache}/huggingface/hub
        assert_eq!(
            super::hf_hub_cache_dir_from_env(),
            Some(root.join(".cache").join("huggingface").join("hub"))
        );

        // An empty value is treated as unset rather than resolving to a
        // relative path.
        std::env::set_var("HF_HUB_CACHE", "");
        assert_eq!(
            super::hf_hub_cache_dir_from_env(),
            Some(root.join(".cache").join("huggingface").join("hub"))
        );
        std::env::remove_var("HF_HUB_CACHE");

        std::env::set_var("XDG_CACHE_HOME", "/x/xdg");
        assert_eq!(
            super::hf_hub_cache_dir_from_env(),
            Some(PathBuf::from("/x/xdg/huggingface/hub"))
        );

        // $HF_HOME/hub beats XDG_CACHE_HOME.
        std::env::set_var("HF_HOME", "/x/hfhome");
        assert_eq!(
            super::hf_hub_cache_dir_from_env(),
            Some(PathBuf::from("/x/hfhome/hub"))
        );

        // The legacy var beats HF_HOME and is itself the full hub path, no
        // `hub` suffix appended.
        std::env::set_var("HUGGINGFACE_HUB_CACHE", "/x/legacy");
        assert_eq!(
            super::hf_hub_cache_dir_from_env(),
            Some(PathBuf::from("/x/legacy"))
        );

        // HF_HUB_CACHE wins outright.
        std::env::set_var("HF_HUB_CACHE", "/x/win");
        assert_eq!(
            super::hf_hub_cache_dir_from_env(),
            Some(PathBuf::from("/x/win"))
        );
    }

    #[cfg(feature = "local-community")]
    #[test]
    #[serial_test::serial]
    fn community_hf_hub_cache_is_workspace_scoped() {
        let _hf = HfEnvGuard::new();
        std::env::set_var("HF_HUB_CACHE", "/shared/huggingface/hub");
        std::env::set_var("HF_HOME", "/shared/huggingface");

        assert_eq!(
            super::hf_hub_cache_dir(),
            Some(crate::edition::huggingface_hub_cache_dir())
        );
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn repair_stale_requirements_updates_receipt_and_emits_progress() {
        let (root, runtime, manager) = seed_test_runtime("repair-requirements");
        let _home = HomeGuard::new(&root);
        write_executable(&runtime.managed_python(), "#!/bin/sh\nexit 0\n");
        write_executable(&manager.headroom_entrypoint(), "#!/bin/sh\nexit 0\n");
        fs::write(
            runtime.tools_dir.join("headroom.json"),
            br#"{
                "version":"0.8.2",
                "artifact":{"requirementsLockSha256":"stale"},
                "mcp":{"configured":false}
            }"#,
        )
        .expect("seed receipt");

        let mut steps = Vec::new();
        manager
            .repair_stale_requirements_with_progress(|step| steps.push(step.step.to_string()))
            .expect("repair succeeds");

        assert!(steps.iter().any(|step| step == "Repairing dependencies"));
        assert!(steps.iter().any(|step| step == "Configuring integrations"));
        assert!(steps.iter().any(|step| step == "Repair complete"));

        let receipt = fs::read(runtime.tools_dir.join("headroom.json")).expect("receipt");
        let receipt: serde_json::Value = serde_json::from_slice(&receipt).expect("receipt json");
        assert_eq!(
            receipt["artifact"]["requirementsLockSha256"],
            requirements_lock_sha(super::bootstrap_requirements_lock())
        );
        assert_eq!(receipt["mcp"]["configured"], true);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn smoke_test_headroom_succeeds_with_executable_python() {
        let (root, runtime, manager) = seed_test_runtime("smoke-ok");
        write_executable(&runtime.managed_python(), "#!/bin/sh\nexit 0\n");

        manager
            .smoke_test_headroom_with_timeout(Duration::from_secs(10))
            .expect("smoke test succeeds");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn smoke_test_headroom_returns_command_failure_output_on_nonzero_exit() {
        let (root, runtime, manager) = seed_test_runtime("smoke-fail");
        write_executable(
            &runtime.managed_python(),
            "#!/bin/sh\necho failure-stdout\necho failure-stderr >&2\nexit 7\n",
        );

        let err = manager
            .smoke_test_headroom_with_timeout(Duration::from_secs(10))
            .expect_err("smoke test should fail");
        let failure = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CommandFailure>())
            .expect("command failure");
        assert_eq!(failure.exit_code, Some(7));
        assert!(failure.stdout.contains("failure-stdout"));
        assert!(failure.stderr.contains("failure-stderr"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn smoke_test_markitdown_is_noop_when_not_installed() {
        let (root, _runtime, manager) = seed_test_runtime("markitdown-smoke-absent");
        manager
            .smoke_test_markitdown_with_timeout(Duration::from_secs(2))
            .expect("no-op when markitdown is absent");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn marketplace_build_root_contains_manifest_source_root() {
        let (root, _runtime, manager) = seed_test_runtime("plugin-marketplace-roots");
        for id in ["openspec", "gstack"] {
            let plugin = super::plugin_addon(id).expect("plugin manifest");
            let package_root = manager.marketplace_package_root(plugin);
            let source_root = manager.codex_plugin_source_path(plugin);
            assert!(package_root.ends_with(plugin.marketplace_name));
            assert!(source_root.starts_with(&package_root));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)] // exercises a fake Codex shell binary; Windows cannot exec it
    fn successful_codex_command_survives_cache_normalization_failure() {
        let (root, _runtime, manager) = seed_test_runtime("plugin-cmd-cache-normalization");
        let _home = HomeGuard::new(&root);
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "stop-that-shit")
            .expect("Stop That Shit addon");

        // A syntactically invalid copied manifest makes the post-command
        // normalization fail. The CLI command itself still exits successfully;
        // run_plugin_cmd must preserve that committed result so callers do not
        // attempt a compensating rollback against an already-mutated registry.
        let cached_manifest = root
            .join(".codex/plugins/cache")
            .join(plugin.marketplace_name)
            .join(plugin.id)
            .join("test-version/.codex-plugin/plugin.json");
        fs::create_dir_all(cached_manifest.parent().expect("manifest parent"))
            .expect("cache manifest parent");
        fs::write(&cached_manifest, b"{not valid json").expect("corrupt cached manifest");

        let cli = root.join("fake-codex");
        write_executable(&cli, "#!/bin/sh\nexit 0\n");
        manager
            .run_plugin_cmd(plugin, &cli, PluginHost::Codex, &["plugin", "add", "x"])
            .expect("a successful Codex command must remain successful");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial]
    fn codex_plugin_paths_follow_custom_codex_home() {
        let (root, _runtime, manager) = seed_test_runtime("plugin-custom-codex-home");
        let custom_codex_home = root.join("codex-home");

        {
            // HomeGuard serializes environment-sensitive tests and keeps the
            // real user profile out of plugin path resolution.
            let _home = HomeGuard::new(&root);
            std::env::set_var("CODEX_HOME", &custom_codex_home);

            let plugin = super::plugin_addon("allinluna").expect("All in Luna plugin");
            let package_root = custom_codex_home
                .join(".tmp")
                .join("marketplaces")
                .join("allinluna");
            assert_eq!(
                manager.marketplace_package_root(plugin),
                package_root,
                "marketplace roots must use CODEX_HOME"
            );
            assert_eq!(
                manager.codex_plugin_source_path(plugin),
                package_root.join("plugins/allinluna"),
                "plugin source paths must use CODEX_HOME"
            );

            let runtime = package_root.join("plugins/allinluna/runtime");
            fs::create_dir_all(runtime.join("allinluna_runtime")).expect("runtime directory");
            fs::write(
                runtime.join("allinluna_runtime/__main__.py"),
                b"# test runtime\n",
            )
            .expect("runtime marker");
            assert_eq!(
                manager.allinluna_runtime_path(),
                Some(runtime),
                "runtime discovery must follow CODEX_HOME"
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial]
    fn smoke_test_plugin_is_noop_when_not_installed() {
        let (root, _runtime, manager) = seed_test_runtime("plugin-smoke-absent");
        let _home = HomeGuard::new(&root);
        for plugin in &PLUGIN_ADDONS {
            manager
                .smoke_test_plugin(plugin.id)
                .expect("no-op when plugin receipt is absent");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn codex_plugin_toggle_preserves_unrelated_toml_and_inline_comment() {
        let before = concat!(
            "model = \"gpt-5\"\n",
            "\n",
            "[plugins.\"ponytail@ponytail\"]\n",
            "source = \"user\"\n",
            "enabled = true # keep this explanation\n",
            "\n",
            "[plugins.\"other@marketplace\"]\n",
            "enabled = true\n",
        );

        let after = render_codex_plugin_enabled(before, "ponytail@ponytail", false)
            .expect("render toggle")
            .expect("toggle changes config");
        assert!(
            after.contains("enabled = false # keep this explanation\n"),
            "only the boolean value should change: {after:?}"
        );
        assert!(
            after.contains("[plugins.\"other@marketplace\"]\nenabled = true\n"),
            "unrelated plugin table must remain byte-for-byte present"
        );
        let parsed: toml::Value = after.parse().expect("rewritten config parses");
        assert_eq!(
            parsed["plugins"]["ponytail@ponytail"]["enabled"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn codex_plugin_toggle_inserts_explicit_flag_and_keeps_crlf() {
        let before = concat!(
            "[plugins.'allinluna@allinluna']\r\n",
            "source = 'user'\r\n",
            "[profiles.default]\r\n",
            "model = 'gpt-5'\r\n",
        );
        let after = render_codex_plugin_enabled(before, "allinluna@allinluna", false)
            .expect("render toggle")
            .expect("toggle changes config");
        assert!(
            after.starts_with(
                "[plugins.'allinluna@allinluna']\r\nenabled = false\r\nsource = 'user'\r\n"
            ),
            "the inserted flag should use the existing line ending: {after:?}"
        );
        assert!(after.contains("[profiles.default]\r\nmodel = 'gpt-5'\r\n"));
        assert_eq!(
            super::codex_plugin_enabled_from_text(&after, "allinluna@allinluna")
                .expect("read enabled state"),
            Some(false)
        );
    }

    #[test]
    fn codex_plugin_toggle_rejects_missing_or_malformed_registration() {
        let missing_header = "[plugins]\n\"ponytail@ponytail\" = { enabled = true }\n";
        assert!(
            render_codex_plugin_enabled(missing_header, "ponytail@ponytail", false).is_err(),
            "inline user config must not be rewritten as a managed table"
        );

        let malformed = "[plugins.\"ponytail@ponytail\"]\nenabled = nope\n";
        assert!(
            render_codex_plugin_enabled(malformed, "ponytail@ponytail", false).is_err(),
            "a non-boolean enabled value must fail closed"
        );
    }

    #[test]
    #[serial_test::serial]
    fn codex_plugin_toggle_keeps_registration_for_fast_reenable() {
        let (root, _runtime, _manager) = seed_test_runtime("codex-plugin-toggle");
        let _home = HomeGuard::new(&root);
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "ponytail")
            .expect("Ponytail addon");
        let codex_home = root.join(".codex");
        fs::create_dir_all(&codex_home).expect("Codex home");
        let config = codex_home.join("config.toml");
        fs::write(
            &config,
            "[plugins.\"ponytail@ponytail\"]\nsource = \"managed\"\nenabled = true\n",
        )
        .expect("Codex plugin registration");

        set_codex_plugin_enabled(plugin, false).expect("disable Codex plugin");
        let disabled = fs::read_to_string(&config).expect("disabled config");
        assert!(disabled.contains("enabled = false"));
        assert!(disabled.contains("source = \"managed\""));
        assert!(
            super::codex_plugin_present_checked(plugin).expect("presence check"),
            "Disable must keep the plugin registration for uninstall/re-enable"
        );

        set_codex_plugin_enabled(plugin, true).expect("re-enable Codex plugin");
        let enabled = fs::read_to_string(&config).expect("enabled config");
        assert!(enabled.contains("enabled = true"));
        assert!(enabled.contains("source = \"managed\""));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial]
    fn smoke_test_plugin_skips_disabled_and_self_heals_stale_receipt() {
        let (root, runtime, manager) = seed_test_runtime("plugin-smoke-receipt");
        // Host registries resolve under $HOME; point it at the empty test
        // root so no plugin reads as registered.
        let _home = HomeGuard::new(&root);
        for plugin in &PLUGIN_ADDONS {
            let receipt = runtime.tools_dir.join(format!("{}.json", plugin.id));

            // Disabled by the user: the receipt is enough to make the smoke
            // path a no-op even when the host registry is unavailable.
            fs::write(&receipt, br#"{"version":"latest","enabled":false}"#).expect("receipt");
            manager
                .smoke_test_plugin(plugin.id)
                .expect("disabled plugin is not a smoke failure");
            assert!(receipt.exists(), "disabled receipt must be kept");

            // Enabled but deregistered behind our back: warn once, then
            // self-heal by dropping the stale receipt.
            fs::write(&receipt, br#"{"version":"latest","enabled":true}"#).expect("receipt");
            let err = manager
                .smoke_test_plugin(plugin.id)
                .expect_err("enabled but unregistered must fail");
            assert!(err.to_string().contains("no longer registered"));
            assert!(!receipt.exists(), "stale receipt must be removed");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_disabled_receipt_reports_installed_not_missing() {
        // A receipt with enabled:false means the user disabled it via the app;
        // the card must still show "installed" (Enable), not "not installed"
        // (Install), while the host registration remains available for a fast
        // re-enable.
        let (root, runtime, manager) = seed_test_runtime("plugin-disabled");
        for plugin in &PLUGIN_ADDONS {
            fs::write(
                runtime.tools_dir.join(format!("{}.json", plugin.id)),
                br#"{"version":"latest","enabled":false}"#,
            )
            .expect("receipt");
            assert!(matches!(
                manager.detect_status(plugin.id),
                crate::models::ToolStatus::Healthy
            ));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn allinluna_is_registered_for_command_dispatch() {
        assert!(super::is_plugin_addon("allinluna"));
    }

    #[test]
    fn uninstall_plugin_is_noop_without_receipt() {
        // Cleanup must not touch plugin/marketplace config Headroom never wrote.
        let (root, _runtime, manager) = seed_test_runtime("plugin-uninstall-noreceipt");
        for plugin in &PLUGIN_ADDONS {
            manager
                .uninstall_plugin(plugin.id)
                .expect("no-op when plugin receipt is absent");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_plugin_receipt_is_not_ownership_proof() {
        let (root, runtime, manager) = seed_test_runtime("plugin-malformed-receipt");
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");
        let receipt = runtime.tools_dir.join(format!("{}.json", plugin.id));
        fs::write(&receipt, b"{this is not json").expect("malformed receipt");

        assert!(
            !manager.plugin_receipt_exists(plugin),
            "malformed receipt must not count as ownership"
        );
        let err = manager
            .uninstall_plugin(plugin.id)
            .expect_err("destructive cleanup must reject malformed ownership data");
        assert!(
            err.to_string().contains("ownership receipt is invalid"),
            "unexpected error: {err:#}"
        );
        assert!(
            receipt.exists(),
            "invalid receipt must be preserved for repair"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_plugin_receipt_cannot_authorize_destructive_mutation() {
        let (root, runtime, manager) = seed_test_runtime("plugin-legacy-receipt");
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");
        let receipt = runtime.tools_dir.join(format!("{}.json", plugin.id));
        fs::write(&receipt, br#"{"version":"latest","enabled":true}"#).expect("legacy receipt");

        let err = manager
            .uninstall_plugin(plugin.id)
            .expect_err("legacy receipt must not authorize uninstall");
        assert!(
            err.to_string()
                .contains("legacy receipt lacks explicit Headroom ownership"),
            "unexpected error: {err:#}"
        );
        assert!(receipt.exists(), "legacy receipt must remain for migration");

        let err = manager
            .set_plugin_enabled(plugin.id, false)
            .expect_err("legacy receipt must not authorize toggling");
        assert!(
            err.to_string()
                .contains("legacy receipt lacks explicit Headroom ownership"),
            "unexpected error: {err:#}"
        );
        assert!(receipt.exists(), "legacy receipt must remain for migration");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn install_refuses_to_take_over_preexisting_codex_plugin_without_receipt() {
        let (root, _runtime, manager) = seed_test_runtime("plugin-preexisting-codex");
        let _home = HomeGuard::new(&root);
        let codex_home = root.join(".codex");
        fs::create_dir_all(&codex_home).expect("Codex home");
        let config = codex_home.join("config.toml");
        let before = b"[plugins.\"allinluna@allinluna\"]\nsource = \"user\"\n";
        fs::write(&config, before).expect("user plugin registration");
        let invoked = root.join("codex-invoked");
        let codex = root.join(".local/bin/codex");
        write_executable(
            &codex,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\ntouch '{}'\nexit 99\n",
                invoked.display()
            ),
        );

        let err = manager
            .install_plugin("allinluna")
            .expect_err("existing user plugin must not be adopted implicitly");
        assert!(
            err.to_string().contains("refusing to take over existing"),
            "unexpected error: {err:#}"
        );
        assert_eq!(fs::read(&config).expect("config bytes"), before);
        assert!(
            !invoked.exists(),
            "preflight refusal must happen before invoking the host CLI"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial]
    fn codex_marketplace_recovery_marker_roundtrips_atomically() {
        let (root, _runtime, manager) = seed_test_runtime("plugin-recovery-marker");
        let _home = HomeGuard::new(&root);
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");
        let backup = manager
            .marketplace_package_root(plugin)
            .with_file_name("allinluna.headroom-legacy");
        let recovery = super::CodexMarketplaceRecovery {
            backup_path: backup.clone(),
            original_git_source: Some("https://example.invalid/allinluna.git".to_string()),
            moved: true,
        };
        manager
            .write_codex_marketplace_recovery(plugin, &recovery)
            .expect("write recovery marker");
        let loaded = manager
            .read_codex_marketplace_recovery(plugin)
            .expect("read recovery marker")
            .expect("recovery marker exists");
        assert_eq!(loaded.backup_path, backup);
        assert_eq!(loaded.original_git_source, recovery.original_git_source);
        assert!(loaded.moved);
        manager
            .clear_codex_marketplace_recovery(plugin)
            .expect("clear recovery marker");
        assert!(!manager.codex_marketplace_recovery_path(plugin).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial]
    fn legacy_codex_marketplace_recovery_marker_defaults_to_moved() {
        let (root, _runtime, manager) = seed_test_runtime("plugin-recovery-marker-legacy");
        let _home = HomeGuard::new(&root);
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|plugin| plugin.id == "allinluna")
            .expect("All in Luna addon");
        let backup = manager
            .marketplace_package_root(plugin)
            .with_file_name("allinluna.headroom-legacy");
        let marker = manager.codex_marketplace_recovery_path(plugin);
        // Markers from the first recovery implementation had no `moved` bit;
        // they were emitted only after the checkout rename, so they must load
        // as moved=true and remain recoverable after an app upgrade.
        let legacy = serde_json::json!({
            "backupPath": backup,
            "originalGitSource": "https://example.invalid/allinluna.git"
        });
        fs::write(
            &marker,
            serde_json::to_vec(&legacy).expect("serialize legacy marker"),
        )
        .expect("write legacy marker");

        let loaded = manager
            .read_codex_marketplace_recovery(plugin)
            .expect("read legacy marker")
            .expect("legacy marker exists");
        assert!(loaded.moved, "legacy marker must default to moved=true");
        let _ = fs::remove_dir_all(root);
    }

    /// End-to-end round trip against the real Codex plugin CLI: install,
    /// disable, re-enable, smoke-test, and uninstall. Ignored by default - it
    /// needs network and mutates the real ~/.codex plugin config. Select the
    /// addon with HEADROOM_PLUGIN_ROUNDTRIP_ID (defaults to ponytail).
    #[test]
    #[ignore]
    fn plugin_install_roundtrip() {
        let id = std::env::var("HEADROOM_PLUGIN_ROUNDTRIP_ID")
            .unwrap_or_else(|_| "ponytail".to_string());
        let (root, _runtime, manager) = seed_test_runtime("plugin-roundtrip");
        if crate::claude_cli::detect_claude_cli().is_none()
            && crate::claude_cli::detect_codex_cli().is_none()
        {
            eprintln!("skipping plugin_install_roundtrip: no claude/codex CLI on PATH");
            let _ = fs::remove_dir_all(&root);
            return;
        }

        // Capture every result and always run uninstall before asserting, so a
        // failed assertion never leaves the plugin behind on the real machine.
        let install = manager.install_plugin(&id);
        let installed = manager.plugin_installed(&id);
        let disable = manager.set_plugin_enabled(&id, false);
        let disabled = !manager.tool_enabled(&id);
        let enable = manager.set_plugin_enabled(&id, true);
        let enabled = manager.tool_enabled(&id);
        let smoke_while_installed = manager.smoke_test_plugin(&id);
        let uninstall = manager.uninstall_plugin(&id);
        let gone = !manager.plugin_installed(&id);
        let _ = fs::remove_dir_all(&root);

        install.expect("install_plugin should succeed");
        assert!(installed, "plugin_installed() should be true after install");
        disable.expect("set_plugin_enabled(false) should succeed");
        assert!(disabled, "tool_enabled() should be false after disable");
        enable.expect("set_plugin_enabled(true) should succeed");
        assert!(enabled, "tool_enabled() should be true after re-enable");
        smoke_while_installed.expect("smoke_test_plugin should pass while installed");
        uninstall.expect("uninstall_plugin should succeed");
        assert!(gone, "plugin_installed() should be false after uninstall");
    }

    #[test]
    /// Installs plugins into an explicitly selected live Headroom data root
    /// and verifies them through the same smoke path used by the dashboard.
    /// Ignored by default because this requires network access and mutates the
    /// selected Headroom/Codex installation.
    #[ignore = "requires network and an explicit live Headroom data root"]
    fn plugin_live_install_and_smoke() {
        let base_dir = std::env::var_os("HEADROOM_PLUGIN_LIVE_BASE_DIR")
            .map(PathBuf::from)
            .expect("set HEADROOM_PLUGIN_LIVE_BASE_DIR explicitly");
        assert!(base_dir.is_absolute(), "live data root must be absolute");

        let ids = std::env::var("HEADROOM_PLUGIN_LIVE_IDS")
            .expect("set HEADROOM_PLUGIN_LIVE_IDS to a comma-separated plugin list");
        let runtime = ManagedRuntime::bootstrap_root(&base_dir);
        runtime.ensure_layout().expect("create live runtime layout");
        let manager = ToolManager::new(runtime);

        for id in ids.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            super::plugin_addon(id).unwrap_or_else(|| panic!("unknown plugin addon: {id}"));
            manager
                .install_plugin(id)
                .unwrap_or_else(|err| panic!("install {id}: {err:#}"));
            manager
                .smoke_test_plugin(id)
                .unwrap_or_else(|err| panic!("smoke-test {id}: {err:#}"));
            assert!(
                manager.plugin_installed(id),
                "{id} not installed after smoke"
            );
        }
    }

    #[test]
    fn workflow_conflict_groups_are_complete_and_symmetric() {
        let (_root, _runtime, manager) = seed_test_runtime("workflow-conflicts");
        assert_eq!(
            manager.conflicting_plugin_ids("openspec"),
            vec!["superpowers".to_string(), "gstack".to_string()]
        );
        assert_eq!(
            manager.conflicting_plugin_ids("superpowers"),
            vec!["openspec".to_string(), "gstack".to_string()]
        );
        assert_eq!(
            manager.conflicting_plugin_ids("gstack"),
            vec!["openspec".to_string(), "superpowers".to_string()]
        );
        assert_eq!(
            manager.conflicting_plugin_ids("allinluna"),
            vec!["ralph-loop".to_string()]
        );
        assert_eq!(
            manager.conflicting_plugin_ids("ralph-loop"),
            vec!["allinluna".to_string()]
        );
        assert!(manager.conflicting_plugin_ids("agent-guard").is_empty());
    }

    #[test]
    fn is_outdated_codex_detects_unrecognized_subcommand() {
        let outdated = anyhow::Error::new(CommandFailure {
            program: "codex".into(),
            args: vec!["plugin".into(), "add".into()],
            stdout: String::new(),
            stderr: "error: unrecognized subcommand 'add'\n".into(),
            exit_code: Some(2),
            signal: None,
        });
        assert!(is_outdated_codex(&outdated));

        let other = anyhow::Error::new(CommandFailure {
            program: "codex".into(),
            args: vec!["plugin".into(), "add".into()],
            stdout: String::new(),
            stderr: "error: network unreachable\n".into(),
            exit_code: Some(1),
            signal: None,
        });
        assert!(!is_outdated_codex(&other));

        // A non-CommandFailure error must not be misclassified.
        assert!(!is_outdated_codex(&anyhow::anyhow!(
            "unrecognized subcommand"
        )));
    }

    #[test]
    #[serial_test::serial]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn smoke_test_markitdown_succeeds_when_entrypoint_runs() {
        let (root, runtime, manager) = seed_test_runtime("markitdown-smoke-ok");
        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            br#"{"version":"0.1.6","enabled":true}"#,
        )
        .expect("receipt");
        write_executable(&manager.markitdown_entrypoint(), "#!/bin/sh\nexit 0\n");

        manager
            .smoke_test_markitdown_with_timeout(Duration::from_secs(10))
            .expect("smoke test succeeds");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn smoke_test_markitdown_fails_on_nonzero_exit() {
        let (root, runtime, manager) = seed_test_runtime("markitdown-smoke-fail");
        fs::write(
            runtime.tools_dir.join("markitdown.json"),
            br#"{"version":"0.1.6","enabled":true}"#,
        )
        .expect("receipt");
        write_executable(&manager.markitdown_entrypoint(), "#!/bin/sh\nexit 3\n");

        manager
            .smoke_test_markitdown_with_timeout(Duration::from_secs(2))
            .expect_err("smoke test should fail");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn smoke_test_headroom_repairs_pydantic_core_skew_and_retries() {
        let (root, runtime, manager) = seed_test_runtime("smoke-pydantic-skew");
        let state_file = root.join("smoke-attempts");
        let pip_log = root.join("pip-args");
        let script = format!(
            r#"#!/bin/sh
case "$1" in
  -c)
    if [ -f '{state}' ]; then
      exit 0
    fi
    touch '{state}'
    cat >&2 <<'EOF'
Traceback (most recent call last):
  File "<string>", line 1, in <module>
SystemError: The installed pydantic-core version (2.41.5) is incompatible with the current pydantic version, which requires 2.46.3. If you encounter this error, make sure that you haven't upgraded pydantic-core manually.
EOF
    exit 1
    ;;
  -m)
    echo "$@" >> '{pip_log}'
    exit 0
    ;;
esac
exit 0
"#,
            state = state_file.display(),
            pip_log = pip_log.display(),
        );
        write_executable(&runtime.managed_python(), &script);

        manager
            .smoke_test_headroom()
            .expect("repair should let smoke retry succeed");

        let pip_args = fs::read_to_string(&pip_log).expect("pip log written");
        assert!(
            pip_args.contains("pydantic-core==2.46.3"),
            "expected repair to install pydantic-core==2.46.3, got: {pip_args}"
        );
        // pydantic itself must also be force-reinstalled to collapse any
        // duplicate dist-info dirs that cause the flip-flop skew.
        let pydantic_invocations = pip_args
            .lines()
            .filter(|line| {
                line.contains("--force-reinstall")
                    && line.split_whitespace().any(|tok| tok == "pydantic")
            })
            .count();
        assert_eq!(
            pydantic_invocations, 1,
            "expected exactly one force-reinstall of pydantic, got: {pip_args}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn smoke_test_headroom_does_not_repair_unrelated_failures() {
        let (root, runtime, manager) = seed_test_runtime("smoke-unrelated-fail");
        let state_file = root.join("attempts");
        let script = format!(
            "#!/bin/sh\necho >> '{state}'\necho boom >&2\nexit 1\n",
            state = state_file.display(),
        );
        write_executable(&runtime.managed_python(), &script);

        let err = manager
            .smoke_test_headroom()
            .expect_err("smoke should fail without retry");
        let failure = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CommandFailure>())
            .expect("command failure");
        assert_eq!(failure.exit_code, Some(1));

        let attempts = fs::read_to_string(&state_file).expect("attempts log");
        assert_eq!(
            attempts.lines().count(),
            1,
            "non-skew failures should not retry"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)] // exercises a fake shell-script binary; Windows cannot exec it
    fn smoke_test_headroom_times_out() {
        let (root, runtime, manager) = seed_test_runtime("smoke-timeout");
        write_executable(&runtime.managed_python(), "#!/bin/sh\nsleep 1\n");

        let err = manager
            .smoke_test_headroom_with_timeout(Duration::from_millis(100))
            .expect_err("smoke test should time out");
        let failure = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CommandFailure>())
            .expect("command failure");
        assert_eq!(failure.exit_code, None);
        assert!(failure.stderr.contains("command timed out after 100ms"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_major_minor_patch_handles_clean_and_pre_release() {
        assert_eq!(parse_major_minor_patch("0.19.0"), Some((0, 19, 0)));
        assert_eq!(parse_major_minor_patch("1.2.3"), Some((1, 2, 3)));
        // Patch defaults to 0.
        assert_eq!(parse_major_minor_patch("0.19"), Some((0, 19, 0)));
        // Pre-release / build suffixes are stripped.
        assert_eq!(parse_major_minor_patch("0.19.0-rc.1"), Some((0, 19, 0)));
        assert_eq!(parse_major_minor_patch("0.19.0+build.5"), Some((0, 19, 0)));
        assert_eq!(parse_major_minor_patch("0.19.0.dev0"), Some((0, 19, 0)));
        // Nonsense returns None — caller treats as "rebuild" to be safe.
        assert_eq!(parse_major_minor_patch(""), None);
        assert_eq!(parse_major_minor_patch("not-a-version"), None);
        assert_eq!(parse_major_minor_patch("0"), None);
    }

    #[test]
    fn rtk_release_parser_accepts_platform_asset_and_digest() {
        let asset_name = rtk_release_asset_name().unwrap();
        let value = json!({
            "tag_name": "v0.47.0",
            "assets": [{
                "name": asset_name,
                "browser_download_url": format!("https://github.com/rtk-ai/rtk/releases/download/v0.47.0/{asset_name}"),
                "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            }]
        });
        let release = parse_rtk_release(&value, "v0.47.0").unwrap();
        assert_eq!(release.version, "0.47.0");
        assert_eq!(release.sha256.len(), 64);
    }

    #[test]
    #[cfg(unix)]
    fn set_codebase_memory_enabled_preserves_installed_version() {
        let (root, runtime, manager) = seed_test_runtime("codebase-memory-preserve-version");
        write_executable(&runtime.managed_python(), "#!/usr/bin/env bash\nexit 0\n");
        write_executable(&manager.codebase_memory_entrypoint(), "binary\n");
        manager
            .write_tool_receipt(
                "codebase-memory",
                json!({ "version": "0.10.9", "enabled": true }),
            )
            .unwrap();

        manager.set_codebase_memory_enabled(false).unwrap();

        assert_eq!(
            manager
                .read_tool_receipt("codebase-memory")
                .and_then(|value| {
                    value
                        .get("version")
                        .and_then(|version| version.as_str())
                        .map(str::to_owned)
                })
                .as_deref(),
            Some("0.10.9")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn recover_codebase_memory_pending_restores_binary_and_receipt() {
        let (root, runtime, manager) = seed_test_runtime("codebase-memory-pending-recovery");
        write_executable(&runtime.managed_python(), "#!/usr/bin/env bash\nexit 0\n");

        let destination = manager.codebase_memory_entrypoint();
        let backup = runtime.bin_dir.join("codebase-memory-mcp.backup");
        let receipt = runtime.tools_dir.join("codebase-memory.json");
        let receipt_backup = runtime.tools_dir.join("codebase-memory.json.backup");
        let marker = manager.codebase_memory_pending_marker_path();
        write_executable(&destination, "new binary\n");
        write_executable(&backup, "old binary\n");
        fs::write(
            &receipt,
            serde_json::to_vec(&json!({ "version": "0.10.9", "enabled": false })).unwrap(),
        )
        .unwrap();
        fs::write(
            &receipt_backup,
            serde_json::to_vec(&json!({ "version": "0.10.8", "enabled": false })).unwrap(),
        )
        .unwrap();
        fs::write(
            &marker,
            serde_json::to_vec(&json!({
                "version": "0.10.9",
                "hadDestination": true,
                "hadReceipt": true,
            }))
            .unwrap(),
        )
        .unwrap();

        manager
            .recover_codebase_memory_pending()
            .expect("recover codebase-memory transaction");

        assert_eq!(fs::read_to_string(&destination).unwrap(), "old binary\n");
        assert_eq!(
            manager
                .read_tool_receipt("codebase-memory")
                .and_then(|value| {
                    value
                        .get("version")
                        .and_then(|version| version.as_str())
                        .map(str::to_owned)
                })
                .as_deref(),
            Some("0.10.8")
        );
        assert!(!backup.exists());
        assert!(!receipt_backup.exists());
        assert!(!marker.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn recover_codebase_memory_pending_rejects_missing_rollback_flags() {
        let (root, runtime, manager) = seed_test_runtime("codebase-memory-pending-malformed");
        let destination = manager.codebase_memory_entrypoint();
        let receipt = runtime.tools_dir.join("codebase-memory.json");
        let marker = manager.codebase_memory_pending_marker_path();
        write_executable(&destination, "live binary\n");
        fs::write(&receipt, br#"{"version":"0.10.8","enabled":true}"#).expect("receipt");
        fs::write(&marker, br#"{"version":"0.10.8"}"#).expect("incomplete marker");

        let err = manager
            .recover_codebase_memory_pending()
            .expect_err("missing rollback flags must stop recovery");
        assert!(
            format!("{err:#}").contains("missing required hadDestination"),
            "unexpected error: {err:#}"
        );
        assert_eq!(
            fs::read(&destination).expect("live binary"),
            b"live binary\n"
        );
        assert!(receipt.exists(), "receipt must remain untouched");
        assert!(marker.exists(), "incomplete marker must remain for repair");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn codebase_memory_release_parser_requires_exact_version_url_and_digest() {
        assert!(valid_codebase_memory_version("0.10.8"));
        for version in ["v0.10.8", "0.10", "0.10.8-rc.1", "0.10.8/evil"] {
            assert!(!valid_codebase_memory_version(version));
        }
        let asset_name = format!(
            "codebase-memory-mcp-{}.tar.gz",
            codebase_memory_release_target().unwrap()
        );
        let value = json!({
            "tag_name": "v0.10.9",
            "assets": [{
                "name": asset_name,
                "browser_download_url": format!(
                    "https://github.com/DeusData/codebase-memory-mcp/releases/download/v0.10.9/{asset_name}"
                ),
                "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            }]
        });
        let artifact = parse_codebase_memory_release(&value, "0.10.9").unwrap();
        assert_eq!(artifact.sha256.unwrap().len(), 64);
        assert!(parse_codebase_memory_release(&value, "0.10.8").is_err());
        let mut bad = value.clone();
        bad["assets"][0]["browser_download_url"] = json!("https://evil.example/x.tar.gz");
        assert!(parse_codebase_memory_release(&bad, "0.10.9").is_err());
    }

    #[test]
    fn rtk_version_checks_are_exact_and_refuse_downgrade() {
        assert!(rtk_version_output_matches("rtk 0.47.0\n", "0.47.0"));
        assert!(rtk_version_output_matches("rtk v0.47.0\n", "0.47.0"));
        assert!(!rtk_version_output_matches("rtk 0.47.01\n", "0.47.0"));
        assert!(rtk_version_is_downgrade("0.48.0", "0.47.0"));
        assert!(!rtk_version_is_downgrade("0.47.0", "0.48.0"));
    }

    #[test]
    fn rtk_release_parser_rejects_injection_suffix_and_tag_mismatch() {
        for version in [
            "0.47.0-/../../x",
            "0.47.0-rc.1",
            "0.47",
            "0.47.0/evil",
            " 0.47.0",
        ] {
            assert!(
                !valid_rtk_version(version),
                "accepted unsafe version {version}"
            );
        }
        let value = json!({ "tag_name": "v0.48.0", "assets": [] });
        assert!(parse_rtk_release(&value, "0.47.0").is_err());
    }

    #[test]
    fn rtk_release_parser_requires_valid_digest_and_trusted_url() {
        let name = rtk_release_asset_name().unwrap();
        for (url, digest) in [
            (
                "https://github.com/rtk-ai/rtk/releases/download/v0.47.0/x.tar.gz",
                None,
            ),
            (
                "https://github.com/rtk-ai/rtk/releases/download/v0.47.0/x.tar.gz",
                Some("sha256:no"),
            ),
            (
                "https://evil.example/rtk.tar.gz",
                Some("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
            ),
        ] {
            let value = json!({ "tag_name": "v0.47.0", "assets": [{ "name": name, "browser_download_url": url, "digest": digest }] });
            assert!(parse_rtk_release(&value, "0.47.0").is_err());
        }
    }

    #[test]
    fn newer_rtk_receipt_is_not_downgraded_to_pin() {
        let (root, runtime, manager) = seed_test_runtime("rtk-newer-no-downgrade");
        write_executable(&runtime.bin_dir.join("rtk"), "#!/bin/sh\nexit 0\n");
        manager
            .write_tool_receipt("rtk", json!({ "version": "99.0.0" }))
            .unwrap();
        assert!(!manager.rtk_needs_install());
        assert!(!manager.ensure_rtk_current().unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn receipt_requires_atomic_rebuild_below_floor() {
        // Floor raised to 0.20.0 in 0.4.0: upstream 0.20.x switched
        // headroom-ai to a maturin/Rust-native single-wheel build (upstream
        // #355). The 0.10.x–0.19.x cohort was built against the old
        // py3-none-any wheel with no `headroom_core` `.so`; an in-place
        // upgrade onto the new native wheel would layer a fresh extension
        // on top of stale transitive native pins, which is the exact
        // segfault-on-import pattern this floor exists to prevent.
        assert_eq!(ATOMIC_REBUILD_FLOOR_VERSION, (0, 20, 0));

        // Pre-floor: every desktop shipment up to and including 0.3.x
        // (which bundled headroom-ai 0.19.0). Both the original 0.8.2
        // fallback cohort and the 0.10.x → 0.19.x cohort now fall here.
        assert!(receipt_requires_atomic_rebuild("0.5.18"));
        assert!(receipt_requires_atomic_rebuild("0.8.2"));
        assert!(receipt_requires_atomic_rebuild("0.9.7"));
        assert!(receipt_requires_atomic_rebuild("0.10.4"));
        assert!(receipt_requires_atomic_rebuild("0.10.12"));
        assert!(receipt_requires_atomic_rebuild("0.19.0"));

        // At-or-above the floor: in-place is allowed (0.20.x cohort + future).
        assert!(!receipt_requires_atomic_rebuild("0.20.0"));
        assert!(!receipt_requires_atomic_rebuild("0.21.39"));
        assert!(!receipt_requires_atomic_rebuild("1.0.0"));

        // Pre-release suffixes don't change the comparison.
        assert!(!receipt_requires_atomic_rebuild("0.20.0-rc.1"));
        assert!(receipt_requires_atomic_rebuild("0.19.99-rc.1"));

        // Unparseable receipts are treated as too-old (conservative).
        assert!(receipt_requires_atomic_rebuild(""));
        assert!(receipt_requires_atomic_rebuild("garbage"));
    }

    #[test]
    fn pip_output_capture_keeps_last_n_lines() {
        let mut cap = PipOutputCapture::new(3);
        cap.push("first");
        cap.push("second");
        cap.push("third");
        // Buffer is now full; the next push must evict "first".
        cap.push("fourth");
        cap.push("fifth");
        let out = cap.into_string();
        // We keep the LAST 3 lines because the tail (warnings, "Successfully
        // installed", "Skipping X") is the diagnostically interesting part.
        assert_eq!(out, "third\nfourth\nfifth");
    }

    #[test]
    fn pip_output_capture_handles_empty_and_partial_fill() {
        let cap = PipOutputCapture::new(10);
        assert_eq!(cap.into_string(), "");

        let mut cap = PipOutputCapture::new(10);
        cap.push("only line");
        assert_eq!(cap.into_string(), "only line");

        let mut cap = PipOutputCapture::new(10);
        cap.push("a");
        cap.push("b");
        assert_eq!(cap.into_string(), "a\nb");
    }

    #[test]
    fn claude_json_write_preserves_existing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude.json");
        fs::write(
            &path,
            r#"{"oauthAccount":{"id":"abc"},"projects":{"/x":{}}}"#,
        )
        .unwrap();

        super::write_headroom_to_claude_json_at(&path, Path::new("/bin/headroom"), "http://p")
            .unwrap();

        let after: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(after["oauthAccount"]["id"], "abc");
        assert!(after["projects"]["/x"].is_object());
        assert_eq!(
            after["mcpServers"]["headroom_local_community"]["command"],
            "/bin/headroom"
        );
    }

    #[test]
    fn claude_json_write_refuses_to_clobber_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude.json");
        fs::write(&path, r#"{"oauthAccount":{"id":"ab"#).unwrap(); // truncated mid-write

        let err =
            super::write_headroom_to_claude_json_at(&path, Path::new("/bin/headroom"), "http://p")
                .unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"));
        // Original bytes untouched.
        assert_eq!(fs::read(&path).unwrap(), br#"{"oauthAccount":{"id":"ab"#);
    }

    #[test]
    fn claude_json_write_treats_empty_file_as_fresh_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude.json");
        fs::write(&path, "  \n").unwrap();

        super::write_headroom_to_claude_json_at(&path, Path::new("/bin/headroom"), "http://p")
            .unwrap();

        let after: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(after["mcpServers"]["headroom_local_community"].is_object());
        // Backup taken, no tmp file left behind.
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names
            .iter()
            .any(|n| n.contains(".headroom-local-community-backup-")));
        assert!(!names
            .iter()
            .any(|n| n.ends_with(".headroom-local-community-tmp")));
    }

    fn pip_failure(stderr: &str) -> anyhow::Error {
        anyhow::Error::new(CommandFailure {
            program: "python".into(),
            args: vec!["-m".into(), "pip".into(), "install".into()],
            stdout: String::new(),
            stderr: stderr.into(),
            exit_code: Some(1),
            signal: None,
        })
    }

    /// RUST-82: an install-blocking venv failure that reached triage with no
    /// cause in it. Two defects stacked. `python -m venv` runs ensurepip via
    /// `check_output` and re-raises only the exit status, so all Sentry ever saw
    /// was "Command '[... -m ensurepip ...]' returned non-zero exit status 1";
    /// and even once ensurepip is run directly, pip's `ERROR:` line is followed
    /// by a traceback, so the 300-byte tail kept the traceback and dropped the
    /// diagnosis. Verified against the real stderr of a failing ensurepip.
    #[test]
    fn compact_pip_failure_keeps_pips_diagnosis_ahead_of_a_traceback() {
        let stderr = concat!(
            "ERROR: Could not install packages due to an OSError: ",
            "[Errno 13] Permission denied: '/opt/venv/lib/python3.14/site-packages/pip'\n",
            "Check the permissions.\n",
            "\n",
            "Traceback (most recent call last):\n",
            "  File \"<frozen runpy>\", line 203, in _run_module_as_main\n",
            "  File \"/usr/lib/python3.14/ensurepip/__init__.py\", line 88, in _run_pip\n",
            "    return subprocess.run(cmd, check=True).returncode\n",
            "  File \"/usr/lib/python3.14/subprocess.py\", line 578, in run\n",
            "    raise CalledProcessError(retcode, process.args,\n",
            "subprocess.CalledProcessError: Command '['/opt/venv/bin/python', '-W', ",
            "'ignore::DeprecationWarning', '-c', 'import runpy...']' ",
            "returned non-zero exit status 1.\n",
        );
        let compact = compact_pip_failure(&pip_failure(stderr));
        assert!(
            compact.contains("Permission denied"),
            "pip named the cause and it must survive: {compact}"
        );
        assert_eq!(
            pip_failure_category(&compact),
            "permission",
            "a named cause must not sit in the `other` grab-bag: {compact}"
        );
    }

    /// The tail is still right when pip prints no `ERROR:` line at all, so the
    /// RUST-82 fix must not cost us the ordinary case.
    #[test]
    fn compact_pip_failure_still_tails_when_pip_named_no_error() {
        let stderr = format!(
            "{}\nno matching distribution found for onnxruntime",
            "x".repeat(400)
        );
        let compact = compact_pip_failure(&pip_failure(&stderr));
        assert!(compact.ends_with("no matching distribution found for onnxruntime"));
        assert_eq!(pip_failure_category(&compact), "no-matching-dist");
    }

    #[test]
    fn compact_pip_failure_survives_multibyte_stderr() {
        // Non-English Windows locale: the 300-byte tail offset lands mid-
        // character, which used to panic on the slice.
        let stderr = "エラー: パッケージをインストールできませんでした。".repeat(20);
        let compact = compact_pip_failure(&pip_failure(&stderr));
        assert!(compact.starts_with("exit=1; stderr tail: "));
        assert!(compact.contains("エラー"));
    }

    #[test]
    fn every_index_resolving_pip_install_is_wheels_only() {
        // A dependency built from source needs a compiler the user was never
        // asked to have, and fails minutes later with a clang/cargo error they
        // cannot act on. `PIP_ONLY_BINARY` prevents that -- but only on the
        // call sites that carry it, and these arg lists are copy-pasted across
        // nine of them. Any install that resolves from an index must be
        // wheels-only; the optional markitdown/serena addons deliberately do
        // not resolve from `--extra-index-url` and so are not covered here.
        let source = include_str!("tool_manager.rs");
        let indexed = source.matches("\"--extra-index-url\",").count();
        // Split so this needle does not count itself in the scan above.
        let only_binary = source.matches(concat!("PIP_ONLY_", "BINARY,")).count();
        assert!(indexed > 0, "sanity: expected index-resolving installs");
        assert_eq!(
            indexed, only_binary,
            "an index-resolving pip install is missing PIP_ONLY_BINARY \
             ({indexed} indexed installs vs {only_binary} wheels-only)"
        );
    }

    #[test]
    fn pip_failure_category_splits_the_buckets_triage_uses() {
        // One issue per cause class, not per stderr tail (RUST-6M/6N/6P) and
        // not one flat grab-bag (RUST-5Q).
        let cases = [
            ("exit=1; stderr tail: No module named pip", "no-pip"),
            (
                "exit=1; stderr tail: FileNotFoundError: [Errno 2] No usable temporary directory",
                "no-tempdir",
            ),
            (
                "exit=1; stderr tail: OSError: [Errno 28] No space left on device",
                "disk-full",
            ),
            ("exit=1; stderr tail: Check the permissions.", "permission"),
            (
                "exit=1; stderr tail: ERROR: Could not find a version that satisfies the \
                 requirement onnxruntime==1.27.0 (from versions: 1.23.2)\nERROR: No matching \
                 distribution found for onnxruntime==1.27.0",
                "no-matching-dist",
            ),
            (
                "exit=1; stderr tail: ERROR: Could not install packages due to an OSError: \
                 [Errno 2] No such file or directory: 'C:\\\\...\\\\INSTALLERvp0i8uew.tmp'",
                "missing-file",
            ),
            (
                "exit=1; stderr tail: ERROR: Failed building wheel for hnswlib",
                "build",
            ),
            (
                "exit=1; stderr tail: Could not fetch URL https://pypi.org/simple/",
                "network",
            ),
            ("exit=1; stderr tail: something new", "other"),
            // RUST-8K, verbatim: Windows access-denied on a Korean install.
            // Only the numeric code survives translation.
            (
                "publishing extracted python into ~\\AppData\\Local\\Headroom: \
                 액세스가 거부되었습니다. (os error 5)",
                "permission",
            ),
            // The macOS network errnos must not read as a denial: the
            // closing paren in the needle is what keeps 51 out of "permission".
            (
                "exit=1; stderr tail: error sending request (os error 51)",
                "other",
            ),
        ];
        for (compact, expected) in cases {
            assert_eq!(pip_failure_category(compact), expected, "for: {compact}");
        }
    }

    #[test]
    fn plugin_install_failure_category_splits_the_rust_6k_grab_bag() {
        // Every string below is a real RUST-6K event body. They arrived under ONE
        // fingerprint, which is why that issue could never be resolved: a resolve
        // regressed on the next sibling shape. One bucket per cause class.
        let cases = [
            (
                "Codex: command failed (exit 1): /opt/homebrew/bin/codex plugin add \
                 ponytail@ponytail\nstdout:\n\nstderr:\nError: plugin `ponytail` was not found \
                 in marketplace `ponytail`",
                "marketplace-missing",
            ),
            (
                "Codex: command failed (exit 1): /opt/homebrew/bin/codex plugin add \
                 ponytail@ponytail\nstdout:\n\nstderr:\nError: failed to load configuration\n\
                 Caused by:\n    0: ~/.codex/config.toml:646:18: duplicate key",
                "host-config-invalid",
            ),
            ("Codex: CLI not found on PATH", "cli-missing"),
            (
                "Claude Code: command failed (exit 1): ~/.local/bin/claude plugin install \
                 caveman@caveman --scope user\nstdout:\n\nstderr:\nerror: unknown option '--scope'",
                "cli-version-skew",
            ),
            (
                "Codex: command failed (exit 1): codex plugin add caveman@caveman\nstderr:\n\
                 Error: failed to load configuration\n\nCaused by:\n    0: \
                 ~/.codex/config.toml:209:12: `wire_api = \"chat\"` is no longer supported.",
                "cli-version-skew",
            ),
            ("Codex: something we have not seen", "other"),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (detail, expected) in cases {
            assert_eq!(
                plugin_install_failure_category(detail),
                expected,
                "for: {detail}"
            );
            seen.insert(expected);
        }
        assert!(
            seen.len() >= 5,
            "the five RUST-6K shapes must land in distinct buckets, got: {seen:?}"
        );
    }

    #[test]
    fn host_marketplace_missing_plugin_gains_a_cause_and_a_fix() {
        // The user's report: Update on Superpowers returned the bare CLI line
        // every single time. Codex prepares `openai-curated` itself, so the
        // only useful thing to say is why it is empty and what resolves it.
        let plugin = PLUGIN_ADDONS
            .iter()
            .find(|p| p.id == "superpowers")
            .expect("superpowers addon");
        let raw = anyhow::anyhow!(
            "command failed (exit 1): codex plugin add superpowers@openai-curated\n\
             stdout:\n\nstderr:\nError: plugin `superpowers` was not found in marketplace \
             `openai-curated`"
        );

        let explained = explain_host_marketplace_missing_plugin(plugin, raw);
        let message = explained.to_string();
        assert!(
            message.contains("has not finished preparing its built-in `openai-curated` marketplace"),
            "the cause must be named, got: {message}"
        );
        assert!(
            message.contains("Open the Codex app once"),
            "the fix must be actionable, got: {message}"
        );

        // The CLI text has to survive the rewrite: it is the needle the Sentry
        // category matches on, so bucketing must not change.
        let chained = format!(
            "{:#}",
            explain_host_marketplace_missing_plugin(
                plugin,
                anyhow::anyhow!(
                    "stderr:\nError: plugin `superpowers` was not found in marketplace `openai-curated`"
                ),
            )
        );
        assert!(
            chained.contains("not found in marketplace"),
            "raw CLI text must stay in the chain, got: {chained}"
        );
        assert_eq!(plugin_install_failure_category(&chained), "marketplace-missing");

        // Unrelated failures from the same addon pass through untouched --
        // otherwise a network error would tell the user to open Codex.
        let other = explain_host_marketplace_missing_plugin(
            plugin,
            anyhow::anyhow!("network is unreachable"),
        );
        assert!(
            !other.to_string().contains("Open the Codex app once"),
            "only the marketplace-miss shape is rewritten, got: {other}"
        );
        // And it is scoped to the addon whose marketplace the host owns.
        let ponytail = PLUGIN_ADDONS
            .iter()
            .find(|p| p.id == "ponytail")
            .expect("ponytail addon");
        let untouched = explain_host_marketplace_missing_plugin(
            ponytail,
            anyhow::anyhow!("Error: plugin `ponytail` was not found in marketplace `ponytail`"),
        );
        assert!(
            !untouched.to_string().contains("Open the Codex app once"),
            "Headroom registers ponytail's marketplace itself, so it keeps the raw error, got: {untouched}"
        );
    }

    #[test]
    fn retry_fs_survives_a_transient_denial() {
        // RUST-8K: Windows kept the old python dir alive after remove_dir_all
        // and denied the rename onto it. The op succeeds once the handle
        // closes, so the bootstrap must not dead-end on the first refusal.
        let attempts = Cell::new(0u32);
        let result = ToolManager::retry_fs("unit", || {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                Ok(attempts.get())
            }
        });
        assert_eq!(result.expect("third attempt succeeds"), 3);
        assert_eq!(
            attempts.get(),
            3,
            "must retry, not give up on the first Err"
        );
    }

    #[test]
    fn retry_fs_gives_up_and_returns_the_last_error() {
        // The bound matters as much as the retry: an unbounded loop would hang
        // the bootstrap on a genuinely broken install instead of reporting it.
        let attempts = Cell::new(0u32);
        let result = ToolManager::retry_fs("unit", || {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert_eq!(
            result.expect_err("exhausted").kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(attempts.get(), 5, "bounded at ATTEMPTS");
    }

    #[test]
    fn package_update_versions_are_stable_and_specs_are_exact() {
        assert_eq!(stable_package_version("4.0.4").unwrap(), "4.0.4");
        for version in ["4.0", "4.0.4-rc.1", "latest", "v4.0.4", "4.0.4/evil"] {
            assert!(
                stable_package_version(version).is_err(),
                "accepted {version}"
            );
        }
        assert_eq!(
            context7_package_spec_for("4.0.5"),
            "@upstash/context7-mcp@4.0.5"
        );
    }
}
