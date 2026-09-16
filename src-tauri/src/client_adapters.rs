use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::models::{
    ClientConnectorStatus, ClientHealth, ClientSetupResult, ClientSetupVerification, ClientStatus,
};
use crate::storage::{app_data_dir, config_file};

// Raw proxy base — use provider-specific constants below when configuring client endpoints.
const HEADROOM_PROXY_URL: &str = "http://127.0.0.1:6867";
const HEADROOM_ANTHROPIC_BASE_URL: &str = "http://127.0.0.1:6867";

/// Every loopback port Headroom itself serves on.
///
/// 6867 is the fixed ingress clients are configured for, 6868..=6890 is the
/// backend proxy (6868 normally, a fallback when something already holds
/// 6868), and 6891 is the Codex router. Cleanup that matched only the ingress
/// left the others behind, so a client could keep pointing at a port nothing
/// serves once Headroom stopped -- and the cc-switch reconciler really did
/// write the backend port into `~/.claude/settings.json` (reproduced live
/// 2026-09-10: the file held 6868 while teardown only recognised 6867, so the
/// dead route survived every disable).
const HEADROOM_MANAGED_PORT_RANGE: std::ops::RangeInclusive<u16> = 6867..=6891;

/// The loopback port a base URL points at, when it is a URL at all.
fn loopback_url_port(value: &str) -> Option<u16> {
    let trimmed = value.trim().trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| trimmed.strip_prefix("http://localhost:"))?;
    rest.split(['/', '?', '#']).next()?.parse::<u16>().ok()
}

/// Whether a base URL points at a port Headroom owns, i.e. whether the value
/// can only have come from us. Used by teardown, which must never delete a
/// gateway URL the user configured for themselves -- only ports in
/// [`HEADROOM_MANAGED_PORT_RANGE`] qualify, and a foreign value captured
/// before setup is restored rather than removed.
fn is_headroom_managed_base_url(value: &str) -> bool {
    matches!(
        loopback_url_port(value),
        Some(port) if HEADROOM_MANAGED_PORT_RANGE.contains(&port)
    )
}
// Codex keeps its provider URL in memory for the lifetime of a session.  Use
// the detached router's stable endpoint so closing or crashing the GUI can
// switch the route to the native provider without requiring a Codex restart.
const HEADROOM_OPENAI_BASE_URL: &str = crate::codex_router::CODEX_ROUTER_OPENAI_BASE_URL;
const HEADROOM_CHATGPT_BASE_URL: &str = crate::codex_router::CODEX_ROUTER_CHATGPT_BASE_URL;
const HEADROOM_GROK_PROXY_BASE_URL: &str = "http://127.0.0.1:6867/v1";
const ZSH_PROFILE_FILE: &str = ".zprofile";
const ZSH_RC_FILE: &str = ".zshrc";
const BASH_PROFILE_FILE: &str = ".bash_profile";
const BASH_LOGIN_FILE: &str = ".bash_login";
const POSIX_PROFILE_FILE: &str = ".profile";
const BASH_RC_FILE: &str = ".bashrc";
const ALL_SHELL_FILES: [&str; 6] = [
    ZSH_PROFILE_FILE,
    ZSH_RC_FILE,
    BASH_PROFILE_FILE,
    BASH_LOGIN_FILE,
    POSIX_PROFILE_FILE,
    BASH_RC_FILE,
];

/// Select the stable Codex router URL that matches the credential Codex will
/// use. ChatGPT OAuth needs the native `/backend-api/codex` suffix so Codex
/// enables its account/search routes; API-key auth uses the public `/v1`
/// compatibility route.
fn headroom_codex_base_url() -> &'static str {
    crate::codex_router::codex_router_base_url(codex_uses_chatgpt_auth())
}

#[derive(Debug, Clone, Copy)]
struct ManagedClientSpec {
    id: &'static str,
    name: &'static str,
}

const MANAGED_CLIENT_SPECS: [ManagedClientSpec; 5] = [
    ManagedClientSpec {
        id: "claude_code",
        name: "Claude Code",
    },
    ManagedClientSpec {
        id: "codex",
        name: "Codex",
    },
    ManagedClientSpec {
        id: "grok_build",
        name: "Grok Build",
    },
    ManagedClientSpec {
        id: "opencode",
        name: "OpenCode",
    },
    ManagedClientSpec {
        id: "zcode",
        name: "ZCode",
    },
];

const CODEX_DESKTOP_EXECUTABLE_SUFFIXES: [&str; 2] = [
    "/ChatGPT.app/Contents/MacOS/ChatGPT",
    "/Codex.app/Contents/MacOS/Codex",
];
#[cfg(target_os = "macos")]
const CODEX_DESKTOP_BUNDLE_ID: &str = "com.openai.codex";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellFamily {
    Zsh,
    Bash,
    Posix,
}

pub fn detect_clients() -> Vec<ClientStatus> {
    let setup_state = load_setup_state();

    vec![
        detect_claude_code_client(is_configured(&setup_state, "claude_code")),
        detect_codex_client(is_configured(&setup_state, "codex")),
        detect_grok_build_client(is_configured(&setup_state, "grok_build")),
        detect_opencode_client(is_configured(&setup_state, "opencode")),
        detect_zcode_client(is_configured(&setup_state, "zcode")),
    ]
}

pub fn ensure_rtk_integrations(
    managed_rtk_path: &Path,
    managed_python_path: &Path,
) -> Result<(Vec<String>, Vec<String>)> {
    ensure_rtk_integrations_for_targets(
        managed_rtk_path,
        managed_python_path,
        &resolve_default_shell_targets(),
    )
}

fn ensure_rtk_integrations_for_targets(
    managed_rtk_path: &Path,
    managed_python_path: &Path,
    shell_targets: &[PathBuf],
) -> Result<(Vec<String>, Vec<String>)> {
    // Respect the user's opt-out so bootstrap, restore, and client setup don't
    // silently re-add the PATH export and Claude Code hook after they've been
    // turned off via the tool status toggle. Also skip when the binary is absent
    // (not installed / uninstalled) so we never write integrations pointing at a
    // missing rtk.
    if is_rtk_disabled() || !managed_rtk_path.exists() {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut changed_files = Vec::new();
    let mut backup_files = Vec::new();

    let mut path_updates = ensure_managed_rtk_on_path(managed_rtk_path, shell_targets)?;
    let mut hook_updates = ensure_claude_code_rtk_hook(managed_rtk_path, managed_python_path)?;
    changed_files.append(&mut path_updates.0);
    backup_files.append(&mut path_updates.1);
    changed_files.append(&mut hook_updates.0);
    backup_files.append(&mut hook_updates.1);

    // Codex has no PreToolUse-style hook, so the auto-rewrite can't be wired the
    // way it is for Claude Code. Mirror the MarkItDown approach: drop a managed
    // `~/.codex/AGENTS.md` nudge telling Codex to route shell commands through
    // the managed `rtk` binary (which is already on PATH via the block above).
    if is_codex_enabled() {
        let agents = rtk_codex_agents_path();
        let (codex_changed, codex_backup) =
            upsert_managed_block(&agents, "rtk", &build_rtk_codex_nudge(managed_rtk_path))?;
        if codex_changed {
            changed_files.push(agents.display().to_string());
        }
        if let Some(path) = codex_backup {
            backup_files.push(path.display().to_string());
        }
    }

    Ok((changed_files, backup_files))
}

fn rtk_codex_agents_path() -> PathBuf {
    codex_home().join("AGENTS.md")
}

/// Template catalog shared by installation and instruction audit. Only existing
/// managed blocks are eligible for refresh; auditing never enables an addon.
pub(crate) fn instruction_targets() -> Vec<(String, PathBuf, Vec<(String, String)>)> {
    let converter = default_headroom_root_dir().join("bin").join(if cfg!(windows) {
        "markitdown.cmd"
    } else {
        "markitdown"
    });
    vec![
        ("codex".into(), rtk_codex_agents_path(), vec![
            ("rtk".into(), build_rtk_codex_nudge(&default_headroom_rtk_path())),
            ("markitdown".into(), build_markitdown_codex_nudge(&converter)),
            ("serena".into(), build_serena_usage_nudge().into()),
        ]),
        ("claude".into(), markitdown_claude_md_path(), vec![
            ("markitdown_office".into(), build_markitdown_office_nudge(&converter)),
            ("serena".into(), build_serena_usage_nudge().into()),
        ]),
    ]
}

/// Codex nudge: Codex has no command-rewrite hook, so it routes shell commands
/// through the managed `rtk` binary by being told to prefix them with it.
fn build_rtk_codex_nudge(managed_rtk_path: &Path) -> String {
    let bin = format!("\"{}\"", shell_double_quote(&managed_rtk_path.to_string_lossy()));
    format!(
        "## Token-saving shell commands (Headroom RTK)\n\
         Follow the system/developer/user instructions and the active project's\n\
         `AGENTS.md`, `CLAUDE.md`, and skills first; this note only fills gaps.\n\
         When the managed RTK binary is available, use it for compact status, build and test output.\n\
         Prefix each applicable command segment, for example `{bin} git status`.\n\
         Use raw commands when you need verbatim code, complete diagnostics, machine-readable output,\n\
         or `git diff --check` whitespace reports. Compression can omit relevant details.\n\
         If RTK is unavailable or fails, report the issue and use the raw command."
    )
}

pub fn rtk_integration_status() -> Result<(bool, bool)> {
    let path_configured = shell_block_contains_text_in_files(
        &resolve_default_shell_targets(),
        "managed_rtk",
        "export PATH=",
    )?;
    let hook_configured = claude_settings_hook_matches("headroom-local-community-rtk-rewrite.sh")?
        && headroom_rtk_hook_path().exists();
    Ok((path_configured, hook_configured))
}

/// True when the user turned RTK off via the tool status toggle.
pub fn is_rtk_disabled() -> bool {
    load_setup_state().rtk_disabled
}

/// True when the user turned auto-learning off in the Optimize view. The proxy
/// then runs without the passive traffic-learning flags; manual Learn scans are
/// unaffected.
pub fn is_auto_learn_disabled() -> bool {
    load_setup_state().auto_learn_disabled
}

/// Persist the auto-learning opt-out. The proxy reads this flag when it starts;
/// callers must not stop a live proxy here because doing so can drop an
/// in-flight Codex or Claude request. The next normal proxy start picks it up.
pub fn set_auto_learn_enabled(enabled: bool) -> Result<()> {
    let mut state = load_setup_state();
    state.auto_learn_disabled = !enabled;
    write_setup_state(&mut state)
}

/// Enable or disable RTK from the tool status toggle. Disabling tears down the
/// RTK PATH export, the Claude Code hook, and the Codex AGENTS.md nudge (without
/// touching `ANTHROPIC_BASE_URL` routing) and persists the opt-out so bootstrap
/// won't re-add them. Enabling clears the flag and re-applies the integrations.
pub fn set_rtk_enabled(
    enabled: bool,
    managed_rtk_path: &Path,
    managed_python_path: &Path,
) -> Result<()> {
    let mut state = load_setup_state();
    state.rtk_disabled = !enabled;
    write_setup_state(&mut state)?;

    if enabled {
        ensure_rtk_integrations(managed_rtk_path, managed_python_path)?;
    } else {
        let shell_targets = resolve_client_shell_targets_for_cleanup(&state, "claude_code")?;
        remove_shell_block(&shell_targets, "managed_rtk")?;
        let mut failures = Vec::new();
        for settings_path in claude_settings_candidates() {
            if let Err(err) = strip_headroom_hook_from_settings(&settings_path) {
                failures.push(format!("{}: {err:#}", settings_path.display()));
            }
        }
        let hook_path = headroom_rtk_hook_path();
        if hook_path.exists() {
            if let Err(err) = std::fs::remove_file(&hook_path) {
                failures.push(format!("{}: {err}", hook_path.display()));
            }
        }
        if let Err(err) = remove_managed_block(&rtk_codex_agents_path(), "rtk") {
            failures.push(format!("{}: {err:#}", rtk_codex_agents_path().display()));
        }
        if !failures.is_empty() {
            return Err(anyhow!(
                "disabling RTK integrations was incomplete: {}",
                failures.join("; ")
            ));
        }
    }

    Ok(())
}

/// True when the error chain contains a filesystem PermissionDenied (os error 13),
/// i.e. an unwritable target -- an environment issue, not an app bug.
pub fn is_permission_denied(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

/// Raw OS codes for a full disk. ErrorKind::StorageFull isn't stable, so match
/// the platform's codes: ENOSPC (28) on macOS/Linux; ERROR_HANDLE_DISK_FULL (39)
/// and ERROR_DISK_FULL (112) on Windows.
#[cfg(unix)]
const NO_SPACE_OS_ERRORS: &[i32] = &[28];
#[cfg(windows)]
const NO_SPACE_OS_ERRORS: &[i32] = &[39, 112];

/// True when the error chain contains a filesystem "no space left on device" --
/// a full disk, an environment issue not an app bug, same class as
/// `is_permission_denied`.
pub fn is_no_space(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            io.raw_os_error()
                .is_some_and(|code| NO_SPACE_OS_ERRORS.contains(&code))
        })
    })
}

/// True when the error chain contains an io InvalidData -- in practice a
/// `read_to_string` on a file that isn't valid UTF-8 (RUST-5X: a latin-1
/// ~/.bashrc). Rewriting such a file would mangle the user's own bytes, so the
/// step that wanted to rewrite it is skipped, same class as a locked file.
pub fn is_invalid_utf8(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::InvalidData)
    })
}

/// Runs a shell-profile write step, tolerating a profile we can't safely rewrite
/// (a read-only ~/.zshrc -> os error 13, or a non-UTF-8 ~/.bashrc). The env that
/// actually routes a client lives in app-owned config (~/.claude/settings.json,
/// ~/.codex/config.toml), so an untouchable shell file costs terminal
/// convenience, not core routing. Returns `Ok(None)` when the step was skipped
/// for that reason.
fn shell_step_best_effort(
    step: Result<(Vec<String>, Vec<String>)>,
) -> Result<Option<(Vec<String>, Vec<String>)>> {
    match step {
        Ok(updates) => Ok(Some(updates)),
        Err(err) if is_permission_denied(&err) || is_invalid_utf8(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

fn official_headroom_routing_conflict() -> Option<PathBuf> {
    let mut paths = all_shell_paths();
    paths.extend(claude_settings_candidates());
    paths.push(codex_config_toml_path());
    paths.push(grok_config_toml_path());
    paths.push(opencode_config_path());

    paths.into_iter().find(|path| {
        std::fs::read_to_string(path).ok().is_some_and(|text| {
            text.contains("# >>> headroom:")
                || text.contains("# <<< headroom:")
                || text.contains("[model_providers.headroom]")
                || text.contains("model_provider = \"headroom\"")
                || text.contains("127.0.0.1:6767")
                || text.contains("127.0.0.1:6768")
                || text.contains("127.0.0.1:6769")
                || text.contains("127.0.0.1:6790")
        })
    })
}

pub fn apply_client_setup(client_id: &str) -> Result<ClientSetupResult> {
    apply_client_setup_with_options(client_id, false)
}

/// `allow_takeover` is set only after the user explicitly confirms replacing an
/// existing custom Codex `model_provider` (for example Cockpit Tools'
/// `codex_local_access`). The displaced provider is recorded and restored on
/// disable; without the flag Headroom still refuses to clobber an unknown one.
pub fn apply_client_setup_with_options(
    client_id: &str,
    allow_takeover: bool,
) -> Result<ClientSetupResult> {
    if crate::edition::LOCAL_COMMUNITY {
        // Enabling Codex is an explicit takeover action. Its renderer preserves
        // the official provider table for rollback while replacing only the
        // root routing block. Other connectors still refuse an implicit
        // takeover because their configuration formats do not yet provide the
        // same reversible migration.
        let codex_takeover = normalized_setup_id(client_id) == "codex_cli";
        if !codex_takeover {
            if let Some(path) = official_headroom_routing_conflict() {
                return Err(anyhow!(
                    "Official Headroom routing is active in {}. Pause that connector in the paid app before enabling Headroom Local Community; Community will not overwrite or remove the official configuration.",
                    path.display()
                ));
            }
        }
    }
    let first = apply_client_setup_once(client_id, allow_takeover)?;
    if first.verification.verified {
        return Ok(first);
    }
    // Apply-ok-but-verify-miss is a lost-update race (RUST-3W): a concurrent
    // read-modify-write on the same file — the MCP registrar re-run at boot
    // rewrites ~/.codex/config.toml non-atomically, Codex itself rewrites it on
    // exit — can clobber a just-written block before verification reads it
    // back. Re-apply once; a persistent failure still returns unverified and
    // reaches Sentry.
    let mut second = apply_client_setup_once(client_id, allow_takeover)?;
    for file in first.changed_files {
        if !second.changed_files.contains(&file) {
            second.changed_files.push(file);
        }
    }
    for file in first.backup_files {
        if !second.backup_files.contains(&file) {
            second.backup_files.push(file);
        }
    }
    second.already_configured &= first.already_configured;
    Ok(second)
}

fn apply_client_setup_once(client_id: &str, allow_takeover: bool) -> Result<ClientSetupResult> {
    let mut changed_files = Vec::new();
    let mut backup_files = Vec::new();
    let mut state = load_setup_state();
    let state_id = normalized_setup_id(client_id).to_string();
    let mut shell_unwritable = false;
    let mut replaced_base_url = None;

    match client_id {
        "claude_code" => {
            let shell_targets = resolve_client_shell_targets(&state, client_id)?;
            // Critical, app-owned writes first: the ~/.claude/settings.json env is
            // what actually routes Claude Code through Headroom. Do it before the
            // shell profile so a locked ~/.zshrc can't block core setup.
            let (changed, backups, replaced) =
                configure_claude_settings_env("ANTHROPIC_BASE_URL", HEADROOM_ANTHROPIC_BASE_URL)?;
            let mut updates = (changed, backups);
            if let Some(original) = replaced {
                // A custom gateway/proxy URL was routing Claude before us:
                // remember it for restore-on-disable and tell the caller so
                // the UI can inform the user their routing changed.
                state
                    .preserved_base_urls
                    .insert(state_id.clone(), original.clone());
                replaced_base_url = Some(original);
            }
            let mut legacy_updates = remove_legacy_vscode_base_url_keys()?;
            updates.0.append(&mut legacy_updates.0);
            updates.1.append(&mut legacy_updates.1);

            // Loud-fail guard so a closed app or lost ANTHROPIC_BASE_URL routing
            // surfaces in Claude instead of silently hitting Anthropic directly.
            let mut guard = ensure_claude_guard_hook()?;
            updates.0.append(&mut guard.0);
            updates.1.append(&mut guard.1);

            // Shell profile (RTK PATH + env export) is convenience; tolerate an
            // unwritable profile rather than failing the whole setup.
            let env_block = format!("export ANTHROPIC_BASE_URL={}", HEADROOM_ANTHROPIC_BASE_URL);
            let shell_step = ensure_rtk_integrations_for_targets(
                &default_headroom_rtk_path(),
                &default_headroom_managed_python_path(),
                &shell_targets,
            )
            .and_then(|mut rtk| {
                let mut env = configure_shell_block(&shell_targets, "claude_code", &env_block)?;
                rtk.0.append(&mut env.0);
                rtk.1.append(&mut env.1);
                Ok(rtk)
            });
            match shell_step_best_effort(shell_step)? {
                Some(mut shell) => {
                    updates.0.append(&mut shell.0);
                    updates.1.append(&mut shell.1);
                }
                None => shell_unwritable = true,
            }

            changed_files.extend(updates.0);
            backup_files.extend(updates.1);
            state
                .managed_shell_files
                .insert(state_id.clone(), serialize_paths(&shell_targets));
        }
        "vscode" => {
            let (changed, backups, replaced) = configure_vscode_settings()?;
            changed_files.extend(changed);
            backup_files.extend(backups);
            if let Some(original) = replaced {
                state
                    .preserved_base_urls
                    .insert(state_id.clone(), original.clone());
                replaced_base_url = Some(original);
            }
        }
        "codex" | "codex_cli" => {
            let shell_targets = resolve_client_shell_targets(&state, client_id)?;
            // Critical, app-owned write first: the ~/.codex/config.toml provider
            // block is what routes Codex through Headroom.
            let (changed, backups, preserved_provider) =
                configure_codex_provider_block(allow_takeover)?;
            let mut updates = (changed, backups);
            if let Some(original) = preserved_provider {
                // A custom root `model_provider` (gateway/alternate provider) was
                // routing Codex before us: remember it for restore-on-disable so
                // we don't silently drop the user onto api.openai.com. Restored
                // silently (no takeover notice — that copy is Claude/base_url
                // specific).
                state.preserved_base_urls.insert(state_id.clone(), original);
            }

            // Loud-fail guard so a closed app or clobbered config surfaces in
            // Codex instead of silently routing direct to OpenAI.
            let mut guard = ensure_codex_guard_hook()?;
            updates.0.append(&mut guard.0);
            updates.1.append(&mut guard.1);

            let env_block = format!("export OPENAI_BASE_URL={}", headroom_codex_base_url());
            match shell_step_best_effort(configure_shell_block(
                &shell_targets,
                "codex_cli",
                &env_block,
            ))? {
                Some(mut shell) => {
                    updates.0.append(&mut shell.0);
                    updates.1.append(&mut shell.1);
                }
                None => shell_unwritable = true,
            }
            changed_files.extend(updates.0);
            backup_files.extend(updates.1);
            state
                .managed_shell_files
                .insert(state_id.clone(), serialize_paths(&shell_targets));
            // Pull existing native threads into the headroom-provider menu so the
            // Codex history list stays whole once it routes through Headroom.
            retag_codex_thread_providers(CODEX_NATIVE_PROVIDER, CODEX_HEADROOM_PROVIDER);
        }
        "grok_build" => {
            let shell_targets = resolve_client_shell_targets(&state, client_id)?;
            let mut updates = configure_grok_proxy_block()?;
            let env_block = format!(
                "export GROK_CLI_CHAT_PROXY_BASE_URL={}",
                HEADROOM_GROK_PROXY_BASE_URL
            );
            match shell_step_best_effort(configure_shell_block(
                &shell_targets,
                "grok_build",
                &env_block,
            ))? {
                Some(mut shell) => {
                    updates.0.append(&mut shell.0);
                    updates.1.append(&mut shell.1);
                }
                None => shell_unwritable = true,
            }
            changed_files.extend(updates.0);
            backup_files.extend(updates.1);
            state
                .managed_shell_files
                .insert(state_id.clone(), serialize_paths(&shell_targets));
        }
        "opencode" => {
            // Config-file routing only: OpenCode reads provider base URLs from
            // opencode.json(c); no env vars or shell blocks are involved.
            let updates = configure_opencode_provider_block(&mut state)?;
            changed_files.extend(updates.0);
            backup_files.extend(updates.1);
        }
        "zcode" => {
            // Plugin-line integration only: register the Headroom MCP server
            // in ZCode's user config. No base URLs, no shell blocks, no
            // provider or account state.
            let updates = configure_zcode_mcp_entry()?;
            changed_files.extend(updates.0);
            backup_files.extend(updates.1);
        }
        other => return Err(anyhow!("Automatic setup is not supported yet for {other}.",)),
    }

    let configured_at = Utc::now().to_rfc3339();
    state.configured_clients.insert(state_id, configured_at);
    write_setup_state(&mut state)?;

    let already_configured = changed_files.is_empty();
    let summary = if already_configured {
        "Client was already configured for Headroom.".to_string()
    } else if normalized_setup_id(client_id) == "zcode" {
        "Headroom MCP tools are configured in ZCode.".to_string()
    } else {
        "Client configuration updated to route through Headroom.".to_string()
    };

    let verification = verify_client_setup(client_id)?;

    Ok(ClientSetupResult {
        client_id: client_id.to_string(),
        applied: true,
        already_configured,
        summary,
        changed_files,
        backup_files,
        next_steps: if normalized_setup_id(client_id) == "zcode" {
            vec!["Restart ZCode and check that the Headroom MCP tools are available.".into()]
        } else {
            let mut steps = Vec::new();
            if shell_unwritable {
                steps.push(
                    "Your shell profile (e.g. ~/.zshrc) couldn't be updated - it isn't writable, or it isn't valid UTF-8 text. Core routing still works via the client's own config; to launch the client from a terminal, fix the file and re-run setup, or add the export manually."
                        .into(),
                );
            }
            steps.push(
                "Restart your terminal/editor session to pick up environment changes.".into(),
            );
            if normalized_setup_id(client_id) == "codex_cli" {
                steps.push(
                    "After first setup, quit and reopen any Codex desktop, CLI, or IDE session once to load the stable route; later Headroom pauses or crashes do not require a Codex restart."
                        .into(),
                );
                steps.push(
                    "In Codex, run /hooks and trust the Headroom routing guard so it can warn you if routing breaks (re-trust if Headroom updates the guard)."
                        .into(),
                );
            }
            steps.push(format!(
                "Run one {} prompt and verify activity appears in Headroom.",
                match normalized_setup_id(client_id) {
                    "codex_cli" => "Codex",
                    "grok_build" => "Grok Build",
                    "opencode" => "OpenCode",
                    "zcode" => "ZCode",
                    _ => "Claude Code",
                }
            ));
            steps
        },
        verification,
        shell_profile_unwritable: shell_unwritable,
        replaced_base_url,
    })
}

pub fn verify_client_setup(client_id: &str) -> Result<ClientSetupVerification> {
    let mut checks = Vec::new();
    let mut failures = Vec::new();
    let mut foreign_provider: Option<String> = None;

    match client_id {
        "claude_code" => {
            let state = load_setup_state();
            let shell_targets = resolve_client_shell_targets(&state, client_id)?;
            let shell_ok = shell_block_contains_in_files(
                &shell_targets,
                "claude_code",
                "ANTHROPIC_BASE_URL",
                HEADROOM_ANTHROPIC_BASE_URL,
            )?;
            let rtk_path_ok =
                shell_block_contains_text_in_files(&shell_targets, "managed_rtk", "export PATH=")?;
            let claude_settings_ok =
                claude_settings_env_matches("ANTHROPIC_BASE_URL", HEADROOM_ANTHROPIC_BASE_URL)?;
            let rtk_hook_ok =
                claude_settings_hook_matches("headroom-local-community-rtk-rewrite.sh")?
                    && headroom_rtk_hook_path().exists();

            if shell_ok {
                checks.push(
                    "Found Claude Code ANTHROPIC_BASE_URL export in managed shell block.".into(),
                );
            }
            if rtk_path_ok {
                checks.push("Found Headroom-managed RTK PATH export in shell profiles.".into());
            }
            if claude_settings_ok {
                checks.push(
                    "Found ~/.claude/settings.json env.ANTHROPIC_BASE_URL pointing to Headroom."
                        .into(),
                );
            }
            if rtk_hook_ok {
                checks.push(
                    "Found Headroom-managed RTK Claude hook in ~/.claude/settings.json.".into(),
                );
            }
            if !shell_ok && !claude_settings_ok {
                failures.push(
                    "Claude Code ANTHROPIC_BASE_URL was not found in shell blocks or ~/.claude/settings.json."
                        .into(),
                );
            }
            // RTK is a separate, opt-in integration (`set_rtk_enabled` tears it
            // down without touching ANTHROPIC_BASE_URL routing). Its wiring is
            // only ever added when the managed binary exists on disk (see
            // `ensure_rtk_integrations_for_targets`), so its absence must not
            // fail Claude Code verification when RTK isn't installed or the user
            // disabled it — routing is what "connected" means here.
            let rtk_required = !state.rtk_disabled && default_headroom_rtk_path().exists();
            if rtk_required && !rtk_path_ok {
                failures.push(
                    "Headroom-managed RTK PATH export was not found in shell profiles.".into(),
                );
            }
            if rtk_required && !rtk_hook_ok {
                failures.push(
                    "Headroom-managed RTK Claude hook was not found in ~/.claude/settings.json."
                        .into(),
                );
            }

            if claude_guard_hook_path().exists() && claude_guard_registered()? {
                checks.push(
                    "Found Headroom routing guard registered in ~/.claude/settings.json.".into(),
                );
            } else {
                failures.push(
                    "Headroom routing guard was not found in ~/.claude/settings.json.".into(),
                );
            }
        }
        "vscode" => {
            let mut delegated = verify_client_setup("claude_code")?;
            delegated.client_id = "vscode".to_string();
            return Ok(delegated);
        }
        "codex" | "codex_cli" => {
            let state = load_setup_state();
            let shell_targets = resolve_client_shell_targets(&state, client_id)?;
            let shell_ok = shell_block_contains_in_files(
                &shell_targets,
                "codex_cli",
                "OPENAI_BASE_URL",
                headroom_codex_base_url(),
            )?;
            let toml_ok = codex_provider_block_matches()?;

            if shell_ok {
                checks.push("Found Codex OPENAI_BASE_URL export in managed shell block.".into());
            }
            if toml_ok {
                checks
                    .push("Found Headroom-managed provider block in ~/.codex/config.toml.".into());
            }
            if !toml_ok {
                // A different tool may legitimately own Codex routing through
                // its own root `model_provider`. Headroom deliberately refuses
                // to clobber an unknown provider, so that is a coexist state,
                // not a broken setup: name the current owner instead of
                // reporting a missing block.
                match codex_external_provider() {
                    Some(provider) => {
                        // Neither a pass nor a failure: Headroom is deliberately
                        // standing aside. The UI renders this from the structured
                        // field as a neutral note.
                        foreign_provider = Some(provider);
                    }
                    None => failures.push(
                        "Headroom-managed provider block was not found in ~/.codex/config.toml."
                            .into(),
                    ),
                }
            }
            // Shell export is convenience, not routing: config.toml is what routes
            // Codex (apply tolerates an unwritable shell profile). A missing export
            // must not fail verification -- mirrors Claude, which only fails when
            // *no* routing source is present.

            if codex_guard_hook_path().exists() && codex_guard_registered()? {
                checks
                    .push("Found Headroom routing guard registered in ~/.codex/hooks.json.".into());
            } else {
                failures
                    .push("Headroom routing guard was not found in ~/.codex/hooks.json.".into());
            }

            // Independent confirmation from Codex itself, run off-thread: the
            // `codex doctor` call takes seconds and `verify` is awaited by the
            // setup UI, so block it there and we stall the flow. Detached and
            // logged (its output isn't surfaced in the result today); never a
            // `verified` failure (doctor can flag unrelated issues, and an
            // untrusted-but-installed guard is expected until the user runs
            // /hooks).
            std::thread::spawn(|| {
                if let Some(summary) = codex_doctor_summary() {
                    log::info!("codex doctor: {summary}");
                }
            });
        }
        "grok_build" => {
            let state = load_setup_state();
            let shell_targets = resolve_client_shell_targets(&state, client_id)?;
            let shell_ok = shell_block_contains_in_files(
                &shell_targets,
                "grok_build",
                "GROK_CLI_CHAT_PROXY_BASE_URL",
                HEADROOM_GROK_PROXY_BASE_URL,
            )?;
            let toml_ok = grok_proxy_block_matches()?;

            if shell_ok {
                checks.push(
                    "Found Grok Build GROK_CLI_CHAT_PROXY_BASE_URL export in managed shell block."
                        .into(),
                );
            }
            if toml_ok {
                checks.push("Found Headroom-managed proxy block in ~/.grok/config.toml.".into());
            }
            if !toml_ok {
                failures.push(
                    "Headroom-managed proxy block was not found in ~/.grok/config.toml.".into(),
                );
            }
            if !shell_ok {
                failures.push(
                    "Grok Build GROK_CLI_CHAT_PROXY_BASE_URL export was not found in shell profiles."
                        .into(),
                );
            }
        }
        "opencode" => {
            if opencode_provider_block_matches()? {
                checks.push(
                    "Found Headroom proxy base URLs for the anthropic and openai providers in OpenCode's config."
                        .into(),
                );
            } else {
                failures.push(
                    "Headroom proxy base URLs were not found for the anthropic and openai providers in OpenCode's config."
                        .into(),
                );
            }
        }
        "zcode" => {
            if zcode_mcp_entry_matches()? {
                checks.push(
                    "Found the Headroom MCP server entry in ~/.zcode/cli/config.json.".into(),
                );
            } else {
                failures.push(
                    "The Headroom MCP server entry was not found in ~/.zcode/cli/config.json."
                        .into(),
                );
            }
        }
        other => return Err(anyhow!("Verification is not supported yet for {other}.",)),
    }

    // Proxy reachability is transient runtime state — the runtime warm-up
    // can finish after this verification runs. Surface it via the
    // `proxy_reachable` field, but don't fail `verified` on it. `verified`
    // attests only to "we wrote everything we needed to write".
    // Reuse the status/watchdog probe so setup verification does not report
    // "proxy not answering" while the proxy is merely busy or returning a
    // transient non-routing readiness warning.
    let proxy_reachable = crate::state::headroom_proxy_reachable();
    if proxy_reachable {
        checks.push("Headroom proxy is reachable on 127.0.0.1:6867.".into());
    }

    Ok(ClientSetupVerification {
        client_id: client_id.to_string(),
        // A connector that stood aside for another provider manager has not
        // written its route, so it is not "verified" -- but it is also not a
        // failure, which is why `foreign_provider` is reported separately.
        verified: failures.is_empty() && foreign_provider.is_none(),
        proxy_reachable,
        checks,
        failures,
        foreign_provider,
    })
}

pub fn is_claude_code_enabled() -> bool {
    is_configured(&load_setup_state(), "claude_code")
}

pub fn is_codex_enabled() -> bool {
    is_configured(&load_setup_state(), "codex_cli")
}

pub fn is_grok_build_enabled() -> bool {
    is_configured(&load_setup_state(), "grok_build")
}

pub fn is_opencode_enabled() -> bool {
    is_configured(&load_setup_state(), "opencode")
}

pub fn is_zcode_enabled() -> bool {
    is_configured(&load_setup_state(), "zcode")
}

/// True when an enabled connector bills against the user's own provider keys
/// (or ChatGPT plan), so the Claude pricing gate must neither stop the Python
/// backend nor bypass the proxy for it. ZCode is exempt twice over: it keeps
/// its own provider settings, and its connector writes an MCP registration
/// rather than a route.
pub fn any_gate_exempt_client_enabled() -> bool {
    is_codex_enabled() || is_opencode_enabled() || is_grok_build_enabled() || is_zcode_enabled()
}

pub fn list_client_connectors(
    detected_clients: &[ClientStatus],
) -> Result<Vec<ClientConnectorStatus>> {
    let setup_state = load_setup_state();

    let connectors = MANAGED_CLIENT_SPECS
        .iter()
        .map(|spec| {
            let installed = detected_clients
                .iter()
                .find(|client| client.id == spec.id)
                .map(|client| client.installed)
                .unwrap_or(false);
            // Fall back to the remembered snapshot while restore_client_setups
            // is still re-applying on launch, so the connector doesn't flash
            // "disabled" during the async restore window after a restart.
            let enabled = is_configured(&setup_state, spec.id)
                || setup_state
                    .remembered_clients
                    .contains_key(normalized_setup_id(spec.id));
            // Which tool owns the Codex route is a property of the machine's
            // `~/.codex/config.toml`, not of our connector state: report it
            // while the connector is off too, or the settings toggle cannot
            // warn the user before it replaces another manager's provider (it
            // read `verification`, which is deliberately absent while off, and
            // therefore sent `allowTakeover: false` straight into the
            // backend's refusal). A single small file read -- it never drags
            // `verify_client_setup`'s blocking probes along for a disabled
            // connector.
            let foreign_provider = if spec.id == "codex" {
                codex_external_provider()
            } else {
                None
            };
            let verification = if enabled {
                verify_client_setup(spec.id).ok()
            } else {
                None
            };
            let verified = verification.as_ref().is_some_and(|result| result.verified);
            let last_configured_at = configured_timestamp(&setup_state, spec.id);
            let restart_required = if spec.id == "codex" && enabled {
                codex_desktop_restart_required(last_configured_at.as_deref())
            } else {
                None
            };

            ClientConnectorStatus {
                client_id: spec.id.to_string(),
                name: spec.name.to_string(),
                installed,
                enabled,
                verified,
                last_configured_at,
                restart_required,
                foreign_provider,
                verification,
            }
        })
        .collect();

    Ok(connectors)
}

#[derive(Debug, Clone)]
struct CodexDesktopProcess {
    pid: u32,
    started_at: DateTime<Local>,
    executable: String,
}

fn same_codex_desktop_process(left: &CodexDesktopProcess, right: &CodexDesktopProcess) -> bool {
    left.pid == right.pid
        && left.started_at.timestamp() == right.started_at.timestamp()
        && left.executable == right.executable
}

fn is_codex_desktop_executable(command: &str) -> bool {
    CODEX_DESKTOP_EXECUTABLE_SUFFIXES
        .iter()
        .any(|suffix| command.ends_with(suffix))
}

fn parse_codex_desktop_processes(output: &str) -> Vec<CodexDesktopProcess> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let weekday = fields.next()?;
            let month = fields.next()?;
            let day = fields.next()?;
            let time = fields.next()?;
            let year = fields.next()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            if !is_codex_desktop_executable(&command) {
                return None;
            }

            let started_at = NaiveDateTime::parse_from_str(
                &format!("{weekday} {month} {day} {time} {year}"),
                "%a %b %e %T %Y",
            )
            .ok()
            .and_then(|value| Local.from_local_datetime(&value).earliest())?;

            Some(CodexDesktopProcess {
                pid,
                started_at,
                executable: command,
            })
        })
        .collect()
}

fn restart_required_from_processes(
    processes: &[CodexDesktopProcess],
    configured_at: &str,
) -> Option<bool> {
    let configured_at = DateTime::parse_from_rfc3339(configured_at).ok()?;
    let configured_at = configured_at.timestamp();
    let mut same_second = false;
    for process in processes {
        match process.started_at.timestamp().cmp(&configured_at) {
            std::cmp::Ordering::Less => return Some(true),
            std::cmp::Ordering::Equal => same_second = true,
            std::cmp::Ordering::Greater => {}
        }
    }
    (!same_second).then_some(false)
}

fn codex_desktop_restart_required(configured_at: Option<&str>) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let configured_at = configured_at?;
        let processes = running_codex_desktop_processes().ok()?;
        restart_required_from_processes(&processes, configured_at)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = configured_at;
        None
    }
}

#[cfg(target_os = "macos")]
fn running_codex_desktop_processes() -> Result<Vec<CodexDesktopProcess>> {
    let output = crate::proc::command("ps")
        .env("LC_ALL", "C")
        .args(["-xww", "-o", "pid=,lstart=,comm="])
        .output()
        .context("checking whether Codex Desktop is running")?;
    if !output.status.success() {
        return Err(anyhow!(
            "checking Codex Desktop processes failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8(output.stdout).context("reading Codex Desktop process list")?;
    Ok(parse_codex_desktop_processes(&stdout))
}

#[cfg(target_os = "macos")]
fn terminate_codex_desktop_process(expected: &CodexDesktopProcess) -> Result<()> {
    if !running_codex_desktop_processes()?
        .iter()
        .any(|process| same_codex_desktop_process(process, expected))
    {
        return Ok(());
    }

    let pid = i32::try_from(expected.pid).context("Codex Desktop PID is out of range")?;
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    if result == 0 {
        return Ok(());
    }

    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error).context(format!("asking Codex Desktop process {pid} to quit"))
    }
}

#[cfg(target_os = "macos")]
fn wait_for_codex_desktop_exit(expected: &[CodexDesktopProcess]) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let processes = running_codex_desktop_processes()?;
        if !processes.iter().any(|process| {
            expected
                .iter()
                .any(|candidate| same_codex_desktop_process(process, candidate))
        }) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "Codex did not quit within 5 seconds; it was not force-quit"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "macos")]
fn wait_for_codex_desktop_launch() -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !running_codex_desktop_processes()?.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("Codex did not reopen within 10 seconds"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "macos")]
pub fn restart_codex_desktop() -> Result<()> {
    // Ask the application to quit through LaunchServices first. The `ps`
    // process list can miss Codex during a quick relaunch or while its
    // renderer is transitioning, in which case `open -b` would merely focus
    // the existing process instead of restarting it.
    let quit = crate::proc::command("osascript")
        .args(["-e", "tell application id \"com.openai.codex\" to quit"])
        .output()
        .context("asking Codex Desktop to quit")?;
    if !quit.status.success() {
        let stderr = String::from_utf8_lossy(&quit.stderr);
        if !stderr.contains("isn't running") && !stderr.contains("not running") {
            log::debug!("Codex quit request returned non-zero: {}", stderr.trim());
        }
    }
    let processes = running_codex_desktop_processes()?;

    for process in &processes {
        terminate_codex_desktop_process(process)?;
    }
    if !processes.is_empty() {
        wait_for_codex_desktop_exit(&processes)?;
    }

    let output = crate::proc::command("open")
        .args(["-b", CODEX_DESKTOP_BUNDLE_ID])
        .output()
        .context("reopening Codex Desktop")?;
    if !output.status.success() {
        return Err(anyhow!(
            "reopening Codex Desktop failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    wait_for_codex_desktop_launch()
}

#[cfg(not(target_os = "macos"))]
pub fn restart_codex_desktop() -> Result<()> {
    Err(anyhow!(
        "Restarting Codex Desktop is only supported on macOS"
    ))
}

pub fn disable_client_setup(client_id: &str) -> Result<()> {
    let mut state = load_setup_state();

    match client_id {
        "codex" | "codex_cli" => {
            let preserved_provider = state
                .preserved_base_urls
                .get(normalized_setup_id(client_id))
                .cloned();
            disable_codex_cli()?;
            // Restore any pre-Headroom root model_provider instead of leaving the
            // key deleted -- deleting it silently drops a gateway user onto
            // api.openai.com (mirrors the Claude base_url restore).
            if let Some(provider) = preserved_provider {
                // Do not clear the preserved value below unless this restore
                // succeeds. Leaving a gateway user on the native endpoint is
                // worse than returning a visible cleanup error.
                restore_codex_model_provider(&provider)?;
            }
            disable_codex_gui()?;
            // Hand the threads back to the native-provider menu so the full
            // history stays visible once Codex no longer routes through Headroom.
            retag_codex_thread_providers(CODEX_HEADROOM_PROVIDER, CODEX_NATIVE_PROVIDER);
        }
        "codex_gui" => {
            disable_codex_gui()?;
        }
        "claude_code" => {
            let shell_targets = resolve_client_shell_targets_for_cleanup(&state, client_id)?;
            remove_shell_block(&shell_targets, "claude_code")?;
            // Also drop the managed_rtk PATH block so `rtk` isn't exported from
            // shell profiles after quit — otherwise the user's next shell still
            // has Headroom binaries shadowing whatever's on PATH.
            remove_shell_block(&shell_targets, "managed_rtk")?;
            // Restore any pre-Headroom gateway/proxy URL instead of deleting
            // the key — deleting it pointed gateway users at api.anthropic.com
            // where their credentials may not even work.
            let preserved = state
                .preserved_base_urls
                .get(normalized_setup_id(client_id))
                .cloned();
            remove_claude_settings_env(
                "ANTHROPIC_BASE_URL",
                HEADROOM_ANTHROPIC_BASE_URL,
                preserved.as_deref(),
            )?;
            remove_legacy_vscode_base_url_keys()?;
            // Strip the PreToolUse hook entry and delete the hook script so CC
            // behaves exactly as it did before Headroom was launched.
            for settings_path in claude_settings_candidates() {
                strip_headroom_hook_from_settings(&settings_path)?;
            }
            let hook_path = headroom_rtk_hook_path();
            if hook_path.exists() {
                std::fs::remove_file(&hook_path)
                    .with_context(|| format!("removing {}", hook_path.display()))?;
            }
            remove_claude_guard_hook()?;
        }
        "vscode" => {
            let preserved = state
                .preserved_base_urls
                .get(normalized_setup_id(client_id))
                .cloned();
            remove_vscode_connector_keys(preserved.as_deref())?;
        }
        "grok_build" => disable_grok_build()?,
        "opencode" => disable_opencode(&state)?,
        "zcode" => disable_zcode()?,
        other => {
            return Err(anyhow!(
                "Automatic setup disable is not supported yet for {other}.",
            ))
        }
    }

    match client_id {
        "codex" | "codex_cli" => {
            state.configured_clients.remove("codex");
            state.configured_clients.remove("codex_cli");
            state.configured_clients.remove("codex_gui");
            state.remembered_clients.remove("codex");
            state.remembered_clients.remove("codex_cli");
            state.remembered_clients.remove("codex_gui");
            state.managed_shell_files.remove("codex");
            state.managed_shell_files.remove("codex_cli");
            state.managed_shell_files.remove("codex_gui");
            state.remembered_shell_files.remove("codex");
            state.remembered_shell_files.remove("codex_cli");
            state.remembered_shell_files.remove("codex_gui");
            // Consumed: the provider is back in the user's config now. The next
            // apply re-captures it if Headroom is re-enabled.
            state.preserved_base_urls.remove("codex_cli");
        }
        "opencode" => {
            state.configured_clients.remove("opencode");
            state.remembered_clients.remove("opencode");
            state.managed_shell_files.remove("opencode");
            state.remembered_shell_files.remove("opencode");
            // Consumed: the URLs are back in the user's config now. The next
            // apply re-captures them if Headroom is re-enabled.
            state.preserved_base_urls.remove("opencode_anthropic");
            state.preserved_base_urls.remove("opencode_openai");
        }
        _ => {
            let state_id = normalized_setup_id(client_id);
            state.configured_clients.remove(state_id);
            state.remembered_clients.remove(state_id);
            state.managed_shell_files.remove(state_id);
            state.remembered_shell_files.remove(state_id);
            // Consumed: the URL is back in the user's config now. The next
            // apply re-captures it if Headroom is re-enabled.
            state.preserved_base_urls.remove(state_id);
        }
    }
    write_setup_state(&mut state)?;
    Ok(())
}

/// Clear all managed routes while retaining Codex's stable local-router
/// configuration. The router is switched to native forwarding by lifecycle
/// callers before the Headroom backend is stopped, so existing Codex sessions
/// keep using the same URL while the GUI is closed or has crashed.
pub fn clear_client_setups_preserving_codex() -> Result<()> {
    clear_client_setups_with_options(true)
}

pub fn clear_client_setups() -> Result<()> {
    clear_client_setups_with_options(false)
}

fn clear_client_setups_with_options(preserve_codex: bool) -> Result<()> {
    // Capture snapshot before disabling. We re-apply it afterwards because
    // disable_client_setup also clears remembered_clients as a side effect,
    // which would otherwise erase the snapshot we need for restore_client_setups.
    let pre = load_setup_state();
    // Merge with any prior snapshot so a second clear is idempotent: after a
    // pause, configured_clients is already empty and only remembered_clients
    // holds the restore set — a quit-time clear must not wipe it (pause then
    // Cmd-Q used to permanently lose all connectors).
    let mut snapshot_clients = pre.remembered_clients.clone();
    snapshot_clients.extend(pre.configured_clients.clone());
    let mut snapshot_shell_files = pre.remembered_shell_files.clone();
    snapshot_shell_files.extend(pre.managed_shell_files.clone());

    // Keep the Codex ownership metadata and custom-provider backup intact when
    // a normal pause/quit leaves its config pointed at the detached router.
    let codex_configured: BTreeMap<_, _> = pre
        .configured_clients
        .iter()
        .filter(|(id, _)| is_codex_state_id(id))
        .map(|(id, value)| (id.clone(), value.clone()))
        .collect();
    let codex_remembered: BTreeMap<_, _> = pre
        .remembered_clients
        .iter()
        .filter(|(id, _)| is_codex_state_id(id))
        .map(|(id, value)| (id.clone(), value.clone()))
        .collect();
    let codex_managed_shell: BTreeMap<_, _> = pre
        .managed_shell_files
        .iter()
        .filter(|(id, _)| is_codex_state_id(id))
        .map(|(id, value)| (id.clone(), value.clone()))
        .collect();
    let codex_remembered_shell: BTreeMap<_, _> = pre
        .remembered_shell_files
        .iter()
        .filter(|(id, _)| is_codex_state_id(id))
        .map(|(id, value)| (id.clone(), value.clone()))
        .collect();
    let codex_preserved_urls: BTreeMap<_, _> = pre
        .preserved_base_urls
        .iter()
        .filter(|(id, _)| is_codex_state_id(id))
        .map(|(id, value)| (id.clone(), value.clone()))
        .collect();

    if preserve_codex {
        snapshot_clients.retain(|id, _| !is_codex_state_id(id));
        snapshot_shell_files.retain(|id, _| !is_codex_state_id(id));
    }

    // Each client owns independent files, so keep attempting the remaining
    // reversals after one fails. Do not, however, report success: callers need
    // to retain setup state and rollback copies until every client is clean.
    let mut failures = Vec::new();
    for spec in MANAGED_CLIENT_SPECS {
        if preserve_codex && spec.id == "codex" {
            continue;
        }
        if let Err(err) = disable_client_setup(spec.id) {
            failures.push(format!("{} ({}): {err:#}", spec.name, spec.id));
        }
    }
    // This legacy cleanup only removes launchctl environment variables. It is
    // still required in preserve mode: an old OPENAI_BASE_URL=6867 would
    // otherwise override the stable 6891 config for GUI launches.
    let legacy_codex_cleanup = if preserve_codex {
        disable_codex_gui()
    } else {
        disable_client_setup("codex_gui")
    };
    if let Err(err) = legacy_codex_cleanup {
        failures.push(format!("Codex desktop (codex_gui): {err:#}"));
    }

    // Re-save the remembered snapshot so restore_client_setups works on next launch.
    if !snapshot_clients.is_empty() || preserve_codex {
        let mut state = load_setup_state();
        if !snapshot_clients.is_empty() {
            state.remembered_clients = snapshot_clients;
            state.remembered_shell_files = snapshot_shell_files;
        }
        if preserve_codex {
            // Re-install the exact Codex entries captured before the other
            // clients were disabled, including a custom provider backup needed
            // by a later explicit "Disconnect Codex" action.
            state
                .configured_clients
                .retain(|id, _| !is_codex_state_id(id));
            state
                .remembered_clients
                .retain(|id, _| !is_codex_state_id(id));
            state
                .managed_shell_files
                .retain(|id, _| !is_codex_state_id(id));
            state
                .remembered_shell_files
                .retain(|id, _| !is_codex_state_id(id));
            state
                .preserved_base_urls
                .retain(|id, _| !is_codex_state_id(id));
            state.configured_clients.extend(codex_configured);
            state.remembered_clients.extend(codex_remembered);
            state.managed_shell_files.extend(codex_managed_shell);
            state.remembered_shell_files.extend(codex_remembered_shell);
            state.preserved_base_urls.extend(codex_preserved_urls);
        }
        if let Err(err) = write_setup_state(&mut state) {
            failures.push(format!("persisting the client rollback snapshot: {err:#}"));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "one or more client setup reversals failed: {}",
            failures.join("; ")
        ))
    }
}

fn is_codex_state_id(id: &str) -> bool {
    matches!(id, "codex" | "codex_cli" | "codex_gui")
}

/// Fully uninstalls Headroom's on-disk footprint on a best-effort basis:
/// reverses every client setup, strips Headroom's hook entry from Claude Code
/// settings (both `settings.json` and `settings.local.json`), deletes the
/// managed hook script, the Community application-support directory, the
/// `~/.headroom-local-community` workspace, the macOS LaunchAgent plist, Preferences,
/// Caches, and keychain entries.
///
/// Returns the list of paths that were successfully removed (useful for
/// surfacing to the user). Per-step failures are logged and skipped.
/// `remove_dir_all`, retrying on transient `ENOTEMPTY`. A backend/proxy
/// process killed in `stop_headroom` may still flush a log line into the
/// directory tree mid-walk, re-creating an entry so the final `rmdir` fails
/// with "Directory not empty". A short backoff lets the writer finish.
///
/// A `PermissionDenied` is different: it is usually NOT transient, so retrying
/// alone never clears it. The two causes that reach us are a read-only
/// attribute somewhere in the tree -- which blocks the delete outright on
/// Windows, and blocks it via a read-only *directory* on Unix -- and a live
/// process holding an open handle to a file inside it (Sentry RUST-6T: an agent
/// session still running serena's MCP server out of the venv being removed).
/// The first is fixable here, so on the first `PermissionDenied` we clear
/// read-only bits across the tree and try again. The second is not ours to fix
/// by force; callers surface it so the user can close the session.
pub(crate) fn remove_dir_all_retry(path: &Path) -> std::io::Result<()> {
    let mut last = Ok(());
    let mut cleared_readonly = false;
    for attempt in 0..5 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                if e.kind() == std::io::ErrorKind::PermissionDenied && !cleared_readonly {
                    // Once per call: if this does not free the tree, the cause is
                    // an open handle and further passes are wasted work.
                    cleared_readonly = true;
                    clear_readonly_recursive(path);
                }
                last = Err(e);
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
        }
    }
    last
}

/// Best-effort: drop the read-only bit on `path` and everything under it, so a
/// following `remove_dir_all` is not blocked by it. Depth-first, because a
/// read-only directory has to stay writable until its children are gone.
///
/// `DirEntry::metadata` does not traverse symlinks or Windows reparse points, so
/// a junction or symlink is never descended into -- this cannot walk out of the
/// tree or loop. Every failure is ignored: this only ever runs as a rescue pass
/// before a delete that has already failed once.
fn clear_readonly_recursive(path: &Path) {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            match entry.metadata() {
                Ok(md) if md.is_dir() => clear_readonly_recursive(&entry.path()),
                Ok(md) => clear_readonly(&entry.path(), md.permissions()),
                Err(_) => {}
            }
        }
    }
    // The directory itself last: on Unix its write bit is what permits unlinking
    // the children above, so clearing it earlier would be undone by nothing but
    // is pointless before they are gone.
    if let Ok(md) = std::fs::symlink_metadata(path) {
        clear_readonly(path, md.permissions());
    }
}

fn clear_readonly(path: &Path, perms: std::fs::Permissions) {
    if !perms.readonly() {
        return;
    }
    // Deliberately NOT `set_readonly(false)`: on Unix that sets the write bit for
    // group and other as well. This runs on a delete that has already failed, so
    // the delete may fail again (an open handle is not fixable here) -- and then
    // whatever we widened is left behind permanently on the user's disk. Grant
    // the minimum that permits the unlink: owner write.
    let mut perms = perms;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = perms.mode();
        perms.set_mode(mode | 0o200);
    }
    #[cfg(not(unix))]
    {
        // Windows has no per-class write bit here; the read-only attribute is the
        // whole mechanism, and clearing it is what unblocks the delete.
        perms.set_readonly(false);
    }
    let _ = std::fs::set_permissions(path, perms);
}

/// Undo every edit Headroom made to *other* tools' state: agent settings, shell
/// rc blocks, hook scripts, MCP registrations, login-keychain credentials, the
/// backup files we left behind, and the LaunchAgent plist.
///
/// Deliberately leaves Headroom's own directories alone. Splitting it out this
/// way is what lets the Homebrew cask call `--uninstall` from its `uninstall`
/// stanza without destroying user data that belongs to `zap` — see
/// docs/macos-release.md. Idempotent: safe to run when the app is not running,
/// and safe to run twice.
fn revert_external_mutations_with_status() -> (Vec<String>, bool, bool) {
    let mut removed: Vec<String> = Vec::new();
    let mut cleanup_failed = false;

    // Reverse settings.json mutations and shell blocks for every known client.
    if let Err(err) = clear_client_setups() {
        log::warn!("cleanup: clear_client_setups failed: {err}");
        cleanup_failed = true;
    }

    // Strip the Headroom hook entry from both ~/.claude/settings.json and
    // ~/.claude/settings.local.json. `clear_client_setups` doesn't do this —
    // it only removes env keys — so without this step the hook entry remains,
    // points to a deleted script, and Claude Code logs errors on every call.
    for settings_path in claude_settings_candidates() {
        match strip_headroom_hook_from_settings(&settings_path) {
            Ok(true) => removed.push(settings_path.display().to_string()),
            Ok(false) => {}
            Err(err) => {
                cleanup_failed = true;
                log::warn!(
                    "cleanup: stripping hook from {} failed: {err}",
                    settings_path.display()
                );
            }
        }
    }

    // Independently strip the ANTHROPIC_BASE_URL routing env and the Claude
    // guard hook. clear_client_setups() above also removes these via
    // disable_client_setup, but only after remove_shell_block succeeds (it runs
    // under `?` before them): a shell-rc failure there silently leaves both in
    // place, and each bricks Claude once the proxy is gone (stale base URL ->
    // dead 127.0.0.1:6867; guard hook errors on every prompt). Do them
    // unconditionally here. Idempotent: each only acts on Headroom's own value,
    // restoring any preserved pre-Headroom gateway URL.
    let preserved = load_setup_state()
        .preserved_base_urls
        .get(normalized_setup_id("claude_code"))
        .cloned();
    if let Err(err) = remove_claude_settings_env(
        "ANTHROPIC_BASE_URL",
        HEADROOM_ANTHROPIC_BASE_URL,
        preserved.as_deref(),
    ) {
        log::warn!("cleanup: removing ANTHROPIC_BASE_URL from Claude settings failed: {err}");
        cleanup_failed = true;
    }
    if let Err(err) = remove_claude_guard_hook() {
        log::warn!("cleanup: removing Claude guard hook failed: {err}");
        cleanup_failed = true;
    }

    // Restore the open-source Claude Code plugin hook if we neutralized it.
    let (oss_hooks_pending, restored_oss_hooks) = restore_oss_plugin_hooks();
    removed.extend(restored_oss_hooks);

    for hook_path in [headroom_rtk_hook_path(), headroom_markitdown_hook_path()] {
        if hook_path.exists() {
            match std::fs::remove_file(&hook_path) {
                Ok(_) => removed.push(hook_path.display().to_string()),
                Err(err) => {
                    cleanup_failed = true;
                    log::warn!("cleanup: removing {} failed: {err}", hook_path.display());
                }
            }
        }
    }

    // Drop the managed RTK nudge from ~/.codex/AGENTS.md (clear_client_setups
    // handles env/shell blocks but not these managed Markdown blocks).
    if let Err(err) = remove_managed_block(&rtk_codex_agents_path(), "rtk") {
        log::warn!("cleanup: removing rtk AGENTS.md block failed: {err}");
        cleanup_failed = true;
    }

    // MCP server registrations live in the agents' own configs, outside
    // Headroom's footprint. uninstall_and_quit unregisters via the Python
    // helpers first, but that needs a working runtime — strip anything left
    // that provably launches from Headroom's install dirs (plus the
    // `headroom` server itself), or every new agent session would spawn a
    // failing MCP server against the deleted entrypoint.
    if let Err(err) = disable_serena_integration() {
        log::warn!("cleanup: removing Serena instruction blocks failed: {err}");
        cleanup_failed = true;
    }
    match remove_headroom_mcp_entries() {
        Ok(paths) => removed.extend(paths),
        Err(err) => {
            cleanup_failed = true;
            log::warn!("cleanup: removing Headroom MCP registrations failed: {err:#}");
        }
    }

    // Credentials live in the login keychain, which no Homebrew cask stanza can
    // reach, so this has to happen here rather than being left to `zap`.
    remove_known_keychain_entries();

    // Sweep `<basename>.headroom-local-community-backup-*` siblings created by
    // `backup_if_exists` for every file we ever mutated.
    // Without this, stale backups remain in ~/.claude, ~/.claude/hooks,
    // ~/.codex, ~/Library/Application Support/Code/User, and the user's
    // shell rc directory after uninstall.
    if cleanup_failed || oss_hooks_pending {
        log::warn!(
            "cleanup: preserving managed rollback backups because one or more external reversals are incomplete"
        );
    } else {
        for target in managed_backup_targets() {
            removed.extend(sweep_managed_backups(&target));
        }
    }

    // The LaunchAgent plist and its Linux counterpart are install side effects
    // outside Headroom's own directories, so they belong here and not with the
    // user-data removal.
    #[cfg(target_os = "macos")]
    removed.extend(remove_macos_launch_agents());
    #[cfg(target_os = "linux")]
    removed.extend(remove_linux_autostart_entries());

    (removed, oss_hooks_pending, cleanup_failed)
}

pub fn revert_external_mutations() -> Vec<String> {
    revert_external_mutations_with_status().0
}

/// Full uninstall: everything `revert_external_mutations` undoes, plus every
/// Community-owned directory (app data, workspace, caches, logs, preferences).
/// Shared model caches are deliberately preserved. Used by the in-app
/// "uninstall and quit".
///
/// The `--uninstall` CLI flag deliberately calls the narrower function instead:
/// a Homebrew cask's `uninstall` must not delete user data, which is what `zap`
/// is for.
pub fn perform_full_cleanup() -> Vec<String> {
    let (mut removed, oss_hooks_pending, cleanup_failed) = revert_external_mutations_with_status();
    let preserve_rollback_state = cleanup_failed || oss_hooks_pending;

    // Also wipe the per-client setup-state file so a reinstall starts clean.
    let setup_state = setup_state_path();
    if preserve_rollback_state {
        log::warn!(
            "cleanup: preserving {} because external configuration rollback is incomplete",
            setup_state.display()
        );
    } else if setup_state.exists() {
        match std::fs::remove_file(&setup_state) {
            Ok(()) => removed.push(setup_state.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", setup_state.display()),
        }
    }

    let app_dir = app_data_dir();
    if app_dir.exists() {
        if preserve_rollback_state {
            log::warn!(
                "cleanup: preserving {} because external configuration rollback is incomplete",
                app_dir.display()
            );
        } else {
            match remove_dir_all_retry(&app_dir) {
                Ok(_) => removed.push(app_dir.display().to_string()),
                Err(err) => log::warn!("cleanup: removing {} failed: {err}", app_dir.display()),
            }
        }
    }

    let dot_headroom = crate::edition::workspace_dir();
    if preserve_rollback_state {
        log::warn!(
            "cleanup: preserving {} because external configuration rollback is incomplete",
            dot_headroom.display()
        );
    } else if dot_headroom.exists() {
        match std::fs::remove_dir_all(&dot_headroom) {
            Ok(_) => removed.push(dot_headroom.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", dot_headroom.display()),
        }
    }

    // HuggingFace's cache is shared across applications. Even author-prefixed
    // model snapshots may be in use by the paid Headroom app, so Community
    // cleanup deliberately leaves the entire shared cache untouched.

    #[cfg(target_os = "macos")]
    {
        // remove_macos_launch_agents() runs in revert_external_mutations().
        removed.extend(remove_macos_preferences());
        removed.extend(remove_macos_caches());
        removed.extend(remove_macos_logs());
        removed.extend(remove_macos_bundle_dirs());
    }

    #[cfg(target_os = "windows")]
    {
        // Remove the autostart Run key tauri-plugin-autostart creates
        // (HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Headroom).
        let _ = crate::proc::command("reg")
            .args([
                "delete",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "Headroom",
                "/f",
            ])
            .status();

        // Windows app-data dirs not covered by app_data_dir() (which resolves
        // to %APPDATA%\Headroom already) and the huggingface cache (local).
        if let Some(base) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            for candidate in [base.join("HeadroomLocalCommunity")] {
                if candidate.exists() {
                    match remove_dir_all_retry(&candidate) {
                        Ok(_) => removed.push(candidate.display().to_string()),
                        Err(err) => {
                            log::warn!("cleanup: removing {} failed: {err}", candidate.display())
                        }
                    }
                }
            }
        }
    }

    removed
}

/// Every file Community may mutate, and therefore every file that may have a
/// `.headroom-local-community-backup-*` sibling to sweep.
fn managed_backup_targets() -> Vec<PathBuf> {
    let mut targets: Vec<PathBuf> = claude_settings_candidates();
    targets.push(zcode_cli_config_path());
    targets.push(home_dir().join(".claude.json"));
    targets.push(headroom_rtk_hook_path());
    targets.push(headroom_markitdown_hook_path());
    targets.push(claude_guard_hook_path());
    targets.push(codex_config_toml_path());
    targets.push(codex_hooks_json_path());
    targets.push(codex_guard_hook_path());
    targets.push(grok_config_toml_path());
    // Both possible opencode config names: backups are created next to
    // whichever file was active at apply/disable time.
    targets.push(opencode_config_dir().join("opencode.json"));
    targets.push(opencode_config_dir().join("opencode.jsonc"));
    targets.push(
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("settings.json"),
    );
    targets.extend(all_shell_paths());
    targets
}

/// Remove sibling backup files created by this Community edition next to
/// `target`. Legacy/paid-edition backup names are deliberately untouched.
/// Returns the paths removed.
fn sweep_managed_backups(target: &Path) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(parent) = target.parent() else {
        return removed;
    };
    let Some(file_name) = target.file_name().and_then(|n| n.to_str()) else {
        return removed;
    };
    let headroom_prefix = format!("{}.headroom-local-community-backup-", file_name);

    let Ok(entries) = std::fs::read_dir(parent) else {
        return removed;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(&headroom_prefix) {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(_) => removed.push(path.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", path.display()),
        }
    }
    removed
}

/// True when an MCP server command launches from inside Headroom's install
/// footprint (the Community app-support or workspace directory). Uninstall
/// deletes only those isolated locations,
/// so a surviving entry could only ever spawn a failing server.
fn mcp_command_in_headroom_footprint(command: &str) -> bool {
    let app_dir = format!("{}/", app_data_dir().display());
    let dot_headroom = format!("{}/", crate::edition::workspace_dir().display());
    command.starts_with(&app_dir) || command.starts_with(&dot_headroom)
}

/// Headroom-owned MCP entry: the `headroom` server itself (desktop owns that
/// name — install always writes it with --force), or any entry whose command
/// resolves into Headroom's install footprint (serena, codebase-memory).
/// `command` is a string in Claude's config and an array in OpenCode's.
fn mcp_json_entry_is_headroom(name: &str, entry: &Value) -> bool {
    if name == crate::edition::MCP_SERVER_NAME {
        return true;
    }
    let command = match entry.get("command") {
        Some(Value::String(command)) => Some(command.as_str()),
        Some(Value::Array(items)) => items.first().and_then(Value::as_str),
        _ => None,
    };
    command.is_some_and(mcp_command_in_headroom_footprint)
}

/// Drop Headroom-owned entries from a `mcpServers`/`mcp` JSON map in place.
/// Returns whether anything was removed.
fn remove_headroom_mcp_json_entries(servers: &mut serde_json::Map<String, Value>) -> bool {
    let owned: Vec<String> = servers
        .iter()
        .filter(|(name, entry)| mcp_json_entry_is_headroom(name, entry))
        .map(|(name, _)| name.clone())
        .collect();
    for name in &owned {
        servers.remove(name);
    }
    !owned.is_empty()
}

/// Strip Headroom-owned MCP servers from `mcpServers` in `~/.claude.json`.
/// Parse failure ⇒ skip: the file holds OAuth state and per-project settings,
/// so it must never be rewritten from a state we couldn't fully read.
fn strip_headroom_mcp_from_claude_json() -> Result<Option<String>> {
    let path = home_dir().join(".claude.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let mut root: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {}; leaving it untouched", path.display()))?;
    let Some(root) = root.as_object_mut() else {
        return Err(anyhow!(
            "{} is not a JSON object; leaving it untouched",
            path.display()
        ));
    };
    let Some(servers_value) = root.get_mut("mcpServers") else {
        return Ok(None);
    };
    let Some(servers) = servers_value.as_object_mut() else {
        return Err(anyhow!(
            "{}.mcpServers is not an object; leaving it untouched",
            path.display()
        ));
    };
    if !remove_headroom_mcp_json_entries(servers) {
        return Ok(None);
    }
    let bytes = serde_json::to_vec_pretty(&root)
        .with_context(|| format!("serializing {}", path.display()))?;
    if !backup_for_cleanup(&path) {
        return Err(anyhow!(
            "backing up {} failed; leaving it untouched",
            path.display()
        ));
    }
    atomic_write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(path.display().to_string()))
}

/// Strip Headroom-owned MCP servers from OpenCode's top-level `mcp` table.
/// Same parse-failure contract as the Claude variant.
fn strip_headroom_mcp_from_opencode() -> Result<Option<String>> {
    let path = opencode_config_path();
    if !path.exists() {
        return Ok(None);
    }
    let mut config = read_opencode_config(&path)?;
    let Some(servers_value) = config.get_mut("mcp") else {
        return Ok(None);
    };
    let Some(servers) = servers_value.as_object_mut() else {
        return Err(anyhow!(
            "{}.mcp is not an object; leaving it untouched",
            path.display()
        ));
    };
    if !remove_headroom_mcp_json_entries(servers) {
        return Ok(None);
    }
    if !backup_for_cleanup(&path) {
        return Err(anyhow!(
            "backing up {} failed; leaving it untouched",
            path.display()
        ));
    }
    write_opencode_config(&path, &config)?;
    Ok(Some(path.display().to_string()))
}

/// Pure-text removal of Headroom-owned `[mcp_servers.*]` tables (including
/// subtables) and the Python registrar's
/// `# --- [end ]Headroom Local Community MCP server[: name] ---` marker comments from a
/// Codex-style TOML config. A table is ours when its name is `headroom` or
/// its `command` launches from Headroom's install footprint; user-managed
/// servers stay untouched.
fn strip_headroom_mcp_toml(content: &str) -> String {
    fn mcp_table_name(line: &str) -> Option<&str> {
        let inner = line
            .trim()
            .strip_prefix("[mcp_servers.")?
            .strip_suffix(']')?;
        Some(inner.split('.').next().unwrap_or(inner))
    }

    let lines: Vec<&str> = content.lines().collect();

    // Pass 1: which server names are Headroom-owned.
    let mut owned: BTreeSet<String> = BTreeSet::new();
    let mut current: Option<&str> = None;
    for line in &lines {
        if line.trim().starts_with('[') {
            current = mcp_table_name(line);
        }
        let Some(name) = current else { continue };
        if name == crate::edition::MCP_SERVER_NAME {
            owned.insert(name.to_string());
        } else if line
            .split_once('=')
            .is_some_and(|(key, _)| key.trim() == "command")
            && toml_line_value(line)
                .as_deref()
                .is_some_and(mcp_command_in_headroom_footprint)
        {
            owned.insert(name.to_string());
        }
    }

    // Pass 2: rebuild without owned spans and marker comments. A span runs
    // from its `[mcp_servers.<name>]`/`[...<name>.<sub>]` header to the next
    // table header of any other name.
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut dropping = false;
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("# --- Headroom Local Community MCP server")
            || trimmed.starts_with("# --- end Headroom Local Community MCP server")
        {
            continue;
        }
        if trimmed.starts_with('[') {
            dropping = matches!(mcp_table_name(line), Some(name) if owned.contains(name));
        }
        if !dropping {
            out.push(line);
        }
    }
    out.join("\n")
}

/// Strip Headroom-owned MCP tables from a Codex/Grok `config.toml`. Returns
/// the path when the file changed.
fn strip_headroom_mcp_from_toml_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let existing =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let stripped = strip_headroom_mcp_toml(&existing);
    let normalized = {
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("{trimmed}\n")
        }
    };
    if normalized == existing {
        return Ok(None);
    }
    if !backup_for_cleanup(path) {
        return Err(anyhow!(
            "backing up {} failed; leaving it untouched",
            path.display()
        ));
    }
    atomic_write(path, normalized.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(path.display().to_string()))
}

/// Strip ZCode servers that launch from the runtime being uninstalled.
fn strip_headroom_mcp_from_zcode() -> Result<Option<String>> {
    let path = zcode_cli_config_path();
    if !path.exists() {
        return Ok(None);
    }
    let mut config = read_zcode_config(&path)?;
    let Some(servers) = config.pointer_mut("/mcp/servers") else {
        return Ok(None);
    };
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}.mcp.servers is not an object", path.display()))?;
    let original_len = servers.len();
    // Full uninstall must remove even customized entries that launch a
    // runtime we are deleting, while keeping independently managed servers.
    servers.retain(|_, entry| {
        !entry.get("command")
            .and_then(Value::as_str)
            .is_some_and(mcp_command_in_headroom_footprint)
    });
    if servers.len() == original_len {
        return Ok(None);
    }
    backup_if_exists(&path)?;
    write_zcode_config(&path, &config)?;
    Ok(Some(path.display().to_string()))
}

/// Strip Headroom-registered MCP servers from every client config. Runs even
/// when the Python unregister helpers in uninstall_and_quit already succeeded
/// (then it's a no-op) so a broken runtime can't leave dead entries behind.
fn remove_headroom_mcp_entries() -> Result<Vec<String>> {
    let mut removed = Vec::new();
    let mut failures = Vec::new();
    for (label, result) in [
        ("Claude", strip_headroom_mcp_from_claude_json()),
        (
            "Codex",
            strip_headroom_mcp_from_toml_file(&codex_config_toml_path()),
        ),
        (
            "Grok",
            strip_headroom_mcp_from_toml_file(&grok_config_toml_path()),
        ),
        ("OpenCode", strip_headroom_mcp_from_opencode()),
        ("ZCode", strip_headroom_mcp_from_zcode()),
    ] {
        match result {
            Ok(Some(path)) => removed.push(path),
            Ok(None) => {}
            Err(err) => failures.push(format!("{label}: {err:#}")),
        }
    }
    if failures.is_empty() {
        Ok(removed)
    } else {
        Err(anyhow!(
            "one or more MCP config reversals failed: {}",
            failures.join("; ")
        ))
    }
}

/// A cleanup rewrite is destructive when it removes user configuration. Keep
/// the original in place if the rollback copy cannot be made; logging the
/// error and proceeding to `atomic_write` would violate the persistence
/// contract by replacing the only recoverable copy.
fn backup_for_cleanup(path: &Path) -> bool {
    match backup_if_exists(path) {
        Ok(_) => true,
        Err(err) => {
            log::error!(
                "cleanup: backing up {} failed; leaving original untouched: {err:#}",
                path.display()
            );
            false
        }
    }
}

fn claude_settings_candidates() -> Vec<PathBuf> {
    let claude_dir = home_dir().join(".claude");
    vec![
        claude_dir.join("settings.json"),
        claude_dir.join("settings.local.json"),
    ]
}

/// Remove the PreToolUse entry pointing at `headroom-local-community-rtk-rewrite.sh`. Drops
/// the `PreToolUse` array if it becomes empty, and the `hooks` object if it
/// has no remaining event arrays. Returns true if the file was modified.
fn strip_headroom_hook_from_settings(settings_path: &Path) -> Result<bool> {
    remove_pre_tool_use_markers(
        settings_path,
        &[
            "headroom-local-community-rtk-rewrite.sh",
            "headroom-local-community-markitdown-read.sh",
        ],
    )
}

/// Removes every PreToolUse hook entry whose command contains one of `markers`,
/// pruning empty `PreToolUse`/`hooks` containers. Returns whether the file changed.
fn remove_pre_tool_use_markers(settings_path: &Path, markers: &[&str]) -> Result<bool> {
    if !settings_path.exists() {
        return Ok(false);
    }

    let raw = std::fs::read_to_string(settings_path)
        .with_context(|| format!("reading {}", settings_path.display()))?;
    if raw.trim().is_empty() {
        return Ok(false);
    }
    let mut root = parse_json_object(&raw, settings_path)?;

    let Some(hooks_val) = root.get_mut("hooks") else {
        return Ok(false);
    };
    let Some(hooks_obj) = hooks_val.as_object_mut() else {
        return reject_invalid_json_container(settings_path, "hooks", "an object").map(|_| false);
    };

    let mut changed = false;

    if let Some(pre_tool_use_value) = hooks_obj.get_mut("PreToolUse") {
        let Some(pre_tool_use) = pre_tool_use_value.as_array_mut() else {
            return reject_invalid_json_container(settings_path, "hooks.PreToolUse", "an array")
                .map(|_| false);
        };
        let before = pre_tool_use.len();
        pre_tool_use.retain(|entry| {
            !markers
                .iter()
                .any(|marker| entry_contains_hook(entry, marker))
        });
        if pre_tool_use.len() != before {
            changed = true;
        }
        if pre_tool_use.is_empty() {
            hooks_obj.remove("PreToolUse");
        }
    }

    if hooks_obj.is_empty() {
        root.remove("hooks");
    }

    if !changed {
        return Ok(false);
    }

    backup_if_exists(settings_path)?;
    atomic_write(
        settings_path,
        &serde_json::to_vec_pretty(&Value::Object(root))
            .context("serializing Claude settings for hook cleanup")?,
    )?;

    Ok(true)
}

#[cfg(target_os = "macos")]
fn remove_macos_launch_agents() -> Vec<String> {
    let mut removed = Vec::new();
    let launch_agents_dir = home_dir().join("Library").join("LaunchAgents");

    // Bundle-id-style plist (tauri-plugin-autostart default) and the
    // "HeadroomLocalCommunity.plist" name some older builds shipped. Either can exist.
    let candidates = [
        "org.headroomlocal.community.plist",
        "HeadroomLocalCommunity.plist",
    ];

    for name in candidates {
        let path = launch_agents_dir.join(name);
        if !path.exists() {
            continue;
        }
        // Best-effort unload before deletion so launchd forgets the job.
        let _ = crate::proc::command("launchctl")
            .args(["unload", "-w"])
            .arg(&path)
            .output();
        match std::fs::remove_file(&path) {
            Ok(_) => removed.push(path.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", path.display()),
        }
    }

    removed
}

/// tauri-plugin-autostart writes `~/.config/autostart/<product name>.desktop`
/// on Linux (auto-launch names the file after `package_info().name`). Left
/// behind, it execs a binary uninstall just deleted, on every login — the same
/// class of leftover as the macOS LaunchAgent plist.
#[cfg(target_os = "linux")]
fn remove_linux_autostart_entries() -> Vec<String> {
    let mut removed = Vec::new();
    let autostart_dir = home_dir().join(".config").join("autostart");

    // Current product name, plus the binary name in case a build ever shipped
    // the plugin's `app_name` override. Either can exist.
    for name in [
        "HeadroomLocalCommunity.desktop",
        "headroom-local-community.desktop",
    ] {
        let path = autostart_dir.join(name);
        if !path.exists() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", path.display()),
        }
    }

    removed
}

#[cfg(target_os = "macos")]
fn remove_macos_preferences() -> Vec<String> {
    let mut removed = Vec::new();
    let prefs_dir = home_dir().join("Library").join("Preferences");
    let Ok(entries) = std::fs::read_dir(&prefs_dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with("org.headroomlocal.community") {
            continue;
        }
        let path = entry.path();
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(_) => removed.push(path.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", path.display()),
        }
    }
    removed
}

#[cfg(target_os = "macos")]
fn remove_macos_caches() -> Vec<String> {
    let mut removed = Vec::new();
    let caches_dir = home_dir()
        .join("Library")
        .join("Caches")
        .join("org.headroomlocal.community");
    if caches_dir.exists() {
        match std::fs::remove_dir_all(&caches_dir) {
            Ok(_) => removed.push(caches_dir.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", caches_dir.display()),
        }
    }
    removed
}

#[cfg(target_os = "macos")]
fn remove_macos_logs() -> Vec<String> {
    let mut removed = Vec::new();
    let logs_dir = home_dir()
        .join("Library")
        .join("Logs")
        .join("HeadroomLocalCommunity");
    if logs_dir.exists() {
        match std::fs::remove_dir_all(&logs_dir) {
            Ok(_) => removed.push(logs_dir.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", logs_dir.display()),
        }
    }
    removed
}

/// Sweep the per-bundle-id directories macOS creates for a GUI app outside the
/// Caches/Preferences locations already handled above: the WKWebView data
/// store, HTTP cookie/storage caches, and saved window state.
#[cfg(target_os = "macos")]
fn remove_macos_bundle_dirs() -> Vec<String> {
    let mut removed = Vec::new();
    let lib = home_dir().join("Library");
    let targets = [
        lib.join("WebKit").join("org.headroomlocal.community"),
        lib.join("HTTPStorages").join("org.headroomlocal.community"),
        lib.join("HTTPStorages")
            .join("org.headroomlocal.community.binarycookies"),
        lib.join("Saved Application State")
            .join("org.headroomlocal.community.savedState"),
    ];
    for path in targets {
        if !path.exists() {
            continue;
        }
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(_) => removed.push(path.display().to_string()),
            Err(err) => log::warn!("cleanup: removing {} failed: {err}", path.display()),
        }
    }
    removed
}

/// Delete every keychain entry Headroom is known to write. Accounts are
/// captured alongside services because macOS keychain queries require both.
fn remove_known_keychain_entries() {
    const ENTRIES: &[(&str, &str)] = &[
        ("org.headroomlocal.community.account", "session-token"),
        ("org.headroomlocal.community.device", "machine-id-digest"),
        ("org.headroomlocal.community.headroom-learn", "openai"),
        ("org.headroomlocal.community.headroom-learn", "anthropic"),
        ("org.headroomlocal.community.headroom-learn", "gemini"),
    ];
    for (service, account) in ENTRIES {
        if let Err(err) = crate::keychain::delete_secret(service, account) {
            log::warn!("cleanup: deleting keychain {service}/{account} failed: {err}");
        }
    }
}

/// Re-applies setup for all clients that were active at the last pause or quit.
pub fn restore_client_setups() {
    let state = load_setup_state();
    let to_restore: Vec<String> = state.remembered_clients.keys().cloned().collect();
    for client_id in to_restore {
        let _ = apply_client_setup(&client_id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
// Container-level default, not per-field: one field added or removed in a
// future build must not fail the whole parse and hand back an empty state,
// which reads as "no clients configured" and orphans every shell block we
// wrote (uninstall then can't find them to remove).
#[serde(rename_all = "camelCase", default)]
struct ClientSetupState {
    configured_clients: BTreeMap<String, String>,
    /// Snapshot of configured_clients taken at last pause/quit, used to restore on next startup.
    #[serde(default)]
    remembered_clients: BTreeMap<String, String>,
    #[serde(default)]
    managed_shell_files: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    remembered_shell_files: BTreeMap<String, Vec<String>>,
    /// Pre-existing custom base URLs (corporate gateway, LiteLLM, Bedrock
    /// proxy) that setup replaced with Headroom's, keyed by client state id.
    /// Restored verbatim on disable/uninstall — setup used to clobber these
    /// and never put them back, silently unrouting enterprise users from
    /// their gateway.
    #[serde(default)]
    preserved_base_urls: BTreeMap<String, String>,
    /// User opted RTK out via the tool status toggle. When true, bootstrap and
    /// client setup skip re-adding the RTK PATH export and Claude Code hook.
    #[serde(default)]
    rtk_disabled: bool,
    /// User turned auto-learning off in the Optimize view. When true the proxy
    /// is spawned without the passive traffic-learning flags.
    #[serde(default)]
    auto_learn_disabled: bool,
    /// Set when an unreadable state file could not be quarantined. While the
    /// original source remains present, refuse to publish a fresh state over
    /// it; otherwise a transient parse failure would destroy the only
    /// recovery copy.
    #[serde(skip)]
    persistence_blocked: bool,
}

fn is_configured(state: &ClientSetupState, client_id: &str) -> bool {
    configured_timestamp(state, client_id).is_some()
}

fn configured_timestamp(state: &ClientSetupState, client_id: &str) -> Option<String> {
    let primary = normalized_setup_id(client_id);
    state.configured_clients.get(primary).cloned()
}

fn load_setup_state() -> ClientSetupState {
    let path = setup_state_path();
    if !path.exists() {
        return ClientSetupState::default();
    }

    // The on-disk file is rewritten by other code paths in this module
    // (apply_client_setup, disable_client_setup, clear_client_setups). Even
    // though `write_setup_state` now publishes via tmp+rename, retry once
    // before giving up: a parse failure on an existing file is almost always
    // a transient race or a partially-written file from an older build, and
    // returning the empty default flips `is_claude_code_enabled` to false,
    // which the tray reads as "Claude Code disconnected" and notifies on.
    match try_load_setup_state(&path) {
        Ok(state) => normalize_setup_state(state),
        Err(first_err) => {
            std::thread::sleep(std::time::Duration::from_millis(15));
            match try_load_setup_state(&path) {
                Ok(state) => normalize_setup_state(state),
                Err(second_err) => {
                    log::warn!(
                        "load_setup_state: failed to read/parse {} twice ({first_err:#}; {second_err:#}); returning default",
                        path.display()
                    );
                    // The next write_setup_state would overwrite the file with
                    // the empty default; keep the original for recovery.
                    let mut state = ClientSetupState::default();
                    state.persistence_blocked =
                        path.exists() && !quarantine_unparsable(&path, "client setup state");
                    state
                }
            }
        }
    }
}

fn try_load_setup_state(path: &Path) -> Result<ClientSetupState> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice::<ClientSetupState>(&bytes)
        .with_context(|| format!("parsing {}", path.display()))
}

fn normalize_setup_state(mut state: ClientSetupState) -> ClientSetupState {
    state.configured_clients = normalize_setup_entries(state.configured_clients);
    state.remembered_clients = normalize_setup_entries(state.remembered_clients);
    state.managed_shell_files = normalize_shell_file_entries(state.managed_shell_files);
    state.remembered_shell_files = normalize_shell_file_entries(state.remembered_shell_files);
    state
}

fn normalize_setup_entries(mut entries: BTreeMap<String, String>) -> BTreeMap<String, String> {
    // codex_gui is a removed id; codex/codex_cli are live again, keep them.
    entries.remove("codex_gui");

    entries
}

fn normalize_shell_file_entries(
    mut entries: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    entries.remove("codex_gui");

    for files in entries.values_mut() {
        dedupe_strings(files);
    }

    entries
}

fn write_setup_state(state: &mut ClientSetupState) -> Result<()> {
    let path = setup_state_path();
    if state.persistence_blocked && path.exists() {
        return Err(anyhow!(
            "refusing to replace {} because its recovery copy could not be created",
            path.display()
        ));
    }
    let payload = serde_json::to_vec_pretty(state).context("serializing client setup state")?;

    atomic_write(&path, &payload)?;
    state.persistence_blocked = false;
    Ok(())
}

/// Write via a sibling tmp file then rename. POSIX rename is atomic, so
/// concurrent readers (other apps parsing their own config, the tray-icon
/// thread calling `is_claude_code_enabled` every 2s) see either the old file
/// or the new one — never a half-written truncate. A plain `fs::write` also
/// leaves a truncated file behind on crash/power loss mid-write, which for
/// user-owned configs (settings.json, config.toml, shell rc files) breaks the
/// user's shell or client startup.
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    // Per-writer unique tmp name. A fixed `<path>.tmp` is shared by concurrent
    // writers to the same file: A renames tmp->path, then B's rename finds its
    // tmp already consumed and fails ENOENT (Sentry RUST-3W / RUST-4W). pid +
    // a process-local counter makes each write's tmp its own.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp_path = {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut s = path.as_os_str().to_os_string();
        s.push(format!(".tmp.{}.{}", std::process::id(), n));
        PathBuf::from(s)
    };
    // Keep the replacement's permissions aligned with the destination. A
    // plain `fs::write` creates the temporary file with the process umask and
    // then `rename` replaces a 0600 credential/config file with (often) 0644.
    // New files start private; existing files retain their exact mode. This is
    // done before any bytes are written so a crash cannot expose a secret in a
    // world-readable temporary file.
    let existing_permissions = std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode = existing_permissions
            .as_ref()
            .map(PermissionsExt::mode)
            .unwrap_or(0o600);
        options.mode(mode);
    }
    let mut file = options.open(&tmp_path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow!("writing {}: {err}", tmp_path.display())
    })?;
    #[cfg(not(unix))]
    if let Some(permissions) = existing_permissions {
        // Windows exposes fewer permission bits, but preserving its readonly
        // flag still prevents an atomic replacement from changing user intent.
        let _ = file.set_permissions(permissions);
    }

    // The io error goes in the *message*, not a source: every caller logs this
    // with `{err}`, which prints only the top context and drops the chain, so
    // Sentry saw "failed to persist usage-counters.json: writing <path>.tmp.N"
    // with no reason at all (RUST-77). Baking the cause in fixes all 50-odd
    // callers at once instead of auditing each log site for `{err:#}`.
    use std::io::Write;
    file.write_all(contents).map_err(|err| {
        // A failed write still leaves the (partial) tmp behind, and the name is
        // unique per write, so nothing ever reclaims it. On a full disk that is
        // one orphan per attempt, each holding whatever bytes did land (RUST-6R).
        let _ = std::fs::remove_file(&tmp_path);
        anyhow!("writing {}: {err}", tmp_path.display())
    })?;
    file.sync_all().map_err(|err| {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow!("flushing {}: {err}", tmp_path.display())
    })?;
    drop(file);
    std::fs::rename(&tmp_path, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp_path); // don't leak the tmp on failure
        anyhow!(
            "renaming {} -> {}: {err}",
            tmp_path.display(),
            path.display()
        )
    })
}

/// Move an unparsable state file aside instead of letting the next write
/// silently overwrite it. Every quarantine gets a unique sibling name, so a
/// second parse failure cannot destroy the first recovery copy. Best-effort:
/// callers use this while recovering optional state and must keep the app
/// usable even when the directory is read-only.
pub(crate) fn quarantine_unparsable(path: &Path, reason: &str) -> bool {
    if !path.exists() {
        return false;
    }
    static QUARANTINE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = QUARANTINE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = Utc::now().format("%Y%m%d%H%M%S");
    let mut suffix = path.as_os_str().to_os_string();
    suffix.push(format!(
        ".corrupt-{}-{}-{}",
        stamp,
        std::process::id(),
        counter
    ));
    let dest = PathBuf::from(suffix);
    match std::fs::rename(path, &dest) {
        Ok(()) => {
            log::warn!(
                "quarantined unparsable {} -> {} ({reason})",
                path.display(),
                dest.display()
            );
            true
        }
        Err(err) => {
            log::warn!(
                "could not quarantine unparsable {} ({reason}); leaving original in place: {err}",
                path.display()
            );
            false
        }
    }
}

fn setup_state_path() -> PathBuf {
    config_file(&app_data_dir(), "client-setup.json")
}

fn default_headroom_root_dir() -> PathBuf {
    app_data_dir().join("headroom")
}

// Windows layout mirrors tool_manager: `rtk.exe` in bin, venv interpreters
// under `Scripts\` with `.exe`. Without this, `managed_rtk_path.exists()` is
// always false on Windows and the RTK shell/hook integration silently skips.
fn default_headroom_rtk_path() -> PathBuf {
    let name = if cfg!(target_os = "windows") {
        "rtk.exe"
    } else {
        "rtk"
    };
    default_headroom_root_dir().join("bin").join(name)
}

fn default_headroom_managed_python_path() -> PathBuf {
    let (dir, name) = if cfg!(target_os = "windows") {
        ("Scripts", "python.exe")
    } else {
        ("bin", "python3")
    };
    default_headroom_root_dir()
        .join("runtime")
        .join("venv")
        .join(dir)
        .join(name)
}

fn resolve_client_shell_targets(state: &ClientSetupState, client_id: &str) -> Result<Vec<PathBuf>> {
    let state_id = normalized_setup_id(client_id);
    let mut targets = shell_targets_from_state(state.managed_shell_files.get(state_id));
    if targets.is_empty() {
        targets = shell_targets_from_state(state.remembered_shell_files.get(state_id));
    }
    targets.extend(discover_managed_shell_targets(&[
        "claude_code",
        "managed_rtk",
        "codex_cli",
    ])?);

    let default_targets = default_shell_targets_for_family(detect_shell_family());
    if targets.is_empty() {
        targets = default_targets;
    } else {
        for file in default_targets {
            if is_profile_file(&file) {
                targets.push(file);
            }
        }
    }

    Ok(dedupe_shell_targets(targets))
}

fn resolve_client_shell_targets_for_cleanup(
    state: &ClientSetupState,
    client_id: &str,
) -> Result<Vec<PathBuf>> {
    let mut targets = resolve_client_shell_targets(state, client_id)?;
    targets.extend(all_shell_paths());
    Ok(dedupe_shell_targets(targets))
}

fn configure_shell_block(
    shell_targets: &[PathBuf],
    block_id: &str,
    block_body: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut changed = Vec::new();
    let mut backups = Vec::new();

    for file in shell_targets {
        let (did_change, backup) = upsert_managed_block(&file, block_id, block_body)?;
        if did_change {
            changed.push(file.display().to_string());
            if let Some(path) = backup {
                backups.push(path.display().to_string());
            }
        }
    }

    Ok((changed, backups))
}

fn ensure_managed_rtk_on_path(
    rtk_path: &Path,
    shell_targets: &[PathBuf],
) -> Result<(Vec<String>, Vec<String>)> {
    let managed_bin_dir = rtk_path.parent().ok_or_else(|| {
        anyhow!(
            "managed RTK path {} is missing a parent directory",
            rtk_path.display()
        )
    })?;
    let path_value = shell_double_quote(&managed_bin_dir.to_string_lossy());
    configure_shell_block(
        shell_targets,
        "managed_rtk",
        &format!("export PATH=\"{path_value}:$PATH\""),
    )
}

fn ensure_claude_code_rtk_hook(
    managed_rtk_path: &Path,
    managed_python_path: &Path,
) -> Result<(Vec<String>, Vec<String>)> {
    let hook_path = headroom_rtk_hook_path();
    let hook_body = build_headroom_rtk_hook(managed_rtk_path, managed_python_path);
    let (hook_changed, hook_backup) = write_file_if_changed(&hook_path, &hook_body, true)?;
    let mut changed_files = Vec::new();
    let mut backup_files = Vec::new();

    if hook_changed {
        changed_files.push(hook_path.display().to_string());
    }
    if let Some(path) = hook_backup {
        backup_files.push(path.display().to_string());
    }

    let (settings_changed, settings_backups) = ensure_claude_settings_hook(
        &hook_path,
        "Bash",
        "headroom-local-community-rtk-rewrite.sh",
    )?;
    changed_files.extend(settings_changed);
    backup_files.extend(settings_backups);

    Ok((changed_files, backup_files))
}

fn markitdown_claude_md_path() -> PathBuf {
    home_dir().join(".claude").join("CLAUDE.md")
}

fn markitdown_codex_agents_path() -> PathBuf {
    codex_home().join("AGENTS.md")
}

/// Office-only nudge for Claude Code, where PDFs are already handled by the
/// PreToolUse(Read) hook.
fn build_markitdown_office_nudge(shim_path: &Path) -> String {
    let bin = format!("\"{}\"", shell_double_quote(&shim_path.to_string_lossy()));
    format!(
        "## Reading Office documents (Headroom MarkItDown)\n\
         Advisory only: follow system/developer/user instructions and active project instructions first.\n\
         When the converter is available, extract Office (.docx, .doc, .pptx, .ppt, .xlsx, .xls) text with\n\
         `{bin} \"<path>\"` via the shell. PDFs may also be handled by the installed Read hook.\n\
         For layout, images, spreadsheet formulas or visual verification, use the relevant\n\
         document skill and rendering tools; Markdown extraction alone is insufficient.\n\
         If conversion fails, report it and use an available reader; do not claim unread content was checked."
    )
}

/// Codex nudge: Codex has no PreToolUse-style hook, so it covers PDF *and*
/// Office formats through the `markitdown` CLI.
fn build_markitdown_codex_nudge(shim_path: &Path) -> String {
    let bin = format!("\"{}\"", shell_double_quote(&shim_path.to_string_lossy()));
    format!(
        "## Reading documents (Headroom MarkItDown)\n\
         Advisory only: follow system/developer/user instructions and active project instructions first.\n\
         When the converter is available, extract text from PDF (.pdf) or Office (.docx, .doc, .pptx, .ppt, .xlsx, .xls) files with\n\
         `{bin} \"<path>\"` in the shell.\n\
         For layout, images, spreadsheet formulas or visual verification, use the relevant\n\
         document skill and rendering tools; Markdown extraction alone is insufficient.\n\
         If conversion fails, report it and use an available reader; do not claim unread content was checked."
    )
}

fn build_serena_usage_nudge() -> &'static str {
    "## Semantic code tools (Headroom Serena)\n\
     Advisory only: follow system/developer/user instructions and active project instructions first.\n\
     For non-trivial coding tasks, consider Serena when it materially helps and its tools are available.\n\
     Keep simple tasks direct. Call\n\
     `initial_instructions` once before code exploration, then call `activate_project`\n\
     with the actual repository when no project is active or the current project is\n\
     wrong. Prefer Serena's symbol, reference, and pattern tools over whole-file reads.\n\
     Skip Serena for non-code tasks and trivial exact edits; do not run onboarding or\n\
     write Serena memories unless the task requires it or the user asks."
}

fn serena_claude_target_exists() -> bool {
    let home = home_dir();
    is_claude_code_enabled()
        || home.join(".claude").is_dir()
        || home.join(".claude.json").is_file()
        || detect_claude_code_client(false).installed
}

fn serena_codex_target_exists() -> bool {
    // A directory is created by many tools before Codex is ever installed.
    // Requiring an enabled connector or a real Codex state marker prevents
    // enabling Serena for another client from creating ~/.codex/AGENTS.md.
    is_codex_enabled() || codex_user_state_exists()
}

/// Add an independently-managed Serena usage nudge to every detected coding
/// client. Serena MCP registration is independent of Headroom proxy routing,
/// so an installed-but-unconnected client still needs the usage hint.
pub fn enable_serena_integration() -> Result<(Vec<String>, Vec<String>)> {
    match write_serena_integration() {
        Ok(result) => Ok(result),
        Err(err) => match disable_serena_integration() {
            Ok(_) => Err(err),
            Err(cleanup_err) => Err(err.context(format!(
                "rolling back partial Serena usage instructions also failed: {cleanup_err:#}"
            ))),
        },
    }
}

fn write_serena_integration() -> Result<(Vec<String>, Vec<String>)> {
    let mut changed_files = Vec::new();
    let mut backup_files = Vec::new();
    let nudge = build_serena_usage_nudge();

    if serena_claude_target_exists() {
        let path = markitdown_claude_md_path();
        let (changed, backup) = upsert_managed_block(&path, "serena", nudge)?;
        if changed {
            changed_files.push(path.display().to_string());
        }
        if let Some(path) = backup {
            backup_files.push(path.display().to_string());
        }
    }

    if serena_codex_target_exists() {
        let path = markitdown_codex_agents_path();
        let (changed, backup) = upsert_managed_block(&path, "serena", nudge)?;
        if changed {
            changed_files.push(path.display().to_string());
        }
        if let Some(path) = backup {
            backup_files.push(path.display().to_string());
        }
    }

    Ok((changed_files, backup_files))
}

/// Remove Serena usage nudges unconditionally. This remains safe after a
/// client is disconnected because the fenced block is the ownership boundary.
pub fn disable_serena_integration() -> Result<bool> {
    let mut changed = remove_managed_block(&markitdown_claude_md_path(), "serena")?;
    changed |= remove_managed_block(&markitdown_codex_agents_path(), "serena")?;
    Ok(changed)
}

/// Enables the MarkItDown addon integration for whichever coding clients are
/// configured through Headroom: Claude Code gets the PDF Read hook plus an
/// Office nudge (managed `~/.claude/CLAUDE.md` block + scoped Bash permission);
/// Codex gets a managed `~/.codex/AGENTS.md` nudge covering PDF and Office (it
/// has no hook mechanism). Idempotent and safe to re-run.
pub fn enable_markitdown_integration(
    markitdown_entrypoint: &Path,
    markitdown_shim: &Path,
    python_path: &Path,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut changed_files = Vec::new();
    let mut backup_files = Vec::new();

    if is_claude_code_enabled() {
        let hook_path = headroom_markitdown_hook_path();
        let hook_body = build_headroom_markitdown_hook(markitdown_entrypoint, python_path);
        let (hook_changed, hook_backup) = write_file_if_changed(&hook_path, &hook_body, true)?;
        if hook_changed {
            changed_files.push(hook_path.display().to_string());
        }
        if let Some(path) = hook_backup {
            backup_files.push(path.display().to_string());
        }

        let (settings_changed, settings_backups) = ensure_claude_settings_hook(
            &hook_path,
            "Read",
            "headroom-local-community-markitdown-read.sh",
        )?;
        changed_files.extend(settings_changed);
        backup_files.extend(settings_backups);

        let claude_md = markitdown_claude_md_path();
        let (md_changed, md_backup) = upsert_managed_block(
            &claude_md,
            "markitdown_office",
            &build_markitdown_office_nudge(markitdown_shim),
        )?;
        if md_changed {
            changed_files.push(claude_md.display().to_string());
        }
        if let Some(path) = md_backup {
            backup_files.push(path.display().to_string());
        }

        if set_markitdown_bash_permission(markitdown_shim, true)? {
            changed_files.push(claude_settings_path().display().to_string());
        }
    }

    if is_codex_enabled() {
        let agents = markitdown_codex_agents_path();
        let (codex_changed, codex_backup) = upsert_managed_block(
            &agents,
            "markitdown",
            &build_markitdown_codex_nudge(markitdown_shim),
        )?;
        if codex_changed {
            changed_files.push(agents.display().to_string());
        }
        if let Some(path) = codex_backup {
            backup_files.push(path.display().to_string());
        }
    }

    Ok((changed_files, backup_files))
}

/// Removes every MarkItDown integration artifact for all clients (Claude Read
/// hook + script + Office nudge + Bash permission, and the Codex AGENTS.md
/// nudge), leaving any RTK hook untouched. Cleanup runs unconditionally so a
/// client that was later disconnected is still scrubbed.
pub fn disable_markitdown_integration(markitdown_shim: &Path) -> Result<bool> {
    let mut changed = remove_pre_tool_use_markers(
        &claude_settings_path(),
        &["headroom-local-community-markitdown-read.sh"],
    )?;
    let hook_path = headroom_markitdown_hook_path();
    if hook_path.exists() {
        std::fs::remove_file(&hook_path)
            .with_context(|| format!("removing {}", hook_path.display()))?;
    }
    changed |= remove_managed_block(&markitdown_claude_md_path(), "markitdown_office")?;
    changed |= set_markitdown_bash_permission(markitdown_shim, false)?;
    changed |= remove_managed_block(&markitdown_codex_agents_path(), "markitdown")?;
    Ok(changed)
}

/// Adds or removes a `Bash(<shim> *)` entry in `permissions.allow` so the Office
/// nudge can run `markitdown` without prompting. Returns whether settings changed.
fn set_markitdown_bash_permission(shim_path: &Path, present: bool) -> Result<bool> {
    let settings_path = claude_settings_path();
    let entry = format!("Bash({} *)", shim_path.display());

    let mut content = if settings_path.exists() {
        let raw = std::fs::read_to_string(&settings_path)
            .with_context(|| format!("reading {}", settings_path.display()))?;
        if raw.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            Value::Object(parse_json_object(&raw, &settings_path)?)
        }
    } else if present {
        Value::Object(Default::default())
    } else {
        return Ok(false);
    };

    let root = content
        .as_object_mut()
        .ok_or_else(|| anyhow!("unable to write Claude permissions settings"))?;
    let allow = root
        .entry("permissions")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("permissions is not an object"))?
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("permissions.allow is not an array"))?;

    let already = allow.iter().any(|v| v.as_str() == Some(entry.as_str()));
    if present == already {
        return Ok(false);
    }
    if present {
        allow.push(Value::String(entry));
    } else {
        allow.retain(|v| v.as_str() != Some(entry.as_str()));
    }

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let _ = backup_if_exists(&settings_path)?;
    atomic_write(
        &settings_path,
        &serde_json::to_vec_pretty(&content).context("serializing Claude permissions settings")?,
    )
    .with_context(|| format!("writing {}", settings_path.display()))?;
    Ok(true)
}

fn disable_codex_cli() -> Result<()> {
    remove_codex_provider_block()?;
    remove_codex_toml_key("openai_base_url", HEADROOM_OPENAI_BASE_URL)?;
    remove_codex_toml_key("openai_base_url", HEADROOM_CHATGPT_BASE_URL)?;
    remove_codex_guard_hook()?;
    let shell_targets = all_shell_paths();
    remove_shell_block(&shell_targets, "codex_cli")?;
    remove_shell_block(&shell_targets, "codex")?;
    Ok(())
}

fn disable_codex_gui() -> Result<()> {
    clear_legacy_codex_gui_launch_env()?;
    Ok(())
}

fn clear_legacy_codex_gui_launch_env() -> Result<()> {
    remove_launchctl_env(&["OPENAI_BASE_URL", "OPENAI_API_BASE"])?;
    Ok(())
}

fn configure_vscode_settings() -> Result<(Vec<String>, Vec<String>, Option<String>)> {
    let (mut changed_files, mut backup_files, replaced) =
        configure_claude_settings_env("ANTHROPIC_BASE_URL", HEADROOM_ANTHROPIC_BASE_URL)?;
    let (legacy_changed, legacy_backups) = remove_legacy_vscode_base_url_keys()?;
    changed_files.extend(legacy_changed);
    backup_files.extend(legacy_backups);
    Ok((changed_files, backup_files, replaced))
}

fn remove_vscode_connector_keys(restore_value: Option<&str>) -> Result<()> {
    remove_claude_settings_env(
        "ANTHROPIC_BASE_URL",
        HEADROOM_ANTHROPIC_BASE_URL,
        restore_value,
    )?;
    remove_legacy_vscode_base_url_keys()?;
    Ok(())
}

fn set_json_string(
    obj: &mut serde_json::Map<String, Value>,
    key: &str,
    expected_value: &str,
) -> bool {
    let next = Value::String(expected_value.to_string());
    match obj.get(key) {
        Some(existing) if existing == &next => false,
        _ => {
            obj.insert(key.to_string(), next);
            true
        }
    }
}

fn remove_json_key_if_matches(
    obj: &mut serde_json::Map<String, Value>,
    key: &str,
    expected_value: &str,
) -> bool {
    match obj.get(key) {
        Some(Value::String(value)) if value == expected_value => obj.remove(key).is_some(),
        _ => false,
    }
}

/// As [`remove_json_key_if_matches`], for callers whose "ours" test is a
/// predicate rather than one literal value (a client's base URL can be any of
/// Headroom's own loopback ports -- see `is_headroom_managed_base_url`).
fn remove_json_key_if(
    obj: &mut serde_json::Map<String, Value>,
    key: &str,
    matches: impl Fn(&str) -> bool,
) -> bool {
    match obj.get(key) {
        Some(Value::String(value)) if matches(value) => obj.remove(key).is_some(),
        _ => false,
    }
}

/// Point `env.<env_key>` at Headroom. The third return element is a
/// pre-existing *foreign* value this write replaced (a corporate gateway,
/// LiteLLM, or Bedrock-proxy URL) — callers must preserve it and restore it
/// on disable instead of just deleting the key.
fn configure_claude_settings_env(
    env_key: &str,
    env_value: &str,
) -> Result<(Vec<String>, Vec<String>, Option<String>)> {
    let settings_path = claude_settings_path();
    let mut content = if settings_path.exists() {
        let raw = std::fs::read_to_string(&settings_path)
            .with_context(|| format!("reading {}", settings_path.display()))?;
        Value::Object(parse_json_object(&raw, &settings_path)?)
    } else {
        Value::Object(Default::default())
    };

    let Some(root) = content.as_object_mut() else {
        return Err(anyhow!("unable to write Claude settings"));
    };

    match root.get("env") {
        None => {
            root.insert("env".into(), Value::Object(Default::default()));
        }
        Some(value) if value.is_object() => {}
        Some(_) => {
            reject_invalid_json_container(&settings_path, "env", "an object")?;
        }
    }

    let Some(env_obj) = root.get_mut("env").and_then(|value| value.as_object_mut()) else {
        return Err(anyhow!("unable to write Claude env settings"));
    };
    if env_obj.get(env_key).is_some_and(|value| !value.is_string()) {
        reject_invalid_json_container(&settings_path, &format!("env.{env_key}"), "a string")?;
    }

    let replaced_foreign_value = env_obj
        .get(env_key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty() && *value != env_value)
        .map(str::to_string);

    let changed = set_json_string(env_obj, env_key, env_value);
    if !changed {
        return Ok((Vec::new(), Vec::new(), None));
    }

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let backup = backup_if_exists(&settings_path)?;
    atomic_write(
        &settings_path,
        &serde_json::to_vec_pretty(&content).context("serializing Claude settings")?,
    )
    .with_context(|| format!("writing {}", settings_path.display()))?;

    Ok((
        vec![settings_path.display().to_string()],
        backup
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
        replaced_foreign_value,
    ))
}

/// Absolute, quoted `bash.exe` for a Windows hook command. Git for Windows
/// only adds `Git\cmd` to PATH in its default setup while `bash.exe` lives in
/// `Git\bin`, so a bare `bash` resolved on a dev box and nowhere else -- the
/// rtk and markitdown hooks installed fine and then silently never fired.
/// Install locations are probed before PATH deliberately: `System32\bash.exe`
/// is WSL, whose filesystem view cannot see the `C:\Users\...` script path.
/// Bare `bash` remains the last resort -- a hook that needs the user to fix
/// their PATH still beats no hook at all.
fn windows_bash_command() -> String {
    let git_bash = ["ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("Git"))
        .chain(
            std::env::var_os("LOCALAPPDATA")
                .map(|root| PathBuf::from(root).join("Programs").join("Git")),
        )
        .map(|root| root.join("bin").join("bash.exe"))
        .find(|candidate| candidate.exists());

    git_bash
        .or_else(|| find_on_path(&["bash"]))
        .map(|path| format!("\"{}\"", path.display()))
        .unwrap_or_else(|| "bash".to_string())
}

/// The command Claude Code runs for a Headroom PreToolUse hook. The hooks are
/// bash scripts; on Windows they are launched through PowerShell, which needs
/// the call operator in front of a quoted interpreter path (see
/// [`join_guard_command`]).
fn hook_shell_command(hook_path: &Path) -> Result<String> {
    if cfg!(target_os = "windows") {
        return Ok(join_guard_command(
            &windows_bash_command(),
            &hook_path.to_string_lossy(),
            true,
        ));
    }
    hook_path
        .to_str()
        .ok_or_else(|| anyhow!("hook path contains invalid UTF-8: {}", hook_path.display()))
        .map(str::to_string)
}

fn ensure_claude_settings_hook(
    hook_path: &Path,
    matcher: &str,
    marker: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let settings_path = claude_settings_path();
    let mut content = if settings_path.exists() {
        let raw = std::fs::read_to_string(&settings_path)
            .with_context(|| format!("reading {}", settings_path.display()))?;
        Value::Object(parse_json_object(&raw, &settings_path)?)
    } else {
        Value::Object(Default::default())
    };

    let hook_command = hook_shell_command(hook_path)?;
    let already_present = claude_hook_present_in_value(&content, &hook_command);
    if already_present {
        return Ok((Vec::new(), Vec::new()));
    }

    let Some(root) = content.as_object_mut() else {
        return Err(anyhow!("unable to write Claude hook settings"));
    };

    match root.get("hooks") {
        None => {
            root.insert("hooks".into(), Value::Object(Default::default()));
        }
        Some(value) if value.is_object() => {}
        Some(_) => {
            reject_invalid_json_container(&settings_path, "hooks", "an object")?;
        }
    }

    let Some(hooks_obj) = root
        .get_mut("hooks")
        .and_then(|value| value.as_object_mut())
    else {
        return Err(anyhow!("unable to write Claude hooks settings"));
    };
    match hooks_obj.get("PreToolUse") {
        None => {
            hooks_obj.insert("PreToolUse".into(), Value::Array(Vec::new()));
        }
        Some(value) if value.is_array() => {}
        Some(_) => {
            reject_invalid_json_container(&settings_path, "hooks.PreToolUse", "an array")?;
        }
    }

    let Some(pre_tool_use) = hooks_obj
        .get_mut("PreToolUse")
        .and_then(|value| value.as_array_mut())
    else {
        return Err(anyhow!("unable to write Claude PreToolUse hooks"));
    };

    pre_tool_use.retain(|entry| !entry_contains_hook(entry, marker));
    pre_tool_use.push(serde_json::json!({
        "matcher": matcher,
        "hooks": [{
            "type": "command",
            "command": hook_command
        }]
    }));

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let backup = backup_if_exists(&settings_path)?;
    atomic_write(
        &settings_path,
        &serde_json::to_vec_pretty(&content).context("serializing Claude hook settings")?,
    )
    .with_context(|| format!("writing {}", settings_path.display()))?;

    Ok((
        vec![settings_path.display().to_string()],
        backup
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    ))
}

/// Undo `configure_claude_settings_env`: if `env.<env_key>` still equals
/// Headroom's value, put back `restore_value` (the user's pre-Headroom
/// gateway URL) when one was preserved, otherwise delete the key. A key that
/// no longer matches Headroom's value was changed by the user and is left
/// alone.
fn remove_claude_settings_env(
    env_key: &str,
    expected_value: &str,
    restore_value: Option<&str>,
) -> Result<()> {
    let settings_path = claude_settings_path();
    if !settings_path.exists() {
        return Ok(());
    }

    let raw = std::fs::read_to_string(&settings_path)
        .with_context(|| format!("reading {}", settings_path.display()))?;
    let mut root = parse_json_object(&raw, &settings_path)?;
    let mut changed = false;
    // Ours is the expected value *or* any of our own loopback ports. The
    // second case is not hypothetical: the cc-switch reconciler rewrote this
    // key to the backend port, and a teardown that only knew 6867 left the
    // client pointing at a port nothing serves.
    let owned = |value: &str| value == expected_value || is_headroom_managed_base_url(value);

    if let Some(Value::Object(env_obj)) = root.get_mut("env") {
        match restore_value {
            Some(original)
                if env_obj
                    .get(env_key)
                    .and_then(|v| v.as_str())
                    .is_some_and(owned) =>
            {
                env_obj.insert(env_key.into(), Value::String(original.to_string()));
                changed = true;
            }
            _ => {
                changed |= remove_json_key_if(env_obj, env_key, owned);
            }
        }
        if env_obj.is_empty() {
            root.remove("env");
            changed = true;
        }
    }

    if !changed {
        return Ok(());
    }

    let _ = backup_if_exists(&settings_path)?;
    atomic_write(
        &settings_path,
        &serde_json::to_vec_pretty(&Value::Object(root))
            .context("serializing Claude settings for connector removal")?,
    )?;

    Ok(())
}

fn claude_hook_present_in_value(content: &Value, hook_path: &str) -> bool {
    content
        .get("hooks")
        .and_then(|value| value.get("PreToolUse"))
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries.iter().any(|entry| {
                entry
                    .get("hooks")
                    .and_then(|hooks| hooks.as_array())
                    .map(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command")
                                .and_then(|command| command.as_str())
                                .map(|command| command == hook_path)
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn entry_contains_hook(entry: &Value, hook_fragment: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|hooks| hooks.as_array())
        .map(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .is_some_and(|c| command_contains(c, hook_fragment))
            })
        })
        .unwrap_or(false)
}

/// Match a hook `command` against a fragment, tolerating both the Claude string
/// form (`"/usr/bin/python3 /path/guard.py"`) and the argv-array form Codex
/// normalizes to (`["python3", "/path/guard.py"]`). Callers pass the guard
/// *script path* as the fragment so a differing interpreter (system vs Homebrew
/// python3) can't leave the entry behind when the script is deleted.
fn command_contains(command: &Value, fragment: &str) -> bool {
    match command {
        Value::String(s) => s.contains(fragment),
        Value::Array(parts) => parts
            .iter()
            .filter_map(Value::as_str)
            .any(|p| p.contains(fragment)),
        _ => false,
    }
}

fn remove_legacy_vscode_base_url_keys() -> Result<(Vec<String>, Vec<String>)> {
    // Deliberately the macOS path only. These keys were written into VS Code's
    // settings.json by macOS-only builds; the connector has since moved to
    // ~/.claude/settings.json, which is where every platform reads and writes
    // today. No Linux or Windows build ever wrote a key for this to clean up,
    // so there is nothing to make platform-aware here.
    let settings_path = home_dir()
        .join("Library")
        .join("Application Support")
        .join("Code")
        .join("User")
        .join("settings.json");
    if !settings_path.exists() {
        return Ok((Vec::new(), Vec::new()));
    }

    let raw = std::fs::read_to_string(&settings_path)
        .with_context(|| format!("reading {}", settings_path.display()))?;
    let mut obj = parse_json_object(&raw, &settings_path)?;

    let mut changed = false;
    changed |= remove_json_key_if_matches(&mut obj, "openai.baseUrl", HEADROOM_PROXY_URL);
    changed |= remove_json_key_if_matches(&mut obj, "anthropic.baseUrl", HEADROOM_PROXY_URL);
    if !changed {
        return Ok((Vec::new(), Vec::new()));
    }

    let backup = backup_if_exists(&settings_path)?;
    atomic_write(
        &settings_path,
        &serde_json::to_vec_pretty(&Value::Object(obj))
            .context("serializing VS Code settings for legacy key cleanup")?,
    )?;

    Ok((
        vec![settings_path.display().to_string()],
        backup
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    ))
}

fn codex_config_toml_path() -> PathBuf {
    codex_home().join("config.toml")
}

// The managed Codex config is split across two marker blocks so each lands in
// the correct TOML scope. `model_provider`/`openai_base_url` are root keys: a
// bare key belongs to the most recently opened `[table]` above it, so appending
// them at end-of-file (as a naive text upsert does) silently absorbs them into
// whatever table the user's config happens to end in (e.g. `[features]`, whose
// values must be booleans), producing
// `invalid type: string "headroom", expected a boolean in features`. The root
// keys therefore go in a block at the *top* of the file (nothing above ⇒ root
// scope), and the `[model_providers.headroom_local_community]` table goes in a block at the
// *end*. `requires_openai_auth` is emitted only for account-backed ChatGPT
// users: the
// flag is what makes Codex render the account menu (profile/email/plan/usage),
// but it also forces Codex to demand an OpenAI OAuth login (issue #406), which
// would break users authenticated with an OpenAI API key. See
// `codex_requires_openai_auth`.
const CODEX_ROOT_BLOCK_ID: &str = "codex_cli";
const CODEX_TABLE_BLOCK_ID: &str = "codex_cli_provider";

// Codex permanently stamps every thread with the `model_provider` it ran under,
// and its history/projects menu filters threads by the *active* provider set. So
// threads created through Headroom (provider `headroom`) disappear from the menu
// when Codex runs natively (provider `openai`) and vice-versa. To keep the menu
// whole we retag threads to match whichever provider is currently active:
// `openai -> headroom` on connect, `headroom -> openai` on disconnect/quit.
const CODEX_HEADROOM_PROVIDER: &str = "headroom_local_community";
const CODEX_NATIVE_PROVIDER: &str = "openai";
const CODEX_OFFICIAL_PROVIDER: &str = "headroom";

/// Root providers whose request semantics Headroom understands well enough to
/// take over temporarily.  A user-named provider may point at a corporate
/// gateway, a local relay, or a different credential scheme; replacing it with
/// our OpenAI-compatible route would silently discard that choice. Keep this
/// allowlist deliberately small and explicit.
const CODEX_TAKEOVER_PROVIDER_ALLOWLIST: &[&str] = &[
    CODEX_NATIVE_PROVIDER,
    CODEX_OFFICIAL_PROVIDER,
    CODEX_HEADROOM_PROVIDER,
];

/// Local ports used by released Headroom/Community builds before the detached
/// Codex router moved to 6891. These are migration sources only: an arbitrary
/// loopback port can still be a user-owned gateway and must not be replaced.
const CODEX_LEGACY_HEADROOM_PORTS: &[u16] = &[6767, 6768, 6769, 6790, 6867, 8787];

/// Directories Codex is known to keep its state store in: the v148 GUI uses
/// `<codex_home>/sqlite/`, the CLI/TUI uses `<codex_home>/`.
fn codex_state_dirs() -> Vec<PathBuf> {
    let codex = codex_home();
    vec![codex.join("sqlite"), codex]
}

/// True when Codex keeps (or kept) a sqlite-backed thread store on this machine,
/// so a *missing* recognized `state_<N>.sqlite` means the store moved/renamed --
/// the case worth a signal. The only evidence we trust is a `state_*.sqlite`-shaped
/// file in `<codex_home>/sqlite/` (GUI) or `<codex_home>/` (CLI/TUI), including a
/// renamed one whose version no longer parses -- exactly the relocation we want to
/// catch. The bare `sqlite/` dir is NOT evidence: it also holds unrelated stores
/// (logs/goals/memories), so a fresh install with those but no thread store would
/// otherwise false-fire "store moved" (Sentry RUST-3R). CLI-only or pre-sqlite
/// installs with just `config.toml`/`sessions/` stay silent -- nothing to split.
fn codex_sqlite_store_expected() -> bool {
    codex_state_dirs().iter().any(|dir| {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries.flatten().any(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("state_") && n.ends_with(".sqlite"))
                })
            })
            .unwrap_or(false)
    })
}

/// Discover every `*.sqlite` file under the known Codex dirs. The thread store's
/// *filename* has changed across Codex versions (`state_5.sqlite`, and whatever
/// comes next), so we no longer couple discovery to a name scheme: every sqlite
/// candidate is handed to `retag_one_codex_db`, which identifies the real store
/// by its `threads` table and no-ops on anything else (logs/goals/memories). A
/// rename can no longer silently split the history menu. A missing dir
/// (`read_dir` error) is skipped. Paths are deduped in case the two dirs ever
/// resolve to the same place.
fn discover_codex_state_dbs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for dir in codex_state_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("sqlite")
                && seen.insert(path.clone())
            {
                out.push(path);
            }
        }
    }
    out
}

/// Best-effort retag of Codex thread provider tags so the history menu stays
/// whole across the Headroom proxy boundary. Never fails the caller: a missing
/// store, a missing `threads` table, or a DB locked by a running Codex is logged
/// and skipped. Only rows whose `model_provider` equals `from` are touched, so
/// third-party providers are left alone.
fn retag_codex_thread_providers(from: &str, to: &str) {
    let mut found_thread_store = false;
    for path in discover_codex_state_dbs() {
        match retag_one_codex_db(&path, from, to) {
            // No `threads` table: unrelated sqlite store (logs/goals/memories).
            Ok(None) => {}
            Ok(Some(n)) => {
                found_thread_store = true;
                if n > 0 {
                    log::info!(
                        "codex retag {from}->{to}: {n} thread(s) in {}",
                        path.display()
                    );
                }
            }
            Err(e) => log::warn!(
                "codex retag {from}->{to} skipped for {}: {e}",
                path.display()
            ),
        }
    }
    // A `state_*.sqlite`-shaped file with no `threads` table means Codex renamed
    // the table itself (discovery already survives a file rename). Only flag when
    // the store-shaped name is present, so a clean or CLI-only / pre-sqlite
    // machine -- or one with just logs/goals/memories DBs -- stays silent
    // (Sentry RUST-3R). This is the last remaining schema-drift signal worth a
    // release (Sentry RUST-43).
    if !found_thread_store && codex_sqlite_store_expected() {
        log::warn!(
            "codex retag {from}->{to}: a state_*.sqlite is present but has no \
             `threads` table under {dirs:?}; the history menu may split. Codex \
             likely renamed the table.",
            dirs = codex_state_dirs(),
        );
    }
}

fn retag_one_codex_db(path: &Path, from: &str, to: &str) -> rusqlite::Result<Option<usize>> {
    use rusqlite::OptionalExtension;

    let conn = rusqlite::Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(750))?;
    // No-op (without erroring) on builds whose store lacks the threads table.
    let has_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'threads'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(None);
    }
    conn.execute(
        "UPDATE threads SET model_provider = ?2 WHERE model_provider = ?1",
        rusqlite::params![from, to],
    )
    .map(Some)
}

/// Retag Codex threads back to the native provider. Exposed for the app-quit
/// hook in `lib.rs`, which covers exit paths (Cmd-Q, dock quit, signals) that
/// bypass `clear_client_setups` and therefore the disconnect retag.
pub fn retag_codex_threads_to_native() {
    // Nothing to hand back when the route was never ours.
    if codex_route_is_ours() {
        retag_codex_thread_providers(CODEX_HEADROOM_PROVIDER, CODEX_NATIVE_PROVIDER);
    }
}

/// Point Codex's thread provider tags at whoever actually routes Codex.
/// Exposed for the app-launch hook in `lib.rs`, which must undo the quit-time
/// native retag on the exit paths (Cmd-Q, dock quit, app-update restart) that
/// never populate `remembered_clients` and are therefore skipped by
/// `restore_client_setups`.
///
/// Ownership decides the direction, because the tag is a claim about where
/// those threads are served. When another manager owns the route -- Cockpit
/// Tools writes `model_provider = "codex_local_access"`, the official Headroom
/// build writes `headroom` -- Headroom is not intercepting, so threads it
/// relabelled in an earlier run are wrong and go back to native; the tags that
/// tool wrote for its own threads (`codex_local_access`) are never touched.
/// Pulling the user's history into a provider menu Headroom does not serve is
/// exactly the kind of "fix" that breaks the other tool (observed live
/// 2026-09-10: 45 threads relabelled while Cockpit owned the route).
pub fn reconcile_codex_thread_providers_on_launch() {
    if codex_route_is_ours() {
        retag_codex_thread_providers(CODEX_NATIVE_PROVIDER, CODEX_HEADROOM_PROVIDER);
    } else {
        retag_codex_thread_providers(CODEX_HEADROOM_PROVIDER, CODEX_NATIVE_PROVIDER);
    }
}

/// Whether the root `model_provider` currently routes Codex through Headroom.
/// The authority for every thread-tag write: a tag is a claim about where the
/// thread is served, and Headroom may only make that claim while it is the one
/// serving it.
pub fn codex_route_is_ours() -> bool {
    let Ok(content) = std::fs::read_to_string(codex_config_toml_path()) else {
        return false;
    };
    codex_root_model_provider(&content).as_deref() == Some(CODEX_HEADROOM_PROVIDER)
}

fn codex_root_keys_body(chatgpt_auth: bool) -> String {
    format!(
        "model_provider = \"headroom_local_community\"\n\
         openai_base_url = \"{base}\"",
        base = crate::codex_router::codex_router_base_url(chatgpt_auth),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexAuthRouting {
    /// Public OpenAI API-key mode. Requests belong on `/v1` and Codex should
    /// not be sent through its ChatGPT account-login flow.
    OpenAiApi,
    /// A host-supplied Codex backend credential (`headers` or
    /// `agentIdentity`). These modes use the ChatGPT backend URL, but the host
    /// owns authentication, so Codex must not force an interactive login.
    ChatGptBackend,
    /// A user/account-backed ChatGPT credential. These modes use the ChatGPT
    /// backend URL and need `requires_openai_auth = true` for account features.
    ChatGptAccount,
}

fn normalized_codex_auth_mode(mode: &str) -> String {
    mode.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Classify the credential once, keeping endpoint selection separate from the
/// `requires_openai_auth` capability flag. Codex's official registry routes
/// `headers` and `agentIdentity` through the ChatGPT backend too, but those
/// externally supplied credentials are not human account sessions.
fn codex_auth_routing() -> CodexAuthRouting {
    let access_token_present = std::env::var("CODEX_ACCESS_TOKEN")
        .ok()
        .is_some_and(|token| !token.trim().is_empty());
    let api_key_present = std::env::var("OPENAI_API_KEY")
        .ok()
        .is_some_and(|key| !key.trim().is_empty());
    let path = codex_home().join("auth.json");
    let obj = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_object().cloned());
    let Some(obj) = obj else {
        return if access_token_present && !api_key_present {
            CodexAuthRouting::ChatGptAccount
        } else {
            CodexAuthRouting::OpenAiApi
        };
    };
    // Codex records the active method explicitly; trust it when present. The
    // token-bearing ChatGPT variants have appeared under several spellings in
    // desktop/CLI releases, so normalize punctuation before matching. An
    // explicit API-key mode wins over stale OAuth tokens that may remain in the
    // same file.
    if let Some(mode) = obj.get("auth_mode").and_then(Value::as_str) {
        let normalized = normalized_codex_auth_mode(mode);
        if normalized == "apikey" || normalized == "codexapikey" {
            return CodexAuthRouting::OpenAiApi;
        }
        if normalized.starts_with("chatgpt") || normalized == "personalaccesstoken" {
            return CodexAuthRouting::ChatGptAccount;
        }
        if matches!(normalized.as_str(), "headers" | "agentidentity") {
            return CodexAuthRouting::ChatGptBackend;
        }
        // Unknown explicit modes must not be guessed as OAuth from a stale
        // environment variable; continue to the token/account evidence below.
    }
    // Older auth.json files predate `auth_mode`: infer ChatGPT mode from the
    // presence of an OAuth account id.
    let tokens = obj.get("tokens").and_then(Value::as_object);
    if tokens
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
    {
        return CodexAuthRouting::ChatGptAccount;
    }
    // Newer Codex writes an auth.json with neither `auth_mode` nor a
    // top-level `tokens.account_id`: the account identity lives only in the
    // `id_token` claims (upstream #3206 / #3212). Those configs read as
    // API-key mode, so `requires_openai_auth` is omitted, Codex attaches no
    // Authorization header, and every request 401s with "Missing bearer".
    // The payload is decoded, not verified: it is a local file the user
    // already owns, and the result only picks which key we write into their
    // own config.toml. An API-key user has no ChatGPT id_token, so this
    // cannot resurrect the forced-OAuth-login regression in #406. Keep this
    // fallback outside the optional `tokens` object because some Codex
    // releases write the token at the top level.
    let id_token = tokens
        .and_then(|tokens| tokens.get("id_token"))
        .and_then(Value::as_str)
        .or_else(|| obj.get("id_token").and_then(Value::as_str));
    if id_token
        .and_then(|token| {
            crate::proxy_intercept::decode_codex_auth_claim(token, "chatgpt_account_id")
        })
        .is_some_and(|id| !id.trim().is_empty())
    {
        return CodexAuthRouting::ChatGptAccount;
    }

    // Headless/IDE integrations can provide a Codex access token without
    // writing auth.json. Treat it as ChatGPT OAuth only when no explicit API
    // key is present; an API key is the native public `/v1` credential.
    if access_token_present && !api_key_present {
        CodexAuthRouting::ChatGptAccount
    } else {
        CodexAuthRouting::OpenAiApi
    }
}

/// Whether the active credential uses Codex's ChatGPT backend URL. This is
/// intentionally broader than `requires_openai_auth`: host-supplied
/// `headers`/`agentIdentity` credentials also use that backend.
fn codex_uses_chatgpt_auth() -> bool {
    matches!(
        codex_auth_routing(),
        CodexAuthRouting::ChatGptBackend | CodexAuthRouting::ChatGptAccount
    )
}

fn codex_requires_openai_auth() -> bool {
    matches!(codex_auth_routing(), CodexAuthRouting::ChatGptAccount)
}

fn codex_provider_table_body(chatgpt_backend: bool, requires_openai_auth: bool) -> String {
    let mut body = format!(
        concat!(
            "[model_providers.headroom_local_community]\n",
            "# Codex currently keys OpenAI-only capabilities (including remote\n",
            "# Responses compaction) off this exact provider name. Keep the\n",
            "# user-facing provider id in the table key while preserving those\n",
            "# native capabilities through the stable local router.\n",
            "name = \"OpenAI\"\n",
            "base_url = \"{base}\"\n",
            "supports_websockets = false\n",
            "supports_standalone_web_search = true",
        ),
        base = crate::codex_router::codex_router_base_url(chatgpt_backend),
    );
    if requires_openai_auth {
        body.push_str("\nrequires_openai_auth = true");
    }
    body
}

fn codex_marker_block(block_id: &str, body: &str) -> String {
    format!("# >>> headroom-local-community:{block_id} >>>\n{body}\n# <<< headroom-local-community:{block_id} <<<\n")
}

/// Remove every Headroom-managed artifact from Codex `config.toml` text: both
/// managed marker blocks, plus any orphan root keys an older (buggy) build may
/// have left absorbed into a preceding table. Leaves all other content intact.
fn strip_codex_managed_toml(content: &str) -> String {
    let without_blocks = strip_codex_provider_block_keeping_foreign(&strip_marker_block(
        content,
        CODEX_ROOT_BLOCK_ID,
    ));
    let openai_orphan_prefix = "openai_base_url = \"http://127.0.0.1:";
    without_blocks
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed == "model_provider = \"headroom_local_community\""
                || (trimmed.starts_with(openai_orphan_prefix)
                    && (trimmed.ends_with("/v1\"") || trimmed.ends_with("/backend-api/codex\""))))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pure-text removal of every `# >>> headroom-local-community:<id> >>> ... <<<` block. Loops so
/// a config that already holds duplicate managed blocks (interrupted write,
/// older build) is fully cleaned, not left with one survivor that regenerates.
fn strip_marker_block(content: &str, block_id: &str) -> String {
    strip_namespaced_marker_block(content, "headroom-local-community", block_id)
}

/// Remove the managed `codex_cli_provider` block while keeping any foreign table
/// another tool appended between our markers.
///
/// A different provider manager can land its own `[model_providers.<id>]` table
/// inside our markers (observed with Cockpit Tools' `codex_local_access`).
/// Dropping the whole block would silently delete that tool's configuration, so
/// only Headroom's own table is removed and everything else is handed back.
fn strip_codex_provider_block_keeping_foreign(content: &str) -> String {
    // Only the Community namespace: the official `headroom` provider table is
    // deliberately kept as the rollback source for `model_provider = "headroom"`.
    let namespace = "headroom-local-community";
    let start = format!("# >>> {namespace}:{CODEX_TABLE_BLOCK_ID} >>>");
    let end = format!("# <<< {namespace}:{CODEX_TABLE_BLOCK_ID} <<<");
    let mut out = content.to_string();
    loop {
        let (Some(start_idx), Some(end_idx)) = (out.find(&start), out.find(&end)) else {
            break;
        };
        if start_idx >= end_idx {
            break;
        }
        let inner = out[start_idx + start.len()..end_idx].to_string();
        let kept = drop_headroom_provider_table(&inner);
        out.replace_range(start_idx..end_idx + end.len(), kept.trim_end_matches('\n'));
    }
    // An orphan closing marker carries no content to salvage.
    out.replace(&end, "")
}

/// Everything in `inner` except Headroom's own provider table. A table runs from
/// its header to the next header; the header and the keys under it are ours.
fn drop_headroom_provider_table(inner: &str) -> String {
    let mut kept = String::new();
    let mut dropping = false;
    for line in inner.lines() {
        let header = line.split('#').next().unwrap_or("").trim();
        if header.starts_with('[') && header.ends_with(']') {
            // Only our own table: any other table inside the block belongs to a
            // different tool and must be handed back.
            dropping = header == "[model_providers.headroom_local_community]";
            if dropping {
                continue;
            }
        }
        if dropping {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

fn strip_namespaced_marker_block(content: &str, namespace: &str, block_id: &str) -> String {
    let start = format!("# >>> {namespace}:{block_id} >>>");
    let end = format!("# <<< {namespace}:{block_id} <<<");
    let mut out = content.to_string();
    loop {
        let Some(start_idx) = out.find(&start) else {
            return out.replace(&end, "");
        };
        let Some(end_idx) = out.find(&end) else {
            break;
        };
        if end_idx < start_idx {
            out.replace_range(end_idx..end_idx + end.len(), "");
            continue;
        }
        let tail = out[end_idx + end.len()..]
            .trim_start_matches('\n')
            .to_string();
        let head = out[..start_idx].trim_end().to_string();
        let mut rebuilt = String::with_capacity(out.len());
        rebuilt.push_str(&head);
        if !rebuilt.is_empty() && !tail.is_empty() {
            rebuilt.push('\n');
        }
        rebuilt.push_str(&tail);
        out = rebuilt;
    }
    out
}

/// Read the root-scope `model_provider` value from a Codex config. A
/// `model_provider` inside a `[profiles.x]`/`[model_providers.x]` table belongs
/// to that table, not the global route. Keep this parser text-only so the
/// detached router can inspect a stale config without loading credentials or
/// invoking Codex.
fn codex_root_model_provider(content: &str) -> Option<String> {
    let mut in_root = true;
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_root = false;
            continue;
        }
        if !in_root {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "model_provider" {
                let name = value.trim().trim_matches('"');
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Read every root-scope `openai_base_url` assignment without parsing the
/// whole Codex config. Older Headroom builds can leave schema-invalid (but
/// repairable) tables behind, so takeover cannot depend on a full TOML parse.
/// Values themselves are still parsed as TOML strings before they are trusted.
fn codex_root_openai_base_urls(content: &str) -> Result<Vec<String>> {
    let mut in_root = true;
    let mut values = Vec::new();
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_root = false;
            continue;
        }
        if !in_root {
            continue;
        }
        let Some((key, raw_value)) = raw.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let key_matches = key == "openai_base_url"
            || key.strip_prefix('"').and_then(|key| key.strip_suffix('"'))
                == Some("openai_base_url")
            || key
                .strip_prefix('\'')
                .and_then(|key| key.strip_suffix('\''))
                == Some("openai_base_url");
        if !key_matches {
            continue;
        }

        let snippet = format!("value = {raw_value}");
        let value = snippet
            .parse::<toml::Value>()
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("value")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
            })
            .ok_or_else(|| {
                anyhow!("refusing to replace an unrecognized Codex root openai_base_url value")
            })?;
        values.push(value);
    }
    Ok(values)
}

/// A takeover may replace only Codex's native OpenAI route or routes written
/// by known Headroom releases. A different URL can carry corporate gateway
/// semantics and credentials that must never be redirected to OpenAI.
fn codex_takeover_base_url_allowed(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value.trim()) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }

    let path = url.path().trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    if url.scheme() == "https"
        && url.host_str() == Some("api.openai.com")
        && url.port_or_known_default() == Some(443)
    {
        return matches!(path, "/" | "/v1");
    }

    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
        return false;
    }
    let Some(port) = url.port() else {
        return false;
    };
    if port == crate::codex_router::CODEX_ROUTER_PORT {
        return matches!(path, "/v1" | "/backend-api/codex");
    }
    CODEX_LEGACY_HEADROOM_PORTS.contains(&port)
        && matches!(path, "/" | "/v1" | "/backend-api/codex")
}

/// A native/direct fallback may use the canonical OpenAI endpoint or the
/// current stable router only. Legacy Headroom ports are valid *migration*
/// sources during setup, but forwarding a crashed session to one of those
/// ports would just recreate the dead-route failure.
fn codex_native_fallback_base_url_allowed(value: &str) -> bool {
    if !codex_takeover_base_url_allowed(value) {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(value.trim()) else {
        return false;
    };
    if url.scheme() == "https"
        && url.host_str() == Some("api.openai.com")
        && url.port_or_known_default() == Some(443)
    {
        return true;
    }
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port() == Some(crate::codex_router::CODEX_ROUTER_PORT)
        && matches!(
            url.path().trim_end_matches('/'),
            "/v1" | "/backend-api/codex"
        )
}

fn validate_codex_openai_base_url_for_takeover(content: &str) -> Result<()> {
    for value in codex_root_openai_base_urls(content)? {
        if !codex_takeover_base_url_allowed(&value) {
            bail!(
                "refusing to replace custom Codex openai_base_url; only native OpenAI and known Headroom routes may be migrated"
            );
        }
    }
    Ok(())
}

/// The root-scope `model_provider` value if it is foreign to Community. This
/// is the Codex analog of a foreign `ANTHROPIC_BASE_URL`: older versions
/// preserved it for restore-on-disable, while current setup refuses to replace
/// an unknown provider (see [`codex_takeover_provider_allowed`]).
fn codex_foreign_model_provider(content: &str) -> Option<String> {
    codex_root_model_provider(content).filter(|name| name != CODEX_HEADROOM_PROVIDER)
}

/// Root `model_provider` in `~/.codex/config.toml` when another tool owns the
/// Codex route. `None` when the key is absent, unreadable, or already ours.
///
/// Lets the UI tell "another manager is in charge" apart from "Headroom failed
/// to write its block", so a coexist state never surfaces as a broken setup.
pub fn codex_external_provider() -> Option<String> {
    let path = codex_config_toml_path();
    let content = std::fs::read_to_string(path).ok()?;
    codex_foreign_model_provider(&content).map(|id| codex_provider_display_name(&id, &content))
}

/// Product names for the provider managers Headroom recognizes on sight.
///
/// *Which* tool owns the Codex route is a class, not a product: any manager
/// that rewrites the root `model_provider` gets the same treatment -- Headroom
/// names the owner, refuses to clobber it without consent, records it, and
/// restores it exactly on disable. Nothing else in that path keys off a
/// specific tool, so supporting a new one is a label at most.
const CODEX_FOREIGN_OWNER_NAMES: &[(&str, &str)] = &[("codex_local_access", "Cockpit")];

/// Human label for whoever owns Codex's route.
///
/// Both the coexist note and the takeover confirmation interpolate this value
/// into a sentence, so a raw TOML table key (`codex_local_access`) reads as
/// jargon where the user expects the name of the tool they installed. Ids we do
/// not recognize are still *named* -- from the `name` the other tool declared
/// for its own provider table in the same config -- rather than guessed at: a
/// hand-written provider keeps its own id, and the id always stays in the label
/// because it is the string support has to grep for.
fn codex_provider_display_name(id: &str, config: &str) -> String {
    match id {
        CODEX_OFFICIAL_PROVIDER => return "Headroom (official)".to_string(),
        CODEX_NATIVE_PROVIDER => return "OpenAI".to_string(),
        CODEX_HEADROOM_PROVIDER => return "Headroom".to_string(),
        _ => {}
    }
    let known = CODEX_FOREIGN_OWNER_NAMES
        .iter()
        .find(|(candidate, _)| *candidate == id)
        .map(|(_, name)| (*name).to_string());
    match known.or_else(|| codex_provider_table_name(config, id)) {
        Some(name) if name != id => format!("{name} ({id})"),
        _ => id.to_string(),
    }
}

/// The `name` another tool declared for its own Codex provider table.
///
/// Hand-rolled for the same reason as `codex_root_model_provider`: a table
/// another manager owns may be schema-invalid for our parser while being
/// exactly what Codex reads, and *naming* the owner must never be the step that
/// fails. Values that would not read as a name in a one-line status string
/// (multi-line or runaway) are dropped so the id is shown instead.
fn codex_provider_table_name(config: &str, provider_id: &str) -> Option<String> {
    let header = format!("[model_providers.{provider_id}]");
    let mut in_table = false;
    for raw in config.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_table = line == header;
            continue;
        }
        if !in_table {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "name" {
            continue;
        }
        let name = value.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        if name.is_empty() || name.len() > 64 {
            return None;
        }
        return Some(name.to_string());
    }
    None
}

fn codex_takeover_provider_allowed(provider: &str) -> bool {
    CODEX_TAKEOVER_PROVIDER_ALLOWLIST
        .iter()
        .any(|allowed| provider == *allowed)
}

/// Return the current root provider for the detached Codex router. The helper
/// intentionally exposes only the provider identifier; it never reads or
/// returns the provider table's base URL, API key, or OAuth token.
#[cfg(test)]
pub(crate) fn codex_root_provider_for_router() -> Option<String> {
    let path = codex_config_toml_path();
    let content = std::fs::read_to_string(path).ok()?;
    codex_root_model_provider(&content)
}

/// Whether a direct fallback may safely use the native OpenAI/ChatGPT
/// endpoints. Unknown custom providers *and* custom root `openai_base_url`
/// values are denied because sending their credentials to api.openai.com would
/// silently change the user's route. Missing root config is allowed for
/// legacy/API-key sessions whose route is supplied through environment
/// variables instead.
pub(crate) fn codex_root_provider_allows_native_fallback() -> bool {
    let path = codex_config_toml_path();
    let Ok(content) = std::fs::read_to_string(path) else {
        // A missing file is a valid API-key setup when OPENAI_BASE_URL is
        // supplied by the shell/environment rather than config.toml.
        return true;
    };
    let provider_allowed = codex_root_model_provider(&content)
        .as_deref()
        // The legacy official `headroom` provider is safe to migrate *from*,
        // but its table may still point at an old local port. The root URL
        // check below decides whether that stale route may be treated as
        // native; an unknown provider is always denied.
        .map(|provider| matches!(provider, CODEX_NATIVE_PROVIDER | CODEX_HEADROOM_PROVIDER))
        .unwrap_or(true);
    if !provider_allowed {
        return false;
    }
    // A user can keep the native provider name while overriding its root URL
    // with a corporate gateway. Validate that URL too; otherwise the detached
    // helper would silently send the gateway's credential to OpenAI after a
    // Headroom crash.
    codex_root_openai_base_urls(&content)
        .map(|values| {
            values
                .iter()
                .all(|value| codex_native_fallback_base_url_allowed(value))
        })
        .unwrap_or(false)
}

/// Drop any root-scope `model_provider = ...` line so the managed block's
/// `model_provider = "headroom_local_community"` isn't a duplicate root key (which is invalid
/// TOML and makes Codex refuse to load its config). A `model_provider` inside a
/// table is left untouched.
fn strip_codex_root_toml_key(content: &str, key: &str) -> String {
    let mut in_root = true;
    content
        .lines()
        .filter(|raw| {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') && line.ends_with(']') {
                in_root = false;
                return true;
            }
            !(in_root
                && line
                    .split_once('=')
                    .map(|(candidate, _)| {
                        let candidate = candidate.trim();
                        candidate == key
                            || candidate
                                .strip_prefix('"')
                                .and_then(|candidate| candidate.strip_suffix('"'))
                                == Some(key)
                            || candidate
                                .strip_prefix('\'')
                                .and_then(|candidate| candidate.strip_suffix('\''))
                                == Some(key)
                    })
                    .unwrap_or(false))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_codex_root_model_provider(content: &str) -> String {
    strip_codex_root_toml_key(content, "model_provider")
}

fn strip_codex_root_openai_base_url(content: &str) -> String {
    strip_codex_root_toml_key(content, "openai_base_url")
}

/// Drop an unmarked `[model_providers.headroom_local_community]` table so the managed block's
/// copy isn't a duplicate table key. This is the table-scope analog of
/// [`strip_codex_root_model_provider`]: a second `[model_providers.headroom_local_community]`
/// makes Codex refuse to load its *entire* config, so one stale table breaks
/// every `codex` invocation, not just our routing (Sentry RUST-6K).
///
/// Such a table is left behind by an OSS `pip install headroom` (which wrote the
/// provider with no marker comments) or by a user who hand-added it before
/// marker blocks existed. It is only ever removed for the `headroom` provider
/// name -- our own namespace, and byte-identical in intent to the block we are
/// about to write. Every other provider table is left untouched.
///
/// Marker-wrapped copies are already gone by the time this runs
/// ([`strip_codex_managed_toml`]), so this only sees unmarked leftovers.
fn strip_codex_headroom_provider_table(content: &str) -> String {
    let mut dropping = false;
    content
        .lines()
        .filter(|raw| {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') && line.ends_with(']') {
                // A new table header always ends any drop; it starts one only
                // for our own provider name.
                dropping = line == "[model_providers.headroom_local_community]";
                return !dropping;
            }
            !dropping
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Restore a preserved pre-Headroom root `model_provider` after teardown, so a
/// gateway/alternate-provider user isn't silently left on api.openai.com. No-op
/// if the config already has a root `model_provider` (user re-added their own).
fn restore_codex_model_provider(provider: &str) -> Result<()> {
    let path = codex_config_toml_path();
    let existing = if path.exists() {
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    if codex_foreign_model_provider(&existing).is_some() {
        return Ok(());
    }
    let line = format!("model_provider = {}", toml_basic_string(provider));
    let trimmed = existing.trim();
    let rebuilt = if trimmed.is_empty() {
        format!("{line}\n")
    } else {
        format!("{line}\n{trimmed}\n")
    };
    let _ = backup_if_exists(&path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    atomic_write(&path, rebuilt.as_bytes())?;
    Ok(())
}

/// Reconstruct `config.toml` with the managed root keys pinned to the top and
/// the provider table appended at the end, around the user's other content.
fn render_codex_config(existing: &str) -> Result<String> {
    // Validate before discarding any root URL. This makes the pure renderer
    // enforce the same credential boundary as its on-disk caller.
    validate_codex_openai_base_url_for_takeover(existing)?;
    let mid = strip_codex_managed_toml(existing);
    // Official Headroom writes the active Codex root route in this block. Drop
    // only that block during an explicit Community takeover; keep its provider
    // table so disabling Community can restore `model_provider = "headroom"`.
    let mid = strip_namespaced_marker_block(&mid, "headroom", CODEX_ROOT_BLOCK_ID);
    // Drop a foreign root model_provider too, else our managed
    // `model_provider = "headroom_local_community"` collides with it as a duplicate root key.
    let mid = strip_codex_root_model_provider(&mid);
    // The managed root block supplies this key. Known native/Headroom values
    // were validated above, so remove them here instead of producing duplicate
    // root TOML. Unknown custom URLs never reach this point.
    let mid = strip_codex_root_openai_base_url(&mid);
    // Same collision one scope down: an unmarked `[model_providers.headroom_local_community]`
    // table would duplicate the one in the managed block below.
    let mid = strip_codex_headroom_provider_table(&mid);
    let mid = mid.trim();

    let chatgpt_backend = codex_uses_chatgpt_auth();
    let requires_openai_auth = codex_requires_openai_auth();
    let mut out = codex_marker_block(CODEX_ROOT_BLOCK_ID, &codex_root_keys_body(chatgpt_backend));
    if !mid.is_empty() {
        out.push('\n');
        out.push_str(mid);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&codex_marker_block(
        CODEX_TABLE_BLOCK_ID,
        &codex_provider_table_body(chatgpt_backend, requires_openai_auth),
    ));
    Ok(out)
}

/// Returns `(changed_files, backup_files, preserved_provider)`. The third
/// element is a pre-existing *foreign* root `model_provider` this write replaced
/// -- callers must preserve it and restore it on disable instead of dropping the
/// user onto api.openai.com (mirrors [`configure_claude_settings_env`]).
fn configure_codex_provider_block(
    allow_takeover: bool,
) -> Result<(Vec<String>, Vec<String>, Option<String>)> {
    let path = codex_config_toml_path();
    let existing = if path.exists() {
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };

    let preserved = codex_foreign_model_provider(&existing);
    if let Some(provider) = preserved.as_deref() {
        if !allow_takeover && !codex_takeover_provider_allowed(provider) {
            // Refuse before creating a rollback copy or touching any other
            // Codex integration. A custom gateway must remain exactly as the
            // user configured it; silently replacing it with api.openai.com is
            // both a routing change and a credential-boundary violation.
            // `allow_takeover` is only set after the user explicitly confirms
            // the takeover in Headroom; the displaced provider is recorded and
            // restored on disable.
            bail!("refusing to replace custom Codex model_provider {provider:?}; use one of the supported providers: openai, headroom, headroom_local_community");
        }
    }
    let updated = render_codex_config(&existing)?;
    if updated == existing {
        return Ok((Vec::new(), Vec::new(), None));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let backup = backup_if_exists(&path)?;
    atomic_write(&path, updated.as_bytes())?;

    let mut backup_files = Vec::new();
    if let Some(backup_path) = backup {
        backup_files.push(backup_path.display().to_string());
    }
    Ok((vec![path.display().to_string()], backup_files, preserved))
}

/// Rewrite the `command` of the `[mcp_servers.headroom_local_community]` table in
/// `~/.codex/config.toml` to the absolute `entrypoint`. The upstream Python
/// registrar writes a bare `command = "headroom"` that relies on PATH; when
/// the managed runtime relocates, `~/.local/bin/headroom` dangles and Codex
/// fails to start the MCP server with `No such file or directory`. Desktop
/// re-runs `mcp install` on every launch, so pinning the absolute path here
/// self-heals the config. No-op when the config or table is absent. Targets
/// the table by header rather than the Headroom marker block, which the
/// upstream registrar can mis-place around unrelated user tables.
pub fn pin_codex_mcp_command(entrypoint: &Path) -> Result<Option<String>> {
    let path = codex_config_toml_path();
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    let target_line = format!(
        "command = {}",
        toml_basic_string(&entrypoint.to_string_lossy())
    );
    // The upstream registrar may resolve the server as `<python> -m headroom.cli
    // mcp serve`. Pinning only `command` to the console script would leave
    // `args = ["-m", "headroom.cli", ...]` behind, and `headroom -m ...` fails
    // with "No such option '-m'" — so the args must be pinned together.
    let target_args_line = r#"args = ["mcp", "serve"]"#;

    let mut in_headroom_table = false;
    let mut replaced = false;
    // When the replaced `args` value is a multi-line array, the continuation
    // lines ("-m", / "headroom.cli", / ]) must be dropped too, or the rebuilt
    // file is invalid TOML and Codex fails to load its config entirely.
    let mut skip_array_depth: i32 = 0;
    let mut out: Vec<String> = Vec::with_capacity(content.lines().count());
    for line in content.lines() {
        if skip_array_depth > 0 {
            skip_array_depth += bracket_delta(line);
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_headroom_table = trimmed == "[mcp_servers.headroom_local_community]";
            out.push(line.to_string());
            continue;
        }
        if in_headroom_table {
            match trimmed
                .split_once('=')
                .map(|(key, value)| (key.trim(), value))
            {
                Some(("command", _)) => {
                    out.push(target_line.clone());
                    replaced = true;
                    continue;
                }
                Some(("args", value)) => {
                    out.push(target_args_line.to_string());
                    skip_array_depth = bracket_delta(value).max(0);
                    continue;
                }
                _ => {}
            }
        }
        out.push(line.to_string());
    }

    if !replaced {
        return Ok(None);
    }
    let mut rebuilt = out.join("\n");
    if content.ends_with('\n') {
        rebuilt.push('\n');
    }
    if rebuilt == content {
        return Ok(None);
    }
    // Never publish a config Codex can't parse — bail and leave the user's
    // file untouched instead.
    toml::from_str::<toml::Value>(&rebuilt).with_context(|| {
        format!(
            "rebuilt {} is not valid TOML; refusing to overwrite",
            path.display()
        )
    })?;
    let _ = backup_if_exists(&path)?;
    atomic_write(&path, rebuilt.as_bytes())?;
    Ok(Some(path.display().to_string()))
}

/// Net `[` minus `]` on a line, ignoring brackets inside basic strings.
/// Good enough for tracking whether a TOML array value has closed.
fn bracket_delta(line: &str) -> i32 {
    let mut delta = 0;
    let mut in_string = false;
    let mut escaped = false;
    for c in line.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '#' if !in_string => break,
            '[' if !in_string => delta += 1,
            ']' if !in_string => delta -= 1,
            _ => {}
        }
    }
    delta
}

const GROK_PROXY_BLOCK_ID: &str = "grok_build_proxy";

fn grok_config_toml_path() -> PathBuf {
    grok_home().join("config.toml")
}

fn grok_proxy_body() -> String {
    format!(
        "[model.grok-build]\nbase_url = \"{base}\"",
        base = HEADROOM_GROK_PROXY_BASE_URL
    )
}

fn strip_grok_managed_toml(content: &str) -> String {
    strip_marker_block(content, GROK_PROXY_BLOCK_ID)
}

/// Locate a `[model.grok-build]` table in `lines`: returns the header line
/// index and, when present, the index of its `base_url` line.
fn find_grok_build_table(lines: &[&str]) -> Option<(usize, Option<usize>)> {
    let mut header_idx = None;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if trimmed == "[model.grok-build]" {
                header_idx = Some(idx);
            } else if header_idx.is_some() {
                return Some((header_idx.unwrap(), None));
            }
            continue;
        }
        if header_idx.is_some()
            && trimmed
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "base_url")
        {
            return Some((header_idx.unwrap(), Some(idx)));
        }
    }
    header_idx.map(|h| (h, None))
}

/// Extract the quoted string value of a `key = "value"` TOML line, ignoring
/// any trailing comment.
fn toml_line_value(line: &str) -> Option<String> {
    let (_, rest) = line.split_once('=')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Rewrite `base_url` inside a user-owned `[model.grok-build]` table (e.g.
/// written by `headroom wrap grok`), keeping the previous value in a trailing
/// `# was:` comment so disable can restore it. Mirrors the upstream Python
/// registrar (headroom/providers/grok_build/config.py). Returns `None` when no
/// such table exists.
fn redirect_existing_grok_build_base_url(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let (header_idx, base_url_idx) = find_grok_build_table(&lines)?;
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match base_url_idx {
        Some(idx) => {
            let old = toml_line_value(lines[idx]);
            if old.as_deref() == Some(HEADROOM_GROK_PROXY_BASE_URL) {
                return Some(format!("{content}\n"));
            }
            let indent: String = lines[idx]
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            out[idx] = match old {
                Some(old) => {
                    format!("{indent}base_url = \"{HEADROOM_GROK_PROXY_BASE_URL}\"  # was: {old}")
                }
                None => format!("{indent}base_url = \"{HEADROOM_GROK_PROXY_BASE_URL}\""),
            };
        }
        None => out.insert(
            header_idx + 1,
            format!("base_url = \"{HEADROOM_GROK_PROXY_BASE_URL}\""),
        ),
    }
    let mut rebuilt = out.join("\n");
    rebuilt.push('\n');
    Some(rebuilt)
}

/// Undo a `base_url` redirect left by [`redirect_existing_grok_build_base_url`]:
/// restore the value recorded in the `# was:` comment, or drop the line when
/// Headroom inserted it into a table that had none.
fn restore_grok_build_base_url(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let Some((_, Some(idx))) = find_grok_build_table(&lines) else {
        return content.to_string();
    };
    let line = lines[idx];
    if toml_line_value(line).as_deref() != Some(HEADROOM_GROK_PROXY_BASE_URL) {
        return content.to_string();
    }
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    if let Some((_, was)) = line.split_once("# was: ") {
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        out[idx] = format!("{indent}base_url = \"{}\"", was.trim());
    } else {
        out.remove(idx);
    }
    out.join("\n")
}

fn render_grok_config(existing: &str) -> String {
    let mid = strip_grok_managed_toml(existing);
    let mid = mid.trim();

    // A user-owned [model.grok-build] table must not be duplicated - a second
    // table is invalid TOML. Redirect its base_url in place instead.
    if let Some(redirected) = redirect_existing_grok_build_base_url(mid) {
        return redirected;
    }

    // The managed block opens a [model.grok-build] table, so it must sit after
    // the user's content: any top-level key following the block would be
    // absorbed into the table.
    let block = codex_marker_block(GROK_PROXY_BLOCK_ID, &grok_proxy_body());
    if mid.is_empty() {
        return block;
    }
    format!("{mid}\n\n{block}")
}

fn configure_grok_proxy_block() -> Result<(Vec<String>, Vec<String>)> {
    let path = grok_config_toml_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let existing = if path.exists() {
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };

    let updated = render_grok_config(&existing);
    if updated == existing {
        return Ok((Vec::new(), Vec::new()));
    }

    let backup = backup_if_exists(&path)?;
    atomic_write(&path, updated.as_bytes())?;

    let mut backup_files = Vec::new();
    if let Some(backup_path) = backup {
        backup_files.push(backup_path.display().to_string());
    }
    Ok((vec![path.display().to_string()], backup_files))
}

fn grok_proxy_block_matches() -> Result<bool> {
    let path = grok_config_toml_path();
    if !path.exists() {
        return Ok(false);
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let base_url = format!("base_url = \"{}\"", HEADROOM_GROK_PROXY_BASE_URL);
    if marker_block_contains(&content, GROK_PROXY_BLOCK_ID, &base_url) {
        return Ok(true);
    }
    // Redirected user-owned table (no managed block).
    let lines: Vec<&str> = content.lines().collect();
    Ok(matches!(
        find_grok_build_table(&lines),
        Some((_, Some(idx)))
            if toml_line_value(lines[idx]).as_deref() == Some(HEADROOM_GROK_PROXY_BASE_URL)
    ))
}

fn remove_grok_proxy_block() -> Result<()> {
    let path = grok_config_toml_path();
    if !path.exists() {
        return Ok(());
    }
    let existing =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let stripped = restore_grok_build_base_url(&strip_grok_managed_toml(&existing));
    let normalized = {
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("{trimmed}\n")
        }
    };
    if normalized == existing {
        return Ok(());
    }
    let _ = backup_if_exists(&path)?;
    atomic_write(&path, normalized.as_bytes())?;
    Ok(())
}

fn disable_grok_build() -> Result<()> {
    remove_grok_proxy_block()?;
    let shell_targets = all_shell_paths();
    let _ = remove_shell_block(&shell_targets, "grok_build");
    Ok(())
}

/// Both OpenCode @ai-sdk transports append their endpoint to a `/v1` base
/// (`/messages`, `/responses`), so anthropic and openai share one proxy URL.
/// Verified against opencode 1.18.5.
const HEADROOM_OPENCODE_BASE_URL: &str = "http://127.0.0.1:6867/v1";
const OPENCODE_MANAGED_PROVIDERS: [&str; 2] = ["anthropic", "openai"];

fn opencode_config_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("opencode");
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("opencode")
}

/// OpenCode's global config file. Honors `$OPENCODE_CONFIG`; otherwise
/// prefers `opencode.jsonc` when it exists (OpenCode does the same).
fn opencode_config_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("OPENCODE_CONFIG").filter(|v| !v.is_empty()) {
        return PathBuf::from(explicit);
    }
    let dir = opencode_config_dir();
    let jsonc = dir.join("opencode.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        dir.join("opencode.json")
    }
}

fn opencode_data_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local").join("share"))
            .join("opencode");
    }
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local").join("share"))
        .join("opencode")
}

fn read_opencode_config(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => {
            let value: serde_json::Value =
                serde_json::from_str(&strip_jsonc(&raw)).with_context(|| {
                    format!(
                        "parsing {} failed (JSON/JSONC); refusing to overwrite potentially valid user config",
                        path.display()
                    )
                })?;
            // Same contract as parse_json_object's JSON5 fallback: writers
            // re-serialize with serde_json (comment-free), the byte-for-byte
            // .headroom-local-community-backup keeps the original. Local info only - expected,
            // benign behavior (RUST-61 was setup refusing valid .jsonc files).
            log::info!(
                "{} contains JSONC syntax (comments/trailing commas); a Headroom rewrite will normalize it to strict JSON - the original is kept as a .headroom-local-community-backup file",
                path.display()
            );
            value
        }
    };
    if !value.is_object() {
        return Err(anyhow!("{} is not a JSON object", path.display()));
    }
    Ok(value)
}

/// Strip `//` and `/* */` comments plus trailing commas so a JSONC config can
/// be parsed with serde_json. String contents (including escapes) survive.
fn strip_jsonc(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            '/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            ',' => {
                // Trailing comma: skip when the next non-whitespace,
                // non-comment character closes the container.
                let mut j = i + 1;
                loop {
                    while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'/') && bytes.get(j + 1) == Some(&b'/') {
                        while j < bytes.len() && bytes[j] != b'\n' {
                            j += 1;
                        }
                        continue;
                    }
                    if bytes.get(j) == Some(&b'/') && bytes.get(j + 1) == Some(&b'*') {
                        j += 2;
                        while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                            j += 1;
                        }
                        j = (j + 2).min(bytes.len());
                        continue;
                    }
                    break;
                }
                if !matches!(bytes.get(j), Some(b'}') | Some(b']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn opencode_provider_base_url(config: &serde_json::Value, provider: &str) -> Option<String> {
    config
        .get("provider")?
        .get(provider)?
        .get("options")?
        .get("baseURL")?
        .as_str()
        .map(str::to_string)
}

fn ensure_json_object<'a>(
    value: &'a mut serde_json::Value,
    key: &str,
) -> &'a mut serde_json::Value {
    let obj = value
        .as_object_mut()
        .expect("read_opencode_config guarantees an object root");
    let entry = obj
        .entry(key.to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    entry
}

fn set_opencode_provider_base_url(config: &mut serde_json::Value, provider: &str, url: &str) {
    let options = ensure_json_object(
        ensure_json_object(ensure_json_object(config, "provider"), provider),
        "options",
    );
    options
        .as_object_mut()
        .expect("ensure_json_object returns an object")
        .insert("baseURL".into(), serde_json::json!(url));
}

/// Remove the managed `baseURL`, pruning `options`/provider/`provider` map
/// entries that end up empty so disable leaves no husks behind.
fn remove_opencode_provider_base_url(config: &mut serde_json::Value, provider: &str) {
    let Some(providers) = config.get_mut("provider").and_then(|v| v.as_object_mut()) else {
        return;
    };
    if let Some(entry) = providers.get_mut(provider) {
        if let Some(options) = entry.get_mut("options").and_then(|v| v.as_object_mut()) {
            options.remove("baseURL");
            if options.is_empty() {
                entry.as_object_mut().map(|o| o.remove("options"));
            }
        }
        if entry.as_object().is_some_and(|o| o.is_empty()) {
            providers.remove(provider);
        }
    }
    if providers.is_empty() {
        config.as_object_mut().map(|o| o.remove("provider"));
    }
}

fn write_opencode_config(path: &Path, config: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut payload = serde_json::to_string_pretty(config)
        .with_context(|| format!("serializing {}", path.display()))?;
    payload.push('\n');
    atomic_write(path, payload.as_bytes())
}

/// Self-contained OpenCode transport plugin (all-provider routing via
/// `x-headroom-base-url`), vendored from headroom-ai's `plugins/opencode`
/// built with the desktop wrapper entry (proxy default 127.0.0.1:6867).
/// Regenerate: `npx tsup --config tsup.desktop.config.ts` in the plugin dir,
/// copy `dist-desktop/entry.opencode.js` here. Replace with the wheel-shipped
/// bundle once upstream PR headroomlabs-ai/headroom#2601 lands in a release.
const OPENCODE_PLUGIN_BYTES: &[u8] = include_bytes!("../resources/opencode/entry.opencode.js");

fn opencode_plugin_install_path() -> PathBuf {
    crate::storage::app_data_dir()
        .join("opencode")
        .join("entry.opencode.js")
}

/// Write (or refresh after an app update) the vendored plugin bundle.
fn ensure_opencode_plugin_file() -> Result<PathBuf> {
    let path = opencode_plugin_install_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if std::fs::read(&path).ok().as_deref() != Some(OPENCODE_PLUGIN_BYTES) {
        atomic_write(&path, OPENCODE_PLUGIN_BYTES)?;
    }
    Ok(path)
}

fn opencode_plugin_array_contains(config: &serde_json::Value, entry: &str) -> bool {
    config
        .get("plugin")
        .and_then(|p| p.as_array())
        .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(entry)))
}

fn add_opencode_plugin_entry(config: &mut serde_json::Value, entry: &str) {
    let obj = config
        .as_object_mut()
        .expect("read_opencode_config guarantees an object root");
    let list = obj
        .entry("plugin".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !list.is_array() {
        *list = serde_json::json!([]);
    }
    list.as_array_mut()
        .expect("ensured array above")
        .push(serde_json::json!(entry));
}

fn remove_opencode_plugin_entry(config: &mut serde_json::Value, entry: &str) {
    let Some(list) = config.get_mut("plugin").and_then(|p| p.as_array_mut()) else {
        return;
    };
    list.retain(|v| v.as_str() != Some(entry));
    if list.is_empty() {
        config.as_object_mut().map(|o| o.remove("plugin"));
    }
}

fn configure_opencode_provider_block(
    state: &mut ClientSetupState,
) -> Result<(Vec<String>, Vec<String>)> {
    let path = opencode_config_path();
    let mut config = read_opencode_config(&path)?;

    let mut changed = false;

    // `headroom wrap opencode` (bundled CLI) injects its own provider block and
    // repoints the native providers at a wrap-managed proxy, restoring both when
    // it exits. A SIGKILL, a crash, or a reboot leaves that state behind, and the
    // user has no `headroom` on PATH to unwrap it with - so do the unwrap here.
    // The block names the port it hijacked, which is how a wrap-managed base URL
    // is told apart from one the user actually chose (and so must not be
    // preserved as the "original" for restore-on-disable).
    if config.pointer("/provider/headroom").is_some() {
        let wrap_url = config
            .pointer("/provider/headroom/options/baseURL")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(providers) = config.get_mut("provider").and_then(|v| v.as_object_mut()) {
            providers.remove("headroom");
        }
        for provider in OPENCODE_MANAGED_PROVIDERS {
            if wrap_url.is_some() && opencode_provider_base_url(&config, provider) == wrap_url {
                remove_opencode_provider_base_url(&mut config, provider);
            }
        }
        changed = true;
    }
    for provider in OPENCODE_MANAGED_PROVIDERS {
        let existing = opencode_provider_base_url(&config, provider);
        if existing.as_deref() == Some(HEADROOM_OPENCODE_BASE_URL) {
            continue;
        }
        if let Some(original) = existing {
            // Pre-existing custom base URL (gateway, LiteLLM, ...): preserve
            // for restore-on-disable, same contract as codex/claude.
            state
                .preserved_base_urls
                .insert(format!("opencode_{provider}"), original);
        }
        set_opencode_provider_base_url(&mut config, provider, HEADROOM_OPENCODE_BASE_URL);
        changed = true;
    }

    // Transport plugin: routes every other provider (Google, custom
    // gateways, ...) through the proxy via x-headroom-base-url. The bundle
    // defaults to 6867, so no env vars are needed.
    let plugin_path = ensure_opencode_plugin_file()?;
    let plugin_entry = plugin_path.display().to_string();
    if !opencode_plugin_array_contains(&config, &plugin_entry) {
        add_opencode_plugin_entry(&mut config, &plugin_entry);
        changed = true;
    }

    if !changed {
        return Ok((Vec::new(), Vec::new()));
    }

    let backup = backup_if_exists(&path)?;
    write_opencode_config(&path, &config)?;

    let mut backup_files = Vec::new();
    if let Some(backup_path) = backup {
        backup_files.push(backup_path.display().to_string());
    }
    Ok((vec![path.display().to_string()], backup_files))
}

fn opencode_provider_block_matches() -> Result<bool> {
    let path = opencode_config_path();
    if !path.exists() {
        return Ok(false);
    }
    let config = read_opencode_config(&path)?;
    let base_urls_ok = OPENCODE_MANAGED_PROVIDERS.iter().all(|provider| {
        opencode_provider_base_url(&config, provider).as_deref() == Some(HEADROOM_OPENCODE_BASE_URL)
    });
    let plugin_path = opencode_plugin_install_path();
    let plugin_ok = plugin_path.is_file()
        && opencode_plugin_array_contains(&config, &plugin_path.display().to_string());
    Ok(base_urls_ok && plugin_ok)
}

fn disable_opencode(state: &ClientSetupState) -> Result<()> {
    let path = opencode_config_path();
    if !path.exists() {
        return Ok(());
    }
    let mut config = read_opencode_config(&path)?;
    let mut changed = false;
    for provider in OPENCODE_MANAGED_PROVIDERS {
        if opencode_provider_base_url(&config, provider).as_deref()
            != Some(HEADROOM_OPENCODE_BASE_URL)
        {
            // Not ours (user changed it since) - leave it alone.
            continue;
        }
        match state
            .preserved_base_urls
            .get(&format!("opencode_{provider}"))
        {
            Some(original) => set_opencode_provider_base_url(&mut config, provider, original),
            None => remove_opencode_provider_base_url(&mut config, provider),
        }
        changed = true;
    }
    let plugin_entry = opencode_plugin_install_path().display().to_string();
    if opencode_plugin_array_contains(&config, &plugin_entry) {
        remove_opencode_plugin_entry(&mut config, &plugin_entry);
        changed = true;
    }
    if changed {
        let _ = backup_if_exists(&path)?;
        write_opencode_config(&path, &config)?;
    }
    let _ = std::fs::remove_file(opencode_plugin_install_path());
    Ok(())
}

// ===== ZCode connector (plugin line) =====
//
// The ZCode integration is deliberately MCP-only: register Headroom's local
// MCP server in ZCode's officially documented user config
// (`~/.zcode/cli/config.json`, `mcp.servers`, hand-editable per ZCode's own
// documentation) so ZCode shares the plugin toolset Claude Code and Codex
// already have. No provider, baseURL, or account state is touched -- model
// routing stays entirely under ZCode's own provider settings.

fn zcode_home() -> PathBuf {
    home_dir().join(".zcode")
}

fn zcode_cli_config_path() -> PathBuf {
    zcode_home().join("cli").join("config.json")
}

/// The exact entry Headroom writes for ZCode. Mirrors what the community
/// install helper registers for Claude/Codex/Grok: `headroom mcp serve` from
/// the managed venv plus the five HEADROOM_* env vars.
fn zcode_mcp_entry(entrypoint: &Path) -> serde_json::Value {
    serde_json::json!({
        "command": entrypoint.to_string_lossy(),
        "args": ["mcp", "serve"],
        "env": {
            "HEADROOM_PROXY_URL": HEADROOM_PROXY_URL,
            "HEADROOM_WORKSPACE_DIR": crate::edition::workspace_dir().to_string_lossy(),
            "HEADROOM_CONFIG_DIR": crate::edition::config_dir().to_string_lossy(),
            "HEADROOM_TELEMETRY": "off",
            "HEADROOM_BEACON": "0",
        }
    })
}

fn zcode_servers_pointer() -> String {
    format!("/mcp/servers/{}", crate::edition::MCP_SERVER_NAME)
}

fn read_zcode_config(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    let value: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "parsing {} failed; refusing to overwrite potentially valid user config",
            path.display()
        )
    })?;
    if !value.is_object() {
        return Err(anyhow!("{} is not a JSON object", path.display()));
    }
    Ok(value)
}

fn write_zcode_config(path: &Path, config: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut body = serde_json::to_string_pretty(config)
        .with_context(|| format!("serializing {}", path.display()))?;
    body.push('\n');
    atomic_write(path, body.as_bytes())
}

/// The canonical community `headroom` CLI entrypoint, derived from the same
/// [`crate::tool_manager::ManagedRuntime::bootstrap_root`] layout the
/// installer uses, so connector setup never needs ToolManager state.
fn community_headroom_entrypoint() -> PathBuf {
    let binary = if cfg!(target_os = "windows") {
        "headroom.exe"
    } else {
        "headroom"
    };
    let bin_dir = if cfg!(target_os = "windows") {
        "Scripts"
    } else {
        "bin"
    };
    crate::tool_manager::ManagedRuntime::bootstrap_root(&app_data_dir())
        .venv_dir
        .join(bin_dir)
        .join(binary)
}

/// Insert Headroom's MCP entry without overwriting a customized entry.
/// An identical entry is a no-op so re-running setup stays idempotent.
fn configure_zcode_mcp_entry() -> Result<(Vec<String>, Vec<String>)> {
    let entrypoint = community_headroom_entrypoint();
    if !entrypoint.exists() {
        bail!(
            "Headroom runtime entrypoint {} was not found; let the Headroom runtime finish installing before connecting ZCode.",
            entrypoint.display()
        );
    }
    let path = zcode_cli_config_path();
    let mut config = read_zcode_config(&path)?;
    let entry = zcode_mcp_entry(&entrypoint);
    if config.pointer(&zcode_servers_pointer()) == Some(&entry) {
        return Ok((Vec::new(), Vec::new()));
    }
    if config.pointer(&zcode_servers_pointer()).is_some() {
        bail!(
            "{} contains a customized Headroom MCP entry; refusing to overwrite it.",
            path.display()
        );
    }
    // ZCode skips the entire user-level .agents fallback once a native
    // server exists. Do not silently disable the user's existing tools.
    if config.pointer("/mcp/servers").and_then(Value::as_object)
        .is_none_or(|servers| servers.is_empty())
    {
        let fallback_path = home_dir().join(".agents/mcp.json");
        let fallback = read_zcode_config(&fallback_path)?;
        if fallback.get("mcpServers").is_some_and(|servers| {
            servers.as_object().is_none_or(|servers| !servers.is_empty())
        }) {
            bail!(
                "ZCode currently uses {}. Import those MCP servers into ZCode Settings > MCP Servers before connecting Headroom, so they remain available.",
                fallback_path.display()
            );
        }
    }

    {
        let mcp = config
            .as_object_mut()
            .expect("read_zcode_config checked is_object")
            .entry("mcp")
            .or_insert_with(|| serde_json::json!({}));
        if !mcp.is_object() {
            bail!(
                "{}: \"mcp\" is not a JSON object; refusing to edit",
                path.display()
            );
        }
        let servers = mcp
            .as_object_mut()
            .expect("checked is_object")
            .entry("servers")
            .or_insert_with(|| serde_json::json!({}));
        if !servers.is_object() {
            bail!(
                "{}: \"mcp.servers\" is not a JSON object; refusing to edit",
                path.display()
            );
        }
        servers
            .as_object_mut()
            .expect("checked is_object")
            .insert(crate::edition::MCP_SERVER_NAME.to_string(), entry);
    }

    let backup = backup_if_exists(&path)?;
    write_zcode_config(&path, &config)?;
    let mut backup_files = Vec::new();
    if let Some(backup_path) = backup {
        backup_files.push(backup_path.display().to_string());
    }
    Ok((vec![path.display().to_string()], backup_files))
}

pub(crate) fn zcode_mcp_entry_matches() -> Result<bool> {
    let path = zcode_cli_config_path();
    if !path.exists() {
        return Ok(false);
    }
    let config = read_zcode_config(&path)?;
    Ok(
        config.pointer(&zcode_servers_pointer())
            == Some(&zcode_mcp_entry(&community_headroom_entrypoint())),
    )
}

fn disable_zcode() -> Result<()> {
    let path = zcode_cli_config_path();
    if !path.exists() {
        return Ok(());
    }
    let mut config = read_zcode_config(&path)?;
    // Only remove an entry structurally identical to what we write; a
    // customized entry is the user's, same contract as the OpenCode base URLs.
    if config.pointer(&zcode_servers_pointer())
        != Some(&zcode_mcp_entry(&community_headroom_entrypoint()))
    {
        return Ok(());
    }
    if let Some(servers) = config
        .pointer_mut("/mcp/servers")
        .and_then(|value| value.as_object_mut())
    {
        servers.remove(crate::edition::MCP_SERVER_NAME);
    }
    // When the file now holds nothing but the empty scaffolding our first
    // write created, remove it outright so an uninstall leaves no husk.
    let only_our_scaffolding = config
        .as_object()
        .is_some_and(|object| {
            object.len() == 1
                && object
                    .get("mcp")
                    .and_then(|value| value.as_object())
                    .is_some_and(|mcp| {
                        mcp.len() == 1
                            && mcp.get("servers")
                                .and_then(|value| value.as_object())
                                .is_some_and(|servers| servers.is_empty())
                    })
        });
    if only_our_scaffolding {
        std::fs::remove_file(&path)
            .with_context(|| format!("removing {}", path.display()))?;
        return Ok(());
    }
    let _ = backup_if_exists(&path)?;
    write_zcode_config(&path, &config)
}

/// Rewrite only the `command` of an existing Headroom entry in ZCode's config.
/// Mirrors [`pin_codex_mcp_command`] / [`pin_grok_mcp_command`]: it never
/// creates the entry (a disabled connector must not be resurrected by a
/// runtime reinstall) and never touches the user's other fields.
pub fn repin_zcode_mcp_command(entrypoint: &Path) -> Result<Option<String>> {
    let path = zcode_cli_config_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let mut config: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "parsing {} failed; refusing to edit potentially valid user config",
            path.display()
        )
    })?;
    // Use the same ownership check as disable: user edits are not ours to repin.
    if config.pointer(&zcode_servers_pointer())
        != Some(&zcode_mcp_entry(&community_headroom_entrypoint()))
    {
        return Ok(None);
    }
    let Some(entry) = config
        .pointer_mut(&zcode_servers_pointer())
        .and_then(|value| value.as_object_mut())
    else {
        return Ok(None);
    };
    if entry.get("command").and_then(|value| value.as_str())
        == Some(&entrypoint.to_string_lossy())
    {
        return Ok(None);
    }
    entry.insert(
        "command".to_string(),
        serde_json::Value::String(entrypoint.to_string_lossy().into_owned()),
    );
    let _ = backup_if_exists(&path)?;
    write_zcode_config(&path, &config)?;
    Ok(Some(path.display().to_string()))
}

/// Whether ZCode itself is present on this machine. Deliberately excludes
/// `~/.zcode/cli/config.json` -- setup writes that file, so counting it would
/// make detection self-fulfilling after an uninstall (the grok_build bug).
fn zcode_installed_on_machine() -> bool {
    let zcode = zcode_home();
    if zcode.join("v2").exists() {
        return true;
    }
    if let Ok(entries) = std::fs::read_dir(zcode.join("cli")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name != "config.json"
                && !name.starts_with("config.json.headroom-local-community-backup-")
            {
                return true;
            }
        }
    }
    find_on_path(&["zcode"]).is_some()
}

fn detect_zcode_client(configured: bool) -> ClientStatus {
    if zcode_installed_on_machine() {
        let detected_note = format!("Detected ZCode data in {}.", zcode_home().display());
        return ClientStatus {
            id: "zcode".into(),
            name: "ZCode".into(),
            installed: true,
            configured,
            health: if configured {
                ClientHealth::Healthy
            } else {
                ClientHealth::Attention
            },
            notes: if configured {
                vec![detected_note, "Configured by Headroom.".into()]
            } else {
                vec![
                    detected_note,
                    "Register Headroom's local MCP server in ZCode so it shares the same plugin toolset.".into(),
                ]
            },
        };
    }

    ClientStatus {
        id: "zcode".into(),
        name: "ZCode".into(),
        installed: false,
        configured: false,
        health: ClientHealth::NotDetected,
        notes: vec!["Not detected on this machine yet.".into()],
    }
}

fn detect_opencode_client(configured: bool) -> ClientStatus {
    let executable = opencode_candidate_paths()
        .into_iter()
        .find(|path| path.exists())
        .or_else(|| find_on_path(&["opencode"]));

    let detected = executable
        .as_ref()
        .map(|path| format!("Detected at {}", path.display()))
        .or_else(|| {
            opencode_user_state_exists().then(|| {
                format!(
                    "Detected OpenCode data in {}.",
                    opencode_data_dir().display()
                )
            })
        });

    if let Some(detected_note) = detected {
        return ClientStatus {
            id: "opencode".into(),
            name: "OpenCode".into(),
            installed: true,
            configured,
            health: if configured {
                ClientHealth::Healthy
            } else {
                ClientHealth::Attention
            },
            notes: if configured {
                vec![detected_note, "Configured by Headroom.".into()]
            } else {
                vec![
                    detected_note,
                    "Route OpenCode through Headroom's localhost proxy so prompts stay lean."
                        .into(),
                ]
            },
        };
    }

    ClientStatus {
        id: "opencode".into(),
        name: "OpenCode".into(),
        installed: false,
        configured: false,
        health: ClientHealth::NotDetected,
        notes: vec!["Not detected on this machine yet.".into()],
    }
}

fn opencode_candidate_paths() -> Vec<PathBuf> {
    let home = home_dir();
    let mut candidates = vec![
        home.join(".opencode").join("bin").join("opencode"),
        PathBuf::from("/opt/homebrew/bin/opencode"),
        PathBuf::from("/usr/local/bin/opencode"),
    ];
    let user_bin_dirs = vec![home.join(".local").join("bin"), home.join("bin")];
    candidates.extend(binary_candidates_in_dirs(&user_bin_dirs, &["opencode"]));
    dedupe_paths(candidates)
}

/// Deliberately excludes the config file: setup itself creates one, which
/// would make detection self-fulfilling after disable (the grok_build bug).
fn opencode_user_state_exists() -> bool {
    let data = opencode_data_dir();
    data.join("auth.json").exists() || data.join("storage").exists()
}

/// Rewrite the `command` of the `[mcp_servers.headroom_local_community]` table in
/// `~/.grok/config.toml` to the absolute `entrypoint`. Mirrors
/// [`pin_codex_mcp_command`]: the upstream Python registrar writes a bare
/// `command = "headroom"` that relies on PATH, which dangles when the managed
/// runtime relocates.
pub fn pin_grok_mcp_command(entrypoint: &Path) -> Result<Option<String>> {
    let path = grok_config_toml_path();
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    let target_line = format!(
        "command = {}",
        toml_basic_string(&entrypoint.to_string_lossy())
    );

    let mut in_headroom_table = false;
    let mut replaced = false;
    let mut out: Vec<String> = Vec::with_capacity(content.lines().count());
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_headroom_table = trimmed == "[mcp_servers.headroom_local_community]";
            out.push(line.to_string());
            continue;
        }
        if in_headroom_table
            && !replaced
            && trimmed
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "command")
        {
            out.push(target_line.clone());
            replaced = true;
            continue;
        }
        out.push(line.to_string());
    }

    if !replaced {
        return Ok(None);
    }
    let mut rebuilt = out.join("\n");
    if content.ends_with('\n') {
        rebuilt.push('\n');
    }
    if rebuilt == content {
        return Ok(None);
    }
    let _ = backup_if_exists(&path)?;
    atomic_write(&path, rebuilt.as_bytes())?;
    Ok(Some(path.display().to_string()))
}

fn toml_basic_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn codex_provider_block_matches() -> Result<bool> {
    let path = codex_config_toml_path();
    if !path.exists() {
        return Ok(false);
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let expected_base_url = headroom_codex_base_url();
    let base_url = format!("base_url = \"{expected_base_url}\"");
    let openai_base = format!("openai_base_url = \"{expected_base_url}\"");
    let root_ok = marker_block_contains(
        &content,
        CODEX_ROOT_BLOCK_ID,
        "model_provider = \"headroom_local_community\"",
    ) && marker_block_contains(&content, CODEX_ROOT_BLOCK_ID, &openai_base);
    let table_ok = marker_block_contains(&content, CODEX_TABLE_BLOCK_ID, "name = \"OpenAI\"")
        && marker_block_contains(&content, CODEX_TABLE_BLOCK_ID, &base_url)
        && marker_block_contains(
            &content,
            CODEX_TABLE_BLOCK_ID,
            "supports_websockets = false",
        )
        && marker_block_contains(
            &content,
            CODEX_TABLE_BLOCK_ID,
            "supports_standalone_web_search = true",
        );
    Ok(root_ok && table_ok)
}

fn marker_block_contains(content: &str, block_id: &str, needle: &str) -> bool {
    // Accept the current community namespace and the pre-community marker
    // emitted by older installed builds. Values are still checked by callers.
    for namespace in ["headroom-local-community", "headroom"] {
        let start = format!("# >>> {namespace}:{block_id} >>>");
        let end = format!("# <<< {namespace}:{block_id} <<<");
        let (Some(start_idx), Some(end_idx)) = (content.find(&start), content.find(&end)) else {
            continue;
        };
        if start_idx < end_idx && content[start_idx..end_idx].contains(needle) {
            return true;
        }
    }
    false
}

/// Health of one Headroom-managed MCP registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedMcpRegistrationState {
    Healthy,
    Missing,
    UserManaged,
    Mismatch,
}

/// Read-only health result for the Codex registrations owned by Headroom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexMcpRegistrationHealth {
    pub context7: ManagedMcpRegistrationState,
    pub codebase_memory: ManagedMcpRegistrationState,
}

/// Check Codex's Headroom MCP registrations against the current expectations.
///
/// `config` must be parsed by the caller (normally the existing Codex config
/// loading path). `owned_entries` must contain only names proven by
/// `headroom_installed_matching` (including any accepted legacy ledger). A
/// same-name entry absent from that set is therefore user-managed and is never
/// treated as healthy, even if its command happens to match.
fn codex_headroom_mcp_registration_health_for_config(
    config: &toml::Value,
    expected_context7_package: &str,
    expected_codebase_memory_entrypoint: &Path,
    expected_codebase_memory_cache_dir: &Path,
    owned_entries: &BTreeSet<String>,
) -> CodexMcpRegistrationHealth {
    let context7 = managed_mcp_entry_state(
        config,
        "context7",
        owned_entries.contains("context7"),
        |entry| {
            entry.get("command").and_then(toml::Value::as_str) == Some("npx")
                && entry
                    .get("args")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|args| {
                        args.len() == 2
                            && args[0].as_str() == Some("-y")
                            && args[1].as_str() == Some(expected_context7_package)
                    })
        },
    );
    let codebase_memory = managed_mcp_entry_state(
        config,
        "codebase-memory",
        owned_entries.contains("codebase-memory"),
        |entry| {
            entry.get("command").and_then(toml::Value::as_str)
                == expected_codebase_memory_entrypoint.to_str()
                && entry
                    .get("env")
                    .and_then(toml::Value::as_table)
                    .and_then(|env| env.get("CBM_CACHE_DIR"))
                    .and_then(toml::Value::as_str)
                    == expected_codebase_memory_cache_dir.to_str()
        },
    );

    CodexMcpRegistrationHealth {
        context7,
        codebase_memory,
    }
}

/// Read-only health check for Codex's Headroom-managed MCP registrations.
/// Reads and parses the existing Codex config and both the Community and
/// legacy Headroom ledgers; it never writes or changes user configuration.
pub fn codex_headroom_mcp_registration_health(
    expected_context7_package: &str,
    expected_codebase_memory_entrypoint: &Path,
    expected_codebase_memory_cache_dir: &Path,
) -> Result<CodexMcpRegistrationHealth> {
    let path = codex_config_toml_path();
    if !path.exists() {
        return Ok(CodexMcpRegistrationHealth {
            context7: ManagedMcpRegistrationState::Missing,
            codebase_memory: ManagedMcpRegistrationState::Missing,
        });
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config = content
        .parse::<toml::Value>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let owned_entries = codex_ledger_owned_entries(&config);
    Ok(codex_headroom_mcp_registration_health_for_config(
        &config,
        expected_context7_package,
        expected_codebase_memory_entrypoint,
        expected_codebase_memory_cache_dir,
        &owned_entries,
    ))
}

fn codex_ledger_owned_entries(config: &toml::Value) -> BTreeSet<String> {
    let Some(servers) = config.get("mcp_servers").and_then(toml::Value::as_table) else {
        return BTreeSet::new();
    };
    let ledger_paths = [
        crate::edition::workspace_dir().join("mcp_installs.json"),
        home_dir().join(".headroom").join("mcp_installs.json"),
    ];
    servers
        .iter()
        .filter_map(|(name, entry)| {
            let table = entry.as_table()?;
            let fingerprint = mcp_entry_fingerprint(name, table)?;
            ledger_paths
                .iter()
                .any(|path| ledger_contains_fingerprint(path, "codex", name, &fingerprint))
                .then(|| name.clone())
        })
        .collect()
}

fn ledger_contains_fingerprint(path: &Path, agent: &str, name: &str, fingerprint: &str) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    root.get("agents")
        .and_then(|v| v.get(agent))
        .and_then(|v| v.get(name))
        .and_then(|v| v.get("fingerprint"))
        .and_then(Value::as_str)
        == Some(fingerprint)
}

fn mcp_entry_fingerprint(name: &str, entry: &toml::value::Table) -> Option<String> {
    let command = entry.get("command")?.as_str()?;
    let args = entry
        .get("args")
        .and_then(toml::Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let env = entry
        .get("env")
        .and_then(toml::Value::as_table)
        .map(|env| {
            env.iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let payload = serde_json::json!({
        "args": args,
        "command": command,
        "env": env,
        "name": name,
    });
    let raw = serde_json::to_vec(&payload).ok()?;
    Some(format!("{:x}", Sha256::digest(raw)))
}

fn managed_mcp_entry_state(
    config: &toml::Value,
    name: &str,
    owned: bool,
    matches: impl FnOnce(&toml::value::Table) -> bool,
) -> ManagedMcpRegistrationState {
    let Some(entry) = config
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .and_then(|servers| servers.get(name))
        .and_then(toml::Value::as_table)
    else {
        return ManagedMcpRegistrationState::Missing;
    };
    if !owned {
        return ManagedMcpRegistrationState::UserManaged;
    }
    if matches(entry) {
        ManagedMcpRegistrationState::Healthy
    } else {
        ManagedMcpRegistrationState::Mismatch
    }
}

fn remove_codex_provider_block() -> Result<()> {
    let path = codex_config_toml_path();
    if !path.exists() {
        return Ok(());
    }
    let existing =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let stripped = strip_codex_managed_toml(&existing);
    let normalized = {
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("{trimmed}\n")
        }
    };
    if normalized == existing {
        return Ok(());
    }
    let _ = backup_if_exists(&path)?;
    atomic_write(&path, normalized.as_bytes())?;
    Ok(())
}

fn remove_codex_toml_key(key: &str, expected_value: &str) -> Result<()> {
    let path = codex_config_toml_path();
    if !path.exists() {
        return Ok(());
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let target_line = format!("{key} = \"{expected_value}\"");
    // Only remove the key from the root table: an identical `key = value`
    // line inside some other table ([profiles.x], a user's own server entry)
    // belongs to that table, not to the block we installed.
    let mut in_root_table = true;
    let filtered: Vec<&str> = content
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_root_table = false;
            }
            !(in_root_table && trimmed == target_line)
        })
        .collect();
    if filtered.len() == content.lines().count() {
        return Ok(());
    }
    let _ = backup_if_exists(&path)?;
    let mut result = filtered.join("\n");
    if !result.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    atomic_write(&path, result.as_bytes())?;
    Ok(())
}

const CODEX_GUARD_STATUS_MESSAGE: &str = "Verifying Headroom route";

fn codex_hooks_json_path() -> PathBuf {
    codex_home().join("hooks.json")
}

fn codex_guard_hook_path() -> PathBuf {
    codex_home()
        .join("hooks")
        .join("headroom-local-community-codex-guard.py")
}

/// Interpreter used by the Claude/Codex session-start guard hooks. On macOS
/// and Linux the system `/usr/bin/python3` (>=3.9) is always present. On
/// Windows there's no such guarantee -- bare `python` on a stock box is
/// either absent from PATH or the Microsoft Store stub that opens the Store
/// instead of running -- so point at the managed runtime's own bundled
/// interpreter, which this app installs regardless of what's on PATH.
fn guard_python_command() -> String {
    if cfg!(target_os = "windows") {
        let managed =
            crate::tool_manager::ManagedRuntime::bootstrap_root(&app_data_dir()).managed_python();
        format!("\"{}\"", managed.display())
    } else {
        "/usr/bin/python3".to_string()
    }
}

/// Join the guard interpreter and its script into a command string the host
/// shell can actually run. Claude Code and Codex launch hooks through
/// PowerShell on Windows, where a command that *starts* with a quoted path
/// parses as a string literal rather than a command ("At line:1 char:81" --
/// the offset lands on the unquoted script path), so the call operator is
/// required and the script path needs quoting too because profile directories
/// contain spaces. Deliberately not `shell_double_quote`: that escapes
/// backslashes POSIX-style and would mangle every Windows path. PowerShell
/// escapes with a backtick, and both `"` and backtick are invalid in Windows
/// filenames, so bare double quotes are already sufficient.
fn guard_command(script_path: &Path) -> String {
    join_guard_command(
        &guard_python_command(),
        &script_path.to_string_lossy(),
        cfg!(target_os = "windows"),
    )
}

/// Pure so the Windows branch is exercised by tests on every platform.
fn join_guard_command(python: &str, script: &str, windows: bool) -> String {
    if windows {
        format!("& {python} \"{script}\"")
    } else {
        format!("{python} {script}")
    }
}

fn codex_guard_command() -> String {
    guard_command(&codex_guard_hook_path())
}

/// Informational guard that Codex runs at session start: it checks that
/// `~/.codex/config.toml` still routes through Headroom's stable router and
/// that the detached listener is reachable, and surfaces a notification when a
/// genuinely broken route is visible. It never blocks (always exits 0): Codex
/// is the user's own OpenAI account and must keep working whether Headroom is
/// active or closed because the router switches to the native provider when its
/// heartbeat expires. Runs under system `/usr/bin/python3` (>=3.9), so it
/// carries a tiny TOML fallback parser for the pre-3.11 interpreters that lack
/// `tomllib`. The expected local base is selected from the managed provider's
/// `requires_openai_auth` flag, auth.json, and access-token environment so a
/// ChatGPT session is checked against `/backend-api/codex` while an API-key
/// session is checked against `/v1`.
fn build_codex_guard_script() -> String {
    format!(
        r##"#!/usr/bin/env python3
"""Headroom Codex routing guard (managed by Headroom Desktop -- do not edit)."""
import base64
import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request

try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None

CODEX_HOME = pathlib.Path(os.environ.get("CODEX_HOME") or (pathlib.Path.home() / ".codex"))
CONFIG = CODEX_HOME / "config.toml"
OPENAI_BASE_URL = "{openai_base}"
CHATGPT_BASE_URL = "{chatgpt_base}"
READYZ = "{readyz}"
# stderr fires every invocation; the macOS notification is rate-limited so an
# app restart doesn't produce a storm of alerts.
DEBOUNCE_PATH = pathlib.Path(__file__).with_name(".headroom-local-community-guard-notified")
DEBOUNCE_SECONDS = 600


def notify(message):
    if sys.platform == "win32":
        return
    try:
        if time.time() - DEBOUNCE_PATH.stat().st_mtime < DEBOUNCE_SECONDS:
            return
    except OSError:
        pass
    try:
        DEBOUNCE_PATH.touch()
        subprocess.run(
            [
                "/usr/bin/osascript",
                "-e",
                'display notification ' + json.dumps(message) + ' with title "Headroom Codex guard"',
            ],
            check=False,
            timeout=5,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


def toml_fallback(text):
    result, current = {{}}, []
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            current = [p.strip() for p in line.strip("[]").split(".") if p.strip()]
            continue
        if "=" not in line:
            continue
        key, value = (p.strip() for p in line.split("=", 1))
        if value[:1] == '"' and value[-1:] == '"':
            value = value[1:-1]
        target = result
        for part in current:
            target = target.setdefault(part, {{}})
        target[key] = value
    return result


def load_config():
    try:
        text = CONFIG.read_text()
    except OSError:
        return None
    if tomllib is not None:
        try:
            return tomllib.loads(text)
        except Exception:
            return toml_fallback(text)
    return toml_fallback(text)


def chatgpt_auth(config):
    provider = (config or {{}}).get("model_providers", {{}}).get("headroom_local_community") or {{}}
    requires = provider.get("requires_openai_auth")
    if requires is True or str(requires).lower() == "true":
        return True
    try:
        auth = json.loads((CODEX_HOME / "auth.json").read_text())
    except Exception:
        auth = {{}}
    mode = str(auth.get("auth_mode") or "").replace("_", "").replace("-", "").lower()
    if mode == "apikey" or mode == "codexapikey":
        return False
    if mode.startswith("chatgpt") or mode in ("headers", "agentidentity", "personalaccesstoken"):
        return True
    tokens = auth.get("tokens") or {{}}
    if str(tokens.get("account_id") or "").strip():
        return True
    id_token = tokens.get("id_token") or auth.get("id_token")
    if isinstance(id_token, str):
        try:
            payload = id_token.split(".")[1]
            payload += "=" * (-len(payload) % 4)
            claims = json.loads(base64.urlsafe_b64decode(payload).decode())
            account = (claims.get("https://api.openai.com/auth") or {{}}).get("chatgpt_account_id")
            if str(account or "").strip():
                return True
        except Exception:
            pass
    if os.environ.get("CODEX_ACCESS_TOKEN", "").strip() and not os.environ.get("OPENAI_API_KEY", "").strip():
        return True
    return False


def expected_base_url(config):
    return CHATGPT_BASE_URL if chatgpt_auth(config) else OPENAI_BASE_URL


def probe():
    # Any HTTP response means our server answered -- the app is up. A 503 during
    # bypass mode is still "up", so only connection errors / timeouts count as down.
    try:
        urllib.request.urlopen(READYZ, timeout=2)
        return True
    except urllib.error.HTTPError:
        return True
    except Exception:
        return False


def reachable():
    # One retry after a short pause so an app-relaunch blip doesn't read as "down".
    if probe():
        return True
    time.sleep(2)
    return probe()


def main():
    issues = []
    config = load_config()
    if config is None:
        issues.append("~/.codex/config.toml is missing or unreadable")
    else:
        provider_name = config.get("model_provider")
        if provider_name != "headroom_local_community":
            issues.append('Codex model_provider is "' + str(provider_name) + '" (expected "headroom_local_community"); Codex is not being optimized by Headroom')
        else:
            provider = (config.get("model_providers") or {{}}).get("headroom_local_community") or {{}}
            base = provider.get("base_url")
            expected = expected_base_url(config)
            if base != expected:
                issues.append("Headroom provider base_url is " + str(base) + " (expected " + expected + ")")
    if not reachable():
        issues.append("Headroom Codex router isn't running; open Headroom once to start routing")

    # Never block (exit 2): Codex is the user's own OpenAI account and must keep
    # working whether or not Headroom is active. Surface issues as a once-per-
    # session notification so a genuinely broken route is visible, without
    # holding a paused or departing user's Codex hostage to the app being open.
    if issues:
        notify("; ".join(issues))
        sys.stderr.write("Headroom Codex guard:\n")
        for issue in issues:
            sys.stderr.write("- " + issue + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"##,
        openai_base = HEADROOM_OPENAI_BASE_URL,
        chatgpt_base = HEADROOM_CHATGPT_BASE_URL,
        readyz = "http://127.0.0.1:6891/__headroom_codex_router_health",
    )
}

/// Merge guard entries for the given `events` into a hooks file.
/// Codex `hooks.json` and Claude `settings.json` share the identical
/// `{{"hooks": {{event: [{{matcher?, hooks: [...]}}]}}}}` shape, so both clients
/// use this. Preserves every other key in the file. Idempotent: an
/// already-registered guard command is left untouched.
fn register_guard_hook_entries(
    hooks_path: &Path,
    command: &str,
    status_message: &str,
    events: &[(&str, Option<&str>)],
) -> Result<(Vec<String>, Vec<String>)> {
    let mut content = if hooks_path.exists() {
        let raw = std::fs::read_to_string(hooks_path)
            .with_context(|| format!("reading {}", hooks_path.display()))?;
        Value::Object(parse_json_object(&raw, hooks_path)?)
    } else {
        Value::Object(Default::default())
    };

    let root = content
        .as_object_mut()
        .ok_or_else(|| anyhow!("unable to write hooks settings"))?;
    match root.get("hooks") {
        None => {
            root.insert("hooks".into(), Value::Object(Default::default()));
        }
        Some(value) if value.is_object() => {}
        Some(_) => {
            reject_invalid_json_container(hooks_path, "hooks", "an object")?;
        }
    }
    let hooks_obj = root
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("unable to write hooks settings"))?;

    let mut mutated = false;
    for &(event, matcher) in events {
        match hooks_obj.get(event) {
            None => {
                hooks_obj.insert(event.to_string(), Value::Array(Vec::new()));
            }
            Some(value) if value.is_array() => {}
            Some(_) => {
                reject_invalid_json_container(hooks_path, &format!("hooks.{event}"), "an array")?;
            }
        }
        let entries = hooks_obj
            .get_mut(event)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("unable to write hooks settings"))?;
        if entries
            .iter()
            .any(|entry| entry_contains_hook(entry, command))
        {
            continue;
        }
        let handler = serde_json::json!({
            "type": "command",
            "command": command,
            "timeout": 10,
            "statusMessage": status_message,
        });
        let mut entry = serde_json::Map::new();
        if let Some(matcher) = matcher {
            entry.insert("matcher".into(), Value::String(matcher.to_string()));
        }
        entry.insert("hooks".into(), Value::Array(vec![handler]));
        entries.push(Value::Object(entry));
        mutated = true;
    }

    if !mutated {
        return Ok((Vec::new(), Vec::new()));
    }

    let backup = backup_if_exists(hooks_path)?;
    if let Some(parent) = hooks_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    atomic_write(
        hooks_path,
        &serde_json::to_vec_pretty(&content).context("serializing hooks file")?,
    )?;

    let mut backups = Vec::new();
    if let Some(backup) = backup {
        backups.push(backup.display().to_string());
    }
    Ok((vec![hooks_path.display().to_string()], backups))
}

/// Whether `command` is registered under any event in a hooks file.
fn guard_registered_in_hooks(hooks_path: &Path, command: &str) -> Result<bool> {
    if !hooks_path.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(hooks_path)
        .with_context(|| format!("reading {}", hooks_path.display()))?;
    let content = Value::Object(parse_json_object(&raw, hooks_path)?);
    Ok(content
        .get("hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks.values().any(|entries| {
                entries
                    .as_array()
                    .map(|arr| arr.iter().any(|entry| entry_contains_hook(entry, command)))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false))
}

/// Strip the guard entries for `command` from a hooks file. Leaves any
/// user-authored hooks intact and drops now-empty event arrays. `delete_if_empty`
/// removes the whole file when nothing remains -- correct for Codex's standalone
/// `hooks.json`, but never for Claude's shared `settings.json`. `only_events`
/// limits the sweep to specific event names; `None` sweeps every event.
fn remove_guard_hook_entries(
    hooks_path: &Path,
    command: &str,
    delete_if_empty: bool,
    only_events: Option<&[&str]>,
) -> Result<()> {
    if !hooks_path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(hooks_path)
        .with_context(|| format!("reading {}", hooks_path.display()))?;
    let mut content = Value::Object(parse_json_object(&raw, hooks_path)?);
    let mut changed = false;
    let hooks_empty = {
        let Some(hooks_value) = content.get_mut("hooks") else {
            return Ok(());
        };
        let Some(hooks_obj) = hooks_value.as_object_mut() else {
            return reject_invalid_json_container(hooks_path, "hooks", "an object");
        };

        // Sweep every event, not just the ones we register, so a guard that Codex
        // (or an older build) moved to another event is still stripped.
        let events: Vec<String> = hooks_obj.keys().cloned().collect();
        for event in events {
            if let Some(filter) = only_events {
                if !filter.contains(&event.as_str()) {
                    continue;
                }
            }
            let Some(entries_value) = hooks_obj.get_mut(&event) else {
                continue;
            };
            let Some(entries) = entries_value.as_array_mut() else {
                return reject_invalid_json_container(
                    hooks_path,
                    &format!("hooks.{event}"),
                    "an array",
                );
            };
            let before = entries.len();
            entries.retain(|entry| !entry_contains_hook(entry, command));
            if entries.len() != before {
                changed = true;
            }
        }
        hooks_obj.retain(|_, value| !value.as_array().map(|arr| arr.is_empty()).unwrap_or(false));
        hooks_obj.is_empty()
    };
    if hooks_empty {
        if let Some(root) = content.as_object_mut() {
            root.remove("hooks");
        }
    }
    if !changed {
        return Ok(());
    }
    backup_if_exists(hooks_path)?;
    let root_empty = content.as_object().map(|o| o.is_empty()).unwrap_or(false);
    if delete_if_empty && root_empty {
        std::fs::remove_file(hooks_path)
            .with_context(|| format!("removing {}", hooks_path.display()))?;
    } else {
        atomic_write(
            hooks_path,
            &serde_json::to_vec_pretty(&content).context("serializing hooks file")?,
        )?;
    }
    Ok(())
}

/// Write the guard script and register it in `~/.codex/hooks.json` for the
/// SessionStart event only. `hooks.json` is auto-discovered by Codex (no
/// `config.toml` flag needed). The user must trust the hook once via Codex's
/// `/hooks` command before it runs (re-trust after any guard update).
///
/// SessionStart only (mirrors `ensure_claude_guard_hook`): on UserPromptSubmit a
/// nonzero exit blocks every prompt, which held a paused or departing user's own
/// OpenAI-billed Codex hostage to the desktop app being open -- the exact reason
/// users uninstalled. The guard is informational, not a gate; the intercept
/// forwards Codex direct to OpenAI when the app is down or the gate trips.
fn ensure_codex_guard_hook() -> Result<(Vec<String>, Vec<String>)> {
    let script_path = codex_guard_hook_path();
    let (script_changed, script_backup) =
        write_file_if_changed(&script_path, &build_codex_guard_script(), true)?;
    // Migration: earlier builds registered on UserPromptSubmit, where a nonzero
    // exit blocked every Codex prompt. Strip that entry from existing installs;
    // match on the script path so it lands regardless of interpreter drift.
    remove_guard_hook_entries(
        &codex_hooks_json_path(),
        &script_path.display().to_string(),
        false,
        Some(&["UserPromptSubmit"]),
    )?;
    // Same stale-command migration as `ensure_claude_guard_hook`; see there.
    if !codex_guard_registered().unwrap_or(false) {
        remove_guard_hook_entries(
            &codex_hooks_json_path(),
            &script_path.display().to_string(),
            false,
            Some(&["SessionStart"]),
        )?;
    }
    let (mut changed, mut backups) = register_guard_hook_entries(
        &codex_hooks_json_path(),
        &codex_guard_command(),
        CODEX_GUARD_STATUS_MESSAGE,
        &[("SessionStart", Some("startup|resume|clear|compact"))],
    )?;
    if script_changed {
        changed.insert(0, script_path.display().to_string());
    }
    if let Some(backup) = script_backup {
        backups.insert(0, backup.display().to_string());
    }
    Ok((changed, backups))
}

fn codex_guard_registered() -> Result<bool> {
    guard_registered_in_hooks(&codex_hooks_json_path(), &codex_guard_command())
}

fn remove_codex_guard_hook() -> Result<()> {
    let script_path = codex_guard_hook_path();
    // Match on the script path, not the full `/usr/bin/python3 <path>` command,
    // so the registration is stripped even if the interpreter differs -- otherwise
    // deleting the script below leaves a dangling hook that fails with ENOENT.
    remove_guard_hook_entries(
        &codex_hooks_json_path(),
        &script_path.display().to_string(),
        true,
        None,
    )?;
    if script_path.exists() {
        std::fs::remove_file(&script_path)
            .with_context(|| format!("removing {}", script_path.display()))?;
    }
    Ok(())
}

const CLAUDE_GUARD_STATUS_MESSAGE: &str = "Verifying Headroom route";

fn claude_guard_hook_path() -> PathBuf {
    home_dir()
        .join(".claude")
        .join("hooks")
        .join("headroom-local-community-claude-guard.py")
}

fn claude_guard_command() -> String {
    guard_command(&claude_guard_hook_path())
}

/// Loud-fail guard that Claude Code runs at session start (SessionStart only:
/// exit 2 there surfaces a warning but cannot block, whereas on UserPromptSubmit
/// it blocks every prompt -- which broke Claude Desktop / Cowork VM sessions
/// that share `~/.claude/settings.json` but can never reach 127.0.0.1:6867).
/// Because the hook inherits Claude's environment, it checks the *effective*
/// routing -- `ANTHROPIC_BASE_URL` as Claude actually sees it -- rather than a
/// config file, plus that the desktop app is reachable. Pure stdlib so it runs
/// on the system `/usr/bin/python3`. Unlike Codex, Claude Code runs app-written
/// `settings.json` hooks without a manual trust step.
fn build_claude_guard_script() -> String {
    format!(
        r##"#!/usr/bin/env python3
"""Headroom Claude routing guard (managed by Headroom Desktop -- do not edit)."""
import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE_URL = "{base}"
READYZ = "{readyz}"
# stderr fires every invocation; the macOS notification is rate-limited so an
# app restart doesn't produce a storm of alerts.
DEBOUNCE_PATH = pathlib.Path(__file__).with_name(".headroom-local-community-guard-notified")
DEBOUNCE_SECONDS = 600


def notify(message):
    if sys.platform == "win32":
        return
    try:
        if time.time() - DEBOUNCE_PATH.stat().st_mtime < DEBOUNCE_SECONDS:
            return
    except OSError:
        pass
    try:
        DEBOUNCE_PATH.touch()
        subprocess.run(
            [
                "/usr/bin/osascript",
                "-e",
                'display notification ' + json.dumps(message) + ' with title "Headroom Claude guard"',
            ],
            check=False,
            timeout=5,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


def probe():
    # Any HTTP response means our server answered -- the app is up. A 503 during
    # bypass mode is still "up", so only connection errors / timeouts count as down.
    try:
        urllib.request.urlopen(READYZ, timeout=2)
        return True
    except urllib.error.HTTPError:
        return True
    except Exception:
        return False


def reachable():
    # One retry after a short pause so an app-relaunch blip doesn't read as "down".
    if probe():
        return True
    time.sleep(2)
    return probe()


def settings_base(path):
    # env.ANTHROPIC_BASE_URL from a Claude settings file, or None if absent/unreadable.
    try:
        with open(path) as handle:
            data = json.load(handle)
    except Exception:
        return None
    env = data.get("env") if isinstance(data, dict) else None
    if isinstance(env, dict):
        value = env.get("ANTHROPIC_BASE_URL")
        return str(value) if value is not None else None
    return None


def diagnose_route(effective):
    # A real routing break is (a) a higher-precedence project-local scope pointing
    # elsewhere, or (b) neither user settings nor the session env routing to Headroom.
    # settings.json's env is what Claude Code actually applies to its API calls, so
    # a correct user settings + unset process env (GUI / `open` launch that didn't
    # inherit the shell export) is HEALTHY, not a failure -- don't warn on it.
    shown = effective if effective else "unset"
    home = os.path.expanduser("~")
    user_val = settings_base(os.path.join(home, ".claude", "settings.json"))
    cwd = os.getcwd()
    for path in (
        os.path.join(cwd, ".claude", "settings.local.json"),
        os.path.join(cwd, ".claude", "settings.json"),
    ):
        val = settings_base(path)
        if val is not None and val != BASE_URL:
            return "ANTHROPIC_BASE_URL -- " + path + " sets it to " + val + ", which overrides Headroom's route (" + BASE_URL + "). Remove or fix that entry."
    if user_val != BASE_URL and effective != BASE_URL:
        return "ANTHROPIC_BASE_URL is not routed to Headroom (user settings: " + (str(user_val) if user_val else "no entry") + ", session env: " + shown + "). Reopen the Headroom app or re-run client setup."
    return None


def main():
    issues = []
    route_issue = diagnose_route(os.environ.get("ANTHROPIC_BASE_URL"))
    if route_issue:
        issues.append(route_issue)
    if not reachable():
        issues.append("Headroom Desktop is not reachable on 127.0.0.1:6867 -- it may be restarting; open the app if it isn't")

    if issues:
        notify("; ".join(issues))
        sys.stderr.write("Headroom Claude guard failed:\n")
        for issue in issues:
            sys.stderr.write("- " + issue + "\n")
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"##,
        base = HEADROOM_ANTHROPIC_BASE_URL,
        readyz = "http://127.0.0.1:6867/readyz",
    )
}

/// Write the Claude guard script and register it in `~/.claude/settings.json`
/// for SessionStart only. No trust step required.
///
/// Never UserPromptSubmit: exit 2 there blocks the prompt, and Claude Desktop /
/// Cowork VM sessions read the same settings.json but can never reach
/// 127.0.0.1:6867 from inside the VM, so the guard bricked every prompt in the
/// Claude desktop app while the routing it verifies didn't even apply there.
fn ensure_claude_guard_hook() -> Result<(Vec<String>, Vec<String>)> {
    let script_path = claude_guard_hook_path();
    let (script_changed, script_backup) =
        write_file_if_changed(&script_path, &build_claude_guard_script(), true)?;
    // Migration: earlier builds also registered on UserPromptSubmit; strip that
    // entry from existing installs. Match on the script path (see codex counterpart).
    remove_guard_hook_entries(
        &claude_settings_path(),
        &script_path.display().to_string(),
        false,
        Some(&["UserPromptSubmit"]),
    )?;
    // Migration: the Windows command string changed (PowerShell needs the call
    // operator), and `register_guard_hook_entries` dedupes on the exact command
    // string, so an upgrading install would keep the old unparseable entry
    // alongside the fixed one and keep erroring at every session start. Strip
    // stale forms by script path -- but only when the current command is not
    // already registered, since an unconditional remove-then-re-add would
    // rewrite settings.json on every launch.
    if !claude_guard_registered().unwrap_or(false) {
        remove_guard_hook_entries(
            &claude_settings_path(),
            &script_path.display().to_string(),
            false,
            Some(&["SessionStart"]),
        )?;
    }
    let (mut changed, mut backups) = register_guard_hook_entries(
        &claude_settings_path(),
        &claude_guard_command(),
        CLAUDE_GUARD_STATUS_MESSAGE,
        &[("SessionStart", Some("startup|resume|clear|compact"))],
    )?;
    if script_changed {
        changed.insert(0, script_path.display().to_string());
    }
    if let Some(backup) = script_backup {
        backups.insert(0, backup.display().to_string());
    }
    Ok((changed, backups))
}

fn claude_guard_registered() -> Result<bool> {
    guard_registered_in_hooks(&claude_settings_path(), &claude_guard_command())
}

/// Strip the Claude guard from every settings candidate and delete the script.
/// Never deletes settings.json (it carries other keys), so `delete_if_empty` is
/// false.
fn remove_claude_guard_hook() -> Result<()> {
    let script_path = claude_guard_hook_path();
    // Match on the script path, not the full interpreter command (see codex counterpart).
    let fragment = script_path.display().to_string();
    for settings_path in claude_settings_candidates() {
        remove_guard_hook_entries(&settings_path, &fragment, false, None)?;
    }
    if script_path.exists() {
        std::fs::remove_file(&script_path)
            .with_context(|| format!("removing {}", script_path.display()))?;
    }
    Ok(())
}

/// Run `codex doctor` as an independent confirmation that Codex itself accepts
/// the route (stronger than our "is the text in the file" checks). Best-effort
/// and never a hard failure: a missing CLI or a doctor error for unrelated
/// reasons must not flip `verified`.
fn codex_doctor_summary() -> Option<String> {
    let codex = find_on_path(&["codex"])?;
    let output = crate::proc::command(codex).arg("doctor").output().ok()?;
    if output.status.success() {
        Some("`codex doctor` reports the Codex install is healthy.".into())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("run `codex doctor` for details");
        Some(format!("`codex doctor` reported issues: {}", first.trim()))
    }
}

fn remove_launchctl_env(keys: &[&str]) -> Result<()> {
    for key in keys {
        let _ = run_launchctl(&["unsetenv", key]);
    }
    Ok(())
}

fn run_launchctl(args: &[&str]) -> Result<std::process::Output> {
    let output = crate::proc::command("launchctl")
        .args(args)
        .output()
        .with_context(|| format!("running launchctl {}", args.join(" ")))?;
    if output.status.success() {
        return Ok(output);
    }

    Err(anyhow!(
        "launchctl {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn normalized_setup_id(client_id: &str) -> &str {
    match client_id {
        "codex" | "codex_gui" => "codex_cli",
        "vscode" => "claude_code",
        other => other,
    }
}

fn upsert_managed_block(
    file_path: &Path,
    block_id: &str,
    block_body: &str,
) -> Result<(bool, Option<PathBuf>)> {
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let existing = if file_path.exists() {
        std::fs::read_to_string(file_path)
            .with_context(|| format!("reading {}", file_path.display()))?
    } else {
        String::new()
    };

    let start = format!("# >>> headroom-local-community:{block_id} >>>");
    let end = format!("# <<< headroom-local-community:{block_id} <<<");
    let block = format!("{start}\n{block_body}\n{end}\n");
    let updated = match (existing.find(&start), existing.find(&end)) {
        // Only rewrite in place when the markers are well-ordered. A stray or
        // reordered end-before-start (leftover from an interrupted write, or a
        // hand-pasted/duplicated half-block) makes `end_with_marker < start_idx`,
        // so `existing[..start_idx]` re-emits the region the suffix also carries
        // and the old opening marker gets duplicated. Mirror strip_marker_block:
        // treat a malformed block as absent and append a fresh one instead.
        (Some(start_idx), Some(end_idx)) if end_idx >= start_idx => {
            let end_with_marker = end_idx + end.len();
            let mut rebuilt = String::with_capacity(existing.len() + block.len());
            rebuilt.push_str(&existing[..start_idx]);
            rebuilt.push_str(&block);
            if end_with_marker < existing.len() {
                // `block` already ends in `\n`; if the surviving suffix also
                // starts with `\n`, drop one to avoid blank-line padding
                // accumulating between managed blocks on repeat applies.
                let suffix = &existing[end_with_marker..];
                let suffix = suffix.strip_prefix('\n').unwrap_or(suffix);
                rebuilt.push_str(suffix);
            }
            rebuilt
        }
        _ if existing.trim().is_empty() => block,
        _ => format!("{}\n{}", existing.trim_end(), block),
    };

    if updated == existing {
        return Ok((false, None));
    }

    let backup = backup_if_exists(file_path)?;
    atomic_write(file_path, updated.as_bytes())?;
    Ok((true, backup))
}

fn write_file_if_changed(
    file_path: &Path,
    content: &str,
    executable: bool,
) -> Result<(bool, Option<PathBuf>)> {
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let existing = if file_path.exists() {
        Some(
            std::fs::read_to_string(file_path)
                .with_context(|| format!("reading {}", file_path.display()))?,
        )
    } else {
        None
    };

    if existing.as_deref() == Some(content) {
        #[cfg(unix)]
        if executable {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(file_path)
                .with_context(|| format!("reading {}", file_path.display()))?
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(file_path, permissions)
                .with_context(|| format!("chmod {}", file_path.display()))?;
        }
        return Ok((false, None));
    }

    let backup = backup_if_exists(file_path)?;
    atomic_write(file_path, content.as_bytes())?;

    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(file_path)
            .with_context(|| format!("reading {}", file_path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(file_path, permissions)
            .with_context(|| format!("chmod {}", file_path.display()))?;
    }

    Ok((true, backup))
}

fn remove_shell_block(shell_targets: &[PathBuf], block_id: &str) -> Result<()> {
    for file in shell_targets {
        remove_managed_block(&file, block_id)?;
    }
    Ok(())
}

fn remove_managed_block(file_path: &Path, block_id: &str) -> Result<bool> {
    if !file_path.exists() {
        return Ok(false);
    }

    let bytes =
        std::fs::read(file_path).with_context(|| format!("reading {}", file_path.display()))?;
    let Ok(existing) = String::from_utf8(bytes) else {
        // ponytail: a non-UTF-8 profile is left untouched -- rewriting it from a
        // lossy decode would mangle the user's own bytes. Cost: a stale managed
        // block survives uninstall on such a file. Upgrade path if that matters:
        // splice the block out at the byte level instead of via String.
        log::info!(
            "leaving {} alone: not valid UTF-8, cannot rewrite safely",
            file_path.display()
        );
        return Ok(false);
    };
    let start = format!("# >>> headroom-local-community:{block_id} >>>");
    let end = format!("# <<< headroom-local-community:{block_id} <<<");

    let Some(start_idx) = existing.find(&start) else {
        return Ok(false);
    };
    let Some(end_idx) = existing[start_idx + start.len()..]
        .find(&end)
        .map(|end_idx| start_idx + start.len() + end_idx)
    else {
        return Ok(false);
    };

    let end_with_marker = end_idx + end.len();
    let tail = existing[end_with_marker..].trim_start_matches('\n');
    let mut rebuilt = String::with_capacity(existing.len());
    rebuilt.push_str(existing[..start_idx].trim_end());
    if !rebuilt.is_empty() && !tail.is_empty() {
        rebuilt.push('\n');
    }
    rebuilt.push_str(tail);
    if !rebuilt.is_empty() && !rebuilt.ends_with('\n') {
        rebuilt.push('\n');
    }

    let _ = backup_if_exists(file_path)?;
    atomic_write(file_path, rebuilt.as_bytes())?;
    Ok(true)
}

pub(crate) fn backup_if_exists(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }

    // Setup paths can update the same file within one second. A timestamp-only
    // name would let the later copy overwrite the earlier rollback snapshot.
    static BACKUP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = BACKUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = Utc::now().format("%Y%m%d%H%M%S");
    let backup_path = PathBuf::from(format!(
        "{}.headroom-local-community-backup-{}-{}-{}",
        path.display(),
        stamp,
        std::process::id(),
        counter
    ));
    std::fs::copy(path, &backup_path)
        .with_context(|| format!("creating backup {}", backup_path.display()))?;

    // Prune old backups — keep only the 3 most recent for this base path.
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let headroom_prefix = format!("{}.headroom-local-community-backup-", file_name);
    if let Some(dir) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut backups: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with(&headroom_prefix))
                        .unwrap_or(false)
                })
                .collect();
            backups.sort();
            if backups.len() > 3 {
                for old in &backups[..backups.len() - 3] {
                    let _ = std::fs::remove_file(old);
                }
            }
        }
    }

    Ok(Some(backup_path))
}

/// Reject a known JSON schema mismatch before any in-memory replacement can be
/// persisted. Keep a rollback copy and emit a local warning so callers can
/// surface the exact file that needs manual repair.
fn reject_invalid_json_container(path: &Path, field: &str, expected: &str) -> Result<()> {
    let backup = backup_if_exists(path).with_context(|| {
        format!(
            "backing up {} before rejecting invalid {field} schema",
            path.display()
        )
    })?;

    if let Some(backup_path) = backup.as_ref() {
        log::warn!(
            "refusing to rewrite {}: {field} must be {expected}; original preserved in {}",
            path.display(),
            backup_path.display()
        );
    } else {
        log::warn!(
            "refusing to rewrite {}: {field} must be {expected}; original preserved",
            path.display()
        );
    }

    let backup_suffix = backup
        .as_ref()
        .map(|backup_path| format!("; backup: {}", backup_path.display()))
        .unwrap_or_default();
    Err(anyhow!(
        "{} has invalid {field} schema (expected {expected}); refusing to overwrite original{backup_suffix}",
        path.display()
    ))
}

/// Reads a file for inspection only, replacing invalid UTF-8 instead of failing
/// on it. Shell profiles can carry non-UTF-8 bytes (RUST-5X), and marker/export
/// scanning only ever looks for ASCII. Never write the result back -- the lossy
/// decode would replace the user's bytes with U+FFFD.
fn read_to_string_lossy(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn shell_block_contains_in_files(
    shell_targets: &[PathBuf],
    block_id: &str,
    var_name: &str,
    expected_value: &str,
) -> Result<bool> {
    for file in shell_targets {
        if !file.exists() {
            continue;
        }
        let content = read_to_string_lossy(file)?;
        let start = format!("# >>> headroom-local-community:{block_id} >>>");
        let end = format!("# <<< headroom-local-community:{block_id} <<<");

        if let (Some(start_idx), Some(end_idx)) = (content.find(&start), content.find(&end)) {
            let block = &content[start_idx..end_idx];
            let expected_line = format!("export {var_name}={expected_value}");
            if block.contains(&expected_line) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn shell_block_contains_text_in_files(
    shell_targets: &[PathBuf],
    block_id: &str,
    expected_text: &str,
) -> Result<bool> {
    for file in shell_targets {
        if !file.exists() {
            continue;
        }

        let content = read_to_string_lossy(file)?;
        let start = format!("# >>> headroom-local-community:{block_id} >>>");
        let end = format!("# <<< headroom-local-community:{block_id} <<<");

        if let (Some(start_idx), Some(end_idx)) = (content.find(&start), content.find(&end)) {
            if content[start_idx..end_idx].contains(expected_text) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn claude_settings_env_matches(env_key: &str, expected_value: &str) -> Result<bool> {
    let path = claude_settings_path();
    if !path.exists() {
        return Ok(false);
    }

    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let content: Value = Value::Object(parse_json_object(&raw, &path)?);
    Ok(matches!(
        content.get("env").and_then(|env| env.get(env_key)),
        Some(Value::String(value)) if value == expected_value
    ))
}

fn claude_settings_hook_matches(hook_fragment: &str) -> Result<bool> {
    let path = claude_settings_path();
    if !path.exists() {
        return Ok(false);
    }

    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let content: Value = Value::Object(parse_json_object(&raw, &path)?);

    Ok(content
        .get("hooks")
        .and_then(|hooks| hooks.get("PreToolUse"))
        .and_then(|hooks| hooks.as_array())
        .map(|entries| {
            entries
                .iter()
                .any(|entry| entry_contains_hook(entry, hook_fragment))
        })
        .unwrap_or(false))
}

/// Pure core for `detect_oss_remnants`: given the environment facts, produce the
/// operator-facing warnings. Stale open-source-install remnants coexisting with
/// the paid desktop app are the root cause of instability under concurrent
/// agents (duplicate `mcp serve`, `:8787` vs Cursor OAuth callback conflicts,
/// hooks pointing at a non-app binary). Kept pure so it is unit-testable.
fn oss_remnant_warnings(
    local_headroom_exists: bool,
    local_rtk_exists: bool,
    port_8787_listening: bool,
    claude_hook_points_at_local_bin: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if port_8787_listening {
        warnings.push(
            "An open-source Headroom proxy is listening on :8787. It conflicts with the paid \
             desktop proxy (:6867/:6868) and Cursor MCP OAuth callbacks. Stop it and remove the \
             open-source install."
                .into(),
        );
    }
    if local_headroom_exists {
        warnings.push(
            "Found a stale open-source binary at ~/.local/bin/headroom. Remove it so only the \
             app-owned runtime serves MCP."
                .into(),
        );
    }
    if local_rtk_exists {
        warnings.push(
            "Found a stale open-source binary at ~/.local/bin/rtk. Remove it so the Claude hook \
             uses the app-owned RTK binary."
                .into(),
        );
    }
    if claude_hook_points_at_local_bin {
        warnings.push(
            "The Claude hook in ~/.claude/settings.json points at ~/.local/bin (open-source \
             install) instead of the app-owned binary. Re-run client setup to repair it."
                .into(),
        );
    }
    warnings
}

/// Gather real environment facts and return OSS-remnant warnings, empty when the
/// install is clean.
pub fn detect_oss_remnants() -> Vec<String> {
    let local_bin = home_dir().join(".local").join("bin");
    let hook_points_at_local_bin = std::fs::read_to_string(claude_settings_path())
        .map(|raw| raw.contains(".local/bin/rtk") || raw.contains(".local/bin/headroom"))
        .unwrap_or(false);
    oss_remnant_warnings(
        local_bin.join("headroom").exists(),
        local_bin.join("rtk").exists(),
        port_listening(8787),
        hook_points_at_local_bin,
    )
}

/// True when something accepts a TCP connection on `127.0.0.1:<port>`.
fn port_listening(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// The open-source Claude Code plugin runs this bare command at SessionStart
/// and before Bash/PowerShell calls, where it exits 127 because the app ships
/// no `headroom` on PATH. Replace only that exact command: no global PATH
/// mutation, and the same rewrite works on Windows, macOS, and Linux.
const OSS_PLUGIN_HOOK_COMMAND: &str = "headroom init hook ensure";
/// What we put in its place: a builtin every hook host we can be launched
/// under (sh, cmd.exe, PowerShell) understands. Deliberately not a path to a
/// file we ship -- an absolute path goes dead if our app data is ever removed
/// or relocated, stranding the plugin with a hook that fails on every Bash
/// call and a restore string we can no longer match.
const OSS_PLUGIN_MANAGED_COMMAND: &str = "exit 0";
static OSS_PLUGIN_HOOK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Escape hatch. `HEADROOM_ABSORB_OSS_PLUGIN=0` restores anything we already
/// rewrote and then leaves the plugin alone.
fn oss_absorb_disabled() -> bool {
    std::env::var_os("HEADROOM_ABSORB_OSS_PLUGIN").is_some_and(|v| v == "0")
}

/// True when Claude Code has the open-source `headroom` plugin installed, from
/// any marketplace. It is mirrored under several marketplace names, so match the
/// plugin half of the `<plugin>@<marketplace>` key rather than a fixed ref.
fn oss_headroom_plugin_installed() -> bool {
    let Some(plugins) = crate::tool_manager::claude_installed_plugins() else {
        return false;
    };
    let Some(map) = plugins.get("plugins").and_then(Value::as_object) else {
        return false;
    };
    map.iter().any(|(key, installs)| {
        key.split('@').next() == Some("headroom")
            && installs.as_array().is_some_and(|list| !list.is_empty())
    })
}

/// Canonicalize a path while preserving a not-yet-created suffix. Plugin
/// installs normally exist when we inspect them, but a receipt can outlive a
/// plugin version directory after an update or uninstall. Resolving the
/// nearest existing ancestor still makes symlink escapes visible without
/// dropping those recoverable receipt entries on the floor.
fn canonicalize_with_missing_tail(path: &Path) -> std::io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path must be absolute",
        ));
    }

    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        let Some(name) = cursor.file_name() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has no existing ancestor",
            ));
        };
        missing.push(name.to_os_string());
        cursor = cursor.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has no existing ancestor",
            )
        })?;
    }

    let mut canonical = std::fs::canonicalize(cursor)?;
    for name in missing.iter().rev() {
        canonical.push(name);
    }
    Ok(canonical)
}

fn claude_plugin_cache_dir() -> PathBuf {
    home_dir().join(".claude").join("plugins").join("cache")
}

/// Only plugin cache paths may be inspected or rewritten. This check is done
/// after resolving symlinks (including the existing ancestor of a stale path)
/// so a malicious `installPath` or receipt cannot redirect cleanup into `/tmp`,
/// another user's home, or an arbitrary mounted directory. A hooks file must
/// also end in exactly `hooks.json`; receipts never authorize arbitrary files.
fn is_trusted_oss_plugin_hook_path(path: &Path) -> bool {
    let Ok(cache_root) = canonicalize_with_missing_tail(&claude_plugin_cache_dir()) else {
        return false;
    };
    let Ok(candidate) = canonicalize_with_missing_tail(path) else {
        return false;
    };
    candidate.starts_with(&cache_root)
        && candidate
            .file_name()
            .is_some_and(|name| name == OsStr::new("hooks.json"))
}

fn trusted_oss_plugin_install_root(path: &Path) -> Option<PathBuf> {
    if !path.is_dir() {
        return None;
    }
    let cache_root = canonicalize_with_missing_tail(&claude_plugin_cache_dir()).ok()?;
    let canonical = canonicalize_with_missing_tail(path).ok()?;
    canonical.starts_with(&cache_root).then_some(canonical)
}

/// Installed plugin records carry their cache directory. Resolve the hooks file
/// from there instead of guessing where Claude or its shell looks for commands.
fn oss_headroom_plugin_hook_paths() -> Vec<PathBuf> {
    let Some(plugins) = crate::tool_manager::claude_installed_plugins() else {
        return Vec::new();
    };
    let Some(map) = plugins.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut hooks = Vec::new();
    for (key, installs) in map {
        if key.split('@').next() != Some("headroom") {
            continue;
        }
        let Some(installs) = installs.as_array() else {
            continue;
        };
        for install in installs {
            let Some(root) = install.get("installPath").and_then(Value::as_str) else {
                continue;
            };
            let root = PathBuf::from(root);
            let Some(root) = trusted_oss_plugin_install_root(&root) else {
                log::warn!(
                    "oss plugin hook: ignoring untrusted installPath {}",
                    root.display()
                );
                continue;
            };
            for candidate in [
                root.join("hooks").join("hooks.json"),
                root.join("hooks.json"),
            ] {
                if is_trusted_oss_plugin_hook_path(&candidate) && candidate.is_file() {
                    hooks.push(candidate);
                }
            }
        }
    }
    dedupe_paths(hooks)
}

fn oss_plugin_hook_receipt_path() -> PathBuf {
    config_file(&app_data_dir(), "oss-plugin-hooks.json")
}

fn load_oss_plugin_hook_receipt() -> Vec<PathBuf> {
    let path = oss_plugin_hook_receipt_path();
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    match serde_json::from_slice::<Vec<PathBuf>>(&bytes) {
        Ok(paths) => paths
            .into_iter()
            .filter(|candidate| {
                if is_trusted_oss_plugin_hook_path(candidate) {
                    true
                } else {
                    log::warn!(
                        "oss plugin hook: ignoring untrusted receipt path {}",
                        candidate.display()
                    );
                    false
                }
            })
            .collect(),
        Err(err) => {
            // This file is the only record of which third-party hooks we
            // rewrote. Silently overwriting an unreadable one strands them
            // neutralized with nothing left pointing at them, so keep a copy.
            log::warn!(
                "oss plugin hook: unreadable receipt {}: {err}",
                path.display()
            );
            let _ = backup_if_exists(&path);
            Vec::new()
        }
    }
}

fn save_oss_plugin_hook_receipt(paths: &[PathBuf]) -> Result<()> {
    let path = oss_plugin_hook_receipt_path();
    if paths.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    atomic_write(&path, &serde_json::to_vec_pretty(paths)?)
}

fn hook_file_contains_command(path: &Path, command: &str) -> Result<bool> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading OSS plugin hooks {}", path.display()))?;
    Ok(raw.contains(&serde_json::to_string(command)?))
}

/// Exact JSON-string replacement preserves the plugin's formatting and becomes
/// a no-op if upstream changes the command or schema.
fn replace_oss_plugin_hook_command(path: &Path, from: &str, to: &str) -> Result<bool> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading OSS plugin hooks {}", path.display()))?;
    serde_json::from_str::<Value>(&raw)
        .with_context(|| format!("parsing OSS plugin hooks {}", path.display()))?;
    let from = serde_json::to_string(from)?;
    if !raw.contains(&from) {
        return Ok(false);
    }
    let updated = raw.replace(&from, &serde_json::to_string(to)?);
    atomic_write(path, updated.as_bytes())?;
    Ok(true)
}

/// What the open-source plugin/CLI look like on this machine right now.
pub struct OssPluginStatus {
    pub plugin_installed: bool,
    pub hook_absorbed: bool,
    pub cli_on_path: bool,
    /// An open-source proxy is serving on :8787 (the OSS default port).
    pub oss_proxy_8787: bool,
    /// Claude Code's `ANTHROPIC_BASE_URL` still points at our proxy.
    pub base_url_ours: bool,
}

/// True when Claude Code's `ANTHROPIC_BASE_URL` still points at our proxy.
fn claude_base_url_is_ours() -> bool {
    std::fs::read_to_string(claude_settings_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| {
            v.get("env")?
                .get("ANTHROPIC_BASE_URL")?
                .as_str()
                .map(|url| url == HEADROOM_ANTHROPIC_BASE_URL)
        })
        .unwrap_or(false)
}

/// True when a real open-source `headroom` CLI exists for the plugin hook to
/// run. `find_on_path` alone is not enough: a GUI launch inherits launchd's
/// bare PATH, which never contains `~/.local/bin` -- exactly where the OSS
/// installer puts the binary. Probing the known install locations too is what
/// keeps us from neutralizing a plugin hook that works. Same helper Claude/
/// Codex detection uses, so a broken binary counts as absent and gets absorbed.
fn oss_cli_present() -> bool {
    crate::claude_cli::probe_on_path("headroom").is_some()
        || crate::claude_cli::probe_known_paths("headroom").is_some()
}

pub fn absorb_oss_plugin() -> OssPluginStatus {
    // Probing runs `headroom --version` against every known install location,
    // so it execs whatever binary of that name happens to be on disk. Nobody
    // without the plugin needs that at every launch: the answer only decides
    // whether to leave a plugin hook alone.
    let probe = !oss_absorb_disabled() && oss_headroom_plugin_installed();
    absorb_oss_plugin_with_cli_on_path(probe && oss_cli_present())
}

/// Cheap poll for the one state a single startup pass cannot cover: Claude Code
/// updated the plugin, which re-clones into a fresh version directory that never
/// saw our rewrite, so the bare command is back and failing on every Bash call.
/// A tray app can sit for weeks between launches, so waiting for the next start
/// means weeks of 127s.
///
/// Deliberately narrow. It fires only for users we are already managing (a
/// non-empty receipt) and only for a hook path we have not rewritten, so a user
/// with a real OSS CLI -- whose receipt is empty because we restored theirs --
/// never sends us back through the exec probe on a timer.
pub fn oss_plugin_hook_needs_absorbing() -> bool {
    if oss_absorb_disabled() {
        return false;
    }
    let receipt = load_oss_plugin_hook_receipt();
    if receipt.is_empty() {
        return false;
    }
    oss_headroom_plugin_hook_paths().iter().any(|path| {
        !receipt.contains(path)
            && matches!(
                hook_file_contains_command(path, OSS_PLUGIN_HOOK_COMMAND),
                Ok(true)
            )
    })
}

fn absorb_oss_plugin_with_cli_on_path(cli_on_path: bool) -> OssPluginStatus {
    let plugin_installed = oss_headroom_plugin_installed();
    let hooks = oss_headroom_plugin_hook_paths();
    let absorb = plugin_installed && !cli_on_path && !oss_absorb_disabled();
    let hook_absorbed = {
        let _guard = OSS_PLUGIN_HOOK_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if crate::SHUTTING_DOWN.load(std::sync::atomic::Ordering::Acquire) {
            false
        } else {
            reconcile_oss_plugin_hooks(&hooks, absorb).0
        }
    };

    OssPluginStatus {
        plugin_installed,
        hook_absorbed,
        cli_on_path,
        oss_proxy_8787: port_listening(8787),
        base_url_ours: claude_base_url_is_ours(),
    }
}

/// Neutralize or restore the exact OSS hook command. The receipt retains cache
/// paths after plugin removal, so uninstall can still restore inactive caches.
fn reconcile_oss_plugin_hooks(current_hooks: &[PathBuf], absorb: bool) -> (bool, Vec<String>) {
    let managed = OSS_PLUGIN_MANAGED_COMMAND;
    let mut hooks = load_oss_plugin_hook_receipt();
    hooks.extend_from_slice(current_hooks);
    hooks = dedupe_paths(hooks);

    // Persist targets before touching third-party files. If the app crashes
    // after the rewrite, the next launch or uninstall can still restore them.
    if absorb {
        if let Err(err) = save_oss_plugin_hook_receipt(&hooks) {
            log::warn!("oss plugin hook: preparing receipt failed: {err:#}");
            return (false, Vec::new());
        }
    }

    let mut changed = Vec::new();
    let mut still_managed = Vec::new();
    for path in hooks {
        // The receipt outlives the files it names: a plugin update re-clones
        // into a new version dir and the old one goes away. That is the normal
        // end of an entry, not a failure -- skipping it here keeps the read
        // error out of the log (and out of Sentry) and lets the entry fall off
        // the receipt below.
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                log::warn!(
                    "oss plugin hook: inspecting {} failed: {err}",
                    path.display()
                );
                if !absorb {
                    still_managed.push(path);
                }
                continue;
            }
        }
        let result = if absorb {
            replace_oss_plugin_hook_command(&path, OSS_PLUGIN_HOOK_COMMAND, managed)
        } else {
            replace_oss_plugin_hook_command(&path, managed, OSS_PLUGIN_HOOK_COMMAND)
        };
        match result {
            Ok(true) => changed.push(path.display().to_string()),
            Ok(false) => {}
            Err(err) => log::warn!("oss plugin hook: {err:#}"),
        }
        match hook_file_contains_command(&path, managed) {
            Ok(true) => still_managed.push(path),
            Ok(false) => {}
            Err(err) => {
                log::warn!("oss plugin hook: {err:#}");
                if !absorb {
                    still_managed.push(path);
                }
            }
        }
    }

    if let Err(err) = save_oss_plugin_hook_receipt(&still_managed) {
        log::warn!("oss plugin hook: saving receipt failed: {err:#}");
    }
    (!still_managed.is_empty(), changed)
}

fn restore_oss_plugin_hooks() -> (bool, Vec<String>) {
    let _guard = OSS_PLUGIN_HOOK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let hooks = oss_headroom_plugin_hook_paths();
    reconcile_oss_plugin_hooks(&hooks, false)
}

fn resolve_default_shell_targets() -> Vec<PathBuf> {
    let mut targets =
        discover_managed_shell_targets(&["managed_rtk", "claude_code"]).unwrap_or_default();
    if targets.is_empty() {
        targets = default_shell_targets_for_family(detect_shell_family());
    }
    dedupe_shell_targets(targets)
}

fn detect_shell_family() -> ShellFamily {
    if let Some(shell_name) = std::env::var_os("SHELL")
        .and_then(|value| value.into_string().ok())
        .and_then(|value| {
            Path::new(&value)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_ascii_lowercase())
        })
    {
        if shell_name.contains("zsh") {
            return ShellFamily::Zsh;
        }
        if shell_name.contains("bash") {
            return ShellFamily::Bash;
        }
        if shell_name == "sh" {
            return ShellFamily::Posix;
        }
    }

    let has_zsh_files = [ZSH_PROFILE_FILE, ZSH_RC_FILE]
        .into_iter()
        .map(shell_path)
        .any(|path| path.is_file());
    let has_bash_files = [
        BASH_PROFILE_FILE,
        BASH_LOGIN_FILE,
        POSIX_PROFILE_FILE,
        BASH_RC_FILE,
    ]
    .into_iter()
    .map(shell_path)
    .any(|path| path.is_file());

    match (has_zsh_files, has_bash_files) {
        (true, false) => ShellFamily::Zsh,
        (false, true) => ShellFamily::Bash,
        _ if cfg!(target_os = "macos") => ShellFamily::Zsh,
        _ => ShellFamily::Bash,
    }
}

fn default_shell_targets_for_family(shell_family: ShellFamily) -> Vec<PathBuf> {
    match shell_family {
        ShellFamily::Zsh => {
            dedupe_shell_targets(vec![shell_path(ZSH_PROFILE_FILE), shell_path(ZSH_RC_FILE)])
        }
        ShellFamily::Bash => dedupe_shell_targets(vec![
            preferred_bash_profile_path(),
            shell_path(BASH_RC_FILE),
        ]),
        ShellFamily::Posix => dedupe_shell_targets(vec![shell_path(POSIX_PROFILE_FILE)]),
    }
}

fn preferred_bash_profile_path() -> PathBuf {
    [BASH_PROFILE_FILE, BASH_LOGIN_FILE, POSIX_PROFILE_FILE]
        .into_iter()
        .map(shell_path)
        .find(|path| path.is_file())
        .unwrap_or_else(|| shell_path(BASH_PROFILE_FILE))
}

fn discover_managed_shell_targets(block_ids: &[&str]) -> Result<Vec<PathBuf>> {
    let mut discovered = Vec::new();
    for file in all_shell_paths() {
        for block_id in block_ids {
            if file_has_managed_block(&file, block_id)? {
                discovered.push(file.clone());
                break;
            }
        }
    }
    Ok(dedupe_paths(discovered))
}

fn shell_targets_from_state(serialized_paths: Option<&Vec<String>>) -> Vec<PathBuf> {
    serialized_paths
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        // A buggy build could persist an unexpanded, relative path (e.g.
        // `$XDG_CONFIG_HOME/zsh/.zshrc`); re-using it would create files under
        // the Finder-launch cwd `/`. Drop non-absolute stragglers.
        .filter(|p| p.is_absolute())
        .collect::<Vec<_>>()
}

fn serialize_paths(paths: &[PathBuf]) -> Vec<String> {
    let mut serialized = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    dedupe_strings(&mut serialized);
    serialized
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        let key = path.display().to_string();
        if seen.insert(key) {
            deduped.push(path);
        }
    }
    deduped
}

/// Dedupe a shell-target list and drop anything that already exists as a
/// directory. Such a path is neither readable nor rewritable: `read_to_string`
/// fails with `EISDIR` ("Is a directory", os error 21), which aborted the whole
/// client setup for a user whose `~/.profile` is a directory (RUST-5X/5Y/5Z —
/// it broke claude_code, codex and grok_build alike). Paths that do not exist
/// yet stay eligible; we create those.
fn dedupe_shell_targets(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    dedupe_paths(paths.into_iter().filter(|path| !path.is_dir()).collect())
}

fn dedupe_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn all_shell_paths() -> Vec<PathBuf> {
    dedupe_shell_targets(ALL_SHELL_FILES.into_iter().map(shell_path).collect())
}

fn is_profile_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(ZSH_PROFILE_FILE | BASH_PROFILE_FILE | BASH_LOGIN_FILE | POSIX_PROFILE_FILE)
    )
}

fn file_has_managed_block(file_path: &Path, block_id: &str) -> Result<bool> {
    if !file_path.exists() {
        return Ok(false);
    }

    let content = read_to_string_lossy(file_path)?;
    let start = format!("# >>> headroom-local-community:{block_id} >>>");
    let end = format!("# <<< headroom-local-community:{block_id} <<<");
    Ok(content.contains(&start) && content.contains(&end))
}

fn shell_path(name: &str) -> PathBuf {
    match name {
        ZSH_PROFILE_FILE | ZSH_RC_FILE => zsh_dir().join(name),
        _ => home_dir().join(name),
    }
}

/// Directory zsh reads its rc/profile files from. zsh honors `$ZDOTDIR`
/// (falling back to `$HOME`); a Finder-launched app rarely inherits `$ZDOTDIR`
/// from the login shell, so when it's absent from our own env we recover it
/// from `~/.zshenv` — the file zsh always sources from `$HOME` and the
/// conventional place users set ZDOTDIR.
fn zsh_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZDOTDIR").filter(|v| !v.is_empty()) {
        let dir = PathBuf::from(dir);
        // A relative ZDOTDIR would create files under the (Finder-launch) cwd
        // of `/`. Only trust it if absolute; otherwise fall through to $HOME.
        if dir.is_absolute() {
            return dir;
        }
    }
    zdotdir_from_zshenv(&home_dir()).unwrap_or_else(home_dir)
}

/// Expand `$VAR` / `${VAR}` from the process env. Unset vars are left as the
/// literal `$VAR` so the caller can detect an unresolved (non-absolute) path
/// and fall back rather than creating a bogus relative dir.
fn expand_env_vars(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        let (name, next) = if bytes.get(i + 1) == Some(&b'{') {
            match raw[i + 2..].find('}') {
                Some(end) => (&raw[i + 2..i + 2 + end], i + 2 + end + 1),
                None => (&raw[i..i], i + 1), // unterminated `${` -> emit `$` literally
            }
        } else {
            let end = raw[i + 1..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .map(|o| i + 1 + o)
                .unwrap_or(raw.len());
            (&raw[i + 1..end], end)
        };
        match (!name.is_empty())
            .then(|| std::env::var(name).ok())
            .flatten()
        {
            Some(val) => out.push_str(&val),
            None => out.push_str(&raw[i..next]), // keep literal `$VAR` when unset
        }
        i = next;
    }
    out
}

fn zdotdir_from_zshenv(home: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(home.join(".zshenv")).ok()?;
    for line in content.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some(value) = line.strip_prefix("ZDOTDIR=") else {
            continue;
        };
        let raw = if let Some(inner) = value.strip_prefix('"').and_then(|v| v.split('"').next()) {
            inner
        } else if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.split('\'').next()) {
            inner
        } else {
            value.split([' ', '\t', '#']).next().unwrap_or("")
        };
        if raw.is_empty() {
            continue;
        }
        let expanded = if let Some(tail) = raw.strip_prefix("~/") {
            home.join(tail)
        } else if raw == "~" {
            home.to_path_buf()
        } else if let Some(tail) = raw
            .strip_prefix("$HOME/")
            .or_else(|| raw.strip_prefix("${HOME}/"))
        {
            home.join(tail)
        } else {
            PathBuf::from(expand_env_vars(raw))
        };
        // If expansion couldn't fully resolve the value (unset env var leaves a
        // literal `$`, or it's otherwise relative), returning it would create a
        // bogus dir relative to cwd (e.g. `$XDG_CONFIG_HOME/zsh` under `/`).
        // Fall back to $HOME instead.
        if !expanded.is_absolute() {
            return None;
        }
        return Some(expanded);
    }
    None
}

fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

fn headroom_rtk_hook_path() -> PathBuf {
    home_dir()
        .join(".claude")
        .join("hooks")
        .join("headroom-local-community-rtk-rewrite.sh")
}

fn headroom_markitdown_hook_path() -> PathBuf {
    home_dir()
        .join(".claude")
        .join("hooks")
        .join("headroom-local-community-markitdown-read.sh")
}

/// PreToolUse(Read) hook: when Claude reads a PDF, convert it to Markdown via
/// the managed `markitdown` and redirect the read at the converted file through
/// `updatedInput.file_path`. Fails open at every step so a missing binary,
/// oversized file, or conversion error falls through to a native Read.
///
/// Scoped to PDF deliberately: Claude Code's Read tool rejects unsupported
/// binary types (docx/pptx/xlsx) at input validation *before* PreToolUse hooks
/// run, so a hook can never intercept them. Office formats are handled instead
/// by the managed CLAUDE.md nudge that points Claude at the `markitdown` CLI.
fn build_headroom_markitdown_hook(markitdown_path: &Path, python_path: &Path) -> String {
    let markitdown = shell_double_quote(&markitdown_path.to_string_lossy());
    let python = shell_double_quote(&python_path.to_string_lossy());

    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

HEADROOM_MARKITDOWN="{markitdown}"
HEADROOM_PYTHON="{python}"

if [ ! -x "$HEADROOM_MARKITDOWN" ] || [ ! -x "$HEADROOM_PYTHON" ]; then
  exit 0
fi

INPUT="$(cat)"
if [ -z "$INPUT" ]; then
  exit 0
fi

HEADROOM_MD_CACHE="${{TMPDIR:-/tmp}}/headroom-markitdown"
mkdir -p "$HEADROOM_MD_CACHE" 2>/dev/null || exit 0

HEADROOM_MARKITDOWN_BIN="$HEADROOM_MARKITDOWN" HEADROOM_MD_CACHE="$HEADROOM_MD_CACHE" "$HEADROOM_PYTHON" -c 'import json, os, sys, subprocess, hashlib
ALLOWED = {{".pdf"}}
MAX_BYTES = 25 * 1024 * 1024
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
tool_input = data.get("tool_input")
if not isinstance(tool_input, dict):
    sys.exit(0)
fp = tool_input.get("file_path")
if not isinstance(fp, str) or not fp:
    sys.exit(0)
if os.path.splitext(fp)[1].lower() not in ALLOWED:
    sys.exit(0)
try:
    st = os.stat(fp)
except OSError:
    sys.exit(0)
if st.st_size > MAX_BYTES:
    sys.exit(0)
binpath = os.environ["HEADROOM_MARKITDOWN_BIN"]
cache = os.environ["HEADROOM_MD_CACHE"]
key = hashlib.sha256((os.path.abspath(fp) + ":" + str(st.st_mtime_ns)).encode()).hexdigest()[:16]
out = os.path.join(cache, key + ".md")
if not (os.path.exists(out) and os.path.getsize(out) > 0):
    try:
        subprocess.run([binpath, fp, "-o", out], check=True, capture_output=True, timeout=120)
    except Exception:
        sys.exit(0)
if not (os.path.exists(out) and os.path.getsize(out) > 0):
    sys.exit(0)
updated = dict(tool_input)
updated["file_path"] = out
json.dump({{"hookSpecificOutput": {{"hookEventName": "PreToolUse", "permissionDecision": "allow", "permissionDecisionReason": "Headroom MarkItDown conversion", "updatedInput": updated}}}}, sys.stdout)' <<<"$INPUT" 2>/dev/null || exit 0
"#
    )
}

fn shell_double_quote(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn build_headroom_rtk_hook(managed_rtk_path: &Path, managed_python_path: &Path) -> String {
    let rtk = shell_double_quote(&managed_rtk_path.to_string_lossy());
    let python = shell_double_quote(&managed_python_path.to_string_lossy());

    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

HEADROOM_RTK="{rtk}"
HEADROOM_PYTHON="{python}"

if [ ! -x "$HEADROOM_RTK" ] || [ ! -x "$HEADROOM_PYTHON" ]; then
  exit 0
fi

INPUT="$(cat)"
if [ -z "$INPUT" ]; then
  exit 0
fi

CMD="$("$HEADROOM_PYTHON" -c 'import json, sys; data = json.load(sys.stdin); cmd = data.get("tool_input", {{}}).get("command", ""); print(cmd if isinstance(cmd, str) else "")' <<<"$INPUT" 2>/dev/null || true)"
if [ -z "$CMD" ]; then
  exit 0
fi

# `rtk git diff --check` swallows the whitespace-error report the flag exists to
# produce (only the exit code survives), so any --check command must stay raw.
case " $CMD " in
  *" --check "*) exit 0 ;;
esac

REWRITTEN="$("$HEADROOM_RTK" rewrite "$CMD" 2>/dev/null || true)"
if [ -z "$REWRITTEN" ] || [ "$CMD" = "$REWRITTEN" ]; then
  exit 0
fi

# `rtk rewrite` emits a bare `rtk` leading token, which only resolves if the
# managed PATH export has propagated into this session's environment. GUI apps
# (VSCode, terminals) launched before rtk was enabled inherit a stale PATH, so
# `rtk` is missing and the rewrite would fail with "command not found". Pin the
# leading token to the managed binary's absolute path so it works regardless.
if [ "${{REWRITTEN%% *}}" = "rtk" ]; then
  REWRITTEN="$HEADROOM_RTK${{REWRITTEN#rtk}}"
fi

# Defense-in-depth: if the rewritten command's first token isn't resolvable
# (e.g. a partial uninstall left `rtk` missing from PATH), fall through to the
# original command instead of handing Claude Code a command that will fail with
# "command not found".
FIRST_TOKEN="${{REWRITTEN%% *}}"
case "$FIRST_TOKEN" in
  /*)
    [ -x "$FIRST_TOKEN" ] || exit 0
    ;;
  *)
    command -v "$FIRST_TOKEN" >/dev/null 2>&1 || exit 0
    ;;
esac

# The pin above only fixes the LEADING token. `rtk rewrite` also emits `rtk`
# embedded after a `&&`, `;`, or `|` (e.g. `cd web && rtk npx ...`), and those
# stay bare -- they fail with "command not found: rtk" in the non-interactive,
# non-login shell Claude Code's Bash tool spawns, which sources only ~/.zshenv
# (never the .zprofile/.zshrc where the managed PATH export lands). Prepend the
# managed bin dir to PATH for this one invocation so every `rtk`, at any
# position, resolves regardless of which profile files the shell sourced.
REWRITTEN="export PATH=\"$(dirname "$HEADROOM_RTK"):\$PATH\"; $REWRITTEN"

HEADROOM_RTK_REWRITTEN="$REWRITTEN" "$HEADROOM_PYTHON" -c 'import json, os, sys; data = json.load(sys.stdin); tool_input = data.get("tool_input"); 
if not isinstance(tool_input, dict):
    sys.exit(0)
updated = dict(tool_input)
updated["command"] = os.environ["HEADROOM_RTK_REWRITTEN"]
json.dump({{"hookSpecificOutput": {{"hookEventName": "PreToolUse", "permissionDecision": "allow", "permissionDecisionReason": "Headroom RTK auto-rewrite", "updatedInput": updated}}}}, sys.stdout)' <<<"$INPUT" 2>/dev/null || exit 0
"#
    )
}

/// `HOME` is checked before `dirs::home_dir()`: on Windows the dirs crate
/// resolves the profile via the known-folder API and ignores `HOME`, so an
/// env override (TestHome in tests, Git Bash parity in production) would be
/// silently bypassed and writes would land in the real profile. On Unix the
/// two sources agree, so the order change is a no-op there.
pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
}

/// Codex's home directory. Mirrors the Codex CLI and the upstream Headroom
/// proxy: honor `$CODEX_HOME` when set, else `~/.codex`. Staying in sync with
/// the proxy matters — if the two layers disagree on where Codex lives, the
/// provider retag rewrites a different store than the config it edited.
pub(crate) fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".codex"))
}

/// Grok Build's home directory. Honors `$GROK_HOME` when set, else `~/.grok`.
fn grok_home() -> PathBuf {
    std::env::var_os("GROK_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".grok"))
}

fn detect_claude_code_client(configured: bool) -> ClientStatus {
    let executable = claude_code_candidate_paths()
        .into_iter()
        .find(|path| path.exists())
        .or_else(|| find_on_path(&["claude", "claude-code"]));

    if let Some(path) = executable {
        return ClientStatus {
            id: "claude_code".into(),
            name: "Claude Code".into(),
            installed: true,
            configured,
            health: if configured {
                ClientHealth::Healthy
            } else {
                ClientHealth::Attention
            },
            notes: if configured {
                vec![
                    format!("Detected at {}", path.display()),
                    "Configured by Headroom.".into(),
                ]
            } else {
                vec![
                    format!("Detected at {}", path.display()),
                    "Route Claude Code through Headroom's localhost proxy so prompts stay lean."
                        .into(),
                ]
            },
        };
    }

    if claude_code_user_state_exists(&home_dir()) {
        return ClientStatus {
            id: "claude_code".into(),
            name: "Claude Code".into(),
            installed: true,
            configured,
            health: if configured {
                ClientHealth::Healthy
            } else {
                ClientHealth::Attention
            },
            notes: if configured {
                vec![
                    "Detected Claude Code data in ~/.claude.".into(),
                    "Configured by Headroom.".into(),
                ]
            } else {
                vec![
                    "Detected Claude Code data in ~/.claude.".into(),
                    "Claude Code appears to be installed, but Headroom could not resolve the CLI from its current launch PATH. This is common when Headroom starts outside your shell and Claude was installed via nvm or another user-local toolchain.".into(),
                ]
            },
        };
    }

    ClientStatus {
        id: "claude_code".into(),
        name: "Claude Code".into(),
        installed: false,
        configured: false,
        health: ClientHealth::NotDetected,
        notes: vec!["Not detected on this machine yet.".into()],
    }
}

fn claude_code_candidate_paths() -> Vec<PathBuf> {
    let home = home_dir();
    let binary_names = ["claude", "claude-code"];
    let mut candidates = vec![
        PathBuf::from("/usr/local/bin/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude-code"),
        PathBuf::from("/opt/homebrew/bin/claude-code"),
    ];

    let user_bin_dirs = vec![
        home.join(".local").join("bin"),
        home.join("bin"),
        home.join(".npm-global").join("bin"),
        home.join(".yarn").join("bin"),
        home.join(".config")
            .join("yarn")
            .join("global")
            .join("node_modules")
            .join(".bin"),
        home.join(".volta").join("bin"),
        home.join(".bun").join("bin"),
        home.join(".asdf").join("shims"),
        home.join(".mise").join("shims"),
        home.join(".nodenv").join("shims"),
    ];

    candidates.extend(binary_candidates_in_dirs(&user_bin_dirs, &binary_names));
    candidates.extend(nvm_binary_candidates(&home, &binary_names));
    dedupe_paths(candidates)
}

fn binary_candidates_in_dirs(directories: &[PathBuf], binary_names: &[&str]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for directory in directories {
        for binary_name in binary_names {
            candidates.push(directory.join(binary_name));
            if cfg!(windows) {
                for ext in windows_path_extensions() {
                    candidates.push(directory.join(format!("{binary_name}{ext}")));
                }
            }
        }
    }
    candidates
}

fn nvm_binary_candidates(home: &Path, binary_names: &[&str]) -> Vec<PathBuf> {
    let mut candidates = binary_candidates_in_dirs(
        &[home.join(".nvm").join("current").join("bin")],
        binary_names,
    );
    let versions_dir = home.join(".nvm").join("versions").join("node");
    let Ok(entries) = std::fs::read_dir(versions_dir) else {
        return candidates;
    };

    let mut version_bins = entries
        .flatten()
        .map(|entry| entry.path().join("bin"))
        .collect::<Vec<_>>();
    version_bins.sort();
    version_bins.reverse();
    candidates.extend(binary_candidates_in_dirs(&version_bins, binary_names));
    candidates
}

fn claude_code_user_state_exists(home: &Path) -> bool {
    let claude_root = home.join(".claude");
    claude_root.join("settings.json").exists()
        || claude_root.join("projects").exists()
        || claude_root.join("sessions").exists()
        || claude_root.join("statsig").exists()
}

fn detect_codex_client(configured: bool) -> ClientStatus {
    let executable = codex_candidate_paths()
        .into_iter()
        .find(|path| path.exists())
        .or_else(|| find_on_path(&["codex"]));

    let detected = executable
        .as_ref()
        .map(|path| format!("Detected at {}", path.display()))
        .or_else(|| {
            codex_user_state_exists()
                .then(|| format!("Detected Codex data in {}.", codex_home().display()))
        });

    if let Some(detected_note) = detected {
        return ClientStatus {
            id: "codex".into(),
            name: "Codex".into(),
            installed: true,
            configured,
            health: if configured {
                ClientHealth::Healthy
            } else {
                ClientHealth::Attention
            },
            notes: if configured {
                vec![detected_note, "Configured by Headroom.".into()]
            } else {
                vec![
                    detected_note,
                    "Route Codex through Headroom's localhost proxy so prompts stay lean.".into(),
                ]
            },
        };
    }

    ClientStatus {
        id: "codex".into(),
        name: "Codex".into(),
        installed: false,
        configured: false,
        health: ClientHealth::NotDetected,
        notes: vec!["Not detected on this machine yet.".into()],
    }
}

fn detect_grok_build_client(configured: bool) -> ClientStatus {
    let executable = grok_candidate_paths()
        .into_iter()
        .find(|path| path.exists())
        .or_else(|| find_on_path(&["grok"]));

    let detected = executable
        .as_ref()
        .map(|path| format!("Detected at {}", path.display()))
        .or_else(|| {
            grok_user_state_exists()
                .then(|| format!("Detected Grok Build data in {}.", grok_home().display()))
        });

    if let Some(detected_note) = detected {
        return ClientStatus {
            id: "grok_build".into(),
            name: "Grok Build".into(),
            installed: true,
            configured,
            health: if configured {
                ClientHealth::Healthy
            } else {
                ClientHealth::Attention
            },
            notes: if configured {
                vec![detected_note, "Configured by Headroom.".into()]
            } else {
                vec![
                    detected_note,
                    "Route Grok Build through Headroom's localhost proxy so prompts stay lean."
                        .into(),
                ]
            },
        };
    }

    ClientStatus {
        id: "grok_build".into(),
        name: "Grok Build".into(),
        installed: false,
        configured: false,
        health: ClientHealth::NotDetected,
        notes: vec!["Not detected on this machine yet.".into()],
    }
}

fn grok_candidate_paths() -> Vec<PathBuf> {
    let home = home_dir();
    let mut candidates = vec![
        // Official installer target (verified against grok 0.2.112).
        home.join(".grok").join("bin").join("grok"),
        PathBuf::from("/usr/local/bin/grok"),
        PathBuf::from("/opt/homebrew/bin/grok"),
        home.join(".grok")
            .join("downloads")
            .join("grok-macos-aarch64"),
        home.join(".grok")
            .join("downloads")
            .join("grok-macos-x86_64"),
    ];

    let user_bin_dirs = vec![
        home.join(".local").join("bin"),
        home.join("bin"),
        home.join(".cargo").join("bin"),
    ];
    candidates.extend(binary_candidates_in_dirs(&user_bin_dirs, &["grok"]));
    dedupe_paths(candidates)
}

/// Deliberately excludes config.toml: setup itself creates one, which would
/// make detection self-fulfilling after disable (same rule as opencode).
fn grok_user_state_exists() -> bool {
    let grok_root = grok_home();
    grok_root.join("auth.json").exists()
        || grok_root.join("sessions").exists()
        || grok_root.join("downloads").exists()
        || grok_root.join("bin").exists()
}

fn codex_candidate_paths() -> Vec<PathBuf> {
    let home = home_dir();
    let binary_names = ["codex"];
    let mut candidates = vec![
        PathBuf::from("/usr/local/bin/codex"),
        PathBuf::from("/opt/homebrew/bin/codex"),
    ];

    let user_bin_dirs = vec![
        home.join(".local").join("bin"),
        home.join(".cargo").join("bin"),
        home.join("bin"),
        home.join(".npm-global").join("bin"),
        home.join(".yarn").join("bin"),
        home.join(".volta").join("bin"),
        home.join(".bun").join("bin"),
        home.join(".asdf").join("shims"),
        home.join(".mise").join("shims"),
        home.join(".nodenv").join("shims"),
    ];

    candidates.extend(binary_candidates_in_dirs(&user_bin_dirs, &binary_names));
    candidates.extend(nvm_binary_candidates(&home, &binary_names));
    #[cfg(target_os = "macos")]
    candidates.extend(codex_app_candidates(&home));
    dedupe_paths(candidates)
}

#[cfg(target_os = "macos")]
fn codex_app_candidates(home: &Path) -> Vec<PathBuf> {
    [PathBuf::from("/Applications"), home.join("Applications")]
        .into_iter()
        .flat_map(|root| ["Codex.app", "ChatGPT.app"].map(|app| root.join(app).join("Contents/Resources/codex")))
        .collect()
}

fn codex_user_state_exists() -> bool {
    let codex_root = codex_home();
    codex_root.join("config.toml").exists()
        || codex_root.join("auth.json").exists()
        || codex_root.join("sessions").exists()
}

/// Locate the Codex CLI binary the same way [`detect_codex_client`] does: known
/// install locations first, then a PATH lookup. Used as the Headroom Learn
/// analysis backend (`codex exec`) for Codex sessions.
pub(crate) fn detect_codex_cli() -> Option<PathBuf> {
    crate::claude_cli::first_runnable(codex_candidate_paths().into_iter())
        .or_else(|| crate::claude_cli::probe_on_path("codex"))
        .or_else(|| crate::claude_cli::probe_via_login_shell("codex"))
}

/// True once the user has signed in to Codex with their ChatGPT account — the
/// OAuth token lands in `~/.codex/auth.json`. Required for the keyless
/// `codex exec` analysis backend.
pub(crate) fn codex_logged_in() -> bool {
    codex_home().join("auth.json").is_file()
}

fn parse_json_object(raw: &str, path: &Path) -> Result<serde_json::Map<String, Value>> {
    let value: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => {
            let value = json5::from_str(raw).with_context(|| {
                format!(
                    "parsing {} failed (JSON/JSON5); refusing to overwrite potentially valid user settings",
                    path.display()
                )
            })?;
            // Writers re-serialize with serde_json, which strips the
            // comments/relaxed syntax that forced the JSON5 fallback. Log it
            // locally so the .headroom-local-community-backup is discoverable, but do NOT
            // capture to Sentry: this is expected, benign behavior (user keeps
            // comments in their settings), and the capture just inflated
            // RUST-4R with 120+ no-action events. Local info only.
            log::info!(
                "{} contains JSON5 syntax (comments/trailing commas); a Headroom rewrite will normalize it to strict JSON — the original is kept as a .headroom-local-community-backup file",
                path.display()
            );
            value
        }
    };
    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("{} must contain a top-level JSON object", path.display()))
}

pub(crate) fn find_on_path(binary_names: &[&str]) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    find_on_path_entries(std::env::split_paths(&path_var), binary_names)
}

fn find_on_path_entries<I>(path_entries: I, binary_names: &[&str]) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    for entry in path_entries {
        for binary_name in binary_names {
            // PATHEXT variants first on Windows: npm drops an extensionless
            // shim (`claude`, a bash script) next to `claude.cmd`, and only
            // the PATHEXT one is executable there. Matching the bare name
            // first handed callers a path Windows cannot spawn.
            if cfg!(windows) {
                for ext in windows_path_extensions() {
                    let with_ext = entry.join(format!("{binary_name}{ext}"));
                    if with_ext.exists() {
                        return Some(with_ext);
                    }
                }
            }

            let candidate = entry.join(binary_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

fn windows_path_extensions() -> Vec<String> {
    std::env::var_os("PATHEXT")
        .unwrap_or_else(|| OsStr::new(".COM;.EXE;.BAT;.CMD").to_os_string())
        .to_string_lossy()
        .split(';')
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with('.') {
                value.to_string()
            } else {
                format!(".{value}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn codex_bundle_candidates_cover_system_and_user_applications() {
        let home = std::path::Path::new("/Users/example");
        let candidates = super::codex_app_candidates(home);
        for path in [
            "/Applications/Codex.app/Contents/Resources/codex",
            "/Applications/ChatGPT.app/Contents/Resources/codex",
            "/Users/example/Applications/Codex.app/Contents/Resources/codex",
            "/Users/example/Applications/ChatGPT.app/Contents/Resources/codex",
        ] {
            assert!(candidates.contains(&std::path::PathBuf::from(path)));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundled_codex_is_resolved_without_shell_path_and_invalid_files_are_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Applications/ChatGPT.app/Contents/Resources/codex");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\n[ \"$1\" = --version ]\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let invalid = root.path().join("codex");
        std::fs::write(&invalid, "not an executable").unwrap();
        let candidates = vec![invalid, path.clone()];
        assert_eq!(crate::claude_cli::first_runnable(candidates.into_iter()), Some(path));
    }
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{
        build_claude_guard_script, build_codex_guard_script, build_headroom_markitdown_hook,
        build_headroom_rtk_hook, build_markitdown_codex_nudge, build_markitdown_office_nudge,
        claude_code_user_state_exists, claude_hook_present_in_value, codex_home,
        codex_route_is_ours,
        codex_sqlite_store_expected, default_shell_targets_for_family, discover_codex_state_dbs,
        entry_contains_hook, find_on_path_entries, is_no_space, is_permission_denied,
        normalize_setup_state, normalized_setup_id, nvm_binary_candidates, oss_remnant_warnings,
        parse_json_object, pin_codex_mcp_command, reconcile_codex_thread_providers_on_launch,
        remove_managed_block, remove_pre_tool_use_markers, render_codex_config,
        retag_codex_thread_providers, retag_codex_threads_to_native, retag_one_codex_db,
        serialize_paths,
        shell_block_contains_in_files, shell_block_contains_text_in_files, shell_double_quote,
        strip_headroom_hook_from_settings, upsert_managed_block, write_file_if_changed,
        ClientSetupState, ShellFamily, NO_SPACE_OS_ERRORS,
    };
    #[cfg(target_os = "windows")]
    use super::{claude_guard_command, codex_guard_command};
    use rusqlite::Connection;

    #[test]
    fn codex_desktop_process_parser_only_accepts_main_app_executables() {
        let output = "4242 Sun Aug 30 10:11:12 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT\n\
                      4243 Sun Aug 30 10:11:13 2026 /Applications/ChatGPT.app/Contents/Frameworks/ChatGPT Helper (Renderer).app/Contents/MacOS/ChatGPT Helper (Renderer)\n\
                      4244 Sun Aug 30 10:11:14 2026 /Applications/ChatGPT.app/Contents/Resources/codex app-server\n\
                      4245 Sun Aug 30 10:11:15 2026 /opt/homebrew/bin/codex\n\
                      4246 Sun Aug 30 10:11:16 2026 /Applications/Codex.app/Contents/MacOS/Codex\n";

        let processes = super::parse_codex_desktop_processes(output);

        assert_eq!(
            processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            vec![4242, 4246]
        );
    }

    #[test]
    fn codex_restart_requirement_compares_process_start_with_configuration_time() {
        let processes = super::parse_codex_desktop_processes(
            "4242 Sun Aug 30 10:11:12 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT\n",
        );
        let started_at = processes[0].started_at;

        assert_eq!(
            super::restart_required_from_processes(
                &processes,
                &(started_at + chrono::Duration::seconds(1)).to_rfc3339(),
            ),
            Some(true)
        );
        assert_eq!(
            super::restart_required_from_processes(
                &processes,
                &(started_at - chrono::Duration::seconds(1)).to_rfc3339(),
            ),
            Some(false)
        );
        assert_eq!(
            super::restart_required_from_processes(&[], &started_at.to_rfc3339()),
            Some(false)
        );
        assert_eq!(
            super::restart_required_from_processes(&processes, &started_at.to_rfc3339()),
            None
        );
        assert_eq!(
            super::restart_required_from_processes(&processes, "not-a-timestamp"),
            None
        );
    }

    #[test]
    fn codex_desktop_process_identity_rejects_reused_pids() {
        let expected = super::parse_codex_desktop_processes(
            "4242 Sun Aug 30 10:11:12 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT\n",
        )
        .remove(0);
        let same = super::parse_codex_desktop_processes(
            "4242 Sun Aug 30 10:11:12 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT\n",
        )
        .remove(0);
        let reused = super::parse_codex_desktop_processes(
            "4242 Sun Aug 30 10:11:13 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT\n",
        )
        .remove(0);
        let different_app = super::parse_codex_desktop_processes(
            "4242 Sun Aug 30 10:11:12 2026 /Applications/Codex.app/Contents/MacOS/Codex\n",
        )
        .remove(0);

        assert!(super::same_codex_desktop_process(&expected, &same));
        assert!(!super::same_codex_desktop_process(&expected, &reused));
        assert!(!super::same_codex_desktop_process(
            &expected,
            &different_app
        ));
    }

    #[test]
    fn strip_headroom_mcp_toml_removes_owned_tables_keeps_user_tables() {
        // Same straddled-HOME race as the JSON twin below: this reads
        // app_data_dir() to build the fixture and strip_headroom_mcp_toml
        // reads it again to match, while sibling tests flip HOME to a tempdir.
        let _env_lock = crate::test_env_lock::lock_home();
        let app_dir = crate::storage::app_data_dir().display().to_string();
        let content = format!(
            "model = \"gpt-5\"\n\
             \n\
             # --- Headroom Local Community MCP server ---\n\
             [mcp_servers.headroom_local_community]\n\
             command = \"{app_dir}/runtime/venv/bin/headroom\"\n\
             args = [\"mcp\", \"serve\"]\n\
             \n\
             [mcp_servers.headroom_local_community.env]\n\
             HEADROOM_PROXY_URL = \"http://127.0.0.1:6867\"\n\
             # --- end Headroom Local Community MCP server ---\n\
             # --- Headroom Local Community MCP server: serena ---\n\
             [mcp_servers.serena]\n\
             command = \"{app_dir}/serena-venv/bin/serena\"\n\
             \n\
             [mcp_servers.context7]\n\
             command = \"npx\"\n\
             \n\
             [mcp_servers.node_repl]\n\
             command = \"/Applications/ChatGPT.app/bin/node_repl\"\n"
        );
        let stripped = super::strip_headroom_mcp_toml(&content);
        assert!(!stripped.contains("mcp_servers.headroom_local_community"));
        assert!(!stripped.contains("serena"));
        assert!(!stripped.contains("Headroom Local Community MCP server"));
        assert!(stripped.contains("[mcp_servers.context7]"));
        assert!(stripped.contains("command = \"npx\""));
        assert!(stripped.contains("[mcp_servers.node_repl]"));
        assert!(stripped.contains("model = \"gpt-5\""));
    }

    #[test]
    fn strip_headroom_mcp_toml_is_noop_without_headroom_entries() {
        let content = "[mcp_servers.node_repl]\ncommand = \"/usr/local/bin/node_repl\"\n";
        assert_eq!(
            super::strip_headroom_mcp_toml(content),
            content.trim_end_matches('\n')
        );
    }

    #[test]
    fn remove_headroom_mcp_json_entries_removes_by_name_and_footprint() {
        // app_data_dir() derives from HOME, and this test reads it once to
        // build the fixture while remove_headroom_mcp_json_entries reads it
        // again to match. Sibling tests repoint HOME at a tempdir, so without
        // the lock the two reads can straddle a flip and disagree (~1 run in 6
        // of the full suite).
        let _env_lock = crate::test_env_lock::lock_home();
        let app_dir = crate::storage::app_data_dir().display().to_string();
        let mut servers = json!({
            "headroom": { "command": "python3", "args": ["mcp", "serve"] },
            "serena": { "command": format!("{app_dir}/serena-venv/bin/serena") },
            "codebase-memory": { "command": [format!("{app_dir}/runtime/bin/codebase-memory-mcp")] },
            "context7": { "command": "npx" },
        });
        let map = servers.as_object_mut().unwrap();
        assert!(super::remove_headroom_mcp_json_entries(map));
        assert!(map.get("headroom_local_community").is_none());
        assert!(map.get("serena").is_none());
        assert!(map.get("codebase-memory").is_none());
        assert!(map.get("context7").is_some());

        let mut untouched = json!({ "context7": { "command": "npx" } });
        assert!(!super::remove_headroom_mcp_json_entries(
            untouched.as_object_mut().unwrap()
        ));
    }

    #[test]
    fn is_permission_denied_matches_only_permission_errors() {
        // Construct by ErrorKind, not raw errno: 13 is EACCES on Unix but
        // ERROR_INVALID_DATA on Windows, where it does not map to
        // PermissionDenied.
        let denied = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "permission denied",
        ))
        .context("writing /Users/x/.zshrc");
        assert!(is_permission_denied(&denied));

        let not_found = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "not found",
        ))
        .context("writing /Users/x/.zshrc");
        assert!(!is_permission_denied(&not_found));

        assert!(!is_permission_denied(&anyhow::anyhow!("Permission denied")));
    }

    #[test]
    fn is_no_space_matches_only_disk_full_codes() {
        for &code in NO_SPACE_OS_ERRORS {
            let full = anyhow::Error::new(std::io::Error::from_raw_os_error(code)).context(
                "creating backup /Users/x/.claude/settings.json.headroom-local-community-backup",
            );
            assert!(is_no_space(&full));
        }

        let denied = anyhow::Error::new(std::io::Error::from_raw_os_error(13))
            .context("writing /Users/x/.zshrc");
        assert!(!is_no_space(&denied));

        assert!(!is_no_space(&anyhow::anyhow!("No space left on device")));
    }

    #[test]
    fn client_setup_state_survives_schema_drift_in_either_direction() {
        // A newer build's extra field must be ignored, and a build that
        // drops/renames configured_clients must still yield the rest. A parse
        // failure here returns the empty default, which reads as "nothing
        // configured": the tray reports Claude Code disconnected and uninstall
        // can no longer find the shell blocks listed in managedShellFiles.
        let newer = r#"{"configuredClients":{"claude_code":"2026-03-27T10:00:00Z"},
            "managedShellFiles":{"claude_code":["/Users/test/.zshrc"]},
            "someFutureFlag":42}"#;
        let state: ClientSetupState = serde_json::from_str(newer).unwrap();
        assert!(state.configured_clients.contains_key("claude_code"));
        assert!(state.managed_shell_files.contains_key("claude_code"));

        let dropped =
            r#"{"managedShellFiles":{"codex_cli":["/Users/test/.zshrc"]},"rtkDisabled":true}"#;
        let state: ClientSetupState = serde_json::from_str(dropped).unwrap();
        assert!(state.managed_shell_files.contains_key("codex_cli"));
        assert!(state.rtk_disabled);
    }

    #[test]
    fn quarantine_unparsable_keeps_each_recovery_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client-setup.json");
        std::fs::write(&path, b"{ truncated").unwrap();

        assert!(super::quarantine_unparsable(&path, "test"));
        assert!(
            !path.exists(),
            "original is moved, not left to be overwritten"
        );
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("client-setup.json.corrupt-"))
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), b"{ truncated");

        // Repeat failures get a new slot instead of destroying the first copy.
        std::fs::write(&path, b"{ again").unwrap();
        assert!(super::quarantine_unparsable(&path, "test"));
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("client-setup.json.corrupt-"))
            })
            .collect();
        assert_eq!(backups.len(), 2);
        assert!(backups.iter().any(|backup| {
            std::fs::read(backup)
                .map(|bytes| bytes == b"{ truncated")
                .unwrap_or(false)
        }));
        assert!(backups.iter().any(|backup| {
            std::fs::read(backup)
                .map(|bytes| bytes == b"{ again")
                .unwrap_or(false)
        }));

        // Missing file is a no-op, not an error.
        assert!(!super::quarantine_unparsable(
            &dir.path().join("absent.json"),
            "test"
        ));
    }

    #[test]
    fn normalize_setup_state_keeps_codex_but_drops_legacy_codex_gui() {
        let state = ClientSetupState {
            configured_clients: BTreeMap::from([
                ("claude_code".into(), "2026-03-27T10:00:00Z".into()),
                ("codex_cli".into(), "2026-03-27T10:01:00Z".into()),
                ("codex_gui".into(), "2026-03-27T10:02:00Z".into()),
            ]),
            remembered_clients: BTreeMap::from([
                ("codex".into(), "2026-03-27T10:03:00Z".into()),
                ("claude_code".into(), "2026-03-27T10:04:00Z".into()),
            ]),
            managed_shell_files: BTreeMap::from([
                ("claude_code".into(), vec!["/Users/test/.zprofile".into()]),
                ("codex_cli".into(), vec!["/Users/test/.zshrc".into()]),
                ("codex_gui".into(), vec!["/Users/test/.zshrc".into()]),
            ]),
            remembered_shell_files: BTreeMap::from([
                ("codex".into(), vec!["/Users/test/.bash_profile".into()]),
                ("claude_code".into(), vec!["/Users/test/.bashrc".into()]),
            ]),
            preserved_base_urls: BTreeMap::new(),
            rtk_disabled: false,
            auto_learn_disabled: false,
            persistence_blocked: false,
        };

        let normalized = normalize_setup_state(state);

        // codex_cli stays configured; only the removed codex_gui id is stripped.
        assert!(normalized.configured_clients.contains_key("claude_code"));
        assert!(normalized.configured_clients.contains_key("codex_cli"));
        assert!(!normalized.configured_clients.contains_key("codex_gui"));

        assert!(normalized.remembered_clients.contains_key("claude_code"));
        assert!(normalized.remembered_clients.contains_key("codex"));

        assert!(normalized.managed_shell_files.contains_key("claude_code"));
        assert!(normalized.managed_shell_files.contains_key("codex_cli"));
        assert!(!normalized.managed_shell_files.contains_key("codex_gui"));

        assert!(normalized
            .remembered_shell_files
            .contains_key("claude_code"));
        assert!(normalized.remembered_shell_files.contains_key("codex"));
    }

    #[test]
    fn parse_json_object_accepts_json5_but_rejects_non_objects() {
        let parsed = parse_json_object(
            "{ unquoted: 'value', trailing: true, }",
            Path::new("settings.json"),
        )
        .expect("json5 object should parse");
        assert_eq!(
            parsed.get("unquoted").and_then(|value| value.as_str()),
            Some("value")
        );
        assert_eq!(
            parsed.get("trailing").and_then(|value| value.as_bool()),
            Some(true)
        );

        let err =
            parse_json_object("[]", Path::new("settings.json")).expect_err("arrays are rejected");
        assert!(err
            .to_string()
            .contains("must contain a top-level JSON object"));
    }

    #[test]
    fn setup_aliases_map_to_current_primary_ids() {
        assert_eq!(normalized_setup_id("codex"), "codex_cli");
        assert_eq!(normalized_setup_id("codex_gui"), "codex_cli");
        assert_eq!(normalized_setup_id("vscode"), "claude_code");
        assert_eq!(normalized_setup_id("claude_code"), "claude_code");
    }

    #[test]
    fn shell_double_quote_escapes_shell_sensitive_characters() {
        let escaped = shell_double_quote("path with spaces/$HOME/\"quoted\"`cmd`\\tail");
        assert_eq!(
            escaped,
            "path with spaces/\\$HOME/\\\"quoted\\\"\\`cmd\\`\\\\tail"
        );
    }

    #[test]
    fn shell_targets_include_profile_and_rc_for_supported_shells() {
        let zsh_targets = default_shell_targets_for_family(ShellFamily::Zsh);
        let bash_targets = default_shell_targets_for_family(ShellFamily::Bash);

        assert!(zsh_targets.iter().any(|path| path.ends_with(".zprofile")));
        assert!(zsh_targets.iter().any(|path| path.ends_with(".zshrc")));
        assert!(bash_targets.iter().any(|path| {
            path.ends_with(".bash_profile")
                || path.ends_with(".bash_login")
                || path.ends_with(".profile")
        }));
        assert!(bash_targets.iter().any(|path| path.ends_with(".bashrc")));
    }

    #[test]
    fn serialize_paths_dedupes_repeated_entries() {
        let serialized = serialize_paths(&[
            PathBuf::from("/Users/test/.zprofile"),
            PathBuf::from("/Users/test/.zprofile"),
            PathBuf::from("/Users/test/.zshrc"),
        ]);

        assert_eq!(
            serialized,
            vec![
                "/Users/test/.zprofile".to_string(),
                "/Users/test/.zshrc".to_string()
            ]
        );
    }

    #[test]
    fn generated_rtk_hook_uses_escaped_paths_and_rewrite_reason() {
        let hook = build_headroom_rtk_hook(
            Path::new("/tmp/head room/bin/rtk"),
            Path::new("/tmp/head room/runtime/$python"),
        );

        assert!(hook.contains("HEADROOM_RTK=\"/tmp/head room/bin/rtk\""));
        assert!(hook.contains("HEADROOM_PYTHON=\"/tmp/head room/runtime/\\$python\""));
        assert!(hook.contains("Headroom RTK auto-rewrite"));
        assert!(hook.contains("\"updatedInput\": updated"));
    }

    #[test]
    fn generated_markitdown_hook_escapes_paths_and_redirects_read() {
        let hook = build_headroom_markitdown_hook(
            Path::new("/tmp/head room/venv/bin/markitdown"),
            Path::new("/tmp/head room/venv/bin/$python"),
        );

        assert!(hook.contains("HEADROOM_MARKITDOWN=\"/tmp/head room/venv/bin/markitdown\""));
        assert!(hook.contains("HEADROOM_PYTHON=\"/tmp/head room/venv/bin/\\$python\""));
        // Scoped to PDF only (Office is handled by the nudge, not the hook),
        // redirects via updatedInput, and fails open.
        assert!(hook.contains("ALLOWED = {\".pdf\"}"));
        assert!(!hook.contains(".docx"));
        assert!(hook.contains("updated[\"file_path\"] = out"));
        assert!(hook.contains("\"updatedInput\": updated"));
        assert!(hook.contains("Headroom MarkItDown conversion"));
        assert!(hook.contains("sys.exit(0)"));
    }

    #[test]
    fn disabling_markitdown_marker_leaves_rtk_hook_intact() {
        let root = unique_temp_dir("headroom-strip-markitdown");
        fs::create_dir_all(&root).expect("create root");
        let settings = root.join("settings.json");
        fs::write(
            &settings,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [
                        { "matcher": "Bash", "hooks": [{ "type": "command", "command": "/h/headroom-local-community-rtk-rewrite.sh" }] },
                        { "matcher": "Read", "hooks": [{ "type": "command", "command": "/h/headroom-local-community-markitdown-read.sh" }] }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write settings");

        let changed = remove_pre_tool_use_markers(
            &settings,
            &["headroom-local-community-markitdown-read.sh"],
        )
        .expect("strip");
        assert!(changed);

        let after: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        let entries = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entry_contains_hook(
            &entries[0],
            "headroom-local-community-rtk-rewrite.sh"
        ));
    }

    #[test]
    fn markitdown_office_nudge_points_at_the_shim_and_skips_pdf() {
        let nudge = build_markitdown_office_nudge(Path::new("/h/bin/markitdown"));
        assert!(nudge.contains("\"/h/bin/markitdown\" \"<path>\""));
        assert!(nudge.contains(".docx"));
        assert!(nudge.contains("PDFs may also be handled by the installed Read hook"));
    }

    #[test]
    fn markitdown_codex_nudge_covers_pdf_and_office() {
        let nudge = build_markitdown_codex_nudge(Path::new("/h/bin/markitdown"));
        assert!(nudge.contains("\"/h/bin/markitdown\" \"<path>\""));
        // Codex has no hook, so PDF is covered by the CLI nudge too.
        assert!(nudge.contains(".pdf"));
        assert!(nudge.contains(".docx"));
    }

    #[test]
    fn hook_detection_finds_nested_hook_commands() {
        let hook_path = "/Users/test/.claude/hooks/headroom-local-community-rtk-rewrite.sh";
        let content = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "bash",
                        "hooks": [
                            { "type": "command", "command": hook_path }
                        ]
                    }
                ]
            }
        });

        assert!(claude_hook_present_in_value(&content, hook_path));
        assert!(entry_contains_hook(
            &content["hooks"]["PreToolUse"][0],
            "headroom-local-community-rtk-rewrite.sh"
        ));
        assert!(!entry_contains_hook(
            &json!({ "hooks": [] }),
            "headroom-local-community-rtk-rewrite.sh"
        ));
    }

    #[test]
    fn nvm_binary_candidates_include_installed_versions() {
        let home = unique_temp_dir("headroom-nvm-detect");
        let version_bin = home
            .join(".nvm")
            .join("versions")
            .join("node")
            .join("v22.17.1")
            .join("bin");
        fs::create_dir_all(&version_bin).expect("create nvm bin");
        fs::write(version_bin.join("claude"), "").expect("write fake claude binary");

        let candidates = nvm_binary_candidates(&home, &["claude"]);

        assert!(candidates
            .iter()
            .any(|candidate| candidate == &version_bin.join("claude")));

        let _ = fs::remove_dir_all(home);
    }

    /// npm installs drop an extensionless bash shim next to the `.cmd`, and
    /// only the `.cmd` is spawnable on Windows. Matching the bare name first
    /// handed every caller (`claude`, `codex`, `npx`) a dead path.
    #[test]
    #[cfg(windows)]
    fn windows_path_lookup_prefers_the_pathext_variant() {
        let home = unique_temp_dir("headroom-path-pathext");
        let bin_dir = home.join("custom-bin");
        fs::create_dir_all(&bin_dir).expect("create custom bin");
        fs::write(bin_dir.join("claude"), "").expect("write npm bash shim");
        fs::write(bin_dir.join("claude.cmd"), "").expect("write npm cmd shim");

        let detected = find_on_path_entries(vec![bin_dir.clone()], &["claude"]).expect("detected");

        // The extension's CASE comes from PATHEXT, which is uppercase on a real
        // Windows box (`.COM;.EXE;.BAT;.CMD`), so the returned path is the one
        // we constructed -- `claude.CMD` -- not the on-disk spelling. It spawns
        // either way because NTFS is case-insensitive; only a byte-compare in a
        // test can tell them apart. Assert what actually matters: the .cmd was
        // picked over the bare shim.
        assert_eq!(detected.parent(), Some(bin_dir.as_path()));
        assert!(
            detected
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("claude.cmd")),
            "{}",
            detected.display()
        );

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn path_lookup_scans_supplied_entries() {
        let home = unique_temp_dir("headroom-path-detect");
        let bin_dir = home.join("custom-bin");
        fs::create_dir_all(&bin_dir).expect("create custom bin");
        fs::write(bin_dir.join("claude"), "").expect("write fake claude binary");

        let detected = find_on_path_entries(vec![bin_dir.clone()], &["claude"]);

        assert_eq!(detected, Some(bin_dir.join("claude")));

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn claude_user_state_detection_accepts_settings_or_projects() {
        let home = unique_temp_dir("headroom-claude-home");
        let claude_root = home.join(".claude");
        fs::create_dir_all(&claude_root).expect("create claude root");
        assert!(!claude_code_user_state_exists(&home));

        fs::write(claude_root.join("settings.json"), "{}").expect("write settings");
        assert!(claude_code_user_state_exists(&home));

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn managed_block_upsert_replaces_existing_block_without_duplication() {
        let root = unique_temp_dir("headroom-managed-block");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join(".zshrc");
        fs::write(&path, "export PATH=/usr/bin\n").expect("write shell file");

        let first = upsert_managed_block(
            &path,
            "claude_code",
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:6867",
        )
        .expect("insert managed block");
        assert!(first.0);
        assert!(first.1.is_some());

        upsert_managed_block(
            &path,
            "claude_code",
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:6867\nexport HEADROOM_LOCAL_COMMUNITY=1",
        )
        .expect("replace managed block");

        let content = fs::read_to_string(&path).expect("read updated shell file");
        assert_eq!(
            content
                .matches("# >>> headroom-local-community:claude_code >>>")
                .count(),
            1
        );
        assert!(content.contains("export PATH=/usr/bin"));
        assert!(content.contains("export HEADROOM_LOCAL_COMMUNITY=1"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_block_upsert_treats_reordered_markers_as_absent() {
        // A stray end-before-start block (leftover from an interrupted write).
        // The old slice-based rewrite duplicated the stray fragments and left a
        // dangling opening marker at the tail; the guarded path appends a fresh,
        // well-formed block instead.
        let root = unique_temp_dir("headroom-reordered-markers");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join(".zshrc");
        fs::write(
            &path,
            "# <<< headroom-local-community:claude_code <<<\nstray old body\n# >>> headroom-local-community:claude_code >>>\n",
        )
        .expect("write malformed shell file");

        upsert_managed_block(
            &path,
            "claude_code",
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:6867",
        )
        .expect("upsert over malformed block");

        let content = fs::read_to_string(&path).expect("read updated shell file");
        // Tail must be a well-ordered block: the last opening marker precedes the
        // last closing marker, and the file ends on the closing marker (not on a
        // dangling opener as the buggy slice produced).
        let last_start = content
            .rfind("# >>> headroom-local-community:claude_code >>>")
            .expect("start marker present");
        let last_end = content
            .rfind("# <<< headroom-local-community:claude_code <<<")
            .expect("end marker present");
        assert!(last_start < last_end, "tail block must be well-ordered");
        assert!(content
            .trim_end()
            .ends_with("# <<< headroom-local-community:claude_code <<<"));
        assert!(content.contains("export ANTHROPIC_BASE_URL=http://127.0.0.1:6867"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remove_managed_block_ignores_an_end_marker_before_its_start() {
        let root = unique_temp_dir("headroom-remove-reordered-markers");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join(".zshrc");
        let original = "# <<< headroom-local-community:claude_code <<<\nstray old body\n# >>> headroom-local-community:claude_code >>>\n";
        fs::write(&path, original).expect("write malformed shell file");

        assert!(!remove_managed_block(&path, "claude_code").expect("ignore malformed block"));
        assert_eq!(
            fs::read_to_string(&path).expect("read untouched shell file"),
            original
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remove_managed_block_keeps_surrounding_shell_content_intact() {
        let root = unique_temp_dir("headroom-remove-block");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join(".zprofile");
        fs::write(
            &path,
            "export PATH=/usr/bin\n# >>> headroom-local-community:claude_code >>>\nexport ANTHROPIC_BASE_URL=http://127.0.0.1:6867\n# <<< headroom-local-community:claude_code <<<\nexport EDITOR=vim\n",
        )
        .expect("write shell file");

        let removed = remove_managed_block(&path, "claude_code").expect("remove managed block");

        assert!(removed);
        assert_eq!(
            fs::read_to_string(&path).expect("read cleaned shell file"),
            "export PATH=/usr/bin\nexport EDITOR=vim\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn shell_block_helpers_only_match_content_inside_the_named_block() {
        let root = unique_temp_dir("headroom-shell-match");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join(".bashrc");
        fs::write(
            &path,
            "export ANTHROPIC_BASE_URL=https://example.com\n# >>> headroom-local-community:claude_code >>>\nexport ANTHROPIC_BASE_URL=http://127.0.0.1:6867\nexport PATH=/tmp/headroom:$PATH\n# <<< headroom-local-community:claude_code <<<\n",
        )
        .expect("write shell file");

        assert!(shell_block_contains_in_files(
            &[path.clone()],
            "claude_code",
            "ANTHROPIC_BASE_URL",
            "http://127.0.0.1:6867",
        )
        .expect("detect managed export"));
        assert!(
            shell_block_contains_text_in_files(&[path.clone()], "claude_code", "export PATH=",)
                .expect("detect managed text")
        );
        assert!(!shell_block_contains_in_files(
            &[path],
            "managed_rtk",
            "ANTHROPIC_BASE_URL",
            "http://127.0.0.1:6867",
        )
        .expect("ignore other block ids"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_file_if_changed_skips_backups_when_content_is_unchanged() {
        let root = unique_temp_dir("headroom-write-file");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join("headroom-local-community-rtk-rewrite.sh");
        fs::write(&path, "#!/bin/sh\necho headroom\n").expect("write hook file");

        let changed = write_file_if_changed(&path, "#!/bin/sh\necho headroom\n", false)
            .expect("skip unchanged write");

        assert_eq!(changed, (false, None));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_block_round_trip_preserves_realistic_zshrc_content() {
        let root = unique_temp_dir("headroom-zshrc-roundtrip");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join(".zshrc");
        let original = r#"export NVM_DIR="$HOME/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"

# pnpm
export PNPM_HOME="/Users/test/Library/pnpm"
case ":$PATH:" in
  *":$PNPM_HOME:"*) ;;
  *) export PATH="$PNPM_HOME:$PATH" ;;
esac

export BUN_INSTALL="$HOME/.bun"
export PATH="$BUN_INSTALL/bin:$PATH"
"#;
        fs::write(&path, original).expect("write zshrc");

        upsert_managed_block(
            &path,
            "managed_rtk",
            "export PATH=\"/tmp/headroom/bin:$PATH\"",
        )
        .expect("add managed rtk block");
        upsert_managed_block(
            &path,
            "claude_code",
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:6867",
        )
        .expect("add claude block");

        remove_managed_block(&path, "claude_code").expect("remove claude block");
        remove_managed_block(&path, "managed_rtk").expect("remove managed rtk block");

        let final_content = fs::read_to_string(&path).expect("read round-tripped zshrc");
        assert_eq!(final_content, original);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn updating_one_managed_block_does_not_touch_other_blocks_or_user_content() {
        let root = unique_temp_dir("headroom-multi-block-update");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join(".zprofile");
        let original = r#"eval "$(/opt/homebrew/bin/brew shellenv)"

# >>> headroom-local-community:managed_rtk >>>
export PATH="/old/headroom/bin:$PATH"
# <<< headroom-local-community:managed_rtk <<<

# >>> headroom-local-community:claude_code >>>
export ANTHROPIC_BASE_URL=http://127.0.0.1:6867
# <<< headroom-local-community:claude_code <<<

eval "$(/opt/homebrew/bin/rbenv init - zsh)"
"#;
        fs::write(&path, original).expect("write zprofile");

        upsert_managed_block(
            &path,
            "managed_rtk",
            "export PATH=\"/new/headroom/bin:$PATH\"",
        )
        .expect("update managed rtk block");

        let updated = fs::read_to_string(&path).expect("read updated zprofile");
        assert!(updated.contains("eval \"$(/opt/homebrew/bin/brew shellenv)\""));
        assert!(updated.contains("eval \"$(/opt/homebrew/bin/rbenv init - zsh)\""));
        assert!(updated.contains("export PATH=\"/new/headroom/bin:$PATH\""));
        assert!(updated.contains("export ANTHROPIC_BASE_URL=http://127.0.0.1:6867"));
        assert_eq!(
            updated
                .matches("# >>> headroom-local-community:managed_rtk >>>")
                .count(),
            1
        );
        assert_eq!(
            updated
                .matches("# >>> headroom-local-community:claude_code >>>")
                .count(),
            1
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn removing_one_managed_block_leaves_other_managed_blocks_and_user_content() {
        let root = unique_temp_dir("headroom-remove-single-block");
        fs::create_dir_all(&root).expect("create root");
        let path = root.join(".zshrc");
        fs::write(
            &path,
            r#"export NVM_DIR="$HOME/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"

# >>> headroom-local-community:managed_rtk >>>
export PATH="/tmp/headroom/bin:$PATH"
# <<< headroom-local-community:managed_rtk <<<

# >>> headroom-local-community:claude_code >>>
export ANTHROPIC_BASE_URL=http://127.0.0.1:6867
# <<< headroom-local-community:claude_code <<<
"#,
        )
        .expect("write zshrc");

        remove_managed_block(&path, "claude_code").expect("remove claude block");

        let updated = fs::read_to_string(&path).expect("read cleaned zshrc");
        assert!(updated.contains("export NVM_DIR=\"$HOME/.nvm\""));
        assert!(updated.contains("[ -s \"$NVM_DIR/nvm.sh\" ] && \\. \"$NVM_DIR/nvm.sh\""));
        assert!(updated.contains("# >>> headroom-local-community:managed_rtk >>>"));
        assert!(updated.contains("export PATH=\"/tmp/headroom/bin:$PATH\""));
        assert!(!updated.contains("# >>> headroom-local-community:claude_code >>>"));

        let _ = fs::remove_dir_all(root);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }

    #[test]
    fn zdotdir_from_zshenv_parses_common_forms() {
        let cases: [(&str, Option<&str>); 6] = [
            (
                "export ZDOTDIR=\"$HOME/.config/zsh\"\n",
                Some(".config/zsh"),
            ),
            ("ZDOTDIR=~/.config/zsh\n", Some(".config/zsh")),
            (
                "export ZDOTDIR='${HOME}/dotfiles/zsh'\n",
                Some("dotfiles/zsh"),
            ),
            ("# comment\nexport ZDOTDIR=$HOME/z  # trailing\n", Some("z")),
            ("export ZDOTDIR=~\n", Some("")),
            ("# no zdotdir here\nexport FOO=bar\n", None),
        ];
        for (i, (contents, expected_tail)) in cases.into_iter().enumerate() {
            let home = unique_temp_dir(&format!("headroom-zdotdir-{i}"));
            fs::create_dir_all(&home).unwrap();
            fs::write(home.join(".zshenv"), contents).unwrap();
            let got = super::zdotdir_from_zshenv(&home);
            let expected = expected_tail.map(|tail| {
                if tail.is_empty() {
                    home.clone()
                } else {
                    home.join(tail)
                }
            });
            assert_eq!(got, expected, "case {i}");
        }
        // Missing file -> None.
        let empty = unique_temp_dir("headroom-zdotdir-none");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(super::zdotdir_from_zshenv(&empty), None);
    }

    #[test]
    fn zdotdir_unresolved_env_var_falls_back_to_none() {
        // TestHome sets XDG_CONFIG_HOME under this lock, so without it the
        // remove_var below races those tests (~1 run in 6 of the full suite).
        let _env_lock = crate::test_env_lock::lock_home();
        // Reproduces os error 30: `$XDG_CONFIG_HOME` unset under a Finder launch
        // must NOT yield a relative `$XDG_CONFIG_HOME/zsh` path.
        std::env::remove_var("XDG_CONFIG_HOME");
        let home = unique_temp_dir("headroom-zdotdir-unresolved");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(".zshenv"),
            "export ZDOTDIR=\"$XDG_CONFIG_HOME/zsh\"\n",
        )
        .unwrap();
        assert_eq!(super::zdotdir_from_zshenv(&home), None);
    }

    #[test]
    fn strip_hook_returns_false_when_file_missing() {
        let root = unique_temp_dir("headroom-strip-missing");
        let settings = root.join("does-not-exist.json");
        let changed = strip_headroom_hook_from_settings(&settings).expect("strip should succeed");
        assert!(!changed, "missing file should report no change");
        assert!(!settings.exists(), "should not create the file");
    }

    #[test]
    fn strip_hook_removes_headroom_entry_and_leaves_other_entries() {
        let root = unique_temp_dir("headroom-strip-mixed");
        fs::create_dir_all(&root).expect("create root");
        let settings = root.join("settings.json");
        let content = json!({
            "env": { "SOME_KEY": "keep-me" },
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "/other/tool/script.sh" }
                        ]
                    },
                    {
                        "matcher": "Bash",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "/Users/test/.claude/hooks/headroom-local-community-rtk-rewrite.sh"
                            }
                        ]
                    }
                ]
            }
        });
        fs::write(&settings, serde_json::to_string_pretty(&content).unwrap())
            .expect("write settings");

        let changed = strip_headroom_hook_from_settings(&settings).expect("strip should succeed");
        assert!(changed, "should report change");

        let raw = fs::read_to_string(&settings).expect("read settings");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse settings");
        let entries = parsed
            .get("hooks")
            .and_then(|v| v.get("PreToolUse"))
            .and_then(|v| v.as_array())
            .expect("PreToolUse preserved");
        assert_eq!(entries.len(), 1, "only the non-headroom entry remains");
        assert!(
            entry_contains_hook(&entries[0], "other/tool/script.sh"),
            "unrelated entry preserved"
        );
        assert_eq!(
            parsed.get("env").and_then(|v| v.get("SOME_KEY")),
            Some(&json!("keep-me")),
            "unrelated top-level keys untouched"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn strip_hook_drops_empty_pre_tool_use_and_hooks_keys() {
        let root = unique_temp_dir("headroom-strip-empty");
        fs::create_dir_all(&root).expect("create root");
        let settings = root.join("settings.json");
        let content = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "/path/to/headroom-local-community-rtk-rewrite.sh"
                            }
                        ]
                    }
                ]
            }
        });
        fs::write(&settings, serde_json::to_string_pretty(&content).unwrap())
            .expect("write settings");

        let changed = strip_headroom_hook_from_settings(&settings).expect("strip should succeed");
        assert!(changed);

        let raw = fs::read_to_string(&settings).expect("read settings");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse settings");
        assert!(
            parsed.get("hooks").is_none(),
            "empty hooks object should be removed, got {parsed}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn strip_hook_leaves_file_untouched_when_no_headroom_entry_present() {
        let root = unique_temp_dir("headroom-strip-noop");
        fs::create_dir_all(&root).expect("create root");
        let settings = root.join("settings.json");
        let original = serde_json::to_string_pretty(&json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "/unrelated.sh" }
                        ]
                    }
                ]
            }
        }))
        .unwrap();
        fs::write(&settings, &original).expect("write settings");

        let changed = strip_headroom_hook_from_settings(&settings).expect("strip should succeed");
        assert!(!changed, "should report no change");

        let after = fs::read_to_string(&settings).expect("read settings");
        assert_eq!(after, original, "file should be byte-identical");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn strip_hook_tolerates_empty_file() {
        let root = unique_temp_dir("headroom-strip-empty-file");
        fs::create_dir_all(&root).expect("create root");
        let settings = root.join("settings.json");
        fs::write(&settings, "").expect("write empty file");

        let changed = strip_headroom_hook_from_settings(&settings).expect("strip should succeed");
        assert!(!changed, "empty file should report no change");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn hook_script_falls_through_when_rewritten_first_token_missing_from_path() {
        // The hook has an OR guard that exits 0 when the binaries are missing,
        // so we give it real paths and verify the PATH-resolution check kicks in
        // when `rtk rewrite` produces a command whose first token can't be
        // resolved. That's the regression-prone slice added this session.
        let root = unique_temp_dir("headroom-hook-bash");
        fs::create_dir_all(&root).expect("create root");

        // Fake rtk that always prepends a made-up binary name that won't be on PATH.
        let fake_rtk = root.join("fake-rtk");
        fs::write(
            &fake_rtk,
            "#!/usr/bin/env bash\nshift  # drop the 'rewrite' arg\necho \"__headroom_nonexistent_binary_xyzzy__ $*\"\n",
        )
        .expect("write fake rtk");
        fs::set_permissions(
            &fake_rtk,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod rtk");

        // Use the real system python3 so the embedded Python snippets run.
        let system_python = PathBuf::from("/usr/bin/python3");
        assert!(system_python.exists(), "this test assumes /usr/bin/python3");

        let hook_body = build_headroom_rtk_hook(&fake_rtk, &system_python);
        let hook_path = root.join("hook.sh");
        fs::write(&hook_path, &hook_body).expect("write hook");
        fs::set_permissions(
            &hook_path,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod hook");

        // Hook expects a JSON object on stdin with tool_input.command.
        let stdin = r#"{"tool_input":{"command":"git status"}}"#;
        let output = crate::proc::command("bash")
            .arg(&hook_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(stdin.as_bytes())
                    .unwrap();
                child.wait_with_output()
            })
            .expect("run hook");

        assert!(output.status.success(), "hook should exit 0");
        assert!(
            output.stdout.is_empty(),
            "hook should emit no rewrite when first token isn't resolvable, got: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn hook_script_passes_through_check_commands() {
        // `rtk git diff --check` swallows the whitespace report; the hook must
        // leave any --check command unrewritten even when rtk would rewrite it.
        let root = unique_temp_dir("headroom-hook-check");
        fs::create_dir_all(&root).expect("create root");

        let fake_rtk = root.join("fake-rtk");
        fs::write(
            &fake_rtk,
            "#!/usr/bin/env bash\nshift\necho \"/bin/echo $*\"\n",
        )
        .expect("write fake rtk");
        fs::set_permissions(
            &fake_rtk,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod rtk");

        let system_python = PathBuf::from("/usr/bin/python3");
        let hook_body = build_headroom_rtk_hook(&fake_rtk, &system_python);
        let hook_path = root.join("hook.sh");
        fs::write(&hook_path, &hook_body).expect("write hook");
        fs::set_permissions(
            &hook_path,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod hook");

        for cmd in ["git diff --cached --check", "git diff --check"] {
            let stdin = format!(r#"{{"tool_input":{{"command":"{cmd}"}}}}"#);
            let output = crate::proc::command("bash")
                .arg(&hook_path)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child
                        .stdin
                        .as_mut()
                        .unwrap()
                        .write_all(stdin.as_bytes())
                        .unwrap();
                    child.wait_with_output()
                })
                .expect("run hook");
            assert!(output.status.success(), "hook should exit 0 for {cmd}");
            assert!(
                output.stdout.is_empty(),
                "hook must not rewrite {cmd}, got: {:?}",
                String::from_utf8_lossy(&output.stdout)
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn hook_script_emits_rewrite_when_first_token_is_valid_absolute_path() {
        let root = unique_temp_dir("headroom-hook-bash-ok");
        fs::create_dir_all(&root).expect("create root");

        // Pick a binary that definitely exists on macOS/Linux test hosts.
        let real_binary = "/bin/echo";
        assert!(Path::new(real_binary).exists());

        // Fake rtk rewrites to use an absolute path that *does* exist.
        let fake_rtk = root.join("fake-rtk");
        fs::write(
            &fake_rtk,
            format!("#!/usr/bin/env bash\nshift\necho \"{real_binary} $*\"\n"),
        )
        .expect("write fake rtk");
        fs::set_permissions(
            &fake_rtk,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod rtk");

        let system_python = PathBuf::from("/usr/bin/python3");
        let hook_body = build_headroom_rtk_hook(&fake_rtk, &system_python);
        let hook_path = root.join("hook.sh");
        fs::write(&hook_path, &hook_body).expect("write hook");
        fs::set_permissions(
            &hook_path,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod hook");

        let stdin = r#"{"tool_input":{"command":"git status"}}"#;
        let output = crate::proc::command("bash")
            .arg(&hook_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(stdin.as_bytes())
                    .unwrap();
                child.wait_with_output()
            })
            .expect("run hook");

        assert!(output.status.success(), "hook should exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(real_binary),
            "rewrite should be emitted when first token is a valid absolute path, got stdout: {stdout:?}, stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("Headroom RTK auto-rewrite"),
            "should be a rewrite hookSpecificOutput payload"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn hook_script_pins_bare_rtk_token_to_managed_absolute_path() {
        let root = unique_temp_dir("headroom-hook-pin-rtk");
        fs::create_dir_all(&root).expect("create root");

        // Fake rtk emits a bare `rtk` leading token, like the real binary.
        // `rtk` is NOT on PATH here, so without pinning the rewrite would be a
        // "command not found" landmine and the defense-in-depth guard would
        // drop it. Pinning to the managed absolute path must keep the rewrite.
        let fake_rtk = root.join("rtk");
        fs::write(&fake_rtk, "#!/usr/bin/env bash\nshift\necho \"rtk $*\"\n")
            .expect("write fake rtk");
        fs::set_permissions(
            &fake_rtk,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod rtk");

        let system_python = PathBuf::from("/usr/bin/python3");
        let hook_body = build_headroom_rtk_hook(&fake_rtk, &system_python);
        let hook_path = root.join("hook.sh");
        fs::write(&hook_path, &hook_body).expect("write hook");
        fs::set_permissions(
            &hook_path,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod hook");

        let stdin = r#"{"tool_input":{"command":"git status"}}"#;
        let output = crate::proc::command("bash")
            .arg(&hook_path)
            .env("PATH", "/usr/bin:/bin") // ensure bare `rtk` is unresolvable
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(stdin.as_bytes())
                    .unwrap();
                child.wait_with_output()
            })
            .expect("run hook");

        assert!(output.status.success(), "hook should exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Headroom RTK auto-rewrite"),
            "rewrite should survive when bare `rtk` is pinned to absolute path, got stdout: {stdout:?}, stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&fake_rtk.to_string_lossy().replace('"', "\\\"")),
            "rewritten command should invoke the managed rtk by absolute path, got: {stdout:?}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn hook_script_prepends_managed_path_so_embedded_rtk_resolves() {
        // Regression for compound commands: `rtk rewrite` embeds a bare `rtk`
        // after `&&`/`;`/`|`, which the leading-token pin never touches. The
        // hook must prepend the managed bin dir to PATH so the embedded token
        // resolves in the non-interactive, non-login shell Claude Code spawns.
        let root = unique_temp_dir("headroom-hook-embedded-rtk");
        fs::create_dir_all(&root).expect("create root");

        // Fake rtk emits a compound command with rtk embedded mid-chain, like
        // the real binary does for `cd x && <cmd>`. The leading token is `cd`.
        let fake_rtk = root.join("rtk");
        fs::write(
            &fake_rtk,
            "#!/usr/bin/env bash\nshift\necho \"cd /tmp && rtk $*\"\n",
        )
        .expect("write fake rtk");
        fs::set_permissions(
            &fake_rtk,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod rtk");

        let system_python = PathBuf::from("/usr/bin/python3");
        let hook_body = build_headroom_rtk_hook(&fake_rtk, &system_python);
        let hook_path = root.join("hook.sh");
        fs::write(&hook_path, &hook_body).expect("write hook");
        fs::set_permissions(
            &hook_path,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod hook");

        let stdin = r#"{"tool_input":{"command":"git status"}}"#;
        let output = crate::proc::command("bash")
            .arg(&hook_path)
            .env("PATH", "/usr/bin:/bin") // bare `rtk` unresolvable without the prepend
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(stdin.as_bytes())
                    .unwrap();
                child.wait_with_output()
            })
            .expect("run hook");

        assert!(output.status.success(), "hook should exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Headroom RTK auto-rewrite"),
            "compound rewrite should be emitted, got stdout: {stdout:?}, stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The emitted command must export the managed bin dir onto PATH so the
        // embedded `rtk` resolves, and must preserve that embedded token.
        assert!(
            stdout.contains("export PATH="),
            "rewrite must prepend a PATH export, got: {stdout:?}"
        );
        assert!(
            stdout.contains(&root.to_string_lossy().replace('"', "\\\"")),
            "PATH export must point at the managed bin dir, got: {stdout:?}"
        );
        assert!(
            stdout.contains("&& rtk "),
            "embedded rtk token must be preserved, got: {stdout:?}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn hook_script_emits_rewrite_even_when_rtk_rewrite_exits_nonzero() {
        let root = unique_temp_dir("headroom-hook-bash-nonzero");
        fs::create_dir_all(&root).expect("create root");

        let real_binary = "/bin/echo";
        assert!(Path::new(real_binary).exists());

        // Match the real rtk behavior we observed during smoke testing:
        // emit a rewrite, then exit non-zero. The hook's `|| true` should
        // still preserve the rewritten command.
        let fake_rtk = root.join("fake-rtk");
        fs::write(
            &fake_rtk,
            format!("#!/usr/bin/env bash\nshift\necho \"{real_binary} $*\"\nexit 3\n"),
        )
        .expect("write fake rtk");
        fs::set_permissions(
            &fake_rtk,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod rtk");

        let system_python = PathBuf::from("/usr/bin/python3");
        let hook_body = build_headroom_rtk_hook(&fake_rtk, &system_python);
        let hook_path = root.join("hook.sh");
        fs::write(&hook_path, &hook_body).expect("write hook");
        fs::set_permissions(
            &hook_path,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod hook");

        let stdin = r#"{"tool_input":{"command":"git status"}}"#;
        let output = crate::proc::command("bash")
            .arg(&hook_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(stdin.as_bytes())
                    .unwrap();
                child.wait_with_output()
            })
            .expect("run hook");

        assert!(output.status.success(), "hook should exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(real_binary),
            "rewrite output should survive non-zero RTK exit, got stdout: {stdout:?}, stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("Headroom RTK auto-rewrite"),
            "should still emit a rewrite hookSpecificOutput payload"
        );

        let _ = fs::remove_dir_all(root);
    }

    // ── Lifecycle integration tests ──────────────────────────────────────────
    //
    // These tests drive `apply_client_setup` / `verify_client_setup` /
    // `disable_client_setup` / `clear_client_setups` against a temp $HOME so we
    // catch regressions in the user-visible setup-then-teardown flow. Tests are
    // serialized via `serial_test` because they mutate process-wide env vars
    // (HOME, XDG_DATA_HOME, SHELL).

    /// RAII-style guard that snapshots HOME / XDG_DATA_HOME / SHELL, points
    /// them at a fresh tempdir, and restores them on drop. Used to keep
    /// lifecycle tests from touching the developer's real profile.
    struct TestHome {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        prev_home: Option<std::ffi::OsString>,
        prev_xdg: Option<std::ffi::OsString>,
        prev_shell: Option<std::ffi::OsString>,
        prev_codex: Option<std::ffi::OsString>,
        prev_zdotdir: Option<std::ffi::OsString>,
        prev_xdg_config: Option<std::ffi::OsString>,
        prev_opencode_config: Option<std::ffi::OsString>,
        prev_grok_home: Option<std::ffi::OsString>,
        prev_headroom_data_dir: Option<std::ffi::OsString>,
        prev_appdata: Option<std::ffi::OsString>,
        prev_localappdata: Option<std::ffi::OsString>,
        // Held for the guard's lifetime: env vars are process-global, so two
        // TestHome tests running on parallel threads corrupt each other's HOME
        // (and can leak writes into the developer's real profile). serial_test
        // only covers tests that opted in; this lock covers every TestHome user.
        _env_lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TestHome {
        fn new() -> Self {
            let env_lock = crate::test_env_lock::lock_home();
            let tmp = tempfile::tempdir().expect("create temp home");
            let home = tmp.path().to_path_buf();
            let prev_home = std::env::var_os("HOME");
            let prev_xdg = std::env::var_os("XDG_DATA_HOME");
            let prev_shell = std::env::var_os("SHELL");
            let prev_codex = std::env::var_os("CODEX_HOME");
            let prev_zdotdir = std::env::var_os("ZDOTDIR");
            let prev_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
            let prev_opencode_config = std::env::var_os("OPENCODE_CONFIG");
            let prev_grok_home = std::env::var_os("GROK_HOME");
            let prev_headroom_data_dir = std::env::var_os("HEADROOM_DATA_DIR");
            let prev_appdata = std::env::var_os("APPDATA");
            let prev_localappdata = std::env::var_os("LOCALAPPDATA");
            std::env::set_var("HOME", &home);
            // Pin the Windows profile dirs into the temp home: opencode_config_dir
            // and perform_full_cleanup read these on Windows, and the runner's
            // real AppData is otherwise shared across all parallel test
            // processes. No-ops on Unix (only read under cfg windows).
            std::env::set_var("APPDATA", home.join("AppData").join("Roaming"));
            std::env::set_var("LOCALAPPDATA", home.join("AppData").join("Local"));
            std::env::set_var("XDG_DATA_HOME", home.join(".local").join("share"));
            // Pin the app data dir into the temp home. dirs::data_local_dir()
            // ignores HOME/XDG on macOS and Windows, so without this the setup
            // state, seeded rtk, and cleanup sweeps all hit the REAL profile —
            // and under nextest (process per test) the env lock below cannot
            // serialize that sharing across processes.
            std::env::set_var(
                "HEADROOM_DATA_DIR",
                home.join(".local")
                    .join("share")
                    .join("HeadroomLocalCommunity"),
            );
            // Pin XDG_CONFIG_HOME into the temp home and clear the opencode /
            // grok override vars: a dev machine with any of these set would
            // otherwise have the opencode/grok tests write the developer's
            // REAL client configs.
            std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
            std::env::remove_var("OPENCODE_CONFIG");
            std::env::remove_var("GROK_HOME");
            // Force a deterministic shell family so tests don't depend on the
            // dev's login shell.
            std::env::set_var("SHELL", "/bin/zsh");
            // Clear any real CODEX_HOME so codex_home() falls back to the temp
            // $HOME/.codex and the Codex tests stay hermetic on dev machines.
            std::env::remove_var("CODEX_HOME");
            // Clear any real ZDOTDIR so zsh_dir() resolves against the temp
            // $HOME and the shell-block tests stay hermetic on dev machines.
            std::env::remove_var("ZDOTDIR");
            // Clear every var huggingface_hub honours, so hf_hub_cache_dir()
            // resolves to the temp $HOME. Without this, a dev with HF_HOME or
            // HF_HUB_CACHE set would have the cleanup tests delete models out
            // of their REAL HuggingFace cache.
            for var in [
                "HF_HUB_CACHE",
                "HUGGINGFACE_HUB_CACHE",
                "HF_HOME",
                "XDG_CACHE_HOME",
            ] {
                std::env::remove_var(var);
            }
            // Mirror what the app does at startup so write_setup_state has a
            // config dir to land in.
            crate::storage::ensure_data_dirs(&crate::storage::app_data_dir())
                .expect("ensure_data_dirs in test home");
            TestHome {
                _tmp: tmp,
                home,
                prev_home,
                prev_xdg,
                prev_shell,
                prev_codex,
                prev_zdotdir,
                prev_xdg_config,
                prev_opencode_config,
                prev_grok_home,
                prev_headroom_data_dir,
                prev_appdata,
                prev_localappdata,
                _env_lock: env_lock,
            }
        }

        fn path(&self) -> &Path {
            &self.home
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match self.prev_xdg.take() {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
            match self.prev_shell.take() {
                Some(v) => std::env::set_var("SHELL", v),
                None => std::env::remove_var("SHELL"),
            }
            match self.prev_codex.take() {
                Some(v) => std::env::set_var("CODEX_HOME", v),
                None => std::env::remove_var("CODEX_HOME"),
            }
            match self.prev_xdg_config.take() {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match self.prev_opencode_config.take() {
                Some(v) => std::env::set_var("OPENCODE_CONFIG", v),
                None => std::env::remove_var("OPENCODE_CONFIG"),
            }
            match self.prev_grok_home.take() {
                Some(v) => std::env::set_var("GROK_HOME", v),
                None => std::env::remove_var("GROK_HOME"),
            }
            match self.prev_zdotdir.take() {
                Some(v) => std::env::set_var("ZDOTDIR", v),
                None => std::env::remove_var("ZDOTDIR"),
            }
            match self.prev_headroom_data_dir.take() {
                Some(v) => std::env::set_var("HEADROOM_DATA_DIR", v),
                None => std::env::remove_var("HEADROOM_DATA_DIR"),
            }
            match self.prev_appdata.take() {
                Some(v) => std::env::set_var("APPDATA", v),
                None => std::env::remove_var("APPDATA"),
            }
            match self.prev_localappdata.take() {
                Some(v) => std::env::set_var("LOCALAPPDATA", v),
                None => std::env::remove_var("LOCALAPPDATA"),
            }
        }
    }

    /// RTK is opt-in: its PATH block and Claude Code hook are only wired when the
    /// managed binary exists on disk. Drop a fake one at the default location so
    /// tests covering a fully-configured environment exercise the RTK wiring.
    fn seed_installed_rtk() {
        let rtk = super::default_headroom_rtk_path();
        fs::create_dir_all(rtk.parent().unwrap()).unwrap();
        fs::write(&rtk, "#!/bin/sh\n").unwrap();
    }

    fn read_settings_json(path: &Path) -> serde_json::Value {
        let raw = fs::read_to_string(path).expect("read settings.json");
        serde_json::from_str(&raw).expect("parse settings.json")
    }

    fn assert_schema_backup_preserves(path: &Path, original: &[u8]) {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("schema test path has a UTF-8 file name");
        let prefix = format!("{file_name}.headroom-local-community-backup-");
        let backups: Vec<PathBuf> = fs::read_dir(path.parent().expect("schema test parent"))
            .expect("read schema test parent")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect();
        assert!(
            !backups.is_empty(),
            "schema rejection should leave a rollback backup next to {}",
            path.display()
        );
        assert!(
            backups.iter().any(|backup| fs::read(backup)
                .map(|bytes| bytes == original)
                .unwrap_or(false)),
            "at least one schema backup should preserve the original bytes"
        );
    }

    #[test]
    #[serial_test::serial]
    fn configure_claude_settings_env_rejects_non_object_without_overwrite() {
        let home = TestHome::new();
        let settings_path = home.path().join(".claude").join("settings.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        let original = br#"{"env":[],"keep":"user value"}"#;
        fs::write(&settings_path, original).unwrap();

        let err =
            super::configure_claude_settings_env("ANTHROPIC_BASE_URL", "http://127.0.0.1:6867")
                .expect_err("a non-object env container must be rejected");
        assert!(err.to_string().contains("env"), "{err:#}");
        assert_eq!(fs::read(&settings_path).unwrap(), original);
        assert_schema_backup_preserves(&settings_path, original);
    }

    #[test]
    #[serial_test::serial]
    fn configure_claude_settings_env_rejects_non_string_value_without_overwrite() {
        let home = TestHome::new();
        let settings_path = home.path().join(".claude").join("settings.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        let original =
            br#"{"env":{"ANTHROPIC_BASE_URL":["managed elsewhere"]},"keep":"user value"}"#;
        fs::write(&settings_path, original).unwrap();

        let err =
            super::configure_claude_settings_env("ANTHROPIC_BASE_URL", "http://127.0.0.1:6867")
                .expect_err("a non-string env value must be rejected");
        assert!(
            err.to_string().contains("env.ANTHROPIC_BASE_URL"),
            "{err:#}"
        );
        assert_eq!(fs::read(&settings_path).unwrap(), original);
        assert_schema_backup_preserves(&settings_path, original);
    }

    /// A base URL on any of Headroom's own loopback ports is ours, whichever
    /// way it got written. The reconciler really did leave the backend port in
    /// this file, and a teardown that only recognised 6867 kept the dead
    /// route alive for the user (2026-09-10).
    #[test]
    #[serial_test::serial]
    fn headroom_managed_base_urls_cover_every_port_we_serve() {
        for value in [
            "http://127.0.0.1:6867",
            "http://127.0.0.1:6868/",
            "http://127.0.0.1:6869",
            "http://127.0.0.1:6890",
            "http://127.0.0.1:6891/backend-api/codex",
            "http://localhost:6868",
            "  http://127.0.0.1:6868  ",
        ] {
            assert!(super::is_headroom_managed_base_url(value), "{value}");
        }
        for value in [
            // A gateway the user configured, including one that merely looks
            // like ours, must never be treated as Headroom's own.
            "https://api.anthropic.com",
            "http://127.0.0.1:6866",
            "http://127.0.0.1:6892",
            "http://127.0.0.1",
            "http://127.0.0.1:not-a-port",
            "http://192.168.1.9:6868",
            "",
        ] {
            assert!(!super::is_headroom_managed_base_url(value), "{value}");
        }
    }

    #[test]
    #[serial_test::serial]
    fn teardown_clears_every_headroom_port_and_keeps_foreign_urls() {
        let home = TestHome::new();
        let settings_path = home.path().join(".claude").join("settings.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();

        // The reconciler's residue: teardown used to leave this behind.
        fs::write(
            &settings_path,
            br#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:6868","KEEP":"x"}}"#,
        )
        .unwrap();
        super::remove_claude_settings_env("ANTHROPIC_BASE_URL", "http://127.0.0.1:6867", None)
            .expect("teardown");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(value["env"]["KEEP"], "x", "unrelated env must survive");
        assert!(
            value["env"].get("ANTHROPIC_BASE_URL").is_none(),
            "our own dead port must be removed: {value}"
        );

        // A foreign gateway is not ours to delete...
        fs::write(
            &settings_path,
            br#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.example/anthropic"}}"#,
        )
        .unwrap();
        super::remove_claude_settings_env("ANTHROPIC_BASE_URL", "http://127.0.0.1:6867", None)
            .expect("teardown");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(
            value["env"]["ANTHROPIC_BASE_URL"], "https://gateway.example/anthropic",
            "a user's gateway must survive teardown"
        );

        // ...and when setup captured one before taking over, it comes back.
        fs::write(
            &settings_path,
            br#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:6868"}}"#,
        )
        .unwrap();
        super::remove_claude_settings_env(
            "ANTHROPIC_BASE_URL",
            "http://127.0.0.1:6867",
            Some("https://gateway.example/anthropic"),
        )
        .expect("teardown");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(
            value["env"]["ANTHROPIC_BASE_URL"],
            "https://gateway.example/anthropic"
        );
    }

    #[test]
    #[serial_test::serial]
    fn ensure_claude_settings_hook_rejects_non_object_hooks_without_overwrite() {
        let home = TestHome::new();
        let settings_path = home.path().join(".claude").join("settings.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        let original = br#"{"hooks":[],"keep":"user value"}"#;
        fs::write(&settings_path, original).unwrap();
        let hook_path = home.path().join("headroom-hook.sh");

        let err = super::ensure_claude_settings_hook(&hook_path, "Bash", "headroom-hook")
            .expect_err("a non-object hooks container must be rejected");
        assert!(err.to_string().contains("hooks"), "{err:#}");
        assert_eq!(fs::read(&settings_path).unwrap(), original);
        assert_schema_backup_preserves(&settings_path, original);
    }

    #[test]
    #[serial_test::serial]
    fn ensure_claude_settings_hook_rejects_non_array_event_without_overwrite() {
        let home = TestHome::new();
        let settings_path = home.path().join(".claude").join("settings.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        let original = br#"{"hooks":{"PreToolUse":{}},"keep":"user value"}"#;
        fs::write(&settings_path, original).unwrap();
        let hook_path = home.path().join("headroom-hook.sh");

        let err = super::ensure_claude_settings_hook(&hook_path, "Bash", "headroom-hook")
            .expect_err("a non-array PreToolUse event must be rejected");
        assert!(err.to_string().contains("hooks.PreToolUse"), "{err:#}");
        assert_eq!(fs::read(&settings_path).unwrap(), original);
        assert_schema_backup_preserves(&settings_path, original);
    }

    #[test]
    fn register_guard_hook_entries_rejects_non_array_event_without_overwrite() {
        let root = unique_temp_dir("headroom-schema-event");
        fs::create_dir_all(&root).unwrap();
        let hooks_path = root.join("hooks.json");
        let original = br#"{"hooks":{"AfterToolUse":{}},"keep":"user value"}"#;
        fs::write(&hooks_path, original).unwrap();

        let err = super::register_guard_hook_entries(
            &hooks_path,
            "/tmp/headroom-guard.sh",
            "Headroom guard",
            &[("AfterToolUse", None)],
        )
        .expect_err("a non-array event must be rejected");
        assert!(err.to_string().contains("hooks.AfterToolUse"), "{err:#}");
        assert_eq!(fs::read(&hooks_path).unwrap(), original);
        assert_schema_backup_preserves(&hooks_path, original);

        let _ = fs::remove_dir_all(root);
    }

    /// RUST-5X: a shell profile with non-UTF-8 bytes (latin-1 comment) made
    /// `read_to_string` fail and took the whole client setup down with it, so
    /// Claude Code never got routed. The profile is convenience; core routing
    /// via ~/.claude/settings.json must still land, and the user's bytes must
    /// survive untouched.
    #[test]
    #[serial_test::serial]
    fn apply_client_setup_survives_non_utf8_shell_profile() {
        let home = TestHome::new();
        // 0xFF is never valid UTF-8.
        let latin1 = b"# caf\xe9 alias\nalias ll='ls -l'\n\xff\n";
        let zshrc = home.path().join(".zshrc");
        fs::write(&zshrc, latin1).unwrap();
        fs::write(home.path().join(".zshenv"), latin1).unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude").join("settings.json"),
            r#"{"hooks": {}}"#,
        )
        .unwrap();
        seed_installed_rtk();

        let result =
            super::apply_client_setup("claude_code").expect("setup succeeds despite bad profile");
        assert!(result.applied);
        assert!(
            result.shell_profile_unwritable,
            "shell step reported as skipped"
        );
        assert_eq!(
            fs::read(&zshrc).unwrap(),
            latin1,
            "user's non-UTF-8 profile left byte-identical"
        );

        // The part that actually routes Claude Code still happened.
        let settings = read_settings_json(&home.path().join(".claude").join("settings.json"));
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("http://127.0.0.1:6867")
        );
        // Verification reads the same profiles and must not blow up either.
        super::verify_client_setup("claude_code").expect("verification tolerates bad profile");
    }

    #[test]
    #[serial_test::serial]
    fn community_refuses_to_overwrite_official_headroom_routing() {
        let home = TestHome::new();
        let zshrc = home.path().join(".zshrc");
        let original = "# >>> headroom:claude_code >>>\nexport ANTHROPIC_BASE_URL=http://127.0.0.1:6767\n# <<< headroom:claude_code <<<\n";
        fs::write(&zshrc, original).unwrap();

        let err = super::apply_client_setup("claude_code").unwrap_err();
        assert!(
            err.to_string()
                .contains("Official Headroom routing is active"),
            "{err:#}"
        );
        assert_eq!(fs::read_to_string(&zshrc).unwrap(), original);
        assert!(!fs::read_to_string(&zshrc)
            .unwrap()
            .contains("headroom-local-community:claude_code"));
    }

    #[test]
    #[serial_test::serial]
    fn apply_then_verify_claude_code_writes_expected_files() {
        let home = TestHome::new();
        // Seed an empty zshrc/zshenv so the shell-block writers have files to
        // edit and don't depend on the dev's real shell config layout.
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude").join("settings.json"),
            r#"{"hooks": {}}"#,
        )
        .unwrap();
        seed_installed_rtk();

        let result = super::apply_client_setup("claude_code").expect("apply_client_setup succeeds");
        assert!(result.applied);
        assert_eq!(result.client_id, "claude_code");

        // Hook script and settings.json hook entry must be present.
        let hook_path = home
            .path()
            .join(".claude")
            .join("hooks")
            .join("headroom-local-community-rtk-rewrite.sh");
        assert!(hook_path.exists(), "hook script written to disk");
        let hook_contents = fs::read_to_string(&hook_path).unwrap();
        assert!(
            hook_contents.starts_with("#!/usr/bin/env bash"),
            "hook has expected shebang"
        );

        let settings = read_settings_json(&home.path().join(".claude").join("settings.json"));
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("http://127.0.0.1:6867"),
            "claude settings.json points env at headroom proxy"
        );
        let pre_tool_use = &settings["hooks"]["PreToolUse"];
        assert!(
            pre_tool_use.is_array() && !pre_tool_use.as_array().unwrap().is_empty(),
            "PreToolUse hook entry exists, got: {settings}"
        );

        // Shell block in zshenv (or whichever profile the writer chose) should
        // export ANTHROPIC_BASE_URL pointing at the loopback proxy.
        let zshrc = fs::read_to_string(home.path().join(".zshrc")).unwrap();
        let zshenv = fs::read_to_string(home.path().join(".zshenv")).unwrap();
        let combined = format!("{zshrc}\n{zshenv}");
        assert!(
            combined.contains("ANTHROPIC_BASE_URL=http://127.0.0.1:6867"),
            "ANTHROPIC_BASE_URL exported from a managed shell block, got:\n{combined}"
        );

        // verify_client_setup should report all the configured checks.
        // Proxy reachability is reported via `proxy_reachable` only, so a
        // missing proxy in the test environment no longer flips `verified`.
        let verification =
            super::verify_client_setup("claude_code").expect("verify_client_setup succeeds");
        assert_eq!(verification.client_id, "claude_code");
        assert!(
            verification
                .checks
                .iter()
                .any(|c| c.contains("ANTHROPIC_BASE_URL")),
            "verification reports the env check, got: {:?}",
            verification.checks
        );
        assert!(
            verification
                .checks
                .iter()
                .any(|c| c.contains("RTK Claude hook")),
            "verification reports the hook check, got: {:?}",
            verification.checks
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_claude_writes_guard_and_disable_preserves_env_and_user_hooks() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        // Pre-existing user-authored hook that must survive apply and disable.
        fs::write(
            home.path().join(".claude").join("settings.json"),
            r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#,
        )
        .unwrap();
        seed_installed_rtk();

        super::apply_client_setup("claude_code").expect("first apply");
        super::apply_client_setup("claude_code").expect("second apply");

        let script = home
            .path()
            .join(".claude")
            .join("hooks")
            .join("headroom-local-community-claude-guard.py");
        assert!(script.exists(), "guard script written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&script).unwrap().permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "guard script is executable, got {mode:o}"
            );
        }

        let settings_path = home.path().join(".claude").join("settings.json");
        let settings = read_settings_json(&settings_path);
        // The registered command is platform-dependent (/usr/bin/python3 vs the
        // quoted managed python.exe), so assert against the real builder.
        let command = super::claude_guard_command();
        let guard_count = |event: &str| {
            settings["hooks"][event]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| {
                    entry["hooks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|h| h["command"] == serde_json::Value::String(command.clone()))
                })
                .count()
        };
        // Guard registered exactly once on SessionStart despite the double-apply.
        assert_eq!(
            guard_count("SessionStart"),
            1,
            "guard registered once for SessionStart, got:\n{settings:#}"
        );
        // Never on UserPromptSubmit: exit 2 there blocks every prompt in Claude
        // Desktop / Cowork VM sessions that can't reach the app.
        assert_eq!(
            guard_count("UserPromptSubmit"),
            0,
            "guard must not register on UserPromptSubmit, got:\n{settings:#}"
        );
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["matcher"],
            "startup|resume|clear|compact"
        );

        super::disable_client_setup("claude_code").expect("disable");

        assert!(!script.exists(), "guard script removed on disable");
        let after = read_settings_json(&settings_path);
        let after_str = serde_json::to_string(&after).unwrap();
        assert!(
            !after_str.contains("headroom-local-community-claude-guard.py"),
            "guard stripped from settings.json, got:\n{after:#}"
        );
        assert!(
            after_str.contains("echo mine"),
            "user-authored hook preserved, got:\n{after:#}"
        );
        // settings.json must NOT be deleted even if it were otherwise empty.
        assert!(settings_path.exists(), "settings.json preserved on disable");
    }

    #[test]
    #[serial_test::serial]
    fn apply_migrates_guard_off_user_prompt_submit() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        // An older build registered the guard on UserPromptSubmit, where exit 2
        // blocks every prompt in Claude Desktop / Cowork VM sessions. A user
        // hook on the same event must survive the migration.
        let script = home
            .path()
            .join(".claude")
            .join("hooks")
            .join("headroom-local-community-claude-guard.py");
        // Build via serde_json so the script path is JSON-escaped: raw format!
        // interpolation of a Windows path writes lone backslashes that json5
        // parsing silently eats, leaving a command the strip can never match.
        let old_command = format!("/usr/bin/python3 {}", script.display());
        let seeded = serde_json::json!({"hooks":{
            "SessionStart":[{"matcher":"startup|resume|clear|compact","hooks":[{"type":"command","command": old_command.as_str()}]}],
            "UserPromptSubmit":[
                {"hooks":[{"type":"command","command": old_command.as_str()}]},
                {"hooks":[{"type":"command","command":"echo mine"}]}
            ]
        }});
        fs::write(
            home.path().join(".claude").join("settings.json"),
            serde_json::to_string(&seeded).unwrap(),
        )
        .unwrap();
        seed_installed_rtk();

        super::apply_client_setup("claude_code").expect("apply");

        let settings = read_settings_json(&home.path().join(".claude").join("settings.json"));
        let ups = serde_json::to_string(&settings["hooks"]["UserPromptSubmit"]).unwrap();
        assert!(
            !ups.contains("headroom-local-community-claude-guard.py"),
            "guard stripped from UserPromptSubmit, got:\n{settings:#}"
        );
        assert!(
            ups.contains("echo mine"),
            "user-authored UserPromptSubmit hook preserved, got:\n{settings:#}"
        );
        let ss = serde_json::to_string(&settings["hooks"]["SessionStart"]).unwrap();
        assert!(
            ss.contains("headroom-local-community-claude-guard.py"),
            "guard still registered on SessionStart, got:\n{settings:#}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn revert_external_mutations_spares_user_data_but_full_cleanup_removes_it() {
        // The Homebrew cask calls `--uninstall` (-> revert_external_mutations)
        // from its `uninstall` stanza, which runs on every `brew uninstall`.
        // Homebrew reserves user-data deletion for the opt-in `zap`, so the
        // narrow function must undo our edits to OTHER tools while leaving
        // Headroom's own directories intact. perform_full_cleanup (the in-app
        // "uninstall and quit") must still remove both.
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude").join("settings.json"),
            r#"{"hooks": {}}"#,
        )
        .unwrap();
        seed_installed_rtk();
        super::apply_client_setup("claude_code").expect("apply");

        // User data: Headroom's own directories.
        let app_dir = super::app_data_dir();
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("memory.db"), b"user data").unwrap();
        let community_workspace = home.path().join(".headroom-local-community");
        fs::create_dir_all(&community_workspace).unwrap();
        fs::write(community_workspace.join("keep.json"), b"user data").unwrap();
        let paid_workspace = home.path().join(".headroom");
        fs::create_dir_all(&paid_workspace).unwrap();
        fs::write(paid_workspace.join("paid.json"), b"paid app data").unwrap();

        // An external mutation and a stray backup file, both of which the
        // narrow function is responsible for.
        let settings_path = home.path().join(".claude").join("settings.json");
        let stray_backup = home
            .path()
            .join(".zshrc.headroom-local-community-backup-20260101000000");
        fs::write(&stray_backup, "# old\n").unwrap();
        assert_eq!(
            read_settings_json(&settings_path)["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("http://127.0.0.1:6867"),
            "precondition: base url wired"
        );

        super::revert_external_mutations();

        assert!(
            read_settings_json(&settings_path)["env"]["ANTHROPIC_BASE_URL"].is_null(),
            "revert should strip the routing env"
        );
        assert!(
            !stray_backup.exists(),
            "revert should sweep stray backup files"
        );
        assert!(
            app_dir.join("memory.db").exists(),
            "revert must NOT delete Headroom's app data — that belongs to `brew zap`"
        );
        assert!(
            community_workspace.join("keep.json").exists(),
            "revert must preserve Community user data until full cleanup"
        );

        super::perform_full_cleanup();

        assert!(
            !app_dir.exists(),
            "full cleanup should remove Headroom's app data"
        );
        assert!(
            !community_workspace.exists(),
            "full cleanup should remove only the Community workspace"
        );
        assert!(
            paid_workspace.join("paid.json").exists(),
            "Community cleanup must never remove the paid app's ~/.headroom workspace"
        );
    }

    #[test]
    #[serial_test::serial]
    fn full_cleanup_spares_all_shared_hf_models() {
        // Community and paid editions may use the same HuggingFace cache. No
        // author prefix is sufficient proof that a snapshot belongs exclusively
        // to Community, so uninstall leaves every model and lock directory alone.
        let home = TestHome::new();
        let hub = home.path().join(".cache").join("huggingface").join("hub");

        // Author-prefixed models may also be used by the paid app.
        let ours = [
            "models--chopratejas--kompress-v2-base",
            "models--chopratejas--technique-router-onnx",
            "models--chopratejas--siglip-image-encoder-onnx",
        ];
        // Generic models we also pull, but which another tool may share. Removing
        // these would break that tool's cache, so they must survive.
        let shared = [
            "models--answerdotai--ModernBERT-base",
            "models--sentence-transformers--all-MiniLM-L6-v2",
            "models--Qdrant--all-MiniLM-L6-v2-onnx",
        ];

        for name in ours.iter().chain(shared.iter()) {
            for parent in [hub.join(name), hub.join(".locks").join(name)] {
                fs::create_dir_all(&parent).unwrap();
                fs::write(parent.join("blob"), b"weights").unwrap();
            }
        }

        super::perform_full_cleanup();

        for name in ours.iter().chain(shared.iter()) {
            assert!(
                hub.join(name).join("blob").exists(),
                "{name} may be shared and must survive Community uninstall"
            );
            assert!(
                hub.join(".locks").join(name).exists(),
                "{name} lock dir is shared and must survive"
            );
        }
        // The cache root itself is never ours to delete.
        assert!(hub.exists(), "hub cache root preserved");
    }

    #[test]
    #[serial_test::serial]
    fn full_cleanup_strips_base_url_and_guard_when_shell_block_removal_fails() {
        // Regression: perform_full_cleanup used to remove ANTHROPIC_BASE_URL and
        // the guard hook ONLY via clear_client_setups -> disable_client_setup,
        // where remove_shell_block runs first under `?`. A failure there left the
        // routing env and the guard hook in place, both of which brick Claude
        // once the proxy is gone. Force that failure and confirm cleanup still
        // strips them.
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude").join("settings.json"),
            r#"{"hooks": {}}"#,
        )
        .unwrap();
        seed_installed_rtk();

        super::apply_client_setup("claude_code").expect("apply");

        let settings_path = home.path().join(".claude").join("settings.json");
        let guard_script = home
            .path()
            .join(".claude")
            .join("hooks")
            .join("headroom-local-community-claude-guard.py");
        assert_eq!(
            read_settings_json(&settings_path)["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("http://127.0.0.1:6867"),
            "precondition: base url wired"
        );
        assert!(guard_script.exists(), "precondition: guard script written");

        // Sabotage a shell target: a directory where remove_managed_block expects
        // a file makes read_to_string fail, so disable_client_setup("claude_code")
        // bails before it reaches base-url / guard removal.
        let zshrc = home.path().join(".zshrc");
        fs::remove_file(&zshrc).unwrap();
        fs::create_dir(&zshrc).unwrap();

        super::perform_full_cleanup();

        assert!(settings_path.exists(), "settings.json preserved");
        let after = read_settings_json(&settings_path);
        assert!(
            after["env"]["ANTHROPIC_BASE_URL"].is_null(),
            "base url stripped despite shell-block failure, got:\n{after:#}"
        );
        assert!(
            !serde_json::to_string(&after["hooks"])
                .unwrap()
                .contains("headroom-local-community-claude-guard.py"),
            "guard hook stripped despite shell-block failure, got:\n{after:#}"
        );
        assert!(!guard_script.exists(), "guard script deleted");
    }

    #[test]
    #[serial_test::serial]
    fn apply_preserves_and_disable_restores_custom_base_url() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        // A corporate gateway already routes Claude Code before Headroom.
        let gateway = "https://gateway.corp.example/anthropic";
        fs::write(
            home.path().join(".claude").join("settings.json"),
            format!(r#"{{"env":{{"ANTHROPIC_BASE_URL":"{gateway}"}}}}"#),
        )
        .unwrap();
        seed_installed_rtk();

        let result = super::apply_client_setup("claude_code").expect("apply");
        // Setup captured the gateway and told the caller it took over routing.
        assert_eq!(result.replaced_base_url.as_deref(), Some(gateway));
        let settings_path = home.path().join(".claude").join("settings.json");
        let after_apply = read_settings_json(&settings_path);
        assert_eq!(
            after_apply["env"]["ANTHROPIC_BASE_URL"],
            serde_json::Value::String(super::HEADROOM_ANTHROPIC_BASE_URL.to_string())
        );
        assert_eq!(
            super::load_setup_state().preserved_base_urls["claude_code"],
            gateway
        );

        super::disable_client_setup("claude_code").expect("disable");
        // The gateway URL is restored, not deleted.
        let after_disable = read_settings_json(&settings_path);
        assert_eq!(
            after_disable["env"]["ANTHROPIC_BASE_URL"],
            serde_json::Value::String(gateway.to_string()),
            "custom base URL restored on disable, got:\n{after_disable:#}"
        );
        assert!(
            !super::load_setup_state()
                .preserved_base_urls
                .contains_key("claude_code"),
            "preserved entry consumed after restore"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_without_custom_base_url_does_not_report_takeover() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        seed_installed_rtk();

        let result = super::apply_client_setup("claude_code").expect("apply");
        assert!(result.replaced_base_url.is_none());
        assert!(super::load_setup_state().preserved_base_urls.is_empty());

        // Disable deletes the key (nothing to restore).
        super::disable_client_setup("claude_code").expect("disable");
        let settings_path = home.path().join(".claude").join("settings.json");
        if settings_path.exists() {
            let after = read_settings_json(&settings_path);
            assert!(after["env"]["ANTHROPIC_BASE_URL"].is_null());
        }
    }

    #[test]
    #[serial_test::serial]
    fn verify_claude_code_passes_when_rtk_deliberately_disabled() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude").join("settings.json"),
            r#"{"hooks": {}}"#,
        )
        .unwrap();

        super::apply_client_setup("claude_code").expect("apply_client_setup succeeds");

        // User turns RTK off: this strips the RTK PATH block + hook but leaves
        // ANTHROPIC_BASE_URL routing intact, and persists the opt-out.
        super::set_rtk_enabled(false, home.path(), home.path()).expect("disable RTK");

        let hook_path = home
            .path()
            .join(".claude")
            .join("hooks")
            .join("headroom-local-community-rtk-rewrite.sh");
        assert!(!hook_path.exists(), "RTK hook removed when RTK disabled");

        // Routing config is still present, so Claude Code must verify green
        // even though the RTK pieces are gone.
        let verification =
            super::verify_client_setup("claude_code").expect("verify_client_setup succeeds");
        assert!(
            verification.verified,
            "claude_code verifies on routing alone when RTK is disabled, failures: {:?}",
            verification.failures
        );
        assert!(
            verification.failures.iter().all(|f| !f.contains("RTK")),
            "no RTK failures reported when RTK is disabled, got: {:?}",
            verification.failures
        );
    }

    #[test]
    #[serial_test::serial]
    fn verify_claude_code_passes_when_rtk_not_installed() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude").join("settings.json"),
            r#"{"hooks": {}}"#,
        )
        .unwrap();

        // Clean install with RTK auto-install removed: routing is configured but
        // the managed RTK binary was never dropped on disk and the user never
        // toggled RTK off (rtk_disabled stays false). Claude Code must still
        // verify green on routing alone.
        super::apply_client_setup("claude_code").expect("apply_client_setup succeeds");

        assert!(
            !super::default_headroom_rtk_path().exists(),
            "RTK binary must be absent for this test"
        );
        let state = super::load_setup_state();
        assert!(
            !state.rtk_disabled,
            "rtk_disabled stays false when untoggled"
        );

        let verification =
            super::verify_client_setup("claude_code").expect("verify_client_setup succeeds");
        assert!(
            verification.verified,
            "claude_code verifies on routing alone when RTK isn't installed, failures: {:?}",
            verification.failures
        );
        assert!(
            verification.failures.iter().all(|f| !f.contains("RTK")),
            "no RTK failures reported when RTK isn't installed, got: {:?}",
            verification.failures
        );
    }

    #[test]
    #[serial_test::serial]
    fn ensure_rtk_integrations_writes_codex_nudge_and_disable_removes_it() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(home.path().join(".claude").join("settings.json"), "{}").unwrap();

        // Mark Codex as a configured client so the AGENTS.md nudge path runs.
        let mut state = super::load_setup_state();
        state
            .configured_clients
            .insert("codex_cli".into(), "now".into());
        super::write_setup_state(&mut state).unwrap();

        // Fake managed rtk + python binaries so the binary-present guard passes.
        let bin_dir = home.path().join("managed-bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let rtk = bin_dir.join("rtk");
        fs::write(&rtk, "#!/bin/sh\n").unwrap();
        let python = bin_dir.join("python3");
        fs::write(&python, "#!/bin/sh\n").unwrap();

        super::ensure_rtk_integrations(&rtk, &python).expect("ensure_rtk_integrations");

        let agents = home.path().join(".codex").join("AGENTS.md");
        let body = fs::read_to_string(&agents).expect("AGENTS.md written");
        assert!(
            body.contains("Headroom RTK"),
            "nudge heading present: {body}"
        );
        assert!(
            body.contains(&rtk.display().to_string()),
            "nudge references the managed rtk path: {body}"
        );

        // Disabling RTK must remove the managed block.
        super::set_rtk_enabled(false, &rtk, &python).expect("disable rtk");
        let after = fs::read_to_string(&agents).unwrap_or_default();
        assert!(
            !after.contains("Headroom RTK"),
            "nudge removed on disable: {after}"
        );
    }

    #[test]
    fn instruction_templates_preserve_raw_and_visual_workflows() {
        let binary = Path::new("/tmp/Headroom Tools/rtk");
        let rtk = super::build_rtk_codex_nudge(binary);
        assert!(rtk.contains("\"/tmp/Headroom Tools/rtk\""), "quote executable paths");
        assert!(rtk.contains("git diff --check"));
        assert!(rtk.contains("raw"));
        for body in [
            super::build_markitdown_codex_nudge(binary),
            super::build_markitdown_office_nudge(binary),
        ] {
            assert!(body.contains("layout"), "text extraction must not replace layout verification");
            assert!(body.contains("available"), "do not assume the converter is available");
        }
    }

    #[test]
    #[serial_test::serial]
    fn instruction_templates_round_trip_without_changing_user_rules() {
        let home = TestHome::new();
        let binary = Path::new("/tmp/Headroom Tools/tool");
        let templates = [
            ("rtk", super::build_rtk_codex_nudge(binary)),
            ("markitdown", super::build_markitdown_codex_nudge(binary)),
            ("markitdown_office", super::build_markitdown_office_nudge(binary)),
            ("serena", super::build_serena_usage_nudge().to_string()),
        ];
        for (id, body) in templates {
            let path = home.path().join(format!("{id}.md"));
            let original = "# User rules\n\n保留用户规则。\n";
            fs::write(&path, original).unwrap();
            let (changed, backup) = super::upsert_managed_block(&path, id, &body).unwrap();
            assert!(changed);
            assert_eq!(fs::read_to_string(backup.unwrap()).unwrap(), original);
            let installed = fs::read(&path).unwrap();
            let repeated = super::upsert_managed_block(&path, id, &body).unwrap();
            assert!(!repeated.0);
            assert!(repeated.1.is_none());
            assert_eq!(fs::read(&path).unwrap(), installed);
            assert!(super::remove_managed_block(&path, id).unwrap());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            assert!(!super::remove_managed_block(&path, id).unwrap());
        }
    }

    #[test]
    #[serial_test::serial]
    fn serena_integration_preserves_user_content_and_is_byte_idempotent() {
        let home = TestHome::new();
        let claude_md = home.path().join(".claude").join("CLAUDE.md");
        let codex_agents = home.path().join(".codex").join("AGENTS.md");
        fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
        fs::create_dir_all(codex_agents.parent().unwrap()).unwrap();
        fs::write(
            home.path().join(".codex").join("config.toml"),
            "# Codex user configuration\n",
        )
        .unwrap();

        let claude_original = "# Claude user instructions\n\n\
# >>> headroom-local-community:markitdown_office >>>\n\
keep markitdown\n\
# <<< headroom-local-community:markitdown_office <<<\n";
        let codex_original = "# Codex user instructions\n\n\
# >>> headroom-local-community:rtk >>>\n\
keep rtk\n\
# <<< headroom-local-community:rtk <<<\n";
        fs::write(&claude_md, claude_original).unwrap();
        fs::write(&codex_agents, codex_original).unwrap();

        assert!(!super::is_claude_code_enabled());
        assert!(!super::is_codex_enabled());

        let (changed, backups) = super::enable_serena_integration().expect("enable Serena");
        assert_eq!(changed.len(), 2, "both detected clients updated");
        assert_eq!(backups.len(), 2, "both pre-existing files backed up");

        let claude_after_first = fs::read(&claude_md).unwrap();
        let codex_after_first = fs::read(&codex_agents).unwrap();
        for body in [&claude_after_first, &codex_after_first] {
            let body = String::from_utf8_lossy(body);
            assert_eq!(
                body.matches("# >>> headroom-local-community:serena >>>")
                    .count(),
                1,
                "one Serena managed block: {body}"
            );
            assert!(body.contains("Semantic code tools (Headroom Serena)"));
            assert!(body.contains("`initial_instructions`"));
            assert!(body.contains("`activate_project`"));
            assert!(body.contains("do not run onboarding"));
        }
        assert!(String::from_utf8_lossy(&claude_after_first).contains(claude_original.trim_end()));
        assert!(String::from_utf8_lossy(&codex_after_first).contains(codex_original.trim_end()));

        let second = super::enable_serena_integration().expect("enable Serena again");
        assert!(second.0.is_empty(), "second enable changes no files");
        assert!(second.1.is_empty(), "second enable creates no backups");
        assert_eq!(fs::read(&claude_md).unwrap(), claude_after_first);
        assert_eq!(fs::read(&codex_agents).unwrap(), codex_after_first);

        assert!(super::disable_serena_integration().expect("disable Serena"));
        assert_eq!(fs::read_to_string(&claude_md).unwrap(), claude_original);
        assert_eq!(fs::read_to_string(&codex_agents).unwrap(), codex_original);

        assert!(
            !super::disable_serena_integration().expect("disable Serena again"),
            "second disable is a no-op"
        );
        assert_eq!(fs::read_to_string(&claude_md).unwrap(), claude_original);
        assert_eq!(fs::read_to_string(&codex_agents).unwrap(), codex_original);
    }

    #[test]
    #[serial_test::serial]
    fn serena_integration_rolls_back_first_prompt_when_second_target_fails() {
        let home = TestHome::new();
        let claude_md = home.path().join(".claude").join("CLAUDE.md");
        let codex_agents = home.path().join(".codex").join("AGENTS.md");
        fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
        fs::create_dir_all(&codex_agents).unwrap();
        fs::write(
            home.path().join(".codex").join("config.toml"),
            "# Codex user configuration\n",
        )
        .unwrap();
        let original = "# Claude user instructions\n";
        fs::write(&claude_md, original).unwrap();

        let err = super::enable_serena_integration()
            .expect_err("Codex AGENTS.md directory must reject the write");
        assert!(
            err.to_string().contains("rolling back partial Serena"),
            "cleanup failure remains visible: {err:#}"
        );
        assert_eq!(
            fs::read_to_string(&claude_md).unwrap(),
            original,
            "Claude prompt written before the Codex failure was rolled back"
        );
    }

    #[test]
    #[serial_test::serial]
    fn serena_integration_ignores_bare_codex_directory() {
        let home = TestHome::new();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let codex_agents = codex_dir.join("AGENTS.md");

        assert!(
            !super::serena_codex_target_exists(),
            "a bare .codex directory is not evidence of a Codex installation"
        );
        super::enable_serena_integration().expect("enable Serena");
        assert!(
            !codex_agents.exists(),
            "Serena must not create ~/.codex/AGENTS.md for a bare directory"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_claude_code_is_byte_idempotent() {
        // Regression: a second apply used to add blank-line padding between
        // managed blocks, so byte-exact idempotency now holds and is
        // asserted here.
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        seed_installed_rtk();

        super::apply_client_setup("claude_code").expect("first apply");
        let zshrc_after_first = fs::read_to_string(home.path().join(".zshrc")).unwrap();
        let zshenv_after_first = fs::read_to_string(home.path().join(".zshenv")).unwrap();
        let settings_after_first =
            fs::read_to_string(home.path().join(".claude").join("settings.json")).unwrap();
        let hook_after_first = fs::read_to_string(
            home.path()
                .join(".claude")
                .join("hooks")
                .join("headroom-local-community-rtk-rewrite.sh"),
        )
        .unwrap();

        super::apply_client_setup("claude_code").expect("second apply");
        let zshrc_after_second = fs::read_to_string(home.path().join(".zshrc")).unwrap();
        let zshenv_after_second = fs::read_to_string(home.path().join(".zshenv")).unwrap();
        let settings_after_second =
            fs::read_to_string(home.path().join(".claude").join("settings.json")).unwrap();
        let hook_after_second = fs::read_to_string(
            home.path()
                .join(".claude")
                .join("hooks")
                .join("headroom-local-community-rtk-rewrite.sh"),
        )
        .unwrap();

        assert_eq!(zshrc_after_first, zshrc_after_second, "zshrc byte-stable");
        assert_eq!(
            zshenv_after_first, zshenv_after_second,
            "zshenv byte-stable"
        );
        assert_eq!(
            settings_after_first, settings_after_second,
            "settings.json byte-stable"
        );
        assert_eq!(
            hook_after_first, hook_after_second,
            "hook script byte-stable"
        );

        // Sanity: each managed block still appears exactly once.
        let combined = format!("{zshrc_after_second}\n{zshenv_after_second}");
        assert_eq!(
            combined
                .matches("# >>> headroom-local-community:claude_code >>>")
                .count(),
            1
        );
        assert_eq!(
            combined
                .matches("# >>> headroom-local-community:managed_rtk >>>")
                .count(),
            1
        );
    }

    #[test]
    #[serial_test::serial]
    fn disable_then_clear_claude_code_removes_traces() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        seed_installed_rtk();

        super::apply_client_setup("claude_code").expect("apply");
        let hook_path = home
            .path()
            .join(".claude")
            .join("hooks")
            .join("headroom-local-community-rtk-rewrite.sh");
        assert!(hook_path.exists(), "hook present after apply");

        super::disable_client_setup("claude_code").expect("disable");

        // Hook script removed.
        assert!(!hook_path.exists(), "hook removed after disable");

        // Shell blocks removed.
        let zshrc = fs::read_to_string(home.path().join(".zshrc")).unwrap();
        let zshenv = fs::read_to_string(home.path().join(".zshenv")).unwrap();
        let combined = format!("{zshrc}\n{zshenv}");
        assert!(
            !combined.contains("ANTHROPIC_BASE_URL=http://127.0.0.1:6867"),
            "ANTHROPIC_BASE_URL export removed, got:\n{combined}"
        );

        // settings.json no longer points env at the proxy and no longer carries
        // the Headroom hook entry.
        let settings = read_settings_json(&home.path().join(".claude").join("settings.json"));
        assert!(
            settings["env"]["ANTHROPIC_BASE_URL"].is_null(),
            "ANTHROPIC_BASE_URL stripped from settings.json env, got: {settings}"
        );
        let still_has_headroom_hook =
            claude_hook_present_in_value(&settings, "headroom-local-community-rtk-rewrite.sh");
        assert!(
            !still_has_headroom_hook,
            "Headroom hook entry stripped from settings.json, got: {settings}"
        );

        // clear_client_setups runs disable across all clients without error,
        // and the setup state file is left without a `claude_code` entry.
        super::clear_client_setups().expect("clear");
        let post = super::load_setup_state();
        assert!(
            post.configured_clients.get("claude_code").is_none(),
            "claude_code dropped from configured_clients, got: {:?}",
            post.configured_clients
        );
    }

    #[test]
    #[serial_test::serial]
    fn clear_client_setups_twice_preserves_remembered_snapshot() {
        // Regression: pause (first clear) moves configured -> remembered; the
        // quit-time second clear used to wipe remembered_clients because the
        // re-save was skipped while configured was empty — so a pause
        // followed by Cmd-Q permanently lost every connector.
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        seed_installed_rtk();

        super::apply_client_setup("claude_code").expect("apply");

        super::clear_client_setups().expect("first clear (pause)");
        let state = super::load_setup_state();
        assert!(state.configured_clients.is_empty());
        assert!(
            state.remembered_clients.contains_key("claude_code"),
            "pause snapshots the configured client, got: {:?}",
            state.remembered_clients
        );

        super::clear_client_setups().expect("second clear (quit)");
        let state = super::load_setup_state();
        assert!(
            state.remembered_clients.contains_key("claude_code"),
            "quit-time clear after a pause must keep the restore snapshot, got: {:?}",
            state.remembered_clients
        );
    }

    #[test]
    #[serial_test::serial]
    fn preserving_clear_keeps_codex_router_and_snapshots_other_clients() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        seed_installed_rtk();

        super::apply_client_setup("codex").expect("apply Codex");
        super::apply_client_setup("claude_code").expect("apply Claude");
        let config_path = home.path().join(".codex").join("config.toml");

        super::clear_client_setups_preserving_codex().expect("preserving clear");

        let config = fs::read_to_string(&config_path).expect("Codex config retained");
        assert!(
            config.contains(&format!(
                "base_url = \"{}\"",
                super::HEADROOM_OPENAI_BASE_URL
            )),
            "Codex remains on the stable router, got:\n{config}"
        );
        let state = super::load_setup_state();
        assert!(
            state.configured_clients.contains_key("codex_cli"),
            "Codex ownership survives preserving clear: {:?}",
            state.configured_clients
        );
        assert!(
            state.remembered_clients.contains_key("claude_code"),
            "non-Codex client is snapshotted for restore: {:?}",
            state.remembered_clients
        );
        assert!(
            !fs::read_to_string(home.path().join(".zshrc"))
                .unwrap()
                .contains("ANTHROPIC_BASE_URL=http://127.0.0.1:6867"),
            "Claude route is removed while Codex remains managed"
        );
    }

    #[test]
    #[serial_test::serial]
    fn list_client_connectors_carries_verification_only_for_enabled_clients() {
        // The connector panel keys its status line off these two fields: an
        // enabled client must arrive with its checks attached (the panel has
        // no other way to say what is wrong), and a disabled one must carry
        // none, so the list never implies it verified something it skipped.
        let _home = TestHome::new();
        super::apply_client_setup("codex").expect("apply_client_setup succeeds");

        // installed: false is the Codex desktop-app/IDE user -- they share
        // ~/.codex/config.toml with the CLI, so the connector is configurable
        // and verifiable without the CLI binary on disk.
        let detected = vec![crate::models::ClientStatus {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            installed: false,
            configured: true,
            health: crate::models::ClientHealth::Healthy,
            notes: Vec::new(),
        }];
        let connectors = super::list_client_connectors(&detected).expect("listing succeeds");

        let codex = connectors
            .iter()
            .find(|connector| connector.client_id == "codex")
            .expect("codex connector listed");
        assert!(!codex.installed);
        assert!(codex.enabled);
        assert!(codex.verified);
        let verification = codex
            .verification
            .as_ref()
            .expect("verification attached for an enabled client");
        assert_eq!(verification.verified, codex.verified);
        assert!(
            verification
                .checks
                .iter()
                .any(|check| check.contains("config.toml")),
            "config.toml check reported, got: {:?}",
            verification.checks
        );

        let grok = connectors
            .iter()
            .find(|connector| connector.client_id == "grok_build")
            .expect("grok connector listed");
        assert!(!grok.enabled);
        assert!(grok.verification.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn list_client_connectors_names_a_foreign_codex_owner_while_disabled() {
        // Regression: the Codex route can be owned by another tool while our
        // connector is *off* (Cockpit Tools writes root `model_provider =
        // "codex_local_access"`). `verification` is deliberately absent when
        // disabled, so the panel read `undefined`, skipped the takeover
        // confirmation, and sent `allowTakeover: false` straight into the
        // backend's "refusing to replace custom Codex model_provider" error --
        // the toggle looked broken with no way to say yes. The owner must be
        // reported independently of our own on/off state.
        let home = TestHome::new();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"codex_local_access\"\n\n[model_providers.codex_local_access]\nname = \"Cockpit\"\n",
        )
        .unwrap();

        let detected = vec![crate::models::ClientStatus {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            installed: true,
            configured: false,
            health: crate::models::ClientHealth::Healthy,
            notes: Vec::new(),
        }];
        let connectors = super::list_client_connectors(&detected).expect("listing succeeds");

        let codex = connectors
            .iter()
            .find(|connector| connector.client_id == "codex")
            .expect("codex connector listed");
        assert!(!codex.enabled, "connector starts disabled");
        assert!(
            codex.verification.is_none(),
            "a disabled connector still carries no verification"
        );
        assert_eq!(
            codex.foreign_provider.as_deref(),
            Some("Cockpit (codex_local_access)"),
            "the off connector must still name whoever owns the Codex route"
        );

        // Non-Codex connectors share no route with `~/.codex/config.toml`.
        let grok = connectors
            .iter()
            .find(|connector| connector.client_id == "grok_build")
            .expect("grok connector listed");
        assert!(grok.foreign_provider.is_none());

        // Once Headroom owns the route the field empties out, so the toggle
        // stops asking for a takeover nobody needs.
        super::apply_client_setup_with_options("codex", true)
            .expect("confirmed takeover succeeds");
        let connectors = super::list_client_connectors(&detected).expect("listing succeeds");
        let codex = connectors
            .iter()
            .find(|connector| connector.client_id == "codex")
            .expect("codex connector listed");
        assert!(codex.enabled);
        assert!(codex.verified);
        assert!(
            codex.foreign_provider.is_none(),
            "Headroom's own provider is not foreign, got: {:?}",
            codex.foreign_provider
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_then_verify_then_disable_codex_round_trip() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();

        let result = super::apply_client_setup("codex").expect("apply_client_setup succeeds");
        assert!(result.applied);
        assert_eq!(result.client_id, "codex");

        // Managed provider block lands in ~/.codex/config.toml.
        let config_toml = home.path().join(".codex").join("config.toml");
        let toml = fs::read_to_string(&config_toml).expect("codex config.toml written");
        assert!(
            toml.contains("# >>> headroom-local-community:codex_cli >>>"),
            "managed marker present, got:\n{toml}"
        );
        assert!(
            toml.contains("model_provider = \"headroom_local_community\""),
            "model_provider set, got:\n{toml}"
        );
        assert!(
            toml.contains(&format!(
                "base_url = \"{}\"",
                super::HEADROOM_OPENAI_BASE_URL
            )),
            "provider base_url points at proxy, got:\n{toml}"
        );
        assert!(
            toml.contains("name = \"OpenAI\""),
            "managed provider keeps Codex's OpenAI capability classification, got:\n{toml}"
        );
        assert!(
            toml.contains("supports_websockets = false"),
            "Codex must use the reliable HTTP Responses transport, got:\n{toml}"
        );
        // No ~/.codex/auth.json in this test ⇒ not ChatGPT-OAuth ⇒ the flag is
        // omitted (it would force an OpenAI OAuth login for API-key users, #406).
        assert!(
            !toml.contains("requires_openai_auth"),
            "requires_openai_auth must NOT be written without ChatGPT auth, got:\n{toml}"
        );

        // OPENAI_BASE_URL exported from a managed shell block.
        let zshrc = fs::read_to_string(home.path().join(".zshrc")).unwrap();
        let zshenv = fs::read_to_string(home.path().join(".zshenv")).unwrap();
        let combined = format!("{zshrc}\n{zshenv}");
        assert!(
            combined.contains(&format!(
                "OPENAI_BASE_URL={}",
                super::HEADROOM_OPENAI_BASE_URL
            )),
            "OPENAI_BASE_URL exported from a managed shell block, got:\n{combined}"
        );

        // verify_client_setup reports the configured checks and passes.
        let verification =
            super::verify_client_setup("codex").expect("verify_client_setup succeeds");
        assert_eq!(verification.client_id, "codex");
        assert!(
            verification.failures.is_empty(),
            "no verification failures, got: {:?}",
            verification.failures
        );
        assert!(
            verification
                .checks
                .iter()
                .any(|c| c.contains("config.toml")),
            "verification reports the toml check, got: {:?}",
            verification.checks
        );

        // Disable strips both the toml block and the shell export.
        super::disable_client_setup("codex").expect("disable_client_setup succeeds");
        let toml_after = fs::read_to_string(&config_toml).unwrap_or_default();
        assert!(
            !toml_after.contains("# >>> headroom-local-community:codex_cli >>>"),
            "managed block removed on disable, got:\n{toml_after}"
        );
        let combined_after = format!(
            "{}\n{}",
            fs::read_to_string(home.path().join(".zshrc")).unwrap(),
            fs::read_to_string(home.path().join(".zshenv")).unwrap(),
        );
        assert!(
            !combined_after.contains(&format!(
                "OPENAI_BASE_URL={}",
                super::HEADROOM_OPENAI_BASE_URL
            )),
            "shell export removed on disable, got:\n{combined_after}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_then_verify_then_disable_opencode_round_trip() {
        let _home = TestHome::new(); // env guard

        let result = super::apply_client_setup("opencode").expect("apply_client_setup succeeds");
        assert!(result.applied);
        assert_eq!(result.client_id, "opencode");

        // Resolve via the same function the apply path uses: the config lands
        // under XDG_CONFIG_HOME on Unix but %APPDATA% on Windows.
        let config_path = super::opencode_config_path();
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).expect("config written"))
                .expect("valid json");
        for provider in ["anthropic", "openai"] {
            assert_eq!(
                config["provider"][provider]["options"]["baseURL"],
                serde_json::json!("http://127.0.0.1:6867/v1"),
                "{provider} routed through proxy, got:\n{config:#}"
            );
        }

        let verification =
            super::verify_client_setup("opencode").expect("verify_client_setup succeeds");
        assert!(
            verification.failures.is_empty(),
            "{:?}",
            verification.failures
        );

        super::disable_client_setup("opencode").expect("disable_client_setup succeeds");
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            after.get("provider").is_none(),
            "provider husk removed on disable, got:\n{after:#}"
        );
        assert!(!super::is_opencode_enabled());
    }

    #[test]
    #[serial_test::serial]
    fn opencode_apply_preserves_existing_base_url_and_restores_on_disable() {
        let _home = TestHome::new(); // env guard
        let config_dir = super::opencode_config_dir();
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("opencode.json");
        fs::write(
            &config_path,
            r#"{
  "theme": "tokyonight",
  "provider": {
    "anthropic": {
      "options": {
        "baseURL": "https://gateway.corp.example/v1",
        "timeout": 5000
      }
    }
  }
}"#,
        )
        .unwrap();

        super::apply_client_setup("opencode").expect("apply succeeds");

        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(config["theme"], serde_json::json!("tokyonight"));
        assert_eq!(
            config["provider"]["anthropic"]["options"]["timeout"],
            serde_json::json!(5000),
            "sibling option keys preserved"
        );
        assert_eq!(
            config["provider"]["anthropic"]["options"]["baseURL"],
            serde_json::json!("http://127.0.0.1:6867/v1")
        );

        super::disable_client_setup("opencode").expect("disable succeeds");
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            after["provider"]["anthropic"]["options"]["baseURL"],
            serde_json::json!("https://gateway.corp.example/v1"),
            "original gateway URL restored, got:\n{after:#}"
        );
        assert_eq!(after["theme"], serde_json::json!("tokyonight"));
    }

    #[test]
    #[serial_test::serial]
    fn opencode_apply_unwraps_stale_wrap_config() {
        let _home = TestHome::new(); // env guard
        let config_dir = super::opencode_config_dir();
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("opencode.json");
        // `headroom wrap opencode` killed before it could restore: its own
        // provider block, plus a native provider repointed at its proxy port.
        fs::write(
            &config_path,
            r#"{
  "theme": "tokyonight",
  "provider": {
    "headroom": {
      "npm": "@ai-sdk/openai-compatible",
      "options": { "baseURL": "http://127.0.0.1:8787/v1" }
    },
    "anthropic": {
      "options": { "baseURL": "http://127.0.0.1:8787/v1" }
    }
  }
}"#,
        )
        .unwrap();

        super::apply_client_setup("opencode").expect("apply unwraps instead of refusing");

        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            config["provider"].get("headroom_local_community").is_none(),
            "stale wrap provider removed, got:\n{config:#}"
        );
        assert_eq!(
            config["provider"]["anthropic"]["options"]["baseURL"],
            serde_json::json!("http://127.0.0.1:6867/v1")
        );
        assert_eq!(config["theme"], serde_json::json!("tokyonight"));

        super::disable_client_setup("opencode").expect("disable succeeds");
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            after
                .pointer("/provider/anthropic/options/baseURL")
                .is_none(),
            "wrap's dead port must not be restored as the user's own, got:\n{after:#}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn opencode_apply_installs_transport_plugin_and_disable_removes_it() {
        let _home = TestHome::new(); // env guard

        super::apply_client_setup("opencode").expect("apply succeeds");

        let plugin_path = super::opencode_plugin_install_path();
        assert!(plugin_path.is_file(), "vendored plugin written to app data");
        let config_path = super::opencode_config_path();
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        let plugins = config["plugin"].as_array().expect("plugin array present");
        assert!(
            plugins
                .iter()
                .any(|v| v.as_str() == Some(&plugin_path.display().to_string())),
            "plugin path registered, got:\n{config:#}"
        );

        super::disable_client_setup("opencode").expect("disable succeeds");
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            after.get("plugin").is_none(),
            "plugin entry removed on disable, got:\n{after:#}"
        );
        assert!(!plugin_path.exists(), "plugin file removed on disable");
    }

    #[test]
    fn strip_jsonc_removes_comments_and_trailing_commas() {
        let src = r#"{
  // line comment
  "a": "value with // not a comment",
  /* block
     comment */
  "b": [1, 2, /* inline */ 3,],
  "c": "trailing \" escape",
}"#;
        let parsed: serde_json::Value =
            serde_json::from_str(&super::strip_jsonc(src)).expect("stripped source parses");
        assert_eq!(
            parsed["a"],
            serde_json::json!("value with // not a comment")
        );
        assert_eq!(parsed["b"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    #[serial_test::serial]
    fn opencode_apply_tolerates_jsonc_config() {
        let _home = TestHome::new(); // env guard
        let config_path = super::opencode_config_dir().join("opencode.jsonc");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            "{\n  // user comment (RUST-61: setup used to refuse this file)\n  \"theme\": \"dark\",\n}\n",
        )
        .unwrap();

        super::apply_client_setup("opencode").expect("apply succeeds on .jsonc with comments");
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap())
                .expect("apply wrote strict json");
        assert_eq!(after["theme"], serde_json::json!("dark"), "user key kept");
        for provider in super::OPENCODE_MANAGED_PROVIDERS {
            assert_eq!(
                super::opencode_provider_base_url(&after, provider).as_deref(),
                Some(super::HEADROOM_OPENCODE_BASE_URL),
                "provider {provider} routed"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn opencode_disable_tolerates_comments_added_after_apply() {
        let _home = TestHome::new(); // env guard

        super::apply_client_setup("opencode").expect("apply succeeds");
        let config_path = super::opencode_config_path();
        let mut contents = fs::read_to_string(&config_path).unwrap();
        contents.insert_str(0, "// routed through headroom\n");
        fs::write(&config_path, &contents).unwrap();

        super::disable_client_setup("opencode").expect("disable succeeds despite comments");
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap())
                .expect("disable wrote parseable json");
        assert!(
            after.get("provider").is_none(),
            "proxy URLs removed, got:\n{after:#}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn opencode_config_path_prefers_jsonc_when_present() {
        let _home = TestHome::new(); // env guard
        let config_dir = super::opencode_config_dir();
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("opencode.jsonc"), "{}").unwrap();

        super::apply_client_setup("opencode").expect("apply succeeds");
        let jsonc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(config_dir.join("opencode.jsonc")).unwrap())
                .unwrap();
        assert_eq!(
            jsonc["provider"]["anthropic"]["options"]["baseURL"],
            serde_json::json!("http://127.0.0.1:6867/v1"),
            "jsonc file managed when it is the active config"
        );
        assert!(
            !config_dir.join("opencode.json").exists(),
            "no stray opencode.json created next to the active jsonc"
        );
    }

    #[test]
    #[serial_test::serial]
    fn grok_config_preserves_user_top_level_keys() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        let grok_dir = home.path().join(".grok");
        fs::create_dir_all(&grok_dir).unwrap();
        fs::write(grok_dir.join("config.toml"), "default_model = \"grok-4\"\n").unwrap();

        super::apply_client_setup("grok_build").expect("apply_client_setup succeeds");

        let toml = fs::read_to_string(grok_dir.join("config.toml")).unwrap();
        let key_pos = toml.find("default_model").expect("user key kept");
        let table_pos = toml
            .find("[model.grok-build]")
            .expect("managed table present");
        assert!(
            key_pos < table_pos,
            "top-level key must precede the managed table, got:\n{toml}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn grok_config_redirects_existing_grok_build_table_and_restores_on_disable() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        let grok_dir = home.path().join(".grok");
        fs::create_dir_all(&grok_dir).unwrap();
        fs::write(
            grok_dir.join("config.toml"),
            "[model.grok-build]\nbase_url = \"http://127.0.0.1:8787/v1\"\n",
        )
        .unwrap();

        super::apply_client_setup("grok_build").expect("apply_client_setup succeeds");

        let toml = fs::read_to_string(grok_dir.join("config.toml")).unwrap();
        assert_eq!(
            toml.matches("[model.grok-build]").count(),
            1,
            "no duplicate table, got:\n{toml}"
        );
        assert!(
            toml.contains(
                "base_url = \"http://127.0.0.1:6867/v1\"  # was: http://127.0.0.1:8787/v1"
            ),
            "base_url redirected in place, got:\n{toml}"
        );

        let verification =
            super::verify_client_setup("grok_build").expect("verify_client_setup succeeds");
        assert!(
            verification.failures.is_empty(),
            "{:?}",
            verification.failures
        );

        super::disable_client_setup("grok_build").expect("disable_client_setup succeeds");
        let after = fs::read_to_string(grok_dir.join("config.toml")).unwrap();
        assert!(
            after.contains("base_url = \"http://127.0.0.1:8787/v1\""),
            "original base_url restored, got:\n{after}"
        );
        assert!(
            !after.contains("# was:"),
            "redirect comment removed, got:\n{after}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_then_verify_then_disable_grok_build_round_trip() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();

        let result = super::apply_client_setup("grok_build").expect("apply_client_setup succeeds");
        assert!(result.applied);
        assert_eq!(result.client_id, "grok_build");

        let config_toml = home.path().join(".grok").join("config.toml");
        let toml = fs::read_to_string(&config_toml).expect("grok config.toml written");
        assert!(
            toml.contains("# >>> headroom-local-community:grok_build_proxy >>>"),
            "managed marker present, got:\n{toml}"
        );
        assert!(
            toml.contains("base_url = \"http://127.0.0.1:6867/v1\""),
            "proxy base_url set, got:\n{toml}"
        );

        let zshrc = fs::read_to_string(home.path().join(".zshrc")).unwrap();
        let zshenv = fs::read_to_string(home.path().join(".zshenv")).unwrap();
        let combined = format!("{zshrc}\n{zshenv}");
        assert!(
            combined.contains("GROK_CLI_CHAT_PROXY_BASE_URL=http://127.0.0.1:6867/v1"),
            "GROK_CLI_CHAT_PROXY_BASE_URL exported, got:\n{combined}"
        );

        let verification =
            super::verify_client_setup("grok_build").expect("verify_client_setup succeeds");
        assert!(
            verification.failures.is_empty(),
            "{:?}",
            verification.failures
        );

        super::disable_client_setup("grok_build").expect("disable_client_setup succeeds");
        let toml_after = fs::read_to_string(&config_toml).unwrap_or_default();
        assert!(
            !toml_after.contains("# >>> headroom-local-community:grok_build_proxy >>>"),
            "managed block removed on disable, got:\n{toml_after}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_is_byte_idempotent() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();

        super::apply_client_setup("codex").expect("first apply");
        let config_toml = home.path().join(".codex").join("config.toml");
        let toml_first = fs::read_to_string(&config_toml).unwrap();
        let zshenv_first = fs::read_to_string(home.path().join(".zshenv")).unwrap();

        super::apply_client_setup("codex").expect("second apply");
        let toml_second = fs::read_to_string(&config_toml).unwrap();
        let zshenv_second = fs::read_to_string(home.path().join(".zshenv")).unwrap();

        assert_eq!(toml_first, toml_second, "config.toml byte-stable");
        assert_eq!(zshenv_first, zshenv_second, "zshenv byte-stable");
        assert_eq!(
            toml_second
                .matches("# >>> headroom-local-community:codex_cli >>>")
                .count(),
            1,
            "managed block appears exactly once"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_writes_and_registers_guard() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();

        super::apply_client_setup("codex").expect("apply");

        let script = home
            .path()
            .join(".codex")
            .join("hooks")
            .join("headroom-local-community-codex-guard.py");
        assert!(script.exists(), "guard script written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&script).unwrap().permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "guard script is executable, got {mode:o}"
            );
        }

        let hooks: serde_json::Value =
            read_settings_json(&home.path().join(".codex").join("hooks.json"));
        // The registered command is platform-dependent (/usr/bin/python3 vs the
        // quoted managed python.exe), so assert against the real builder.
        let command = super::codex_guard_command();
        // SessionStart only: on UserPromptSubmit a nonzero exit blocks the prompt.
        let session_registered = hooks["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["hooks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|h| h["command"] == serde_json::Value::String(command.clone()))
            });
        assert!(
            session_registered,
            "guard registered on SessionStart, got:\n{hooks:#}"
        );
        assert!(
            !hooks["hooks"]["UserPromptSubmit"]
                .to_string()
                .contains("headroom-local-community-codex-guard.py"),
            "guard must not register on UserPromptSubmit, got:\n{hooks:#}"
        );
        assert_eq!(
            hooks["hooks"]["SessionStart"][0]["matcher"],
            "startup|resume|clear|compact"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn guard_commands_do_not_hardcode_unix_python() {
        assert!(claude_guard_command().contains("python.exe"));
        assert!(codex_guard_command().contains("python.exe"));
        assert!(!claude_guard_command().starts_with("/usr/bin/python3"));
        assert!(!codex_guard_command().starts_with("/usr/bin/python3"));
    }

    #[test]
    fn hook_command_is_the_bare_script_path_on_unix() {
        let path = PathBuf::from("/home/g/.claude/hooks/headroom-local-community-rtk-rewrite.sh");
        let cmd = super::hook_shell_command(&path).expect("hook command");
        if cfg!(target_os = "windows") {
            // Same PowerShell contract as the guard: call operator, quoted
            // interpreter, quoted script.
            assert!(cmd.starts_with("& "), "{cmd}");
            assert!(
                cmd.ends_with("\"/home/g/.claude/hooks/headroom-local-community-rtk-rewrite.sh\"")
            );
            assert!(cmd.contains("bash"), "{cmd}");
        } else {
            assert_eq!(
                cmd,
                "/home/g/.claude/hooks/headroom-local-community-rtk-rewrite.sh"
            );
        }
    }

    /// Regression: Claude Code runs SessionStart hooks through PowerShell on
    /// Windows. A command that starts with a quoted interpreter path parses as
    /// a string literal, not a command, so the guard died with
    /// "SessionStart:startup hook error / Failed with non-blocking status code:
    /// At line:1 char:81" -- char 81 being the first character of the unquoted
    /// script path that followed the 79-char quoted python path plus a space.
    /// The call operator is what makes it a command, and the script path must
    /// be quoted because profile directories contain spaces.
    #[test]
    fn windows_guard_command_is_powershell_callable() {
        let cmd = super::join_guard_command(
            "\"C:\\Users\\garm\\AppData\\Local\\Headroom\\headroom\\runtime\\venv\\Scripts\\python.exe\"",
            "C:\\Users\\garm space\\.claude\\hooks\\headroom-local-community-claude-guard.py",
            true,
        );
        assert!(
            cmd.starts_with("& \""),
            "PowerShell needs the call operator before a quoted path, got: {cmd}"
        );
        assert!(
            cmd.ends_with("headroom-local-community-claude-guard.py\""),
            "script path must be quoted so spaces survive, got: {cmd}"
        );
        // Backslashes stay single: PowerShell escapes with a backtick, so
        // POSIX-style doubling would break every Windows path.
        assert!(
            !cmd.contains("\\\\"),
            "path must not be POSIX-escaped: {cmd}"
        );
    }

    #[test]
    fn unix_guard_command_is_unquoted() {
        let cmd = super::join_guard_command("/usr/bin/python3", "/home/g/.claude/guard.py", false);
        assert_eq!(cmd, "/usr/bin/python3 /home/g/.claude/guard.py");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn opencode_dirs_resolve_under_appdata_on_windows() {
        let config = super::opencode_config_dir();
        let data = super::opencode_data_dir();
        assert!(config.ends_with("opencode"));
        assert!(data.ends_with("opencode"));
        // XDG vars are unset in a clean cmd.exe session; APPDATA must be used.
        let appdata = std::env::var("APPDATA").expect("APPDATA should be set on Windows");
        let local_appdata =
            std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA should be set on Windows");
        assert!(config.starts_with(PathBuf::from(appdata)));
        assert!(data.starts_with(PathBuf::from(local_appdata)));
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_guard_is_idempotent_and_disable_preserves_user_hooks() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        fs::write(home.path().join(".zshenv"), "# user zshenv\n").unwrap();
        // Pre-existing user-authored hook that must survive apply and disable.
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        fs::write(
            home.path().join(".codex").join("hooks.json"),
            r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#,
        )
        .unwrap();

        super::apply_client_setup("codex").expect("first apply");
        super::apply_client_setup("codex").expect("second apply");

        let hooks_path = home.path().join(".codex").join("hooks.json");
        let hooks = read_settings_json(&hooks_path);
        // Guard registered on SessionStart exactly once (not UserPromptSubmit,
        // where a nonzero exit would block every prompt).
        let guard_count = hooks["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| {
                entry["hooks"].as_array().unwrap().iter().any(|h| {
                    h["command"]
                        .as_str()
                        .map(|c| c.contains("headroom-local-community-codex-guard.py"))
                        .unwrap_or(false)
                })
            })
            .count();
        assert_eq!(
            guard_count, 1,
            "guard registered exactly once on SessionStart, got:\n{hooks:#}"
        );
        // The guard must NOT be on UserPromptSubmit; the pre-existing user hook
        // there survives untouched.
        let user_prompt = hooks["hooks"]["UserPromptSubmit"].to_string();
        assert!(
            !user_prompt.contains("headroom-local-community-codex-guard.py"),
            "guard must not register on UserPromptSubmit, got:\n{hooks:#}"
        );
        assert!(
            user_prompt.contains("echo mine"),
            "user's UserPromptSubmit hook preserved, got:\n{hooks:#}"
        );

        super::disable_client_setup("codex").expect("disable");

        let script = home
            .path()
            .join(".codex")
            .join("hooks")
            .join("headroom-local-community-codex-guard.py");
        assert!(!script.exists(), "guard script removed on disable");
        let after = read_settings_json(&hooks_path);
        let after_str = serde_json::to_string(&after).unwrap();
        assert!(
            !after_str.contains("headroom-local-community-codex-guard.py"),
            "guard stripped from hooks.json, got:\n{after:#}"
        );
        assert!(
            after_str.contains("echo mine"),
            "user-authored hook preserved, got:\n{after:#}"
        );
    }

    #[test]
    fn remove_guard_hook_entries_strips_stale_interpreter_and_argv_forms() {
        // Regression: an entry written by another build (different interpreter) or
        // normalized by Codex into argv-array form under an unregistered event must
        // still be stripped -- otherwise deleting the script leaves a dangling hook.
        let home = TestHome::new();
        let hooks_path = home.path().join("hooks.json");
        let script = "/Users/x/.codex/hooks/headroom-local-community-codex-guard.py";
        fs::write(
            &hooks_path,
            format!(
                r#"{{"hooks":{{
                    "SessionStart":[{{"hooks":[{{"type":"command","command":"/opt/homebrew/bin/python3 {script}"}}]}}],
                    "SessionEnd":[{{"hooks":[{{"type":"command","command":["python3","{script}"]}}]}}],
                    "UserPromptSubmit":[{{"hooks":[{{"type":"command","command":"echo mine"}}]}}]
                }}}}"#
            ),
        )
        .unwrap();

        super::remove_guard_hook_entries(&hooks_path, script, true, None).unwrap();

        let after = read_settings_json(&hooks_path);
        let after_str = serde_json::to_string(&after).unwrap();
        assert!(
            !after_str.contains("headroom-local-community-codex-guard.py"),
            "stale guard forms stripped, got:\n{after:#}"
        );
        assert!(
            after_str.contains("echo mine"),
            "user hook preserved, got:\n{after:#}"
        );
    }

    #[test]
    fn codex_guard_script_is_informational_never_blocks() {
        // Regression: a nonzero exit on a Codex hook blocks the session, which
        // held lapsed users' own OpenAI-billed Codex hostage to the app. The
        // guard must only notify, never block.
        let script = super::build_codex_guard_script();
        assert!(
            !script.contains("return 2"),
            "codex guard must never block (exit 2)"
        );
        assert!(script.contains("return 0"));
    }

    #[test]
    #[serial_test::serial]
    fn ensure_codex_guard_migrates_off_user_prompt_submit() {
        let home = TestHome::new();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(codex_dir.join("hooks")).unwrap();
        let hooks_path = codex_dir.join("hooks.json");
        let cmd = format!(
            "/usr/bin/python3 {}",
            codex_dir
                .join("hooks")
                .join("headroom-local-community-codex-guard.py")
                .display()
        );
        // Old install: guard registered on both SessionStart and UserPromptSubmit.
        // Built via serde_json so the path is JSON-escaped on Windows (raw
        // format! would write lone backslashes the parser mangles).
        let seeded = serde_json::json!({"hooks":{
            "SessionStart":[{"matcher":"startup|resume|clear|compact","hooks":[{"type":"command","command": cmd.as_str()}]}],
            "UserPromptSubmit":[{"hooks":[{"type":"command","command": cmd.as_str()}]}]
        }});
        fs::write(&hooks_path, serde_json::to_string(&seeded).unwrap()).unwrap();

        super::ensure_codex_guard_hook().unwrap();

        let after = read_settings_json(&hooks_path);
        let dump = serde_json::to_string_pretty(&after).unwrap();
        assert!(
            !after["hooks"]["UserPromptSubmit"]
                .to_string()
                .contains("headroom-local-community-codex-guard.py"),
            "guard stripped from UserPromptSubmit, got:\n{dump}"
        );
        assert!(
            after["hooks"]["SessionStart"]
                .to_string()
                .contains("headroom-local-community-codex-guard.py"),
            "guard kept on SessionStart, got:\n{dump}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_emits_requires_openai_auth_for_chatgpt_users() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("auth.json"),
            "{\"auth_mode\":\"chatgpt\",\"tokens\":{\"account_id\":\"acct_123\"}}",
        )
        .unwrap();

        super::apply_client_setup("codex").expect("apply_client_setup succeeds");
        let toml = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            toml.contains("requires_openai_auth = true"),
            "ChatGPT-OAuth users need the flag for the account menu, got:\n{toml}"
        );
        assert!(
            toml.contains(&format!(
                "base_url = \"{}\"",
                super::HEADROOM_CHATGPT_BASE_URL
            )),
            "ChatGPT-OAuth provider must retain the native backend path, got:\n{toml}"
        );
        assert!(
            toml.contains("supports_standalone_web_search = true"),
            "ChatGPT provider must advertise Codex standalone search, got:\n{toml}"
        );
        let shell = fs::read_to_string(home.path().join(".zshrc")).unwrap();
        assert!(
            shell.contains(&format!(
                "OPENAI_BASE_URL={}",
                super::HEADROOM_CHATGPT_BASE_URL
            )),
            "ChatGPT shell route must use the backend path, got:\n{shell}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_omits_requires_openai_auth_for_api_key_users() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("auth.json"),
            "{\"auth_mode\":\"apikey\",\"OPENAI_API_KEY\":\"sk-test\"}",
        )
        .unwrap();

        super::apply_client_setup("codex").expect("apply_client_setup succeeds");
        let toml = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !toml.contains("requires_openai_auth"),
            "API-key users must not be forced into an OpenAI OAuth login (#406), got:\n{toml}"
        );
        assert!(
            toml.contains(&format!(
                "base_url = \"{}\"",
                super::HEADROOM_OPENAI_BASE_URL
            )),
            "API-key provider must use the public /v1 route, got:\n{toml}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn codex_auth_mode_aliases_select_chatgpt_router_path() {
        let home = TestHome::new();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();

        for mode in ["headers", "agent_identity", "personal-access-token"] {
            fs::write(
                codex_dir.join("auth.json"),
                serde_json::json!({"auth_mode": mode}).to_string(),
            )
            .unwrap();
            assert!(
                super::codex_uses_chatgpt_auth(),
                "{mode} must retain the ChatGPT /backend-api/codex route"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn externally_supplied_codex_backend_auth_does_not_force_login() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("auth.json"),
            serde_json::json!({"auth_mode":"headers"}).to_string(),
        )
        .unwrap();

        super::apply_client_setup("codex").expect("apply_client_setup succeeds");
        let toml = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            toml.contains(&format!(
                "base_url = \"{}\"",
                super::HEADROOM_CHATGPT_BASE_URL
            )),
            "headers auth still needs the ChatGPT backend route, got:\n{toml}"
        );
        assert!(
            !toml.contains("requires_openai_auth"),
            "host-supplied headers auth must not trigger an interactive login, got:\n{toml}"
        );
    }

    /// Build an unsigned JWT whose payload carries `claims`. Only the payload
    /// segment is read, so header and signature are placeholders.
    fn fake_id_token(claims: &str) -> String {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.sig",
            b64.encode(b"{\"alg\":\"none\"}"),
            b64.encode(claims.as_bytes())
        )
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_emits_requires_openai_auth_from_id_token_claim() {
        // Newer Codex writes neither `auth_mode` nor `tokens.account_id`; the
        // account id lives only in the id_token claims (upstream #3206). Read
        // as API-key mode, Codex sends no Authorization header and every
        // request 401s.
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let token = fake_id_token(
            "{\"https://api.openai.com/auth\":{\"chatgpt_account_id\":\"acct_123\"}}",
        );
        fs::write(
            codex_dir.join("auth.json"),
            format!("{{\"tokens\":{{\"id_token\":\"{token}\"}}}}"),
        )
        .unwrap();

        super::apply_client_setup("codex").expect("apply_client_setup succeeds");
        let toml = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            toml.contains("requires_openai_auth = true"),
            "id_token-only ChatGPT auth still needs the flag, got:\n{toml}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_emits_requires_openai_auth_from_top_level_id_token_claim() {
        // Some Codex releases put `id_token` at the auth.json root instead of
        // under `tokens`. The routing classifier must still preserve the
        // ChatGPT backend path and account capability flag in that layout.
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let token = fake_id_token(
            "{\"https://api.openai.com/auth\":{\"chatgpt_account_id\":\"acct_top_level\"}}",
        );
        fs::write(
            codex_dir.join("auth.json"),
            format!("{{\"id_token\":\"{token}\"}}"),
        )
        .unwrap();

        super::apply_client_setup("codex").expect("apply_client_setup succeeds");
        let toml = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            toml.contains("requires_openai_auth = true"),
            "top-level id_token ChatGPT auth still needs the flag, got:\n{toml}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_omits_requires_openai_auth_when_apikey_mode_is_explicit() {
        // An explicit `auth_mode` wins outright: a stale ChatGPT id_token
        // alongside it must not force an OAuth login (#406).
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let token = fake_id_token(
            "{\"https://api.openai.com/auth\":{\"chatgpt_account_id\":\"acct_123\"}}",
        );
        fs::write(
            codex_dir.join("auth.json"),
            format!("{{\"auth_mode\":\"apikey\",\"tokens\":{{\"id_token\":\"{token}\"}}}}"),
        )
        .unwrap();

        super::apply_client_setup("codex").expect("apply_client_setup succeeds");
        let toml = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !toml.contains("requires_openai_auth"),
            "explicit apikey mode must win over a ChatGPT id_token (#406), got:\n{toml}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_omits_requires_openai_auth_for_id_token_without_claim() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let token = fake_id_token("{\"sub\":\"user_1\"}");
        fs::write(
            codex_dir.join("auth.json"),
            format!("{{\"tokens\":{{\"id_token\":\"{token}\"}}}}"),
        )
        .unwrap();

        super::apply_client_setup("codex").expect("apply_client_setup succeeds");
        let toml = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !toml.contains("requires_openai_auth"),
            "an id_token without the ChatGPT claim is not ChatGPT auth, got:\n{toml}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_keeps_root_keys_at_root_scope_when_config_ends_in_a_table() {
        // Regression for the `invalid type: string "headroom", expected a
        // boolean in features` error: a config whose last table is `[features]`
        // (boolean-only values) used to absorb the appended root keys.
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let config_toml = codex_dir.join("config.toml");
        fs::write(
            &config_toml,
            "model = \"gpt-5.4\"\n\n[features]\njs_repl = false\n",
        )
        .unwrap();

        super::apply_client_setup("codex").expect("apply succeeds");

        let raw = fs::read_to_string(&config_toml).unwrap();
        let parsed: toml::Value = raw
            .parse()
            .unwrap_or_else(|e| panic!("valid toml: {e}\n{raw}"));

        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some("headroom_local_community"),
            "model_provider must resolve at root scope, got:\n{raw}"
        );
        assert!(
            parsed
                .get("features")
                .and_then(|f| f.get("model_provider"))
                .is_none(),
            "model_provider must not leak into [features], got:\n{raw}"
        );
        assert_eq!(
            parsed
                .get("model_providers")
                .and_then(|m| m.get("headroom_local_community"))
                .and_then(|h| h.get("base_url"))
                .and_then(|v| v.as_str()),
            Some(super::HEADROOM_OPENAI_BASE_URL),
            "provider table base_url points at the proxy, got:\n{raw}"
        );
        // The user's own content survives untouched.
        assert_eq!(
            parsed.get("model").and_then(|v| v.as_str()),
            Some("gpt-5.4"),
            "existing root key preserved, got:\n{raw}"
        );
        assert_eq!(
            parsed
                .get("features")
                .and_then(|f| f.get("js_repl"))
                .and_then(|v| v.as_bool()),
            Some(false),
            "existing [features] table preserved, got:\n{raw}"
        );
    }

    #[test]
    fn oss_remnant_warnings_clean_install_is_silent() {
        assert!(oss_remnant_warnings(false, false, false, false).is_empty());
    }

    #[test]
    fn oss_remnant_warnings_flags_each_remnant() {
        let w = oss_remnant_warnings(true, true, true, true);
        assert_eq!(w.len(), 4, "one warning per remnant, got: {w:?}");
        assert!(w.iter().any(|m| m.contains(":8787")));
        assert!(w.iter().any(|m| m.contains("~/.local/bin/headroom")));
        assert!(w.iter().any(|m| m.contains("~/.local/bin/rtk")));
        assert!(w.iter().any(|m| m.contains("settings.json")));
    }

    #[test]
    fn render_codex_config_collapses_duplicate_managed_blocks() {
        // Regression: a config left with TWO managed provider blocks (interrupted
        // write / older build) used to keep one survivor that regenerated forever,
        // surfacing as a duplicate [model_providers.headroom_local_community] the user deleted by
        // hand. render must collapse all duplicates down to exactly one.
        let dup = "# >>> headroom-local-community:codex_cli_provider >>>\n\
                   [model_providers.headroom_local_community]\n\
                   base_url = \"http://stale/v1\"\n\
                   # <<< headroom-local-community:codex_cli_provider <<<\n\
                   model = \"gpt-5.4\"\n\
                   # >>> headroom-local-community:codex_cli_provider >>>\n\
                   [model_providers.headroom_local_community]\n\
                   base_url = \"http://stale2/v1\"\n\
                   # <<< headroom-local-community:codex_cli_provider <<<\n";

        let rendered = render_codex_config(dup).expect("managed config renders");

        assert_eq!(
            rendered
                .matches("[model_providers.headroom_local_community]")
                .count(),
            1,
            "exactly one managed provider table after render, got:\n{rendered}"
        );
        assert!(
            rendered.parse::<toml::Value>().is_ok(),
            "rendered config is valid toml, got:\n{rendered}"
        );
        assert!(
            rendered.contains("model = \"gpt-5.4\""),
            "user content between the duplicates is preserved, got:\n{rendered}"
        );
    }

    #[test]
    fn codex_provider_block_matches_legacy_headroom_markers() {
        let home = TestHome::new();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let legacy = format!(
            "# >>> headroom:codex_cli >>>\n\
             model_provider = \"headroom_local_community\"\n\
             openai_base_url = \"{}\"\n\
             # <<< headroom:codex_cli <<<\n\n\
             # >>> headroom:codex_cli_provider >>>\n\
             [model_providers.headroom_local_community]\n\
             name = \"OpenAI\"\n\
             base_url = \"{}\"\n\
             supports_websockets = false\n\
             supports_standalone_web_search = true\n\
             # <<< headroom:codex_cli_provider <<<\n",
            super::HEADROOM_OPENAI_BASE_URL,
            super::HEADROOM_OPENAI_BASE_URL,
        );
        fs::write(codex_dir.join("config.toml"), legacy).unwrap();

        assert!(super::codex_provider_block_matches().unwrap());
    }

    #[test]
    fn render_codex_config_drops_orphan_managed_end_markers() {
        let malformed = "# <<< headroom-local-community:codex_cli <<<\n\
                         # <<< headroom-local-community:codex_cli <<<\n\
                         model = \"gpt-5.4\"\n";

        let rendered = render_codex_config(malformed).expect("managed config renders");

        assert_eq!(
            rendered
                .matches("# <<< headroom-local-community:codex_cli <<<")
                .count(),
            1,
            "orphan closing markers must be replaced by one managed block, got:\n{rendered}"
        );
        assert!(
            rendered.parse::<toml::Value>().is_ok(),
            "rendered config is valid toml, got:\n{rendered}"
        );
        assert!(
            rendered.contains("model = \"gpt-5.4\""),
            "user config is preserved, got:\n{rendered}"
        );
    }

    #[test]
    fn render_codex_config_drops_an_unmarked_headroom_provider_table() {
        // Regression for Sentry RUST-6K: an OSS `pip install headroom` (or a
        // hand-added table from before marker blocks) leaves an UNMARKED
        // [model_providers.headroom_local_community]. Adding our marked copy alongside it made
        // the whole config invalid TOML ("duplicate key"), and Codex then
        // refused to load ANY of it -- breaking every `codex` invocation, not
        // just our routing.
        let existing = "model = \"gpt-5.4\"\n\
                        \n\
                        [model_providers.headroom_local_community]\n\
                        name = \"Headroom (old oss install)\"\n\
                        base_url = \"http://127.0.0.1:8787/v1\"\n\
                        \n\
                        [model_providers.other]\n\
                        base_url = \"http://elsewhere/v1\"\n";

        let rendered = render_codex_config(existing).expect("managed config renders");

        assert_eq!(
            rendered
                .matches("[model_providers.headroom_local_community]")
                .count(),
            1,
            "exactly one headroom provider table after render, got:\n{rendered}"
        );
        assert!(
            rendered.parse::<toml::Value>().is_ok(),
            "rendered config is valid toml, got:\n{rendered}"
        );
        // The stale table's body must go with its header, not linger as orphan
        // keys absorbed into whatever table precedes them.
        assert!(
            !rendered.contains("8787"),
            "stale provider body is removed with its header, got:\n{rendered}"
        );
        // Everything that is not ours is untouched.
        assert!(
            rendered.contains("[model_providers.other]")
                && rendered.contains("http://elsewhere/v1")
                && rendered.contains("model = \"gpt-5.4\""),
            "foreign providers and user content are preserved, got:\n{rendered}"
        );
    }

    #[test]
    fn codex_foreign_model_provider_is_root_scope_only() {
        assert_eq!(
            super::codex_foreign_model_provider("model_provider = \"gateway\"\n").as_deref(),
            Some("gateway"),
        );
        // Our own managed value is not "foreign".
        assert_eq!(
            super::codex_foreign_model_provider("model_provider = \"headroom_local_community\"\n"),
            None,
        );
        // A model_provider inside a table belongs to that table, not the route.
        assert_eq!(
            super::codex_foreign_model_provider("[profiles.work]\nmodel_provider = \"gateway\"\n"),
            None,
        );
        assert_eq!(super::codex_foreign_model_provider(""), None);
    }

    #[test]
    fn codex_root_openai_base_url_parser_is_root_scope_only() {
        let existing = "'openai_base_url' = 'https://api.openai.com/v1/'\n\
                        model = \"gpt-5.6-terra\"\n\
                        [profiles.work]\n\
                        openai_base_url = \"https://gateway.example/v1\"\n";

        assert_eq!(
            super::codex_root_openai_base_urls(existing).unwrap(),
            vec!["https://api.openai.com/v1/"],
            "a profile-scoped gateway URL must not be treated as the root route"
        );
    }

    #[test]
    fn codex_takeover_base_url_allowlist_is_exact() {
        for allowed in [
            "https://api.openai.com",
            "https://api.openai.com/",
            "https://api.openai.com/v1",
            "https://api.openai.com/v1/",
            "http://127.0.0.1:6891/v1",
            "http://127.0.0.1:6891/backend-api/codex/",
        ] {
            assert!(
                super::codex_takeover_base_url_allowed(allowed),
                "expected migration source to be allowed: {allowed}"
            );
        }
        for port in [6767, 6768, 6769, 6790, 6867, 8787] {
            for path in ["", "/", "/v1", "/v1/", "/backend-api/codex/"] {
                let allowed = format!("http://127.0.0.1:{port}{path}");
                assert!(
                    super::codex_takeover_base_url_allowed(&allowed),
                    "known Headroom route should migrate: {allowed}"
                );
            }
        }

        for rejected in [
            "https://gateway.example/v1",
            "http://127.0.0.1:7777/v1",
            "http://localhost:6867/v1",
            "http://127.0.0.1:6891/",
            "http://127.0.0.1:6891/v1/custom",
            "https://api.openai.com/v1?tenant=other",
            "https://user:secret@api.openai.com/v1",
        ] {
            assert!(
                !super::codex_takeover_base_url_allowed(rejected),
                "custom route must be refused: {rejected}"
            );
        }
    }

    #[test]
    fn render_codex_config_replaces_allowed_root_url_without_duplicate_toml() {
        let existing = "model_provider = \"openai\"\n\
                        openai_base_url = \"https://api.openai.com/v1/\"\n\
                        model = \"gpt-5.6-terra\"\n\
                        [profiles.work]\n\
                        openai_base_url = \"https://gateway.example/v1\"\n";

        let rendered = render_codex_config(existing).expect("native route may be migrated");
        let parsed: toml::Value = rendered
            .parse()
            .unwrap_or_else(|e| panic!("valid TOML after route migration: {e}\n{rendered}"));
        assert!(matches!(
            parsed.get("openai_base_url").and_then(toml::Value::as_str),
            Some(super::HEADROOM_OPENAI_BASE_URL | super::HEADROOM_CHATGPT_BASE_URL)
        ));
        assert_eq!(
            parsed
                .get("profiles")
                .and_then(|profiles| profiles.get("work"))
                .and_then(|profile| profile.get("openai_base_url"))
                .and_then(toml::Value::as_str),
            Some("https://gateway.example/v1"),
            "table-scoped user URL must survive"
        );
    }

    #[test]
    fn render_codex_config_refuses_custom_root_url_for_allowed_providers() {
        for provider in ["openai", "headroom"] {
            let existing = format!(
                "model_provider = \"{provider}\"\nopenai_base_url = \"https://gateway.example/v1\"\n"
            );
            let error =
                render_codex_config(&existing).expect_err("a custom root URL must not be replaced");
            assert!(
                error
                    .to_string()
                    .contains("refusing to replace custom Codex openai_base_url"),
                "unexpected refusal for {provider}: {error:#}"
            );
        }
    }

    #[test]
    fn codex_takeover_allowlist_keeps_native_and_headroom_routes_only() {
        assert!(super::codex_takeover_provider_allowed("openai"));
        assert!(super::codex_takeover_provider_allowed("headroom"));
        assert!(super::codex_takeover_provider_allowed(
            "headroom_local_community"
        ));
        assert!(!super::codex_takeover_provider_allowed("gateway"));
        assert!(!super::codex_takeover_provider_allowed("OpenAI"));
    }

    #[test]
    #[serial_test::serial]
    fn codex_router_provider_helper_reads_root_name_without_table_values() {
        let home = TestHome::new();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"gateway\"\n\n[model_providers.gateway]\nbase_url = \"https://secret.example/v1\"\napi_key = \"sk-secret\"\n",
        )
        .unwrap();

        assert_eq!(
            super::codex_root_provider_for_router().as_deref(),
            Some("gateway")
        );
        assert!(
            !super::codex_root_provider_allows_native_fallback(),
            "custom root provider must block native fallback"
        );

        // A legacy `headroom` root is allowed as a migration source, but its
        // old provider table can point at a retired local port and therefore
        // must not be treated as native by the detached fail-open router.
        fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"headroom\"\n[model_providers.headroom]\nbase_url = \"http://127.0.0.1:6767/v1\"\n",
        )
        .unwrap();
        assert!(!super::codex_root_provider_allows_native_fallback());

        // Even with a native provider name, a custom root URL must remain
        // untouched rather than being sent to OpenAI by the detached helper.
        fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"openai\"\nopenai_base_url = \"https://gateway.example/v1\"\n",
        )
        .unwrap();
        assert!(!super::codex_root_provider_allows_native_fallback());

        fs::write(
            codex_dir.join("config.toml"),
            format!(
                "model_provider = \"openai\"\nopenai_base_url = \"{}\"\n",
                super::HEADROOM_OPENAI_BASE_URL
            ),
        )
        .unwrap();
        assert!(super::codex_root_provider_allows_native_fallback());
    }

    #[test]
    fn render_codex_config_does_not_duplicate_a_foreign_root_model_provider() {
        // Regression: a pre-existing root model_provider used to survive into the
        // rendered body, colliding with the managed `model_provider = "headroom_local_community"`
        // as a duplicate root key -> invalid TOML, Codex refuses to load config.
        let existing = "model_provider = \"gateway\"\n\
                        [model_providers.gateway]\n\
                        base_url = \"http://gw/v1\"\n";
        let rendered = render_codex_config(existing).expect("managed config renders");
        let parsed: toml::Value = rendered
            .parse()
            .unwrap_or_else(|e| panic!("rendered config is valid toml: {e}\n{rendered}"));
        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some("headroom_local_community"),
            "managed provider wins at root, got:\n{rendered}"
        );
        assert_eq!(
            rendered.matches("model_provider =").count(),
            1,
            "exactly one root model_provider, got:\n{rendered}"
        );
        // The user's own provider table is left untouched for the restore.
        assert!(
            rendered.contains("[model_providers.gateway]"),
            "user provider table preserved, got:\n{rendered}"
        );
    }

    #[test]
    fn render_codex_config_migrates_official_root_route_and_keeps_rollback_table() {
        let existing = "# >>> headroom:codex_cli >>>\n\
                        model_provider = \"headroom\"\n\
                        openai_base_url = \"http://127.0.0.1:6767/v1\"\n\
                        # <<< headroom:codex_cli <<<\n\
                        model = \"gpt-5.6-terra\"\n\
                        [model_providers.headroom]\n\
                        base_url = \"http://127.0.0.1:6767/v1\"\n";

        let rendered = render_codex_config(existing).expect("managed config renders");
        let parsed: toml::Value = rendered
            .parse()
            .unwrap_or_else(|e| panic!("valid toml after takeover: {e}\n{rendered}"));
        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some("headroom_local_community")
        );
        assert!(!rendered.contains("# >>> headroom:codex_cli >>>"));
        assert!(
            rendered.contains("[model_providers.headroom]")
                && rendered.contains("http://127.0.0.1:6767/v1"),
            "official provider table is retained for rollback, got:\n{rendered}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_refuses_a_custom_root_model_provider_without_mutation() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let config_toml = codex_dir.join("config.toml");
        let original = "model_provider = \"gateway\"\n\n[model_providers.gateway]\nbase_url = \"http://gw/v1\"\n";
        fs::write(&config_toml, original).unwrap();

        let error = super::apply_client_setup("codex")
            .expect_err("custom provider takeover must be refused");
        assert!(
            error
                .to_string()
                .contains("refusing to replace custom Codex model_provider")
                && error.to_string().contains("gateway"),
            "error should identify the preserved provider, got: {error:#}"
        );
        assert_eq!(
            fs::read_to_string(&config_toml).unwrap(),
            original,
            "refused takeover must leave config.toml byte-for-byte unchanged"
        );
        assert_eq!(
            fs::read_to_string(home.path().join(".zshrc")).unwrap(),
            "# user zshrc\n",
            "refused takeover must not add shell routing"
        );
        assert!(
            !super::setup_state_path().exists(),
            "refused takeover must not persist a configured-client state entry"
        );
    }

    /// Line-level diff for the rehearsal printout. Only used to make the
    /// evidence readable; the assertions are semantic.
    #[cfg(test)]
    fn rehearsal_line_diff(left: &str, right: &str) -> String {
        let left_lines: Vec<&str> = left.lines().collect();
        let right_lines: Vec<&str> = right.lines().collect();
        let mut out = String::new();
        for line in &left_lines {
            if !right_lines.contains(line) {
                out.push_str(&format!("- {line}\n"));
            }
        }
        for line in &right_lines {
            if !left_lines.contains(line) {
                out.push_str(&format!("+ {line}\n"));
            }
        }
        if out.is_empty() {
            out.push_str("(no line-level difference)\n");
        }
        out
    }

    /// Flatten parsed TOML to `path -> value`, so a round trip can be checked
    /// key by key instead of by string comparison.
    fn flatten_toml(
        value: &toml::Value,
        prefix: &str,
        out: &mut std::collections::BTreeMap<String, String>,
    ) {
        match value {
            toml::Value::Table(table) => {
                for (key, child) in table {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    flatten_toml(child, &path, out);
                }
            }
            other => {
                out.insert(prefix.to_string(), format!("{other:?}"));
            }
        }
    }

    /// Rehearse the whole Codex connector against a *copy* of a real
    /// `config.toml` that another tool owns, in a throwaway HOME.
    ///
    /// Ignored by default: it needs a real config and touches nothing of the
    /// caller's. Run it before claiming a coexistence change is safe:
    ///
    /// ```text
    /// HEADROOM_REHEARSAL_CODEX_CONFIG=~/.codex/config.toml \
    ///   cargo test --lib rehearsal_codex_round_trip -- --ignored --nocapture
    /// ```
    ///
    /// The three claims it proves, in order: verification classifies the other
    /// tool as the owner without writing anything; a confirmed takeover routes
    /// Codex through Headroom while keeping the other tool's provider table;
    /// disabling restores the file to a *semantically identical* document, so
    /// the other tool never learns Headroom was there.
    #[test]
    #[serial_test::serial]
    #[ignore = "needs HEADROOM_REHEARSAL_CODEX_CONFIG pointing at a real config.toml"]
    fn rehearsal_codex_round_trip_on_a_real_config() {
        let Ok(seed) = std::env::var("HEADROOM_REHEARSAL_CODEX_CONFIG") else {
            eprintln!("set HEADROOM_REHEARSAL_CODEX_CONFIG to run the rehearsal");
            return;
        };
        let original = fs::read_to_string(&seed).expect("rehearsal config is readable");
        let parsed_original: toml::Value = original
            .parse()
            .unwrap_or_else(|e| panic!("the seeded config is valid toml: {e}"));

        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# rehearsal zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let config = codex_dir.join("config.toml");
        fs::write(&config, &original).unwrap();

        // 1. Nothing is written just by looking, and the other tool is named as
        //    the current owner instead of Headroom reporting a broken install.
        let before = super::verify_client_setup("codex").expect("verify runs");
        eprintln!(
            "[1] foreign_provider={:?} verified={} failures={:?}",
            before.foreign_provider, before.verified, before.failures
        );
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            original,
            "verification must be read-only"
        );
        assert!(
            before
                .failures
                .iter()
                .all(|failure| !failure.contains("provider block was not found")),
            "a tool that owns the route is not a Headroom failure, got: {:?}",
            before.failures
        );

        // 2. A confirmed takeover routes Codex through Headroom and leaves the
        //    other tool's provider table (and its token) alone.
        super::apply_client_setup_with_options("codex", true).expect("confirmed takeover");
        let after_apply = fs::read_to_string(&config).unwrap();
        eprintln!("[2] takeover diff:\n{}", rehearsal_line_diff(&original, &after_apply));
        let parsed_apply: toml::Value = after_apply
            .parse()
            .unwrap_or_else(|e| panic!("valid toml after takeover: {e}\n{after_apply}"));
        assert_eq!(
            parsed_apply.get("model_provider").and_then(|v| v.as_str()),
            Some("headroom_local_community")
        );
        assert!(super::codex_provider_block_matches().unwrap());
        assert!(
            parsed_apply
                .get("model_providers")
                .and_then(|v| v.get("codex_local_access"))
                .is_some(),
            "the displaced provider's table must survive, got:\n{after_apply}"
        );
        let after = super::verify_client_setup("codex").expect("verify runs");
        assert!(
            after.verified && after.failures.is_empty(),
            "once Headroom owns the route the connector verifies, got: {:?}",
            after.failures
        );

        // 3. Disable hands the route back. Semantic equality, not string
        //    equality: the renderer may normalize blank lines, and the other
        //    tool only ever sees parsed TOML.
        super::disable_client_setup("codex").expect("disable runs");
        let restored = fs::read_to_string(&config).unwrap();
        eprintln!("[3] disable diff:\n{}", rehearsal_line_diff(&original, &restored));
        let parsed_restored: toml::Value = restored
            .parse()
            .unwrap_or_else(|e| panic!("valid toml after disable: {e}\n{restored}"));
        assert!(
            !restored.contains("headroom-local-community"),
            "no Headroom marker may survive disable, got:\n{restored}"
        );

        // The coexistence guarantee, stated as an invariant rather than as
        // "the files are equal": disable may remove Headroom's own provider
        // table, and may not add, drop, or rewrite anything else. The seeded
        // config legitimately carries a leftover Headroom table from an earlier
        // run, which is exactly what a teardown is supposed to clean up.
        let mut expected = std::collections::BTreeMap::new();
        flatten_toml(&parsed_original, "", &mut expected);
        let mut actual = std::collections::BTreeMap::new();
        flatten_toml(&parsed_restored, "", &mut actual);
        for (path, value) in &expected {
            match actual.get(path) {
                Some(kept) => assert_eq!(
                    kept, value,
                    "{path} must survive the round trip unchanged"
                ),
                None => assert!(
                    path.starts_with("model_providers.headroom_local_community"),
                    "{path} belongs to another tool and must not be dropped"
                ),
            }
        }
        for path in actual.keys() {
            assert!(
                expected.contains_key(path),
                "disable must not introduce {path}"
            );
        }
    }

    #[test]
    fn strip_codex_managed_toml_keeps_a_foreign_provider_table_wrapped_in_our_block() {
        // Another provider manager (observed: Cockpit Tools' `codex_local_access`)
        // appended its table between our markers. Tearing Headroom down must not
        // delete that tool's configuration.
        let corrupted = "model_provider = \"codex_local_access\"\n\
                         # >>> headroom-local-community:codex_cli_provider >>>\n\
                         [model_providers.headroom_local_community]\n\
                         name = \"OpenAI\"\n\
                         base_url = \"http://127.0.0.1:6891/backend-api/codex\"\n\
                         \n\
                         [model_providers.codex_local_access]\n\
                         name = \"Codex API Service\"\n\
                         base_url = \"http://localhost:52980/v1\"\n\
                         # <<< headroom-local-community:codex_cli_provider <<<\n";

        let stripped = super::strip_codex_managed_toml(corrupted);

        assert!(
            stripped.contains("[model_providers.codex_local_access]")
                && stripped.contains("http://localhost:52980/v1"),
            "the foreign provider table must survive teardown, got:\n{stripped}"
        );
        assert!(
            !stripped.contains("[model_providers.headroom_local_community]"),
            "Headroom's own table must be removed, got:\n{stripped}"
        );
        assert!(
            stripped.parse::<toml::Value>().is_ok(),
            "teardown output must stay valid toml, got:\n{stripped}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_takes_over_a_confirmed_custom_provider_and_keeps_its_table() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let config_toml = codex_dir.join("config.toml");
        fs::write(
            &config_toml,
            "model_provider = \"codex_local_access\"\n\n\
             [model_providers.codex_local_access]\n\
             name = \"Codex API Service\"\n\
             base_url = \"http://localhost:52980/v1\"\n\
             experimental_bearer_token = \"agt_secret\"\n",
        )
        .unwrap();

        assert_eq!(
            super::codex_external_provider().as_deref(),
            Some("Cockpit (codex_local_access)"),
            "the other tool's provider must be reported as the current owner"
        );

        super::apply_client_setup_with_options("codex", true)
            .expect("an explicitly confirmed takeover must succeed");

        let after = fs::read_to_string(&config_toml).unwrap();
        let parsed: toml::Value = after
            .parse()
            .unwrap_or_else(|e| panic!("valid toml after takeover: {e}\n{after}"));
        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some("headroom_local_community"),
            "confirmed takeover must route Codex through Headroom, got:\n{after}"
        );
        assert!(
            after.contains("[model_providers.codex_local_access]")
                && after.contains("http://localhost:52980/v1"),
            "the displaced provider's table must survive the takeover, got:\n{after}"
        );
        assert_eq!(
            super::load_setup_state()
                .preserved_base_urls
                .get("codex_cli")
                .map(String::as_str),
            Some("codex_local_access"),
            "the displaced provider must be recorded for restore-on-disable"
        );

        // The other half of "不影响其他的软件": switching the connector back
        // off must hand Cockpit its route back exactly as it was, otherwise a
        // user who merely tried Headroom once loses Codex to api.openai.com.
        super::disable_client_setup("codex").expect("disable restores the displaced provider");

        let restored = fs::read_to_string(&config_toml).unwrap();
        let parsed: toml::Value = restored
            .parse()
            .unwrap_or_else(|e| panic!("valid toml after disable: {e}\n{restored}"));
        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some("codex_local_access"),
            "disable must restore the displaced root provider, got:\n{restored}"
        );
        assert!(
            !restored.contains("headroom-local-community:codex_cli")
                && !restored.contains("[model_providers.headroom_local_community]"),
            "disable must remove our own blocks, got:\n{restored}"
        );
        assert!(
            restored.contains("http://localhost:52980/v1")
                && restored.contains("agt_secret"),
            "Cockpit's provider table must survive the round trip, got:\n{restored}"
        );
        assert_eq!(
            super::codex_external_provider().as_deref(),
            Some("Cockpit (codex_local_access)"),
            "after disable, Cockpit is reported as the route owner again"
        );
    }

    #[test]
    #[serial_test::serial]
    fn verify_reports_a_coexisting_provider_instead_of_a_missing_block() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"codex_local_access\"\n",
        )
        .unwrap();

        // The note and the takeover confirmation print this string verbatim.
        // A raw table key ("codex_local_access") beside the tool the user
        // installed is what made the card read as a machine error.
        let config = "model_provider = \"codex_local_access\"\n";
        assert_eq!(
            super::codex_provider_display_name("codex_local_access", config),
            "Cockpit (codex_local_access)"
        );
        assert_eq!(
            super::codex_provider_display_name("headroom", config),
            "Headroom (official)"
        );
        assert_eq!(super::codex_provider_display_name("openai", config), "OpenAI");
        assert_eq!(
            super::codex_provider_display_name("corp-gateway", config),
            "corp-gateway",
            "a hand-written provider id must not be renamed"
        );

        let verification = super::verify_client_setup("codex").expect("verification runs");

        assert_eq!(
            verification.foreign_provider.as_deref(),
            Some("Cockpit (codex_local_access)"),
            "the note names the tool, and keeps the provider id for support"
        );
        assert!(
            verification
                .failures
                .iter()
                .all(|failure| !failure.contains("provider block was not found")),
            "a coexisting provider must not read as a missing block, got: {:?}",
            verification.failures
        );
        assert!(
            !verification.verified,
            "Headroom is not intercepting, so the connector is not verified"
        );
    }

    /// Which manager owns the Codex route is a class, not a product, so naming
    /// the owner cannot depend on having heard of that product. A tool Headroom
    /// has never seen is named from the `name` it wrote into its own provider
    /// table; the id stays in the label because that is what support greps for.
    #[test]
    fn unknown_route_owners_are_named_from_their_own_provider_table() {
        let declared = "model_provider = \"acme-switch\"\n\
                        \n\
                        [model_providers.acme-switch]\n\
                        name = \"Acme Provider Switch\"\n\
                        base_url = \"http://127.0.0.1:8123/v1\"\n";
        assert_eq!(
            super::codex_provider_display_name("acme-switch", declared),
            "Acme Provider Switch (acme-switch)"
        );

        // A name we cannot print must fall back to the id rather than leak a
        // broken or runaway value into a status line.
        let empty = "model_provider = \"acme-switch\"\n\
                     \n\
                     [model_providers.acme-switch]\n\
                     name = \"\"\n";
        assert_eq!(
            super::codex_provider_display_name("acme-switch", empty),
            "acme-switch"
        );

        let runaway = format!(
            "model_provider = \"acme-switch\"\n\n[model_providers.acme-switch]\nname = \"{}\"\n",
            "x".repeat(200)
        );
        assert_eq!(
            super::codex_provider_display_name("acme-switch", &runaway),
            "acme-switch"
        );

        // The name must come from the owning table, not from a neighbour.
        let elsewhere = "model_provider = \"acme-switch\"\n\
                         \n\
                         [model_providers.other]\n\
                         name = \"Other\"\n";
        assert_eq!(
            super::codex_provider_display_name("acme-switch", elsewhere),
            "acme-switch"
        );

        // Single-quoted TOML literals are read the same way, and a tool that
        // names itself exactly like its id is not printed twice.
        let literal = "model_provider = \"acme-switch\"\n\
                       \n\
                       [model_providers.acme-switch]\n\
                       name = 'acme-switch'\n";
        assert_eq!(
            super::codex_provider_display_name("acme-switch", literal),
            "acme-switch"
        );
    }

    /// The shape found on the user's machine after Cockpit Tools took over the
    /// Codex route: Cockpit's root `model_provider`, our provider table left in
    /// place between our markers, and a single orphan *closing* marker where our
    /// root block used to be. The card reported this as "Headroom-managed
    /// provider block was not found", which reads as a broken install and was
    /// the "点了更新还是有提示" complaint. It is a coexist state: Cockpit owns the
    /// route, Headroom stands aside, and nothing of Cockpit's may be touched.
    #[test]
    #[serial_test::serial]
    fn verify_names_the_tool_that_owns_the_codex_route() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("config.toml"),
            "# <<< headroom-local-community:codex_cli <<<\n\
             notify = [\"/Applications/Codex Computer Use.app\"]\n\
             model = \"gpt-5.5\"\n\
             model_provider = \"codex_local_access\"\n\
             \n\
             [marketplaces.openai-bundled]\n\
             source_type = \"local\"\n\
             # >>> headroom-local-community:codex_cli_provider >>>\n\
             [model_providers.headroom_local_community]\n\
             name = \"OpenAI\"\n\
             base_url = \"http://127.0.0.1:6891/backend-api/codex\"\n\
             supports_websockets = false\n\
             supports_standalone_web_search = true\n\
             \n\
             [model_providers.codex_local_access]\n\
             name = \"Codex API Service\"\n\
             base_url = \"http://localhost:52980/v1\"\n\
             experimental_bearer_token = \"agt_secret\"\n\
             # <<< headroom-local-community:codex_cli_provider <<<\n",
        )
        .unwrap();
        let config = codex_dir.join("config.toml");
        let before = fs::read_to_string(&config).unwrap();

        // Our table is intact, but the root block that routes Codex is gone.
        assert!(
            !super::codex_provider_block_matches().unwrap(),
            "a foreign root provider means Headroom is not routing"
        );

        let verification = super::verify_client_setup("codex").expect("verification runs");
        assert_eq!(
            verification.foreign_provider.as_deref(),
            Some("Cockpit (codex_local_access)")
        );
        assert!(
            verification
                .failures
                .iter()
                .all(|failure| !failure.contains("provider block was not found")),
            "a coexisting tool must not be reported as a Headroom write failure, got: {:?}",
            verification.failures
        );
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            before,
            "verifying must not rewrite the other tool's route"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_refuses_a_custom_root_openai_base_url_without_mutation() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let config_toml = codex_dir.join("config.toml");
        let original =
            "model_provider = \"openai\"\nopenai_base_url = \"https://gateway.example/v1\"\n";
        fs::write(&config_toml, original).unwrap();

        let error = super::apply_client_setup("codex")
            .expect_err("custom root URL takeover must be refused");
        assert!(
            error
                .to_string()
                .contains("refusing to replace custom Codex openai_base_url"),
            "error should explain the URL boundary, got: {error:#}"
        );
        assert_eq!(
            fs::read_to_string(&config_toml).unwrap(),
            original,
            "refused takeover must leave config.toml byte-for-byte unchanged"
        );
        assert_eq!(
            fs::read_to_string(home.path().join(".zshrc")).unwrap(),
            "# user zshrc\n",
            "refused takeover must not add shell routing"
        );
        assert!(
            !super::setup_state_path().exists(),
            "refused takeover must not persist a configured-client state entry"
        );
    }

    #[test]
    #[serial_test::serial]
    fn apply_then_disable_codex_can_take_over_and_restore_official_headroom() {
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let config_toml = codex_dir.join("config.toml");
        fs::write(
            &config_toml,
            "# >>> headroom:codex_cli >>>\n\
             model_provider = \"headroom\"\n\
             openai_base_url = \"http://127.0.0.1:6767/v1\"\n\
             # <<< headroom:codex_cli <<<\n\
             [model_providers.headroom]\n\
             base_url = \"http://127.0.0.1:6767/v1\"\n",
        )
        .unwrap();

        super::apply_client_setup("codex").expect("explicit Codex takeover succeeds");
        let after_apply = fs::read_to_string(&config_toml).unwrap();
        assert_eq!(
            after_apply
                .parse::<toml::Value>()
                .unwrap()
                .get("model_provider")
                .and_then(|v| v.as_str()),
            Some("headroom_local_community")
        );

        super::disable_client_setup("codex").expect("disable restores official route");
        let after_disable = fs::read_to_string(&config_toml).unwrap();
        let parsed: toml::Value = after_disable.parse().unwrap();
        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some("headroom")
        );
        assert!(parsed
            .get("model_providers")
            .and_then(|m| m.get("headroom"))
            .is_some());
    }

    #[test]
    #[serial_test::serial]
    fn apply_codex_repairs_a_previously_corrupted_features_block() {
        // A machine upgraded mid-bug: the old single block sits at end-of-file,
        // its root keys absorbed into [features]. Re-applying must repair it so
        // the file parses and the keys resolve at root scope.
        let home = TestHome::new();
        fs::write(home.path().join(".zshrc"), "# user zshrc\n").unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let config_toml = codex_dir.join("config.toml");
        fs::write(
            &config_toml,
            "[features]\njs_repl = false\n\
             # >>> headroom-local-community:codex_cli >>>\n\
             model_provider = \"headroom_local_community\"\n\
             openai_base_url = \"http://127.0.0.1:6867/v1\"\n\n\
             [model_providers.headroom_local_community]\n\
             name = \"OpenAI\"\n\
             base_url = \"http://127.0.0.1:6867/v1\"\n\
             supports_websockets = true\n\
             # <<< headroom-local-community:codex_cli <<<\n",
        )
        .unwrap();

        // The corrupted file is invalid against Codex's schema, but still parses
        // as TOML with the key wrongly nested under [features].
        let before: toml::Value = fs::read_to_string(&config_toml).unwrap().parse().unwrap();
        assert_eq!(
            before
                .get("features")
                .and_then(|f| f.get("model_provider"))
                .and_then(|v| v.as_str()),
            Some("headroom_local_community"),
            "precondition: corruption present"
        );

        super::apply_client_setup("codex").expect("re-apply repairs config");

        let after: toml::Value = fs::read_to_string(&config_toml).unwrap().parse().unwrap();
        assert_eq!(
            after.get("model_provider").and_then(|v| v.as_str()),
            Some("headroom_local_community")
        );
        assert!(after
            .get("features")
            .and_then(|f| f.get("model_provider"))
            .is_none());
    }

    #[test]
    fn sweep_managed_backups_removes_only_community_siblings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("settings.json");
        fs::write(&target, "{}").unwrap();

        let headroom_backup = tmp
            .path()
            .join("settings.json.headroom-local-community-backup-20260101000000");
        let nommer_backup = tmp
            .path()
            .join("settings.json.nommer-backup-20250101000000");
        let unrelated = tmp.path().join("settings.json.bak");
        let other_target_backup = tmp
            .path()
            .join("config.toml.headroom-local-community-backup-20260101000000");
        fs::write(&headroom_backup, "old").unwrap();
        fs::write(&nommer_backup, "older").unwrap();
        fs::write(&unrelated, "user-owned").unwrap();
        fs::write(&other_target_backup, "different file's backup").unwrap();

        let removed = super::sweep_managed_backups(&target);

        assert_eq!(removed.len(), 1, "removed: {removed:?}");
        assert!(!headroom_backup.exists(), "headroom backup should be gone");
        assert!(
            nommer_backup.exists(),
            "legacy/paid-edition backup must be preserved"
        );
        assert!(unrelated.exists(), "unrelated .bak should survive");
        assert!(
            other_target_backup.exists(),
            "another file's backup should survive"
        );
        assert!(target.exists(), "target file itself should survive");
    }

    #[test]
    fn dedupe_shell_targets_drops_directories_keeps_files_and_missing_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let profile_dir = tmp.path().join(".profile");
        fs::create_dir(&profile_dir).unwrap();
        let zshrc = tmp.path().join(".zshrc");
        fs::write(&zshrc, "# user config\n").unwrap();
        let not_created_yet = tmp.path().join(".bash_profile");

        let kept = super::dedupe_shell_targets(vec![
            profile_dir.clone(),
            zshrc.clone(),
            not_created_yet.clone(),
            zshrc.clone(),
        ]);

        assert_eq!(kept, vec![zshrc, not_created_yet]);
        assert!(
            !kept.contains(&profile_dir),
            "a directory named .profile must never become a shell target (RUST-5X)"
        );
    }

    #[test]
    fn upsert_managed_block_never_sees_a_directory_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let profile_dir = tmp.path().join(".profile");
        fs::create_dir(&profile_dir).unwrap();

        // Ground truth for the bug: reading a directory is EISDIR, and that
        // error used to abort setup for every client.
        let err = super::upsert_managed_block(&profile_dir, "claude_code", "export FOO=1")
            .expect_err("reading a directory must fail");
        // The invariant is that it errors instead of clobbering; the OS wording
        // differs (EISDIR on Unix, "Access is denied" os error 5 on Windows).
        #[cfg(unix)]
        assert!(
            format!("{err:#}").contains("Is a directory"),
            "unexpected error: {err:#}"
        );
        let _ = err;
        assert!(super::dedupe_shell_targets(vec![profile_dir]).is_empty());
    }

    #[test]
    fn sweep_managed_backups_is_quiet_when_parent_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does-not-exist").join("settings.json");
        let removed = super::sweep_managed_backups(&missing);
        assert!(removed.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn write_setup_state_publishes_atomically() {
        let _home = TestHome::new();
        let mut state = super::ClientSetupState::default();
        state
            .configured_clients
            .insert("claude_code".into(), "2026-01-01T00:00:00+00:00".into());
        super::write_setup_state(&mut state).expect("write");

        let path = super::setup_state_path();
        assert!(path.exists(), "setup state file written");

        // No sibling .tmp* file may be left behind after a successful publish —
        // its presence would mean the rename step never happened.
        let dir = path.parent().unwrap();
        let stem = path.file_name().unwrap().to_string_lossy().into_owned();
        let leftover: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("{stem}.tmp")))
            .collect();
        assert!(
            leftover.is_empty(),
            "tmp files cleaned up by rename, got: {leftover:?}"
        );

        // Round-trip survives.
        let reloaded = super::load_setup_state();
        assert!(reloaded.configured_clients.contains_key("claude_code"));
    }

    #[test]
    fn atomic_write_concurrent_same_path_no_enoent() {
        // Regression for Sentry RUST-3W / RUST-4W: a shared `<path>.tmp` made
        // concurrent writers race — one rename consumed the tmp, the other hit
        // ENOENT. Unique per-writer tmp names must let all writers succeed.
        let dir = std::env::temp_dir().join(format!("aw_race_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = path.clone();
                std::thread::spawn(move || {
                    let body = format!("{{\"n\":{i}}}");
                    super::atomic_write(&p, body.as_bytes())
                })
            })
            .collect();
        for h in handles {
            h.join()
                .unwrap()
                .expect("concurrent atomic_write must not ENOENT");
        }
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_private_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let existing = dir.path().join("existing.json");
        std::fs::write(&existing, b"old").expect("seed file");
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o600))
            .expect("set private mode");
        super::atomic_write(&existing, b"new").expect("replace existing file");
        assert_eq!(
            std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o600,
            "replacing a credential/config file must retain its mode"
        );

        let fresh = dir.path().join("fresh.json");
        super::atomic_write(&fresh, b"new").expect("write fresh file");
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600,
            "new atomic state files must default to private permissions"
        );
    }

    #[test]
    fn atomic_write_error_names_the_io_cause() {
        // RUST-77: callers log this with `{err}`, which drops the anyhow
        // source, so Sentry only ever saw "writing <path>.tmp.N". The cause
        // must survive plain Display.
        let dir = std::env::temp_dir().join(format!("aw_cause_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Parent does not exist, so writing the tmp file fails with ENOENT.
        let path = dir.join("missing").join("state.json");
        let err = super::atomic_write(&path, b"x").expect_err("write into a missing dir must fail");
        let shown = format!("{err}");
        assert!(shown.starts_with("writing "), "{shown}");
        // Match on the "(os error N)" suffix every platform's io::Error Display
        // carries, not the message text: ENOENT reads "No such file or
        // directory" on unix but "The system cannot find the path specified."
        // on Windows, so a unix-worded assertion fails CI on Windows while the
        // cause it checks for is present.
        assert!(
            shown.contains("os error"),
            "io cause missing from `{{err}}`: {shown}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cleanup_aborts_when_rollback_copy_cannot_be_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("config.json");
        // `backup_if_exists` cannot copy a directory. This gives the cleanup
        // guard a deterministic failure without relying on process permissions.
        std::fs::create_dir(&source).expect("directory source");

        assert!(
            !super::backup_for_cleanup(&source),
            "cleanup must refuse a destructive rewrite without a backup"
        );
        assert!(
            source.is_dir(),
            "failed backup must leave the source untouched"
        );
    }

    #[test]
    fn remove_dir_all_retry_clears_readonly_instead_of_giving_up() {
        // Sentry RUST-6T: uninstall died on Windows "Access is denied (os error
        // 5)". Retrying cannot fix a read-only tree -- the error is deterministic,
        // so all 5 attempts fail identically. The rescue pass must clear the
        // read-only bits and get the delete through.
        //
        // Shaped like the real failure: a venv-ish tree with a read-only file
        // inside a read-only nested directory. On Unix the read-only DIRECTORY
        // blocks unlinking its children, so the rescue pass is load-bearing. On
        // Windows, std's remove_dir_all deletes read-only entries itself since
        // rust-lang/rust#129800 (FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE),
        // so there this only checks the helper handles a read-only tree.
        let root = std::env::temp_dir().join(format!("rdo_retry_{}", std::process::id()));
        // Via the helper, not plain remove: a read-only tree left by an earlier
        // run (recycled pid) would otherwise survive setup and break this test.
        super::remove_dir_all_retry(&root).ok();
        let nested = root.join("Lib").join("site-packages");
        std::fs::create_dir_all(&nested).unwrap();
        let locked_file = nested.join("RECORD");
        std::fs::write(&locked_file, b"x").unwrap();

        for p in [locked_file.as_path(), nested.as_path()] {
            let mut perms = std::fs::metadata(p).unwrap().permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(p, perms).unwrap();
        }
        // Precondition: a plain remove_dir_all really is blocked, else this test
        // would pass even with the rescue pass deleted. Unix-only: modern
        // Windows std ignores the read-only attribute (see header comment), so
        // no such precondition can hold there.
        #[cfg(unix)]
        assert!(
            std::fs::remove_dir_all(&root).is_err(),
            "read-only tree must block a plain remove_dir_all, or this test proves nothing"
        );

        super::remove_dir_all_retry(&root).expect("read-only tree must be removed");
        assert!(!root.exists(), "tree still present after retry helper");
    }

    #[test]
    fn remove_dir_all_retry_is_ok_on_a_missing_path() {
        let missing = std::env::temp_dir().join(format!("rdo_absent_{}", std::process::id()));
        std::fs::remove_dir_all(&missing).ok();
        assert!(super::remove_dir_all_retry(&missing).is_ok());
    }

    #[test]
    #[serial_test::serial]
    fn load_setup_state_falls_back_to_default_on_corrupt_file() {
        let _home = TestHome::new();
        let path = super::setup_state_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Simulate a torn / partial write that would have happened with the
        // pre-fix non-atomic writer. The retry path inside load_setup_state
        // re-reads after a short backoff and, when the file is still bad,
        // logs a warning and returns the default rather than panicking.
        std::fs::write(&path, b"{ not json").unwrap();

        let state = super::load_setup_state();
        assert!(state.configured_clients.is_empty());
        assert!(state.remembered_clients.is_empty());
    }

    fn seed_codex_threads_db(path: &Path, rows: &[(&str, &str)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
            [],
        )
        .unwrap();
        for (id, provider) in rows {
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES (?, ?)",
                [id, provider],
            )
            .unwrap();
        }
    }

    fn provider_count(path: &Path, provider: &str) -> i64 {
        let conn = Connection::open(path).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM threads WHERE model_provider = ?1",
            [provider],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn retag_one_codex_db_moves_only_matching_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state_5.sqlite");
        seed_codex_threads_db(
            &db,
            &[
                ("a", "openai"),
                ("b", "openai"),
                ("c", "headroom"),
                ("d", "anthropic"),
            ],
        );

        let moved = retag_one_codex_db(&db, "openai", "headroom").unwrap();
        assert_eq!(moved, Some(2));
        assert_eq!(provider_count(&db, "openai"), 0);
        assert_eq!(provider_count(&db, "headroom"), 3);
        // Third-party providers are untouched.
        assert_eq!(provider_count(&db, "anthropic"), 1);

        // Reverse direction round-trips only the headroom rows.
        let back = retag_one_codex_db(&db, "headroom", "openai").unwrap();
        assert_eq!(back, Some(3));
        assert_eq!(provider_count(&db, "headroom"), 0);
        assert_eq!(provider_count(&db, "openai"), 3);
        assert_eq!(provider_count(&db, "anthropic"), 1);
    }

    #[test]
    fn retag_one_codex_db_noop_without_threads_table() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state_5.sqlite");
        // Open creates an empty DB with no `threads` table.
        Connection::open(&db).unwrap();
        assert_eq!(retag_one_codex_db(&db, "openai", "headroom").unwrap(), None);
    }

    #[test]
    #[serial_test::serial]
    fn retag_codex_thread_providers_silent_when_no_store() {
        let _home = TestHome::new();
        // No ~/.codex stores exist under the temp home: must not panic.
        retag_codex_thread_providers("openai", "headroom");
    }

    #[test]
    #[serial_test::serial]
    fn codex_sqlite_store_expected_gates_on_state_file_not_dir() {
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        // CLI-only / pre-sqlite Codex: config + sessions but no sqlite/ store.
        std::fs::create_dir_all(codex.join("sessions")).unwrap();
        std::fs::write(codex.join("config.toml"), "").unwrap();
        assert!(
            !codex_sqlite_store_expected(),
            "config/sessions alone must not trigger the moved-store warning"
        );
        // sqlite/ dir holding only unrelated stores (logs/goals/memories) but no
        // thread store must NOT fire -- the false positive behind Sentry RUST-3R.
        std::fs::create_dir_all(codex.join("sqlite")).unwrap();
        std::fs::write(codex.join("sqlite").join("logs_2.sqlite"), "").unwrap();
        std::fs::write(codex.join("sqlite").join("goals_1.sqlite"), "").unwrap();
        assert!(
            !codex_sqlite_store_expected(),
            "unrelated sqlite stores must not trigger the moved-store warning"
        );
        // CLI store renamed loose in codex_home (version no longer parses) ->
        // expected, so the relocation gets flagged.
        std::fs::write(codex.join("state_5x.sqlite"), "").unwrap();
        assert!(codex_sqlite_store_expected());
        std::fs::remove_file(codex.join("state_5x.sqlite")).unwrap();
        // GUI thread store present under sqlite/ -> expected.
        std::fs::write(codex.join("sqlite").join("state_6.sqlite"), "").unwrap();
        assert!(codex_sqlite_store_expected());
    }

    #[test]
    #[serial_test::serial]
    fn retag_codex_threads_to_headroom_pulls_native_threads_back() {
        // Reproduces the app-update restart path: the quit handler left threads
        // tagged `openai`; launch must retag them back to `headroom`.
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        // The launch hook only relabels while Headroom owns the route.
        std::fs::write(
            codex.join("config.toml"),
            "model_provider = \"headroom_local_community\"\n",
        )
        .unwrap();
        let db = codex.join("state_5.sqlite");
        seed_codex_threads_db(&db, &[("a", "openai"), ("b", "openai"), ("c", "anthropic")]);

        reconcile_codex_thread_providers_on_launch();

        assert_eq!(provider_count(&db, "headroom_local_community"), 2);
        assert_eq!(provider_count(&db, "openai"), 0);
        // Third-party threads are untouched.
        assert_eq!(provider_count(&db, "anthropic"), 1);
    }

    #[test]
    #[serial_test::serial]
    fn codex_home_honors_env_else_default() {
        let home = TestHome::new();
        // TestHome clears CODEX_HOME, so we fall back to $HOME/.codex.
        assert_eq!(codex_home(), home.path().join(".codex"));

        let custom = home.path().join("custom-codex");
        std::env::set_var("CODEX_HOME", &custom);
        assert_eq!(codex_home(), custom);

        // An empty value is ignored (treated as unset).
        std::env::set_var("CODEX_HOME", "");
        assert_eq!(codex_home(), home.path().join(".codex"));
    }

    #[test]
    #[serial_test::serial]
    fn pin_codex_mcp_command_rewrites_only_headroom_table() {
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        let config = codex.join("config.toml");
        std::fs::write(
            &config,
            "# --- Headroom Local Community MCP server ---\n\
             [mcp_servers.headroom_local_community]\n\
             command = \"headroom\"\n\
             args = [\"mcp\", \"serve\"]\n\
             \n\
             [mcp_servers.headroom_local_community.env]\n\
             HEADROOM_PROXY_URL = \"http://127.0.0.1:6867\"\n\
             \n\
             [mcp_servers.node_repl]\n\
             command = \"/Applications/Codex.app/node_repl\"\n",
        )
        .unwrap();

        let entrypoint = home.path().join("App Support/venv/bin/headroom");
        let changed = pin_codex_mcp_command(&entrypoint).unwrap();
        assert!(changed.is_some(), "config should have been rewritten");

        let after = std::fs::read_to_string(&config).unwrap();
        let abs = entrypoint.display().to_string();
        // Compare parsed values, not raw text: TOML escapes Windows path
        // backslashes on write, so the raw file never contains `abs` verbatim.
        let parsed: toml::Value = toml::from_str(&after).expect("rewritten config parses");
        assert_eq!(
            parsed["mcp_servers"]["headroom_local_community"]["command"].as_str(),
            Some(abs.as_str()),
            "headroom command pinned to absolute path, got:\n{after}"
        );
        // The unrelated server's command must be untouched.
        assert!(after.contains("command = \"/Applications/Codex.app/node_repl\""));
        // The headroom env sub-table has no `command`; nothing spurious added.
        assert_eq!(after.matches("command = ").count(), 2);

        // Idempotent: a second run with the same entrypoint is a no-op.
        assert!(pin_codex_mcp_command(&entrypoint).unwrap().is_none());
    }

    #[test]
    #[serial_test::serial]
    fn pin_codex_mcp_command_normalizes_python_module_args() {
        // Upstream may register `<python> -m headroom.cli mcp serve`. Pinning
        // command to the console script must also rewrite the args, otherwise
        // `headroom -m headroom.cli ...` fails with "No such option '-m'".
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        let config = codex.join("config.toml");
        std::fs::write(
            &config,
            "[mcp_servers.headroom_local_community]\n\
             command = \"/somewhere/venv/bin/python3\"\n\
             args = [\"-m\", \"headroom.cli\", \"mcp\", \"serve\"]\n\
             \n\
             [mcp_servers.headroom_local_community.env]\n\
             HEADROOM_PROXY_URL = \"http://127.0.0.1:6867\"\n",
        )
        .unwrap();

        let entrypoint = home.path().join("venv/bin/headroom");
        assert!(pin_codex_mcp_command(&entrypoint).unwrap().is_some());

        let after = std::fs::read_to_string(&config).unwrap();
        assert!(
            after.contains("args = [\"mcp\", \"serve\"]"),
            "python -m args must be normalized, got:\n{after}"
        );
        assert!(!after.contains("-m"), "no -m leftovers, got:\n{after}");
    }

    #[test]
    #[serial_test::serial]
    fn pin_codex_mcp_command_handles_multi_line_args_array() {
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        let config = codex.join("config.toml");
        std::fs::write(
            &config,
            "[mcp_servers.headroom_local_community]\n\
             command = \"/somewhere/venv/bin/python3\"\n\
             args = [\n  \"-m\",\n  \"headroom.cli\",\n  \"mcp\",\n  \"serve\",\n]\n\
             \n\
             [mcp_servers.headroom_local_community.env]\n\
             HEADROOM_PROXY_URL = \"http://127.0.0.1:6867\"\n",
        )
        .unwrap();

        let entrypoint = home.path().join("venv/bin/headroom");
        assert!(pin_codex_mcp_command(&entrypoint).unwrap().is_some());

        let after = std::fs::read_to_string(&config).unwrap();
        // No orphaned continuation lines — the rebuilt file must parse.
        let parsed: toml::Value = toml::from_str(&after).expect("rebuilt config parses");
        assert_eq!(
            parsed["mcp_servers"]["headroom_local_community"]["args"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(after.contains("[mcp_servers.headroom_local_community.env]"));
        assert!(!after.contains("headroom.cli"));
    }

    #[test]
    #[serial_test::serial]
    fn discover_codex_state_dbs_finds_any_sqlite_regardless_of_name() {
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(codex.join("sqlite")).unwrap();
        // GUI store under sqlite/, CLI store at the root, plus a renamed store
        // whose name no longer follows the `state_<N>` scheme -- discovery is
        // content-based now, so it must still be picked up (the actual fix).
        std::fs::File::create(codex.join("sqlite").join("state_6.sqlite")).unwrap();
        std::fs::File::create(codex.join("state_5.sqlite")).unwrap();
        std::fs::File::create(codex.join("sqlite").join("threads.sqlite")).unwrap();
        // A non-sqlite file in the same dir must be ignored.
        std::fs::File::create(codex.join("config.toml")).unwrap();

        let names: BTreeSet<String> = discover_codex_state_dbs()
            .into_iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "state_6.sqlite".to_owned(),
                "state_5.sqlite".to_owned(),
                "threads.sqlite".to_owned(),
            ])
        );
    }

    #[test]
    #[serial_test::serial]
    fn retag_handles_unknown_store_version() {
        // Future-proofing: a Codex store-version bump (here state_99) must still
        // retag, not silently no-op for every user at once.
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(
            codex.join("config.toml"),
            "model_provider = \"headroom_local_community\"\n",
        )
        .unwrap();
        let db = codex.join("state_99.sqlite");
        seed_codex_threads_db(&db, &[("a", "openai"), ("b", "openai"), ("c", "anthropic")]);

        reconcile_codex_thread_providers_on_launch();

        assert_eq!(provider_count(&db, "headroom_local_community"), 2);
        assert_eq!(provider_count(&db, "openai"), 0);
        assert_eq!(provider_count(&db, "anthropic"), 1);
    }

    #[test]
    #[serial_test::serial]
    fn retag_handles_store_renamed_off_state_scheme() {
        // The regression this change fixes: Codex renames the store off the
        // `state_<N>.sqlite` scheme entirely. Content-based discovery must still
        // find and retag it by its `threads` table, not the filename.
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(codex.join("sqlite")).unwrap();
        std::fs::write(
            codex.join("config.toml"),
            "model_provider = \"headroom_local_community\"\n",
        )
        .unwrap();
        let db = codex.join("sqlite").join("threads.sqlite");
        seed_codex_threads_db(&db, &[("a", "openai"), ("b", "openai"), ("c", "anthropic")]);

        reconcile_codex_thread_providers_on_launch();

        assert_eq!(provider_count(&db, "headroom_local_community"), 2);
        assert_eq!(provider_count(&db, "openai"), 0);
        assert_eq!(provider_count(&db, "anthropic"), 1);
    }

    /// Live regression, 2026-09-10. Cockpit Tools owned the Codex route
    /// (`model_provider = "codex_local_access"`) and the launch hook pushed 45
    /// native threads into Headroom's provider menu anyway, because it keyed on
    /// "is the connector configured" instead of "does Headroom route Codex".
    /// A thread tag is a claim about where the thread is served; while another
    /// tool serves it, that claim is false, and its own tags
    /// (`codex_local_access`) are none of our business.
    #[test]
    #[serial_test::serial]
    fn launch_reconcile_leaves_a_foreign_routed_history_alone() {
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(
            codex.join("config.toml"),
            "model_provider = \"codex_local_access\"\n",
        )
        .unwrap();
        let db = codex.join("state_5.sqlite");
        seed_codex_threads_db(
            &db,
            &[
                ("native-a", "openai"),
                ("native-b", "openai"),
                ("ours", "headroom_local_community"),
                ("cockpit", "codex_local_access"),
            ],
        );

        reconcile_codex_thread_providers_on_launch();

        assert_eq!(
            provider_count(&db, "headroom_local_community"),
            0,
            "no thread may claim a provider Headroom is not serving"
        );
        assert_eq!(
            provider_count(&db, "openai"),
            3,
            "native threads stay native, and our leftover tag comes back"
        );
        assert_eq!(
            provider_count(&db, "codex_local_access"),
            1,
            "the routing tool's own tag is not ours to rewrite"
        );
    }

    /// The exit hook follows the same rule: with another tool routing Codex
    /// there is nothing of ours to hand back.
    #[test]
    #[serial_test::serial]
    fn thread_retag_hooks_are_gated_on_owning_the_codex_route() {
        let home = TestHome::new();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        let db = codex.join("state_5.sqlite");
        seed_codex_threads_db(&db, &[("a", "openai")]);

        // No config.toml at all: Headroom is not routing anything.
        assert!(!codex_route_is_ours());
        retag_codex_threads_to_native();
        reconcile_codex_thread_providers_on_launch();
        assert_eq!(provider_count(&db, "openai"), 1);
        assert_eq!(provider_count(&db, "headroom_local_community"), 0);

        // Route ours: launch pulls threads in, exit hands them back.
        std::fs::write(
            codex.join("config.toml"),
            "model_provider = \"headroom_local_community\"\n",
        )
        .unwrap();
        assert!(codex_route_is_ours());
        reconcile_codex_thread_providers_on_launch();
        assert_eq!(provider_count(&db, "headroom_local_community"), 1);
        retag_codex_threads_to_native();
        assert_eq!(provider_count(&db, "openai"), 1);
    }

    #[test]
    fn claude_guard_script_is_diagnostic_and_reachable_tolerates_any_response() {
        let script = build_claude_guard_script();
        // reachable() no longer flags a 503-during-bypass as "app down".
        assert!(!script.contains("return response.status < 500"));
        assert!(script.contains("except urllib.error.HTTPError:\n        return True"));
        // main() explains WHY instead of the flat "is not" message.
        assert!(script.contains("def diagnose_route"));
        assert!(script.contains("overrides Headroom's route"));
        assert!(!script.contains("ANTHROPIC_BASE_URL is not \" + BASE_URL"));
        // A correct user settings + unset process env (GUI / `open` launch) is
        // healthy and must NOT trigger the old "restart Claude Code" nag.
        assert!(!script.contains("did not inherit the Headroom shell env"));
        assert!(script.contains("user_val != BASE_URL and effective != BASE_URL"));
        // Notifications are debounced and reachability retries once, so an app
        // relaunch doesn't produce a notification storm.
        assert!(script.contains("DEBOUNCE_PATH.touch()"));
        assert!(script.contains("time.sleep(2)\n    return probe()"));
    }

    #[test]
    fn codex_guard_script_names_actual_values_and_tolerates_any_response() {
        let script = build_codex_guard_script();
        assert!(!script.contains("return response.status < 500"));
        assert!(script.contains("except urllib.error.HTTPError:\n        return True"));
        // Messages include the actual found value, not just "is not headroom".
        assert!(script.contains("(expected \"headroom_local_community\")"));
        // The expected base is selected at runtime so ChatGPT OAuth and API-key
        // sessions can use their respective stable local router paths.
        assert!(script.contains("expected = expected_base_url(config)"));
        assert!(script.contains(
            "Headroom provider base_url is \" + str(base) + \" (expected \" + expected + \")"
        ));
        assert!(script.contains("OPENAI_BASE_URL = \"http://127.0.0.1:6891/v1\""));
        assert!(script.contains("CHATGPT_BASE_URL = \"http://127.0.0.1:6891/backend-api/codex\""));
        assert!(script.contains("DEBOUNCE_PATH.touch()"));
        assert!(script.contains("time.sleep(2)\n    return probe()"));
    }

    // --- Open-source plugin coexistence -------------------------------------
    //
    // The four states from the investigation. `headroom init hook ensure` (the
    // plugin's only command) never writes ANTHROPIC_BASE_URL and never picks a
    // port, so the whole surface we own is: does a `headroom` exist on PATH for
    // the hook to run, and did we avoid touching anyone who already had one.

    fn seed_oss_plugin(home: &Path, plugin_ref: &str) -> PathBuf {
        let dir = home.join(".claude").join("plugins");
        fs::create_dir_all(&dir).unwrap();
        let install = dir
            .join("cache")
            .join(plugin_ref.replace('@', "-"))
            .join("0.36.5");
        let hooks = install.join("hooks").join("hooks.json");
        fs::create_dir_all(hooks.parent().unwrap()).unwrap();
        fs::write(
            &hooks,
            json!({
                "hooks": {
                    "SessionStart": [{
                        "hooks": [{ "type": "command", "command": super::OSS_PLUGIN_HOOK_COMMAND }]
                    }],
                    "PreToolUse": [{
                        "matcher": "Bash|PowerShell",
                        "hooks": [{ "type": "command", "command": super::OSS_PLUGIN_HOOK_COMMAND }]
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("installed_plugins.json"),
            json!({
                "version": 2,
                "plugins": { plugin_ref: [{
                    "scope": "user",
                    "version": "0.36.5",
                    "installPath": install
                }] }
            })
            .to_string(),
        )
        .unwrap();
        hooks
    }

    /// Every file under `root`, with its bytes, for before/after comparison.
    fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(bytes) = fs::read(&path) {
                    out.insert(path, bytes);
                }
            }
        }
        out
    }

    #[test]
    fn state_1_app_alone_touches_nothing_on_disk() {
        // The guarantee for the majority of users: someone running the desktop
        // app without the open-source plugin must come out of this byte-for-byte
        // unchanged. Not "no shim" — nothing at all.
        let home = TestHome::new();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude/settings.json"),
            json!({ "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:6867" } }).to_string(),
        )
        .unwrap();
        fs::create_dir_all(home.path().join(".local/bin")).unwrap();

        let before = snapshot_tree(home.path());
        let status = super::absorb_oss_plugin_with_cli_on_path(false);
        let after = snapshot_tree(home.path());

        assert_eq!(before, after, "no-plugin users must see zero writes");
        assert!(!status.plugin_installed);
        assert!(!status.hook_absorbed);
        assert!(
            status.base_url_ours,
            "our routing is left exactly as it was"
        );
    }

    #[test]
    fn state_1_app_alone_stays_inert() {
        let _home = TestHome::new();

        let status = super::absorb_oss_plugin_with_cli_on_path(false);

        assert!(!status.plugin_installed);
        assert!(!status.hook_absorbed);
        assert!(!super::oss_plugin_hook_receipt_path().exists());
    }

    #[test]
    fn oss_plugin_paths_reject_install_and_receipt_paths_outside_cache() {
        let home = TestHome::new();
        let outside_install = PathBuf::from("/tmp/headroom-untrusted-plugin");
        fs::create_dir_all(home.path().join(".claude/plugins")).unwrap();
        fs::write(
            home.path().join(".claude/plugins/installed_plugins.json"),
            json!({
                "version": 2,
                "plugins": {
                    "headroom@untrusted": [{
                        "installPath": outside_install,
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();

        assert!(super::oss_headroom_plugin_hook_paths().is_empty());
        assert!(!super::is_trusted_oss_plugin_hook_path(Path::new(
            "/tmp/headroom-untrusted-plugin/hooks/hooks.json"
        )));

        fs::create_dir_all(super::oss_plugin_hook_receipt_path().parent().unwrap()).unwrap();
        fs::write(
            super::oss_plugin_hook_receipt_path(),
            serde_json::to_vec(&[PathBuf::from(
                "/tmp/headroom-untrusted-plugin/hooks/hooks.json",
            )])
            .unwrap(),
        )
        .unwrap();
        assert!(super::load_oss_plugin_hook_receipt().is_empty());
    }

    #[test]
    fn state_2_plugin_without_cli_gets_a_noop_hook() {
        let home = TestHome::new();
        let hooks = seed_oss_plugin(home.path(), "headroom@headroom-marketplace");

        let status = super::absorb_oss_plugin_with_cli_on_path(false);

        assert!(status.plugin_installed);
        assert!(status.hook_absorbed);
        let raw = fs::read_to_string(&hooks).unwrap();
        assert_eq!(
            raw.matches(&serde_json::to_string(super::OSS_PLUGIN_MANAGED_COMMAND).unwrap())
                .count(),
            2
        );
        assert!(!raw.contains(super::OSS_PLUGIN_HOOK_COMMAND));
    }

    #[test]
    fn plugin_is_recognised_from_any_marketplace_mirror() {
        // The same plugin is listed under several marketplaces (sleetish,
        // burgebj, oll4com) whose hooks.json are byte-identical.
        for plugin_ref in ["headroom@headroom-marketplace", "headroom@sleetish"] {
            let home = TestHome::new();
            seed_oss_plugin(home.path(), plugin_ref);

            assert!(
                super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed,
                "{plugin_ref} should be recognised"
            );
        }
    }

    #[test]
    fn unrelated_plugins_do_not_trigger_the_hook_rewrite() {
        let home = TestHome::new();
        seed_oss_plugin(home.path(), "ponytail@ponytail");

        assert!(!super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);
        assert!(!super::oss_plugin_hook_receipt_path().exists());
    }

    #[test]
    fn state_3_real_cli_on_path_is_never_shadowed() {
        let home = TestHome::new();
        let hooks = seed_oss_plugin(home.path(), "headroom@headroom-marketplace");
        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);

        let status = super::absorb_oss_plugin_with_cli_on_path(true);

        assert!(status.cli_on_path);
        assert!(!status.hook_absorbed);
        assert_eq!(
            fs::read_to_string(hooks)
                .unwrap()
                .matches(super::OSS_PLUGIN_HOOK_COMMAND)
                .count(),
            2
        );
    }

    /// Regression: `state_3` proves the bool is honoured, but the bool itself
    /// came from `find_on_path` alone, and a GUI launch inherits launchd's bare
    /// PATH -- no `~/.local/bin`, which is where the OSS installer puts
    /// `headroom`. Every such user read as "no CLI" and had a working plugin
    /// hook neutralized. Asserts only the positive direction: a machine that
    /// really does have a `headroom` elsewhere cannot make this pass wrongly.
    #[test]
    #[cfg(unix)]
    fn an_oss_cli_in_local_bin_counts_even_when_path_cannot_see_it() {
        use std::os::unix::fs::PermissionsExt;

        let home = TestHome::new();
        let cli = home.path().join(".local/bin/headroom");
        fs::create_dir_all(cli.parent().unwrap()).unwrap();
        fs::write(&cli, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = fs::metadata(&cli).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cli, perms).unwrap();

        assert!(super::oss_cli_present());
    }

    /// The replacement must stay a shell builtin, not a path to anything we
    /// ship. A path goes dead the moment our app data is removed or moved,
    /// and takes the restore string -- our only way back -- with it.
    #[test]
    fn the_managed_hook_command_is_not_a_path() {
        assert_eq!(super::OSS_PLUGIN_MANAGED_COMMAND, "exit 0");
    }

    #[test]
    fn removing_the_plugin_restores_its_hook_and_clears_the_receipt() {
        let home = TestHome::new();
        let hooks = seed_oss_plugin(home.path(), "headroom@headroom-marketplace");
        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);

        seed_oss_plugin(home.path(), "ponytail@ponytail");
        let status = super::absorb_oss_plugin_with_cli_on_path(false);
        assert!(!status.plugin_installed);
        assert!(!status.hook_absorbed);
        assert!(!super::oss_plugin_hook_receipt_path().exists());
        assert_eq!(
            fs::read_to_string(&hooks)
                .unwrap()
                .matches(super::OSS_PLUGIN_HOOK_COMMAND)
                .count(),
            2
        );
    }

    #[test]
    fn a_foreign_headroom_binary_is_never_clobbered() {
        let home = TestHome::new();
        let shim = home.path().join(".local/bin/headroom");
        fs::create_dir_all(shim.parent().unwrap()).unwrap();
        fs::write(&shim, "#!/bin/sh\necho not ours\n").unwrap();
        seed_oss_plugin(home.path(), "headroom@headroom-marketplace");

        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);
        assert_eq!(
            fs::read_to_string(&shim).unwrap(),
            "#!/bin/sh\necho not ours\n"
        );
    }

    #[test]
    fn failed_restore_preserves_the_receipt_for_a_later_retry() {
        let home = TestHome::new();
        let hooks = seed_oss_plugin(home.path(), "headroom@headroom-marketplace");
        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);

        let managed = serde_json::to_string(super::OSS_PLUGIN_MANAGED_COMMAND).unwrap();
        let corrupt = format!("{} trailing", fs::read_to_string(&hooks).unwrap());
        fs::write(&hooks, corrupt).unwrap();

        super::perform_full_cleanup();

        assert!(super::app_data_dir().exists());
        assert!(super::oss_plugin_hook_receipt_path().exists());
        assert!(fs::read_to_string(hooks).unwrap().contains(&managed));
    }

    #[test]
    fn state_4_oss_proxy_bypass_is_measured_not_fought() {
        // The user ran `headroom init` themselves, so their ANTHROPIC_BASE_URL
        // points at the open-source proxy. We report it and change nothing.
        let home = TestHome::new();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        let settings = home.path().join(".claude/settings.json");
        let original = json!({ "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:8787" } });
        fs::write(&settings, original.to_string()).unwrap();
        seed_oss_plugin(home.path(), "headroom@headroom-marketplace");

        let status = super::absorb_oss_plugin_with_cli_on_path(false);

        assert!(status.plugin_installed);
        assert!(
            !status.base_url_ours,
            "the bypass must be visible to telemetry"
        );
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            original.to_string(),
            "we never rewrite a base URL the user set on purpose"
        );
    }

    /// A plugin update re-clones into a fresh version dir carrying the bare
    /// command, which one startup pass can never see. The poll must catch that
    /// and nothing else: a user we do not manage must not be dragged back
    /// through the exec probe every five minutes forever.
    #[test]
    fn the_recheck_fires_only_for_a_fresh_hook_we_are_already_managing() {
        let home = TestHome::new();
        seed_oss_plugin(home.path(), "headroom@headroom-marketplace");

        // Nobody managed yet: an untouched plugin is not the recheck's job.
        assert!(!super::oss_plugin_hook_needs_absorbing());

        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);
        assert!(
            !super::oss_plugin_hook_needs_absorbing(),
            "steady state must stay quiet"
        );

        // Claude Code updates the plugin: a new version dir, bare command back.
        let updated = seed_oss_plugin(home.path(), "headroom@headroom-marketplace-v2");
        assert!(super::oss_plugin_hook_needs_absorbing());
        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);
        assert!(!fs::read_to_string(&updated)
            .unwrap()
            .contains(super::OSS_PLUGIN_HOOK_COMMAND));
        assert!(!super::oss_plugin_hook_needs_absorbing());

        // A real CLI appears, we hand the hook back, and the poll must not
        // immediately claim it again.
        assert!(!super::absorb_oss_plugin_with_cli_on_path(true).hook_absorbed);
        assert!(
            !super::oss_plugin_hook_needs_absorbing(),
            "a restored user must not be re-absorbed on a timer"
        );
    }

    #[test]
    fn the_recheck_respects_the_kill_switch() {
        let home = TestHome::new();
        seed_oss_plugin(home.path(), "headroom@headroom-marketplace");
        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);
        seed_oss_plugin(home.path(), "headroom@headroom-marketplace-v2");
        assert!(super::oss_plugin_hook_needs_absorbing());

        std::env::set_var("HEADROOM_ABSORB_OSS_PLUGIN", "0");
        let needs = super::oss_plugin_hook_needs_absorbing();
        std::env::remove_var("HEADROOM_ABSORB_OSS_PLUGIN");

        assert!(!needs);
    }

    #[test]
    fn the_kill_switch_absorbs_nothing_and_restores_what_it_finds() {
        let home = TestHome::new();
        let hooks = seed_oss_plugin(home.path(), "headroom@headroom-marketplace");
        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);

        std::env::set_var("HEADROOM_ABSORB_OSS_PLUGIN", "0");
        let status = super::absorb_oss_plugin_with_cli_on_path(false);
        std::env::remove_var("HEADROOM_ABSORB_OSS_PLUGIN");

        assert!(status.plugin_installed);
        assert!(!status.hook_absorbed);
        assert_eq!(
            fs::read_to_string(&hooks)
                .unwrap()
                .matches(super::OSS_PLUGIN_HOOK_COMMAND)
                .count(),
            2,
            "the opt-out must hand the hook back, not just stop touching it"
        );
    }

    #[test]
    fn absorbing_the_hook_does_not_hide_a_real_oss_remnant() {
        let home = TestHome::new();
        seed_oss_plugin(home.path(), "headroom@headroom-marketplace");
        assert!(super::absorb_oss_plugin_with_cli_on_path(false).hook_absorbed);

        let foreign = home.path().join(".local/bin/headroom");
        fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        fs::write(&foreign, "#!/bin/sh\necho real OSS CLI\n").unwrap();

        assert!(
            super::detect_oss_remnants()
                .iter()
                .any(|w| w.contains("~/.local/bin/headroom")),
            "absorbing the plugin hook must not hide a real OSS CLI"
        );
    }

    #[test]
    fn codex_mcp_health_requires_ledger_ownership_and_exact_values() {
        let config: toml::Value = r#"
        [mcp_servers.context7]
        command = "npx"
        args = ["-y", "@upstash/context7-mcp@4.0.4"]

        [mcp_servers.codebase-memory]
        command = "/managed/codebase-memory-mcp"

        [mcp_servers.codebase-memory.env]
        CBM_CACHE_DIR = "/managed/cache"
    "#
        .parse()
        .unwrap();
        let mut owned = BTreeSet::from(["context7".to_string(), "codebase-memory".to_string()]);
        let health = super::codex_headroom_mcp_registration_health_for_config(
            &config,
            "@upstash/context7-mcp@4.0.4",
            Path::new("/managed/codebase-memory-mcp"),
            Path::new("/managed/cache"),
            &owned,
        );
        assert_eq!(health.context7, super::ManagedMcpRegistrationState::Healthy);
        assert_eq!(
            health.codebase_memory,
            super::ManagedMcpRegistrationState::Healthy
        );

        owned.remove("context7");
        let health = super::codex_headroom_mcp_registration_health_for_config(
            &config,
            "@upstash/context7-mcp@4.0.4",
            Path::new("/managed/codebase-memory-mcp"),
            Path::new("/managed/cache"),
            &owned,
        );
        assert_eq!(
            health.context7,
            super::ManagedMcpRegistrationState::UserManaged
        );

        owned.insert("context7".to_string());
        let health = super::codex_headroom_mcp_registration_health_for_config(
            &config,
            "@upstash/context7-mcp@4.0.5",
            Path::new("/managed/codebase-memory-mcp"),
            Path::new("/managed/cache"),
            &owned,
        );
        assert_eq!(
            health.context7,
            super::ManagedMcpRegistrationState::Mismatch
        );
        assert_eq!(
            health.codebase_memory,
            super::ManagedMcpRegistrationState::Healthy
        );
    }

    #[test]
    fn codex_mcp_health_distinguishes_missing_and_mismatch() {
        let config: toml::Value = r#"
        [mcp_servers.context7]
        command = "npx"
        args = ["-y", "@upstash/context7-mcp@4.0.3"]
    "#
        .parse()
        .unwrap();
        let owned = BTreeSet::from(["context7".to_string(), "codebase-memory".to_string()]);
        let health = super::codex_headroom_mcp_registration_health_for_config(
            &config,
            "@upstash/context7-mcp@4.0.4",
            Path::new("/managed/codebase-memory-mcp"),
            Path::new("/managed/cache"),
            &owned,
        );
        assert_eq!(
            health.context7,
            super::ManagedMcpRegistrationState::Mismatch
        );
        assert_eq!(
            health.codebase_memory,
            super::ManagedMcpRegistrationState::Missing
        );
    }

    // ---- ZCode connector ----

    /// Seed the managed entrypoint the connector requires, inside the temp
    /// test home (TestHome pins HEADROOM_DATA_DIR there).
    fn seed_zcode_entrypoint() -> PathBuf {
        let entrypoint = super::community_headroom_entrypoint();
        fs::create_dir_all(entrypoint.parent().unwrap()).expect("create venv bin dir");
        fs::write(&entrypoint, "#!/bin/sh\n").expect("seed entrypoint");
        entrypoint
    }

    fn read_zcode_config_json(home: &TestHome) -> serde_json::Value {
        serde_json::from_str(
            &fs::read_to_string(home.path().join(".zcode/cli/config.json"))
                .expect("zcode config exists"),
        )
        .expect("zcode config parses")
    }

    #[test]
    #[serial_test::serial]
    fn zcode_apply_verify_disable_round_trip() {
        let home = TestHome::new();
        seed_zcode_entrypoint();

        let result = super::apply_client_setup("zcode").expect("apply succeeds");
        assert!(result.applied);
        assert_eq!(result.client_id, "zcode");
        assert_eq!(result.summary, "Headroom MCP tools are configured in ZCode.");
        assert_eq!(result.next_steps, vec![
            "Restart ZCode and check that the Headroom MCP tools are available."
        ]);

        let config = read_zcode_config_json(&home);
        let entry = &config["mcp"]["servers"]["headroom_local_community"];
        assert_eq!(entry["args"][0], "mcp", "entry runs the mcp subcommand");
        assert_eq!(entry["args"][1], "serve");
        assert!(
            entry["command"].as_str().is_some_and(|command| !command.is_empty()),
            "entry pins an absolute command"
        );
        assert_eq!(entry["env"]["HEADROOM_TELEMETRY"], "off");
        assert!(super::zcode_mcp_entry_matches().expect("entry matches"));

        // Re-apply is a no-op: the entry is already canonical.
        let second = super::apply_client_setup("zcode").expect("second apply succeeds");
        assert!(second.already_configured, "re-apply detects no changes");

        let verification =
            super::verify_client_setup("zcode").expect("verify succeeds");
        assert!(
            verification
                .checks
                .iter()
                .any(|check| check.contains("~/.zcode/cli/config.json")),
            "verification reports the config check, got: {:?}",
            verification.checks
        );
        assert!(verification.verified, "verification passes: {verification:?}");

        super::disable_client_setup("zcode").expect("disable succeeds");
        assert!(
            !home.path().join(".zcode/cli/config.json").exists(),
            "husk file created by setup is removed on disable"
        );
        assert!(!super::zcode_mcp_entry_matches().expect("entry gone"));
    }

    #[test]
    #[serial_test::serial]
    fn zcode_setup_preserves_sibling_config_content() {
        let home = TestHome::new();
        seed_zcode_entrypoint();

        let config_path = home.path().join(".zcode/cli/config.json");
        fs::create_dir_all(config_path.parent().unwrap()).expect("create cli dir");
        fs::write(
            &config_path,
            serde_json::to_string_pretty(&json!({
                "mcp": {"servers": {"context7": {"command": "npx"}}},
                "theme": "dark"
            }))
            .unwrap(),
        )
        .expect("seed user config");

        super::apply_client_setup("zcode").expect("apply succeeds");
        let config = read_zcode_config_json(&home);
        assert_eq!(
            config["mcp"]["servers"]["context7"]["command"], "npx",
            "sibling MCP server survives"
        );
        assert_eq!(config["theme"], "dark", "sibling top-level key survives");
        assert!(
            config["mcp"]["servers"]["headroom_local_community"].is_object(),
            "Headroom entry added alongside"
        );

        super::disable_client_setup("zcode").expect("disable succeeds");
        let config = read_zcode_config_json(&home);
        assert_eq!(
            config["mcp"]["servers"]["context7"]["command"], "npx",
            "sibling MCP server still present after disable"
        );
        assert_eq!(config["theme"], "dark");
        assert!(
            config["mcp"]["servers"]["headroom_local_community"].is_null(),
            "Headroom entry removed"
        );
    }

    #[test]
    #[serial_test::serial]
    fn zcode_disable_leaves_customized_entry_alone() {
        let home = TestHome::new();
        seed_zcode_entrypoint();

        super::apply_client_setup("zcode").expect("apply succeeds");
        // The user edits the entry after enabling: disable must treat it as
        // theirs and leave it alone (same contract as the OpenCode URLs).
        let config_path = home.path().join(".zcode/cli/config.json");
        let mut config: serde_json::Value = read_zcode_config_json(&home);
        config["mcp"]["servers"]["headroom_local_community"]["env"]["HEADROOM_BEACON"] =
            json!("1");
        fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
            .expect("customize entry");

        super::disable_client_setup("zcode").expect("disable succeeds");
        let config = read_zcode_config_json(&home);
        assert_eq!(
            config["mcp"]["servers"]["headroom_local_community"]["env"]["HEADROOM_BEACON"],
            json!("1"),
            "customized entry is not touched"
        );
    }

    #[test]
    #[serial_test::serial]
    fn zcode_repins_only_existing_entry_command() {
        let home = TestHome::new();
        let entrypoint = seed_zcode_entrypoint();

        // No config file yet: a repin must not create one (a disabled
        // connector is not resurrected by a runtime reinstall).
        assert!(
            super::repin_zcode_mcp_command(&entrypoint)
                .expect("repin on absent file")
                .is_none(),
            "nothing to repin"
        );
        assert!(!home.path().join(".zcode/cli/config.json").exists());

        super::apply_client_setup("zcode").expect("apply succeeds");
        // Same command: no write at all.
        assert!(
            super::repin_zcode_mcp_command(&entrypoint)
                .expect("repin with same command")
                .is_none()
        );

        // Relocated runtime: only the command moves; args and env stay.
        let relocated = home.path().join("relocated/venv/bin/headroom");
        fs::create_dir_all(relocated.parent().unwrap()).expect("create relocated bin dir");
        fs::write(&relocated, "#!/bin/sh\n").expect("seed relocated entrypoint");
        assert!(
            super::repin_zcode_mcp_command(&relocated)
                .expect("repin after move")
                .is_some(),
            "relocation is written"
        );
        let config = read_zcode_config_json(&home);
        assert_eq!(
            config["mcp"]["servers"]["headroom_local_community"]["command"],
            serde_json::Value::String(relocated.to_string_lossy().into_owned()),
            "command repinned"
        );
        assert_eq!(
            config["mcp"]["servers"]["headroom_local_community"]["args"][1],
            "serve",
            "args untouched"
        );
        assert_eq!(
            config["mcp"]["servers"]["headroom_local_community"]["env"]["HEADROOM_TELEMETRY"],
            "off",
            "env untouched"
        );
        // A backup of the pre-repin file exists, matching the pin contract.
        let cli_dir = home.path().join(".zcode/cli");
        let has_backup = fs::read_dir(&cli_dir)
            .expect("list cli dir")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains("backup"));
        assert!(has_backup, "repin leaves a backup beside the config");
    }

    #[test]
    #[serial_test::serial]
    fn zcode_detection_ignores_config_only_installs() {
        let home = TestHome::new();
        // Only the file setup itself writes: not an install (the
        // self-fulfilling detection bug).
        let config_path = home.path().join(".zcode/cli/config.json");
        fs::create_dir_all(config_path.parent().unwrap()).expect("create cli dir");
        fs::write(&config_path, "{}\n").expect("write husk config");
        super::backup_if_exists(&config_path).expect("back up config");
        assert!(
            !super::zcode_installed_on_machine(),
            "config husk alone must not read as installed"
        );

        // Real desktop footprint: detected.
        fs::create_dir_all(home.path().join(".zcode/v2")).expect("create v2 dir");
        assert!(super::zcode_installed_on_machine());
    }

    #[test]
    #[serial_test::serial]
    fn zcode_setup_and_repin_preserve_customized_entry() {
        let home = TestHome::new();
        seed_zcode_entrypoint();
        super::apply_client_setup("zcode").expect("apply succeeds");
        let path = super::zcode_cli_config_path();
        let mut config = read_zcode_config_json(&home);
        config["mcp"]["servers"]["headroom_local_community"]["command"] = json!("custom-wrapper");
        let original = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&path, &original).unwrap();

        assert!(super::apply_client_setup("zcode").is_err());
        assert!(super::repin_zcode_mcp_command(&home.path().join("new/headroom"))
            .unwrap().is_none());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    #[serial_test::serial]
    fn zcode_backups_are_included_in_uninstall_cleanup() {
        let _home = TestHome::new();
        assert!(super::managed_backup_targets().contains(&super::zcode_cli_config_path()));
    }

    #[test]
    #[serial_test::serial]
    fn zcode_setup_does_not_shadow_agents_fallback() {
        let home = TestHome::new();
        seed_zcode_entrypoint();
        let fallback = home.path().join(".agents/mcp.json");
        fs::create_dir_all(fallback.parent().unwrap()).unwrap();
        let original = r#"{"mcpServers":{"memory":{"command":"user-memory"}}}"#;
        fs::write(&fallback, original).unwrap();
        let error = super::apply_client_setup("zcode").unwrap_err();
        assert!(error.to_string().contains("Import those MCP servers"));
        assert!(!super::zcode_cli_config_path().exists());
        assert_eq!(fs::read_to_string(&fallback).unwrap(), original);

        // Once the native config already has servers, the fallback is
        // already inactive and adding Headroom changes no existing tools.
        let native = super::zcode_cli_config_path();
        fs::create_dir_all(native.parent().unwrap()).unwrap();
        fs::write(&native, r#"{"mcp":{"servers":{"memory":{"command":"user-memory"}}}}"#).unwrap();
        super::apply_client_setup("zcode").unwrap();
        assert_eq!(read_zcode_config_json(&home)["mcp"]["servers"]["memory"]["command"], "user-memory");
        assert_eq!(fs::read_to_string(fallback).unwrap(), original);
    }

    #[test]
    #[serial_test::serial]
    fn zcode_uninstall_removes_dead_runtime_entries_but_preserves_other_servers() {
        let home = TestHome::new();
        seed_zcode_entrypoint();
        super::apply_client_setup("zcode").unwrap();
        let path = super::zcode_cli_config_path();
        let mut config = read_zcode_config_json(&home);
        config["mcp"]["servers"]["headroom_local_community"]["env"]["CUSTOM"] = json!("yes");
        config["mcp"]["servers"]["other"] = json!({"command": "user-server"});
        fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();
        super::remove_headroom_mcp_entries().unwrap();
        let cleaned = read_zcode_config_json(&home);
        assert!(cleaned["mcp"]["servers"]["headroom_local_community"].is_null());
        assert_eq!(cleaned["mcp"]["servers"]["other"], config["mcp"]["servers"]["other"]);
        assert!(super::strip_headroom_mcp_from_zcode().unwrap().is_none());
    }

    #[test]
    #[serial_test::serial]
    fn zcode_setup_refuses_missing_runtime_entrypoint() {
        let _home = TestHome::new();
        // No seeded entrypoint: setup must fail loudly instead of writing an
        // entry that points at nothing.
        let err = super::apply_client_setup("zcode").unwrap_err();
        assert!(
            format!("{err:#}").contains("entrypoint"),
            "error names the missing entrypoint: {err:#}"
        );
        assert!(
            !_home.path().join(".zcode/cli/config.json").exists(),
            "failed setup writes no config"
        );
    }
}
