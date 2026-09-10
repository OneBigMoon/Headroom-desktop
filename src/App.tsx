import {
  useEffect,
  useRef,
  useState,
  type ElementType,
  type FormEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent,
  type ReactElement,
  type ReactNode
} from "react";
import {
  ArrowClockwise,
  Bell,
  Brain,
  CaretLeft,
  Cpu,
  CurrencyCircleDollar,
  CurrencyDollar,
  Info,
  EnvelopeSimple,
  GearSix,
  House,
  Key,
  PuzzlePiece,
  SignOut,
  Sliders,
  Sparkle,
  Terminal,
} from "@phosphor-icons/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  platformPreviewNoticeFor,
  platformPreviewSupportIssueUrl,
} from "./lib/platform";
import {
  Bar,
  BarChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis
} from "recharts";
import headroomAvatar from "./assets/headroom-rage-avatar.png";
import packageJson from "../package.json";
import { LOCAL_COMMUNITY_EDITION } from "./lib/localEdition";
import {
  clearAddonOperationMessage,
  setAddonOperationMessage,
  type AddonOperationMessages,
} from "./lib/addonOperationState";
import {
  applyAddonUpdateChecks,
  type AddonUpdateCheck,
} from "./lib/addonUpdates";
import { localeOptions, useI18n, type Locale, type Translate, type TranslationKey } from "./lib/i18n";
import { RuntimeStatusIndicator } from "./components/RuntimeStatusIndicator";
import {
  getActivationScopeCopy,
  groupToolsByCategory,
  toolCopy,
  toolCategoryCopy,
  TOOL_CATEGORY_ORDER,
  workflowGroupCopy,
} from "./lib/workflowCatalog";
import {
  formatAppUpdateProgressCopy,
  getAppUpdateInstallStatusCopy,
  getBlockedAppUpdateCheckPatch,
  loadAppUpdateConfiguration,
  runAppUpdateCheck,
  runAppUpdateInstall,
  sendAppUpdateNotification,
  shouldNotifyAboutAvailableAppUpdate,
  maybeFireStaleAppUpdateNotification,
  type AppUpdateStatePatch,
} from "./lib/appUpdate";
import { maybeFireTrialNotifications } from "./lib/trialNotifications";
import {
  fireUpsellNudge,
  maybeFireUrgentPricingNotifications,
  maybeFireUrgentRuntimeNotification,
} from "./lib/urgentNotifications";
import {
  maybeFireSetupStallAlert,
  setupStallBannerLine,
  SETUP_STALL_CHECK_INTERVAL_MS,
  SETUP_STALL_EARLIEST_MS,
  type SetupStallAlert,
  type SetupStallKind,
} from "./lib/setupHealthAlert";
import { SetupStallModal } from "./components/SetupStallModal";
import {
  buildInstallFailureMailto,
  buildSetupStallMailto,
  describeInvokeError,
  formatCents,
  getNextHigherUpgradePlanId,
  getNextLowerUpgradePlanId,
  getPlanRenewalPriceLabel,
  getUpgradePlans,
  type UpgradePlan,
  introSaleBadgeLabel,
  isTierDowngrade,
  matchesSubscriptionPeriod,
  forgoneSavingsLabel,
  paybackLabel,
  recentDailySavingsUsd,
  setServerPlanPrices,
  tierRecommendationSourceLabel,
  scheduledPlanChange,
  upgradePlanIntentLabel,
  type BillingPeriod,
  type PricingAudience,
  type UpgradePlanId
} from "./lib/appHelpers";
import {
  bootstrapFailureSignature,
  buildBootstrapFailureReport,
  buildBootstrapInvokeFailureReport,
  reportBootstrapFailure
} from "./lib/bootstrapSentry";
import {
  aggregateClientConnectors,
  addDays,
  addMonths,
  buildHourlySavingsChartData,
  buildHourlySavingsWindow,
  buildMonthlySavingsChartData,
  buildMonthlySavingsWindow,
  compressibleInputSavingsRate,
  allTimeCacheHitPair,
  cacheHitPair,
  outputReductionForWindow,
  compactNumber,
  connectorDashboardStatus,
  connectorStatusLine,
  currency,
  currencyExact,
  dayOfMonthTickFormatter,
  earliestHourlyDay,
  earliestSavingsMonth,
  formatDateTime,
  formatDayKey,
  formatLearnStatus,
  formatMonthLabel,
  formatSelectedDayLabel,
  getEnabledSupportedConnectors,
  hasEnabledConnector,
  hasNeverScanned,
  hourOfDayTickFormatter,
  mergeProviderSavingsForDisplay,
  percent1,
  shouldShowConnectorDetectionWarning,
  sortClientConnectors,
  startOfDay,
  startOfMonth,
  type SavingsChartDatum
} from "./lib/dashboardHelpers";
import {
  buildInitialProxyVerificationRows,
  formatConnectorNameList,
  getClaudeConnector,
  getContactRequestValidationError,
  getInitialLauncherStage,
  magicLinkScreenCopy,
  getLauncherAutoConfigureDecision,
  isValidEmailAddress,
  needsTermsAcceptance,
  nextAutoConfigureStep,
  nextAutoConfigureStepAfterApply,
  recommendedHeadroomTier,
  type LauncherStage,
  type MagicLinkState,
  type InstallWizardStep
} from "./lib/launcherHelpers";
import { mockDashboard } from "./lib/mockData";
import {
  cachePricingStatus,
  type CachedPricing,
  formatRemainingDays,
  readCachedPricing,
  subscriptionTierLabel,
  writeCachedPricing
} from "./lib/pricing";
import {
  activityFeedSignature,
  notificationActionView,
  serializeState,
  type TrayView
} from "./lib/trayHelpers";
import { trackAnalyticsEvent, trackInstallMilestoneOnce } from "./lib/analytics";
import { ActivityFeed } from "./components/ActivityFeed";
import { AuthCodeForm } from "./components/AuthCodeForm";
import { ConnectorIcon, hasConnectorIcon } from "./components/ConnectorIcon";
import { LauncherShell } from "./components/LauncherShell";
import { LearnScanStatusLine } from "./components/LearnScanStatusLine";
import { OptimizePanel } from "./components/OptimizePanel";
import { ProjectIssuesLink } from "./components/ProjectIssuesLink";
import { TermsGate } from "./components/TermsGate";
import { ToolModeSelector, localizedToolModeLabel } from "./components/ToolModeSelector";
import { WindowChrome } from "./components/WindowChrome";
import { AddonPresetBar, RECOMMENDED_ADDON_PRESET } from "./components/AddonPresetBar";
import type {
  AppUpdateConfiguration,
  AvailableAppUpdate,
  BootstrapFailureReport,
  BootstrapProgress,
  ClaudePlanTier,
  HeadroomAuthCodeRequest,
  HeadroomPricingStatus,
  LaunchFlags,
  ClaudeCodeProject,
  ClientConnectorStatus,
  ClientSetupResult,
  DailySavingsPoint,
  DashboardState,
  DebugOverrides,
  HeadroomLearnPrereqStatus,
  HeadroomLearnStatus,
  HeadroomSubscriptionTier,
  ActivityFeedResponse,
  AppliedPatterns,
  HourlySavingsPoint,
  OutputReduction,
  RuntimeStatus,
  RuntimeUpgradeFailure,
  RuntimeUpgradeProgress,
  SaveOffer,
} from "./lib/types";

interface NavItem {
  id: TrayView;
  labelKey: TranslationKey;
  icon: ElementType;
}

export function recommendedAddonPresetMatches(dashboard: DashboardState): boolean {
  const matches = Object.entries(RECOMMENDED_ADDON_PRESET).every(([id, target]) => {
    const tool = dashboard.tools.find((candidate) => candidate.id === id);
    if (!tool) return false;
    if (target.enabled !== (tool.status !== "not_installed" && tool.enabled)) return false;
    return !target.mode || target.mode === tool.defaultMode;
  });
  return matches && dashboard.tools.every((tool) => tool.required || RECOMMENDED_ADDON_PRESET[tool.id] || !tool.enabled);
}

const navItems: NavItem[] = [
  { id: "home", labelKey: "nav.overview", icon: House },
  { id: "optimization", labelKey: "nav.optimize", icon: Sliders },
  { id: "notifications", labelKey: "nav.activity", icon: Bell },
  { id: "addons", labelKey: "nav.tools", icon: PuzzlePiece },
];

interface AddonCopy {
  whatItDoes: string;
  installing?: string;
  uninstalling?: string;
  installed?: string;
  uninstalled?: string;
  enabling?: string;
  disabling?: string;
  disabled?: string;
}

const addonCopy: Record<string, AddonCopy> = {
  rtk: {
    whatItDoes:
      "Installing downloads the RTK binary into Headroom's managed runtime, adds it to your shell PATH, and turns on the bash auto-rewrite hook. Shell commands your agent runs are routed through RTK, which compacts their output so it costs far fewer tokens. Removed cleanly when you uninstall it or Headroom.",
    installing: "Downloading RTK and registering the bash hook...",
    uninstalling: "Removing RTK, its PATH entry, and the bash hook...",
    uninstalled: "RTK removed. Shell commands run normally, without output rewriting.",
    enabling: "Enabling RTK and registering the bash hook...",
    disabling: "Disabling RTK and removing the bash hook...",
    disabled: "RTK is off but still installed. Re-enable any time without re-downloading."
  },
  markitdown: {
    whatItDoes:
      "Installing adds the MarkItDown converter to Headroom's managed Python runtime and registers a document Read hook. Nothing is installed system-wide - it all lives under Headroom's app data and is removed when you uninstall Headroom.",
    installing: "Installing MarkItDown and registering the Read hook...",
    uninstalling: "Removing MarkItDown and its Read hook...",
    uninstalled: "MarkItDown removed. Your agent reads documents in their original format again.",
    enabling: "Enabling MarkItDown...",
    disabling: "Disabling MarkItDown...",
    disabled: "MarkItDown is off. It stays installed but no longer converts documents."
  },
  ponytail: {
    whatItDoes:
      "Installing registers the Ponytail marketplace and plugin in Claude Code and/or Codex (whichever CLIs are on your PATH). It nudges the agent to write the least code possible. Removed from the plugin registry when you uninstall it or Headroom.",
    installing: "Registering the Ponytail plugin with your agent...",
    uninstalling: "Removing the Ponytail plugin...",
    uninstalled: "Ponytail removed. Your agent writes code without the Ponytail nudge.",
    installed: "Ponytail installed. Run /ponytail-audit in your agent to scan this codebase for over-engineering.",
    enabling: "Enabling Ponytail...",
    disabling: "Disabling Ponytail...",
    disabled: "Ponytail is off. It stays installed but no longer nudges the agent."
  },
  caveman: {
    whatItDoes:
      "Installing registers the Caveman marketplace and plugin in Claude Code and/or Codex (whichever CLIs are on your PATH). It makes the agent reply in terse caveman-speak, cutting output tokens while keeping code, commands, and errors exact. Removed from the plugin registry when you uninstall it or Headroom.",
    installing: "Registering the Caveman plugin with your agent...",
    uninstalling: "Removing the Caveman plugin...",
    uninstalled: "Caveman removed. Your agent speaks in full sentences again.",
    installed: "Caveman installed. Run /caveman-stats in your agent to see session token savings.",
    enabling: "Enabling Caveman...",
    disabling: "Disabling Caveman...",
    disabled: "Caveman is off. It stays installed but no longer compresses replies."
  },
  allinluna: {
    whatItDoes:
      "Installing registers the All in Luna marketplace and plugin in Codex. It runs the plugin's bundled local Python runtime and may coordinate workspace files, Git, and shell commands through Codex. Nothing is installed into Claude Code.",
    installing: "Registering the All in Luna plugin with Codex...",
    uninstalling: "Removing the All in Luna plugin from Codex...",
    uninstalled: "All in Luna removed from Codex.",
    installed: "All in Luna installed. Run /allinluna in Codex to start orchestration.",
    enabling: "Enabling All in Luna in Codex...",
    disabling: "Disabling All in Luna in Codex...",
    disabled: "All in Luna is off. Re-enable it when you want Codex orchestration."
  },
  serena: {
    whatItDoes:
      "Installing sets up Serena in Headroom's managed runtime and registers it as an MCP server in Claude Code and Codex. Your agent gets symbol-level code tools - find a definition, read just that function, edit in place - instead of reading whole files. Its tool definitions add some tokens to every request, so the net saving is largest in bigger codebases. A serena MCP entry you configured yourself is never touched, and everything is removed cleanly when you uninstall it or Headroom.",
    installing: "Installing Serena and registering its MCP server...",
    installed: "Serena installed. Restart open agent sessions to pick up the new MCP server.",
    uninstalling: "Removing Serena and its MCP registrations...",
    uninstalled: "Serena removed. Your agent reads code as whole files again.",
    enabling: "Re-registering the Serena MCP server...",
    disabling: "Removing the Serena MCP registrations...",
    disabled: "Serena is off but still installed. Re-enable any time without re-downloading."
  },
  "codebase-memory": {
    whatItDoes:
      "Installing downloads the codebase-memory binary into Headroom's managed runtime, verifies it, and registers it as an MCP server in Claude Code and Codex. It indexes a repo into a persistent knowledge graph - functions, classes, call chains - so your agent answers structure questions from the graph instead of re-reading files. Ask your agent to index a repo the first time you use it there. Indexes are stored inside Headroom's app data, a codebase-memory MCP entry you configured yourself is never touched, and everything is removed cleanly when you uninstall it or Headroom.",
    installing: "Downloading Codebase Memory and registering its MCP server...",
    installed: "Codebase Memory installed. Restart open agent sessions, then ask your agent to index the repo.",
    uninstalling: "Removing Codebase Memory, its indexes, and its MCP registrations...",
    uninstalled: "Codebase Memory removed. Your agent explores code by reading files again.",
    enabling: "Re-registering the Codebase Memory MCP server...",
    disabling: "Removing the Codebase Memory MCP registrations...",
    disabled: "Codebase Memory is off but still installed. Re-enable any time without re-downloading."
  },
  context7: {
    whatItDoes:
      "Installing verifies the Context7 MCP server runs via npx, then registers it in Claude Code and Codex. Your agent can pull current, version-specific documentation for the libraries you use instead of guessing APIs from stale training data - docs are fetched only when it asks, so the idle cost is just its tool definitions. A context7 MCP entry you configured yourself is never touched, and the registration is removed cleanly when you uninstall it or Headroom. Requires Node.js on PATH.",
    installing: "Verifying Context7 with npx and registering its MCP server...",
    installed: "Context7 installed. Restart open agent sessions to pick up the new MCP server.",
    uninstalling: "Removing the Context7 MCP registrations...",
    uninstalled: "Context7 removed. Your agent relies on its training data for library docs again.",
    enabling: "Re-registering the Context7 MCP server...",
    disabling: "Removing the Context7 MCP registrations...",
    disabled: "Context7 is off but still installed. Re-enable any time."
  }
};

const connectorSetupDetails: Record<string, string> = {
  claude_code:
    "Headroom injects ANTHROPIC_BASE_URL into shell profiles and ~/.claude/settings.json so Claude Code connects through Headroom.",
  codex:
    "Codex CLI, the IDE extension, and the desktop app share ~/.codex/config.toml. Headroom adds a managed provider there and an OPENAI_BASE_URL shell export, plus a SessionStart guard that warns when routing breaks. In Codex CLI, run /hooks once to review and trust the guard (and again after it changes).",
  grok_build:
    "Headroom writes a managed proxy block to ~/.grok/config.toml and exports GROK_CLI_CHAT_PROXY_BASE_URL in your shell profiles so Grok Build connects through Headroom.",
  opencode:
    "Headroom points the anthropic and openai provider base URLs in OpenCode's config file (usually ~/.config/opencode/opencode.json) at its localhost proxy and registers a transport plugin that routes every other provider through it too. Anthropic and OpenAI traffic is optimized; other providers pass through for visibility. A project-level opencode.json can override this for that project."
};

const CONNECTOR_SETUP_KEYS: Record<string, TranslationKey> = {
  claude_code: "connections.details.claude",
  codex: "connections.details.codex",
  grok_build: "connections.details.grok",
  opencode: "connections.details.opencode",
};

function localizeLearnStatus(t: Translate, value: string): string {
  if (value === "never scan") return t("learn.status.never");
  if (value === "last scan: today") return t("learn.status.today");
  if (value === "last scan: yesterday") return t("learn.status.yesterday");
  const days = value.match(/^last scan: (\d+) days ago$/);
  return days ? t("learn.status.daysAgo", { count: days[1] }) : value;
}

function localizeUiText(t: Translate, value: string): string {
  if (!value) return value;
  const restartRequired = value.match(/^Restart (.+) to apply the configuration\.$/);
  if (restartRequired) return t("connections.restartRequired", { name: restartRequired[1] });
  const restart = value.match(/^Quit and reopen (.+) if it was running when you enabled this\.$/);
  if (restart) return t("connections.restartAfterEnable", { name: restart[1] });
  const notDetected = value.match(/^(.+) was not detected\. Install (.+) and restart Headroom\.$/);
  if (notDetected) return t("connections.notDetectedInstall", { name: notDetected[1] });
  const verifyPrompt = value.match(/^Run one (.+) prompt and verify activity appears in Headroom\.$/);
  if (verifyPrompt) return t("connections.setup.verifyPrompt", { name: verifyPrompt[1] });
  const foreignProvider = value.match(
    /^Codex routing is currently handled by (.+), so Headroom is not intercepting\. Turn this connector off and on to route Codex through Headroom\.$/
  );
  if (foreignProvider) {
    return t("connections.foreignProviderStatus", { provider: foreignProvider[1] });
  }
  // Status chip for a client whose route another tool owns (see
  // `connectorDashboardStatus`). The provider is a product label, so it is
  // interpolated rather than looked up.
  const routedBy = value.match(/^Routed by (.+)$/);
  if (routedBy) {
    return t("connections.status.routedBy", { provider: routedBy[1] });
  }
  const exact: Record<string, TranslationKey> = {
    "Client was already configured for Headroom.": "connections.setup.alreadyConfigured",
    "Client configuration updated to route through Headroom.": "connections.setup.updated",
    "Your shell profile (e.g. ~/.zshrc) couldn't be updated - it isn't writable, or it isn't valid UTF-8 text. Core routing still works via the client's own config; to launch the client from a terminal, fix the file and re-run setup, or add the export manually.": "connections.setup.shellProfileUnwritable",
    "Restart your terminal/editor session to pick up environment changes.": "connections.setup.restartSession",
    "After first setup, quit and reopen any Codex desktop, CLI, or IDE session once to load the stable route; later Headroom pauses or crashes do not require a Codex restart.": "connections.setup.reopenCodex",
    "In Codex, run /hooks and trust the Headroom routing guard so it can warn you if routing breaks (re-trust if Headroom updates the guard).": "connections.setup.trustCodexGuard",
    "Found Claude Code ANTHROPIC_BASE_URL export in managed shell block.": "connections.verification.claudeShellFound",
    "Found Headroom-managed RTK PATH export in shell profiles.": "connections.verification.rtkPathFound",
    "Found ~/.claude/settings.json env.ANTHROPIC_BASE_URL pointing to Headroom.": "connections.verification.claudeSettingsFound",
    "Found Headroom-managed RTK Claude hook in ~/.claude/settings.json.": "connections.verification.rtkHookFound",
    "Claude Code ANTHROPIC_BASE_URL was not found in shell blocks or ~/.claude/settings.json.": "connections.verification.claudeRoutingMissing",
    "Headroom-managed RTK PATH export was not found in shell profiles.": "connections.verification.rtkPathMissing",
    "Headroom-managed RTK Claude hook was not found in ~/.claude/settings.json.": "connections.verification.rtkHookMissing",
    "Found Headroom routing guard registered in ~/.claude/settings.json.": "connections.verification.claudeGuardFound",
    "Headroom routing guard was not found in ~/.claude/settings.json.": "connections.verification.claudeGuardMissing",
    "Found Codex OPENAI_BASE_URL export in managed shell block.": "connections.verification.codexShellFound",
    "Found Headroom-managed provider block in ~/.codex/config.toml.": "connections.verification.codexProviderFound",
    "Headroom-managed provider block was not found in ~/.codex/config.toml.": "connections.verification.codexProviderMissing",
    "Found Headroom routing guard registered in ~/.codex/hooks.json.": "connections.verification.codexGuardFound",
    "Headroom routing guard was not found in ~/.codex/hooks.json.": "connections.verification.codexGuardMissing",
    "Found Grok Build GROK_CLI_CHAT_PROXY_BASE_URL export in managed shell block.": "connections.verification.grokShellFound",
    "Found Headroom-managed proxy block in ~/.grok/config.toml.": "connections.verification.grokProxyFound",
    "Headroom-managed proxy block was not found in ~/.grok/config.toml.": "connections.verification.grokProxyMissing",
    "Grok Build GROK_CLI_CHAT_PROXY_BASE_URL export was not found in shell profiles.": "connections.verification.grokShellMissing",
    "Found Headroom proxy base URLs for the anthropic and openai providers in OpenCode's config.": "connections.verification.opencodeProxyFound",
    "Headroom proxy base URLs were not found for the anthropic and openai providers in OpenCode's config.": "connections.verification.opencodeProxyMissing",
    "Headroom proxy is reachable on 127.0.0.1:6867.": "connections.verification.proxyReachable",
    "Setup is incomplete - open the info panel for the exact checks.": "connections.setupIncomplete",
    "Setup could not be verified - open the info panel and re-check.": "connections.setupUnverified",
    // Home banner lines from `setupStallBannerLine`, which builds them without a
    // translator: match them whole so the banner is not the one English-only
    // sentence in an otherwise translated Home screen.
    "No request has come through Headroom yet. Your terminal or editor is probably still running with its pre-Headroom settings - restart it and they should pick the new settings up.":
      "setupStall.bannerNoTraffic",
    "Requests are reaching Headroom but none are being optimized, so nothing is being saved yet. Check that your coding tool is still connected below.":
      "setupStall.bannerNoSavings",
    "Configured. Headroom's proxy is not answering on 127.0.0.1:6867 yet.": "connections.configuredProxyOffline",
    "Headroom Learn is unavailable on this platform.": "learn.unsupported",
    "Off": "connections.status.off",
    "Not installed": "tools.status.notInstalled",
    "Proxy unreachable": "status.issue.proxyUnreachable",
    "Verifying": "connections.status.verifying",
    "Restart needed": "connections.status.restartNeeded",
    "Active": "connections.status.active",
    "Connector is unavailable because this client is not detected on this machine.": "connections.genericUnavailable",
  };
  return exact[value] ? t(exact[value]) : value;
}

function localizedClientSetupNotice(
  t: Translate,
  clientName: string,
  result: ClientSetupResult
): string {
  return [
    result.replacedBaseUrl
      ? t("connections.baseUrlTakeover", { address: result.replacedBaseUrl })
      : t("connections.configuredThroughHeadroom", { name: clientName }),
    ...result.nextSteps.map((step) => localizeUiText(t, step)),
  ].join(" ");
}

function localizeAppUpdateCopy(t: Translate, value: string | null): string | null {
  if (!value) return value;
  const exact: Record<string, TranslationKey> = {
    "Checking for a new Headroom release…": "update.checking",
    "Update checks are not configured in this build yet.": "update.notConfigured",
    "Up to date.": "update.upToDate",
    "Could not check for updates.": "update.checkFailed",
    "Could not load app update settings.": "update.settingsFailed",
  };
  if (exact[value]) return t(exact[value]);
  const available = value.match(/^Update available: (.+)\.$/);
  if (available) return t("update.available", { version: available[1] });
  const downloading = value.match(/^Downloading Headroom (.+)…$/);
  if (downloading) return t("update.downloading", { version: downloading[1] });
  const installing = value.match(/^Installing Headroom (.+)…$/);
  if (installing) return t("update.installing", { version: installing[1] });
  const progressTotal = value.match(/^Downloading Headroom (.+): ([\d.]+) MB of ([\d.]+) MB \((\d+)%\)…$/);
  if (progressTotal) {
    return t("update.downloadProgressTotal", {
      version: progressTotal[1], downloaded: progressTotal[2], total: progressTotal[3], percent: progressTotal[4],
    });
  }
  const progress = value.match(/^Downloading Headroom (.+): ([\d.]+) MB…$/);
  return progress
    ? t("update.downloadProgress", { version: progress[1], downloaded: progress[2] })
    : value;
}

// Claude Code run inside the Claude desktop app is the one Claude Code surface
// Headroom cannot reach. Headroom routes Claude Code by pointing it at a local
// proxy through your shell profile and ~/.claude/settings.json; Anthropic's
// desktop app uses neither, so its requests never pass through Headroom.
//
// The copy names Anthropic as the source of the limitation, because a user who
// is not told whose constraint it is reasonably concludes Headroom is broken.
// It stops at attributing the decision and does not speculate about why they
// made it, which we do not know.
const CLAUDE_DESKTOP_LIMITATION =
  "Claude Code inside the Claude desktop app cannot be optimized. Headroom routes Claude Code by pointing it at a local proxy via your shell profile and ~/.claude/settings.json, and Anthropic's desktop app uses neither, so its requests never reach Headroom. That is a design decision on Anthropic's side and nothing Headroom can work around. Run Claude Code from a terminal, or use the VS Code or JetBrains extension, and Headroom picks it up automatically.";

const connectorSupportWarnings: Record<string, string> = {
  claude_code: CLAUDE_DESKTOP_LIMITATION
};

// Two-letter badges for the home-banner connector cluster: with three or
// more connectors, full names push the banner headline onto a second line.
const connectorMonograms: Record<string, string> = {
  claude_code: "CC",
  codex: "CX",
  grok_build: "GK",
  opencode: "OC"
};

const connectorUnavailableReasons: Record<string, string> = {
  // A user whose only Claude Code is the one built into the Claude desktop app
  // lands here, because the CLI genuinely isn't installed. Say so, or they
  // reasonably conclude Headroom is broken rather than inapplicable.
  claude_code:
    "Claude Code was not detected. Install the Claude Code CLI and restart Headroom. Note that Claude Code inside the Claude desktop app cannot be optimized: Anthropic's desktop app does not use the CLI's configuration, so Headroom never sees its requests. That is their design decision, not something Headroom can configure around.",
  codex:
    "Codex CLI was not detected. Headroom can still configure the Codex desktop app and IDE extension; install the CLI only if you also want terminal use.",
  grok_build:
    "Grok Build was not detected. Install Grok Build and restart Headroom.",
  opencode:
    "OpenCode was not detected. Install OpenCode and restart Headroom."
};

// Grok routing: UA-classified in the intercept, forwarded to api.x.ai via
// the backend's per-request x-headroom-base-url selection.
const GROK_CONNECTOR_ENABLED = true;

// OpenCode is visible in RC builds for end-to-end verification; keep the
// flag so it can ship dark in a stable if the RC pass surfaces problems.
const OPENCODE_CONNECTOR_ENABLED = true;

function withoutHiddenConnectors(list: ClientConnectorStatus[]) {
  return list.filter(
    (connector) =>
      (GROK_CONNECTOR_ENABLED || connector.clientId !== "grok_build") &&
      (OPENCODE_CONNECTOR_ENABLED || connector.clientId !== "opencode")
  );
}

// Connectors the Claude pricing gate neither auto-disables nor blocks
// enabling while the user is authenticated: Codex has its own proxy-side
// gate (codex_bypass); OpenCode bills against the user's own provider API
// keys, so the Claude gate has nothing to meter (no dedicated bypass).
const GATE_EXEMPT_CONNECTOR_IDS = new Set(["codex", "opencode", "grok_build"]);

const launcherConnectorFallback: ClientConnectorStatus[] = withoutHiddenConnectors([
  {
    clientId: "claude_code",
    name: "Claude Code",
    installed: false,
    enabled: false,
    verified: false
  },
  {
    clientId: "codex",
    name: "Codex",
    installed: false,
    enabled: false,
    verified: false
  },
  {
    clientId: "grok_build",
    name: "Grok Build",
    installed: false,
    enabled: false,
    verified: false
  },
  {
    clientId: "opencode",
    name: "OpenCode",
    installed: false,
    enabled: false,
    verified: false
  }
]);

const idleBootstrapProgress: BootstrapProgress = {
  running: false,
  complete: false,
  failed: false,
  currentStep: "Idle",
  message: "Installer has not started.",
  currentStepEtaSeconds: 0,
  overallPercent: 0
};

const idleRuntimeUpgradeProgress: RuntimeUpgradeProgress = {
  running: false,
  complete: false,
  failed: false,
  currentStep: "Idle",
  message: "",
  overallPercent: 0,
  fromVersion: null,
  toVersion: null
};

const MAX_UPGRADE_AUTO_RETRIES = 2;

const GATE_AUTO_DISABLED_STORAGE_KEY = "headroom:gateAutoDisabledConnectors";
const CONNECTOR_TRAFFIC_VERIFIED_STORAGE_KEY = "headroom:connectorTrafficVerified";

function isConnectorTrafficVerified(): boolean {
  try {
    return window.localStorage.getItem(CONNECTOR_TRAFFIC_VERIFIED_STORAGE_KEY) !== null;
  } catch {
    return false;
  }
}

function setConnectorTrafficVerified(verified: boolean): void {
  try {
    if (verified) {
      window.localStorage.setItem(
        CONNECTOR_TRAFFIC_VERIFIED_STORAGE_KEY,
        new Date().toISOString()
      );
    } else {
      window.localStorage.removeItem(CONNECTOR_TRAFFIC_VERIFIED_STORAGE_KEY);
    }
  } catch {
    // best effort
  }
}

// Fire-and-forget install-wizard funnel beacon. Errors (offline/mid-install)
// are swallowed so tracking never affects the wizard. Dedup is server-side
// (first-write-wins), so re-emitting a step on back-nav is harmless.
function reportFunnelStep(step: InstallWizardStep): void {
  void invoke("report_funnel_step", { step }).catch(() => {});
}

// Which funnel step each launcher stage's first paint represents. `install`
// (the landing screen) and `paywall` have no beacon: for new users the
// terms+email sign-up gate renders on top of the launcher, so the first real
// screen is captured by `signup_gate_shown`, not a launcher stage.
const LAUNCHER_STAGE_STEP: Partial<Record<LauncherStage, InstallWizardStep>> = {
  client_setup: "client_setup_shown",
  proxy_verify: "proxy_verify_started",
  post_install: "post_install_shown"
};

// Concrete first prompt for the post-install checklist: blank-page paralysis
// is a real drop-off cause between "agent connected" and "first prompt sent",
// so hand the user something they can paste that works in any repo.
// Live first-savings checklist. Rendered on the launcher's post-install stage
// AND as a Home card in the tray window: the launcher hides on click-away and
// the menu bar icon reopens the tray window, so without the Home card there
// is no way back to the checklist once it's been dismissed.
function FirstSavingsChecklist({
  dashboard,
  onReopenSetup
}: {
  dashboard: DashboardState;
  onReopenSetup: () => void;
}) {
  const { t } = useI18n();
  const starterPrompt = t("onboarding.starterPrompt");
  const [copied, setCopied] = useState(false);
  // A step stuck gray usually means traffic isn't reaching Headroom, not that
  // the user hasn't acted yet. Offer a setup re-check, but only after a grace
  // window: proxyReachable is false for ~1min on a healthy install while the
  // backend binds, so an immediate prompt would nag on good installs.
  const [showTroubleshoot, setShowTroubleshoot] = useState(false);
  useEffect(() => {
    const timer = window.setTimeout(() => setShowTroubleshoot(true), 20000);
    return () => window.clearTimeout(timer);
  }, []);
  return (
    <div className="post-install__checklist">
      <ol className="post-install__steps">
        <li className="post-install__step">
          <span
            className={`callout-banner__dot ${dashboard.lifetimeRequests > 0 ? "callout-banner__dot--healthy" : "callout-banner__dot--disconnected"}`}
            aria-hidden="true"
          />
          <div>
            <strong>{t("onboarding.agentConnected")}</strong>
            {dashboard.lifetimeRequests <= 0 && (
              <p>{t("onboarding.agentConnectedHelp")}</p>
            )}
          </div>
        </li>
        <li className="post-install__step">
          <span
            className={`callout-banner__dot ${dashboard.firstPromptRequestSeen ? "callout-banner__dot--healthy" : "callout-banner__dot--disconnected"}`}
            aria-hidden="true"
          />
          <div>
            <strong>{t("onboarding.firstPrompt")}</strong>
            {!dashboard.firstPromptRequestSeen && (
              <p>{t("onboarding.firstPromptHelp")}</p>
            )}
          </div>
        </li>
        <li className="post-install__step">
          <span className="callout-banner__dot callout-banner__dot--disconnected" aria-hidden="true" />
          <div>
            <strong>{t("onboarding.firstSavings")}</strong>
            <p>{t("onboarding.firstSavingsHelp")}</p>
          </div>
        </li>
      </ol>
      {dashboard.lifetimeEstimatedTokensSaved <= 0 &&
        dashboard.lifetimeEstimatedSavingsUsd <= 0 && (
        <div className="post-install__starter">
          <code>{starterPrompt}</code>
          <button
            className="secondary-button"
            onClick={() => {
              void navigator.clipboard?.writeText(starterPrompt).then(() => {
                setCopied(true);
                window.setTimeout(() => setCopied(false), 2000);
              });
            }}
            type="button"
          >
            {copied ? t("actions.copied") : t("actions.copyPrompt")}
          </button>
        </div>
      )}
      {showTroubleshoot && (
        <button
          type="button"
          className="post-install__troubleshoot"
          onClick={onReopenSetup}
        >
          {t("onboarding.troubleshoot")}
        </button>
      )}
    </div>
  );
}

const idleHeadroomLearnStatus: HeadroomLearnStatus = {
  running: false,
  progressPercent: 0,
  summary: "Select a project to run headroom learn.",
  outputTail: []
};

const idleHeadroomLearnPrereqStatus: HeadroomLearnPrereqStatus = {
  claudeCliAvailable: false,
  claudeCliPath: null,
  codexCliAvailable: false,
  codexCliPath: null,
  codexLoggedIn: false
};

const CLAUDE_CODE_INSTALL_DOCS_URL = "https://docs.claude.com/en/docs/claude-code/setup";
const CLAUDE_CODE_INSTALL_CURL_CMD = "curl -fsSL https://claude.ai/install.sh | bash";
const CODEX_CLI_INSTALL_CMD = "npm install -g @openai/codex";
const CODEX_CLI_LOGIN_CMD = "codex login";
const CODEX_INSTALL_DOCS_URL = "https://developers.openai.com/codex/cli";
const CODEX_INSTALL_NPM_CMD = "npm i -g @openai/codex";

const SALES_CONTACT_URL = (
  import.meta.env.VITE_HEADROOM_SALES_CONTACT_URL ??
  ""
).trim() || "mailto:hello@example.com";
const CONTACT_FORM_URL = (
  import.meta.env.VITE_HEADROOM_CONTACT_FORM_URL ??
  ""
).trim();

type StartupPhase = "window" | "dashboard" | "bootstrap" | "runtime" | "ready";

// Values must match User::CANCELLATION_REASONS server-side; anything else is
// rejected by /desktop/cancellation_intent.
const CANCELLATION_REASONS: { value: string; label: string }[] = [
  { value: "too_expensive", label: "Too expensive" },
  { value: "not_enough_savings", label: "Not saving me enough tokens" },
  { value: "not_using_it", label: "Not using it enough" },
  { value: "switched", label: "Switched to something else" },
  { value: "technical_problems", label: "Ran into technical problems" },
  { value: "other", label: "Something else" }
];

const authCodeExpiryFallbackSeconds = 900;
const APP_UPDATE_BACKGROUND_INITIAL_DELAY_MS = 12_000;
const APP_UPDATE_BACKGROUND_CHECK_INTERVAL_MS = 60 * 60 * 1000;

async function loadDashboard(): Promise<DashboardState> {
  try {
    return await invoke<DashboardState>("get_dashboard_state");
  } catch {
    return mockDashboard;
  }
}

function SavingsChartTooltip({
  active,
  payload,
  chartMode
}: {
  active?: boolean;
  payload?: ReadonlyArray<{ payload?: SavingsChartDatum }>;
  chartMode: SavingsChartMode;
}) {
  const { t } = useI18n();
  const point = payload?.[0]?.payload;
  if (!active || !point) {
    return null;
  }

  const providerSavings = mergeProviderSavingsForDisplay(point.byProvider ?? []);
  // The backend's by_provider rollup has no cache dimension, so the bucket's
  // compressible share is pro-rated across its providers. Exact whenever one
  // connector was active in the hour (the common case); an approximation only
  // when two ran concurrently with different cache hit rates.
  const costScale = point.actualCostUsd > 0 ? point.compressibleCostUsd / point.actualCostUsd : 1;
  const tokenScale =
    point.totalTokensSent > 0 ? point.compressibleTokensSent / point.totalTokensSent : 1;

  return (
    <div className="savings-chart__tooltip">
      <strong>{point.bucketLabel}</strong>
      {providerSavings.length > 0
        ? // Hourly buckets carry per-provider attribution: show Saved/Spent per
          // connector instead of the bucket total (which would be redundant).
          providerSavings.map((provider) => (
            <div className="savings-chart__tooltip-group" key={provider.label}>
              <span className="savings-chart__tooltip-label">{provider.label}</span>
              <span className="savings-chart__tooltip-item">
                <i
                  aria-hidden="true"
                  className={`savings-chart__tooltip-dot savings-chart__tooltip-dot--${
                    chartMode === "usd" ? "saved-usd" : "saved-tokens"
                  }`}
                />
                {/* "Input saved", not "Saved": the output-shaping group below
                    is bucket-level (upstream's by_provider rollup has no output
                    dimension), so an unqualified "Saved" here would read as the
                    connector's whole contribution. */}
                {chartMode === "usd"
                  ? t("chart.inputSaved", { value: currencyExact(provider.estimatedSavingsUsd) })
                  : t("chart.inputSavedTokens", { value: compactNumber(provider.estimatedTokensSaved) })}
              </span>
              <span className="savings-chart__tooltip-item">
                <i
                  aria-hidden="true"
                  className={`savings-chart__tooltip-dot savings-chart__tooltip-dot--${
                    chartMode === "usd" ? "actual-usd" : "actual-tokens"
                  }`}
                />
                {/* "Spent" for brevity; the figure is the compressible slice,
                    matching the bar and the chip's denominator. */}
                {chartMode === "usd"
                  ? t("chart.spent", { value: currencyExact(provider.actualCostUsd * costScale) })
                  : t("chart.spentTokens", { value: compactNumber(provider.totalTokensSent * tokenScale) })}
              </span>
            </div>
          ))
        : // Monthly buckets (and pre-attribution hourly buckets) have no provider
          // dimension: fall back to the aggregate bucket total.
        chartMode === "usd" ? (
          <div className="savings-chart__tooltip-group">
            <span className="savings-chart__tooltip-label">{t("chart.dollars")}</span>
            <span className="savings-chart__tooltip-item">
              <i
                aria-hidden="true"
                className="savings-chart__tooltip-dot savings-chart__tooltip-dot--saved-usd"
              />
              {t("chart.inputSaved", { value: currencyExact(point.estimatedSavingsUsd) })}
            </span>
            <span className="savings-chart__tooltip-item">
              <i
                aria-hidden="true"
                className="savings-chart__tooltip-dot savings-chart__tooltip-dot--actual-usd"
              />
              {t("chart.spent", { value: currencyExact(point.compressibleCostUsd) })}
            </span>
          </div>
        ) : (
          <div className="savings-chart__tooltip-group">
            <span className="savings-chart__tooltip-label">{t("chart.tokens")}</span>
            <span className="savings-chart__tooltip-item">
              <i
                aria-hidden="true"
                className="savings-chart__tooltip-dot savings-chart__tooltip-dot--saved-tokens"
              />
              {t("chart.inputSavedTokens", { value: compactNumber(point.estimatedTokensSaved) })}
            </span>
            <span className="savings-chart__tooltip-item">
              <i
                aria-hidden="true"
                className="savings-chart__tooltip-dot savings-chart__tooltip-dot--actual-tokens"
              />
              {t("chart.spentTokens", { value: compactNumber(point.compressibleTokensSent) })}
            </span>
          </div>
        )}
      {/* Output shaping has no per-provider breakdown, so it always renders as a
          single bucket-level row under whichever branch ran above. */}
      {(chartMode === "usd" ? point.outputSavingsUsd : point.outputTokensSaved) > 0 && (
        <div className="savings-chart__tooltip-group">
          <span className="savings-chart__tooltip-label">{t("chart.outputShaping")}</span>
          <span className="savings-chart__tooltip-item">
            <i
              aria-hidden="true"
              className={`savings-chart__tooltip-dot savings-chart__tooltip-dot--${
                chartMode === "usd" ? "saved-usd" : "saved-tokens"
              } savings-chart__tooltip-dot--output`}
            />
            {chartMode === "usd"
              ? t("chart.saved", { value: currencyExact(point.outputSavingsUsd) })
              : t("chart.savedTokens", { value: compactNumber(point.outputTokensSaved) })}
          </span>
        </div>
      )}
    </div>
  );
}

function delay(ms: number) {
  return new Promise<void>((resolve) => {
    window.setTimeout(resolve, ms);
  });
}

type SavingsChartView = "month" | "day";
type SavingsChartMode = "usd" | "tokens";

// Output-token reduction from the proxy's output shaper, shown as a secondary
// line inside the "Total input tokens saved" card so the two numbers (input
// tokens saved vs. output tokens not emitted) read as distinct. The line shows
// just the headline percent; clicking it opens a popover with the method
// ("estimated"/"measured"), confidence band, request count, and a note that
// output savings are counterfactual. Caller renders this only when `reduction`
// is present (the backend returns null until a verbosity baseline is seeded).
// The parent card is itself clickable, so the trigger stops event propagation.
function WindowRateChip({
  label,
  dot,
  title,
  badge,
  value,
  rows,
  note,
  popSide = "right"
}: {
  label: string;
  dot?: "input" | "output";
  title: string;
  badge: string;
  value: string;
  rows: Array<{ dt: string; dd: string }>;
  note: string;
  popSide?: "right" | "left";
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: Event) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: Event) => {
      if ((e as KeyboardEvent).key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="output-chip" ref={ref}>
      <button
        type="button"
        className={`output-chip__button${dot === "output" ? " output-chip__button--estimated" : ""}${open ? " is-open" : ""}`}
        aria-expanded={open}
        aria-label={t("chart.detailsFor", { title })}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
        onKeyDown={(e) => e.stopPropagation()}
      >
        {dot ? (
          <span
            className={`output-chip__dot${dot === "input" ? " output-chip__dot--input" : ""}`}
            aria-hidden="true"
          />
        ) : null}
        {label}
      </button>
      {open ? (
        <div
          className={`output-chip__popover${popSide === "left" ? " output-chip__popover--flip" : ""}`}
          role="dialog"
          aria-label={t("chart.detailsFor", { title })}
          onClick={(e) => e.stopPropagation()}
        >
          <div className="output-chip__pop-head">
            <span className="output-chip__pop-title">{title}</span>
            <span className="output-chip__pop-badge">{badge}</span>
          </div>
          <div className="output-chip__pop-value">{value}</div>
          <dl className="output-chip__pop-stats">
            {rows.map((row) => (
              <div key={row.dt}>
                <dt>{row.dt}</dt>
                <dd>{row.dd}</dd>
              </div>
            ))}
          </dl>
          <p className="output-chip__pop-note">{note}</p>
        </div>
      ) : null}
    </div>
  );
}

function OutputReductionChip({
  reduction,
  flip = false,
  allTimeFallback = false
}: {
  reduction: OutputReduction;
  flip?: boolean;
  /** Rendered because the visible window has no samples of its own, so this
   * is the lifetime figure standing in. Says so, rather than letting an
   * all-time number read as that period's. */
  allTimeFallback?: boolean;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const isMeasured = reduction.method === "measured";

  useEffect(() => {
    if (!open) return;
    const onDown = (e: Event) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: Event) => {
      if ((e as KeyboardEvent).key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="output-chip" ref={ref}>
      <button
        type="button"
        className={`output-chip__button${isMeasured ? "" : " output-chip__button--estimated"}${open ? " is-open" : ""}`}
        aria-expanded={open}
        aria-label={t("outputReduction.details")}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
        onKeyDown={(e) => e.stopPropagation()}
      >
        <span className="output-chip__dot" aria-hidden="true" />
        {t("outputReduction.short", { value: percent1(reduction.reductionPercent) })}
      </button>
      {open ? (
        <div
          className={`output-chip__popover${flip ? " output-chip__popover--flip" : ""}`}
          role="dialog"
          aria-label={t("outputReduction.details")}
          onClick={(e) => e.stopPropagation()}
        >
          <div className="output-chip__pop-head">
            <span className="output-chip__pop-title">
              {t("outputReduction.title")}{allTimeFallback ? ` · ${t("cacheInfo.allTime")}` : ""}
            </span>
            <span className="output-chip__pop-badge">{isMeasured ? t("outputReduction.measured") : t("outputReduction.estimated")}</span>
          </div>
          <div className="output-chip__pop-value">{percent1(reduction.reductionPercent)}%</div>
          <dl className="output-chip__pop-stats">
            <div>
              <dt>95% CI</dt>
              <dd>
                {percent1(reduction.ciLowPercent)}–{percent1(reduction.ciHighPercent)}%
              </dd>
            </div>
            <div>
              <dt>{t("outputReduction.requests")}</dt>
              <dd>{compactNumber(reduction.requests)}</dd>
            </div>
          </dl>
          <p className="output-chip__pop-note">
            {allTimeFallback ? t("outputReduction.fallback") : ""}
            {isMeasured
              ? t("outputReduction.measuredNote")
              : t("outputReduction.estimatedNote")}
          </p>
        </div>
      ) : null}
    </div>
  );
}

function DailySavingsChart({
  data,
  hourlyData,
  resetSignal,
  chartMode,
  setChartMode,
  outputReduction
}: {
  data: DailySavingsPoint[];
  hourlyData: HourlySavingsPoint[];
  resetSignal: number;
  chartMode: SavingsChartMode;
  setChartMode: (mode: SavingsChartMode) => void;
  outputReduction: OutputReduction | null;
}) {
  const { t } = useI18n();
  const currentMonth = startOfMonth(new Date());
  const today = startOfDay(new Date());
  const [visibleMonth, setVisibleMonth] = useState(() => currentMonth);
  const [visibleDay, setVisibleDay] = useState(() => today);
  const [view, setView] = useState<SavingsChartView>("day");
  const [savingsTodayUsd, setSavingsTodayUsd] = useState<number | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<number>("savings-today-updated", (event) => {
      setSavingsTodayUsd(event.payload);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);
  const firstSavingsMonth = earliestSavingsMonth(data);
  const firstHourlyDay = earliestHourlyDay(hourlyData);
  const monthlyWindow = buildMonthlySavingsWindow(data, visibleMonth);
  const hourlyWindow = buildHourlySavingsWindow(hourlyData, visibleDay);
  const monthlyData = buildMonthlySavingsChartData(monthlyWindow);
  const hourlyChartData = buildHourlySavingsChartData(hourlyWindow);
  const chartData = view === "month" ? monthlyData : hourlyChartData;
  // Canonical rate under the headline: input compression as a share of the
  // BILLABLE input for the visible window. Always priced in dollars regardless
  // of the chart's unit toggle -- a rate is unit-free, and only the dollar
  // figures share one scale (see compressibleInputSavingsRate). Output shaping
  // stays out of every percentage (it keeps its own measured reduction chip);
  // cache reads stay out of the denominator (~0.1x, deliberately untouched).
  const compressibleRate = compressibleInputSavingsRate(
    view === "month" ? monthlyWindow : hourlyWindow
  );
  // Window-scoped output reduction from the locally-sampled series, falling
  // back to the all-time figure for any window without samples of its own.
  // The fallback labels itself all-time (see OutputReductionChip) so it can't
  // read as that period's number — but it does render: since the estimator
  // only scores strata its baseline actually observed, a window of purely
  // unscored traffic has no samples at all, and showing nothing there reads
  // as "the shaper did nothing" rather than "this window can't be measured".
  const windowOutput = outputReductionForWindow(
    view === "month" ? monthlyWindow : hourlyWindow
  );
  const canViewPreviousMonth = firstSavingsMonth ? visibleMonth > firstSavingsMonth : false;
  const canViewNextMonth = visibleMonth < currentMonth;
  const canViewPreviousDay = firstHourlyDay ? visibleDay > firstHourlyDay : false;
  const canViewNextDay = visibleDay < today;
  const label = view === "month" ? formatMonthLabel(visibleMonth) : formatSelectedDayLabel(visibleDay);
  // Headline totals cover both Headroom layers -- input compression plus
  // output shaping -- matching the breakdown rows in the savings modal and the
  // segments stacked in the bars below. The live tray figure for today already
  // sums both, so it can stand in for the bucket sum while today is still open.
  const chartSaved = Math.max(
    0,
    chartMode === "usd"
      ? view === "day" && visibleDay >= today && savingsTodayUsd !== null
        ? savingsTodayUsd
        : chartData.reduce((s, d) => s + d.estimatedSavingsUsd + d.outputSavingsUsd, 0)
      : chartData.reduce((s, d) => s + d.estimatedTokensSaved + d.outputTokensSaved, 0)
  );

  useEffect(() => {
    const now = new Date();
    setVisibleMonth(startOfMonth(now));
    setVisibleDay(startOfDay(now));
  }, [resetSignal]);

  return (
    <div className="savings-chart">
      <section
        aria-label={t(view === "month" ? "chart.monthlyHistory" : "chart.hourlyHistory", { label })}
        className="savings-chart__panel"
      >
        <div className="savings-chart__panel-header">
          <div className="savings-chart__title-row">
            <strong>{t("chart.history")}</strong>
            <div className="savings-chart__toggle" aria-label={t("chart.metric")}>
              <button
                className={`savings-chart__toggle-button${chartMode === "usd" ? " is-active" : ""}`}
                onClick={() => setChartMode("usd")}
                type="button"
              >
                {t("chart.costs")}
              </button>
              <button
                className={`savings-chart__toggle-button${chartMode === "tokens" ? " is-active" : ""}`}
                onClick={() => setChartMode("tokens")}
                type="button"
              >
                {t("chart.tokens")}
              </button>
            </div>
          </div>
          <div className="savings-chart__nav">
            <div className="savings-chart__toggle" aria-label={t("chart.historyView")}>
              <button
                className={`savings-chart__toggle-button${view === "month" ? " is-active" : ""}`}
                onClick={() => setView("month")}
                type="button"
              >
                {t("chart.month")}
              </button>
              <button
                className={`savings-chart__toggle-button${view === "day" ? " is-active" : ""}`}
                onClick={() => setView("day")}
                type="button"
              >
                {t("chart.day")}
              </button>
            </div>
            <button
              className="savings-chart__nav-button"
              disabled={view === "month" ? !canViewPreviousMonth : !canViewPreviousDay}
              onClick={() =>
                view === "month"
                  ? setVisibleMonth((current) => addMonths(current, -1))
                  : setVisibleDay((current) => addDays(current, -1))
              }
              type="button"
            >
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <path d="M10 3L5 8l5 5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
              {t("chart.previous")}
            </button>
            <span className="savings-chart__range-label">{label}</span>
            <button
              className="savings-chart__nav-button"
              disabled={view === "month" ? !canViewNextMonth : !canViewNextDay}
              onClick={() =>
                view === "month"
                  ? setVisibleMonth((current) => addMonths(current, 1))
                  : setVisibleDay((current) => addDays(current, 1))
              }
              type="button"
            >
              {t("chart.next")}
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <path d="M6 3l5 5-5 5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </button>
          </div>
        </div>
        <div className="savings-chart__canvas savings-chart__canvas--combined">
          <div className="savings-chart__overlay" aria-hidden="true">
            <span className="savings-chart__overlay-total">
              {chartMode === "usd" ? currency(chartSaved) : compactNumber(chartSaved)}
            </span>
            <span className="savings-chart__overlay-label">
              {t(view === "day" ? "chart.savedToday" : "chart.savedMonth")}
            </span>
            {compressibleRate !== null || windowOutput !== null || outputReduction ? (
              <span className="savings-chart__overlay-chips">
                {compressibleRate !== null && (
                  <WindowRateChip
                    dot="input"
                    label={t("chart.inputLabel", { value: Math.round(compressibleRate.pct) })}
                    title={t("chart.inputTitle")}
                    badge={t("chart.measured")}
                    value={`${Math.round(compressibleRate.pct)}%`}
                    rows={[
                      { dt: t("chart.removed"), dd: currency(compressibleRate.saved) },
                      {
                        dt: t("chart.compressibleInput"),
                        dd: currency(compressibleRate.saved + compressibleRate.remaining)
                      }
                    ]}
                    note={t("chart.excludesCacheReads")}
                  />
                )}
                {windowOutput !== null ? (
                  <WindowRateChip
                    dot="output"
                    popSide="left"
                    label={t("chart.outputLabel", { value: Math.round(windowOutput.pct) })}
                    title={t("chart.outputTitle")}
                    badge={t("chart.estimated")}
                    value={`${Math.round(windowOutput.pct)}%`}
                    rows={[
                      { dt: t("chart.avoided"), dd: compactNumber(windowOutput.savedTokens) },
                      { dt: t("chart.baseline"), dd: compactNumber(windowOutput.baselineTokens) },
                      ...(outputReduction
                        ? [{ dt: t("chart.allTime"), dd: `${percent1(outputReduction.reductionPercent)}%` }]
                        : [])
                    ]}
                    note={t("chart.learnedBaselineNote")}
                  />
                ) : outputReduction ? (
                  <OutputReductionChip allTimeFallback flip reduction={outputReduction} />
                ) : null}
              </span>
            ) : null}
          </div>
          <ResponsiveContainer height="100%" width="100%">
            <BarChart
              barCategoryGap="5%"
              barGap={1}
              data={chartData}
              // Clears the overlay: total + label + the chip row added in 0.7.9
              // measure ~72px from the canvas top, so this is that plus a few
              // px of breathing room -- not slack to be reclaimed twice.
              margin={{ top: 82, right: 2, left: 2, bottom: 0 }}
            >
              <defs>
                <linearGradient id="actualUsdGradient" x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" stopColor="#c96a30" />
                  <stop offset="100%" stopColor="#ED834E" />
                </linearGradient>
                <linearGradient id="savingsUsdGradient" x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" stopColor="#3a7f74" />
                  <stop offset="100%" stopColor="#4F9E91" />
                </linearGradient>
                <linearGradient id="actualTokensGradient" x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" stopColor="#c96a30" />
                  <stop offset="100%" stopColor="#ED834E" />
                </linearGradient>
                <linearGradient id="savingsTokensGradient" x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" stopColor="#d4b832" stopOpacity="0.35" />
                  <stop offset="100%" stopColor="#EBCC6E" stopOpacity="0.25" />
                </linearGradient>
                {/* Output shaping sits on top of compression in the same hue
                    family, one shade lighter, so it reads as a second layer of
                    the same thing rather than a separate metric. */}
                <linearGradient id="outputUsdGradient" x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" stopColor="#63b3a4" />
                  <stop offset="100%" stopColor="#8CCCBE" />
                </linearGradient>
                <linearGradient id="outputTokensGradient" x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" stopColor="#e2cf6a" stopOpacity="0.3" />
                  <stop offset="100%" stopColor="#F3E2A4" stopOpacity="0.22" />
                </linearGradient>
              </defs>
              <CartesianGrid stroke="rgba(36, 31, 29, 0.06)" strokeDasharray="2 8" vertical={false} />
              <XAxis
                axisLine={false}
                dataKey="bucketKey"
                interval={0}
                minTickGap={view === "month" ? 8 : 8}
                tickFormatter={view === "month" ? dayOfMonthTickFormatter : hourOfDayTickFormatter}
                tick={{ fill: "#7a7169", fontSize: 10 }}
                tickLine={false}
              />
              {/* Both axes are hidden, so recharts' default "nice" rounding
                  buys nothing but empty space above the tallest bar: pin the
                  domain to the data so the peak bucket fills the plot. */}
              <YAxis domain={[0, "dataMax"]} hide yAxisId="usd" />
              <YAxis domain={[0, "dataMax"]} hide yAxisId="tokens" />
              <Tooltip content={(props) => <SavingsChartTooltip {...props} chartMode={chartMode} />} cursor={{ fill: "rgba(36, 31, 29, 0.05)" }} />
              {chartMode === "usd" && (
                <>
                  <Bar
                    dataKey="compressibleCostUsd"
                    fill="url(#actualUsdGradient)"
                    maxBarSize={16}
                    stackId="usd"
                    yAxisId="usd"
                  />
                  <Bar
                    dataKey="estimatedSavingsUsd"
                    fill="url(#savingsUsdGradient)"
                    maxBarSize={16}
                    // Kept on both savings segments: the output bar is empty on
                    // buckets that predate the layer, and a 1px cap under a
                    // present output segment is invisible anyway.
                    radius={[1, 1, 0, 0]}
                    stackId="usd"
                    yAxisId="usd"
                  />
                  <Bar
                    dataKey="outputSavingsUsd"
                    fill="url(#outputUsdGradient)"
                    maxBarSize={16}
                    radius={[1, 1, 0, 0]}
                    stackId="usd"
                    yAxisId="usd"
                  />
                </>
              )}
              {chartMode === "tokens" && (
                <>
                  <Bar
                    dataKey="compressibleTokensSent"
                    fill="url(#actualTokensGradient)"
                    maxBarSize={16}
                    stackId="tokens"
                    yAxisId="tokens"
                  />
                  <Bar
                    dataKey="estimatedTokensSaved"
                    fill="url(#savingsTokensGradient)"
                    maxBarSize={16}
                    stackId="tokens"
                    yAxisId="tokens"
                    shape={(props: any) => {
                      const { x, y, width, height, fill } = props;
                      if (!width || !height) return <g />;
                      const sw = 1.5;
                      return (
                        <rect
                          x={x + sw / 2}
                          y={y + sw / 2}
                          width={Math.max(0, width - sw)}
                          height={Math.max(0, height - sw)}
                          fill={fill}
                          stroke="#EBCC6E"
                          strokeWidth={sw}
                          rx={1}
                        />
                      );
                    }}
                  />
                  <Bar
                    dataKey="outputTokensSaved"
                    fill="url(#outputTokensGradient)"
                    maxBarSize={16}
                    stackId="tokens"
                    yAxisId="tokens"
                    shape={(props: any) => {
                      const { x, y, width, height, fill } = props;
                      if (!width || !height) return <g />;
                      const sw = 1.5;
                      return (
                        <rect
                          x={x + sw / 2}
                          y={y + sw / 2}
                          width={Math.max(0, width - sw)}
                          height={Math.max(0, height - sw)}
                          fill={fill}
                          stroke="#F3E2A4"
                          strokeWidth={sw}
                          rx={1}
                        />
                      );
                    }}
                  />
                </>
              )}
            </BarChart>
          </ResponsiveContainer>
        </div>
      </section>
    </div>
  );
}


function renderConnectorLogo(clientId: string) {
  return <Sparkle className="client-logo__glyph" size={20} weight="duotone" />;
}

function AddonClientChips({
  connectors,
  savings
}: {
  connectors: ClientConnectorStatus[];
  savings?: string | null;
}) {
  const { t, resolvedLocale } = useI18n();
  const localizedSavings = resolvedLocale === "zh-CN" && savings
    ? savings
        .replace(/^(\d+) docs? converted$/i, "已转换 $1 份文档")
        .replace(/lower cost \(benchmark\)/i, "成本降低（基准数据）")
        .replace(/fewer output tokens \(benchmark\)/i, "输出 Token 减少（基准数据）")
    : savings;
  const clients = sortClientConnectors(aggregateClientConnectors(connectors));
  if (clients.length === 0 && !localizedSavings) {
    return null;
  }
  return (
    <div className="addon-card__clients">
      {clients.map((connector) => {
        const status = connectorDashboardStatus(connector);
        return (
          <span
            className="callout-banner__chip"
            key={connector.clientId}
            title={localizeUiText(t, status.label)}
          >
            <span
              className={`callout-banner__chip-dot callout-banner__chip-dot--${status.tone}`}
              aria-hidden="true"
            />
            <span className="callout-banner__chip-name">{connector.name}</span>
            <span className="visually-hidden">{localizeUiText(t, status.label)}</span>
          </span>
        );
      })}
      {localizedSavings ? (
        <span className="callout-banner__chip" title={t("metrics.savingsLabel")}>
          <span
            className="callout-banner__chip-dot callout-banner__chip-dot--active"
            aria-hidden="true"
          />
          <span className="callout-banner__chip-name">{localizedSavings}</span>
        </span>
      ) : null}
    </div>
  );
}

function formatAddonVersion(version: string, locale?: string): string {
  if (version === "latest") {
    if (locale === "zh-CN") return "未获取版本";
    if (locale === "zh-TW") return "未取得版本";
    if (locale === "ja") return "バージョン未取得";
    if (locale === "ko") return "버전 확인 안 됨";
    return "Version unavailable";
  }
  return /^\d/.test(version) ? `v${version}` : version;
}

function AddonCard({
  toolId,
  name,
  version,
  installed,
  enabled,
  description,
  activationLabel,
  copy,
  infoOpen,
  onToggleInfo,
  busy,
  busyLabel,
  resultMessage,
  errorMessage,
  upstreamVersion,
  upstreamUpdateAvailable,
  updateRequiresAppUpdate,
  supportedVersion,
  onDismissResult,
  sourceUrl,
  onOpenSource,
  connectors,
  showClients,
  savings,
  defaultMode,
  supportedModes,
  actionsDisabled,
  onModeChange = () => undefined,
  onInstall,
  onToggleEnabled,
  onUninstall,
  updateAvailable,
  updateActionAvailable,
  repairActionAvailable,
  updateCheckFailed,
  onUpdate,
  availableVersion,
  unavailableReason,
  children
}: {
  toolId: string;
  name: string;
  version?: string | null;
  installed: boolean;
  enabled: boolean;
  description: ReactNode;
  activationLabel?: string | null;
  copy?: AddonCopy;
  infoOpen: boolean;
  onToggleInfo: () => void;
  busy: boolean;
  busyLabel: string | null;
  resultMessage: string | null;
  errorMessage: string | null;
  upstreamVersion?: string | null;
  upstreamUpdateAvailable?: boolean;
  updateRequiresAppUpdate?: boolean;
  supportedVersion?: string | null;
  onDismissResult: () => void;
  sourceUrl: string;
  onOpenSource: () => void;
  connectors: ClientConnectorStatus[];
  showClients: boolean;
  savings?: string | null;
  defaultMode?: string | null;
  supportedModes?: string[];
  actionsDisabled: boolean;
  onModeChange?: (mode: string) => void;
  onInstall: () => void;
  onToggleEnabled: () => void;
  onUninstall: () => void;
  updateAvailable?: boolean;
  updateActionAvailable?: boolean;
  repairActionAvailable?: boolean;
  updateCheckFailed?: boolean;
  onUpdate?: () => void;
  availableVersion?: string | null;
  /** Platform has no installable build: gray the card, drop the actions. */
  unavailableReason?: string | null;
  children?: ReactNode;
}) {
  const { t, resolvedLocale } = useI18n();
  const infoKey = ADDON_INFO_KEYS[toolId];
  return (
    <li className={`addon-card${unavailableReason ? " addon-card--unavailable" : ""}`}>
      <div className="addon-card__body">
        <div className="addon-card__heading">
          <span className="addon-card__name">{name}</span>
          {installed && version ? (
            <span className="addon-card__version">{formatAddonVersion(version, resolvedLocale)}</span>
          ) : null}
          {copy ? (
            <button
              type="button"
              className="addon-card__info"
              aria-label={t("addons.whatItDoes", { name })}
              aria-expanded={infoOpen}
              onClick={onToggleInfo}
            >
              i
            </button>
          ) : null}
          {installed ? (
            <span
              className={`addon-card__badge addon-card__badge--${enabled ? "on" : "off"}`}
            >
              {t(enabled ? "addons.enabledBadge" : "addons.disabledBadge")}
            </span>
          ) : null}
        </div>
        {infoOpen && copy ? (
          <p className="addon-card__info-text">{infoKey ? t(infoKey) : copy.whatItDoes}</p>
        ) : null}
        <p className="addon-card__description">{description}</p>
        {activationLabel ? (
          <p className="addon-card__activation-note">{activationLabel}</p>
        ) : null}
        <ToolModeSelector
          toolId={toolId}
          name={name}
          installed={installed}
          enabled={enabled}
          defaultMode={defaultMode}
          supportedModes={supportedModes}
          disabled={actionsDisabled || Boolean(unavailableReason)}
          onChange={onModeChange}
        />
        {showClients ? (
          <AddonClientChips connectors={connectors} savings={savings} />
        ) : null}
        <button type="button" className="addon-card__link" onClick={onOpenSource}>
          {sourceUrl}
        </button>
        {unavailableReason ? (
          <p className="addon-card__notice">{localizeUiText(t, unavailableReason)}</p>
        ) : null}
        {updateRequiresAppUpdate && upstreamVersion ? (
          <p className="addon-card__notice addon-card__notice--update">
            {t("addons.upstreamRequiresAppUpdate", {
              latest: formatAddonVersion(upstreamVersion, resolvedLocale),
              supported: supportedVersion ? formatAddonVersion(supportedVersion, resolvedLocale) : "—",
            })}
          </p>
        ) : null}
        {updateCheckFailed ? (
          <p className="addon-card__notice">{t("addons.updateCheckFailed")}</p>
        ) : null}
        {busy && busyLabel ? (
          <p className="addon-card__progress">{busyLabel}</p>
        ) : errorMessage ? (
          <p className="addons__error addon-card__error">{errorMessage}</p>
        ) : resultMessage ? (
          <p className="addon-card__result">
            {resultMessage}
            <button
              type="button"
              className="addon-card__result-dismiss"
              aria-label={t("actions.dismiss")}
              onClick={onDismissResult}
            >
              ×
            </button>
          </p>
        ) : null}
        {children}
      </div>
      <div className="addon-card__actions">
        {unavailableReason ? (
          <button type="button" className="addon-card__action" disabled>
            {t("tools.status.unavailable")}
          </button>
        ) : !installed ? (
          <button
            type="button"
            className="addon-card__action addon-card__action--primary"
            disabled={actionsDisabled}
            onClick={onInstall}
          >
            {t("tools.install")}
          </button>
        ) : (
          <>
            {repairActionAvailable && onUpdate ? (
              <button
                type="button"
                className="addon-card__action addon-card__action--primary"
                disabled={actionsDisabled}
                onClick={onUpdate}
              >
                {t("addons.repair")}
              </button>
            ) : updateActionAvailable && onUpdate ? (
              // install_addon is idempotent and always installs the pinned
              // version, so it is also the upgrade path -- no second command.
              <button
                type="button"
                className="addon-card__action addon-card__action--primary"
                disabled={actionsDisabled}
                onClick={onUpdate}
              >
                {availableVersion
                  ? t("addons.updateTo", { version: formatAddonVersion(availableVersion, resolvedLocale) })
                  : t("addons.update")}
              </button>
            ) : null}
            <button
              type="button"
              className="addon-card__action"
              disabled={actionsDisabled}
              onClick={onToggleEnabled}
            >
              {enabled ? t("tools.disable") : t("tools.enable")}
            </button>
            <button
              type="button"
              className="addon-card__action addon-card__action--danger"
              disabled={actionsDisabled}
              onClick={onUninstall}
            >
              {t("addons.uninstall")}
            </button>
          </>
        )}
      </div>
    </li>
  );
}

const ADDON_INFO_KEYS: Record<string, TranslationKey> = {
  rtk: "addons.description.rtk",
  markitdown: "addons.description.markitdown",
  ponytail: "addons.description.ponytail",
  caveman: "addons.description.caveman",
  allinluna: "addons.info.allinluna",
  serena: "addons.description.serena",
  "codebase-memory": "addons.description.codebaseMemory",
  context7: "addons.description.context7",
};

const ADDON_DESCRIPTION_KEYS: Record<string, TranslationKey> = {
  rtk: "addons.description.rtk",
  markitdown: "addons.description.markitdown",
  ponytail: "addons.description.ponytail",
  caveman: "addons.description.caveman",
  allinluna: "addons.description.allinluna",
  serena: "addons.description.serena",
  "codebase-memory": "addons.description.codebaseMemory",
  context7: "addons.description.context7",
};

const ADDON_DISPLAY_ORDER = [
  "stop-that-shit",
  "agent-guard",
  "grill-me",
  "openspec",
  "superpowers",
  "gstack",
  "allinluna",
  "ralph-loop",
  "serena",
  "codebase-memory",
  "context7",
  "markitdown",
  "rtk",
  "ponytail",
  "caveman"
];

const ADDON_CLIENT_IDS: Record<string, readonly string[]> = {
  allinluna: ["codex"],
};

function addonConnectors(
  toolId: string,
  connectors: ClientConnectorStatus[]
): ClientConnectorStatus[] {
  const supportedClientIds = ADDON_CLIENT_IDS[toolId];
  return supportedClientIds
    ? connectors.filter((connector) => supportedClientIds.includes(connector.clientId))
    : connectors;
}

// Unknown ids land after the curated ones, before the trailing RTK card.
function addonDisplayRank(id: string): number {
  const rank = ADDON_DISPLAY_ORDER.indexOf(id);
  return rank === -1 ? ADDON_DISPLAY_ORDER.length : rank;
}

function normalizeDesktopPlatform(platform: string | undefined, userAgent: string) {
  const reportedPlatform = platform?.toLowerCase();
  if (reportedPlatform === "darwin" || reportedPlatform === "macos") {
    return "macos";
  }
  if (reportedPlatform === "win32" || reportedPlatform === "windows") {
    return "windows";
  }
  if (reportedPlatform) {
    return reportedPlatform;
  }
  if (userAgent.includes("Windows")) {
    return "windows";
  }
  return userAgent.includes("Mac") ? "macos" : "linux";
}

function buildUpgradeIssueMailto(failure: RuntimeUpgradeFailure): string {
  const subject = `Headroom update issue (${failure.targetHeadroomVersion}, ${failure.failurePhase})`;
  const diagnosticLines = [
    `App version: ${failure.appVersion}`,
    `Target Headroom: ${failure.targetHeadroomVersion}`,
    failure.fallbackHeadroomVersion
      ? `Fallback running: ${failure.fallbackHeadroomVersion}`
      : null,
    `Failure phase: ${failure.failurePhase}`,
    `Attempts: ${failure.attempts}`,
    `First attempt: ${failure.firstAttemptAt}`,
    `Last attempt: ${failure.lastAttemptAt}`,
    `Rollback restored: ${failure.rollbackRestored ? "yes" : "no"}`,
    "",
    "Error:",
    failure.errorMessage,
  ].filter((line): line is string => line !== null);
  const body =
    "What were you doing when this happened?\n\n\n" +
    "---\n" +
    "Diagnostic info (please keep):\n" +
    diagnosticLines.join("\n");
  return `mailto:headroom-local-community@localhost.invalid?subject=${encodeURIComponent(subject)}&body=${encodeURIComponent(body)}`;
}

interface ProxyVerificationRow {
  clientId: string;
  name: string;
  state: "processing" | "waiting" | "verified";
  message: string;
}


export default function App() {
  const { locale, resolvedLocale, setLocale, t } = useI18n();
  const [dashboard, setDashboard] = useState<DashboardState>(mockDashboard);
  const [addonBusyById, setAddonBusyById] = useState<AddonOperationMessages>({});
  const [addonInfoId, setAddonInfoId] = useState<string | null>(null);
  const [addonResultById, setAddonResultById] = useState<AddonOperationMessages>({});
  const [addonErrorById, setAddonErrorById] = useState<AddonOperationMessages>({});
  const [addonUpdateChecks, setAddonUpdateChecks] = useState<AddonUpdateCheck[]>([]);
  const [addonUpdateBusy, setAddonUpdateBusy] = useState(false);
  const [addonUpdatesChecked, setAddonUpdatesChecked] = useState(false);
  const [addonUpdateCheckFailed, setAddonUpdateCheckFailed] = useState(false);
  // An update check can outlive an install/update operation.  Its result is
  // only valid for the dashboard snapshot that existed when it started.
  const addonUpdateGenerationRef = useRef(0);
  const [bootstrapping, setBootstrapping] = useState(false);
  const [bootstrapProgress, setBootstrapProgress] =
    useState<BootstrapProgress>(idleBootstrapProgress);
  const [runtimeUpgradeProgress, setRuntimeUpgradeProgress] =
    useState<RuntimeUpgradeProgress>(idleRuntimeUpgradeProgress);
  const [cliUpdateError, setCliUpdateError] = useState<string | null>(null);
  const [bootstrapError, setBootstrapError] = useState<string | null>(null);
  const [windowLabel, setWindowLabel] = useState<"main" | "launcher" | null>(null);
  const [startupPhase, setStartupPhase] = useState<StartupPhase>("window");
  const [startupPercent, setStartupPercent] = useState(10);
  const [startupCopy, setStartupCopy] = useState("Opening launch window…");
  const [startupReady, setStartupReady] = useState(false);
  const [activeView, setActiveView] = useState<TrayView>("home");
  const [selectedClaudeProjectPath, setSelectedClaudeProjectPath] = useState<string | null>(null);
  useEffect(() => {
    if (!LOCAL_COMMUNITY_EDITION) return;
    void invoke("set_native_tray_locale", { locale: resolvedLocale }).catch((error) => {
      console.error("Failed to update native tray language", error);
    });
  }, [resolvedLocale]);

  const [pricingAudience, setPricingAudience] = useState<PricingAudience>("individual");
  // Annual first: it is the cheaper per-month number and the tab the "Save 25%"
  // badge points at. A subscriber's own period overrides this once it loads.
  const [billingPeriod, setBillingPeriod] = useState<BillingPeriod>("annual");
  // Launcher stage is a single source of truth for which onboarding screen
  // is showing. Only one screen can be active at a time; transitions go
  // through `setLauncherStage` so implicit renders from bootstrap/dashboard
  // flags cannot bypass the install step's readiness gate.
  const [launcherStage, setLauncherStage] = useState<LauncherStage>("install");
  // Set while a headroom://auth magic link is being redeemed. The launcher is
  // already on screen by then (the deep-link handler shows it), so without this
  // the sign-in ran invisibly behind whatever onboarding stage it landed on.
  const [magicLinkState, setMagicLinkState] = useState<MagicLinkState | null>(null);
  // Paywall-first experiment flag, served from the Rust-side cache (never
  // blocks). Gated flow applies only to fresh installs: an installed runtime
  // means the user is grandfathered and sees zero difference.
  const [launchFlags, setLaunchFlags] = useState<LaunchFlags | null>(null);
  // Set when the app has been up past the stall window with zero savings on
  // record. Held in state (not rendered immediately) so the modal is waiting
  // whenever the user next opens the tray, rather than stealing focus.
  const [setupStall, setSetupStall] = useState<SetupStallAlert | null>(null);
  // Same signals as the modal above, but for the always-on Home banner, whose
  // default copy tells a user with zero savings to check back later.
  const [stallBannerLine, setStallBannerLine] = useState<string | null>(null);
  const [debugOverrides, setDebugOverrides] = useState<DebugOverrides | null>(null);
  const [connectors, setConnectors] = useState<ClientConnectorStatus[]>([]);
  const [openConnectorHelpId, setOpenConnectorHelpId] = useState<string | null>(null);
  const [openConnectorWarningId, setOpenConnectorWarningId] = useState<string | null>(null);
  const [connectorsBusy, setConnectorsBusy] = useState(false);
  const [codexRestartBusy, setCodexRestartBusy] = useState(false);
  const [connectorPhase, setConnectorPhase] = useState<"disabled" | "verifying" | "healthy">(
    () => (isConnectorTrafficVerified() ? "healthy" : "verifying")
  );
  const [connectorsError, setConnectorsError] = useState<string | null>(null);
  const [connectorsNotice, setConnectorsNotice] = useState<string | null>(null);
  const [proxyVerificationRows, setProxyVerificationRows] = useState<ProxyVerificationRow[]>([]);
  const [proxyVerificationHint, setProxyVerificationHint] = useState<
    { text: string; tone: "info" | "error" } | null
  >(null);
  // Leaving the verify step unverified takes two clicks: the first arms the
  // warning, the second leaves. 86% of installs used to click straight past
  // this screen (median 26s, 45% under 15s) and the ones that did went on to
  // send a first prompt 59% of the time vs 76% for the ones that waited -- the
  // single biggest activation leak in onboarding, and invisible in support
  // reports because nothing errors.
  const [proxyVerifySkipArmed, setProxyVerifySkipArmed] = useState(false);
  const proxyVerificationRequestAnchorRef = useRef<Record<string, number> | null>(null);
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus | null>(null);
  // Fresh install (no runtime on disk yet). Drives the onboarding email-harvest
  // step: every fresh install collects an email to start the card-free 7-day
  // trial. The old paywall-first checkout gate has been retired, so this no
  // longer depends on the launch flag.
  const paywallFirstFlow = runtimeStatus?.installed === false;
  // Verify against the always-up 6867 intercept (which counts passthrough
  // traffic) instead of the backend whenever the backend won't be optimizing:
  // pre-install, or when the pricing gate has bypassed it (e.g. ended trial).
  // Otherwise proxy_verify waits forever on a backend that never comes up.
  const interceptOnlyVerify =
    paywallFirstFlow || runtimeStatus?.bypassed === true;
  const [resuming, setResuming] = useState(false);
  const [resumeError, setResumeError] = useState<string | null>(null);
  const [appUpdateConfig, setAppUpdateConfig] = useState<AppUpdateConfiguration | null>(null);
  const [appUpdateAvailable, setAppUpdateAvailable] = useState<AvailableAppUpdate | null>(null);
  const [appUpdateBusy, setAppUpdateBusy] = useState(false);
  const [appUpdateInstallBusy, setAppUpdateInstallBusy] = useState(false);
  const [appUpdateRestartBusy, setAppUpdateRestartBusy] = useState(false);
  const [appUpdateReadyToRestart, setAppUpdateReadyToRestart] = useState(false);
  const [showAppUpdateDialog, setShowAppUpdateDialog] = useState(false);
  const [appUpdateStatusCopy, setAppUpdateStatusCopy] = useState<string | null>(null);
  const [showHeadroomDetails, setShowHeadroomDetails] = useState(false);
  const [headroomLogLines, setHeadroomLogLines] = useState<string[]>([]);
  const headroomLogRef = useRef<HTMLPreElement | null>(null);
  const [showRtkDetails, setShowRtkDetails] = useState(false);
  const [rtkActivityLines, setRtkActivityLines] = useState<string[]>([]);
  const rtkActivityRef = useRef<HTMLPreElement | null>(null);
  const [claudeProjects, setClaudeProjects] = useState<ClaudeCodeProject[]>([]);
  const [claudeProjectsBusy, setClaudeProjectsBusy] = useState(false);
  const [claudeProjectsError, setClaudeProjectsError] = useState<string | null>(null);
  const [showAllClaudeProjects, setShowAllClaudeProjects] = useState(false);
  const [headroomLearnStatus, setHeadroomLearnStatus] =
    useState<HeadroomLearnStatus>(idleHeadroomLearnStatus);
  const [optimizeAppliedByProject, setOptimizeAppliedByProject] =
    useState<Record<string, AppliedPatterns> | null>(null);
  const [optimizeAppliedRefreshTick, setOptimizeAppliedRefreshTick] = useState(0);
  const previousHeadroomLearnRunningRef = useRef(false);
  const [headroomLearnBusy, setHeadroomLearnBusy] = useState(false);
  const [headroomLearnPrereq, setHeadroomLearnPrereq] =
    useState<HeadroomLearnPrereqStatus>(idleHeadroomLearnPrereqStatus);
  const [activityFeed, setActivityFeed] = useState<ActivityFeedResponse>({
    tiles: {
      transformation: null,
      record: null,
      rtkToday: null,
      serenaToday: null,
      learningsMilestone: null,
      weeklyRecap: null,
      trainSuggestion: null
    },
    proxyReachable: false
  });
  // Flipped true after the first activity feed fetch attempt resolves (success
  // OR failure). Before this the feed holds a placeholder value whose
  // `proxyReachable: false` would falsely render the "proxy unreachable"
  // empty state and make the tab feel like it's already in an error state.
  const [activityFeedLoaded, setActivityFeedLoaded] = useState(false);
  // Tray window focus proxies for visibility: the window auto-hides on blur
  // via `triggerHide`, so "not focused" ⇒ "hidden" for polling purposes.
  const [trayWindowFocused, setTrayWindowFocused] = useState(true);
  // Sticky flag: the user has visited a heavy-data tab (Activity or Optimize)
  // at least once this session. The tray-focus pre-warm is gated on this so
  // users who stay on Home don't pay its IPC/subprocess cost on every focus.
  const [heavyTabEverOpened, setHeavyTabEverOpened] = useState(false);
  const activityTabTrackedRef = useRef(false);
  const [activityFeedError, setActivityFeedError] = useState<string | null>(null);
  const [pricingStatus, setPricingStatus] = useState<HeadroomPricingStatus | null>(null);
  const [cachedPricing] = useState<CachedPricing>(() => readCachedPricing());
  const [pricingBusy, setPricingBusy] = useState(false);
  const [pricingError, setPricingError] = useState<string | null>(null);
  const pricingRefreshInFlightRef = useRef(false);
  // When the last authoritative status (verify / sign-out) was applied. A slow
  // pricing fetch issued before it must not land afterwards and overwrite it:
  // on a fresh install the very first fetch is the slowest, and it was still in
  // flight when the magic link signed the user in, so it clobbered the signed-in
  // status with its own stale signed-out one.
  const pricingStatusStampRef = useRef(0);
  const [authEmail, setAuthEmail] = useState("");
  const [authCode, setAuthCode] = useState("");
  const [authCodeRequestedFor, setAuthCodeRequestedFor] = useState<string | null>(null);
  const [authCodeExpirySeconds, setAuthCodeExpirySeconds] = useState(authCodeExpiryFallbackSeconds);
  const [authRequestBusy, setAuthRequestBusy] = useState(false);
  const [authVerifyBusy, setAuthVerifyBusy] = useState(false);
  const [authFlowError, setAuthFlowError] = useState<string | null>(null);
  const [authFlowSuccess, setAuthFlowSuccess] = useState<string | null>(null);
  const [pendingUpgradePlanId, setPendingUpgradePlanId] = useState<UpgradePlanId | null>(null);
  const [showAllUpgradePlans, setShowAllUpgradePlans] = useState(false);
  const [checkoutPollingDeadline, setCheckoutPollingDeadline] = useState<number | null>(null);
  const desktopActivationSentRef = useRef(false);
  // Persisted: pricing gates last days, and an app restart mid-gate used to
  // lose this set — the auto-disabled connectors then never re-enabled when
  // the gate reopened, leaving users silently unoptimized until a manual
  // toggle.
  const [initialAutoDisabledByGate] = useState<Set<string>>(() => {
    try {
      const raw = window.localStorage.getItem(GATE_AUTO_DISABLED_STORAGE_KEY);
      return new Set(raw ? (JSON.parse(raw) as string[]) : []);
    } catch {
      return new Set<string>();
    }
  });
  const autoDisabledByGateRef = useRef<Set<string>>(initialAutoDisabledByGate);
  const persistAutoDisabledByGate = () => {
    try {
      window.localStorage.setItem(
        GATE_AUTO_DISABLED_STORAGE_KEY,
        JSON.stringify([...autoDisabledByGateRef.current])
      );
    } catch {
      // best effort
    }
  };
  const [learnInstallCopyNotice, setLearnInstallCopyNotice] = useState<string | null>(null);

  const [stepSignature, setStepSignature] = useState("");
  const [stepStartedAtMs, setStepStartedAtMs] = useState<number | null>(null);
  const [stepEtaSeedSeconds, setStepEtaSeedSeconds] = useState(0);
  const [stepBasePercent, setStepBasePercent] = useState(0);
  const [chartResetSignal, setChartResetSignal] = useState(0);
  const [chartMode, setChartMode] = useState<SavingsChartMode>("usd");
  // Safety net: if native history never loads (backend unreachable), reveal the
  // chart anyway after this delay rather than spinning forever.
  const [historyLoadTimedOut, setHistoryLoadTimedOut] = useState(false);
  const [showSavingsInfo, setShowSavingsInfo] = useState(false);
  const [showCacheInfo, setShowCacheInfo] = useState(false);
  const [autostartEnabled, setAutostartEnabled] = useState<boolean | null>(null);
  const [autostartBusy, setAutostartBusy] = useState(false);
  const [rtkBusy, setRtkBusy] = useState(false);
  const [autoLearnEnabled, setAutoLearnEnabled] = useState<boolean | null>(null);
  const [autoLearnBusy, setAutoLearnBusy] = useState(false);
  const [showUninstallDialog, setShowUninstallDialog] = useState(false);
  const [uninstallBusy, setUninstallBusy] = useState(false);
  const [uninstallError, setUninstallError] = useState<string | null>(null);
  // "cancel" is not a plan: it is the cancel-subscription action, which shares the
  // busy state so only the button that was clicked reads "Opening...".
  const [upgradeActionBusy, setUpgradeActionBusy] = useState<UpgradePlanId | "cancel" | null>(null);
  const [upgradeActionError, setUpgradeActionError] = useState<string | null>(null);
  const [pendingPlanChange, setPendingPlanChange] = useState<{
    fromTier: HeadroomSubscriptionTier;
    toTier: HeadroomSubscriptionTier;
    billingPeriod: BillingPeriod;
  } | null>(null);
  const [planChangeBusy, setPlanChangeBusy] = useState(false);
  const [planChangeError, setPlanChangeError] = useState<string | null>(null);
  const [reactivateBusy, setReactivateBusy] = useState(false);
  const [reactivateError, setReactivateError] = useState<string | null>(null);
  const [saveOffer, setSaveOffer] = useState<SaveOffer | null>(null);
  const [saveOfferBusy, setSaveOfferBusy] = useState(false);
  const [saveOfferError, setSaveOfferError] = useState<string | null>(null);
  const [saveOfferRedeemed, setSaveOfferRedeemed] = useState(false);
  const [cancelReasonOpen, setCancelReasonOpen] = useState(false);
  const [cancelReason, setCancelReason] = useState("");
  const [cancelNote, setCancelNote] = useState("");
  const [cancelBusy, setCancelBusy] = useState(false);
  const [contactEmail, setContactEmail] = useState("");
  const [contactMessage, setContactMessage] = useState("");
  const [contactSubmitBusy, setContactSubmitBusy] = useState(false);
  const [contactSubmitError, setContactSubmitError] = useState<string | null>(null);
  const [contactSubmitSuccess, setContactSubmitSuccess] = useState<string | null>(null);
  const appSemver = appUpdateConfig?.currentVersion ?? packageJson.version;
  const bootstrapFailureSignatureRef = useRef("");
  const mainWindowLastBlurAtRef = useRef<number | null>(null);
  const mainWindowLastSeenDayRef = useRef(formatDayKey(new Date()));
  // Session uptime anchor for the setup-stall check. The tray webview is
  // created at app launch and stays alive while the window is hidden, so first
  // render is a good stand-in for "when Headroom started running".
  const appStartedAtMsRef = useRef(Date.now());
  // Mirrors the account gate for the setup-stall closure: a signed-out or
  // unpaid user has zero savings by design, and both states already fire their
  // own daily notification. Undefined until pricing status first loads.
  const optimizationBlockedRef = useRef<boolean | undefined>(undefined);
  // Mirrors connector status for the same closure. Undefined until the startup
  // fetch lands, which keeps the no-traffic branch quiet rather than guessing.
  // Staleness in the "became verified" direction is harmless: that only happens
  // once traffic flows, and the no-traffic branch requires zero requests.
  const connectorsRef = useRef<ClientConnectorStatus[] | undefined>(undefined);
  // A forced setup-stall alert skips the day throttle, so this keeps it to one
  // showing per app run instead of resurrecting itself on every poll.
  const forcedSetupStallFiredRef = useRef(false);
  const appUpdateKnownVersionRef = useRef<string | null>(null);
  const appUpdateReadyToRestartRef = useRef(false);
  const appUpdateBusyRef = useRef(false);
  const appUpdateInstallBusyRef = useRef(false);
  const launcherHideAnimationMs = 320;
  const trayFocusPrewarmDelayMs = 250;
  const dashboardSignatureRef = useRef(serializeState(mockDashboard));
  const connectorsSignatureRef = useRef(serializeState([] as ClientConnectorStatus[]));
  const runtimeStatusSignatureRef = useRef(serializeState(null as RuntimeStatus | null));
  // Mirrors runtimeStatus.installed for closures (background update check) that
  // must not fire notifications while first-install bootstrap is still running.
  const runtimeInstalledRef = useRef(false);
  const bootstrapFailedRef = useRef(false);
  const claudeProjectsSignatureRef = useRef(serializeState([] as ClaudeCodeProject[]));
  // Mirror the server's price table into the pricing helpers before anything
  // reads a price this render. Idempotent and derived purely from state, so a
  // repeated render (StrictMode) is a no-op.
  setServerPlanPrices(pricingStatus?.planPrices);
  const upgradePlansState = getUpgradePlans(
    pricingAudience,
    pricingStatus?.claude.planTier ?? cachedPricing.planTier,
    pricingStatus?.recommendedSubscriptionTier ?? cachedPricing.recommendedSubscriptionTier,
    pricingStatus?.account?.subscriptionTier ?? cachedPricing.subscriptionTier,
    pricingStatus?.account?.subscriptionActive ?? false,
    pricingStatus?.launchDiscountActive ?? false,
    billingPeriod,
    pricingStatus?.account?.subscriptionAmountCents,
    pricingStatus?.account?.subscriptionBillingPeriod,
    pricingStatus?.account?.subscriptionRenewsAt,
    pricingStatus?.account?.subscriptionStartedAt,
    pricingStatus?.account?.subscriptionDiscountDuration,
    pricingStatus?.account?.subscriptionDiscountDurationInMonths,
    pricingStatus?.account?.subscriptionCancelAtPeriodEnd ?? false,
    pricingStatus?.account?.subscriptionEndsAt,
    pricingStatus?.activePercentOff ?? 0,
    pricingStatus?.introOffer ?? null,
    pricingStatus?.account?.subscriptionRenewalCents,
    pricingStatus?.account?.subscriptionRenewalEndsAt
  );
  const contactEmailValid = isValidEmailAddress(contactEmail);
  const authEmailValid = isValidEmailAddress(authEmail);
  const showInstallProgress =
    bootstrapping ||
    bootstrapProgress.running ||
    bootstrapProgress.complete ||
    bootstrapProgress.failed ||
    bootstrapProgress.overallPercent > 0;

  const isLastScreen =
    windowLabel === "launcher" && launcherStage === "post_install";
  // While the post-install screen is still waiting on the first savings,
  // a blur-hide would dismiss onboarding with no way back to it — the
  // launcher never reopens. Autohide only arms once savings have landed.
  const awaitingFirstSavings =
    dashboard.launchExperience === "first_run" &&
    dashboard.lifetimeEstimatedTokensSaved <= 0 &&
    dashboard.lifetimeEstimatedSavingsUsd <= 0;
  // Independent of launchExperience: any savings on record at all, which is
  // what retires the setup-stall watchdog below.
  const savingsEverRecorded =
    dashboard.lifetimeEstimatedTokensSaved > 0 || dashboard.lifetimeEstimatedSavingsUsd > 0;
  const forcedSetupStall = debugOverrides?.setupStall ?? null;
  useEffect(() => {
    if (!showHeadroomDetails || !headroomLogRef.current) {
      return;
    }
    headroomLogRef.current.scrollTop = headroomLogRef.current.scrollHeight;
  }, [showHeadroomDetails, headroomLogLines]);

  useEffect(() => {
    const timer = window.setTimeout(() => setHistoryLoadTimedOut(true), 20000);
    return () => window.clearTimeout(timer);
  }, []);

  useEffect(() => {
    if (!showRtkDetails || !rtkActivityRef.current) {
      return;
    }
    rtkActivityRef.current.scrollTop = rtkActivityRef.current.scrollHeight;
  }, [showRtkDetails, rtkActivityLines]);

  useEffect(() => {
    dashboardSignatureRef.current = serializeState(dashboard);
  }, [dashboard]);

  useEffect(() => {
    connectorsSignatureRef.current = serializeState(connectors);
  }, [connectors]);

  useEffect(() => {
    runtimeStatusSignatureRef.current = serializeState(runtimeStatus);
    runtimeInstalledRef.current = runtimeStatus?.installed === true;
  }, [runtimeStatus]);

  useEffect(() => {
    bootstrapFailedRef.current = bootstrapProgress.failed === true;
  }, [bootstrapProgress.failed]);

  useEffect(() => {
    claudeProjectsSignatureRef.current = serializeState(claudeProjects);
  }, [claudeProjects]);

  function applyDashboardIfChanged(next: DashboardState) {
    const nextSignature = serializeState(next);
    if (dashboardSignatureRef.current === nextSignature) {
      return;
    }
    dashboardSignatureRef.current = nextSignature;
    setDashboard(next);
  }

  function applyConnectorsIfChanged(nextConnectors: ClientConnectorStatus[]) {
    const next = withoutHiddenConnectors(nextConnectors);
    const nextSignature = serializeState(next);
    if (connectorsSignatureRef.current === nextSignature) {
      return;
    }
    connectorsSignatureRef.current = nextSignature;
    setConnectors(next);
  }

  function applyRuntimeStatusIfChanged(next: RuntimeStatus | null) {
    const nextSignature = serializeState(next);
    if (runtimeStatusSignatureRef.current === nextSignature) {
      return;
    }
    runtimeStatusSignatureRef.current = nextSignature;
    setRuntimeStatus(next);
  }

  function applyClaudeProjectsIfChanged(next: ClaudeCodeProject[]) {
    const nextSignature = serializeState(next);
    if (claudeProjectsSignatureRef.current === nextSignature) {
      return;
    }
    claudeProjectsSignatureRef.current = nextSignature;
    setClaudeProjects(next);
  }

  useEffect(() => {
    const unlistenPromise = listen<{ action: string | null }>(
      "notification-clicked",
      (event) => {
        const action = event.payload?.action ?? null;
        if (action === "update") {
          setShowAppUpdateDialog(true);
          return;
        }
        const view = notificationActionView(action);
        if (view) {
          setActiveView(view);
        }
      }
    );
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    setShowAllUpgradePlans(false);
    if (pricingAudience !== "individual") setBillingPeriod("annual");
  }, [pricingAudience]);

  // Open on the plan the subscriber actually has, not the monthly default:
  // showing an annual subscriber monthly prices misstates what they pay. Keyed
  // on the account's period alone, so it fires on load and on a plan change but
  // never fights the toggle.
  const accountBillingPeriod = pricingStatus?.account?.subscriptionBillingPeriod;
  useEffect(() => {
    if (accountBillingPeriod === "annual" || accountBillingPeriod === "monthly") {
      setBillingPeriod(accountBillingPeriod);
    }
  }, [accountBillingPeriod]);

  useEffect(() => {
    if (!pricingStatus?.authenticated) {
      desktopActivationSentRef.current = false;
    }
  }, [pricingStatus?.authenticated]);

  useEffect(() => {
    if (!pricingStatus) return;
    writeCachedPricing(cachePricingStatus(pricingStatus));
  }, [pricingStatus]);

  useEffect(() => {
    const STORAGE_KEY = "headroom:lastNotifiedMismatchTier";
    const mismatch = pricingStatus?.tierMismatch;
    // Don't clear the dedupe key when the mismatch reads null: the backend also
    // reports null on a poll where the Claude plan momentarily detects as
    // Unknown, and clearing there re-armed this notification to re-fire on the
    // next confident poll — that's what nagged users overnight. Leave the key;
    // it's overwritten only when we climb to a higher tier below.
    if (!mismatch) return;
    const rank: Record<string, number> = { pro: 1, max5x: 2, max20x: 3 };
    const previous = window.localStorage.getItem(STORAGE_KEY);
    // Notify on first detection and whenever the recommended tier climbs higher.
    if (previous !== null && (rank[mismatch.recommendedTier] ?? 0) <= (rank[previous] ?? 0)) {
      return;
    }
    const paidLabel = upgradePlanIntentLabel(mismatch.paidTier);
    const recommendedLabel = upgradePlanIntentLabel(mismatch.recommendedTier);
    const sourceLabel = tierRecommendationSourceLabel(mismatch.recommendedSource);
    // fireUpsellNudge self-throttles (quiet hours, twice/day, min gap); only
    // advance the dedupe key once it actually showed, or a quiet-hours skip
    // would silently mark this tier as notified and suppress it for good.
    void fireUpsellNudge(
      "Upgrade your Headroom plan",
      `Your ${sourceLabel} usage needs the Headroom ${recommendedLabel} plan, above your current ${paidLabel} plan. Upgrade to keep unlimited optimization.`
    ).then((fired) => {
      if (fired) window.localStorage.setItem(STORAGE_KEY, mismatch.recommendedTier);
    });
  }, [pricingStatus?.tierMismatch?.recommendedTier, pricingStatus?.tierMismatch]);

  useEffect(() => {
    const claudeConnector = getClaudeConnector(connectors);
    if (!claudeConnector?.installed) {
      return;
    }
    trackInstallMilestoneOnce("claude_code_detected", {
      enabled: claudeConnector.enabled,
      verified: claudeConnector.verified
    });
  }, [connectors]);

  useEffect(() => {
    const claudeConnector = getClaudeConnector(connectors);
    if (!claudeConnector?.enabled) {
      return;
    }
    trackInstallMilestoneOnce("optimization_enabled", {
      verified: claudeConnector.verified
    });
  }, [connectors]);

  useEffect(() => {
    if (dashboard.lifetimeRequests <= 0) {
      return;
    }
    trackInstallMilestoneOnce("first_optimized_request", {
      lifetime_requests: dashboard.lifetimeRequests,
      launch_experience: dashboard.launchExperience
    });
  }, [dashboard.launchExperience, dashboard.lifetimeRequests]);

  useEffect(() => {
    if (
      dashboard.lifetimeEstimatedTokensSaved <= 0 &&
      dashboard.lifetimeEstimatedSavingsUsd <= 0
    ) {
      return;
    }
    // Analytics only. The matching funnel beacon is sent from Rust
    // (get_dashboard_state), which retries every launch until it lands —
    // this localStorage gate is once-per-install with no retry, and losing
    // one POST here permanently undercounted the funnel finish line.
    trackInstallMilestoneOnce("first_savings_recorded", {
      lifetime_tokens_saved: dashboard.lifetimeEstimatedTokensSaved,
      lifetime_savings_usd: Number(dashboard.lifetimeEstimatedSavingsUsd.toFixed(4))
    });
  }, [dashboard.lifetimeEstimatedSavingsUsd, dashboard.lifetimeEstimatedTokensSaved]);

  useEffect(() => {
    let active = true;

    const runStartupChecks = async () => {
      const updateStartup = (phase: StartupPhase, percent: number, message: string) => {
        if (!active) {
          return;
        }
        setStartupPhase(phase);
        setStartupPercent((current) => Math.max(current, percent));
        setStartupCopy(message);
      };

      updateStartup("window", 12, "Opening launch window…");
      const label = getCurrentWindow().label;
      if (active) {
        if (label === "main" || label === "launcher") {
          setWindowLabel(label);
        } else {
          setWindowLabel("main");
        }
      }

      updateStartup("dashboard", 35, "Loading local dashboard state…");
      const dashboardResult = await loadDashboard();
      if (!active) {
        return;
      }
      applyDashboardIfChanged(dashboardResult);

      updateStartup("bootstrap", 58, "Checking runtime install state…");
      const bootstrapResult = await invoke<BootstrapProgress>("get_bootstrap_progress").catch(
        () => idleBootstrapProgress
      );
      if (!active) {
        return;
      }
      setBootstrapProgress(bootstrapResult);
      if (bootstrapResult.running) {
        setBootstrapping(true);
      }
      const initialStage = getInitialLauncherStage(
        label,
        bootstrapResult.complete,
        dashboardResult.bootstrapComplete,
        dashboardResult.launchExperience
      );
      if (initialStage) {
        setLauncherStage(initialStage);
      }

      updateStartup("runtime", 80, "Preparing Headroom runtime…");
      const [runtimeResult, pricingResult, launchFlagsResult] = await Promise.all([
        invoke<RuntimeStatus>("get_runtime_status").catch(() => null),
        invoke<HeadroomPricingStatus>("get_headroom_pricing_status").catch(() => null),
        // Local cached read, never blocks on the network. Fetched before
        // startupReady so TermsGate sees the paywall-first flag on its very
        // first render (a late fetch would flicker the auth section in).
        invoke<LaunchFlags>("get_launch_flags").catch(() => null),
        refreshConnectors(),
      ]);
      if (!active) {
        return;
      }
      if (runtimeResult) {
        applyRuntimeStatusIfChanged(runtimeResult);
      }
      if (pricingResult) {
        setPricingStatus(pricingResult);
      }
      if (launchFlagsResult) {
        setLaunchFlags(launchFlagsResult);
      }

      updateStartup(
        "ready",
        95,
        label === "launcher" ? "Preparing launch checklist…" : "Preparing tray dashboard…"
      );
      window.setTimeout(() => {
        if (!active) {
          return;
        }
        setStartupPercent(100);
        setStartupCopy("Headroom is ready.");
        setStartupReady(true);
      }, 120);
    };

    void runStartupChecks();

    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (startupReady) {
      return;
    }

    const phaseCaps: Record<StartupPhase, number> = {
      window: 28,
      dashboard: 54,
      bootstrap: 76,
      runtime: 92,
      ready: 99
    };
    const cap = phaseCaps[startupPhase];

    const interval = window.setInterval(() => {
      setStartupPercent((current) => {
        if (current >= cap) {
          return current;
        }
        return Math.min(cap, current + (current < 20 ? 2 : 1));
      });
    }, 260);

    return () => {
      window.clearInterval(interval);
    };
  }, [startupPhase, startupReady]);

  useEffect(() => {
    if (!bootstrapping) {
      return;
    }

    let active = true;
    let completionHandled = false;
    let unlisten: (() => void) | undefined;
    const detach = () => {
      const fn = unlisten;
      unlisten = undefined;
      fn?.();
    };

    const handleProgress = async (progress: BootstrapProgress) => {
      if (!active) {
        return;
      }

      setBootstrapProgress(progress);

      if (progress.failed) {
        const failureReport = buildBootstrapFailureReport(progress);
        const failureSignature = bootstrapFailureSignature(failureReport);
        if (bootstrapFailureSignatureRef.current !== failureSignature) {
          bootstrapFailureSignatureRef.current = failureSignature;
          reportBootstrapFailure(failureReport);
        }
        setBootstrapError(progress.message);
        setBootstrapping(false);
        completionHandled = true;
        detach();
        return;
      }

      if (progress.complete && !completionHandled) {
        completionHandled = true;
        detach();
        setBootstrapping(false);
        const latestDashboard = await loadDashboard();
        if (!active) {
          return;
        }
        applyDashboardIfChanged(latestDashboard);
        // Always land on the install step after a bootstrap completes during
        // this session, regardless of launchExperience. The install step's
        // Continue button is gated on runtime.running, so it handles both the
        // readiness wait and the "Headroom installation present" confirmation
        // for Resume users whose launch_count > 1 (e.g., they reinstalled the
        // app without clearing ~/Library/Application Support/Headroom).
        if (windowLabel === "launcher") {
          setLauncherStage("install");
        }
      }
    };

    void listen<BootstrapProgress>("bootstrap_progress", (event) => {
      void handleProgress(event.payload);
    }).then((fn) => {
      if (!active || completionHandled) {
        fn();
        return;
      }
      unlisten = fn;
    });

    // Prime with the current state in case we subscribed mid-flight or the
    // bootstrap already completed before the listener attached.
    void invoke<BootstrapProgress>("get_bootstrap_progress")
      .then((progress) => handleProgress(progress))
      .catch(() => {});

    return () => {
      active = false;
      detach();
    };
  }, [bootstrapping]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    void listen<RuntimeUpgradeProgress>("runtime_upgrade_progress", (event) => {
      if (!active) return;
      setRuntimeUpgradeProgress(event.payload);
    }).then((fn) => {
      if (!active) {
        fn();
        return;
      }
      unlisten = fn;
    });

    void invoke<RuntimeUpgradeProgress>("get_runtime_upgrade_progress")
      .then((progress) => {
        if (active) setRuntimeUpgradeProgress(progress);
      })
      .catch(() => {});

    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  // Hand off cleanly once the runtime upgrade finishes: show the success
  // state briefly, then drop the progress object back to idle so the
  // launcher stops rendering the upgrade UI and falls through to whichever
  // window content the user should see next. We also nudge the launcher
  // stage to post_install since bootstrapComplete only gets checked at
  // startup otherwise.
  useEffect(() => {
    if (!runtimeUpgradeProgress.complete || runtimeUpgradeProgress.failed) {
      return;
    }
    const timeout = window.setTimeout(() => {
      setRuntimeUpgradeProgress(idleRuntimeUpgradeProgress);
      if (windowLabel === "launcher") {
        setLauncherStage("post_install");
      }
      // Refresh runtime status so the rest of the app picks up the
      // freshly-installed version immediately.
      void invoke<RuntimeStatus>("get_runtime_status")
        .then((status) => applyRuntimeStatusIfChanged(status))
        .catch(() => {});
      void loadDashboard()
        .then((next) => applyDashboardIfChanged(next))
        .catch(() => {});
    }, 2500);
    return () => window.clearTimeout(timeout);
  }, [runtimeUpgradeProgress.complete, runtimeUpgradeProgress.failed, windowLabel]);

  useEffect(() => {
    if (windowLabel !== "launcher" || launcherStage !== "client_setup") {
      return;
    }
    void refreshConnectors();
  }, [windowLabel, launcherStage]);

  // Paywall stage: poll pricing status so the tier recommendation appears as
  // soon as the first proxied request lands, and so a completed checkout
  // (deep link or the 5-minute checkout poll) flips subscriptionActive here.
  useEffect(() => {
    if (LOCAL_COMMUNITY_EDITION || windowLabel !== "launcher" || launcherStage !== "paywall") {
      return;
    }
    trackAnalyticsEvent("paywall_shown", {});
    let active = true;
    const poll = () => {
      invoke<HeadroomPricingStatus>("get_headroom_pricing_status")
        .then((status) => {
          if (active) setPricingStatus(status);
        })
        .catch(() => {});
    };
    poll();
    const interval = window.setInterval(poll, 3000);
    return () => {
      active = false;
      window.clearInterval(interval);
    };
  }, [windowLabel, launcherStage]);

  // Post-checkout resume: the subscription is live, so move to the install
  // stage and start the (now gate-passing) bootstrap automatically.
  useEffect(() => {
    if (windowLabel !== "launcher" || launcherStage !== "paywall") {
      return;
    }
    if (pricingStatus?.account?.subscriptionActive) {
      trackAnalyticsEvent("paywall_checkout_completed", {
        plan_id: pricingStatus.account.subscriptionTier ?? undefined
      });
      setLauncherStage("install");
      void handleBootstrap();
    }
  }, [windowLabel, launcherStage, pricingStatus?.account?.subscriptionActive]);

  useEffect(() => {
    if (windowLabel !== "launcher" || launcherStage !== "proxy_verify") {
      return;
    }

    let active = true;
    const poll = () => {
      void (async () => {
        try {
          const countsCommand = interceptOnlyVerify
            ? "get_intercept_request_counts_by_agent"
            : "get_headroom_request_counts_by_agent";
          const [runtime, counts] = await Promise.all([
            interceptOnlyVerify
              ? Promise.resolve<RuntimeStatus | null>(null)
              : invoke<RuntimeStatus>("get_runtime_status").catch(() => null),
            invoke<Record<string, number> | null>(countsCommand).catch(() => null)
          ]);

          if (!active) {
            return;
          }

          if ((!interceptOnlyVerify && runtime?.proxyReachable !== true) || counts === null) {
            // On this screen the runtime is always app-managed and coming up
            // (the pre-install case routes through interceptOnlyVerify). First
            // launch synchronously downloads the compression/embedder models
            // before the backend binds, so `proxyReachable` is false for a
            // minute or more on a perfectly healthy install. Keep it calm and
            // informational — only a hard startup fault is a real error.
            const startupError = interceptOnlyVerify ? null : runtime?.startupError;
            setProxyVerificationHint(
              interceptOnlyVerify
                ? { text: t("launcher.waitingSetupTraffic"), tone: "info" }
                : startupError
                ? { text: t("launcher.startFailed", { error: startupError }), tone: "error" }
                : {
                    text: t("launcher.finishingSetup"),
                    tone: "info"
                  }
            );
            return;
          }

          setProxyVerificationHint(null);

          // Capture the baseline on the first reachable poll. Anchoring on a
          // null/unreachable reading would let a later "proxy came up" jump
          // (0 → N) look like new traffic.
          if (proxyVerificationRequestAnchorRef.current === null) {
            proxyVerificationRequestAnchorRef.current = counts;
            return;
          }

          // Attribute traffic per client: a prompt sent to Claude Code must not
          // flip the Codex row (and vice versa). The proxy keys agents as
          // `claude-code` / `codex`; our rows use `claude_code` / `codex`.
          const anchor = proxyVerificationRequestAnchorRef.current;
          setProxyVerificationRows((current) =>
            current.map((row) => {
              if (row.state === "verified") {
                return row;
              }
              const agentKey = row.clientId.replace(/_/g, "-");
              const now = counts[agentKey] ?? 0;
              const base = anchor[agentKey] ?? 0;
              return now > base
                ? { ...row, state: "verified", message: t("launcher.requestReceived") }
                : row;
            })
          );
        } catch {
          if (active) {
            setProxyVerificationHint({ text: t("launcher.waitingActivity"), tone: "info" });
          }
        }
      })();
    };
    poll();
    const interval = window.setInterval(poll, 1000);

    return () => {
      active = false;
      window.clearInterval(interval);
    };
  }, [windowLabel, launcherStage, interceptOnlyVerify, t]);

  // Warm the bootstrap download cache while the user is still signing up.
  // Download-only: nothing is installed until they consent on the install
  // step, and the Rust side no-ops when the runtime already exists, so this
  // is safe to fire on every launcher mount.
  useEffect(() => {
    if (windowLabel !== "launcher") return;
    void invoke("prefetch_bootstrap_artifacts").catch(() => {});
  }, [windowLabel]);

  // One beacon per launcher stage the user reaches. Single source for the
  // "stage shown" funnel steps; sub-steps (client_setup_applied, proxy_verified,
  // email_code_*) fire from their own handlers.
  useEffect(() => {
    if (windowLabel !== "launcher") return;
    const step = LAUNCHER_STAGE_STEP[launcherStage];
    if (step) reportFunnelStep(step);
  }, [windowLabel, launcherStage]);

  // signup_gate_shown: a new (unauthenticated) user is looking at the
  // terms + email sign-up gate -- the true top of the new-user funnel and the
  // "saw it, never typed an email" bucket. Gated to the paywall-first launcher
  // so existing users re-accepting terms in the main window don't count.
  const signupGateVisible =
    windowLabel === "launcher" &&
    paywallFirstFlow &&
    pricingStatus?.authenticated !== true &&
    needsTermsAcceptance(dashboard.requiredTermsVersion, dashboard.acceptedTermsVersion);
  useEffect(() => {
    if (signupGateVisible) reportFunnelStep("signup_gate_shown");
  }, [signupGateVisible]);

  // proxy_verified: every enabled client's test traffic reached the proxy.
  useEffect(() => {
    if (windowLabel !== "launcher" || launcherStage !== "proxy_verify") return;
    if (
      proxyVerificationRows.length > 0 &&
      proxyVerificationRows.every((row) => row.state === "verified")
    ) {
      // Persist for the main window: its own phase poller honors this marker,
      // so the tray doesn't ask the user to verify a second time right after
      // onboarding just did.
      setConnectorTrafficVerified(true);
      reportFunnelStep("proxy_verified");
    }
  }, [windowLabel, launcherStage, proxyVerificationRows]);

  useEffect(() => {
    if (!showInstallProgress) {
      return;
    }

    const signature = `${bootstrapProgress.currentStep}|${bootstrapProgress.running}|${bootstrapProgress.complete}|${bootstrapProgress.failed}`;
    if (signature === stepSignature) {
      return;
    }

    setStepSignature(signature);
    setStepStartedAtMs(Date.now());
    setStepEtaSeedSeconds(bootstrapProgress.currentStepEtaSeconds);
    setStepBasePercent(bootstrapProgress.overallPercent);
  }, [bootstrapProgress, showInstallProgress, stepSignature]);

  // Reaching the final screen IS the end of onboarding, so satisfy the tray's
  // gate here instead of on one specific exit. Every other way off this screen
  // -- the blur-autohide right below, closing the window, quitting -- used to
  // leave `setup_wizard_complete` false while the launcher kept landing here
  // (getInitialLauncherStage sends any returning user straight to
  // post_install). The tray gate then disagreed with the stage machine and
  // reopened this same screen on every click, forever.
  useEffect(() => {
    if (!isLastScreen) return;
    void invoke("complete_setup_wizard");
  }, [isLastScreen]);

  useEffect(() => {
    if (!isLastScreen || awaitingFirstSavings) return;
    let unlisten: (() => void) | undefined;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (!focused) triggerHide();
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => unlisten?.();
  }, [isLastScreen, awaitingFirstSavings]);

  const optimizationBlocked = pricingStatus
    ? pricingStatus.needsAuthentication || !pricingStatus.optimizationAllowed
    : undefined;
  useEffect(() => {
    optimizationBlockedRef.current = optimizationBlocked;
  }, [optimizationBlocked]);

  // Test overrides (HEADROOM_FAKE_* env vars, RC builds only). Null on every
  // shipped stable build and on any RC launched without the vars, so this
  // resolves to "no overrides" in production.
  useEffect(() => {
    if (windowLabel !== "main") {
      return;
    }
    let active = true;
    void invoke<DebugOverrides>("get_debug_overrides")
      .then((overrides) => {
        if (active) setDebugOverrides(overrides);
      })
      .catch(() => {
        // Older backend or command unavailable: behave as if unset.
      });
    return () => {
      active = false;
    };
  }, [windowLabel]);

  // Setup-stall watchdog: Headroom running for a long stretch with nothing
  // saved almost always means the hookup is incomplete, not that the user was
  // idle the whole time. Runs regardless of tray visibility (the other
  // dashboard pollers are focus-gated, so without this a broken install stays
  // silent while the window is closed), and stops for good once any savings
  // land. `maybeFireSetupStallAlert` owns the once-per-local-day throttle.
  useEffect(() => {
    if (windowLabel !== "main") {
      return;
    }
    // Forced runs are for eyeballing the modal, so they ignore the "already
    // earning savings" retirement that would otherwise never let it show.
    if (!forcedSetupStall && savingsEverRecorded) {
      // Savings have landed: retire the banner line too, or it would stick at
      // whatever it last said once this effect stops running.
      setStallBannerLine(null);
      return;
    }

    let active = true;
    const check = async () => {
      const uptimeMs = Date.now() - appStartedAtMsRef.current;
      if (!forcedSetupStall && uptimeMs < SETUP_STALL_EARLIEST_MS) {
        return;
      }
      // Once per app run when forced: the alert bypasses the day throttle, so
      // without this it would reappear on every tick after being dismissed.
      if (forcedSetupStall && forcedSetupStallFiredRef.current) {
        return;
      }
      const latest = await loadDashboard().catch(() => null);
      if (!active || !latest) {
        return;
      }
      applyDashboardIfChanged(latest);
      // Banner first: it is a passive line with its own (wider) gates, and it
      // must not depend on the modal's once-per-day throttle below.
      setStallBannerLine(
        setupStallBannerLine(latest, uptimeMs, {
          optimizationBlocked: optimizationBlockedRef.current,
          connectors: connectorsRef.current,
          forceKind: forcedSetupStall,
        })
      );
      const alert = await maybeFireSetupStallAlert(latest, uptimeMs, {
        optimizationBlocked: optimizationBlockedRef.current,
        connectors: connectorsRef.current,
        forceKind: forcedSetupStall,
      });
      if (active && alert) {
        if (forcedSetupStall) {
          forcedSetupStallFiredRef.current = true;
        }
        setSetupStall(alert);
      }
    };

    void check();
    const interval = window.setInterval(() => {
      void check();
    }, SETUP_STALL_CHECK_INTERVAL_MS);

    return () => {
      active = false;
      window.clearInterval(interval);
    };
  }, [windowLabel, savingsEverRecorded, forcedSetupStall]);

  useEffect(() => {
    if (windowLabel !== "main" || !trayWindowFocused) {
      return;
    }

    void refreshRuntimeStatus();
    const interval = window.setInterval(() => {
      void refreshRuntimeStatus();
    }, 3000);

    return () => window.clearInterval(interval);
  }, [windowLabel, trayWindowFocused]);

  // Poll runtime status while the install step is visible so the Continue
  // button unlocks as soon as headroom is fully running (same signal the
  // tray uses for its solid icon: installed && !paused && proxy_reachable).
  // On a cold first install the Gatekeeper scan can finish after
  // mark_bootstrap_complete fires, and the main-window poller doesn't run
  // on the launcher.
  useEffect(() => {
    if (windowLabel !== "launcher" || launcherStage !== "install") {
      return;
    }
    if (runtimeStatus?.running === true || runtimeStatus?.bypassed === true) {
      return;
    }

    void refreshRuntimeStatus();
    const interval = window.setInterval(() => {
      void refreshRuntimeStatus();
    }, 1000);

    return () => window.clearInterval(interval);
  }, [windowLabel, launcherStage, runtimeStatus?.running, runtimeStatus?.bypassed]);

  useEffect(() => {
    if (windowLabel !== "main") {
      return;
    }

    let unlisten: (() => void) | undefined;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        setTrayWindowFocused(focused);
        const now = new Date();
        const nowDayKey = formatDayKey(now);

        if (!focused) {
          mainWindowLastBlurAtRef.current = now.getTime();
          mainWindowLastSeenDayRef.current = nowDayKey;
          return;
        }

        const inactiveForMs = mainWindowLastBlurAtRef.current
          ? now.getTime() - mainWindowLastBlurAtRef.current
          : null;
        // A pending Codex restart can resolve during a quick alt-tab, so recheck
        // that state immediately; keep the old 30s gate for steady connectors.
        const restartMayHaveResolved =
          connectorsRef.current?.some(
            (connector) => connector.restartRequired === true
          ) ?? false;
        if (restartMayHaveResolved || inactiveForMs === null || inactiveForMs >= 30_000) {
          void refreshConnectors();
        }

        const dayRolledOver = nowDayKey !== mainWindowLastSeenDayRef.current;
        if ((inactiveForMs ?? 0) >= 3_600_000 || dayRolledOver) {
          setChartResetSignal((current) => current + 1);
        }

        mainWindowLastBlurAtRef.current = null;
        mainWindowLastSeenDayRef.current = nowDayKey;
      })
      .then((fn) => {
        unlisten = fn;
      });

    return () => unlisten?.();
  }, [windowLabel]);

  useEffect(() => {
    if (!startupReady) {
      return;
    }
    void refreshAppUpdateConfiguration();
  }, [startupReady]);

  useEffect(() => {
    if (
      !startupReady ||
      windowLabel !== "main" ||
      !appUpdateConfig
    ) {
      return;
    }
    if (!appUpdateConfig.enabled || appUpdateConfig.configurationError) {
      return;
    }

    const runBackgroundCheck = () => {
      if (
        appUpdateReadyToRestartRef.current ||
        appUpdateBusyRef.current ||
        appUpdateInstallBusyRef.current ||
        // Don't fire an "update available" notification while first-install
        // bootstrap is still building the runtime — it piles onto the install
        // window. Resume once the runtime is installed.
        //
        // A *failed* bootstrap is the exception, and suppressing it there was
        // a trap: when the cause is a bad pin in the lock we shipped, every
        // retry fails identically and a newer build is the only fix, so the
        // one screen that needed the updater most was the one screen that
        // never checked (Sentry RUST-1G — users on 0.8.1 retried for hours
        // while 0.8.2 sat in the manifest with the fix).
        (!runtimeInstalledRef.current && !bootstrapFailedRef.current)
      ) {
        return;
      }
      void checkForAppUpdate({
        background: true,
        knownUpdateVersion: appUpdateKnownVersionRef.current,
      });
    };

    const timer = window.setTimeout(runBackgroundCheck, APP_UPDATE_BACKGROUND_INITIAL_DELAY_MS);
    const interval = window.setInterval(runBackgroundCheck, APP_UPDATE_BACKGROUND_CHECK_INTERVAL_MS);

    return () => {
      window.clearTimeout(timer);
      window.clearInterval(interval);
    };
  }, [appUpdateConfig, startupReady, windowLabel]);

  useEffect(() => {
    appUpdateKnownVersionRef.current = appUpdateAvailable?.version ?? null;
  }, [appUpdateAvailable?.version]);

  useEffect(() => {
    appUpdateReadyToRestartRef.current = appUpdateReadyToRestart;
  }, [appUpdateReadyToRestart]);

  useEffect(() => {
    appUpdateBusyRef.current = appUpdateBusy;
  }, [appUpdateBusy]);

  useEffect(() => {
    appUpdateInstallBusyRef.current = appUpdateInstallBusy;
  }, [appUpdateInstallBusy]);

  useEffect(() => {
    if (
      (activeView !== "addons" && activeView !== "settings") ||
      addonUpdatesChecked ||
      addonUpdateBusy
    ) {
      return;
    }
    void refreshAddonUpdates();
  }, [activeView, addonUpdatesChecked, addonUpdateBusy]);

  useEffect(() => {
    if (activeView !== "settings") {
      return;
    }
    void Promise.all([
      refreshConnectors(),
      refreshRuntimeStatus(),
      appUpdateConfig ? Promise.resolve() : refreshAppUpdateConfiguration()
    ]);
    void invoke<boolean>("get_autostart_enabled")
      .then((enabled) => setAutostartEnabled(enabled))
      .catch(() => setAutostartEnabled(false));
  }, [activeView]);

  useEffect(() => {
    if (activeView !== "optimization") {
      return;
    }
    void invoke<boolean>("get_auto_learn_enabled")
      .then((enabled) => setAutoLearnEnabled(enabled))
      .catch(() => setAutoLearnEnabled(true));
  }, [activeView]);

  async function handleAutoLearnToggle(nextEnabled: boolean) {
    setAutoLearnBusy(true);
    try {
      // Persist the preference without stopping an active proxy request. The
      // next normal proxy start picks up the flag.
      const enabled = await invoke<boolean>("set_auto_learn_enabled", { enabled: nextEnabled });
      setAutoLearnEnabled(enabled);
    } catch (error) {
      console.error("Failed to update auto-learning", error);
    } finally {
      setAutoLearnBusy(false);
    }
  }

  // Auto-learning gates every pattern behind minEvidence sightings, so a new
  // user sees zero learnings for a while and reads the silence as "broken"
  // (beta feedback). Show observation progress when the backend reports it,
  // and explain the gate either way.
  const learnerProgress = dashboard.learnerProgress;
  const autoLearnMeta =
    learnerProgress && learnerProgress.pendingPatterns > 0
      ? t("learn.autoProgress", {
          patterns: learnerProgress.pendingPatterns,
          evidence: learnerProgress.minEvidence,
        })
      : t("learn.autoDescription");

  async function handleAutostartToggle(nextEnabled: boolean) {
    setAutostartBusy(true);
    try {
      const enabled = await invoke<boolean>("set_autostart_enabled", { enabled: nextEnabled });
      setAutostartEnabled(enabled);
    } catch (error) {
      console.error("Failed to update autostart", error);
    } finally {
      setAutostartBusy(false);
    }
  }

  async function handleRtkToggle(nextEnabled: boolean) {
    const id = "rtk";
    setRtkBusy(true);
    setAddonBusyById((current) =>
      setAddonOperationMessage(
        current,
        id,
        t(nextEnabled ? "addons.enabling" : "addons.disabling", { name: "RTK" })
      )
    );
    setAddonResultById((current) => clearAddonOperationMessage(current, id));
    setAddonErrorById((current) => clearAddonOperationMessage(current, id));
    try {
      await invoke<boolean>("set_rtk_enabled", { enabled: nextEnabled });
      await refreshRuntimeStatus();
      const message = nextEnabled ? undefined : t("addons.disabled", { name: "RTK" });
      if (message) {
        setAddonResultById((current) => setAddonOperationMessage(current, id, message));
      }
    } catch (error) {
      console.error("Failed to update RTK", error);
      setAddonErrorById((current) =>
        setAddonOperationMessage(
          current,
          id,
          describeInvokeError(error, t("messages.localOperationFailed"))
        )
      );
    } finally {
      setRtkBusy(false);
      setAddonBusyById((current) => clearAddonOperationMessage(current, id));
    }
  }

  async function refreshAddonUpdates() {
    const generation = addonUpdateGenerationRef.current;
    setAddonUpdateBusy(true);
    setAddonUpdateCheckFailed(false);
    try {
      const checks = await invoke<AddonUpdateCheck[]>("check_addon_updates");
      if (!Array.isArray(checks)) {
        throw new Error("invalid addon update response");
      }
      if (generation !== addonUpdateGenerationRef.current) {
        return;
      }
      setAddonUpdateChecks(checks);
      setAddonUpdateCheckFailed(checks.every((check) => Boolean(check.error)));
    } catch (error) {
      console.error("Failed to check addon updates", error);
      if (generation !== addonUpdateGenerationRef.current) {
        return;
      }
      // A failed check must not leave the previous successful result visible.
      setAddonUpdateChecks([]);
      setAddonUpdateCheckFailed(true);
    } finally {
      if (generation === addonUpdateGenerationRef.current) {
        setAddonUpdatesChecked(true);
        setAddonUpdateBusy(false);
      } else {
        // The action that invalidated this request still needs the next
        // effect pass to launch a fresh check.
        setAddonUpdateBusy(false);
      }
    }
  }

  function invalidateAddonUpdateChecks() {
    addonUpdateGenerationRef.current += 1;
    setAddonUpdateChecks([]);
    setAddonUpdatesChecked(false);
    setAddonUpdateCheckFailed(false);
  }

  async function handleHeadroomCliUpdate(version?: string | null) {
    setCliUpdateError(null);
    setRuntimeUpgradeProgress({
      ...idleRuntimeUpgradeProgress,
      running: true,
      currentStep: t("addons.updating", { name: t("settings.headroomCli") }),
      message: t("addons.updating", { name: t("settings.headroomCli") }),
      fromVersion: headroomTool?.version ?? null,
      toVersion: version ?? null,
    });
    try {
      await invoke("update_headroom_cli", { version: version ?? null });
      for (let attempt = 0; attempt < 60; attempt += 1) {
        await new Promise((resolve) => window.setTimeout(resolve, 500));
        const progress = await invoke<RuntimeUpgradeProgress>("get_runtime_upgrade_progress");
        setRuntimeUpgradeProgress(progress);
        if (!progress.running || progress.complete || progress.failed) {
          if (progress.failed) {
            setCliUpdateError(progress.message || t("messages.localOperationFailed"));
          }
          break;
        }
      }
      try {
        const refreshed = await invoke<DashboardState>("get_dashboard_state");
        if (refreshed && Array.isArray(refreshed.tools)) {
          setDashboard(refreshed);
        }
      } catch (refreshError) {
        console.error("Failed refresh dashboard after Headroom CLI update", refreshError);
      }
      invalidateAddonUpdateChecks();
    } catch (error) {
      setCliUpdateError(describeInvokeError(error, t("messages.localOperationFailed")));
    }
  }

  async function handleUninstall() {
    setUninstallBusy(true);
    setUninstallError(null);
    try {
      await invoke<string[]>("uninstall_and_quit");
    } catch (error) {
      setUninstallError(
        typeof error === "string" ? error : "Uninstall failed. Please try again."
      );
      setUninstallBusy(false);
    }
  }

  useEffect(() => {
    if (activeView !== "home" || !trayWindowFocused) {
      return;
    }

    let active = true;
    const refreshDashboard = () => {
      void loadDashboard()
        .then((next) => {
          if (!active) return;
          applyDashboardIfChanged(next);
        })
        .catch(() => {
          // keep last known state
        });
    };

    refreshDashboard();
    const interval = window.setInterval(refreshDashboard, 5000);
    return () => {
      active = false;
      window.clearInterval(interval);
    };
  }, [activeView, trayWindowFocused]);

  // Track whether the user has ever visited a heavy-data tab this session.
  // Once true, stays true until app restart — the pre-warm below is gated
  // on it so Home-only users don't pay its cost on every tray focus.
  useEffect(() => {
    if (activeView === "notifications" || activeView === "optimization") {
      setHeavyTabEverOpened(true);
    }
    // Once per app run, mirroring app_started's cadence so adoption ratios
    // (activity users / app_started users) compare like with like.
    if (activeView === "notifications" && !activityTabTrackedRef.current) {
      activityTabTrackedRef.current = true;
      trackAnalyticsEvent("activity_tab_opened");
    }
  }, [activeView]);

  // Pre-warm Optimize + Activity data the moment the tray gains focus, so
  // switching tabs reveals already-populated content instead of triggering
  // a fresh ~500ms Python subprocess spawn and layout flash. The tab-scoped
  // effects below still run and keep data fresh — they just hit the Rust
  // cache now instead of spawning a cold Python process. Gated on
  // `heavyTabEverOpened` so users who only use Home never trigger it.
  useEffect(() => {
    if (
      windowLabel !== "main" ||
      !trayWindowFocused ||
      !heavyTabEverOpened ||
      activeView === "notifications" ||
      activeView === "optimization"
    ) {
      return;
    }

    let active = true;
    const timeout = window.setTimeout(() => {
      if (!active) {
        return;
      }
      void refreshClaudeProjects();
      void refreshHeadroomLearnPrereq();
      invoke<ActivityFeedResponse>("get_activity_feed")
        .then((next) => {
          if (!active) return;
          setActivityFeed((prev) =>
            activityFeedSignature(prev) === activityFeedSignature(next) ? prev : next
          );
          setActivityFeedError(null);
        })
        .catch(() => {
          // Swallow: the tab-active poll will surface any real error once the
          // user opens Activity. Pre-warm failures shouldn't flash a banner.
        })
        .finally(() => {
          if (!active) return;
          setActivityFeedLoaded(true);
        });
    }, trayFocusPrewarmDelayMs);

    return () => {
      active = false;
      window.clearTimeout(timeout);
    };
  }, [windowLabel, trayWindowFocused, heavyTabEverOpened, activeView]);

  useEffect(() => {
    if (activeView !== "notifications" || !trayWindowFocused) {
      return;
    }
    let active = true;
    const refreshFeed = () => {
      invoke<ActivityFeedResponse>("get_activity_feed")
        .then((next) => {
          if (!active) return;
          setActivityFeed((prev) =>
            activityFeedSignature(prev) === activityFeedSignature(next) ? prev : next
          );
          setActivityFeedError(null);
        })
        .catch((err) => {
          if (!active) return;
          setActivityFeedError(
            err instanceof Error ? err.message : "Could not load activity feed."
          );
        })
        .finally(() => {
          if (!active) return;
          setActivityFeedLoaded(true);
        });
    };
    refreshFeed();
    const interval = window.setInterval(refreshFeed, 4000);
    return () => {
      active = false;
      window.clearInterval(interval);
    };
  }, [activeView, trayWindowFocused]);

  useEffect(() => {
    if (activeView !== "home" || !startupReady) {
      return;
    }
    void Promise.all([refreshConnectors(), refreshRuntimeStatus()]);
  }, [activeView, startupReady]);

  useEffect(() => {
    if (claudeProjects.length === 0) {
      setSelectedClaudeProjectPath(null);
      return;
    }

    setSelectedClaudeProjectPath((current) => {
      if (current && claudeProjects.some((project) => project.projectPath === current)) {
        return current;
      }
      return claudeProjects[0].projectPath;
    });
  }, [claudeProjects]);

  useEffect(() => {
    if (activeView !== "optimization") {
      return;
    }
    void Promise.all([refreshClaudeProjects(), refreshHeadroomLearnPrereq()]);
  }, [activeView]);

  useEffect(() => {
    if (activeView !== "optimization" || !trayWindowFocused) {
      return;
    }

    let active = true;
    const refreshLearnStatus = () => {
      void invoke<HeadroomLearnStatus>("get_headroom_learn_status", {
        projectPath: selectedClaudeProjectPath
      })
        .then((status) => {
          if (active) {
            setHeadroomLearnStatus(status);
          }
        })
        .catch(() => {
          if (active) {
            setHeadroomLearnStatus((current) => ({
              ...current,
              running: false,
              summary: "Could not read headroom learn status."
            }));
          }
        });
    };

    refreshLearnStatus();
    const interval = window.setInterval(
      refreshLearnStatus,
      headroomLearnStatus.running ? 900 : 3200
    );
    return () => {
      active = false;
      window.clearInterval(interval);
    };
  }, [activeView, selectedClaudeProjectPath, headroomLearnStatus.running, trayWindowFocused]);

  useEffect(() => {
    if (activeView !== "upgrade") {
      setUpgradeActionError(null);
    }
  }, [activeView]);

  useEffect(() => {
    const wasRunning = previousHeadroomLearnRunningRef.current;
    previousHeadroomLearnRunningRef.current = headroomLearnStatus.running;

    if (!wasRunning || headroomLearnStatus.running) {
      return;
    }

    if (headroomLearnStatus.success && headroomLearnStatus.projectPath) {
      const completedAt =
        headroomLearnStatus.lastRunAt ??
        headroomLearnStatus.finishedAt ??
        new Date().toISOString();
      setClaudeProjects((current) =>
        current.map((project) =>
          project.projectPath === headroomLearnStatus.projectPath
            ? {
                ...project,
                lastLearnRanAt: completedAt,
                hasPersistedLearnings: true,
                activeDaysSinceLastLearn: 0
              }
            : project
        )
      );
    }

    void refreshClaudeProjects();
  }, [
    headroomLearnStatus.finishedAt,
    headroomLearnStatus.lastRunAt,
    headroomLearnStatus.projectPath,
    headroomLearnStatus.running,
    headroomLearnStatus.success
  ]);

  const claudeProjectPathsKey = claudeProjects
    .map((project) => project.projectPath)
    .sort()
    .join("\t");
  // Batched applied-patterns fetch: one IPC instead of one per OptimizePanel.
  useEffect(() => {
    if (activeView !== "optimization") {
      return;
    }
    const paths = claudeProjectPathsKey === "" ? [] : claudeProjectPathsKey.split("\t");
    if (paths.length === 0) {
      setOptimizeAppliedByProject({});
      return;
    }
    let active = true;
    invoke<Record<string, AppliedPatterns>>("list_applied_patterns_for_projects", {
      projectPaths: paths,
    })
      .then((result) => {
        if (!active) return;
        setOptimizeAppliedByProject(result);
      })
      .catch(() => {
        if (!active) return;
        setOptimizeAppliedByProject(null);
      });
    return () => {
      active = false;
    };
  }, [
    activeView,
    claudeProjectPathsKey,
    headroomLearnStatus.finishedAt,
    optimizeAppliedRefreshTick,
  ]);

  // Keep connectorPhase in sync with the connector enabled state from the backend.
  // Any supported connector (Claude Code, Codex, ...) being enabled counts as
  // "connected" — the request-count poller below is connector-agnostic.
  const anyConnectorEnabled = hasEnabledConnector(connectors);

  // Which agents Headroom Learn should offer, driven by the enabled connectors.
  const claudeLearnEnabled = getClaudeConnector(connectors)?.enabled ?? false;
  const codexLearnEnabled = aggregateClientConnectors(connectors).some(
    (connector) => connector.clientId === "codex" && connector.enabled
  );
  const opencodeLearnEnabled = aggregateClientConnectors(connectors).some(
    (connector) => connector.clientId === "opencode" && connector.enabled
  );
  const grokLearnEnabled = aggregateClientConnectors(connectors).some(
    (connector) => connector.clientId === "grok_build" && connector.enabled
  );
  const learnAgentCount =
    Number(claudeLearnEnabled) +
    Number(codexLearnEnabled) +
    Number(opencodeLearnEnabled) +
    Number(grokLearnEnabled);
  const learnBlurb =
    learnAgentCount > 1
      ? t("learn.blurb.multiple")
      : codexLearnEnabled
        ? t("learn.blurb.codex")
        : opencodeLearnEnabled || grokLearnEnabled
          ? t("learn.blurb.agent")
          : t("learn.blurb.claude");
  useEffect(() => {
    // connectors === [] means get_client_connectors hasn't returned yet (the
    // Rust side always lists every managed client). Don't treat that launch
    // window as "user disabled everything", or the persisted verification
    // marker would be wiped on every start.
    if (connectors.length === 0) return;
    if (!anyConnectorEnabled) {
      // A deliberate all-off is the one case that invalidates the persisted
      // marker: re-enabling must re-verify with fresh traffic.
      setConnectorTrafficVerified(false);
      setConnectorPhase("disabled");
      return;
    }
    // Transition from "disabled" → enabled drops into verifying (unless a
    // past session already verified traffic), so the polling effect below
    // confirms via /stats before the badge flips green.
    setConnectorPhase((prev) =>
      prev === "disabled"
        ? isConnectorTrafficVerified()
          ? "healthy"
          : "verifying"
        : prev
    );
  }, [anyConnectorEnabled, connectors.length]);

  useEffect(() => {
    // Pricing status hits the remote Headroom API. When the tray is focused,
    // poll at 60s so fresh subscription/trial state is visible on demand.
    // When hidden, slow to 10 min — still fast enough for trial-expiry and
    // urgent notifications to fire, while cutting hourly API traffic by
    // ~90%. The launcher window never sets `trayWindowFocused` to false
    // (its focus listener isn't wired up), so it keeps the 60s cadence.
    const intervalMs = trayWindowFocused ? 60_000 : 600_000;
    void refreshPricingStatus();
    const interval = window.setInterval(() => {
      void refreshPricingStatus();
    }, intervalMs);
    return () => {
      window.clearInterval(interval);
    };
  }, [trayWindowFocused]);

  // headroom:// deep links from the backend trigger an immediate pricing
  // refresh — the typical case is Polar's checkout success page redirecting
  // to headroom://upgraded. Backend has already reconciled the runtime; this
  // just pulls the new status into UI state without waiting for the next
  // poll tick.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<HeadroomPricingStatus | null>("pricing-refreshed", (event) => {
      // Auth mutations ship the new status with the event. Using it beats a
      // refetch that the in-flight guard may drop outright, leaving this window
      // stale until the next poll tick (60s focused, 600s not).
      if (event.payload) {
        pricingStatusStampRef.current = Date.now();
        setPricingStatus(event.payload);
        return;
      }
      void refreshPricingStatus();
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  // After the user opens a Polar checkout URL, poll pricing status every 5s
  // for up to 5 minutes so we can flip the UI back to "active" within seconds
  // of payment confirmation, instead of waiting out the 60s baseline cadence.
  // Auto-stops once subscription_active is observed or the window expires.
  useEffect(() => {
    if (checkoutPollingDeadline === null) return;
    if (Date.now() > checkoutPollingDeadline) {
      setCheckoutPollingDeadline(null);
      return;
    }
    const interval = window.setInterval(() => {
      if (Date.now() > checkoutPollingDeadline) {
        setCheckoutPollingDeadline(null);
        return;
      }
      void refreshPricingStatus();
    }, 5_000);
    return () => {
      window.clearInterval(interval);
    };
  }, [checkoutPollingDeadline]);

  // Stop the aggressive checkout poll the moment we observe a live
  // subscription. Saves traffic and stops competing with the 60s cadence.
  useEffect(() => {
    if (checkoutPollingDeadline !== null && pricingStatus?.account?.subscriptionActive) {
      setCheckoutPollingDeadline(null);
    }
  }, [checkoutPollingDeadline, pricingStatus?.account?.subscriptionActive]);

  // When the pricing gate closes, pause optimization on enabled connectors
  // one at a time. Each disable refreshes `connectors`, re-running this
  // effect until none remain. Codex is exempt while authenticated:
  // `optimizationAllowed` reflects the *Claude* paid-plan gate, and Codex has
  // its own independent gate enforced proxy-side (codex_bypass) — a Claude
  // weekly cap must not switch off a Codex-heavy user's optimization.
  useEffect(() => {
    if (!pricingStatus || pricingStatus.optimizationAllowed || connectorsBusy) {
      return;
    }
    const target = getEnabledSupportedConnectors(connectors).find(
      (connector) =>
        !pricingStatus.authenticated || !GATE_EXEMPT_CONNECTOR_IDS.has(connector.clientId)
    );
    if (!target) {
      return;
    }
    autoDisabledByGateRef.current.add(target.clientId);
    persistAutoDisabledByGate();
    void toggleConnector(target, false);
  }, [connectors, connectorsBusy, pricingStatus]);

  // Companion to the auto-disable effect above: when the pricing gate
  // releases (e.g., user just signed up post-grace, or weekly usage
  // rolled over), bring back every connector we auto-disabled without forcing
  // a manual re-enable click. Scoped to our own prior auto-disables so a
  // user's manual disable during an ungated period is preserved.
  useEffect(() => {
    if (!pricingStatus?.optimizationAllowed || autoDisabledByGateRef.current.size === 0) {
      return;
    }
    if (connectorsBusy) {
      return;
    }
    const target = aggregateClientConnectors(connectors).find(
      (connector) =>
        autoDisabledByGateRef.current.has(connector.clientId) && !connector.enabled
    );
    if (!target) {
      autoDisabledByGateRef.current.clear();
      persistAutoDisabledByGate();
      return;
    }
    void toggleConnector(target, true);
  }, [connectors, connectorsBusy, pricingStatus]);

  useEffect(() => {
    const runtimeHealthyNow =
      runtimeStatus?.running === true &&
      runtimeStatus?.proxyReachable === true &&
      connectorPhase === "healthy";
    if (!pricingStatus?.authenticated || !runtimeHealthyNow || desktopActivationSentRef.current) {
      return;
    }
    desktopActivationSentRef.current = true;
    void invoke<HeadroomPricingStatus>("activate_headroom_account")
      .then((status) => setPricingStatus(status))
      .catch(() => {
        desktopActivationSentRef.current = false;
      });
  }, [connectorPhase, pricingStatus?.authenticated, runtimeStatus?.proxyReachable, runtimeStatus?.running]);

  // While verifying, poll the proxy's /stats request counter and flip to
  // healthy when it ticks past the anchor we captured on the first reachable
  // poll. The previous implementation scanned the python proxy log for
  // /v1/messages lines, but Claude Code traffic actually flows through the
  // Rust front proxy on 6867 — the python log only sees background activity,
  // so the regex match could hang forever even while requests were being
  // optimized normally.
  useEffect(() => {
    if (connectorPhase !== "verifying") return;
    let active = true;
    let anchor: number | null = null;
    let attempts = 0;
    let timer: number | undefined;
    // Fast feedback for the first minute after setup, then back off:
    // 'verifying' can last days if the user doesn't code, and a menubar app
    // has no business polling the proxy at 1 Hz around the clock.
    const schedule = () => {
      timer = window.setTimeout(() => void tick(), attempts < 60 ? 1000 : 10000);
    };
    const tick = async () => {
      // The launcher's onboarding verify step (a separate webview) may have
      // already observed traffic — honor its persisted marker instead of
      // demanding a second request.
      if (isConnectorTrafficVerified()) {
        setConnectorPhase("healthy");
        return;
      }
      const count = await invoke<number | null>("get_headroom_request_count").catch(
        () => null
      );
      if (!active) return;
      attempts += 1;
      // null = proxy unreachable. Don't anchor on transient
      // unreachable readings — a later reachable reading would otherwise
      // jump from 0 → N and flip the badge healthy without observing
      // any new traffic.
      if (count !== null) {
        if (anchor === null) {
          anchor = count;
        } else if (count > anchor) {
          setConnectorTrafficVerified(true);
          setConnectorPhase("healthy");
          return;
        } else if (count < anchor) {
          // Backend restart reset the /stats counter; re-anchor or the flip
          // would wait for traffic to climb past the stale pre-restart total.
          anchor = count;
        }
      }
      schedule();
    };
    schedule();
    return () => {
      active = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [connectorPhase]);

  async function handleBootstrap() {
    bootstrapFailureSignatureRef.current = "";
    setBootstrapError(null);
    setBootstrapProgress({
      running: true,
      complete: false,
      failed: false,
      currentStep: "Preparing install",
      message: "Initializing installer workflow.",
      currentStepEtaSeconds: 3,
      overallPercent: 2
    });
    setBootstrapping(true);
    try {
      await invoke("start_bootstrap");
    } catch (error) {
      const failureReport = buildBootstrapInvokeFailureReport(error);
      const failureSignature = bootstrapFailureSignature(failureReport);
      if (bootstrapFailureSignatureRef.current !== failureSignature) {
        bootstrapFailureSignatureRef.current = failureSignature;
        reportBootstrapFailure(failureReport, error);
      }
      setBootstrapError(failureReport.message);
      setBootstrapProgress({
        running: false,
        complete: false,
        failed: true,
        currentStep: failureReport.currentStep,
        message: failureReport.message,
        currentStepEtaSeconds: failureReport.currentStepEtaSeconds,
        overallPercent: failureReport.overallPercent
      });
      setBootstrapping(false);
    } finally {
      // Most completion paths are still managed by progress polling.
    }
  }

  function stepPercentSpan(step: string) {
    switch (step) {
      case "Preparing install":
        return 13;
      case "Downloading Python":
        return 13;
      case "Creating environment":
        return 17;
      case "Installing Headroom":
        return 20;
      case "Finalizing":
        return 4;
      default:
        return 8;
    }
  }

  function getStepProgress(progress: BootstrapProgress) {
    if (progress.complete) {
      return 1;
    }
    if (!progress.running || !stepStartedAtMs) {
      return 0;
    }

    const elapsedSeconds = Math.max(0, (Date.now() - stepStartedAtMs) / 1000);
    const eta = Math.max(8, stepEtaSeedSeconds || progress.currentStepEtaSeconds || 20);
    const linear = Math.min(0.96, elapsedSeconds / eta);

    if (elapsedSeconds <= eta) {
      return linear;
    }

    const overtime = elapsedSeconds - eta;
    const creep = Math.min(0.995, linear + overtime / (eta * 10));
    return creep;
  }

  function animatedOverallPercent(progress: BootstrapProgress) {
    if (progress.complete || progress.failed || !progress.running) {
      return progress.overallPercent;
    }

    const span = stepPercentSpan(progress.currentStep);
    const animated = stepBasePercent + span * getStepProgress(progress);
    return Math.min(99, Math.max(progress.overallPercent, animated));
  }

  function etaCopy(seconds: number, progress: BootstrapProgress) {
    if (!showInstallProgress) {
      return t("launcher.etaAfterInstall");
    }
    if (progress.complete) {
      return t("launcher.etaComplete");
    }
    if (progress.failed) {
      return t("launcher.etaUnavailable");
    }

    const elapsedSeconds = stepStartedAtMs
      ? Math.max(0, Math.round((Date.now() - stepStartedAtMs) / 1000))
      : 0;
    const baselineEta = Math.max(stepEtaSeedSeconds, seconds);
    const remainingSeconds = Math.max(0, baselineEta - elapsedSeconds);

    if (remainingSeconds <= 0 && progress.running) {
      return t("launcher.etaFinishing");
    }
    if (remainingSeconds <= 0) {
      return t("launcher.etaUnknown");
    }
    if (remainingSeconds < 60) {
      return t("launcher.etaSeconds", { seconds: remainingSeconds });
    }
    const mins = Math.floor(remainingSeconds / 60);
    const secs = remainingSeconds % 60;
    return t("launcher.etaMinutesSeconds", { minutes: mins, seconds: secs });
  }

  function getConnectorUnavailableReason(connector: ClientConnectorStatus) {
    if (canConfigureConnectorWithoutDetection(connector)) {
      return null;
    }
    if (connector.clientId === "claude_code") {
      return t("connections.claudeNotDetected");
    }
    return (
      connectorUnavailableReasons[connector.clientId] ??
      t("connections.genericUnavailable")
    );
  }

  function canConfigureConnectorWithoutDetection(connector: ClientConnectorStatus) {
    // Codex configuration is written to ~/.codex/config.toml, which both the CLI
    // and the GUI app read, so the toggle should be usable even when the CLI
    // binary isn't on the app's PATH (same rationale as claude_code).
    // opencode's default install (~/.opencode/bin) is likewise invisible to
    // the GUI app's PATH while its config is a dotfile we can always write.
    return (
      connector.installed ||
      connector.clientId === "claude_code" ||
      connector.clientId === "codex" ||
      connector.clientId === "grok_build" ||
      connector.clientId === "opencode"
    );
  }

  function getConnectorSupportWarning(connector: ClientConnectorStatus) {
    return connector.clientId === "claude_code"
      ? t("connections.claudeDesktopLimitation")
      : connectorSupportWarnings[connector.clientId] ?? null;
  }

  // Pricing gate: enabling a connector while optimization is disallowed just
  // triggers the auto-disable effect (the ON->OFF flash). Mirror that effect's
  // exemption exactly -- Codex is exempt while authenticated (its own
  // proxy-side gate). Only blocks *enabling*; an already-on connector is left
  // to the auto-disable effect.
  function connectorGateBlocksEnable(connector: ClientConnectorStatus) {
    return (
      pricingStatus != null &&
      !pricingStatus.optimizationAllowed &&
      !connector.enabled &&
      (!pricingStatus.authenticated || !GATE_EXEMPT_CONNECTOR_IDS.has(connector.clientId))
    );
  }

  function connectorGateCta() {
    if (pricingStatus?.authenticated) {
      setActiveView("upgrade");
    } else {
      openUpgradeAuthView();
    }
  }

  function getConnectorDetectionWarning(connector: ClientConnectorStatus) {
    if (!shouldShowConnectorDetectionWarning(connector)) {
      return null;
    }
    if (connector.clientId === "claude_code") {
      return t("connections.claudeNotDetected");
    }
    return connectorUnavailableReasons[connector.clientId] ?? null;
  }

  function applyAppUpdatePatch(patch: AppUpdateStatePatch) {
    if (Object.prototype.hasOwnProperty.call(patch, "config")) {
      setAppUpdateConfig(patch.config ?? null);
    }
    if (Object.prototype.hasOwnProperty.call(patch, "availableUpdate")) {
      setAppUpdateAvailable(patch.availableUpdate ?? null);
    }
    if (Object.prototype.hasOwnProperty.call(patch, "readyToRestart")) {
      setAppUpdateReadyToRestart(patch.readyToRestart ?? false);
    }
    if (Object.prototype.hasOwnProperty.call(patch, "showDialog")) {
      setShowAppUpdateDialog(patch.showDialog ?? false);
    }
    if (Object.prototype.hasOwnProperty.call(patch, "statusCopy")) {
      setAppUpdateStatusCopy(localizeAppUpdateCopy(t, patch.statusCopy ?? null));
    }
  }

  async function refreshAppUpdateConfiguration() {
    applyAppUpdatePatch(await loadAppUpdateConfiguration());
  }

  async function checkForAppUpdate({
    background = false,
    knownUpdateVersion = null,
  }: {
    background?: boolean;
    knownUpdateVersion?: string | null;
  } = {}) {
    let config = appUpdateConfig;

    if (!config) {
      const configPatch = await loadAppUpdateConfiguration();
      applyAppUpdatePatch(configPatch);
      config = configPatch.config ?? null;
    }

    if (!config) {
      return;
    }

    const blockedPatch = getBlockedAppUpdateCheckPatch(config, background);
    if (blockedPatch) {
      applyAppUpdatePatch(blockedPatch);
      return;
    }

    setAppUpdateBusy(true);
    if (!background) {
      setAppUpdateStatusCopy(t("update.checking"));
    }

    try {
      const patch = await runAppUpdateCheck({ background, knownUpdateVersion });
      applyAppUpdatePatch(patch);

      if (background && patch.availableUpdate) {
        const windowVisible = await getCurrentWindow().isVisible().catch(() => false);
        const notifyFresh = shouldNotifyAboutAvailableAppUpdate({
          background,
          availableUpdate: patch.availableUpdate,
          knownUpdateVersion,
          windowVisible,
        });
        if (notifyFresh) {
          await sendAppUpdateNotification(patch.availableUpdate.version);
        }
        // Never stack the stale reminder onto the tick that just announced
        // the release — first discovery of an old version used to fire both
        // notifications at once.
        if (!windowVisible && !notifyFresh) {
          await maybeFireStaleAppUpdateNotification(patch.availableUpdate);
        }
      }
    } finally {
      setAppUpdateBusy(false);
    }
  }

  async function installAvailableUpdate() {
    if (!appUpdateAvailable) {
      return;
    }

    setAppUpdateInstallBusy(true);
    const installStatusCopy = getAppUpdateInstallStatusCopy(appUpdateAvailable);
    if (installStatusCopy) {
      setAppUpdateStatusCopy(localizeAppUpdateCopy(t, installStatusCopy));
    }

    try {
      const versionForCopy = appUpdateAvailable.version;
      applyAppUpdatePatch(
        await runAppUpdateInstall({
          availableUpdate: appUpdateAvailable,
          onProgress: (progress) => {
            setAppUpdateStatusCopy(localizeAppUpdateCopy(t, formatAppUpdateProgressCopy(versionForCopy, progress)));
          },
        })
      );
    } finally {
      setAppUpdateInstallBusy(false);
    }
  }

  /// One progressive button on the failed-install screen: check -> install ->
  /// restart. That screen has no other route to a newer build, and when the
  /// cause is a bad pin in the lock we shipped (RUST-1G: onnxruntime on Intel
  /// macOS) a newer build is the *only* fix -- Try again re-resolves the same
  /// impossible pin forever.
  async function handleFailedInstallUpdateAction() {
    if (appUpdateReadyToRestart) {
      restartIntoInstalledUpdate();
      return;
    }
    if (appUpdateAvailable) {
      await installAvailableUpdate();
      return;
    }
    // Foreground check, so a "Up to date."/error line always answers the click.
    await checkForAppUpdate();
  }

  async function handleFailedInstallSupportMail() {
    const report = await invoke<BootstrapFailureReport | null>(
      "get_bootstrap_failure_report"
    ).catch(() => null);
    await invoke("open_external_link", {
      url: buildInstallFailureMailto({
        kind: report?.kind ?? null,
        detail: report?.detail ?? null,
        appVersion: appSemver,
        platform: runtimeStatus?.platform ?? "unknown",
      }),
    });
  }

  function restartIntoInstalledUpdate() {
    // Never reset: restart_app tears down the backend before exiting, which
    // can take seconds on slow machines — the busy label is the only signal
    // the click registered until the window dies.
    setAppUpdateRestartBusy(true);
    void invoke("restart_app");
  }

  // Single fetch path so hidden connectors can never leak into decision
  // logic (launcher auto-configure, verification rows) via a raw invoke.
  async function fetchConnectors(): Promise<ClientConnectorStatus[]> {
    const items = await invoke<ClientConnectorStatus[]>("get_client_connectors");
    return withoutHiddenConnectors(items);
  }

  async function refreshConnectors() {
    try {
      setConnectorsError(null);
      const items = await fetchConnectors();
      // Set unconditionally, unlike the state below: an empty result is a real
      // answer ("nothing connected") that the diff-and-set helper swallows,
      // and the setup-stall watchdog has to tell it apart from "not loaded".
      connectorsRef.current = items;
      applyConnectorsIfChanged(items);
    } catch (error) {
      setConnectorsError(
        describeInvokeError(error, "Could not load connector status.")
      );
    }
  }

  async function refreshRuntimeStatus() {
    try {
      const runtime = await invoke<RuntimeStatus>("get_runtime_status");
      applyRuntimeStatusIfChanged(runtime);
      void maybeFireUrgentRuntimeNotification(runtime);
    } catch (error) {
      setConnectorsError(
        describeInvokeError(error, "Could not load runtime status.")
      );
    }
  }

  async function handleResumeRuntime() {
    if (resuming) {
      return;
    }
    setResuming(true);
    setResumeError(null);
    try {
      await invoke("force_restart_headroom");
      await refreshRuntimeStatus();
    } catch (error) {
      setResumeError(
        describeInvokeError(error, "Could not restart Headroom.")
      );
    } finally {
      setResuming(false);
    }
  }

  async function refreshPricingStatus() {
    if (pricingRefreshInFlightRef.current) {
      return;
    }
    pricingRefreshInFlightRef.current = true;
    setPricingBusy(true);
    const issuedAt = Date.now();
    try {
      const status = await invoke<HeadroomPricingStatus>("get_headroom_pricing_status");
      if (pricingStatusStampRef.current > issuedAt) {
        return;
      }
      setPricingStatus(status);
      // The code step is local UI state, so a sign-in that happened elsewhere
      // (the magic link, or the other window) leaves this one still asking for
      // a code nobody needs to enter. Every refresh path lands here.
      if (status.authenticated) {
        setAuthCode("");
        setAuthCodeRequestedFor(null);
      }
      void maybeFireTrialNotifications(status);
      void maybeFireUrgentPricingNotifications(status);
      setPricingError(null);
    } catch (error) {
      setPricingError(
        describeInvokeError(error, "Could not load pricing status.")
      );
    } finally {
      pricingRefreshInFlightRef.current = false;
      setPricingBusy(false);
    }
  }

  async function refreshClaudeProjects() {
    setClaudeProjectsBusy(true);
    try {
      setClaudeProjectsError(null);
      const projects = await invoke<ClaudeCodeProject[]>("get_claude_code_projects");
      applyClaudeProjectsIfChanged(projects);
    } catch (error) {
      setClaudeProjectsError(
        describeInvokeError(error, "Could not load Claude Code projects.")
      );
    } finally {
      setClaudeProjectsBusy(false);
    }
  }

  async function refreshHeadroomLearnPrereq(force = false) {
    try {
      const status = await invoke<HeadroomLearnPrereqStatus>("get_headroom_learn_prereq_status", {
        force,
      });
      setHeadroomLearnPrereq(status);
    } catch {
      setHeadroomLearnPrereq(idleHeadroomLearnPrereqStatus);
    }
  }

  async function copyLearnInstallCommand(command: string) {
    try {
      if (!navigator.clipboard) {
        throw new Error("Clipboard API unavailable");
      }
      await navigator.clipboard.writeText(command);
      setLearnInstallCopyNotice("Copied install command.");
      window.setTimeout(() => setLearnInstallCopyNotice(null), 2000);
    } catch {
      setLearnInstallCopyNotice("Copy failed. Select the command and copy manually.");
      window.setTimeout(() => setLearnInstallCopyNotice(null), 3000);
    }
  }

  async function autoConfigureConnectorsForLauncher() {
    setConnectorsBusy(true);
    setConnectorsError(null);

    try {
      let latestConnectors = await fetchConnectors();
      applyConnectorsIfChanged(latestConnectors);

      const step = nextAutoConfigureStep(
        getLauncherAutoConfigureDecision(latestConnectors),
        latestConnectors
      );

      if (step.kind === "show_client_setup") {
        setLauncherStage("client_setup");
        return;
      }

      if (step.kind === "apply") {
        for (const clientId of step.clientIds) {
          const result = await invoke<ClientSetupResult>("apply_client_setup", { clientId });
          if (result.replacedBaseUrl) {
            setConnectorsNotice(
              t("connections.baseUrlTakeover", { address: result.replacedBaseUrl })
            );
          }
        }
        latestConnectors = await fetchConnectors();
        applyConnectorsIfChanged(latestConnectors);
        reportFunnelStep("client_setup_applied");

        const postApplyStep = nextAutoConfigureStepAfterApply(
          getLauncherAutoConfigureDecision(latestConnectors)
        );
        if (postApplyStep.kind !== "begin_proxy_verification") {
          setLauncherStage("client_setup");
          return;
        }
      }

      await beginProxyVerificationStep();
    } catch (error) {
      setConnectorsError(
        describeInvokeError(error, "Could not configure your coding tools automatically.")
      );
      setLauncherStage("client_setup");
    } finally {
      setConnectorsBusy(false);
    }
  }

  async function handleFirstLaunchContinue() {
    await autoConfigureConnectorsForLauncher();
  }

  async function handleInstallPrimaryAction() {
    // No checkout gate: the email-harvested trial user installs and uses
    // Headroom directly. Upgrading happens later from the in-app upgrade sheet.
    await handleBootstrap();
  }

  async function runHeadroomLearn(
    agent: "claude" | "codex" | "opencode" | "grok",
    projectPath?: string
  ) {
    if (runtimeStatus?.headroomLearnSupported === false) {
      setHeadroomLearnStatus((current) => ({
        ...current,
        running: false,
        summary: "Headroom Learn is unavailable on this platform.",
        error:
          runtimeStatus.headroomLearnDisabledReason ??
          "Headroom Learn is unavailable on this platform."
      }));
      return;
    }

    // Only Claude is project-organized; every other agent shares a stable
    // run key equal to its agent id.
    const runKey = agent === "claude" ? (projectPath ?? "") : agent;
    const displayName =
      agent === "codex"
        ? "Codex sessions"
        : agent === "opencode"
          ? "OpenCode sessions"
          : agent === "grok"
            ? "Grok sessions"
            : (claudeProjects.find((project) => project.projectPath === projectPath)?.displayName ??
              projectPath ??
              "");
    const startupSummary = `Running headroom learn for ${displayName}.`;
    trackAnalyticsEvent("headroom_learn_run", { agent });
    setHeadroomLearnBusy(true);
    setHeadroomLearnStatus((current) => ({
      ...current,
      running: true,
      projectPath: runKey,
      projectDisplayName: displayName,
      startedAt: new Date().toISOString(),
      finishedAt: null,
      progressPercent: Math.max(8, current.progressPercent || 0),
      summary: startupSummary,
      success: null,
      error: null
    }));
    try {
      await invoke("start_headroom_learn", { agent, projectPath: projectPath ?? null });
      for (const waitMs of [180, 350, 650, 900, 1200, 1800, 2400]) {
        await delay(waitMs);
        const status = await invoke<HeadroomLearnStatus>("get_headroom_learn_status", {
          projectPath: runKey
        });
        setHeadroomLearnStatus(status);
        if (!status.running) {
          break;
        }
      }
    } catch (error) {
      setHeadroomLearnStatus((current) => ({
        ...current,
        running: false,
        summary: "headroom learn could not be started.",
        error: describeInvokeError(error, "Failed to start headroom learn.")
      }));
    } finally {
      setHeadroomLearnBusy(false);
    }
  }

  async function handleRunHeadroomLearn(
    agent: "claude" | "codex" | "opencode" | "grok",
    projectPath?: string
  ) {
    if (agent === "claude" && projectPath) {
      setSelectedClaudeProjectPath(projectPath);
    }
    try {
      const status = await invoke<HeadroomLearnPrereqStatus>("get_headroom_learn_prereq_status");
      setHeadroomLearnPrereq(status);
      // opencode/grok sessions are read from disk; analysis runs through
      // whichever supported CLI exists (mirrors the Rust prereq check).
      const ready =
        agent === "codex"
          ? status.codexCliAvailable && status.codexLoggedIn
          : agent === "claude"
            ? status.claudeCliAvailable
            : status.claudeCliAvailable || (status.codexCliAvailable && status.codexLoggedIn);
      if (!ready) {
        return;
      }
    } catch {
      setHeadroomLearnPrereq(idleHeadroomLearnPrereqStatus);
      return;
    }
    await runHeadroomLearn(agent, projectPath);
  }

  async function openExternalLink(url: string) {
    await invoke("open_external_link", { url });
  }

  async function runAddonAction(
    command: "install_addon" | "set_addon_enabled" | "uninstall_addon",
    id: string,
    enabled?: boolean,
    // An update reuses install_addon, so only the wording and version differ.
    updateLabels?: { busy: string; done: string },
    version?: string | null
  ) {
    if (!addonPresetBusy) {
      addonPresetUserSelectedCustomRef.current = true;
      setAddonPresetMode("custom");
    }
    const toolName = dashboard.tools.find((tool) => tool.id === id)?.name ?? id;
    // Invalidate before invoking the mutating command so an older in-flight
    // check cannot write its pre-action version back into the UI.
    invalidateAddonUpdateChecks();
    const busyLabel =
      command === "install_addon"
        ? (updateLabels?.busy ?? t("addons.installing", { name: toolName }))
        : command === "uninstall_addon"
          ? t("addons.uninstalling", { name: toolName })
          : enabled
            ? t("addons.enabling", { name: toolName })
            : t("addons.disabling", { name: toolName });
    setAddonBusyById((current) => setAddonOperationMessage(current, id, busyLabel));
    setAddonErrorById((current) => clearAddonOperationMessage(current, id));
    setAddonResultById((current) => clearAddonOperationMessage(current, id));
    try {
      const next = await invoke<DashboardState>(command, {
        id,
        enabled,
        ...(command === "install_addon" && version ? { version } : {}),
      });
      // Re-read the dashboard after both success and failure paths.  The
      // command result is a useful fallback, but the explicit read also
      // covers post-install hooks that update state after the command returns.
      try {
        const refreshed = await invoke<DashboardState>("get_dashboard_state");
        if (refreshed && Array.isArray(refreshed.tools)) {
          setDashboard(refreshed);
        } else {
          setDashboard(next);
        }
      } catch (refreshError) {
        console.error("Failed to refresh dashboard after addon action", refreshError);
        setDashboard(next);
      }
      // Invalidate again in case a check was started while the operation was
      // running; the effect below will issue a check for the new dashboard.
      invalidateAddonUpdateChecks();
      if (id === "rtk") {
        await refreshRuntimeStatus();
      }
      const message =
        command === "install_addon"
          ? (updateLabels?.done ?? t("addons.installed", { name: toolName }))
          : command === "uninstall_addon"
            ? t("addons.uninstalled", { name: toolName })
            : enabled
              ? undefined
              : t("addons.disabled", { name: toolName });
      if (message) {
        setAddonResultById((current) => setAddonOperationMessage(current, id, message));
      }
    } catch (error) {
      // The command may have changed local state before reporting an error
      // (for example, a post-install hook can fail). Refresh the dashboard on
      // a best-effort basis, while preserving the original action error for
      // the user.
      try {
        const recovered = await invoke<DashboardState>("get_dashboard_state");
        if (recovered && Array.isArray(recovered.tools)) {
          setDashboard(recovered);
        }
      } catch (refreshError) {
        console.error("Failed to refresh dashboard after addon action error", refreshError);
      }
      invalidateAddonUpdateChecks();
      setAddonErrorById((current) =>
        setAddonOperationMessage(
          current,
          id,
          describeInvokeError(error, t("messages.localOperationFailed"))
        )
      );
    } finally {
      setAddonBusyById((current) => clearAddonOperationMessage(current, id));
    }
  }

  async function runAddonModeAction(id: string, mode: string) {
    if (!addonPresetBusy) {
      addonPresetUserSelectedCustomRef.current = true;
      setAddonPresetMode("custom");
    }
    const tool = dashboard.tools.find((candidate) => candidate.id === id);
    if (
      !tool ||
      !tool.enabled ||
      tool.status === "not_installed" ||
      !tool.supportedModes?.includes(mode) ||
      tool.defaultMode === mode
    ) {
      return;
    }

    setAddonBusyById((current) => setAddonOperationMessage(current, id, t("tools.savingMode")));
    setAddonResultById((current) => clearAddonOperationMessage(current, id));
    setAddonErrorById((current) => clearAddonOperationMessage(current, id));
    try {
      const next = await invoke<DashboardState>("set_addon_mode", { id, mode });
      setDashboard(next);
      setAddonResultById((current) =>
        setAddonOperationMessage(
          current,
          id,
          t("messages.toolModeSaved", {
            name: tool.name,
            mode: localizedToolModeLabel(mode, t),
          })
        )
      );
    } catch (error) {
      setAddonErrorById((current) =>
        setAddonOperationMessage(
          current,
          id,
          describeInvokeError(error, t("messages.localOperationFailed"))
        )
      );
    } finally {
      setAddonBusyById((current) => clearAddonOperationMessage(current, id));
    }
  }

  const [addonPresetBusy, setAddonPresetBusy] = useState(false);
  const [addonPresetMode, setAddonPresetMode] = useState<"recommended" | "custom">("recommended");
  const addonPresetUserSelectedCustomRef = useRef(false);

  useEffect(() => {
    if (addonPresetBusy || addonPresetUserSelectedCustomRef.current) return;
    setAddonPresetMode(recommendedAddonPresetMatches(dashboard) ? "recommended" : "custom");
  }, [dashboard, addonPresetBusy]);

  async function applyRecommendedAddonPreset() {
    if (addonPresetBusy) return;
    addonPresetUserSelectedCustomRef.current = false;
    setAddonPresetMode("recommended");
    setAddonPresetBusy(true);
    try {
      for (const [id, target] of Object.entries(RECOMMENDED_ADDON_PRESET)) {
        let tool = dashboard.tools.find((candidate) => candidate.id === id);
        if (!tool) continue;
        if (target.enabled && tool.status === "not_installed") {
          await runAddonAction("install_addon", id);
          const refreshed = await invoke<DashboardState>("get_dashboard_state");
          setDashboard(refreshed);
          tool = refreshed.tools.find((candidate) => candidate.id === id);
        }
        if (!tool) continue;
        if (tool.enabled !== target.enabled && tool.status !== "not_installed") {
          await runAddonAction("set_addon_enabled", id, target.enabled);
          const refreshed = await invoke<DashboardState>("get_dashboard_state");
          setDashboard(refreshed);
          tool = refreshed.tools.find((candidate) => candidate.id === id);
        }
        if (target.enabled && target.mode && tool?.supportedModes?.includes(target.mode) && tool?.defaultMode !== target.mode) {
          await runAddonModeAction(id, target.mode);
        }
      }
      const refreshed = await invoke<DashboardState>("get_dashboard_state");
      setDashboard(refreshed);
      if (!recommendedAddonPresetMatches(refreshed)) {
        throw new Error("Recommended add-on preset was not fully applied.");
      }
      setAddonPresetMode("recommended");
    } catch (error) {
      setAddonPresetMode("custom");
      console.error("Failed to apply recommended add-on preset", error);
    } finally {
      setAddonPresetBusy(false);
    }
  }

  function openUpgradeAuthView(planId: UpgradePlanId | null = null) {
    setActiveView("upgradeAuth");
    setPendingUpgradePlanId(planId);
    setAuthFlowError(null);
    setAuthFlowSuccess(null);
  }

  function resetUpgradeAuthStep() {
    setAuthCode("");
    setAuthCodeRequestedFor(null);
    setAuthFlowError(null);
    setAuthFlowSuccess(null);
  }

  async function handleRequestAuthCode() {
    if (!authEmailValid) {
      setAuthFlowError("Enter a valid email address.");
      return;
    }
    setAuthRequestBusy(true);
    setAuthFlowError(null);
    setAuthFlowSuccess(null);
    try {
      const result = await invoke<HeadroomAuthCodeRequest>("request_headroom_auth_code", {
        email: authEmail.trim()
      });
      reportFunnelStep("email_code_requested");
      setAuthCodeRequestedFor(result.email);
      setAuthCodeExpirySeconds(result.expiresInSeconds);
      setAuthFlowSuccess(`We sent a sign-in code to ${result.email}.`);
    } catch (error) {
      setAuthFlowError(describeInvokeError(error, "Could not send sign-in code."));
    } finally {
      setAuthRequestBusy(false);
    }
  }

  async function handleVerifyAuthCode() {
    if (!authEmailValid) {
      setAuthFlowError("Enter a valid email address.");
      return;
    }
    if (!authCode.trim()) {
      setAuthFlowError("Enter the authentication code from your email.");
      return;
    }
    await verifyAuthCode(authEmail.trim(), authCode.trim());
  }

  async function verifyAuthCode(email: string, code: string): Promise<boolean> {
    setAuthVerifyBusy(true);
    setAuthFlowError(null);
    setAuthFlowSuccess(null);
    try {
      const status = await invoke<HeadroomPricingStatus>("verify_headroom_auth_code", {
        email,
        code,
        inviteCode: null
      });
      pricingStatusStampRef.current = Date.now();
      setPricingStatus(status);
      setAuthCode("");
      setAuthCodeRequestedFor(null);
      reportFunnelStep("email_code_verified");
      setAuthFlowSuccess("Headroom account connected.");
      setPendingUpgradePlanId(null);
      // The launcher paywall renders its own sign-in inline; switching the
      // (hidden) main-window view would be meaningless there.
      if (windowLabel !== "launcher") {
        setActiveView("upgrade");
      }
      await refreshConnectors();
      return true;
    } catch (error) {
      setAuthFlowError(describeInvokeError(error, "Could not verify sign-in code."));
      return false;
    } finally {
      setAuthVerifyBusy(false);
    }
  }

  // Magic sign-in link (headroom://auth). The browser deliberately cannot sign
  // anyone in -- it has none of the device fingerprints verify_code needs -- so
  // it only hands over the code and this is the ordinary typed-code flow with
  // the typing removed.
  //
  // Drained on mount as well as on the event: a cold start launched *by* the
  // link delivers the URL before this listener exists, so an event alone would
  // be lost exactly when the link was the thing that opened the app. Rust hands
  // the slot out once, so both windows can race for it harmlessly.
  // Only the launcher claims it: both windows mount this component and the slot
  // is one-shot, so the hidden main window could win the race and sign the user
  // in where nobody could see it. The deep-link handler shows the launcher
  // before parking the credentials, so it is the window on screen.
  useEffect(() => {
    if (LOCAL_COMMUNITY_EDITION || windowLabel !== "launcher") {
      return;
    }
    let cancelled = false;
    async function claimMagicLink() {
      const pending = await invoke<[string, string] | null>("take_pending_magic_link");
      if (cancelled || !pending) {
        return;
      }
      const [email, code] = pending;
      setAuthEmail(email);
      setMagicLinkState("verifying");
      const verified = await verifyAuthCode(email, code);
      if (cancelled) {
        return;
      }
      // Success needs no screen: clearing this drops the user straight onto the
      // next onboarding step, already signed in.
      setMagicLinkState(verified ? null : "failed");
    }
    void claimMagicLink();
    const unlistenPromise = listen("magic-link-auth", () => {
      void claimMagicLink();
    });
    return () => {
      cancelled = true;
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, [windowLabel]);

  async function handleSignOutHeadroomAccount() {
    setAuthFlowError(null);
    setAuthFlowSuccess(null);
    try {
      await invoke("sign_out_headroom_account");
      const status = await invoke<HeadroomPricingStatus>("get_headroom_pricing_status");
      pricingStatusStampRef.current = Date.now();
      setPricingStatus(status);
      setAuthCode("");
      setAuthCodeRequestedFor(null);
      setAuthFlowSuccess("Signed out of Headroom.");
      setPendingUpgradePlanId(null);
    } catch (error) {
      setAuthFlowError(
        describeInvokeError(error, "Could not sign out of Headroom.")
      );
    }
  }

  async function openLearnInstallDocsLink() {
    try {
      await openExternalLink(CLAUDE_CODE_INSTALL_DOCS_URL);
    } catch (error) {
      setLearnInstallCopyNotice(
        describeInvokeError(error, "Could not open the install guide.")
      );
      window.setTimeout(() => setLearnInstallCopyNotice(null), 3000);
    }
  }

  async function handleUpgradeAction(planId: UpgradePlanId) {
    const activeHeadroomPlanId =
      pricingStatus?.account?.subscriptionActive
        ? pricingStatus.account.subscriptionTier ?? null
        : null;
    const action = (() => {
      switch (planId) {
        case "free":
          return {
            kind: activeHeadroomPlanId ? "billing_portal" as const : "internal" as const
          };
        case "pro":
        case "max5x":
        case "max20x": {
          // Same tier on the other billing period is a real change (Polar swaps
          // the product), not the plan you are already on.
          if (
            activeHeadroomPlanId === planId &&
            matchesSubscriptionPeriod(
              billingPeriod,
              pricingStatus?.account?.subscriptionBillingPeriod
            )
          ) {
            return { kind: "internal" as const };
          }
          // Polar prorates the product swap with the existing discount applied,
          // so every plan change on an active subscription uses the PATCH path.
          if (activeHeadroomPlanId) {
            return { kind: "change_plan" as const };
          }
          return { kind: "checkout" as const };
        }
        case "team":
          return {
            kind: "external" as const,
            url: SALES_CONTACT_URL,
            missing: "Set VITE_HEADROOM_SALES_CONTACT_URL to enable Team sales inquiries."
          };
        case "enterprise":
          return {
            kind: "external" as const,
            url: SALES_CONTACT_URL,
            missing: "Set VITE_HEADROOM_SALES_CONTACT_URL to enable Enterprise contact."
          };
        default:
          return null;
      }
    })();

    if (!action) {
      return;
    }

    trackAnalyticsEvent("upgrade_button_clicked", {
      plan_id: planId,
      action_kind: action.kind,
      email: pricingStatus?.account?.email ?? pricingStatus?.claude?.email ?? undefined,
    });

    if (action.kind === "internal") {
      setUpgradeActionError(null);
      setActiveView("home");
      return;
    }

    if (!pricingStatus?.authenticated) {
      openUpgradeAuthView(planId);
      return;
    }

    if (action.kind === "change_plan") {
      const fromTier = pricingStatus?.account?.subscriptionTier;
      if (!fromTier) return;
      setPlanChangeError(null);
      setPendingPlanChange({
        fromTier,
        toTier: planId as HeadroomSubscriptionTier,
        billingPeriod
      });
      return;
    }

    if (action.kind === "checkout") {
      setUpgradeActionBusy(planId);
      setUpgradeActionError(null);

      try {
        const url = await invoke<string>("create_headroom_checkout_session", {
          subscriptionTier: planId,
          billingPeriod
        });
        await openExternalLink(url);
        // Aggressive poll for the next 5 minutes so the moment Polar marks
        // the subscription active we surface "Headroom is back online" without
        // making the user wait out the normal 60s pricing-refresh cadence.
        setCheckoutPollingDeadline(Date.now() + 5 * 60_000);
      } catch (error) {
        setUpgradeActionError(
          describeInvokeError(error, "Could not start checkout.")
        );
      } finally {
        setUpgradeActionBusy(null);
      }
      return;
    }

    if (action.kind === "billing_portal") {
      setUpgradeActionBusy(planId);
      setUpgradeActionError(null);

      try {
        // Plain trip to the portal. Cancelling has its own entry point that asks
        // why first; someone updating a card should not meet a retention pitch.
        await openBillingPortal();
      } catch (error) {
        setUpgradeActionError(
          describeInvokeError(error, "Could not open billing portal.")
        );
      } finally {
        setUpgradeActionBusy(null);
      }
      return;
    }

    if (!action.url) {
      setUpgradeActionError(action.missing ?? "Could not open the selected plan link.");
      return;
    }

    setUpgradeActionBusy(planId);
    setUpgradeActionError(null);

    try {
      await openExternalLink(action.url);
    } catch (error) {
      setUpgradeActionError(
        describeInvokeError(error, "Could not open the selected plan link.")
      );
    } finally {
      setUpgradeActionBusy(null);
    }
  }

  async function confirmPlanChange() {
    if (!pendingPlanChange) return;
    setPlanChangeBusy(true);
    setPlanChangeError(null);
    try {
      await invoke("change_headroom_subscription_plan", {
        subscriptionTier: pendingPlanChange.toTier,
        billingPeriod: pendingPlanChange.billingPeriod
      });
      await refreshPricingStatus();
      setPendingPlanChange(null);
      setActiveView("home");
    } catch (error) {
      setPlanChangeError(
        error instanceof Error
          ? error.message
          : typeof error === "string"
            ? error
            : "Could not change subscription plan."
      );
    } finally {
      setPlanChangeBusy(false);
    }
  }

  function cancelPlanChange() {
    if (planChangeBusy) return;
    setPendingPlanChange(null);
    setPlanChangeError(null);
  }

  async function openBillingPortal() {
    // Deep-link to the user's subscription page so they land one click away
    // from "Change plan" instead of at the portal root.
    const url = await invoke<string>("get_headroom_billing_portal_url", {
      target: "subscription"
    });
    await openExternalLink(url);
  }

  function openCancelReason() {
    setCancelReason("");
    setCancelNote("");
    setUpgradeActionError(null);
    setCancelReasonOpen(true);
  }

  async function handleCancelContinue() {
    if (!cancelReason || cancelBusy) return;
    setCancelBusy(true);
    // Fails open: the reason is a nice-to-have, being trapped in the app is not.
    const offer = await invoke<SaveOffer | null>("submit_headroom_cancellation_intent", {
      reason: cancelReason,
      note: cancelNote.trim() || null
    }).catch(() => null);
    setCancelBusy(false);
    setCancelReasonOpen(false);

    if (offer) {
      setSaveOfferError(null);
      setSaveOfferRedeemed(false);
      setSaveOffer(offer);
      return;
    }

    setUpgradeActionBusy("cancel");
    try {
      await openBillingPortal();
    } catch (error) {
      setUpgradeActionError(
        describeInvokeError(error, "Could not open billing portal.")
      );
    } finally {
      setUpgradeActionBusy(null);
    }
  }

  async function handleRedeemSaveOffer() {
    if (saveOfferBusy) return;
    setSaveOfferBusy(true);
    setSaveOfferError(null);
    try {
      await invoke("redeem_headroom_save_offer");
      setSaveOfferRedeemed(true);
      await refreshPricingStatus();
    } catch (error) {
      setSaveOfferError(
        error instanceof Error
          ? error.message
          : typeof error === "string"
            ? error
            : "Could not apply the offer."
      );
    } finally {
      setSaveOfferBusy(false);
    }
  }

  async function handleDeclineSaveOffer() {
    if (saveOfferBusy) return;
    setSaveOffer(null);
    setUpgradeActionBusy("cancel");
    try {
      await openBillingPortal();
    } catch (error) {
      setUpgradeActionError(
        describeInvokeError(error, "Could not open billing portal.")
      );
    } finally {
      setUpgradeActionBusy(null);
    }
  }

  async function handleReactivateSubscription() {
    if (reactivateBusy) return;
    setReactivateBusy(true);
    setReactivateError(null);
    try {
      await invoke("reactivate_headroom_subscription");
      await refreshPricingStatus();
    } catch (error) {
      setReactivateError(
        error instanceof Error
          ? error.message
          : typeof error === "string"
            ? error
            : "Could not reactivate subscription."
      );
    } finally {
      setReactivateBusy(false);
    }
  }

  async function handleContactSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();

    const validationError = getContactRequestValidationError(CONTACT_FORM_URL, contactEmail);
    if (validationError) {
      setContactSubmitError(validationError);
      setContactSubmitSuccess(null);
      return;
    }

    const trimmed = contactEmail.trim();
    const trimmedMessage = contactMessage.trim().slice(0, 2000);
    setContactSubmitBusy(true);
    setContactSubmitError(null);
    setContactSubmitSuccess(null);

    try {
      await invoke("submit_contact_request", {
        url: CONTACT_FORM_URL,
        email: trimmed,
        message: trimmedMessage || null,
      });
      setContactEmail("");
      setContactMessage("");
      setContactSubmitSuccess("Thanks. Check your inbox for a confirmation email.");
    } catch (error) {
      setContactSubmitError(
        describeInvokeError(error, "Could not submit the contact request.")
      );
    } finally {
      setContactSubmitBusy(false);
    }
  }

  async function beginProxyVerificationStep() {
    let fresh = connectors;
    try {
      fresh = await fetchConnectors();
      applyConnectorsIfChanged(fresh);
    } catch {
      // fall back to cached state
    }

    setLauncherStage("proxy_verify");
    setProxyVerificationHint(null);
    setProxyVerifySkipArmed(false);
    setProxyVerificationRows(buildInitialProxyVerificationRows(fresh));
    // Reset to null so the polling effect re-anchors on its first reachable
    // /stats reading. Setting it here would risk anchoring on a stale value
    // from a prior visit to this stage.
    proxyVerificationRequestAnchorRef.current = null;
  }

  async function toggleConnector(connector: ClientConnectorStatus, nextEnabled: boolean) {
    if (connectorsBusy || codexRestartBusy) {
      return;
    }
    // A different tool may own the Codex route right now (its own root
    // `model_provider`). Taking it over replaces that tool's provider, so ask
    // first: Headroom records the displaced provider and restores it when the
    // connector is disabled again.
    const foreignProvider = nextEnabled
      ? connector.verification?.foreignProvider ?? null
      : null;
    if (
      foreignProvider &&
      !window.confirm(t("connections.setup.takeoverConfirm", { provider: foreignProvider }))
    ) {
      return;
    }
    const allowTakeover = Boolean(foreignProvider);
    setConnectorsBusy(true);
    setConnectorsError(null);
    try {
      if (nextEnabled) {
        const result = await invoke<ClientSetupResult>("apply_client_setup", {
          clientId: connector.clientId,
          allowTakeover,
        });
        setConnectorsNotice(localizedClientSetupNotice(t, connector.name, result));
      } else {
        await invoke("disable_client_setup", { clientId: connector.clientId });
        setConnectorsNotice(null);
      }

      const latestDashboard = await loadDashboard();
      applyDashboardIfChanged(latestDashboard);
      const latestConnectors = await fetchConnectors();
      applyConnectorsIfChanged(latestConnectors);
      // Applying a connector changes on-disk routing, but must not terminate
      // an active Codex desktop session behind the user's back. The setup
      // notice and the existing manual button tell the user when a reopen is
      // required; they can choose a safe time for it.
    } catch (error) {
      setConnectorsError(
        describeInvokeError(error, "Failed to update connector.")
      );
    } finally {
      setConnectorsBusy(false);
    }
  }

  async function restartCodexDesktop() {
    if (codexRestartBusy) {
      return;
    }
    setCodexRestartBusy(true);
    setConnectorsError(null);
    setConnectorsNotice(null);
    try {
      await invoke("restart_codex_desktop");
      setConnectorsNotice(t("connections.codexRestarted"));
      await refreshConnectors();
    } catch (error) {
      setConnectorsError(
        describeInvokeError(error, t("connections.codexRestartFailed"))
      );
    } finally {
      setCodexRestartBusy(false);
    }
  }


  function handleLauncherSurfaceMouseDown(event: MouseEvent<HTMLElement>) {
    if (event.button !== 0) {
      return;
    }

    const target = event.target as HTMLElement;
    if (
      target.closest(
        "button, input, textarea, select, a, [role='button'], [data-no-drag]"
      )
    ) {
      return;
    }

    void getCurrentWindow().startDragging();
  }

  const hidingRef = useRef(false);

  function triggerHide() {
    if (hidingRef.current) return;
    hidingRef.current = true;
    document.documentElement.classList.add("window-hiding");
    window.setTimeout(() => {
      invoke("hide_launcher_animated").catch((error) => {
        // Surface the failure and re-arm so a second click can retry instead
        // of silently dead-ending (the 400ms reset may have been throttled
        // while the window was hidden).
        console.error("hide_launcher_animated failed", error);
        document.documentElement.classList.remove("window-hiding");
        hidingRef.current = false;
      });
    }, launcherHideAnimationMs);
    setTimeout(() => {
      document.documentElement.classList.remove("window-hiding");
      hidingRef.current = false;
    }, 400);
  }

  const rawHeadroomTool = dashboard.tools.find((tool) => tool.id === "headroom");
  const headroomTool = rawHeadroomTool
    ? applyAddonUpdateChecks([rawHeadroomTool], addonUpdateChecks)[0]
    : undefined;
  const headroomVersion = headroomTool?.version ?? t("settings.unknown");
  const lifetimeTotalTokensSent = dashboard.dailySavings.reduce(
    (sum, point) => sum + point.totalTokensSent,
    0
  );
  const lifetimeTotalTokensBeforeOptimization =
    lifetimeTotalTokensSent + dashboard.lifetimeEstimatedTokensSaved;
  const headroomLifetimeSavingsPct =
    lifetimeTotalTokensBeforeOptimization > 0
      ? (dashboard.lifetimeEstimatedTokensSaved /
          lifetimeTotalTokensBeforeOptimization) *
        100
      : null;
  // Paired context for the savings headline. The headline rate dilutes as the
  // client's prompt caching improves, because cache reads sit in its
  // denominator while compression deliberately never touches the cached
  // prefix -- so a healthier cache reads as a Headroom regression. Show the
  // two forces side by side instead: how much of lifetime input the client's
  // cache served (cheap, never claimed by Headroom), and how much of the
  // REMAINING (compressible) input Headroom removed.
  // All three rows go through cacheHitPair, which prices both rates in
  // dollars (see its doc for why tokens are invalid here). The all-time row
  // feeds it the lifetime breakdown as a single synthetic bucket;
  // cacheReadTokens is used only as an existence signal for coverage, never
  // ratioed against our own token counts.
  const cachePairAllTime = allTimeCacheHitPair(
    dashboard.savingsBreakdown,
    dashboard.lifetimeEstimatedSavingsUsd
  );
  const compressionOfRestPct = cachePairAllTime?.compressedPct ?? null;
  // Same pair for the shorter windows, from the buckets that carry cache
  // coverage (backend history checkpoints; local-tracker buckets and days
  // aged out of retention are excluded from both rates). The all-time row
  // above uses the true lifetime breakdown instead, which predates coverage.
  const cachePairToday = cacheHitPair(
    buildHourlySavingsWindow(dashboard.hourlySavings, new Date())
  );
  const cachePairMonth = cacheHitPair(
    buildMonthlySavingsWindow(dashboard.dailySavings, new Date())
  );
  const rtkAvgSavingsPct =
    runtimeStatus?.rtk.installed && (runtimeStatus.rtk.totalCommands ?? 0) > 0
      ? runtimeStatus.rtk.avgSavingsPct ?? 0
      : null;
  const rtkSavingsChip =
    runtimeStatus?.rtk.installed && (runtimeStatus.rtk.totalSaved ?? 0) > 0
      ? t("addons.tokensSaved", { count: compactNumber(runtimeStatus.rtk.totalSaved ?? 0) })
      : null;
  const checkedTools = applyAddonUpdateChecks(dashboard.tools, addonUpdateChecks);
  const checkedRtkTool = checkedTools.find((tool) => tool.id === "rtk");
  const groupedAddonTools = groupToolsByCategory(
    checkedTools
      .filter((tool) => !tool.required && tool.id !== "rtk")
      .sort((a, b) => addonDisplayRank(a.id) - addonDisplayRank(b.id))
  );
  const addonUpdateFailureCount = addonUpdateChecks.filter((check) => Boolean(check.error)).length;
  const addonUpdatesFound = checkedTools.filter(
    (tool) =>
      !tool.required &&
      tool.status !== "not_installed" &&
      tool.updateActionAvailable === true
  ).length;
  const lifetimeDataDays = new Set(
    dashboard.dailySavings
      .map((point) => point.date)
      .filter((date) => Boolean(date))
  ).size;
  const lifetimeDataDaysLabel =
    lifetimeDataDays > 0
      ? t("onboarding.basedOnDays", { count: lifetimeDataDays })
      : t("onboarding.noHistory");

  useEffect(() => {
    window.dispatchEvent(
      new CustomEvent("headroom:boot-progress", {
        detail: {
          percent: startupPercent,
          status: startupCopy
        }
      })
    );
  }, [startupPercent, startupCopy]);

  useEffect(() => {
    const root = document.documentElement;
    const previousPlatform = root.dataset.platform;
    const detectedPlatform = normalizeDesktopPlatform(
      runtimeStatus?.platform,
      navigator.userAgent
    );

    root.dataset.platform = detectedPlatform;
    return () => {
      if (previousPlatform) {
        root.dataset.platform = previousPlatform;
      } else {
        delete root.dataset.platform;
      }
    };
  }, [runtimeStatus?.platform]);

  useEffect(() => {
    if (!startupReady || windowLabel === null) {
      return;
    }
    window.dispatchEvent(new CustomEvent("headroom:boot-complete"));
  }, [startupReady, windowLabel]);

  const desktopPlatform = normalizeDesktopPlatform(
    runtimeStatus?.platform,
    navigator.userAgent
  );

  if (!startupReady || windowLabel === null) {
    return null;
  }

  // Ahead of the terms gate: redemption is already under way by the time that
  // gate renders, and its sign-in form has no busy state of its own, so the
  // wait read as nothing happening at all.
  if (windowLabel === "launcher" && magicLinkState !== null) {
    const magicLinkCopy = magicLinkScreenCopy(
      magicLinkState,
      pricingStatus?.account?.email ?? authEmail,
      authFlowError
    );
    return (
      <LauncherShell
        shellClassName="intro-shell intro-shell--post-install"
        spinnerClassName="intro-shell__spinner intro-shell__spinner--post-install"
        copyClassName="intro-shell__copy intro-shell__copy--post-install"
        onMouseDown={handleLauncherSurfaceMouseDown}
        version={appSemver}
        showSpinner={magicLinkState === "verifying"}
      >
        <h1>{magicLinkCopy.title}</h1>
        <p className="launcher-install-notice">{magicLinkCopy.body}</p>
        {magicLinkState === "verifying" ? null : (
          <button
            className="primary-button primary-button--large primary-button--success"
            onClick={() => setMagicLinkState(null)}
            type="button"
          >
            Continue
          </button>
        )}
      </LauncherShell>
    );
  }

  // Block every window (launcher and main) until the user accepts the current
  // Terms of Service. New installs hit this in the launcher; updating users —
  // who may never see the launcher — hit it in the main window. Bumping the
  // backend's REQUIRED_TERMS_VERSION re-triggers it on the next launch.
  if (
    !LOCAL_COMMUNITY_EDITION &&
    needsTermsAcceptance(
      dashboard.requiredTermsVersion,
      dashboard.acceptedTermsVersion
    )
  ) {
    return (
      <TermsGate
        requiredVersion={dashboard.requiredTermsVersion}
        termsUrl={dashboard.termsUrl}
        onAccepted={() => {
          setDashboard((prev) => ({
            ...prev,
            acceptedTermsVersion: prev.requiredTermsVersion
          }));
          // Paywall-first users stay on the landing screen; the primary CTA
          // routes them through connector setup before checkout.
        }}
        authSection={
          paywallFirstFlow && windowLabel === "launcher" ? (
            pricingStatus?.authenticated === true ? (
              <p className="paywall__account-row">
                Signed in as {pricingStatus?.account?.email ?? authEmail}
                {" • "}
                <button
                  className="link-button"
                  onClick={() => void handleSignOutHeadroomAccount()}
                  type="button"
                >
                  or use a different email
                </button>
              </p>
            ) : (
              <AuthCodeForm
                email={authEmail}
                onEmailChange={setAuthEmail}
                emailValid={authEmailValid}
                code={authCode}
                onCodeChange={setAuthCode}
                codeRequested={authCodeRequestedFor !== null}
                requestBusy={authRequestBusy}
                verifyBusy={authVerifyBusy}
                error={authFlowError}
                success={authFlowSuccess}
                onRequestCode={() => void handleRequestAuthCode()}
                onVerify={() => void handleVerifyAuthCode()}
              />
            )
          ) : undefined
        }
        authComplete={pricingStatus?.authenticated === true}
      />
    );
  }

  const upgradeFailure = runtimeStatus?.runtimeUpgradeFailure ?? null;
  const showUpgradeModal =
    runtimeUpgradeProgress.running &&
    !runtimeUpgradeProgress.complete &&
    !runtimeUpgradeProgress.failed;
  const showUpgradeSuccess =
    !runtimeUpgradeProgress.running &&
    runtimeUpgradeProgress.complete &&
    !runtimeUpgradeProgress.failed;
  const showUpgradeBanner =
    !runtimeUpgradeProgress.running && upgradeFailure !== null;
  const upgradeExhausted =
    upgradeFailure !== null && upgradeFailure.attempts >= MAX_UPGRADE_AUTO_RETRIES;
  const canDismissUpgradeFailure =
    upgradeFailure !== null &&
    upgradeFailure.rollbackRestored &&
    runtimeStatus?.proxyReachable === true;

  const upgradeOverlay = (
    <>
      {showUpgradeModal && (
        <div
          className="modal-backdrop runtime-upgrade-backdrop"
          role="dialog"
          aria-modal="true"
        >
          <div className="modal-card runtime-upgrade-modal">
            <h3>
              {runtimeUpgradeProgress.toVersion
                ? `Finishing Headroom update to ${runtimeUpgradeProgress.toVersion}…`
                : "Finishing Headroom update…"}
            </h3>
            <p className="runtime-upgrade-modal__sub">
              {runtimeUpgradeProgress.fromVersion
                ? `From ${runtimeUpgradeProgress.fromVersion}`
                : ""}
            </p>
            <div className="install-progress__bar-track">
              <div
                className="install-progress__bar-fill"
                style={{ width: `${runtimeUpgradeProgress.overallPercent}%` }}
              />
            </div>
            <p className="runtime-upgrade-modal__step">
              {runtimeUpgradeProgress.currentStep}
            </p>
            <p className="runtime-upgrade-modal__message">
              {runtimeUpgradeProgress.message}
            </p>
          </div>
        </div>
      )}
      {showUpgradeBanner && upgradeFailure && (
        <div
          className={`runtime-upgrade-banner runtime-upgrade-banner--${upgradeFailure.failurePhase}`}
          role="alert"
        >
          <div className="runtime-upgrade-banner__body">
            <strong>
              {upgradeFailure.failurePhase === "boot_validation"
                ? `headroom-ai ${upgradeFailure.targetHeadroomVersion} installed but didn't start.`
                : "Headroom update didn't finish."}
            </strong>
            <span>
              {upgradeFailure.errorHint ??
                (upgradeFailure.failurePhase === "boot_validation" &&
                upgradeFailure.fallbackHeadroomVersion
                  ? `Reverted to headroom-ai ${upgradeFailure.fallbackHeadroomVersion}.`
                  : "Running the previous headroom-ai version.")}
            </span>
            {upgradeExhausted && (
              <span className="runtime-upgrade-banner__note">
                We won't auto-retry on launch. Click Retry to try again.
              </span>
            )}
          </div>
          <div className="runtime-upgrade-banner__actions">
            <button
              type="button"
              className="primary-button primary-button--small"
              onClick={() => void invoke("retry_runtime_upgrade")}
              disabled={runtimeUpgradeProgress.running}
            >
              Retry now
            </button>
            {upgradeFailure.failurePhase === "boot_validation" && (
              <button
                type="button"
                className="secondary-button secondary-button--small"
                onClick={() =>
                  void invoke("retry_runtime_upgrade_with_rebuild")
                }
                disabled={runtimeUpgradeProgress.running}
              >
                Retry with full rebuild
              </button>
            )}
            {upgradeFailure.failurePhase === "boot_validation" && (
              <button
                type="button"
                className="secondary-button secondary-button--small"
                onClick={() =>
                  void invoke("open_external_link", {
                    url: buildUpgradeIssueMailto(upgradeFailure),
                  }).catch(() => {})
                }
              >
                Report issue
              </button>
            )}
            {canDismissUpgradeFailure && (
              <button
                type="button"
                className="secondary-button secondary-button--small"
                onClick={() =>
                  void invoke("dismiss_runtime_upgrade_failure").catch(() => {})
                }
              >
                Dismiss
              </button>
            )}
          </div>
        </div>
      )}
    </>
  );

  // While a runtime upgrade is in flight, the venv is in the middle of being
  // swapped so `bootstrapComplete` may return false. Don't render the first-
  // run install wizard in that case — render a dedicated update screen in the
  // launcher instead.
  if (
    windowLabel === "launcher" &&
    (showUpgradeModal || showUpgradeSuccess || (showUpgradeBanner && upgradeFailure))
  ) {
    return (
      <LauncherShell
        shellClassName="intro-shell intro-shell--post-install"
        spinnerClassName="intro-shell__spinner intro-shell__spinner--post-install"
        copyClassName="intro-shell__copy intro-shell__copy--post-install"
        onMouseDown={handleLauncherSurfaceMouseDown}
        version={appSemver}
        showSpinner={showUpgradeModal}
      >
        {showUpgradeSuccess ? (
          <>
            <h1>
              {`Headroom ${runtimeUpgradeProgress.toVersion ?? ""} is ready`}
            </h1>
            <p className="launcher-install-notice">
              {runtimeUpgradeProgress.message}
            </p>
            <div className="install-progress-shell">
              <div className="install-progress" aria-live="polite">
                <div className="install-progress__bar-track">
                  <div
                    className="install-progress__bar-fill"
                    style={{ width: "100%" }}
                  />
                </div>
              </div>
            </div>
          </>
        ) : showUpgradeModal ? (
          <>
            <h1>
              {runtimeUpgradeProgress.toVersion
                ? `Finishing Headroom ${runtimeUpgradeProgress.toVersion} update…`
                : "Finishing Headroom update…"}
            </h1>
            <p className="launcher-install-notice">
              {runtimeUpgradeProgress.message ||
                "Wrapping up the Headroom update."}
            </p>
            <div className="install-progress-shell">
              <div className="install-progress" aria-live="polite">
                <div className="install-progress__bar-track">
                  <div
                    className="install-progress__bar-fill"
                    style={{ width: `${runtimeUpgradeProgress.overallPercent}%` }}
                  />
                </div>
                <div className="install-progress__meta">
                  <p>{runtimeUpgradeProgress.currentStep}</p>
                </div>
              </div>
            </div>
          </>
        ) : upgradeFailure ? (
          <>
            <h1>
              {`Headroom ${upgradeFailure.appVersion} couldn't finish updating`}
            </h1>
            <p className="launcher-install-notice">
              {upgradeFailure.errorHint ??
                (upgradeFailure.fallbackHeadroomVersion
                  ? "Running the previous version while we wait for you to retry."
                  : "Running the previous version.")}
              {upgradeExhausted
                ? " We won't auto-retry on launch — click Retry to try again."
                : ""}
            </p>
            <div className="launcher-install-buttons">
              <button
                type="button"
                className="primary-button primary-button--large"
                onClick={() => void invoke("retry_runtime_upgrade")}
                disabled={runtimeUpgradeProgress.running}
              >
                Retry update
              </button>
              <button
                type="button"
                className="secondary-button"
                onClick={() => void handleFirstLaunchContinue()}
              >
                Continue with previous version
              </button>
              {upgradeFailure.failurePhase === "boot_validation" && (
                <button
                  type="button"
                  className="secondary-button"
                  onClick={() =>
                    void invoke("retry_runtime_upgrade_with_rebuild")
                  }
                  disabled={runtimeUpgradeProgress.running}
                >
                  Retry with full rebuild
                </button>
              )}
              {upgradeFailure.failurePhase === "boot_validation" && (
                <button
                  type="button"
                  className="secondary-button secondary-button--small"
                  onClick={() =>
                    void invoke("open_external_link", {
                      url: buildUpgradeIssueMailto(upgradeFailure),
                    }).catch(() => {})
                  }
                >
                  Report issue
                </button>
              )}
            </div>
          </>
        ) : null}
      </LauncherShell>
    );
  }

  if (
    windowLabel === "launcher" && launcherStage === "install"
  ) {
    const stepProgress = Math.round(getStepProgress(bootstrapProgress) * 100);
    const renderPercent = animatedOverallPercent(bootstrapProgress);
    const installComplete = bootstrapProgress.complete || dashboard.bootstrapComplete;
    const failedInstallUpdateLabel = appUpdateRestartBusy
      ? t("actions.restarting")
      : appUpdateInstallBusy
        ? t("launcher.installingHeadroom")
        : appUpdateBusy
          ? t("settings.checking")
          : appUpdateReadyToRestart
            ? t("update.restartNow")
            : appUpdateAvailable
              ? t("update.installVersion", { version: appUpdateAvailable.version })
              : t("settings.checkForUpdates");

    const statusCopy = !showInstallProgress
      ? ""
      : bootstrapProgress.failed
        ? // The message renders in full in the error paragraph below; repeating
          // it here printed the same three sentences twice.
          bootstrapProgress.currentStep
        : `${bootstrapProgress.message} ${
            bootstrapProgress.running && !bootstrapProgress.complete
              ? `(${t("launcher.stepProgress", { percent: stepProgress })})`
              : ""
          }`.trim();

    return (
      <LauncherShell
        shellClassName="intro-shell"
        spinnerClassName="intro-shell__spinner"
        copyClassName="intro-shell__copy intro-shell__copy--first-run"
        onMouseDown={handleLauncherSurfaceMouseDown}
        version={appSemver}
        showSpinner={bootstrapping}
      >
        <div>
        <h1>{t("launcher.headlineBefore")} <span className="headline-highlight">50%</span> {t("launcher.headlineAfter")}</h1>
        <div className="intro-shell__agents" aria-label={t("aria.supportedAgents")}>
          {[
            ["claude_code", "Claude Code"],
            ["codex", "Codex"],
            ["grok_build", "Grok Build"],
            ["opencode", "OpenCode"]
          ].map(([clientId, label]) => (
            <span className="intro-shell__agent" key={clientId}>
              <ConnectorIcon clientId={clientId} size={14} />
              {label}
            </span>
          ))}
        </div>
        </div>
        <div className="intro-shell__checklist">
          <article>
            <strong>{t("launcher.privacyTitle")}</strong>
            <p>{t("launcher.privacyDescription")}</p>
          </article>
          <article>
            <strong>{t("launcher.selfContainedTitle")}</strong>
            <p>{t("launcher.selfContainedDescription")}</p>
          </article>
          <article>
            <strong>{t("launcher.lessTokensTitle")}</strong>
            <p>{t("launcher.lessTokensDescription")}</p>
          </article>
        </div>
        {installComplete ? (
          <>
            {bootstrapProgress.running ||
            (runtimeStatus?.running !== true && runtimeStatus?.bypassed !== true) ? (
              <>
                <p className="launcher-install-notice">{t("launcher.startingFirstTime")}</p>
                <button
                  className="primary-button primary-button--large primary-button--install launcher-step1-continue"
                  disabled
                  type="button"
                >
                  {t("launcher.starting")}
                </button>
              </>
            ) : (
              <>
                <p className="launcher-install-notice">{t("launcher.installed")}</p>
                <button
                  className="primary-button primary-button--large primary-button--success launcher-step1-continue"
                  onClick={() => void handleFirstLaunchContinue()}
                  type="button"
                >
                  {t("actions.continue")}
                </button>
              </>
            )}
          </>
        ) : (
          <>
            {!bootstrapping && (
              <p className="install-pre-notice">
                {t("launcher.installNotice")}
              </p>
            )}
            <button
              className="primary-button primary-button--large primary-button--install"
              disabled={bootstrapping}
              onClick={() => void handleInstallPrimaryAction()}
              type="button"
            >
              {bootstrapping
                ? t("launcher.installingHeadroom")
                : bootstrapProgress.failed
                  ? t("launcher.tryAgain")
                  : t("launcher.installHeadroom")}
            </button>
          </>
        )}
        <div className="install-progress-shell">
          {showInstallProgress ? (
            <div className="install-progress" aria-live="polite">
              <div className="install-progress__bar-track">
                <div
                  className="install-progress__bar-fill"
                  style={{ width: `${renderPercent}%` }}
                />
              </div>
              <div className="install-progress__meta">
                <p>{statusCopy}</p>
                <span>
                  {etaCopy(
                    bootstrapProgress.currentStepEtaSeconds,
                    bootstrapProgress
                  )}
                </span>
              </div>
              {bootstrapError ? (
                <p className="install-progress__error">{bootstrapError}</p>
              ) : null}
              {bootstrapProgress.failed ? (
                <div className="install-progress__actions">
                  <button
                    className="secondary-button secondary-button--small"
                    disabled={
                      appUpdateBusy || appUpdateInstallBusy || appUpdateRestartBusy
                    }
                    onClick={() => void handleFailedInstallUpdateAction()}
                    type="button"
                  >
                    {failedInstallUpdateLabel}
                  </button>
                  <button
                    className="secondary-button secondary-button--small"
                    onClick={() => void handleFailedInstallSupportMail()}
                    type="button"
                  >
                    {t("launcher.contactSupport")}
                  </button>
                </div>
              ) : null}
              {appUpdateStatusCopy && bootstrapProgress.failed ? (
                <p className="install-progress__update-status">{localizeAppUpdateCopy(t, appUpdateStatusCopy)}</p>
              ) : null}
            </div>
          ) : null}
        </div>
      </LauncherShell>
    );
  }

  if (
    windowLabel === "launcher" && launcherStage === "client_setup"
  ) {
    const launcherConnectors =
      connectors.length > 0 ? connectors : launcherConnectorFallback;
    const sortedLauncherConnectors = sortClientConnectors(launcherConnectors);
    const availableConnectors = sortedLauncherConnectors.filter((connector) =>
      canConfigureConnectorWithoutDetection(connector)
    );
    const unavailableConnectors = sortedLauncherConnectors.filter(
      (connector) => !canConfigureConnectorWithoutDetection(connector)
    );
    const enabledConnectorCount = launcherConnectors.filter((connector) => connector.enabled).length;
    const requireSelection = availableConnectors.length > 0;

    return (
      <LauncherShell
        shellClassName="intro-shell intro-shell--post-install intro-shell--client-setup"
        spinnerClassName="intro-shell__spinner intro-shell__spinner--post-install"
        copyClassName="intro-shell__copy intro-shell__copy--post-install"
        onMouseDown={handleLauncherSurfaceMouseDown}
        version={appSemver}
      >
        <div className="post-install__lead">
          <h1>{t("launcher.connectTitle")}</h1>
          <p>{t("launcher.connectDescription")}</p>
          <div className="connector-list">
            {availableConnectors.map((connector) => {
              const unavailableReason = getConnectorUnavailableReason(connector);
              const detectionWarning = getConnectorDetectionWarning(connector);
              const supportWarning = getConnectorSupportWarning(connector);
              const statusLine = connectorStatusLine(connector);
              const gateBlocksEnable =
                !LOCAL_COMMUNITY_EDITION && connectorGateBlocksEnable(connector);
              return (
                <article className="connector-item" key={connector.clientId}>
                  <div>
                    <h3>
                      <span className="client-logo" aria-hidden="true">
                        {renderConnectorLogo(connector.clientId)}
                      </span>
                      {connector.name}
                      {supportWarning ? (
                        <button
                          className="connector-warning-help"
                          onClick={() =>
                            setOpenConnectorWarningId((current) =>
                              current === connector.clientId ? null : connector.clientId
                            )
                          }
                          type="button"
                          aria-label={t("aria.showWarning", { name: connector.name })}
                          aria-expanded={openConnectorWarningId === connector.clientId}
                        >
                          !
                        </button>
                      ) : null}
                      <button
                        className="connector-help"
                        onClick={() =>
                          setOpenConnectorHelpId((current) =>
                            current === connector.clientId ? null : connector.clientId
                          )
                        }
                        type="button"
                        aria-label={t("connections.showDetails", { name: connector.name })}
                        aria-expanded={openConnectorHelpId === connector.clientId}
                      >
                        i
                      </button>
                    </h3>
                    {openConnectorHelpId === connector.clientId ? (
                      <p className="connector-tooltip">
                        {CONNECTOR_SETUP_KEYS[connector.clientId]
                          ? t(CONNECTOR_SETUP_KEYS[connector.clientId])
                          : t("connections.localConfiguration")}
                      </p>
                    ) : null}
                    {openConnectorWarningId === connector.clientId && supportWarning ? (
                      <p className="connector-tooltip connector-tooltip--warning">
                        {supportWarning}
                      </p>
                    ) : null}
                    {statusLine ? (
                      <p className={`connector-item__${statusLine.tone}`}>{localizeUiText(t, statusLine.text)}</p>
                    ) : null}
                    {(detectionWarning ?? unavailableReason) ? (
                      <p className="connector-item__reason">
                        {localizeUiText(t, detectionWarning ?? unavailableReason ?? "")}
                      </p>
                    ) : null}
                    {gateBlocksEnable ? (
                      <p className="connector-item__reason">
                        {pricingStatus?.gateMessage}{" "}
                        <button
                          className="addon-card__link"
                          type="button"
                          onClick={connectorGateCta}
                        >
                          {t(pricingStatus?.authenticated ? "connections.upgrade" : "connections.signIn")}
                        </button>
                      </p>
                    ) : null}
                  </div>
                  <div className="connector-item__controls">
                    <button
                      aria-checked={connector.enabled}
                      aria-label={t(
                        connector.enabled ? "connections.disableConnector" : "connections.enableConnector",
                        { name: connector.name }
                      )}
                      className={`connector-switch${connector.enabled ? " is-on" : ""}`}
                      disabled={connectorsBusy || codexRestartBusy || gateBlocksEnable}
                      onClick={() =>
                        void toggleConnector(connector, !connector.enabled)
                      }
                      role="switch"
                      title={unavailableReason ? localizeUiText(t, unavailableReason) : undefined}
                      type="button"
                    >
                      <span className="connector-switch__thumb" />
                    </button>
                    {connector.clientId === "codex" &&
                    connector.restartRequired === true &&
                    statusLine?.tone === "restart" ? (
                      <button
                        className="secondary-button secondary-button--small"
                        disabled={connectorsBusy || codexRestartBusy}
                        onClick={() => void restartCodexDesktop()}
                        type="button"
                      >
                        {codexRestartBusy
                          ? t("connections.restartingCodex")
                          : t("connections.restartCodex")}
                      </button>
                    ) : null}
                  </div>
                </article>
              );
            })}
          </div>
          {unavailableConnectors.length > 0 ? (
            <div className="connector-list connector-list--unavailable">
              <p className="connector-list__section-label">
                {t(
                  desktopPlatform === "windows"
                    ? "connections.notInstalledDevice"
                    : "connections.notInstalled"
                )}
              </p>
              {unavailableConnectors.map((connector) => {
                const unavailableReason = getConnectorUnavailableReason(connector);
                const supportWarning = getConnectorSupportWarning(connector);
                return (
                  <article className="connector-item is-unavailable" key={connector.clientId}>
                    <div>
                      <h3>
                        <span className="client-logo" aria-hidden="true">
                          {renderConnectorLogo(connector.clientId)}
                        </span>
                        {connector.name}
                        {supportWarning ? (
                          <button
                            className="connector-warning-help"
                            onClick={() =>
                              setOpenConnectorWarningId((current) =>
                                current === connector.clientId ? null : connector.clientId
                              )
                            }
                            type="button"
                          aria-label={t("aria.showWarning", { name: connector.name })}
                            aria-expanded={openConnectorWarningId === connector.clientId}
                          >
                            !
                          </button>
                        ) : null}
                      </h3>
                      {openConnectorWarningId === connector.clientId && supportWarning ? (
                        <p className="connector-tooltip connector-tooltip--warning">
                          {supportWarning}
                        </p>
                      ) : null}
                      {unavailableReason ? (
                        <p className="connector-item__reason">{localizeUiText(t, unavailableReason)}</p>
                      ) : null}
                    </div>
                  </article>
                );
              })}
            </div>
          ) : null}
          {connectorsError ? (
            <p className="install-progress__error">{connectorsError}</p>
          ) : null}
          {connectorsNotice ? (
            <p className="install-progress__notice">{connectorsNotice}</p>
          ) : null}
        </div>
        <div className="post-install__actions">
          <button
            className="secondary-button post-install__reopen-setup"
            onClick={() => {
              setLauncherStage("install");
            }}
            type="button"
          >
            {t("actions.back")}
          </button>
          <button
            className="primary-button primary-button--large primary-button--success"
            disabled={connectorsBusy || (requireSelection && enabledConnectorCount === 0)}
            onClick={() => {
              void beginProxyVerificationStep();
            }}
            type="button"
          >
            {t("actions.continue")}
          </button>
        </div>
      </LauncherShell>
    );
  }

  if (
    windowLabel === "launcher" && launcherStage === "proxy_verify"
  ) {
    const hasEnabledApps = proxyVerificationRows.length > 0;
    const allVerified =
      hasEnabledApps &&
      proxyVerificationRows.every((row) => row.state === "verified");
    const unverified = proxyVerificationRows.filter((row) => row.state !== "verified");
    const unverifiedNames = formatConnectorNameList(unverified.map((row) => row.name));
    const finishSetup = () => {
      void invoke("complete_setup_wizard");
      setLauncherStage("post_install");
    };

    return (
      <LauncherShell
        shellClassName="intro-shell intro-shell--post-install"
        spinnerClassName="intro-shell__spinner intro-shell__spinner--post-install"
        copyClassName="intro-shell__copy intro-shell__copy--post-install"
        onMouseDown={handleLauncherSurfaceMouseDown}
        version={appSemver}
      >
        <div className="post-install__lead">
          <h1>{t("launcher.testTitle")}</h1>
          <p>{t("launcher.testDescription")}</p>
          {hasEnabledApps ? (
            <div className="connector-list">
              {proxyVerificationRows.map((row) => (
                <article className="connector-item" key={row.clientId}>
                  <div>
                    <h3>
                      <span className="client-logo" aria-hidden="true">
                        {renderConnectorLogo(row.clientId)}
                      </span>
                      {row.name}
                    </h3>
                    <div className="proxy-verify-item__message">
                      <span>{row.message}</span>
                      {row.state === "verified" ? (
                        <span className="proxy-verified-pill">{t("launcher.verified")}</span>
                      ) : null}
                    </div>
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <p className="launcher-restart-hint">
              {t("launcher.noTools")}
            </p>
          )}
          {proxyVerificationHint ? (
            <p
              className={
                proxyVerificationHint.tone === "error"
                  ? "install-progress__error"
                  : "launcher-restart-hint"
              }
            >
              {proxyVerificationHint.text}
            </p>
          ) : null}
          {!allVerified && proxyVerifySkipArmed ? (
            <p className="install-progress__notice">
              {hasEnabledApps
                ? t("launcher.skipWarningWithAgents", { names: unverifiedNames })
                : t("launcher.skipWarningNoAgents")}
            </p>
          ) : null}
        </div>
        <div className="post-install__actions">
          <button
            className="secondary-button post-install__reopen-setup"
            onClick={() => {
              setLauncherStage("client_setup");
            }}
            type="button"
          >
            {t("actions.back")}
          </button>
          {allVerified ? (
            <button
              className="primary-button primary-button--large primary-button--success"
              onClick={finishSetup}
              type="button"
            >
              {t("actions.continue")}
            </button>
          ) : (
            // Deliberately not a primary button: leaving without a single
            // verified connector is the path that ends in "Headroom didn't
            // work", so it should not look like the happy path.
            <button
              className="secondary-button"
              onClick={() => {
                if (proxyVerifySkipArmed) {
                  finishSetup();
                  return;
                }
                setProxyVerifySkipArmed(true);
              }}
              type="button"
            >
              {t(proxyVerifySkipArmed ? "launcher.skipAnyway" : "launcher.skipForNow")}
            </button>
          )}
        </div>
      </LauncherShell>
    );
  }

  if (windowLabel === "launcher" && launcherStage === "paywall") {
    // Pre-install there is no proxied traffic, so detection normally yields
    // nothing and the recommendation defaults to Max x5 — this is a self-select
    // screen. Detected tiers still win if they happen to exist.
    const recommendedTier = recommendedHeadroomTier(
      pricingStatus?.claude?.planTier ?? null,
      pricingStatus?.codexPlanTier ?? null,
      "max5x"
    );
    // Fixed cheapest-first order, independent of the upgrade view's
    // recommended-first sorting; mirrors the website pricing page.
    const paywallPlans = (["pro", "max5x", "max20x"] as const)
      .map((id) => upgradePlansState.plans.find((plan) => plan.id === id))
      .filter((plan): plan is UpgradePlan => plan !== undefined);
    const paywallPlanFit: Record<string, string> = {
      pro: "For Claude Pro or ChatGPT Plus",
      max5x: "Claude Max x5 & ChatGPT Pro Lite",
      max20x: "Claude Max x20 & ChatGPT Pro"
    };
    const signedIn = pricingStatus?.authenticated === true;
    return (
      <LauncherShell
        shellClassName="intro-shell intro-shell--paywall"
        spinnerClassName="intro-shell__spinner intro-shell__spinner--post-install"
        copyClassName="intro-shell__copy intro-shell__copy--post-install"
        onMouseDown={handleLauncherSurfaceMouseDown}
        version={appSemver}
      >
        <div className="paywall">
          <h1>Pick your Headroom plan</h1>
          <div className="upgrade-billing-toggle" role="group" aria-label="Billing period">
            {(["monthly", "annual"] as const).map((period) => (
              <button
                key={period}
                className={`upgrade-billing-toggle__item${billingPeriod === period ? " is-active" : ""}`}
                onClick={() => setBillingPeriod(period)}
                type="button"
              >
                {period === "annual" ? (
                  <>Annual <span className="upgrade-billing-toggle__save">Save 25%</span></>
                ) : "Monthly"}
              </button>
            ))}
          </div>
          {pricingStatus?.introOffer?.active ? (
            <p className="paywall__sale-banner">
              🎉 Intro offer: {pricingStatus.introOffer.percentOff}% off your first{" "}
              {pricingStatus.introOffer.durationMonths} months • on every plan
            </p>
          ) : null}
          <p className="paywall__detection">
            Pick the tier that matches your Claude or ChatGPT plan • <strong>7-day free trial</strong>.
          </p>
          {!signedIn ? (
            <AuthCodeForm
              lead="Sign in to subscribe. We'll email you a one-time code."
              email={authEmail}
              onEmailChange={setAuthEmail}
              emailValid={authEmailValid}
              code={authCode}
              onCodeChange={setAuthCode}
              codeRequested={authCodeRequestedFor !== null}
              requestBusy={authRequestBusy}
              verifyBusy={authVerifyBusy}
              error={authFlowError}
              success={authFlowSuccess}
              onRequestCode={() => void handleRequestAuthCode()}
              onVerify={() => void handleVerifyAuthCode()}
            />
          ) : null}
          <div className="paywall__plans">
            {paywallPlans.map((plan) => {
              const isRecommended = plan.id === recommendedTier;
              return (
                <article
                  className={`soft-card paywall-card${isRecommended ? " paywall-card--recommended" : ""}`}
                  key={plan.id}
                >
                  <strong className="paywall-card__name">{plan.name}</strong>
                  <span className="paywall-card__fit">
                    {paywallPlanFit[plan.id] ?? plan.tagline}
                  </span>
                  {plan.originalPrice ? (
                    <span className="paywall-card__sale-row">
                      <s className="upgrade-plan-card__original-price">
                        {plan.originalPrice}
                      </s>
                      <span className="upgrade-plan-card__sale-badge">
                        {introSaleBadgeLabel(pricingStatus?.introOffer) ??
                          `${(pricingStatus?.activePercentOff ?? 0) || 50}% off`}
                      </span>
                    </span>
                  ) : null}
                  <span className="paywall-card__price">{plan.price}</span>
                  <span className="paywall-card__billing">
                    {plan.billingLines.join(" ")}
                  </span>
                  <button
                    className={isRecommended ? "primary-button" : "secondary-button"}
                    disabled={!signedIn || upgradeActionBusy !== null}
                    onClick={() => void handleUpgradeAction(plan.id)}
                    type="button"
                  >
                    {upgradeActionBusy === plan.id
                      ? "Opening checkout…"
                      : `Start ${plan.name} trial`}
                  </button>
                </article>
              );
            })}
          </div>
          {upgradeActionError ? (
            <p className="install-progress__error">{upgradeActionError}</p>
          ) : null}
          <p className="paywall__footnote">
            <button
              className="link-button"
              onClick={() => void invoke("open_external_link", { url: "https://github.com/" })}
              type="button"
            >
              See all Headroom features
            </button>
            {" • "}
            Headroom finalizes installing and starts optimizing right after checkout.
          </p>
          {signedIn ? (
            <p className="paywall__footnote">
              Signed in as {pricingStatus?.account?.email ?? authEmail}
              {" • "}
              <button
                className="link-button"
                onClick={() => void handleSignOutHeadroomAccount()}
                type="button"
              >
                or use a different email
              </button>
            </p>
          ) : null}
        </div>
      </LauncherShell>
    );
  }

  if (
    windowLabel === "launcher" && launcherStage === "post_install"
  ) {
    // The tray's 5s dashboard poll keeps running under the launcher window,
    // so a first-run user who sends a prompt sees this screen flip from
    // "waiting" to their first real savings without any interaction — the
    // payoff moment stays inside onboarding instead of being deferred to a
    // later session that a third of signups never have. While waiting,
    // blur-autohide is disarmed (see awaitingFirstSavings above).
    return (
      <LauncherShell
        shellClassName="intro-shell intro-shell--post-install"
        spinnerClassName="intro-shell__spinner intro-shell__spinner--post-install"
        copyClassName="intro-shell__copy intro-shell__copy--post-install"
        onMouseDown={handleLauncherSurfaceMouseDown}
        version={appSemver}
      >
        <div className="post-install__lead">
          <h1>
            {t("onboarding.runningBackground")}
          </h1>
          {awaitingFirstSavings ? (
            <FirstSavingsChecklist
              dashboard={dashboard}
              onReopenSetup={() => setLauncherStage("client_setup")}
            />
          ) : (
            <>
              <p>
                {dashboard.launchExperience === "first_run"
                  ? t("onboarding.firstSavingsIn")
                  : t("onboarding.backgroundDescription")}
              </p>
              <div className="post-install__metrics">
                <article className="soft-card stat-card">
                  <span className="stat-card__label">
                    <CurrencyDollar aria-hidden="true" className="stat-card__icon" size={15} weight="bold" />
                    {t("onboarding.savingsAllTime")}
                  </span>
                  <strong className="stat-value--green">{currency(dashboard.lifetimeEstimatedSavingsUsd)}</strong>
                  <p>{lifetimeDataDaysLabel}</p>
                </article>
                <article className="soft-card stat-card">
                  <span className="stat-card__label">
                    <Cpu aria-hidden="true" className="stat-card__icon" size={15} weight="bold" />
                    {t("onboarding.tokensAllTime")}
                  </span>
                  <strong className="stat-value--blue">{compactNumber(dashboard.lifetimeEstimatedTokensSaved)}</strong>
                  <p>
                    {lifetimeDataDays > 0
                      ? t("onboarding.acrossTrackedDays", { count: lifetimeDataDays })
                      : t("onboarding.acrossRecordedUsage")}
                  </p>
                </article>
              </div>
            </>
          )}
        </div>
        <div className="post-install__actions">
          <button
            className="secondary-button post-install__reopen-setup"
            onClick={() => {
              void beginProxyVerificationStep();
            }}
            type="button"
          >
            {t("actions.back")}
          </button>
          <button
            className="primary-button primary-button--large primary-button--success"
            // `complete_setup_wizard` already fired on entering this screen
            // (see the isLastScreen effect), so this is only the exit.
            onClick={() => {
              void invoke("show_dashboard_window").catch((error) => {
                console.error("show_dashboard_window failed", error);
              });
            }}
            type="button"
          >
            {t("actions.getStarted")}
          </button>
          <p>{t("launcher.menuBarNote")}</p>
        </div>
      </LauncherShell>
    );
  }

  // Cold-cache warmup: proxy is up and the ML extras are installed, but the
  // ~260MB Kompress model hasn't loaded yet (it downloads lazily on first use,
  // and the desktop prefetches it in the background after a fresh install).
  // This is normal setup, not a fault, so it must not surface as an issue.
  const kompressWarming = Boolean(
    runtimeStatus &&
      runtimeStatus.running &&
      runtimeStatus.proxyReachable &&
      runtimeStatus.mlInstalled !== false &&
      runtimeStatus.kompressEnabled === false
  );

  const runtimeIssues: string[] = [];
  if (runtimeStatus?.installed === false) {
    runtimeIssues.push(t("status.issue.runtimeNotInstalled"));
  }
  if (runtimeStatus?.running === false) {
    runtimeIssues.push(
      runtimeStatus.startupErrorHint ??
        runtimeStatus.startupError ??
        t("status.issue.runtimeOffline")
    );
  }
  if (runtimeStatus?.proxyReachable === false) {
    runtimeIssues.push(t("status.issue.proxyUnreachable"));
  }
  if (runtimeStatus?.mcpConfigured === false) {
    runtimeIssues.push(t("status.issue.mcpNotConfigured"));
  }
  if (runtimeStatus?.kompressEnabled === false && !kompressWarming) {
    runtimeIssues.push(t("status.issue.kompressDisabled"));
  }

  const runtimeHealthy = Boolean(
    runtimeStatus &&
      runtimeStatus.running &&
      runtimeStatus.proxyReachable &&
      runtimeStatus.mcpConfigured !== false &&
      (runtimeStatus.kompressEnabled !== false || kompressWarming)
  );
  const platformPreviewNotice = platformPreviewNoticeFor(
    runtimeStatus?.platform,
    runtimeStatus?.supportTier,
    t,
  );
  const headroomLearnSupported = runtimeStatus?.headroomLearnSupported !== false;
  const headroomLearnDisabledReason =
    runtimeStatus?.headroomLearnDisabledReason ??
    t("learn.unsupported");

  const calloutBanner = (() => {
    if (!runtimeStatus) {
      return {
        tone: "disconnected",
        title: t("status.unavailable")
      } as const;
    }

    if (runtimeStatus.paused) {
      if (runtimeStatus.autoPaused) {
        return {
          tone: "auto-paused",
          title: t("status.stoppedUnexpectedly")
        } as const;
      }
      return {
        tone: "paused",
        title: t("status.paused")
      } as const;
    }

    if (runtimeStatus.starting) {
      return {
        tone: "starting",
        title: t("status.starting")
      } as const;
    }

    if (pricingStatus?.needsAuthentication) {
      return {
        tone: "degraded",
        title: pricingStatus.gateMessage
      } as const;
    }

    if (pricingStatus && !pricingStatus.optimizationAllowed) {
      return {
        tone: "disabled",
        title: pricingStatus.gateMessage
      } as const;
    }

    if (pricingStatus?.shouldNudge) {
      return {
        tone: "starting",
        title: pricingStatus.gateMessage
      } as const;
    }

    // Codex-only gate: surface in the top banner only when the Claude side isn't
    // itself gating/nudging (handled above), so mixed users never get a double
    // banner. Codex billing/pausing is scoped to Codex traffic.
    const codexUsage = pricingStatus?.codex;
    if (codexUsage && codexUsage.optimizationAllowed === false) {
      return {
        tone: "disabled",
        title: codexUsage.gateMessage
      } as const;
    }
    if (codexUsage?.shouldNudge) {
      return {
        tone: "starting",
        title: codexUsage.gateMessage
      } as const;
    }

    if (runtimeHealthy) {
      if (connectorPhase === "disabled") {
        return {
          tone: "disabled",
          title: t("status.noTools")
        } as const;
      }
      if (connectorPhase === "verifying") {
        return {
          tone: "starting",
          title: t("status.verifyConnection")
        } as const;
      }
      if (kompressWarming) {
        return {
          tone: "healthy",
          title: t("status.finishingSetup")
        } as const;
      }
      return {
        tone: "healthy",
        title: t("status.healthy")
      } as const;
    }

    const disconnected = !runtimeStatus.installed || !runtimeStatus.running || !runtimeStatus.proxyReachable;
    return {
      tone: disconnected ? "disconnected" : "degraded",
      title: disconnected
        ? runtimeIssues.length > 0
          ? t("status.disconnectedWithIssues", { issues: runtimeIssues.join(t("punctuation.listSeparator")) })
          : t("status.disconnected")
        : runtimeIssues.length > 0
          ? t("status.attentionWithIssues", { issues: runtimeIssues.join(t("punctuation.listSeparator")) })
          : t("status.attention")
    } as const;
  })();

  const calloutTitle =
    calloutBanner.title.length <= 110
      ? calloutBanner.title
      : (() => {
          const primaryIssue = runtimeIssues[0];
          if (!primaryIssue) {
            return calloutBanner.title;
          }
          if (calloutBanner.tone === "disconnected") {
            return t("status.disconnectedWithIssues", { issues: primaryIssue });
          }
          return t("status.attentionWithIssues", { issues: primaryIssue });
        })();
  const tierMismatch = pricingStatus?.tierMismatch ?? null;
  // Products the clamp actually limits (per-product scope). Falls back to the
  // recommendation source for payloads cached by builds without the flags.
  const clampScopeLabel = tierMismatch
    ? [
        tierMismatch.claudeUndercovered ? "Claude" : null,
        tierMismatch.codexUndercovered ? "Codex" : null,
      ]
        .filter(Boolean)
        .join(" and ") || tierRecommendationSourceLabel(tierMismatch.recommendedSource)
    : "";
  const sortedClaudeProjects = [...claudeProjects].sort((left, right) => {
    const leftTime = Date.parse(left.lastWorkedAt);
    const rightTime = Date.parse(right.lastWorkedAt);
    return (Number.isNaN(rightTime) ? 0 : rightTime) - (Number.isNaN(leftTime) ? 0 : leftTime);
  });
  const pinnedClaudeProject =
    !showAllClaudeProjects && headroomLearnStatus.projectPath
      ? sortedClaudeProjects.find((project) => project.projectPath === headroomLearnStatus.projectPath) ?? null
      : null;
  const visibleClaudeProjects = (() => {
    if (showAllClaudeProjects) {
      return sortedClaudeProjects;
    }

    const topProjects = sortedClaudeProjects.slice(0, 3);
    if (!pinnedClaudeProject || topProjects.some((project) => project.projectPath === pinnedClaudeProject.projectPath)) {
      return topProjects;
    }
    return [...topProjects, pinnedClaudeProject];
  })();
  const hiddenClaudeProjectsCount = sortedClaudeProjects.length - visibleClaudeProjects.length;
  const trialDaysRemaining = formatRemainingDays(pricingStatus?.account?.trialEndsAt);
  const localGraceHoursRemaining = (() => {
    const target = pricingStatus?.localGraceEndsAt
      ? new Date(pricingStatus.localGraceEndsAt).getTime()
      : Number.NaN;
    if (Number.isNaN(target)) {
      return null;
    }
    return Math.max(0, Math.ceil((target - Date.now()) / 3_600_000));
  })();
  const upgradeDefaultPlanId =
    pricingAudience === "individual"
      ? (pricingStatus?.recommendedSubscriptionTier ??
          pricingStatus?.codex?.recommendedSubscriptionTier ??
          cachedPricing.recommendedSubscriptionTier ??
          upgradePlansState.featuredPlanId)
      : "enterprise";
  const upgradeDefaultPlan = upgradePlansState.plans.find((plan) => plan.id === upgradeDefaultPlanId) ?? null;

  // Upgrade-ask copy anchored on the user's own savings (items 1 & 2). Shown
  // only at a gate/nudge moment, and only when there's enough realized savings
  // for the numbers to land (helpers return null otherwise).
  const recentDailySavings = recentDailySavingsUsd(dashboard.dailySavings);
  const inUpgradeMoment =
    !!pricingStatus &&
    !pricingStatus.needsAuthentication &&
    !pricingStatus.account?.subscriptionActive &&
    (!pricingStatus.optimizationAllowed ||
      pricingStatus.shouldNudge ||
      pricingStatus.codex?.optimizationAllowed === false ||
      !!pricingStatus.codex?.shouldNudge);
  const paybackPlanId =
    upgradeDefaultPlanId === "pro" ||
    upgradeDefaultPlanId === "max5x" ||
    upgradeDefaultPlanId === "max20x"
      ? upgradeDefaultPlanId
      : null;
  // Item 1 - "pays for itself" anchor (recent monthly savings rate vs price).
  const upgradePaybackLabel =
    inUpgradeMoment && paybackPlanId
      ? paybackLabel(recentDailySavings * 30, paybackPlanId, billingPeriod)
      : null;
  // Item 2 - forgone-savings counterfactual until the active weekly limit resets.
  const weeklyGateForgoneLabel = (() => {
    if (!inUpgradeMoment || !pricingStatus) return null;
    const claudeWeeklyActive =
      !pricingStatus.optimizationAllowed || pricingStatus.shouldNudge;
    if (claudeWeeklyActive && pricingStatus.claude.weeklyResetsAt) {
      const days =
        (new Date(pricingStatus.claude.weeklyResetsAt).getTime() - Date.now()) / 86_400_000;
      return forgoneSavingsLabel(recentDailySavings, days);
    }
    // The metered window, not `secondary` — Plus reports its weekly window as
    // `primary`, which left this null and killed the Codex forgone-savings line.
    const codexResetSecs = pricingStatus.codex?.weeklyResetsInSeconds ?? null;
    if (codexResetSecs && codexResetSecs > 0) {
      return forgoneSavingsLabel(recentDailySavings, codexResetSecs / 86_400);
    }
    return null;
  })();
  // Show a single, strongest savings line: at a hard gate (optimization paused,
  // pain is live) lead with the forgone-savings loss; at a nudge lead with the
  // "pays for itself" gain. Fall back to the other only if the primary is null.
  const isHardGate =
    !!pricingStatus &&
    (!pricingStatus.optimizationAllowed || pricingStatus.codex?.optimizationAllowed === false);
  const upgradeSavingsLine = isHardGate
    ? (weeklyGateForgoneLabel ?? upgradePaybackLabel)
    : (upgradePaybackLabel ?? weeklyGateForgoneLabel);
  // Only show it when the pricing gate/nudge banner actually wins: a startup,
  // paused, or disconnected banner takes precedence over the upsell, so the
  // savings line must not leak under those titles.
  const showUpgradeSavingsLine =
    !!upgradeSavingsLine &&
    !!runtimeStatus &&
    !runtimeStatus.paused &&
    !runtimeStatus.starting;
  // Intro-offer nudge for unsubscribed users: surface the offer alongside the
  // savings line so the upgrade moment carries the same pitch as the pricing
  // page.
  const launchPromoLine = (() => {
    if (!inUpgradeMoment || !pricingStatus?.introOffer?.active) return null;
    const { percentOff, durationMonths } = pricingStatus.introOffer;
    return `Intro offer: ${percentOff}% off your first ${durationMonths} months.`;
  })();
  // When the banner is carrying an upgrade nudge (and isn't showing the Resume
  // control), let clicking the card jump straight to the upgrade view.
  const calloutIsUpgradeNudge =
    inUpgradeMoment &&
    !!runtimeStatus &&
    !runtimeStatus.paused &&
    !runtimeStatus.starting &&
    (showUpgradeSavingsLine || !!launchPromoLine);

  const activeHeadroomPlanId =
    pricingAudience === "individual" && pricingStatus?.account?.subscriptionActive
      ? pricingStatus.account.subscriptionTier ?? null
      : null;
  // Plan cards keep their slot on both tabs, but the "Active" chrome only
  // belongs to the period the subscriber actually pays for.
  const viewingSubscribedPeriod = matchesSubscriptionPeriod(
    billingPeriod,
    pricingStatus?.account?.subscriptionBillingPeriod
  );
  // A downgrade waits for the end of the term already paid for, so between
  // confirming it and it landing the subscription still reports the old plan.
  // Without this the change leaves no trace anywhere in the app.
  const pendingPlanChangeInfo = scheduledPlanChange(pricingStatus?.account);
  // The card beside the active plan: the nearest step up, since that is what
  // this view is for. Only the top tier has none, and there the tier below it
  // is the sole remaining move.
  const companionPlanId =
    getNextHigherUpgradePlanId(activeHeadroomPlanId) ?? getNextLowerUpgradePlanId(activeHeadroomPlanId);
  const visibleUpgradePlans = (() => {
    if (showAllUpgradePlans || upgradePlansState.plans.length <= 2) {
      return upgradePlansState.plans;
    }

    if (pricingAudience === "individual" && activeHeadroomPlanId && companionPlanId) {
      const visiblePlanIds = new Set<UpgradePlanId>([activeHeadroomPlanId, companionPlanId]);
      const activeWindowPlans = upgradePlansState.plans.filter((plan) => visiblePlanIds.has(plan.id));
      if (activeWindowPlans.length === 2) {
        return activeWindowPlans;
      }
    }

    // Lead with the plan matched to the user's Claude/ChatGPT tier; the
    // "show more plans" button reveals the rest.
    return upgradePlansState.plans.slice(0, 1);
  })();
  const hasHiddenUpgradePlans = visibleUpgradePlans.length < upgradePlansState.plans.length;
  const pendingUpgradePlanLabel = upgradePlanIntentLabel(pendingUpgradePlanId);
  const upgradeAuthMessage = pendingUpgradePlanLabel
    ? `Sign in with email to upgrade to the ${pendingUpgradePlanLabel} plan`
    : "Sign in with email";
  const accountDisplayEmail = (() => {
    const enteredEmail = authEmail.trim();
    return (
      pricingStatus?.account?.email ??
      (enteredEmail || pricingStatus?.claude.email || "unknown email")
    );
  })();
  const accountPlanName = (() => {
    if (!pricingStatus?.authenticated) {
      return null;
    }
    if (!pricingStatus.account) {
      return pricingStatus.accountSyncError ? "Plan unavailable" : "Syncing plan...";
    }
    if (pricingStatus.account.subscriptionActive) {
      return subscriptionTierLabel(pricingStatus.account.subscriptionTier);
    }
    if (pricingStatus.account.trialActive) {
      if (trialDaysRemaining != null) {
        return `${trialDaysRemaining} day${trialDaysRemaining === 1 ? "" : "s"} left in trial`;
      }
      return "7-day trial";
    }
    return "Trial expired";
  })();
  const upgradeTrialCallout = (() => {
    if (pricingBusy && !pricingStatus) {
      return {
        tone: "neutral" as const,
        message: "Loading your Headroom access..."
      };
    }
    if (!pricingStatus) {
      return {
        tone: "neutral" as const,
        message: "Headroom pricing status is unavailable right now."
      };
    }
    if (!pricingStatus.authenticated) {
      if (!pricingStatus.localGraceActive) {
        return {
          tone: "expired" as const,
          message: "Your 72-hour Headroom access expired. Create an account to extend to 7 days.",
          actionLabel: "Sign up",
          onAction: openUpgradeAuthView
        };
      }
      const hoursLabel =
        localGraceHoursRemaining != null
          ? `${localGraceHoursRemaining} hour${localGraceHoursRemaining === 1 ? "" : "s"}`
          : "72 hours";
      return {
        tone: "warning" as const,
        message: `${hoursLabel} left in your 72-hour trial. Create an account to extend trial to 7 days.`,
        actionLabel: "Sign up",
        onAction: openUpgradeAuthView
      };
    }
    if (!pricingStatus.account) {
      return {
        tone: "neutral" as const,
        message:
          pricingStatus.accountSyncError ??
          "Headroom account connected. Syncing your trial and plan details..."
      };
    }
    if (pricingStatus.account?.subscriptionActive) {
      return {
        tone: "healthy" as const,
        message: `${subscriptionTierLabel(pricingStatus.account.subscriptionTier)} is active. Headroom can keep optimizing without limits.`
      };
    }
    if (pricingStatus.account?.trialActive) {
      const daysLabel =
        trialDaysRemaining != null
          ? `${trialDaysRemaining} day${trialDaysRemaining === 1 ? "" : "s"}`
          : "7 days";
      return {
        tone: "warning" as const,
        message: `${daysLabel} of trial to go. Upgrade to continue using Headroom without limits.`,
        actionLabel: "Upgrade",
        onAction: () => void handleUpgradeAction(upgradeDefaultPlanId)
      };
    }
    return {
      tone: "expired" as const,
      message:
        "Your 7-day trial has ended. Upgrade to keep Headroom optimizing your prompts.",
      actionLabel: "Upgrade",
      onAction: () => void handleUpgradeAction(upgradeDefaultPlanId)
    };
  })();
  const pricingAuthCard = (
    <section className="pricing-auth-card pricing-auth-card--standalone">
      <div className="pricing-auth-card__header">
        <div>
          <h2>{upgradeAuthMessage}.</h2>
        </div>
      </div>
      {!authCodeRequestedFor ? (
        <>
          <div className="pricing-auth-card__grid pricing-auth-card__grid--single">
            <label className="pricing-auth-field">
              <span>Email</span>
              <div className="pricing-auth-field__input">
                <EnvelopeSimple size={16} weight="bold" />
                <input
                  onChange={(event) => {
                    setAuthEmail(event.target.value);
                    setAuthFlowError(null);
                  }}
                  placeholder="you@example.com"
                  type="email"
                  value={authEmail}
                />
              </div>
            </label>
          </div>
          <div className="pricing-auth-card__actions">
            <button
              className="primary-button"
              disabled={!authEmailValid || authRequestBusy}
              onClick={() => void handleRequestAuthCode()}
              type="button"
            >
              {authRequestBusy ? "Sending..." : "Sign in"}
            </button>
          </div>
          <p className="pricing-auth-card__legal">
            {"By signing in, you agree to our "}
            <button className="link-button" onClick={() => void invoke("open_external_link", { url: "https://opensource.org/license/mit" })} type="button">Terms of Service</button>
            {" and "}
            <button className="link-button" onClick={() => void invoke("open_external_link", { url: "https://opensource.org/license/mit" })} type="button">Privacy Policy</button>
            {"."}
          </p>
        </>
      ) : (
        <>
          <div className="pricing-auth-card__code-step">
            <p className="pricing-auth-card__step-copy">
              Enter the authentication code we sent to <strong>{authCodeRequestedFor}</strong>.
            </p>
            <button
              className="link-button pricing-auth-card__change-email"
              onClick={resetUpgradeAuthStep}
              type="button"
            >
              Use a different email
            </button>
          </div>
          <div className="pricing-auth-card__grid pricing-auth-card__grid--single">
            <label className="pricing-auth-field">
              <span>Authentication code</span>
              <div className="pricing-auth-field__input">
                <Key size={16} weight="bold" />
                <input
                  onChange={(event) => {
                    setAuthCode(event.target.value);
                    setAuthFlowError(null);
                  }}
                  placeholder={`Enter the code sent to ${authCodeRequestedFor}`}
                  type="text"
                  value={authCode}
                />
              </div>
            </label>
          </div>
          <div className="pricing-auth-card__actions">
            <button
              className="primary-button"
              disabled={!authCode.trim() || authVerifyBusy}
              onClick={() => void handleVerifyAuthCode()}
              type="button"
            >
              {authVerifyBusy ? "Verifying..." : "Verify and continue"}
            </button>
            <p className="pricing-auth-card__resend">
              Didn't receive a code?{" "}
              <button
                className="link-button"
                disabled={authRequestBusy}
                onClick={() => void handleRequestAuthCode()}
                type="button"
              >
                {authRequestBusy ? "Sending..." : "Resend code"}
              </button>
            </p>
          </div>
        </>
      )}
      {authFlowError ? (
        <p className="install-progress__error">{authFlowError}</p>
      ) : null}
      {authFlowSuccess ? (
        <p className="upgrade-plan-card__contact-status upgrade-plan-card__contact-status--success">
          {authFlowSuccess}
        </p>
      ) : null}
      {pricingError ? (
        <p className="install-progress__error">{pricingError}</p>
      ) : null}
    </section>
  );

  const activeNavItem = navItems.find((item) => item.id === activeView);
  const activeViewTitle = activeNavItem ? t(activeNavItem.labelKey) : t("nav.settings");
  return (
    <main className="tray-shell">
      {upgradeOverlay}
      <WindowChrome
        platform={desktopPlatform === "windows" ? "windows" : "macos"}
        title={`Headroom · ${activeViewTitle}`}
      />
      <aside className="tray-sidebar">
        <div className="tray-sidebar__logo">
          <img src={headroomAvatar} alt="Headroom" />
        </div>
        <nav className="tray-nav" aria-label={t("aria.trayNavigation")}>
          {navItems.map((item) => (
            <button
              key={item.id}
              className={`tray-nav__item${activeView === item.id ? " is-active" : ""}`}
              aria-current={activeView === item.id ? "page" : undefined}
              onClick={() => setActiveView(item.id)}
              type="button"
            >
              <span className="tray-nav__icon" aria-hidden="true">
                <item.icon className="tray-nav__icon-svg" size={26} weight={activeView === item.id ? "fill" : "regular"} />
              </span>
              <span className="tray-nav__text">
                <strong>
                    {t(item.labelKey)}
                </strong>
              </span>
            </button>
          ))}
        </nav>
        <div className="tray-sidebar__footer">
          {!LOCAL_COMMUNITY_EDITION ? (
            <button
              className={`upgrade-pill${activeView === "upgrade" || activeView === "upgradeAuth" ? " is-active" : ""}`}
              onMouseDown={() => setActiveView("upgrade")}
              type="button"
            >
              Upgrade
            </button>
          ) : null}
          <button
            className={`tray-nav__item${activeView === "settings" ? " is-active" : ""}`}
            aria-current={activeView === "settings" ? "page" : undefined}
            onClick={() => setActiveView("settings")}
            type="button"
          >
            <span className="tray-nav__icon" aria-hidden="true">
              <GearSix className="tray-nav__icon-svg" size={26} weight={activeView === "settings" ? "fill" : "regular"} />
            </span>
            <span className="tray-nav__text">
              <strong>{t("nav.settings")}</strong>
            </span>
          </button>
        </div>
      </aside>

      <section className="tray-panel">
        <header className="tray-panel__header">
          <div>
            <p className="tray-panel__eyebrow">{t("brand.tagline")}</p>
            <h1 className="tray-panel__title">{activeViewTitle}</h1>
          </div>
          <RuntimeStatusIndicator
            className="tray-panel__local-status"
            runtime={runtimeStatus}
            label={desktopPlatform === "windows"
                ? t("brand.runsOnThisDevice")
                : t("brand.runsOnThisMac")}
          />
        </header>
        <div className="tray-content" hidden={activeView !== "home"}>
            {!LOCAL_COMMUNITY_EDITION && tierMismatch ? (
              <section className="tier-mismatch-banner" role="alert">
                <div className="tier-mismatch-banner__body">
                  <h2 className="tier-mismatch-banner__title">Upgrade your Headroom plan</h2>
                  <p className="tier-mismatch-banner__message">
                    {tierMismatch.clamped
                      ? `Your ${tierRecommendationSourceLabel(tierMismatch.recommendedSource)} usage needs the Headroom ${upgradePlanIntentLabel(tierMismatch.recommendedTier)} plan, above your current Headroom ${upgradePlanIntentLabel(tierMismatch.paidTier)} plan, so weekly usage limits now apply to ${clampScopeLabel}. Upgrade to restore unlimited optimization.`
                      : `You're on the Headroom ${upgradePlanIntentLabel(tierMismatch.paidTier)} plan but your ${tierRecommendationSourceLabel(tierMismatch.recommendedSource)} usage needs the Headroom ${upgradePlanIntentLabel(tierMismatch.recommendedTier)} plan. Upgrade to match.`}
                  </p>
                  {upgradeActionError && upgradeActionBusy === null ? (
                    <p className="tier-mismatch-banner__error" role="status">
                      {upgradeActionError}
                    </p>
                  ) : null}
                </div>
                <button
                  type="button"
                  className="tier-mismatch-banner__action"
                  disabled={upgradeActionBusy === tierMismatch.recommendedTier}
                  onClick={() => void handleUpgradeAction(tierMismatch.recommendedTier)}
                >
                  {upgradeActionBusy === tierMismatch.recommendedTier
                    ? "Updating…"
                    : `Upgrade to ${upgradePlanIntentLabel(tierMismatch.recommendedTier)}`}
                </button>
              </section>
            ) : null}
            <section
              className={`callout-banner callout-banner--${calloutBanner.tone}${
                calloutIsUpgradeNudge ? " callout-banner--clickable" : ""
              }`}
              {...(calloutIsUpgradeNudge
                ? {
                    role: "button",
                    tabIndex: 0,
                    onClick: () => setActiveView("upgrade"),
                    onKeyDown: (e: React.KeyboardEvent) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        setActiveView("upgrade");
                      }
                    },
                  }
                : {})}
            >
              <span className={`callout-banner__dot callout-banner__dot--${calloutBanner.tone}`} aria-hidden="true" />
              <div className="callout-banner__body">
                <h1>{calloutTitle}</h1>
                {platformPreviewNotice ? (
                  <p className="callout-banner__subtitle">
                    {platformPreviewNotice} {t("platform.previewReportPrompt")}{" "}
                    <button
                      type="button"
                      className="link-button"
                      onClick={() =>
                        void openExternalLink(
                          platformPreviewSupportIssueUrl({
                            platform: runtimeStatus?.platform,
                            appVersion: appSemver,
                            headroomVersion,
                          }, t)
                        ).catch(() => {})
                      }
                    >
                      {t("platform.githubIssues")}
                    </button>
                  </p>
                ) : null}
                {showUpgradeSavingsLine ? (
                  <p className="callout-banner__subtitle">{upgradeSavingsLine}</p>
                ) : null}
                {calloutBanner.tone === "healthy" && dashboard.lifetimeEstimatedTokensSaved < 1_000_000 && (
                  stallBannerLine ? (
                    // Nothing has ever been saved on this install, so the
                    // reassuring "check back later" below would be a lie.
                    <p className="callout-banner__subtitle">
                      {localizeUiText(t, stallBannerLine)}
                    </p>
                  ) : (
                    <p className="callout-banner__subtitle">{t("home.checkBack")}</p>
                  )
                )}
                {(calloutBanner.tone === "auto-paused" || calloutBanner.tone === "paused") && (
                  <div className="callout-banner__resume">
                    <button
                      type="button"
                      className="callout-banner__action"
                      onClick={() => void handleResumeRuntime()}
                      disabled={resuming}
                    >
                      {resuming ? t("actions.restarting") : t("actions.resume")}
                    </button>
                    {resumeError ? (
                      <p className="callout-banner__subtitle callout-banner__error" role="status">
                        {resumeError}
                      </p>
                    ) : null}
                  </div>
                )}
              </div>
              {(() => {
                const homeConnectors = sortClientConnectors(aggregateClientConnectors(connectors))
                  .filter((connector) => connector.installed || connector.enabled);
                if (homeConnectors.length === 0) {
                  return null;
                }
                return (
                  <div className="callout-banner__connectors">
                    {homeConnectors.map((connector) => {
                      const status = connectorDashboardStatus(connector, {
                        proxyReachable: runtimeStatus?.proxyReachable
                      });
                      return (
                        <span
                          className={`callout-banner__badge callout-banner__badge--${status.tone}`}
                          key={connector.clientId}
                          data-tip={`${connector.name}: ${localizeUiText(t, status.label)}`}
                        >
                          {hasConnectorIcon(connector.clientId) ? (
                            <ConnectorIcon clientId={connector.clientId} />
                          ) : (
                            <span aria-hidden="true">
                              {connectorMonograms[connector.clientId] ?? connector.name.slice(0, 2)}
                            </span>
                          )}
                          <span className="visually-hidden">{`${connector.name}: ${localizeUiText(t, status.label)}`}</span>
                        </span>
                      );
                    })}
                  </div>
                );
              })()}
            </section>

            <section className="stat-grid stat-grid--2col">
              <article
                className={`soft-card stat-card stat-card--clickable${chartMode === "usd" ? " is-active" : ""}`}
                onClick={() => setChartMode("usd")}
                role="button"
                tabIndex={0}
                onKeyDown={(e) => e.key === "Enter" && setChartMode("usd")}
              >
                <span className="stat-card__label">
                  <CurrencyCircleDollar aria-hidden="true" className="stat-card__icon" size={15} weight="bold"/>
                  {t("home.totalCostsSaved")}
                  <button
                    className="stat-card__info-button"
                    onClick={(e) => { e.stopPropagation(); setShowSavingsInfo(true); }}
                    type="button"
                    aria-label={t("savingsInfo.title")}
                  >
                    <Info size={13} weight="bold" />
                  </button>
                </span>
                <strong className="stat-value--green">{currency(dashboard.lifetimeEstimatedSavingsUsd)}</strong>
              </article>
              <article
                className={`soft-card stat-card stat-card--clickable${chartMode === "tokens" ? " is-active" : ""}`}
                onClick={() => setChartMode("tokens")}
                role="button"
                tabIndex={0}
                onKeyDown={(e) => e.key === "Enter" && setChartMode("tokens")}
              >
                <span className="stat-card__label">
                  <Cpu aria-hidden="true" className="stat-card__icon" size={15} weight="bold"/>
                  {t("home.totalTokensSaved")}
                  <button
                    className="stat-card__info-button"
                    onClick={(e) => { e.stopPropagation(); setShowCacheInfo(true); }}
                    type="button"
                    aria-label={t("cacheInfo.title")}
                  >
                    <Info size={13} weight="bold" />
                  </button>
                </span>
                <div className="stat-value-row">
                  <strong className="stat-value--blue">
                    {compactNumber(dashboard.lifetimeEstimatedTokensSaved)}
                  </strong>
                </div>
              </article>
            </section>

            {dashboard.savingsHistoryLoaded || historyLoadTimedOut ? (
              <DailySavingsChart
                data={dashboard.dailySavings}
                hourlyData={dashboard.hourlySavings}
                resetSignal={chartResetSignal}
                chartMode={chartMode}
                setChartMode={setChartMode}
                outputReduction={dashboard.outputReduction}
              />
            ) : (
              <div className="savings-chart__skeleton" role="status">
                <p className="loading-copy">{t("home.loadingHistory")}</p>
              </div>
            )}

          </div>

        <div className="tray-content" hidden={activeView !== "optimization"}>
            <article className="soft-card optimize-card">
              <header className="optimize-card__head">
                <div className="optimize-card__title-row">
                  <span className="optimize-card__title-icon" aria-hidden="true">
                    <Brain weight="duotone" />
                  </span>
                  <h1>{t("learn.appliedHeading")}</h1>
                </div>
                <p className="optimize-card__blurb">{learnBlurb}</p>
                {headroomLearnSupported ? (
                  <div className="optimize-card__auto-learn">
                    <div className="optimize-card__auto-learn-text">
                      <span className="optimize-card__auto-learn-label">
                        {t("learn.autoLearning")}
                      </span>
                      <span className="optimize-card__auto-learn-meta">
                        {autoLearnEnabled === false
                          ? t("learn.manualOnly")
                          : autoLearnMeta}
                      </span>
                    </div>
                    <button
                      aria-checked={autoLearnEnabled ?? true}
                      aria-label={t(autoLearnEnabled === false ? "learn.enableAuto" : "learn.disableAuto")}
                      className={`connector-switch${autoLearnEnabled === false ? "" : " is-on"}`}
                      disabled={autoLearnEnabled === null || autoLearnBusy}
                      onClick={() =>
                        void handleAutoLearnToggle(autoLearnEnabled === false)
                      }
                      role="switch"
                      type="button"
                    >
                      <span className="connector-switch__thumb" />
                    </button>
                  </div>
                ) : null}
              </header>
              <div className="optimize-card__body">
                {!headroomLearnSupported ? (
                  <div className="optimize-minimal">
                    <p className="optimize-minimal__meta">
                      {headroomLearnDisabledReason}
                    </p>
                  </div>
                ) : !claudeLearnEnabled && !codexLearnEnabled ? (
                  <p className="loading-copy">
                    {t("learn.enableConnector")}
                  </p>
                ) : (
                  <div className="optimize-minimal">
                    {claudeLearnEnabled && claudeProjectsBusy && claudeProjects.length === 0 ? (
                      <p className="loading-copy">{t("settings.loading")}</p>
                    ) : claudeLearnEnabled && claudeProjects.length === 0 ? (
                      <p className="loading-copy">{t("learn.noProjects")}</p>
                    ) : claudeLearnEnabled ? (
                      <>
                    {!headroomLearnPrereq.claudeCliAvailable ? (
                      <div className="install-prompt" role="status">
                        <header className="install-prompt__head">
                          <span className="install-prompt__icon" aria-hidden="true">
                            <Terminal weight="duotone" />
                          </span>
                          <div className="install-prompt__head-text">
                            <h2 className="install-prompt__title">
                              {t("learn.installClaudeTitle")}
                            </h2>
                            <p className="install-prompt__body">
                              {t("learn.usesClaudeCli")}
                            </p>
                          </div>
                        </header>
                        <div className="install-prompt__cmd">
                          <code className="install-prompt__cmd-text">
                            {CLAUDE_CODE_INSTALL_CURL_CMD}
                          </code>
                          <button
                            className="install-prompt__cmd-copy"
                            type="button"
                            onClick={() => void copyLearnInstallCommand(CLAUDE_CODE_INSTALL_CURL_CMD)}
                          >
                            {t("actions.copy")}
                          </button>
                        </div>
                        <div className="install-prompt__foot">
                          <button
                            className="install-prompt__link"
                            type="button"
                            onClick={() => void openLearnInstallDocsLink()}
                          >
                            {t("actions.openDocs")}
                          </button>
                          <span className="install-prompt__foot-sep" aria-hidden="true">·</span>
                          <button
                            className="install-prompt__link install-prompt__link--recheck"
                            type="button"
                            onClick={() => void refreshHeadroomLearnPrereq(true)}
                          >
                            <ArrowClockwise weight="bold" size={12} aria-hidden="true" />
                            {t("actions.recheck")}
                          </button>
                          {learnInstallCopyNotice ? (
                            <span className="install-prompt__notice">
                              {learnInstallCopyNotice}
                            </span>
                          ) : null}
                        </div>
                      </div>
                    ) : null}
                    <div className="optimize-projects">
                      {visibleClaudeProjects.map((project) => {
                        const isRunning =
                          headroomLearnStatus.running &&
                          headroomLearnStatus.projectPath === project.projectPath;
                        const isLatestLearnProject =
                          headroomLearnStatus.projectPath === project.projectPath;
                        const disableLearn =
                          !headroomLearnPrereq.claudeCliAvailable ||
                          headroomLearnBusy ||
                          claudeProjectsBusy ||
                          (headroomLearnStatus.running && !isRunning);
                        const learnMeta = localizeLearnStatus(t, formatLearnStatus(project));
                        const refreshLabel = isRunning
                          ? t("actions.scanning")
                          : t("actions.scanNow");
                        const showInlineResult =
                          isLatestLearnProject &&
                          !headroomLearnStatus.running &&
                          (headroomLearnStatus.success === false ||
                            Boolean(headroomLearnStatus.error));
                        return (
                          <div
                            className={`optimize-project-row${isRunning || showInlineResult ? " optimize-project-row--active" : ""}`}
                            key={project.id}
                          >
                            <div className="optimize-project-row__main">
                              <span className="optimize-project-row__name">
                                <strong>{project.displayName}</strong>
                                <small>
                                  <span className="optimize-project-row__training" aria-live="polite">
                                    {isRunning ? (
                                      <LearnScanStatusLine
                                        step={headroomLearnStatus.currentStep}
                                        elapsedSeconds={headroomLearnStatus.elapsedSeconds}
                                      />
                                    ) : (
                                      learnMeta
                                    )}
                                    <button
                                      type="button"
                                      className={`optimize-project-row__refresh${isRunning ? " is-spinning" : ""}`}
                                      onClick={() => void handleRunHeadroomLearn("claude", project.projectPath)}
                                      disabled={disableLearn}
                                      aria-label={refreshLabel}
                                      title={refreshLabel}
                                    >
                                      <ArrowClockwise weight="bold" size={12} aria-hidden="true" />
                                    </button>
                                  </span>
                                  {/* Hidden during this project's scan so the status line
                                      keeps the row to itself; remounts with fresh counts
                                      when the run finishes. */}
                                  {!isRunning ? (
                                    <OptimizePanel
                                      projectPath={project.projectPath}
                                      neverScanned={hasNeverScanned(project)}
                                      onScan={() => void handleRunHeadroomLearn("claude", project.projectPath)}
                                      scanDisabled={disableLearn}
                                      refreshSignal={
                                        isLatestLearnProject && !headroomLearnStatus.running
                                          ? Date.parse(headroomLearnStatus.finishedAt ?? "") || 0
                                          : 0
                                      }
                                      preloadedApplied={
                                        optimizeAppliedByProject
                                          ? optimizeAppliedByProject[project.projectPath] ?? {
                                              claudeMd: [],
                                              memoryMd: [],
                                            }
                                          : undefined
                                      }
                                      onAppliedMutated={() =>
                                        setOptimizeAppliedRefreshTick((tick) => tick + 1)
                                      }
                                    />
                                  ) : null}
                                </small>
                              </span>
                              <div className="optimize-project-row__actions">
                                {showInlineResult ? (
                                  <span className="optimize-project-row__status optimize-minimal__result--failure">
                                    {t("actions.lastRunFailed")}
                                  </span>
                                ) : null}
                              </div>
                            </div>
                            {showInlineResult && headroomLearnStatus.error ? (
                              <div className="optimize-project-row__result">
                                  <p className="install-progress__error" role="alert">{headroomLearnStatus.error}</p>
                              </div>
                            ) : null}
                          </div>
                        );
                      })}
                    </div>
                    {sortedClaudeProjects.length > 3 ? (
                      <button
                        className="optimize-minimal__inline-action optimize-projects__toggle"
                        onClick={() => setShowAllClaudeProjects((current) => !current)}
                        type="button"
                      >
                        {showAllClaudeProjects ? t("actions.fewerProjects") : t("actions.moreProjects")}
                      </button>
                    ) : null}
                      </>
                    ) : null}
                    {codexLearnEnabled
                      ? (() => {
                          const codexReady =
                            headroomLearnPrereq.codexCliAvailable &&
                            headroomLearnPrereq.codexLoggedIn;
                          const codexRunning =
                            headroomLearnStatus.running &&
                            headroomLearnStatus.projectPath === "codex";
                          const codexIsLatest = headroomLearnStatus.projectPath === "codex";
                          const codexDisable =
                            !codexReady ||
                            headroomLearnBusy ||
                            (headroomLearnStatus.running && !codexRunning);
                          const codexShowResult =
                            codexIsLatest &&
                            !headroomLearnStatus.running &&
                            (headroomLearnStatus.success != null ||
                              Boolean(headroomLearnStatus.error));
                          const codexSucceeded =
                            codexShowResult &&
                            headroomLearnStatus.success === true &&
                            !headroomLearnStatus.error;
                          if (!codexReady) {
                            const codexCmd = headroomLearnPrereq.codexCliAvailable
                              ? CODEX_CLI_LOGIN_CMD
                              : CODEX_CLI_INSTALL_CMD;
                            return (
                              <div className="install-prompt" role="status">
                                <header className="install-prompt__head">
                                  <span className="install-prompt__icon" aria-hidden="true">
                                    <Terminal weight="duotone" />
                                  </span>
                                  <div className="install-prompt__head-text">
                                    <h2 className="install-prompt__title">
                                      {headroomLearnPrereq.codexCliAvailable
                                        ? t("learn.codexSignInTitle")
                                        : t("learn.codexInstallTitle")}
                                    </h2>
                                    <p className="install-prompt__body">
                                      {t("learn.codexAnalyzerPrefix")} {" "}
                                      <code>codex</code> CLI {t("learn.codexAnalyzerSuffix")}
                                      {headroomLearnPrereq.codexCliAvailable
                                        ? ` ${t("learn.signInToContinue")}`
                                        : ""}
                                    </p>
                                  </div>
                                </header>
                                <div className="install-prompt__cmd">
                                  <code className="install-prompt__cmd-text">{codexCmd}</code>
                                  <button
                                    className="install-prompt__cmd-copy"
                                    type="button"
                                    onClick={() => void copyLearnInstallCommand(codexCmd)}
                                  >
                                    {t("actions.copy")}
                                  </button>
                                </div>
                                <div className="install-prompt__foot">
                                  <button
                                    className="install-prompt__link"
                                    type="button"
                                    onClick={() => void openExternalLink(CODEX_INSTALL_DOCS_URL)}
                                  >
                                    {t("actions.openDocs")}
                                  </button>
                                  <span className="install-prompt__foot-sep" aria-hidden="true">
                                    ·
                                  </span>
                                  <button
                                    className="install-prompt__link install-prompt__link--recheck"
                                    type="button"
                                    onClick={() => void refreshHeadroomLearnPrereq(true)}
                                  >
                                    <ArrowClockwise weight="bold" size={12} aria-hidden="true" />
                                    {t("actions.recheck")}
                                  </button>
                                  {learnInstallCopyNotice ? (
                                    <span className="install-prompt__notice">
                                      {learnInstallCopyNotice}
                                    </span>
                                  ) : null}
                                </div>
                              </div>
                            );
                          }
                          return (
                            <div className="optimize-projects">
                              <div
                                className={`optimize-project-row${codexRunning || codexShowResult ? " optimize-project-row--active" : ""}`}
                              >
                                <div className="optimize-project-row__main">
                                  <span className="optimize-project-row__name">
                                    <strong>{t("learn.codexSessions")}</strong>
                                    <small>
                                      <span
                                        className="optimize-project-row__training"
                                        aria-live="polite"
                                      >
                                        {codexRunning ? (
                                          <LearnScanStatusLine
                                            step={headroomLearnStatus.currentStep}
                                            elapsedSeconds={headroomLearnStatus.elapsedSeconds}
                                          />
                                        ) : (
                                          t("learn.codexScans")
                                        )}
                                        <button
                                          type="button"
                                          className={`optimize-project-row__refresh${codexRunning ? " is-spinning" : ""}`}
                                          onClick={() => void handleRunHeadroomLearn("codex")}
                                          disabled={codexDisable}
                                          aria-label={codexRunning ? t("actions.scanning") : t("actions.scanNow")}
                                          title={codexRunning ? t("actions.scanning") : t("actions.scanNow")}
                                        >
                                          <ArrowClockwise
                                            weight="bold"
                                            size={12}
                                            aria-hidden="true"
                                          />
                                        </button>
                                      </span>
                                    </small>
                                  </span>
                                  <div className="optimize-project-row__actions">
                                    {codexShowResult ? (
                                      <span
                                        className={`optimize-project-row__status ${
                                          codexSucceeded
                                            ? "optimize-minimal__result--success"
                                            : "optimize-minimal__result--failure"
                                        }`}
                                      >
                                        {t(
                                          codexSucceeded
                                            ? "actions.lastRunSucceeded"
                                            : "actions.lastRunFailed",
                                        )}
                                      </span>
                                    ) : null}
                                  </div>
                                </div>
                                {!codexRunning ? (
                                  <OptimizePanel
                                    projectPath="codex"
                                    source="codex"
                                    onScan={() => void handleRunHeadroomLearn("codex")}
                                    scanDisabled={codexDisable}
                                    refreshSignal={
                                      codexIsLatest
                                        ? Date.parse(headroomLearnStatus.finishedAt ?? "") || 0
                                        : 0
                                    }
                                  />
                                ) : null}
                                {codexShowResult && headroomLearnStatus.error ? (
                                  <div className="optimize-project-row__result">
                              <p className="install-progress__error" role="alert">
                                      {headroomLearnStatus.error}
                                    </p>
                                  </div>
                                ) : null}
                              </div>
                            </div>
                          );
                        })()
                      : null}
                    {[
                      {
                        key: "opencode" as const,
                        enabled: opencodeLearnEnabled,
                        title: t("learn.agent.opencode"),
                        subtitle: t("learn.agent.opencodeDescription")
                      },
                      {
                        key: "grok" as const,
                        enabled: grokLearnEnabled,
                        title: t("learn.agent.grok"),
                        subtitle: t("learn.agent.grokDescription")
                      }
                    ].map((row) => {
                      if (!row.enabled) {
                        return null;
                      }
                      const ready =
                        headroomLearnPrereq.claudeCliAvailable ||
                        (headroomLearnPrereq.codexCliAvailable &&
                          headroomLearnPrereq.codexLoggedIn);
                      const running =
                        headroomLearnStatus.running &&
                        headroomLearnStatus.projectPath === row.key;
                      const isLatest = headroomLearnStatus.projectPath === row.key;
                      const disable =
                        !ready || headroomLearnBusy || (headroomLearnStatus.running && !running);
                      const showResult =
                        isLatest &&
                        !headroomLearnStatus.running &&
                        (headroomLearnStatus.success === false ||
                          Boolean(headroomLearnStatus.error));
                      return (
                        <div className="optimize-projects" key={row.key}>
                          <div
                            className={`optimize-project-row${running || showResult ? " optimize-project-row--active" : ""}`}
                          >
                            <div className="optimize-project-row__main">
                              <span className="optimize-project-row__name">
                                <strong>{row.title}</strong>
                                <small>
                                  <span
                                    className="optimize-project-row__training"
                                    aria-live="polite"
                                  >
                                    {running ? (
                                      <LearnScanStatusLine
                                        step={headroomLearnStatus.currentStep}
                                        elapsedSeconds={headroomLearnStatus.elapsedSeconds}
                                      />
                                    ) : ready
                                      ? row.subtitle
                                      : t("learn.needsAnalyzer")}
                                    <button
                                      type="button"
                                      className={`optimize-project-row__refresh${running ? " is-spinning" : ""}`}
                                      onClick={() => void handleRunHeadroomLearn(row.key)}
                                      disabled={disable}
                                      aria-label={running ? t("actions.scanning") : t("actions.scanNow")}
                                      title={running ? t("actions.scanning") : t("actions.scanNow")}
                                    >
                                      <ArrowClockwise weight="bold" size={12} aria-hidden="true" />
                                    </button>
                                  </span>
                                </small>
                              </span>
                              <div className="optimize-project-row__actions">
                                {showResult ? (
                                  <span className="optimize-project-row__status optimize-minimal__result--failure">
                                    {t("actions.lastRunFailed")}
                                  </span>
                                ) : null}
                              </div>
                            </div>
                            {showResult && headroomLearnStatus.error ? (
                              <div className="optimize-project-row__result">
                            <p className="install-progress__error" role="alert">
                                  {headroomLearnStatus.error}
                                </p>
                              </div>
                            ) : null}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
                {claudeProjectsError ? (
                  <p className="install-progress__error">{claudeProjectsError}</p>
                ) : null}
                {headroomLearnStatus.error &&
                !["codex", "opencode", "grok"].includes(headroomLearnStatus.projectPath ?? "") &&
                !claudeProjects.some((project) => project.projectPath === headroomLearnStatus.projectPath) ? (
                  <p className="install-progress__error" role="alert">{headroomLearnStatus.error}</p>
                ) : null}
              </div>
            </article>

          </div>

        <div className="tray-content" hidden={activeView !== "notifications"}>
          <ActivityFeed
            feed={activityFeed}
            error={activityFeedError}
            loaded={activityFeedLoaded}
            onNavigateToOptimize={() => setActiveView("optimization")}
            rtkInstalled={runtimeStatus?.rtk.installed === true}
            serenaInstalled={dashboard.tools.some(
              (tool) => tool.id === "serena" && tool.status !== "not_installed"
            )}
          />
        </div>

        <div className="tray-content" hidden={activeView !== "addons"}>
          <article className="soft-card addons-card">
            <header className="addons-card__head">
              <div className="addons-card__title-row">
                <span className="addons-card__title-icon" aria-hidden="true">
                  <PuzzlePiece weight="duotone" />
                </span>
                <h1>{t("nav.tools")}</h1>
              </div>
              <p className="addons-card__blurb">
                {t(
                  desktopPlatform === "windows"
                    ? "tools.descriptionDevice"
                    : "tools.description"
                )} {" "}
                <ProjectIssuesLink onOpen={openExternalLink}>
                  {t("tools.requestAddon")}
                </ProjectIssuesLink>
              </p>
              <div className="addons-card__updates" aria-live="polite">
                <button
                  type="button"
                  className="secondary-button addons-card__update-button"
                  disabled={addonUpdateBusy}
                  onClick={() => void refreshAddonUpdates()}
                >
                  <ArrowClockwise aria-hidden="true" size={14} />
                  {addonUpdateBusy ? t("addons.checkingUpdates") : t("addons.checkUpdates")}
                </button>
                {addonUpdatesChecked && !addonUpdateBusy ? (
                  <span className={addonUpdateCheckFailed ? "addons-card__update-error" : undefined}>
                    {addonUpdateCheckFailed
                      ? t("addons.updateCheckFailed")
                      : addonUpdateFailureCount > 0
                        ? t("addons.updateCheckPartial", {
                            count: addonUpdateFailureCount,
                            updates: addonUpdatesFound,
                          })
                        : addonUpdatesFound > 0
                          ? t("addons.updatesFound", { count: addonUpdatesFound })
                          : t("addons.allUpToDate")}
                  </span>
                ) : null}
              </div>
              <AddonPresetBar
                dashboard={dashboard}
                busy={addonPresetBusy}
                mode={addonPresetMode}
                onApplyRecommended={() => void applyRecommendedAddonPreset()}
                onSelectCustom={() => {
                  addonPresetUserSelectedCustomRef.current = true;
                  setAddonPresetMode("custom");
                }}
              />
            </header>
          </article>
            <div
              className="addons__groups"
              role="group"
              aria-label={t("aria.localTools")}
            >
              {TOOL_CATEGORY_ORDER.map((category) => {
                const categoryTools = groupedAddonTools.get(category) ?? [];
                const includesRtk = category === "efficiency";
                if (!categoryTools.length && !includesRtk) return null;
                const categoryCopy = toolCategoryCopy[category];
                const workflowGroup = categoryTools.find((tool) => tool.workflowGroup)?.workflowGroup;
                const singleSelectLabel = workflowGroup
                  ? workflowGroupCopy[workflowGroup]?.[resolvedLocale]
                  : null;
                return (
                  <section
 className={`addon-group addon-group--${category}`}
                    key={category}
                    aria-labelledby={`addon-group-${category}`}
                  >
                    <header className="addon-group__head">
                      <div className="addon-group__copy">
                        <h2 id={`addon-group-${category}`}>{categoryCopy.title[resolvedLocale]}</h2>
                        <p>{categoryCopy.description[resolvedLocale]}</p>
                      </div>
                      {singleSelectLabel ? (
                  <span className="tool-group__rule">{singleSelectLabel}</span>
                      ) : null}
                    </header>
                    <ul className="addons__list">
                      {categoryTools.map((tool) => {
                  const installed = tool.status !== "not_installed";
                  return (
                    <AddonCard
                      key={tool.id}
                      toolId={tool.id}
                      name={tool.name}
                      version={tool.version}
                      installed={installed}
                      enabled={tool.enabled}
                    description={
                      ADDON_DESCRIPTION_KEYS[tool.id]
                        ? t(ADDON_DESCRIPTION_KEYS[tool.id])
                        : toolCopy[tool.id]?.[resolvedLocale] ?? tool.description
                    }
                      activationLabel={getActivationScopeCopy(tool.activationScope)[resolvedLocale]}
                      copy={addonCopy[tool.id]}
                      infoOpen={addonInfoId === tool.id}
                      onToggleInfo={() =>
                        setAddonInfoId(addonInfoId === tool.id ? null : tool.id)
                      }
                      busy={tool.id in addonBusyById}
                      busyLabel={addonBusyById[tool.id] ?? null}
                      resultMessage={addonResultById[tool.id] ?? null}
                      errorMessage={addonErrorById[tool.id] ?? null}
                      upstreamVersion={tool.upstreamVersion ?? null}
                      upstreamUpdateAvailable={tool.upstreamUpdateAvailable ?? false}
                      updateRequiresAppUpdate={tool.updateRequiresAppUpdate ?? false}
                      supportedVersion={tool.supportedVersion ?? null}
                      onDismissResult={() =>
                        setAddonResultById((current) =>
                          clearAddonOperationMessage(current, tool.id)
                        )
                      }
                      sourceUrl={tool.sourceUrl}
                      onOpenSource={() => void openExternalLink(tool.sourceUrl)}
                    connectors={addonConnectors(tool.id, connectors)}
                    showClients={installed && tool.enabled}
                    savings={tool.savingsLabel ?? null}
                    defaultMode={tool.defaultMode ?? null}
                    supportedModes={tool.supportedModes ?? []}
                    actionsDisabled={addonPresetMode === "recommended" || tool.id in addonBusyById}
                    onModeChange={(mode) => void runAddonModeAction(tool.id, mode)}
                      updateAvailable={tool.updateAvailable ?? false}
                      updateActionAvailable={tool.updateActionAvailable ?? false}
                      repairActionAvailable={tool.repairActionAvailable ?? false}
                      updateCheckFailed={tool.updateCheckFailed ?? false}
                      availableVersion={tool.availableVersion ?? null}
                      unavailableReason={tool.unavailableReason ?? null}
                      onUpdate={() =>
                        // Pass the current enable state so updating a disabled
                        // addon refreshes its files without switching it on.
                        void runAddonAction("install_addon", tool.id, tool.enabled, {
                          busy: t(tool.repairActionAvailable ? "addons.repairing" : "addons.updating", { name: tool.name }),
                          done: t(tool.repairActionAvailable ? "addons.repaired" : "addons.updated", { name: tool.name })
                        }, tool.availableVersion)
                      }
                      onInstall={() => void runAddonAction("install_addon", tool.id)}
                      onToggleEnabled={() =>
                        void runAddonAction("set_addon_enabled", tool.id, !tool.enabled)
                      }
                      onUninstall={() => void runAddonAction("uninstall_addon", tool.id)}
                    />
                  );
                })}
                      {includesRtk ? (
                        <AddonCard
                key="rtk"
                toolId="rtk"
                name="RTK"
                version={runtimeStatus?.rtk.version}
                installed={runtimeStatus?.rtk.installed === true}
                enabled={runtimeStatus?.rtk.enabled === true}
                description={
                  <>
                    {t("tools.rtkDescription")}
                    {rtkAvgSavingsPct !== null
                      ? ` ${t("addons.avgSavings", { value: percent1(rtkAvgSavingsPct) })}`
                      : ""}
                  </>
                }
                activationLabel={getActivationScopeCopy(checkedRtkTool?.activationScope)[resolvedLocale]}
                copy={addonCopy.rtk}
                infoOpen={addonInfoId === "rtk"}
                onToggleInfo={() => setAddonInfoId(addonInfoId === "rtk" ? null : "rtk")}
                busy={"rtk" in addonBusyById}
                busyLabel={addonBusyById.rtk ?? null}
                resultMessage={addonResultById.rtk ?? null}
                errorMessage={addonErrorById.rtk ?? null}
                upstreamVersion={checkedRtkTool?.upstreamVersion ?? null}
                upstreamUpdateAvailable={checkedRtkTool?.upstreamUpdateAvailable ?? false}
                updateRequiresAppUpdate={checkedRtkTool?.updateRequiresAppUpdate ?? false}
                supportedVersion={checkedRtkTool?.supportedVersion ?? null}
                onDismissResult={() =>
                  setAddonResultById((current) =>
                    clearAddonOperationMessage(current, "rtk")
                  )
                }
                sourceUrl={
                  dashboard.tools.find((tool) => tool.id === "rtk")?.sourceUrl ??
                  "https://github.com/rtk-ai/rtk"
                }
                onOpenSource={() =>
                  void openExternalLink(
                    dashboard.tools.find((tool) => tool.id === "rtk")?.sourceUrl ??
                      "https://github.com/rtk-ai/rtk"
                  )
                }
                connectors={connectors}
                showClients={
                  runtimeStatus?.rtk.installed === true && runtimeStatus.rtk.enabled === true
                }
                savings={rtkSavingsChip}
                actionsDisabled={addonPresetMode === "recommended" || rtkBusy || "rtk" in addonBusyById || !runtimeStatus}
                updateAvailable={checkedRtkTool?.updateAvailable ?? false}
                updateActionAvailable={checkedRtkTool?.updateActionAvailable ?? false}
                repairActionAvailable={checkedRtkTool?.repairActionAvailable ?? false}
                updateCheckFailed={checkedRtkTool?.updateCheckFailed ?? false}
                availableVersion={checkedRtkTool?.availableVersion ?? null}
                unavailableReason={
                  checkedRtkTool?.unavailableReason ??
                  null
                }
                onUpdate={() =>
                  void runAddonAction("install_addon", "rtk", runtimeStatus?.rtk.enabled === true, {
                    busy: t("addons.updating", { name: "RTK" }),
                    done: t("addons.updated", { name: "RTK" })
                  }, checkedRtkTool?.availableVersion)
                }
                onInstall={() => void runAddonAction("install_addon", "rtk")}
                onToggleEnabled={() => void handleRtkToggle(!runtimeStatus?.rtk.enabled)}
                onUninstall={() => void runAddonAction("uninstall_addon", "rtk")}
              >
                {runtimeStatus?.rtk.installed ? (
                  <>
                    <button
                      type="button"
                      className="addon-card__link"
                      onClick={async () => {
                        const next = !showRtkDetails;
                        setShowRtkDetails(next);
                        if (next) {
                          try {
                            const lines = await invoke<string[]>("get_rtk_activity", { maxLines: 80 });
                            setRtkActivityLines(lines);
                          } catch {
                            setRtkActivityLines(["Failed to load RTK activity."]);
                          }
                        }
                      }}
                    >
                      {showRtkDetails ? t("settings.hideLogs") : t("settings.showLogs")}
                    </button>
                    {showRtkDetails ? (
                      <pre className="runtime-log" ref={rtkActivityRef}>
                        {rtkActivityLines.length > 0 ? rtkActivityLines.join("\n") : t("settings.noLogs")}
                      </pre>
                    ) : null}
                  </>
                ) : null}
                        </AddonCard>
                      ) : null}
                    </ul>
                  </section>
                );
              })}
            </div>
        </div>

        {!LOCAL_COMMUNITY_EDITION ? (
          <>
        <div className="tray-content tray-content--upgrade" hidden={activeView !== "upgrade"}>
          <section className="upgrade-hero">
            <h1>Plans based on your AI subscription</h1>
            {pricingAudience === "individual" ? (
              <div className="upgrade-billing-toggle" role="group" aria-label="Billing period">
                {(["monthly", "annual"] as const).map((period) => (
                  <button
                    key={period}
                    className={`upgrade-billing-toggle__item${billingPeriod === period ? " is-active" : ""}`}
                    onClick={() => setBillingPeriod(period)}
                    type="button"
                  >
                    {period === "annual" ? (
                      <>Annual <span className="upgrade-billing-toggle__save">Save 25%</span></>
                    ) : "Monthly"}
                  </button>
                ))}
              </div>
            ) : null}
          </section>

          {!pricingStatus?.account?.subscriptionActive ? (
            <>
              <section
                className={`upgrade-trial-callout upgrade-trial-callout--${upgradeTrialCallout.tone}`}
              >
                <div className="upgrade-trial-callout__content">
                  <p className="upgrade-trial-callout__message">
                    {upgradeTrialCallout.message}
                  </p>
                </div>
                {upgradeTrialCallout.actionLabel && upgradeTrialCallout.onAction ? (
                  <button
                    className="primary-button upgrade-trial-callout__button"
                    disabled={authRequestBusy || authVerifyBusy || upgradeActionBusy !== null}
                    onClick={() => upgradeTrialCallout.onAction?.()}
                    type="button"
                  >
                    {upgradeTrialCallout.actionLabel}
                  </button>
                ) : null}
              </section>

            </>
          ) : null}

          <section
            className={`upgrade-plan-grid${visibleUpgradePlans.length === 1 ? " upgrade-plan-grid--single" : ""}`}
          >
            {visibleUpgradePlans.map((plan) => {
              const isFeatured = plan.id === upgradePlansState.featuredPlanId;
              const downgradeButtonClassName =
                plan.ctaTone === "downgrade" ? " upgrade-plan-card__button--downgrade" : "";
              const buttonClassName =
                plan.id === "free"
                  ? `primary-button upgrade-plan-card__button upgrade-plan-card__button--free${downgradeButtonClassName}`
                  : plan.ctaVariant === "primary"
                  ? `primary-button upgrade-plan-card__button${downgradeButtonClassName}`
                  : `secondary-button upgrade-plan-card__button${downgradeButtonClassName}`;

              const isActivePlan = plan.id === activeHeadroomPlanId && viewingSubscribedPeriod;
              // The plan a scheduled change is already headed for. Its CTA
              // still reads "Downgrade to X" otherwise, inviting a second
              // click at a change that is done.
              const isPendingTarget =
                !!pendingPlanChangeInfo &&
                plan.id === pendingPlanChangeInfo.tier &&
                billingPeriod === pendingPlanChangeInfo.billingPeriod &&
                !isActivePlan;
              return (
                <article
                  className={`upgrade-plan-card${isFeatured ? " upgrade-plan-card--featured" : ""}${isActivePlan ? " upgrade-plan-card--active" : ""}`}
                  key={plan.id}
                >
                  <div className="upgrade-plan-card__top">
                    <div className="upgrade-plan-card__title-block">
                      <span className="upgrade-plan-card__icon" aria-hidden="true">
                        <Sparkle weight={isFeatured ? "fill" : "duotone"} />
                      </span>
                      <div>
                        <h2>
                          {plan.name}
                          {isActivePlan ? (
                            <span className="upgrade-plan-card__active-badge">Active</span>
                          ) : null}
                        </h2>
                        <p>{plan.tagline}</p>
                      </div>
                    </div>
                    {plan.centeredPriceLabel ? (
                      <div className="upgrade-plan-card__price-note">{plan.centeredPriceLabel}</div>
                    ) : (
                      <div>
                        {plan.originalPrice && (plan.saleBadge || !activeHeadroomPlanId) ? (
                          <div className="upgrade-plan-card__sale-row">
                            <s className="upgrade-plan-card__original-price">{plan.originalPrice}</s>
                            <span className="upgrade-plan-card__sale-badge">
                              {plan.saleBadge ??
                                introSaleBadgeLabel(pricingStatus?.introOffer) ??
                                `${pricingStatus?.activePercentOff ?? 50}% off`}
                            </span>
                          </div>
                        ) : null}
                        <div className="upgrade-plan-card__price-block">
                          <strong>{plan.price}</strong>
                          <span>
                            {plan.billingLines[0]}
                            <br />
                            {plan.billingLines[1]}
                          </span>
                        </div>
                        {plan.reversionLine && !activeHeadroomPlanId ? (
                          <p className="upgrade-plan-card__reversion">{plan.reversionLine}</p>
                        ) : null}
                      </div>
                    )}
                    {plan.purchaseInfo ? (
                      <p className="upgrade-plan-card__purchase-info">
                        {plan.purchaseInfo.cancelAtPeriodEnd && plan.purchaseInfo.endsOn
                          ? `Ends on ${plan.purchaseInfo.endsOn}`
                          : isActivePlan && pendingPlanChangeInfo
                          ? // The stored renewal price is this plan's, and this
                            // plan is not the one that renews.
                            pendingPlanChangeInfo.note
                          : isActivePlan ? (
                            <>
                              Renews at{" "}
                              <span className="upgrade-plan-card__renewal-price">
                                {plan.purchaseInfo.renewalPriceLabel}
                              </span>{" "}
                              on {plan.purchaseInfo.renewsOn}
                              {plan.purchaseInfo.renewalNote
                                ? ` (${plan.purchaseInfo.renewalNote})`
                                : ""}
                            </>
                          ) : null}
                      </p>
                    ) : null}
                  </div>
                  <div className="upgrade-plan-card__action">
                    {plan.id === "enterprise" ? (
                      <form className="upgrade-plan-card__contact-form" onSubmit={(event) => void handleContactSubmit(event)}>
                        <input
                          className="upgrade-plan-card__contact-input"
                          onChange={(event) => {
                            setContactEmail(event.target.value);
                            if (contactSubmitError) {
                              setContactSubmitError(null);
                            }
                            if (contactSubmitSuccess) {
                              setContactSubmitSuccess(null);
                            }
                          }}
                          placeholder="you@company.com"
                          type="email"
                          value={contactEmail}
                        />
                        <textarea
                          className="upgrade-plan-card__contact-textarea"
                          maxLength={2000}
                          onChange={(event) => {
                            setContactMessage(event.target.value);
                            if (contactSubmitError) {
                              setContactSubmitError(null);
                            }
                            if (contactSubmitSuccess) {
                              setContactSubmitSuccess(null);
                            }
                          }}
                          placeholder="Tell us about your team and what you're looking for (optional)"
                          rows={4}
                          value={contactMessage}
                        />
                        <button
                          className={`secondary-button upgrade-plan-card__button upgrade-plan-card__contact-submit${contactEmailValid ? " is-ready" : ""}`}
                          disabled={!contactEmailValid || contactSubmitBusy}
                          type="submit"
                        >
                          {contactSubmitBusy ? "Sending..." : plan.ctaLabel}
                        </button>
                      </form>
                    ) : isActivePlan && plan.purchaseInfo?.cancelAtPeriodEnd ? (
                      <button
                        className={buttonClassName}
                        disabled={reactivateBusy}
                        onClick={() => void handleReactivateSubscription()}
                        type="button"
                      >
                        {reactivateBusy ? "Resuming..." : `Resume ${plan.name} plan`}
                      </button>
                    ) : isActivePlan ? (
                      <div className="upgrade-plan-card__action-stack">
                        <button
                          className={buttonClassName}
                          disabled={upgradeActionBusy === plan.id}
                          onClick={() => void handleUpgradeAction(plan.id)}
                          type="button"
                        >
                          {upgradeActionBusy === plan.id ? "Opening..." : plan.ctaLabel}
                        </button>
                        {/* Only in-app entry to the billing portal (card, invoices,
                            downgrade) now that the Free card is gone for active
                            subscribers. Cancelling is deliberately its own action so
                            updating a card does not come with a retention pitch. */}
                        <div className="upgrade-plan-card__manage-row">
                          <button
                            className="upgrade-plan-card__manage-link"
                            disabled={upgradeActionBusy === "free"}
                            onClick={() => void handleUpgradeAction("free")}
                            type="button"
                          >
                            {upgradeActionBusy === "free" ? "Opening..." : "Manage billing"}
                          </button>
                          <button
                            className="upgrade-plan-card__manage-link"
                            disabled={upgradeActionBusy === "cancel"}
                            onClick={openCancelReason}
                            type="button"
                          >
                            {upgradeActionBusy === "cancel" ? "Opening..." : "Cancel subscription"}
                          </button>
                        </div>
                      </div>
                    ) : isPendingTarget ? (
                      <button className={buttonClassName} disabled type="button">
                        {plan.id === activeHeadroomPlanId ? "Switch scheduled" : "Change scheduled"}
                      </button>
                    ) : (
                      <button
                        className={buttonClassName}
                        disabled={upgradeActionBusy === plan.id}
                        onClick={() => void handleUpgradeAction(plan.id)}
                        type="button"
                      >
                        {upgradeActionBusy === plan.id ? "Opening..." : plan.ctaLabel}
                      </button>
                    )}
                  </div>

                  {plan.features.length > 0 ? (
                    <div className="upgrade-plan-card__features">
                      <ul>
                        {plan.features.map((feature) => (
                          <li key={feature}>{feature}</li>
                        ))}
                      </ul>
                    </div>
                  ) : null}
                  {plan.id === "enterprise" && contactSubmitError ? (
                    <p className="upgrade-plan-card__contact-status upgrade-plan-card__contact-status--error">
                      {contactSubmitError}
                    </p>
                  ) : null}
                  {plan.id === "enterprise" && contactSubmitSuccess ? (
                    <p className="upgrade-plan-card__contact-status upgrade-plan-card__contact-status--success">
                      {contactSubmitSuccess}
                    </p>
                  ) : null}
                </article>
              );
            })}
          </section>
          {pricingAudience === "individual" && (hasHiddenUpgradePlans || showAllUpgradePlans) ? (
            <button
              className="upgrade-plan-grid__toggle"
              onClick={() => setShowAllUpgradePlans((current) => !current)}
              type="button"
            >
              {showAllUpgradePlans ? "show fewer plans" : "show more plans"}
            </button>
          ) : null}

          {upgradeActionError ? (
            <p className="install-progress__error">{upgradeActionError}</p>
          ) : null}
          {reactivateError ? (
            <p className="install-progress__error">{reactivateError}</p>
          ) : null}
        </div>

        <div className="tray-content tray-content--upgrade" hidden={activeView !== "upgradeAuth"}>
          <section className="upgrade-auth-view">
            <div className="upgrade-auth-view__header">
              <div className="upgrade-auth-view__title-row">
                <button
                  aria-label="Back to upgrade plans"
                  className="upgrade-auth-view__back"
                  onClick={() => setActiveView("upgrade")}
                  type="button"
                >
                  <CaretLeft size={16} weight="bold" />
                </button>
                <h1>Create account</h1>
              </div>
            </div>
            {pricingAuthCard}
          </section>
        </div>
          </>
        ) : null}

        <div className="tray-content tray-content--settings" hidden={activeView !== "settings"}>
            <section className="panel-stack">
              {!LOCAL_COMMUNITY_EDITION ? (
                <article className="soft-card panel-card settings-account-card">
                <div className="settings-account-row">
                  <p className="settings-account-copy">
                    Headroom account:{" "}
                    {pricingStatus?.authenticated ? (
                      <>
                        {accountDisplayEmail} <em>({accountPlanName})</em>
                      </>
                    ) : (
                      <em>not signed in</em>
                    )}
                  </p>
                  {pricingStatus?.authenticated ? (
                    <button
                      className="secondary-button secondary-button--small"
                      onClick={() => void handleSignOutHeadroomAccount()}
                      type="button"
                    >
                      <SignOut size={16} weight="bold" />
                      Sign out
                    </button>
                  ) : (
                    <button
                      className="secondary-button secondary-button--small"
                      onClick={() => openUpgradeAuthView()}
                      type="button"
                    >
                      Sign in
                    </button>
                  )}
                </div>
                {pricingStatus?.claude?.profileFetchError ? (
                  <p className="settings-account-notice">
                    {pricingStatus.claude.profileFetchError}
                  </p>
                ) : null}
                </article>
              ) : null}

              <article className="soft-card panel-card">
                <div className="panel-card__header">
                  <div>
                    <h3>{t("settings.language")}</h3>
                    <p>{t("settings.description")}</p>
                  </div>
                  <select
                    aria-label={t("settings.language")}
                    className="community-language-select"
                    onChange={(event) => setLocale(event.target.value as Locale)}
                    value={locale}
                  >
                    {localeOptions.map((option) => (
                      <option key={option.value} value={option.value}>
                        {t(option.labelKey)}
                      </option>
                    ))}
                  </select>
                </div>
              </article>

              <article className="soft-card panel-card">
                <div className="panel-card__header">
                  <div>
                    <h3>{t("settings.connections")}</h3>
                  </div>
                </div>
                <div className="connector-list">
                  {sortClientConnectors(aggregateClientConnectors(connectors)).map((connector) => {
                    const connectorLabel =
                      connector.clientId === "claude_code"
                        ? t("connections.claudeConnection")
                        : connector.clientId === "codex"
                          ? t("connections.codexConnection")
                          : connector.name;
                    const unavailableReason = getConnectorUnavailableReason(connector);
                    const detectionWarning = getConnectorDetectionWarning(connector);
                    const gateBlocksEnable =
                      !LOCAL_COMMUNITY_EDITION && connectorGateBlocksEnable(connector);
                    const statusLine = connectorStatusLine(connector);
                    const toggleDisabled =
                      connectorsBusy ||
                      codexRestartBusy ||
                      !canConfigureConnectorWithoutDetection(connector) ||
                      gateBlocksEnable;
                    return (
                      <article className="connector-item" key={connector.clientId}>
                        <div>
                          <h3>
                            <span className="client-logo" aria-hidden="true">
                              {renderConnectorLogo(connector.clientId)}
                            </span>
                            {connectorLabel}
                            <button
                              className="connector-help"
                              onClick={() =>
                                setOpenConnectorHelpId((current) =>
                                  current === connector.clientId ? null : connector.clientId
                                )
                              }
                              type="button"
                              aria-label={t("connections.showDetails", { name: connector.name })}
                              aria-expanded={openConnectorHelpId === connector.clientId}
                            >
                              i
                            </button>
                          </h3>
                          {openConnectorHelpId === connector.clientId ? (
                            <div className="connector-tooltip">
                              <p>
                                {CONNECTOR_SETUP_KEYS[connector.clientId]
                                  ? t(CONNECTOR_SETUP_KEYS[connector.clientId])
                                  : t("connections.localConfiguration")}
                              </p>
                              {connector.enabled ? (
                                <div className="connector-diagnostics">
                                  <strong>{t("connections.checks")}</strong>
                                  {connector.verification ? (
                                    <ul>
                                      {connector.verification.checks.map((check) => (
                                        <li className="is-good" key={check}>
                                  ✓ {localizeUiText(t, check)}
                                        </li>
                                      ))}
                                      {connector.verification.failures.map((failure) => (
                                        <li className="is-bad" key={failure}>
                                  × {localizeUiText(t, failure)}
                                        </li>
                                      ))}
                                      {connector.verification.foreignProvider ? (
                                        <li className="is-note" key="foreign-provider">
                                  • {t("connections.verification.codexForeignProvider", {
                                            provider: connector.verification.foreignProvider,
                                          })}
                                        </li>
                                      ) : null}
                                      {!connector.verification.proxyReachable ? (
                                        <li className="is-waiting">
                                          … {t("connections.proxyNotAnswering")}
                                        </li>
                                      ) : null}
                                    </ul>
                                  ) : (
                                    <p>{t("connections.noVerificationDetails")}</p>
                                  )}
                                  <button
                                    className="addon-card__link connector-diagnostics__refresh"
                                    disabled={connectorsBusy}
                                    onClick={() => void refreshConnectors()}
                                    type="button"
                                  >
                                    {connectorsBusy ? t("settings.checking") : t("actions.recheck")}
                                  </button>
                                </div>
                              ) : null}
                            </div>
                          ) : null}
                          {statusLine ? (
                            <p className={`connector-item__${statusLine.tone}`}>
                              {localizeUiText(t, statusLine.text)}
                            </p>
                          ) : null}
                          {(detectionWarning ?? unavailableReason) ? (
                            <p className="connector-item__reason">
                              {localizeUiText(t, detectionWarning ?? unavailableReason ?? "")}
                            </p>
                          ) : null}
                          {gateBlocksEnable ? (
                            <p className="connector-item__reason">
                              {pricingStatus?.gateMessage}{" "}
                              <button
                                className="addon-card__link"
                                type="button"
                                onClick={connectorGateCta}
                              >
                                {pricingStatus?.authenticated ? "Upgrade" : "Sign in"}
                              </button>
                            </p>
                          ) : null}
                        </div>
                        <div className="connector-item__controls">
                          <button
                            aria-checked={connector.enabled}
                            aria-label={t(connector.enabled ? "connections.disableConnector" : "connections.enableConnector", { name: connector.name })}
                            className={`connector-switch${connector.enabled ? " is-on" : ""}`}
                            disabled={toggleDisabled}
                            onClick={() =>
                              void toggleConnector(connector, !connector.enabled)
                            }
                            role="switch"
                            title={unavailableReason ?? undefined}
                            type="button"
                          >
                            <span className="connector-switch__thumb" />
                          </button>
                          {connector.clientId === "codex" &&
                          connector.restartRequired === true &&
                          statusLine?.tone === "restart" ? (
                            <button
                              className="secondary-button secondary-button--small"
                              disabled={connectorsBusy || codexRestartBusy}
                              onClick={() => void restartCodexDesktop()}
                              type="button"
                            >
                              {codexRestartBusy
                                ? t("connections.restartingCodex")
                                : t("connections.restartCodex")}
                            </button>
                          ) : null}
                        </div>
                      </article>
                    );
                  })}
                </div>
                {connectorsError ? (
                  <p className="install-progress__error">{connectorsError}</p>
                ) : null}
                {connectorsNotice ? (
                  <p className="install-progress__notice">{connectorsNotice}</p>
                ) : null}
              </article>

              <article className="soft-card panel-card">
                <div className="panel-card__header">
                  <div>
                    <h3>{t("settings.toolsStatus")}</h3>
                  </div>
                </div>
                <div className="runtime-status">
                  <div className="runtime-status__topline">
                    <span className="runtime-status__section-title">
                    {t("settings.headroomApp")} ({appSemver})
                      {appUpdateConfig?.betaChannelEnabled ? (
                        <span className="runtime-status__channel-pill">{t("settings.betaChannel")}</span>
                      ) : null}
                    </span>
                  </div>
                  <div className="runtime-status__section-action-row">
                    <button
                      className="secondary-button secondary-button--small"
                      disabled={appUpdateBusy || appUpdateInstallBusy}
                      onClick={() => void checkForAppUpdate()}
                      type="button"
                    >
                      {appUpdateBusy ? t("settings.checking") : t("settings.checkForUpdates")}
                    </button>
                    {appUpdateStatusCopy ? (
                      <p className="app-update-card__summary runtime-status__summary">
                        {localizeAppUpdateCopy(t, appUpdateStatusCopy)}
                      </p>
                    ) : null}
                  </div>
            <div className="runtime-status__meta">
              <span className="runtime-status__section-title">
                {t("settings.headroomCli")} ({headroomVersion})
                      {(compressionOfRestPct ?? headroomLifetimeSavingsPct) !== null ? (
                        <span className="runtime-status__section-context">
                          {" "}
                          ({t("settings.inputRemovedAllTime", { value: percent1((compressionOfRestPct ?? headroomLifetimeSavingsPct)!) })})
                        </span>
                      ) : null}
              </span>
            </div>
            {headroomTool?.updateAvailable ? (
              <div className="runtime-status__section-action-row">
                <button
                  className="secondary-button secondary-button--small"
                  disabled={runtimeUpgradeProgress.running}
                    onClick={() => void handleHeadroomCliUpdate(headroomTool.availableVersion)}
                  type="button"
                >
                  {runtimeUpgradeProgress.running
                    ? t("addons.updating", { name: t("settings.headroomCli") })
                    : t("addons.updateTo", {
                            version: formatAddonVersion(
                              headroomTool.availableVersion ?? headroomTool.supportedVersion ?? "",
                              resolvedLocale,
                            ),
                      })}
                </button>
                {cliUpdateError ? (
                  <p className="app-update-card__summary runtime-status__summary app-update-card__summary--error">
                    {cliUpdateError}
                  </p>
                ) : null}
              </div>
            ) : null}
            {headroomTool?.updateRequiresAppUpdate && headroomTool.upstreamVersion ? (
              <p className="app-update-card__summary runtime-status__summary">
                {t("addons.upstreamRequiresAppUpdate", {
                  latest: formatAddonVersion(headroomTool.upstreamVersion, resolvedLocale),
                  supported: formatAddonVersion(
                    headroomTool.supportedVersion ?? headroomVersion,
                    resolvedLocale,
                  ),
                })}
              </p>
            ) : null}
            <div className="runtime-status__grid runtime-status__grid--4">
                    {([
                      {
                        name: t("settings.runtime"),
                        ok: runtimeStatus?.running === true,
                      },
                      {
                        name: t("settings.proxy"),
                        ok: runtimeStatus?.proxyReachable === true,
                        suffix: "6867",
                        onClick: () => void invoke("open_headroom_dashboard"),
                      },
                      {
                        name: "MCP",
                        ok:
                          runtimeStatus?.mcpConfigured === true
                            ? true
                            : runtimeStatus?.mcpConfigured === false
                              ? false
                              : null,
                      },
                      {
                        name: "Kompress",
                        ok: kompressWarming
                          ? null
                          : runtimeStatus?.kompressEnabled === true
                            ? true
                            : runtimeStatus?.kompressEnabled === false
                              ? false
                              : null,
                        suffix: kompressWarming ? t("settings.warmingUp") : undefined,
                      },
                    ] as { name: string; ok: boolean | null; suffix?: string; onClick?: () => void }[]).map((s) => {
                      const indicatorClass =
                        s.ok === true
                          ? "runtime-status__indicator--ok"
                          : s.ok === false
                            ? "runtime-status__indicator--off"
                            : "runtime-status__indicator--unknown";
                      const indicatorSymbol = s.ok === true ? "✔" : s.ok === false ? "✖" : "–";
                      return (
                        <span
                          key={s.name}
                          className={`runtime-status__item${s.onClick ? " runtime-status__item--clickable" : ""}`}
                          onClick={s.onClick}
                          title={s.ok === null ? t("settings.statusUnknown", { name: s.name }) : undefined}
                        >
                          <span className="runtime-status__label">{s.name}:</span>
                          <span className={`runtime-status__indicator ${indicatorClass}`}>
                            {indicatorSymbol}
                          </span>
                          {s.suffix && <span className="runtime-status__suffix">({s.suffix})</span>}
                        </span>
                      );
                    })}
                  </div>
                  <button
                    className="link-button runtime-status__section-action"
                    onClick={async () => {
                      const next = !showHeadroomDetails;
                      setShowHeadroomDetails(next);
                      if (next) {
                        try {
                          const lines = await invoke<string[]>("get_headroom_logs", { maxLines: 80 });
                          setHeadroomLogLines(lines);
                        } catch {
                          setHeadroomLogLines(["Failed to load headroom logs."]);
                        }
                      }
                    }}
                    type="button"
                  >
                    {showHeadroomDetails ? t("settings.hideLogs") : t("settings.showLogs")}
                  </button>
                  {showHeadroomDetails ? (
                    <pre className="runtime-log" ref={headroomLogRef}>
                      {headroomLogLines.length > 0 ? headroomLogLines.join("\n") : t("settings.noLogs")}
                    </pre>
                  ) : null}
                </div>
              </article>
              <article className="soft-card panel-card">
                <div className="panel-card__header">
                  <div>
                    <h3>{t("settings.autostart")}</h3>
                  </div>
                  <div>
                    <p>
                      {t(
                        desktopPlatform === "windows"
                          ? "settings.startupDescriptionDevice"
                          : "settings.startupDescription"
                      )}
                    </p>
                  </div>
                  <div className="connector-item__controls">
                    <button
                      aria-checked={autostartEnabled === true}
                      aria-label={t("settings.autostart")}
                      className={`connector-switch${autostartEnabled ? " is-on" : ""}`}
                      disabled={autostartBusy || autostartEnabled === null}
                      onClick={() => void handleAutostartToggle(!autostartEnabled)}
                      role="switch"
                      type="button"
                    >
                      <span className="connector-switch__thumb" />
                    </button>
                  </div>
                </div>
              </article>

              <article className="soft-card panel-card">
                <div className="panel-card__header">
                  <div>
                    <h3>{t("settings.uninstall")}</h3>
                  </div>
                </div>
                <p>
                  {t("settings.uninstallDescription")}
                </p>
                <button
                  className="secondary-button secondary-button--small"
                  onClick={() => {
                    setUninstallError(null);
                    setShowUninstallDialog(true);
                  }}
                  type="button"
                >
                  {t("settings.uninstallAction")}
                </button>
            </article>

            <article className="soft-card panel-card">
              <div className="panel-card__header">
                <div>
                  <h3>{t("settings.creditsTitle")}</h3>
                </div>
              </div>
              <p>{t("settings.creditsBody")}</p>
              <p className="credits-license-note">{t("settings.creditsLicenseNote")}</p>
              <div className="credits-list">
                <button
                  className="link-button"
                  onClick={() =>
                    void openExternalLink("https://github.com/headroomlabs-ai/headroom")
                  }
                  type="button"
                >
                  {t("settings.creditsHeadroomCore")}
                </button>
                <button
                  className="link-button"
                  onClick={() => void openExternalLink("https://github.com/rtk-ai/rtk")}
                  type="button"
                >
                  {t("settings.creditsRtk")}
                </button>
                <button
                  className="link-button"
                  onClick={() =>
                    void openExternalLink("https://github.com/microsoft/markitdown")
                  }
                  type="button"
                >
                  {t("settings.creditsMarkitdown")}
                </button>
              </div>
            </article>

<button
              className="quit-button"
                onClick={() => void invoke("quit_headroom")}
                type="button"
              >
                {t("settings.quit")}
              </button>
            </section>
          </div>

          {setupStall && (
            <SetupStallModal
              kind={setupStall.kind}
              onClose={() => setSetupStall(null)}
              onOpenSettings={() => {
                setSetupStall(null);
                setActiveView("settings");
              }}
              onContact={() => {
                void invoke("open_external_link", {
                  url: buildSetupStallMailto(setupStall.kind, {
                    appVersion: appSemver,
                    lifetimeRequests: dashboard.lifetimeRequests,
                    runtime: runtimeStatus,
                    connectors
                  })
                });
              }}
            />
          )}

          {showSavingsInfo && (
            <div
              className="modal-backdrop"
              role="dialog"
              aria-modal="true"
              onClick={() => setShowSavingsInfo(false)}
            >
              <div className="modal-card" onClick={(e) => e.stopPropagation()}>
                <h3>{t("savingsInfo.title")}</h3>
                <p>{t("savingsInfo.description")}</p>
                <p>{t("savingsInfo.formula")}</p>
                {dashboard.savingsBreakdown ? (
                  <div className="savings-breakdown">
                    <div className="savings-breakdown__row">
                      <span>{t("savingsInfo.inputCompression")}</span>
                      <strong>{currency(dashboard.savingsBreakdown.compressionSavingsUsd)}</strong>
                    </div>
                    {dashboard.savingsBreakdown.outputSavingsUsd >= 0.005 ? (
                      <>
                        <div className="savings-breakdown__row">
                          <span>{t("savingsInfo.outputShaping")}</span>
                          <strong>{currency(dashboard.savingsBreakdown.outputSavingsUsd)}</strong>
                        </div>
                        {/* Lifetime figure, recomputed from the shaper's ledger
                            (see output_savings.rs) and predating per-day
                            tracking of the layer -- so the daily bars can add
                            up to less. `requests` counts what the estimate
                            actually covers, not every shaped request: strata
                            the baseline never observed are excluded rather
                            than scored against a global mean. */}
                        <p className="savings-breakdown__note">
                          {dashboard.outputReduction?.method === "measured"
                            ? t("savingsInfo.outputMeasured", { count: compactNumber(dashboard.outputReduction.requests) })
                            : t("savingsInfo.outputEstimated", { count: dashboard.outputReduction ? compactNumber(dashboard.outputReduction.requests) : "—" })}
                        </p>
                      </>
                    ) : null}
                    {(dashboard.savingsBreakdown.toolSchemaTokensSaved ?? 0) > 0 ? (
                      <>
                        <div className="savings-breakdown__row">
                          <span>{t("savingsInfo.toolSchemas")}</span>
                          <strong>{currencyExact(dashboard.savingsBreakdown.toolSchemaSavingsUsd ?? 0)}</strong>
                        </div>
                        {/* Priced at the cache-read rate, not the input rate --
                            see tool_schema_savings_usd in state.rs. */}
                        <p className="savings-breakdown__note">
                          {t("savingsInfo.toolSchemasNote", { count: compactNumber(dashboard.savingsBreakdown.toolSchemaTokensSaved ?? 0) })}
                        </p>
                      </>
                    ) : null}
                    {dashboard.savingsBreakdown.cacheSavingsUsd >= 0.005 ? (
                      <>
                        <div className="savings-breakdown__row savings-breakdown__row--context">
                          <span>{t("savingsInfo.promptCaching")}</span>
                          <strong>{currency(dashboard.savingsBreakdown.cacheSavingsUsd)}</strong>
                        </div>
                        <p className="savings-breakdown__note">
                          {t("savingsInfo.promptCachingNote")}
                        </p>
                      </>
                    ) : null}
                    {(dashboard.savingsBreakdown.modelRates?.length ?? 0) > 1 ? (
                      <details className="savings-breakdown__models">
                        <summary>{t("savingsInfo.byModel")}</summary>
                        <div className="savings-breakdown__models-body">
                          {dashboard.savingsBreakdown.modelRates?.map((row) => (
                            <div className="savings-breakdown__row" key={row.model}>
                              <span>
                                {row.model}{" "}
                                <span className="savings-breakdown__sample">
                                  {t("savingsInfo.requests", { count: compactNumber(row.requests) })}
                                </span>
                              </span>
                              <strong>{percent1(row.savingsPercent)}%</strong>
                            </div>
                          ))}
                          {/* Rates only -- by_model covers a fraction of lifetime
                              history, so its dollars would not add up to the rows
                              above. See ModelSavingsRate in models.rs. */}
                          <p className="savings-breakdown__note">
                            {t("savingsInfo.byModelNote")}
                          </p>
                        </div>
                      </details>
                    ) : null}
                  </div>
                ) : null}
                <div className="modal-actions">
                  <button
                    className="button button--primary"
                    onClick={() => setShowSavingsInfo(false)}
                    type="button"
                  >
                    {t("actions.gotIt")}
                  </button>
                </div>
              </div>
            </div>
          )}

          {showCacheInfo && (
            <div
              className="modal-backdrop"
              role="dialog"
              aria-modal="true"
              onClick={() => setShowCacheInfo(false)}
            >
              <div className="modal-card" onClick={(e) => e.stopPropagation()}>
                <h3>{t("cacheInfo.title")}</h3>
                <p>{t("cacheInfo.description")}</p>
                <div className="savings-breakdown">
                  {[
                    { label: t("cacheInfo.today"), pair: cachePairToday },
                    { label: t("cacheInfo.month"), pair: cachePairMonth },
                    { label: t("cacheInfo.allTime"), pair: cachePairAllTime }
                  ].map(({ label, pair }) => (
                    <div className="savings-breakdown__row" key={label}>
                      <span>{label}</span>
                      <strong>
                        {pair
                          ? t("cacheInfo.rate", { hits: Math.round(pair.hitPct), compressed: Math.round(pair.compressedPct) })
                          : t("cacheInfo.noData")}
                      </strong>
                    </div>
                  ))}
                </div>
                <p className="savings-breakdown__note">
                  {t("cacheInfo.note")}
                </p>
                <div className="modal-actions">
                  <button
                    className="button button--primary"
                    onClick={() => setShowCacheInfo(false)}
                    type="button"
                  >
                    {t("actions.gotIt")}
                  </button>
                </div>
              </div>
            </div>
          )}

          {showUninstallDialog ? (
            <div
              className="modal-backdrop"
              role="dialog"
              aria-modal="true"
              onClick={() => {
                if (!uninstallBusy) {
                  setShowUninstallDialog(false);
                }
              }}
            >
              <div className="modal-card" onClick={(e) => e.stopPropagation()}>
                <h3>{t("uninstall.title")}</h3>
                <p>{t("uninstall.will")}</p>
                <ul className="api-key-guide">
                  <li>
                    {t("uninstall.restoreRouting")}
                  </li>
                  <li>
                    {t("uninstall.removeAddons")}
                  </li>
                  <li>
                    {t("uninstall.deleteData")}
                  </li>
                  <li>{t("uninstall.disableLogin")}</li>
                  <li>{t("uninstall.removeApp")}</li>
                </ul>
                <p className="uninstall-note">
                  {t("uninstall.terminalPrefix")} {" "}
                  <code>ANTHROPIC_BASE_URL</code> {t("uninstall.terminalMiddle")} {" "}
                  <code>unset ANTHROPIC_BASE_URL</code>.
                </p>
                <p>{t("uninstall.reinstall")}</p>
                {uninstallError ? (
                  <p className="install-progress__error">{uninstallError}</p>
                ) : null}
                <div className="modal-actions">
                  <button
                    className="secondary-button"
                    disabled={uninstallBusy}
                    onClick={() => setShowUninstallDialog(false)}
                    type="button"
                  >
                    {t("actions.cancel")}
                  </button>
                  <button
                    className="primary-button"
                    disabled={uninstallBusy}
                    onClick={() => void handleUninstall()}
                    type="button"
                  >
                    {uninstallBusy ? t("uninstall.busy") : t("uninstall.action")}
                  </button>
                </div>
              </div>
            </div>
          ) : null}

          {pendingPlanChange ? (() => {
            const isDowngrade = isTierDowngrade(
              pendingPlanChange.fromTier,
              pendingPlanChange.toTier
            );
            // Same tier, different billing period: a switch, not an upgrade.
            const isPeriodSwitch = pendingPlanChange.fromTier === pendingPlanChange.toTier;
            const action = isPeriodSwitch ? "switch" : isDowngrade ? "downgrade" : "upgrade";
            // The current plan is priced on the period it was bought on, not
            // the one being switched to.
            const currentBillingPeriod =
              pricingStatus?.account?.subscriptionBillingPeriod === "annual"
                ? "annual"
                : pricingStatus?.account?.subscriptionBillingPeriod === "monthly"
                ? "monthly"
                : pendingPlanChange.billingPeriod;
            const currentPriceLabel = getPlanRenewalPriceLabel(
              pendingPlanChange.fromTier,
              currentBillingPeriod,
              {
                fromTier: pendingPlanChange.fromTier,
                currentPaidCents: pricingStatus?.account?.subscriptionAmountCents
              }
            );
            // The target card already prices this tier for this period with the
            // account discount carried at its exact ratio. Re-deriving it from
            // the amount currently paid quoted $0 to anyone mid 100%-off period,
            // and 12x the real figure on an annual -> monthly switch (an annual
            // cycle amount divided by one month).
            const targetCard = upgradePlansState.plans.find(
              (plan) => plan.id === pendingPlanChange.toTier
            );
            const newPriceLabel = targetCard
              ? `${targetCard.price} / month`
              : getPlanRenewalPriceLabel(pendingPlanChange.toTier, pendingPlanChange.billingPeriod);
            // Mirrors the server's rule: a step down the ladder, or the same
            // tier on a shorter cycle, waits for the term already paid for
            // rather than crediting back the unused part of it.
            const isDeferred =
              isDowngrade ||
              (isPeriodSwitch &&
                currentBillingPeriod === "annual" &&
                pendingPlanChange.billingPeriod === "monthly");
            const renewsOnLabel = pricingStatus?.account?.subscriptionRenewsAt
              ? new Date(pricingStatus.account.subscriptionRenewsAt).toLocaleDateString(undefined, {
                  year: "numeric",
                  month: "long",
                  day: "numeric"
                })
              : null;
            return (
              <div
                className="modal-backdrop"
                role="dialog"
                aria-modal="true"
                onClick={cancelPlanChange}
              >
                <div className="modal-card" onClick={(e) => e.stopPropagation()}>
                  <h3>Confirm your {action}</h3>
                  <p>
                    You'll {action} from your{" "}
                    <strong>{currentPriceLabel}</strong>{" "}
                    <strong>{upgradePlanIntentLabel(pendingPlanChange.fromTier)}</strong>{" "}
                    plan to the{" "}
                    <strong>{newPriceLabel}</strong>{" "}
                    <strong>{upgradePlanIntentLabel(pendingPlanChange.toTier)}</strong>{" "}
                    plan, billed{" "}
                    {pendingPlanChange.billingPeriod === "annual" ? "annually" : "monthly"}.
                  </p>
                  <p>
                    {isDeferred
                      ? `Nothing changes today: you keep the ${
                          upgradePlanIntentLabel(pendingPlanChange.fromTier) ?? "current"
                        } plan you have paid for${
                          renewsOnLabel ? ` until ${renewsOnLabel}` : " until the end of the term"
                        }, and the new plan starts then. No charge and no credit today.`
                      : "You'll be charged a prorated amount today for the remaining time in your current billing period, with your existing discount applied."}
                  </p>
                  {/* A period switch moves the renewal date, so the stored one
                      would be stale here; a deferred change already named it. */}
                  {!isPeriodSwitch && !isDeferred && pricingStatus?.account?.subscriptionRenewsAt ? (
                    <p>
                      Your subscription will then renew on{" "}
                      <strong>
                        {new Date(
                          pricingStatus.account.subscriptionRenewsAt
                        ).toLocaleDateString(undefined, {
                          year: "numeric",
                          month: "long",
                          day: "numeric"
                        })}
                      </strong>
                      .
                    </p>
                  ) : null}
                  {planChangeError ? (
                    <p className="install-progress__error">{planChangeError}</p>
                  ) : null}
                  <div className="modal-actions">
                    <button
                      className="secondary-button"
                      disabled={planChangeBusy}
                      onClick={cancelPlanChange}
                      type="button"
                    >
                      Cancel
                    </button>
                    <button
                      className="primary-button"
                      disabled={planChangeBusy}
                      onClick={() => void confirmPlanChange()}
                      type="button"
                    >
                      {planChangeBusy
                        ? isPeriodSwitch
                          ? "Switching…"
                          : isDowngrade
                          ? "Downgrading…"
                          : "Upgrading…"
                        : `Confirm ${action}`}
                    </button>
                  </div>
                </div>
              </div>
            );
          })() : null}

          {cancelReasonOpen ? (
            <div
              className="modal-backdrop"
              role="dialog"
              aria-modal="true"
              onClick={() => {
                if (!cancelBusy) setCancelReasonOpen(false);
              }}
            >
              <div className="modal-card" onClick={(e) => e.stopPropagation()}>
                <h3>Why are you cancelling?</h3>
                <p>
                  This goes straight to us and takes one click. It is the only way
                  we find out what is not working.
                </p>
                <div className="reason-list">
                  {CANCELLATION_REASONS.map((reason) => (
                    <label className="reason-list__item" key={reason.value}>
                      <input
                        checked={cancelReason === reason.value}
                        disabled={cancelBusy}
                        name="cancellation-reason"
                        onChange={() => setCancelReason(reason.value)}
                        type="radio"
                        value={reason.value}
                      />
                      <span>{reason.label}</span>
                    </label>
                  ))}
                </div>
                <textarea
                  className="reason-note"
                  disabled={cancelBusy}
                  onChange={(event) => setCancelNote(event.target.value)}
                  placeholder="Anything else you want to add? (optional)"
                  rows={3}
                  value={cancelNote}
                />
                <div className="modal-actions">
                  <button
                    className="secondary-button"
                    disabled={cancelBusy}
                    onClick={() => setCancelReasonOpen(false)}
                    type="button"
                  >
                    Never mind
                  </button>
                  <button
                    className="primary-button"
                    disabled={!cancelReason || cancelBusy}
                    onClick={() => void handleCancelContinue()}
                    type="button"
                  >
                    {cancelBusy ? "One moment..." : "Continue"}
                  </button>
                </div>
              </div>
            </div>
          ) : null}

          {saveOffer ? (
            <div
              className="modal-backdrop"
              role="dialog"
              aria-modal="true"
              onClick={() => {
                if (!saveOfferBusy) setSaveOffer(null);
              }}
            >
              <div className="modal-card" onClick={(e) => e.stopPropagation()}>
                {saveOfferRedeemed ? (
                  <>
                    <h3>You're all set</h3>
                    <p>
                      Your plan stays active at{" "}
                      <strong>{formatCents(saveOffer.offerMonthlyCents)} / month</strong>{" "}
                      for the next {saveOffer.durationMonths} months. The new price
                      takes effect{" "}
                      {saveOffer.startsOn ? `on ${saveOffer.startsOn}` : "at your next renewal"}.
                    </p>
                    <div className="modal-actions">
                      <button
                        className="primary-button"
                        onClick={() => setSaveOffer(null)}
                        type="button"
                      >
                        Done
                      </button>
                    </div>
                  </>
                ) : (
                  <>
                    <h3>Before you go: {saveOffer.percentOff}% off for {saveOffer.durationMonths} months</h3>
                    <p>
                      Stay on your plan and pay{" "}
                      <strong>{formatCents(saveOffer.offerMonthlyCents)} / month</strong>{" "}
                      instead of{" "}
                      <strong>{formatCents(saveOffer.currentMonthlyCents)} / month</strong>{" "}
                      for the next {saveOffer.durationMonths} months
                      {saveOffer.billingPeriod === "annual" ? ", billed annually" : ""}.
                      That is {saveOffer.percentOff}% off the price your plan renews
                      at.
                    </p>
                    <p>
                      The new price starts{" "}
                      {saveOffer.startsOn ? `on ${saveOffer.startsOn}` : "at your next renewal"}
                      , and any discount you are on until then is unaffected. Nothing
                      else about your plan changes, and you can still cancel any time.
                    </p>
                    {saveOfferError ? (
                      <p className="install-progress__error">{saveOfferError}</p>
                    ) : null}
                    <div className="modal-actions">
                      <button
                        className="secondary-button"
                        disabled={saveOfferBusy}
                        onClick={() => void handleDeclineSaveOffer()}
                        type="button"
                      >
                        Continue to cancel
                      </button>
                      <button
                        className="primary-button"
                        disabled={saveOfferBusy}
                        onClick={() => void handleRedeemSaveOffer()}
                        type="button"
                      >
                        {saveOfferBusy
                          ? "Applying..."
                          : `Keep it at ${formatCents(saveOffer.offerMonthlyCents)} / mo`}
                      </button>
                    </div>
                  </>
                )}
              </div>
            </div>
          ) : null}

          {showAppUpdateDialog && appUpdateAvailable ? (
            <div className="modal-backdrop" role="dialog" aria-modal="true">
              <div className="modal-card">
                <h3>
                  {appUpdateReadyToRestart
                    ? t("update.restartTitle", { version: appUpdateAvailable.version })
                    : t("update.availableTitle", { version: appUpdateAvailable.version })}
                </h3>
                <p>
                  {appUpdateReadyToRestart
                    ? t("update.restartDescription")
                    : t("update.availableDescription")}
                </p>
                <ul className="api-key-guide">
                  <li>{t("update.currentVersion")}: {appUpdateAvailable.currentVersion}</li>
                  <li>{t("update.newVersion")}: {appUpdateAvailable.version}</li>
                  <li>
                    {t("update.published")}: {formatDateTime(appUpdateAvailable.publishedAt ?? null)}
                  </li>
                </ul>
                {appUpdateAvailable.notes && appUpdateAvailable.notes.trim() ? (
                  <div className="release-notes">
                    <h4>{t("update.whatsNew")}</h4>
                    <pre>{appUpdateAvailable.notes.trim()}</pre>
                  </div>
                ) : null}
                {appUpdateStatusCopy ? (
                  <p className="app-update-card__summary">{localizeAppUpdateCopy(t, appUpdateStatusCopy)}</p>
                ) : null}
                <div className="modal-actions">
                  <button
                    className="secondary-button"
                    disabled={appUpdateInstallBusy || appUpdateRestartBusy}
                    onClick={() => setShowAppUpdateDialog(false)}
                    type="button"
                  >
                    {t("update.later")}
                  </button>
                  <button
                    className="primary-button"
                    disabled={appUpdateInstallBusy || appUpdateRestartBusy}
                    onClick={() =>
                      appUpdateReadyToRestart
                        ? restartIntoInstalledUpdate()
                        : void installAvailableUpdate()
                    }
                    type="button"
                  >
                    {appUpdateRestartBusy
                      ? t("actions.restarting")
                      : appUpdateInstallBusy
                        ? t("addons.installing", { name: "Headroom" })
                        : appUpdateReadyToRestart
                          ? t("update.restartNow")
                          : t("update.installVersion", { version: appUpdateAvailable.version })}
                  </button>
                </div>
              </div>
            </div>
          ) : null}
      </section>
    </main>
  );
}
