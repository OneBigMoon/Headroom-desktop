use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    NotInstalled,
    Installing,
    Healthy,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedTool {
    pub id: String,
    pub name: String,
    pub description: String,
    pub runtime: String,
    pub category: String,
    #[serde(default)]
    pub workflow_group: Option<String>,
    pub activation_scope: String,
    pub required: bool,
    pub enabled: bool,
    pub status: ToolStatus,
    pub source_url: String,
    /// What is on disk when installed, falling back to the pinned version when
    /// it is not. Never report the pin for an installed addon: the card would
    /// claim a version the user does not have the moment a release bumps a pin.
    pub version: String,
    /// Version this Community build has pinned and validated for installation.
    /// It may trail the latest upstream release until compatibility and binary
    /// checksums have been reviewed in a newer Community build.
    #[serde(default)]
    pub supported_version: String,
    pub checksum: Option<String>,
    /// Short savings/usage line for the addon card chip ("12 docs converted").
    /// None when the addon has no measurable or citable figure.
    #[serde(default)]
    pub savings_label: Option<String>,
    /// Installed, but the app now pins a newer version. Drives the Update
    /// action on the card. False for addons whose upgrade is automatic (rtk,
    /// plugins) and for anything not installed.
    #[serde(default)]
    pub update_available: bool,
    /// The version an Update would move to. None when no update is pending.
    #[serde(default)]
    pub available_version: Option<String>,
    /// Why this addon cannot be installed on the current OS/arch, shown in
    /// place of an Install button that could only ever error. None when it is
    /// installable here.
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    /// User-level default mode for addons that expose a mode selector.
    /// `None` for addons without selectable modes.
    #[serde(default)]
    pub default_mode: Option<String>,
    /// Canonical modes this addon accepts. Empty for addons without a mode selector.
    #[serde(default)]
    pub supported_modes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineStageMetric {
    pub stage_id: String,
    pub stage_name: String,
    pub applied: bool,
    pub estimated_tokens_saved: u64,
    pub added_latency_ms: u64,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageOutcome {
    Success,
    Bypassed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub client: String,
    pub workspace: String,
    pub upstream_target: String,
    pub stages: Vec<PipelineStageMetric>,
    pub estimated_input_tokens: u64,
    pub estimated_output_tokens: u64,
    pub estimated_cost_savings_usd: f64,
    pub latency_ms: u64,
    pub outcome: UsageOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightCategory {
    Savings,
    Workflow,
    Health,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyInsight {
    pub id: String,
    pub category: InsightCategory,
    pub severity: InsightSeverity,
    pub title: String,
    pub recommendation: String,
    pub evidence: String,
    pub related_workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientHealth {
    Healthy,
    Attention,
    NotDetected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub configured: bool,
    pub health: ClientHealth,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchExperience {
    FirstRun,
    #[serde(alias = "resumed")]
    Resume,
    Dashboard,
}

/// Honestly-labelled output-token reduction estimate surfaced from the proxy's
/// `/stats`. `method` is "estimated" (synthetic control vs a learned baseline)
/// or "measured" (A/B holdout); the percentage carries a 95% confidence band
/// (`ci_low_percent`..`ci_high_percent`). Output savings are counterfactual, so
/// this is never presented as an exact count.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputReduction {
    pub method: String,
    pub reduction_percent: f64,
    pub ci_low_percent: f64,
    pub ci_high_percent: f64,
    pub requests: u64,
}

/// Auto-learning progress from the backend's `/stats` `traffic_learner` block.
/// Patterns need `min_evidence` sightings before they're saved, so early on
/// the learner has nothing to show; `pending_patterns` lets the Optimize view
/// prove learning is alive during that window. Backends without the block
/// (< the wheel carrying headroomlabs-ai/headroom#3104) leave this None.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnerProgress {
    pub pending_patterns: u64,
    pub min_evidence: u64,
    pub patterns_saved: u64,
}

/// Lifetime savings decomposition parsed from the backend's `/stats-history`
/// `lifetime` block. Powers the "How savings are calculated" drill-down.
/// `cache_savings_usd` is the provider cache discount earned by the *client's*
/// own prompt caching — deliberately never summed into any Headroom savings
/// figure (Headroom preserves that discount; it does not cause it).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavingsBreakdown {
    pub compression_savings_usd: f64,
    pub output_savings_usd: f64,
    /// Tool-definition tokens the proxy deferred out of the request, priced at
    /// the provider's cache-read rate rather than the full input rate: tool
    /// schemas sit at the front of the cached prefix, so what they actually
    /// cost on a repeat request is a cache read.
    pub tool_schema_savings_usd: f64,
    pub tool_schema_tokens_saved: u64,
    pub cache_savings_usd: f64,
    pub cache_read_tokens: u64,
    pub total_input_tokens: u64,
    pub total_input_cost_usd: f64,
    /// Per-model compression rates, best first. Empty on backends that predate
    /// `by_model` tracking.
    pub model_rates: Vec<ModelSavingsRate>,
}

/// One row of the backend's `/stats-history` `by_model` block.
///
/// Only the rate travels, never the dollars: `by_model` started being tracked
/// long after the lifetime counters, so its totals cover a fraction of lifetime
/// history and would visibly fail to add up next to the rows above it. A rate
/// stays meaningful on a partial sample; a dollar total does not.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelSavingsRate {
    pub model: String,
    pub requests: u64,
    pub savings_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailySavingsPoint {
    pub date: String,
    pub estimated_savings_usd: f64,
    pub estimated_tokens_saved: u64,
    pub actual_cost_usd: f64,
    pub total_tokens_sent: u64,
    /// Output-shaping savings for the bucket, kept separate from the
    /// compression figures above because it is a counterfactual estimate
    /// (synthetic control vs a learned baseline) rather than a measured diff.
    /// Zero for buckets from the local tracker, which has no output dimension.
    #[serde(default)]
    pub output_savings_usd: f64,
    #[serde(default)]
    pub output_tokens_saved: u64,
    /// Provider prompt-cache reads inside the bucket, derived from consecutive
    /// cumulative checkpoints in the backend's raw `history` array (the rollup
    /// series itself has no cache dimension). UTC-bucketed like the rollups.
    /// The local tracker archives the derived value at ingest, so coverage
    /// survives the backend's history-ring trimming. None only for buckets
    /// observed solely by the local tracker or archived before the field
    /// existed (pre-0.8.3 days are unrecoverable: their checkpoints are gone).
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    /// The provider read discount earned in the bucket, same derivation and
    /// coverage as `cache_read_tokens`. Actual read cost = this / 9.
    #[serde(default)]
    pub cache_savings_usd: Option<f64>,
    /// Locally-sampled output-shaper deltas for the bucket (poll-over-poll
    /// diffs of the estimator's durable cumulative counters, attributed to the
    /// sampling moment). None for buckets without samples: periods before this
    /// build, or while the app wasn't running. Window reduction = saved /
    /// baseline over the covered buckets.
    #[serde(default)]
    pub output_sampled_tokens_saved: Option<u64>,
    #[serde(default)]
    pub output_baseline_tokens: Option<u64>,
}

/// Per-provider (anthropic / openai / unknown) attribution for a single hourly
/// bucket, sourced from the `by_provider` map added to `/stats-history` rollups
/// upstream. Surfaced only in the hourly history-chart hover; empty for buckets
/// that predate the upstream feature (local-tracker hours before the cutoff).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSavingsPoint {
    pub provider: String,
    pub estimated_savings_usd: f64,
    pub estimated_tokens_saved: u64,
    pub actual_cost_usd: f64,
    pub total_tokens_sent: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourlySavingsPoint {
    pub hour: String,
    pub estimated_savings_usd: f64,
    pub estimated_tokens_saved: u64,
    pub actual_cost_usd: f64,
    pub total_tokens_sent: u64,
    /// See `DailySavingsPoint::output_savings_usd`.
    #[serde(default)]
    pub output_savings_usd: f64,
    #[serde(default)]
    pub output_tokens_saved: u64,
    /// See `DailySavingsPoint::cache_read_tokens`.
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub cache_savings_usd: Option<f64>,
    /// See `DailySavingsPoint::output_sampled_tokens_saved`.
    #[serde(default)]
    pub output_sampled_tokens_saved: Option<u64>,
    #[serde(default)]
    pub output_baseline_tokens: Option<u64>,
    #[serde(default)]
    pub by_provider: Vec<ProviderSavingsPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardState {
    pub app_version: String,
    pub launch_experience: LaunchExperience,
    pub bootstrap_complete: bool,
    pub python_runtime_installed: bool,
    pub lifetime_requests: usize,
    /// This process has forwarded a prompt-sized completion request (the
    /// `first_prompt_request` funnel signal). Drives the post-install
    /// checklist's "first prompt sent" row.
    pub first_prompt_request_seen: bool,
    pub lifetime_estimated_savings_usd: f64,
    pub lifetime_estimated_tokens_saved: u64,
    pub session_requests: usize,
    pub session_estimated_savings_usd: f64,
    pub session_estimated_tokens_saved: u64,
    pub session_savings_pct: f64,
    /// Counterfactual output-token reduction from the proxy's output shaper.
    /// `None` until a verbosity baseline is seeded (the dashboard hides the stat
    /// until then). Always honestly labelled (`method` + confidence band).
    pub output_reduction: Option<OutputReduction>,
    /// Auto-learning progress; `None` when the backend doesn't report it.
    #[serde(default)]
    pub learner_progress: Option<LearnerProgress>,
    /// Lifetime decomposition behind the headline savings card. `None` until
    /// the backend's `/stats-history` has been fetched at least once.
    pub savings_breakdown: Option<SavingsBreakdown>,
    pub daily_savings: Vec<DailySavingsPoint>,
    pub hourly_savings: Vec<HourlySavingsPoint>,
    /// True once native savings history has loaded at least once this process.
    /// Until then the Home chart shows a loading state instead of the sparse
    /// tracker-only layer.
    pub savings_history_loaded: bool,
    pub tools: Vec<ManagedTool>,
    pub clients: Vec<ClientStatus>,
    pub recent_usage: Vec<UsageEvent>,
    pub insights: Vec<DailyInsight>,
    /// Terms-of-Service version the app currently requires the user to accept.
    pub required_terms_version: u32,
    /// Highest terms version this user has already accepted (0 = none).
    pub accepted_terms_version: u32,
    /// Canonical Terms-of-Service URL the acceptance gate links to.
    pub terms_url: String,
}

/// Why the last bootstrap failed, in the two forms a support report needs:
/// the stable cause class (same vocabulary as the `failure_kind` Sentry tag,
/// so a mail can be matched to its issue) and the compact technical detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapFailureReport {
    pub kind: String,
    /// Pip's stderr tail, or the whole error chain when the command never ran.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapProgress {
    pub running: bool,
    pub complete: bool,
    pub failed: bool,
    pub current_step: String,
    pub message: String,
    pub current_step_eta_seconds: u64,
    pub overall_percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSetupResult {
    pub client_id: String,
    pub applied: bool,
    pub already_configured: bool,
    pub summary: String,
    pub changed_files: Vec<String>,
    pub backup_files: Vec<String>,
    pub next_steps: Vec<String>,
    pub verification: ClientSetupVerification,
    // True when a shell profile (e.g. ~/.zshrc) couldn't be written because it
    // isn't writable. Core routing still works via the client's own config, so
    // this is a soft, expected degradation -- callers use it to avoid alerting.
    #[serde(default)]
    pub shell_profile_unwritable: bool,
    /// A pre-existing custom base URL (corporate gateway/proxy) that this
    /// setup replaced. The UI must tell the user their routing changed and
    /// that the original is restored on disable.
    #[serde(default)]
    pub replaced_base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSetupVerification {
    pub client_id: String,
    pub verified: bool,
    pub proxy_reachable: bool,
    pub checks: Vec<String>,
    pub failures: Vec<String>,
    /// Set when Codex routing is owned by a different tool's provider rather
    /// than by Headroom (for example `codex_local_access` from Cockpit Tools).
    /// The UI must present this as an informational "not intercepting" state
    /// and offer an explicit takeover, never as a broken setup.
    #[serde(default)]
    pub foreign_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientConnectorStatus {
    pub client_id: String,
    pub name: String,
    pub installed: bool,
    pub enabled: bool,
    pub verified: bool,
    pub last_configured_at: Option<String>,
    #[serde(default)]
    pub restart_required: Option<bool>,
    #[serde(default)]
    /// Who owns the Codex route in `~/.codex/config.toml` right now, when that
    /// is not Headroom. Read from the file rather than from our connector
    /// state, so the panel knows about the other tool *before* the user flips
    /// the toggle on -- the toggle has to ask before it replaces that route,
    /// and the confirm copy needs the owner's name. Always `None` for non-Codex
    /// connectors.
    pub foreign_provider: Option<String>,
    #[serde(default)]
    pub verification: Option<ClientSetupVerification>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtkRuntimeStatus {
    pub installed: bool,
    /// User-facing on/off state from the tool status toggle. False means the
    /// user opted RTK out; integrations are torn down and stay off.
    pub enabled: bool,
    pub version: Option<String>,
    pub path_configured: bool,
    pub hook_configured: bool,
    pub total_commands: Option<u64>,
    pub total_saved: Option<u64>,
    pub avg_savings_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub platform: String,
    pub support_tier: String,
    pub installed: bool,
    pub running: bool,
    pub starting: bool,
    pub paused: bool,
    /// True when the watchdog auto-paused after giving up on a wedged proxy,
    /// distinct from a deliberate user pause. Drives the "stopped unexpectedly"
    /// banner + Resume button.
    pub auto_paused: bool,
    /// True when the proxy is intentionally bypassed (pricing gate on an
    /// unentitled account, or watchdog give-up). The backend is deliberately
    /// not started, so `running` will never become true — the first-run screen
    /// treats this as a terminal state and lets the user into the app.
    pub bypassed: bool,
    pub proxy_reachable: bool,
    pub headroom_pid: Option<u32>,
    pub mcp_configured: Option<bool>,
    pub mcp_error: Option<String>,
    pub ml_installed: Option<bool>,
    pub kompress_enabled: Option<bool>,
    pub headroom_learn_supported: bool,
    pub headroom_learn_disabled_reason: Option<String>,
    pub startup_error: Option<String>,
    pub startup_error_hint: Option<String>,
    pub runtime_upgrade_failure: Option<RuntimeUpgradeFailure>,
    pub rtk: RtkRuntimeStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpgradeProgress {
    pub running: bool,
    pub complete: bool,
    pub failed: bool,
    pub current_step: String,
    pub message: String,
    pub overall_percent: u8,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeFailurePhase {
    Install,
    BootValidation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpgradeFailure {
    pub app_version: String,
    pub target_headroom_version: String,
    pub fallback_headroom_version: Option<String>,
    pub failure_phase: UpgradeFailurePhase,
    pub attempts: u32,
    pub first_attempt_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
    pub error_message: String,
    pub error_hint: Option<String>,
    pub rollback_restored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeProject {
    pub id: String,
    pub project_path: String,
    pub display_name: String,
    pub last_worked_at: String,
    pub session_count: usize,
    // Count of this project's session JSONL files whose mtime falls within the
    // current UTC day. Used by the learnings tile to pick the "most active
    // today" project without rescanning session files a second time.
    pub sessions_today: usize,
    pub last_learn_ran_at: Option<String>,
    pub has_persisted_learnings: bool,
    pub active_days_since_last_learn: usize,
    pub last_learn_pattern_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadroomLearnStatus {
    pub running: bool,
    pub project_path: Option<String>,
    pub project_display_name: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub elapsed_seconds: Option<u64>,
    pub progress_percent: u8,
    pub summary: String,
    pub success: Option<bool>,
    pub error: Option<String>,
    pub last_run_at: Option<String>,
    pub output_tail: Vec<String>,
    #[serde(default)]
    pub current_step: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadroomLearnPrereqStatus {
    pub claude_cli_available: bool,
    pub claude_cli_path: Option<String>,
    pub codex_cli_available: bool,
    pub codex_cli_path: Option<String>,
    pub codex_logged_in: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformationFeedEvent {
    #[serde(default, alias = "request_id")]
    pub request_id: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, alias = "input_tokens_original")]
    pub input_tokens_original: Option<u64>,
    #[serde(default, alias = "input_tokens_optimized")]
    pub input_tokens_optimized: Option<u64>,
    #[serde(default, alias = "tokens_saved")]
    pub tokens_saved: Option<i64>,
    #[serde(default, alias = "savings_percent")]
    pub savings_percent: Option<f64>,
    #[serde(default, alias = "transforms_applied")]
    pub transforms_applied: Vec<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default, alias = "turn_id")]
    pub turn_id: Option<String>,
    // Raw request/response payload captured by the proxy's RequestLogger when
    // `log_full_messages` is enabled. Pass-through as `serde_json::Value` so
    // the exact Anthropic/OpenAI message shape (role + structured content
    // blocks) reaches the frontend unchanged; the desktop renders it, it
    // does not need to re-parse it.
    #[serde(default, alias = "request_messages")]
    pub request_messages: Option<serde_json::Value>,
    // Post-compression message list — what was actually sent upstream after
    // Headroom's pipeline ran. Present only on proxies that carry this field
    // (compressed_messages was added after request_messages was already in
    // use, so older proxies will emit `None` here).
    #[serde(default, alias = "compressed_messages")]
    pub compressed_messages: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformationFeedResponse {
    pub log_full_messages: bool,
    pub transformations: Vec<TransformationFeedEvent>,
    pub proxy_reachable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveLearning {
    pub id: String,
    pub content: String,
    pub category: String,
    pub importance: f64,
    pub evidence_count: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppliedSection {
    pub title: String,
    pub bullets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedPatterns {
    pub claude_md: Vec<AppliedSection>,
    pub memory_md: Vec<AppliedSection>,
    #[serde(default)]
    pub codex_agents_md: Vec<AppliedSection>,
    #[serde(default)]
    pub codex_instructions_md: Vec<AppliedSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtkTodayStats {
    pub date: String,
    pub saved_tokens: u64,
    pub commands: u64,
}

/// Serena activity for the feed tile. Lines are pre-formatted by the same
/// code as the Addons-tab chip (`serena_savings_parts`) so the two surfaces
/// can never phrase the same numbers differently. At least one line is Some.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerenaTodayStats {
    pub calls_line: Option<String>,
    pub tokens_line: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum RecordTag {
    Daily,
    Weekly,
    AllTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordEvent {
    pub observed_at: DateTime<Utc>,
    pub tags: Vec<RecordTag>,
    pub tokens_saved: u64,
    pub savings_percent: Option<f64>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub request_id: Option<String>,
    pub previous_record: Option<u64>,
    pub day: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    // Pass-through from the source transformation so the record card can show
    // the same "Tokens in → out" pair as the compression card.
    #[serde(default, alias = "input_tokens_original")]
    pub input_tokens_original: Option<u64>,
    #[serde(default, alias = "input_tokens_optimized")]
    pub input_tokens_optimized: Option<u64>,
    // Carried forward from the source transformation so the record row can
    // show what the record-setting compression was actually about. Populated
    // only when the proxy's `log_full_messages` is enabled. `compressed_messages`
    // is only populated by proxies that carry the field (see struct doc above).
    #[serde(default)]
    pub request_messages: Option<serde_json::Value>,
    #[serde(default)]
    pub compressed_messages: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyRecapEvent {
    pub observed_at: DateTime<Utc>,
    pub week_start: String,
    pub week_end: String,
    pub total_tokens_saved: u64,
    pub total_savings_usd: f64,
    pub active_days: u32,
}

// `serde(default)` on every field so a pre-v5 `activity-facts.json` (which
// had a different shape — `count`, `kind`) still deserializes via its default
// values; the SCHEMA_VERSION mismatch then drops the file and reinitialises
// from scratch. Without the defaults, the outer parse fails before we can
// reach the version check and the app panics at boot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct LearningsMilestoneEvent {
    #[serde(default = "default_observed_at")]
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub patterns_today: u32,
    #[serde(default)]
    pub reminders_today: u32,
    #[serde(default)]
    pub learnings_today: u32,
    #[serde(default)]
    pub project_path: Option<String>,
    #[serde(default)]
    pub project_display_name: Option<String>,
}

fn default_observed_at() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainSuggestionEvent {
    pub observed_at: DateTime<Utc>,
    pub project_path: String,
    pub project_display_name: String,
    pub session_count: u32,
    pub active_days_since_last_learn: u32,
    // "never_trained" | "stale"
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "camelCase")]
pub enum ActivityEvent {
    #[serde(rename = "transformation")]
    Transformation(TransformationFeedEvent),
    #[serde(rename = "record")]
    Record(RecordEvent),
    #[serde(rename = "weeklyRecap")]
    WeeklyRecap(WeeklyRecapEvent),
    #[serde(rename = "learningsMilestone")]
    LearningsMilestone(LearningsMilestoneEvent),
    #[serde(rename = "trainSuggestion")]
    TrainSuggestion(TrainSuggestionEvent),
}

/// One slot per tile kind. `None` renders as a placeholder on the frontend,
/// `Some(event)` renders the live row. Built from `ActivityFacts`'s latest-of-
/// kind slots — no event stream, no dedupe logic on either side of the IPC
/// boundary.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityFeedSnapshot {
    pub transformation: Option<TransformationFeedEvent>,
    pub record: Option<RecordEvent>,
    pub rtk_today: Option<RtkTodayStats>,
    pub serena_today: Option<SerenaTodayStats>,
    pub learnings_milestone: Option<LearningsMilestoneEvent>,
    pub weekly_recap: Option<WeeklyRecapEvent>,
    pub train_suggestion: Option<TrainSuggestionEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityFeedResponse {
    pub tiles: ActivityFeedSnapshot,
    pub proxy_reachable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsageWindow {
    /// 0–100 percentage consumed
    pub utilization: f64,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeExtraUsage {
    pub is_enabled: bool,
    pub monthly_limit: Option<f64>,
    pub used_credits: Option<f64>,
    /// 0–100 percentage consumed
    pub utilization: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsage {
    pub five_hour: Option<ClaudeUsageWindow>,
    pub seven_day: Option<ClaudeUsageWindow>,
    pub extra_usage: Option<ClaudeExtraUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeAuthMethod {
    ClaudeAiOauth,
    ApiKey,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudePlanTier {
    Free,
    Pro,
    Max5x,
    Max20x,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadroomSubscriptionTier {
    Pro,
    Max5x,
    Max20x,
}

impl HeadroomSubscriptionTier {
    pub fn rank(self) -> u8 {
        match self {
            HeadroomSubscriptionTier::Pro => 1,
            HeadroomSubscriptionTier::Max5x => 2,
            HeadroomSubscriptionTier::Max20x => 3,
        }
    }
}

/// The Headroom subscription tier that matches a detected Claude plan. Unknown
/// maps to Max x20 (these are paying org customers whose taxonomy we couldn't
/// decode, so pitch the top plan rather than under-recommend). Free carries no
/// paid Headroom equivalent.
pub fn headroom_tier_for_claude_plan(plan: &ClaudePlanTier) -> Option<HeadroomSubscriptionTier> {
    match plan {
        ClaudePlanTier::Pro => Some(HeadroomSubscriptionTier::Pro),
        ClaudePlanTier::Max5x => Some(HeadroomSubscriptionTier::Max5x),
        ClaudePlanTier::Max20x => Some(HeadroomSubscriptionTier::Max20x),
        // Undecodable plan: don't drive a confident upsell off a guess. Returning
        // None lets a known Codex plan supply the recommendation instead (and
        // fires no mismatch when Claude is the only signal). Honors the contract
        // in `pricing::detect_tier_mismatch`. The pricing gate's separate
        // Unknown -> Max x20 *threshold* fallback is unaffected.
        ClaudePlanTier::Unknown => None,
        ClaudePlanTier::Free => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BillingPeriod {
    Annual,
    Monthly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingGateReason {
    SignInRequired,
    WeeklyUsageLimitReached,
    CodexWeeklyUsageLimitReached,
    TrialEnded,
}

/// The OpenAI/ChatGPT plan behind a Codex session, decoded best-effort from the
/// `chatgpt_plan_type` claim in the Codex OAuth bearer JWT
/// (`proxy_intercept::decode_codex_plan_tier`). Drives the Codex upgrade nudge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexPlanTier {
    Free,
    Go,
    Plus,
    /// ChatGPT Pro Lite, the ~$100/mo mid-tier between Plus and Pro. Seen in
    /// the fleet from 2026-08 (caught by the `plan_raw` passthrough) before
    /// OpenAI announced it; any other spelling still falls through to
    /// `Unknown` and rides out as a raw claim.
    ProLite,
    Pro,
    Team,
    Business,
    SelfServeBusinessUsageBased,
    Enterprise,
    EnterpriseCbpUsageBased,
    Edu,
    Unknown,
}

impl CodexPlanTier {
    /// Parse the raw `chatgpt_plan_type` claim value.
    pub fn from_claim(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "free" => CodexPlanTier::Free,
            "go" => CodexPlanTier::Go,
            "plus" => CodexPlanTier::Plus,
            "prolite" => CodexPlanTier::ProLite,
            "pro" => CodexPlanTier::Pro,
            "team" => CodexPlanTier::Team,
            "business" => CodexPlanTier::Business,
            "self_serve_business_usage_based" => CodexPlanTier::SelfServeBusinessUsageBased,
            "enterprise" => CodexPlanTier::Enterprise,
            "enterprise_cbp_usage_based" => CodexPlanTier::EnterpriseCbpUsageBased,
            "edu" => CodexPlanTier::Edu,
            _ => CodexPlanTier::Unknown,
        }
    }

    /// Stable wire value for the `X-Headroom-Codex-Plan` header, mirroring
    /// `pricing::plan_tier_header_value` for Claude. Kept in sync with the
    /// server's `TrialIdentity::CODEX_PLAN_TIERS`.
    pub fn as_header_str(&self) -> &'static str {
        match self {
            CodexPlanTier::Free => "free",
            CodexPlanTier::Go => "go",
            CodexPlanTier::Plus => "plus",
            CodexPlanTier::ProLite => "prolite",
            CodexPlanTier::Pro => "pro",
            CodexPlanTier::Team => "team",
            CodexPlanTier::Business => "business",
            CodexPlanTier::SelfServeBusinessUsageBased => "self_serve_business_usage_based",
            CodexPlanTier::Enterprise => "enterprise",
            CodexPlanTier::EnterpriseCbpUsageBased => "enterprise_cbp_usage_based",
            CodexPlanTier::Edu => "edu",
            CodexPlanTier::Unknown => "unknown",
        }
    }
}

/// Price-parity map from an OpenAI plan to the recommended Headroom upgrade
/// tier, by per-seat Codex allowance:
/// - Go ($8) / Plus ($20) -> Pro: individual, low spend.
/// - Business / Team -> Pro: a Standard Business seat ($20-25) carries a
///   Plus-level Codex allowance, so Pro is honest parity. (Team is legacy
///   Business, folded into Business by OpenAI.) The earlier one-tier "org
///   budget" bump to Max x5 read as a wrong plan detection to Business users
///   and was reverted 2026-08-18. Note this is intentionally NOT parity with
///   Claude Team (-> Max x20): a Claude Team seat grants Max-tier limits.
///   If/when Business Premium seats ($100, 5x) ship a distinct plan claim,
///   map that claim to Max x5/x20 -- do not re-bump Standard seats.
/// - Self-serve usage-based -> Max x5: credit-billed Codex seats with no fixed
///   seat price; spend is open-ended, so pitch the middle tier.
/// - Pro ($100/$200) -> Max x20: individual already paying top dollar.
/// - Enterprise / enterprise CBP usage-based -> Max x20: $60+/seat at a 150-seat
///   minimum, the genuine high-budget tier.
/// - Edu -> Max x5: institutional but discounted.
/// - Unknown -> Max x20: plan claim couldn't be decoded, so pitch the top plan
///   rather than under-recommend.
/// Free carries no recommendation (already on the no-cost tier).
pub fn headroom_tier_for_codex_plan(plan: &CodexPlanTier) -> Option<HeadroomSubscriptionTier> {
    match plan {
        CodexPlanTier::Go | CodexPlanTier::Plus | CodexPlanTier::Team | CodexPlanTier::Business => {
            Some(HeadroomSubscriptionTier::Pro)
        }
        CodexPlanTier::ProLite
        | CodexPlanTier::SelfServeBusinessUsageBased
        | CodexPlanTier::Edu => Some(HeadroomSubscriptionTier::Max5x),
        CodexPlanTier::Pro
        | CodexPlanTier::Enterprise
        | CodexPlanTier::EnterpriseCbpUsageBased
        | CodexPlanTier::Unknown => Some(HeadroomSubscriptionTier::Max20x),
        CodexPlanTier::Free => None,
    }
}

/// Codex (OpenAI/ChatGPT) account identity, the Codex analog of
/// [`ClaudeAccountProfile`]. `plan_tier` + `account_uuid` are available from the
/// live access-token bearer the intercept proxy sees; `email` and
/// `organization_type` only exist in the on-disk `~/.codex/auth.json` id_token,
/// so they require reading that file (see `pricing::detect_codex_profile`). All
/// fields ride along to headroom-web on the `X-Headroom-Codex-*` identity
/// headers, mirroring the Claude fields one-for-one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountProfile {
    pub email: Option<String>,
    /// `chatgpt_account_id` from the OAuth JWT (or `tokens.account_id`).
    pub account_uuid: Option<String>,
    pub plan_tier: Option<CodexPlanTier>,
    /// Sanitized raw `chatgpt_plan_type` claim, kept ONLY when it doesn't
    /// decode to a known [`CodexPlanTier`]. Rides to headroom-web as the
    /// `X-Headroom-Codex-Plan` value in place of `"unknown"`, so a new OpenAI
    /// plan (e.g. Business Premium seats) shows up in the fleet by name the
    /// day it ships instead of vanishing into the `unknown` bucket.
    #[serde(default)]
    pub plan_raw: Option<String>,
    /// Raw org signal: the user's `role` in their default org
    /// (`organizations[0].role`, e.g. owner/admin/member). Present for
    /// Business/Enterprise/Team seats. No Codex analog to Claude's
    /// `organization_type` taxonomy string exists, so role is the raw value.
    pub organization_type: Option<String>,
    /// Reserved: Codex exposes no rate-limit-tier claim today.
    pub rate_limit_tier: Option<String>,
    /// Derived: `None` for free/unknown, `"subscription"` for paid personal
    /// plans, the plan string (`"business"`/`"enterprise"`) when an org seat.
    pub billing_type: Option<String>,
    /// Where `plan_tier` came from (`"id_token"`, `"access_token"`, `"none"`),
    /// for server-side auditing of sparse captures.
    pub plan_detection_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccountProfile {
    pub auth_method: ClaudeAuthMethod,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub account_uuid: Option<String>,
    pub organization_uuid: Option<String>,
    pub billing_type: Option<String>,
    pub account_created_at: Option<DateTime<Utc>>,
    pub subscription_created_at: Option<DateTime<Utc>>,
    pub has_extra_usage_enabled: bool,
    pub plan_tier: ClaudePlanTier,
    pub plan_detection_source: Option<String>,
    /// Raw `organization_type` from the OAuth profile, kept verbatim so the
    /// server can audit which taxonomy strings we haven't enumerated yet
    /// (specifically when `plan_tier` ends up `Unknown`).
    pub organization_type: Option<String>,
    /// Raw `rate_limit_tier` — same purpose as `organization_type`.
    pub rate_limit_tier: Option<String>,
    /// Raw per-user `user_rate_limit_tier`. On Team/Enterprise orgs the
    /// org-level `rate_limit_tier` (e.g. "raven") can't distinguish standard
    /// from premium seats; this field carries the seat-level limits.
    #[serde(default)]
    pub user_rate_limit_tier: Option<String>,
    /// Raw `seat_tier` — Anthropic's per-seat entitlement on Team/Enterprise
    /// orgs. Same audit purpose as `user_rate_limit_tier`.
    #[serde(default)]
    pub seat_tier: Option<String>,
    pub weekly_utilization_pct: Option<f64>,
    /// When the Claude seven-day usage window resets (RFC3339). Drives the
    /// "savings you'll miss before reset" counterfactual on the weekly gate.
    pub weekly_resets_at: Option<DateTime<Utc>>,
    pub five_hour_utilization_pct: Option<f64>,
    pub extra_usage_monthly_limit: Option<f64>,
    pub profile_fetch_error: Option<String>,
}

/// A single Codex (OpenAI) rate-limit window, sourced from the `x-codex-*`
/// response headers our intercept proxy captures off live Codex traffic
/// (`proxy_intercept::parse_codex_rate_limit_headers`). Windows are labeled by
/// minute count (e.g. "5h", "7d") derived the same way upstream does.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsageWindow {
    pub used_percent: f64,
    pub window_label: Option<String>,
    pub window_minutes: Option<i64>,
    pub seconds_until_reset: Option<i64>,
}

/// Codex subscription usage surfaced alongside the Claude profile in the pricing
/// status. Present only when the Codex connector is enabled and at least one
/// Codex response with rate-limit headers has flowed through the proxy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsage {
    pub limit_name: Option<String>,
    pub primary: Option<CodexUsageWindow>,
    pub secondary: Option<CodexUsageWindow>,
    pub credits_balance: Option<String>,
    pub credits_unlimited: bool,
    /// True while Codex optimization is permitted. Flips false once weekly
    /// (secondary-window) usage reaches the disable threshold on a free
    /// Headroom account — the Codex-only parallel to the Claude gate.
    pub optimization_allowed: bool,
    /// True once weekly usage crosses the soft nudge threshold.
    pub should_nudge: bool,
    /// Number of nudge thresholds crossed (0..=3), mirroring the Claude gate.
    pub nudge_level: u8,
    /// Set when the gate pauses Codex optimization.
    pub gate_reason: Option<PricingGateReason>,
    /// Headroom tier to recommend, derived from the detected OpenAI plan.
    pub recommended_subscription_tier: Option<HeadroomSubscriptionTier>,
    /// Utilization of the window the gate was actually evaluated against: the
    /// longest one the plan publishes, which is NOT necessarily `secondary`.
    /// Measured 2026-08-17, both free (30-day) and Plus (7-day) report their
    /// long window as `primary` with `secondary` null, so reading `secondary`
    /// here left this permanently null for most of the fleet.
    pub weekly_used_percent: Option<f64>,
    /// Seconds until that same window resets. Mirrors `claude.weekly_resets_at`
    /// so the forgone-savings upgrade copy has a horizon to multiply by.
    #[serde(default)]
    pub weekly_resets_in_seconds: Option<i64>,
    /// Display copy for the codex usage state (active / nudging / near-limit).
    pub gate_message: String,
    /// The tier-dependent nudge ladder the gate applied (10/15/20 for
    /// Max-like plans, 25/35/45 for Go/Plus). Drives notification copy so
    /// titles never hardcode a ladder the gate isn't using.
    #[serde(default)]
    pub effective_nudge_thresholds_percent: Vec<f64>,
    /// The tier-dependent pause threshold the gate applied.
    #[serde(default)]
    pub effective_disable_threshold_percent: f64,
}

/// Raw Codex rate-limit snapshot captured by the intercept proxy from the
/// `x-codex-*` response headers. Internal only (not serialized to the UI):
/// `pricing::fetch_codex_usage` reads the latest snapshot and derives the
/// display-facing `CodexUsage` (nudge state, gate copy) on the fly.
#[derive(Debug, Clone, Default)]
pub struct CodexRateLimitSnapshot {
    pub limit_name: Option<String>,
    pub primary: Option<CodexUsageWindow>,
    pub secondary: Option<CodexUsageWindow>,
    pub credits_balance: Option<String>,
    pub credits_unlimited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadroomAccountProfile {
    pub email: String,
    pub trial_started_at: Option<DateTime<Utc>>,
    pub trial_ends_at: Option<DateTime<Utc>>,
    pub trial_active: bool,
    pub subscription_active: bool,
    pub subscription_tier: Option<HeadroomSubscriptionTier>,
    pub subscription_started_at: Option<DateTime<Utc>>,
    pub subscription_renews_at: Option<DateTime<Utc>>,
    pub subscription_amount_cents: Option<i64>,
    pub subscription_billing_period: Option<String>,
    pub subscription_discount_duration: Option<String>,
    pub subscription_discount_duration_in_months: Option<i64>,
    #[serde(default)]
    pub subscription_cancel_at_period_end: bool,
    #[serde(default)]
    pub subscription_ends_at: Option<DateTime<Utc>>,
    /// What the next renewal actually bills, per billing cycle, when the server
    /// knows it better than the client can derive it from the discount fields.
    /// Only a redeemed save offer fills these in today.
    #[serde(default)]
    pub subscription_renewal_cents: Option<i64>,
    #[serde(default)]
    pub subscription_renewal_ends_at: Option<DateTime<Utc>>,
    /// A downgrade scheduled for the next cycle. The subscription keeps
    /// reporting the plan being paid for until it lands, so these fields are
    /// the only sign the change exists.
    #[serde(default)]
    pub subscription_pending_tier: Option<HeadroomSubscriptionTier>,
    #[serde(default)]
    pub subscription_pending_billing_period: Option<String>,
    #[serde(default)]
    pub subscription_pending_effective_at: Option<DateTime<Utc>>,
    pub invite_code: Option<String>,
    pub accepted_invites_count: usize,
    pub invite_bonus_percent: f64,
    // Early adopters whose earliest trial identity predates the paywall keep a
    // capped free tier instead of the post-trial hard block. serde default so
    // older cached payloads (no field) still deserialize.
    #[serde(default)]
    pub grandfathered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadroomPricingStatus {
    pub authenticated: bool,
    pub local_grace_started_at: DateTime<Utc>,
    pub local_grace_ends_at: DateTime<Utc>,
    pub local_grace_active: bool,
    pub account_sync_error: Option<String>,
    pub needs_authentication: bool,
    pub optimization_allowed: bool,
    pub should_nudge: bool,
    pub nudge_level: u8,
    pub gate_reason: Option<PricingGateReason>,
    pub gate_message: String,
    pub nudge_threshold_percent: Option<f64>,
    pub effective_nudge_thresholds_percent: Option<Vec<f64>>,
    pub disable_threshold_percent: Option<f64>,
    pub effective_disable_threshold_percent: Option<f64>,
    pub recommended_subscription_tier: Option<HeadroomSubscriptionTier>,
    pub tier_mismatch: Option<TierMismatch>,
    pub claude: ClaudeAccountProfile,
    /// Codex subscription usage, populated only when the Codex connector is
    /// enabled and the backend has captured at least one rate-limit snapshot.
    #[serde(default)]
    pub codex: Option<CodexUsage>,
    /// ChatGPT plan decoded from the Codex OAuth JWT by the intercept proxy.
    /// Populated by `get_pricing_status` only; drives the paywall tier
    /// recommendation pre-purchase (`tier_mismatch` requires an active
    /// subscription, so it can't serve that role).
    #[serde(default)]
    pub codex_plan_tier: Option<CodexPlanTier>,
    pub account: Option<HeadroomAccountProfile>,
    pub launch_discount_active: bool,
    /// Percent off applied to the currently-selling founder-pricing cohort
    /// (0 when full price). Drives the discounted prices in the upgrade view.
    #[serde(default)]
    pub active_percent_off: i64,
    /// The founder-pricing ladder (founder -> early -> standard) rendered as a
    /// scarcity stepper. Empty when the server reports no ladder.
    #[serde(default)]
    pub pricing_cohorts: Vec<PricingCohort>,
    /// Slack-style intro offer (50% off the first `duration_months` months on
    /// every plan). `None` when the server doesn't advertise one.
    #[serde(default)]
    pub intro_offer: Option<IntroOffer>,
    /// Per-month list prices in cents, keyed tier -> billing period, served by
    /// headroom-web so a price change ships without an app release. `None`
    /// from servers predating the field, and the frontend then falls back to
    /// its compiled-in table.
    #[serde(default)]
    pub plan_prices: Option<PlanPrices>,
}

/// Per-month list prices in cents: tier ("pro" | "max5x" | "max20x") ->
/// billing period ("annual" | "monthly") -> cents. Deliberately untyped keys:
/// the desktop passes the table through to the frontend verbatim, so a new
/// tier added server-side needs no Rust change.
pub type PlanPrices = std::collections::HashMap<String, std::collections::HashMap<String, i64>>;

/// Intro-offer terms surfaced by headroom-web account/config payloads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IntroOffer {
    pub active: bool,
    pub percent_off: i64,
    pub duration_months: i64,
}

/// One rung of the founder-pricing ladder, surfaced by headroom-web. `status`
/// is "sold_out" | "active" | "upcoming"; `spots_left` is set only for the
/// active, capacity-bound cohort.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingCohort {
    pub key: String,
    pub label: String,
    pub percent_off: i64,
    #[serde(default)]
    pub capacity: Option<i64>,
    pub status: String,
    #[serde(default)]
    pub spots_left: Option<i64>,
}

/// Which provider's detected plan drives a [`TierMismatch`] recommendation, so
/// the upgrade banner can name the right connector. `Both` when the Claude and
/// Codex plans imply the same recommended tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierRecommendationSource {
    Claude,
    Codex,
    Both,
}

/// Set when an active subscriber's paid Headroom tier is lower than the tier
/// implied by their detected Claude or Codex plan. `clamped` flips true once the
/// grace window has elapsed, at which point standard paid-plan usage gating
/// applies — scoped per product via the `*_undercovered` flags: a Codex-only
/// mismatch must never pause Claude optimization (and vice versa), since the
/// other product is exactly what the user is paying for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TierMismatch {
    pub paid_tier: HeadroomSubscriptionTier,
    pub recommended_tier: HeadroomSubscriptionTier,
    pub recommended_source: TierRecommendationSource,
    pub grace_ends_at: DateTime<Utc>,
    pub clamped: bool,
    /// True when the Claude-implied tier alone exceeds the paid tier; only then
    /// may the clamp gate Claude traffic.
    #[serde(default)]
    pub claude_undercovered: bool,
    /// True when the Codex-implied tier alone exceeds the paid tier; only then
    /// may the clamp meter Codex traffic.
    #[serde(default)]
    pub codex_undercovered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadroomAuthCodeRequest {
    pub email: String,
    pub expires_in_seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_plan_tier_round_trips_as_snake_case() {
        for (tier, wire) in [
            (CodexPlanTier::Free, "free"),
            (CodexPlanTier::Plus, "plus"),
            (CodexPlanTier::Enterprise, "enterprise"),
            (CodexPlanTier::Unknown, "unknown"),
        ] {
            assert_eq!(serde_json::to_value(tier).unwrap(), json!(wire));
            let parsed: CodexPlanTier = serde_json::from_value(json!(wire)).unwrap();
            assert_eq!(parsed, tier);
        }
    }

    #[test]
    fn codex_plan_tier_from_claim_is_trimmed_case_insensitive_with_unknown_fallback() {
        assert_eq!(CodexPlanTier::from_claim("Plus"), CodexPlanTier::Plus);
        assert_eq!(CodexPlanTier::from_claim("  TEAM "), CodexPlanTier::Team);
        // ChatGPT Pro Lite: the claim OpenAI mints for the ~$100/mo mid-tier.
        assert_eq!(CodexPlanTier::from_claim("prolite"), CodexPlanTier::ProLite);
        assert_eq!(CodexPlanTier::ProLite.as_header_str(), "prolite");
        assert_eq!(
            CodexPlanTier::from_claim("chatgptpaidplan"),
            CodexPlanTier::Unknown
        );
        assert_eq!(CodexPlanTier::from_claim(""), CodexPlanTier::Unknown);
    }

    #[test]
    fn codex_plan_maps_to_price_parity_headroom_tier() {
        use HeadroomSubscriptionTier::*;
        // Business/Team: Standard seats carry a Plus-level Codex allowance, so
        // they price-match Pro (reverted from the Max x5 org bump 2026-08-18).
        for plan in [
            CodexPlanTier::Go,
            CodexPlanTier::Plus,
            CodexPlanTier::Team,
            CodexPlanTier::Business,
        ] {
            assert_eq!(headroom_tier_for_codex_plan(&plan), Some(Pro));
        }
        for plan in [
            CodexPlanTier::Pro,
            CodexPlanTier::Enterprise,
            CodexPlanTier::EnterpriseCbpUsageBased,
        ] {
            assert_eq!(headroom_tier_for_codex_plan(&plan), Some(Max20x));
        }
        for plan in [
            CodexPlanTier::ProLite,
            CodexPlanTier::SelfServeBusinessUsageBased,
            CodexPlanTier::Edu,
        ] {
            assert_eq!(headroom_tier_for_codex_plan(&plan), Some(Max5x));
        }
        assert_eq!(headroom_tier_for_codex_plan(&CodexPlanTier::Free), None);
        assert_eq!(
            headroom_tier_for_codex_plan(&CodexPlanTier::Unknown),
            Some(Max20x)
        );
    }

    #[test]
    fn codex_usage_window_deserializes_camel_case_keys() {
        let parsed: CodexUsageWindow = serde_json::from_value(json!({
            "usedPercent": 42.5,
            "windowLabel": "7d",
            "windowMinutes": 10080,
            "secondsUntilReset": 3600,
        }))
        .unwrap();
        assert_eq!(parsed.used_percent, 42.5);
        assert_eq!(parsed.window_label.as_deref(), Some("7d"));
        assert_eq!(parsed.window_minutes, Some(10080));
        assert_eq!(parsed.seconds_until_reset, Some(3600));
    }

    #[test]
    fn codex_usage_serializes_camel_case_and_round_trips() {
        let usage = CodexUsage {
            limit_name: Some("codex".into()),
            secondary: Some(CodexUsageWindow {
                used_percent: 80.0,
                window_label: Some("7d".into()),
                window_minutes: Some(10080),
                seconds_until_reset: None,
            }),
            optimization_allowed: true,
            should_nudge: true,
            nudge_level: 2,
            weekly_used_percent: Some(80.0),
            gate_message: "Approaching weekly limit".into(),
            ..Default::default()
        };

        let value = serde_json::to_value(&usage).unwrap();
        // Wire contract for the TS frontend: camelCase keys, not snake_case.
        for key in [
            "optimizationAllowed",
            "shouldNudge",
            "nudgeLevel",
            "gateMessage",
        ] {
            assert!(value.get(key).is_some(), "missing camelCase key {key}");
        }
        assert!(value.get("optimization_allowed").is_none());

        let back: CodexUsage = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), value);
        assert_eq!(back.nudge_level, 2);
        assert_eq!(back.secondary.unwrap().used_percent, 80.0);
        assert!(back.primary.is_none());
    }

    #[test]
    fn pricing_cohort_defaults_optional_capacity_fields_when_absent() {
        // headroom-web omits capacity/spotsLeft for sold-out and upcoming rungs;
        // the #[serde(default)] contract must keep those payloads deserializable.
        let cohort: PricingCohort = serde_json::from_value(json!({
            "key": "founder",
            "label": "Founder",
            "percentOff": 40,
            "status": "active",
        }))
        .unwrap();
        assert_eq!(cohort.percent_off, 40);
        assert_eq!(cohort.status, "active");
        assert_eq!(cohort.capacity, None);
        assert_eq!(cohort.spots_left, None);
    }
}
