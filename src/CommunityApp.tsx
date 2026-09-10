import { useCallback, useEffect, useMemo, useRef, useState, type ElementType } from "react";
import {
  ArrowClockwise,
  Brain,
  Bug,
  ChartLine,
  GearSix,
  House,
  PuzzlePiece,
  Terminal,
} from "@phosphor-icons/react";
import { invoke } from "@tauri-apps/api/core";

import { ActivityFeed } from "./components/ActivityFeed";
import { ConnectorIcon } from "./components/ConnectorIcon";
import { LearnScanStatusLine } from "./components/LearnScanStatusLine";
import { OptimizePanel } from "./components/OptimizePanel";
import { RuntimeStatusIndicator } from "./components/RuntimeStatusIndicator";
import { localeOptions, useI18n, type Locale, type Translate, type TranslationKey } from "./lib/i18n";
import { LOCAL_COMMUNITY_NAME } from "./lib/localEdition";
import { conflictHeadingCopy, conflictMatrixCopy, getActivationScopeCopy, groupToolsByCategory, sourceLinkCopy, toolCategoryCopy, toolCopy, TOOL_CATEGORY_ORDER, workflowGroupCopy } from "./lib/workflowCatalog";
import type {
  ActivityFeedResponse,
  ClaudeCodeProject,
  ClientConnectorStatus,
  ClientSetupResult,
  DashboardState,
  HeadroomLearnPrereqStatus,
  HeadroomLearnStatus,
  ManagedTool,
  RuntimeStatus,
} from "./lib/types";

type CommunityView = "overview" | "connections" | "tools" | "activity" | "optimize" | "diagnostics" | "settings";
type LearnAgent = "claude" | "codex" | "opencode" | "grok";

interface NavigationItem {
  id: CommunityView;
  labelKey: TranslationKey;
  icon: ElementType;
}

const navigationItems: NavigationItem[] = [
  { id: "overview", labelKey: "nav.overview", icon: House },
  { id: "connections", labelKey: "nav.connections", icon: Terminal },
  { id: "tools", labelKey: "nav.tools", icon: PuzzlePiece },
  { id: "activity", labelKey: "nav.activity", icon: ChartLine },
  { id: "optimize", labelKey: "nav.optimize", icon: Brain },
  { id: "diagnostics", labelKey: "nav.diagnostics", icon: Bug },
  { id: "settings", labelKey: "nav.settings", icon: GearSix },
];

const EMPTY_ACTIVITY_FEED: ActivityFeedResponse = {
  proxyReachable: false,
  tiles: {
    transformation: null,
    record: null,
    rtkToday: null,
    serenaToday: null,
    learningsMilestone: null,
    weeklyRecap: null,
    trainSuggestion: null,
  },
};

const learnAgents: Array<{ id: LearnAgent; labelKey: TranslationKey; descriptionKey: TranslationKey }> = [
  { id: "claude", labelKey: "learn.agent.claude", descriptionKey: "learn.agent.claudeDescription" },
  { id: "codex", labelKey: "learn.agent.codex", descriptionKey: "learn.agent.codexDescription" },
  { id: "opencode", labelKey: "learn.agent.opencode", descriptionKey: "learn.agent.opencodeDescription" },
  { id: "grok", labelKey: "learn.agent.grok", descriptionKey: "learn.agent.grokDescription" },
];

const toolRuntimeKeys: Partial<Record<ManagedTool["runtime"], TranslationKey>> = {
  python: "tools.runtime.python",
  binary: "tools.runtime.binary",
  plugin: "tools.runtime.plugin",
};

const toolDescriptionKeys: Record<string, TranslationKey> = {
  rtk: "addons.description.rtk",
  markitdown: "addons.description.markitdown",
  ponytail: "addons.description.ponytail",
  caveman: "addons.description.caveman",
  allinluna: "addons.info.allinluna",
  serena: "addons.description.serena",
  "codebase-memory": "addons.description.codebaseMemory",
  context7: "addons.description.context7",
};

const toolModeLabelKeys: Record<string, TranslationKey> = {
  lite: "tools.mode.lite",
  full: "tools.mode.full",
  ultra: "tools.mode.ultra",
  "wenyan-lite": "tools.mode.wenyanLite",
  "wenyan-full": "tools.mode.wenyanFull",
  "wenyan-ultra": "tools.mode.wenyanUltra",
};

function describeError(error: unknown, t: Translate): string {
  if (error instanceof Error && error.message.trim()) {
    return error.message;
  }
  if (typeof error === "string" && error.trim()) {
    return error;
  }
  return t("messages.localOperationFailed");
}

function proxyStatus(runtime: RuntimeStatus | null, t: Translate): {
  title: string;
  detail: string;
  tone: "ready" | "idle" | "attention";
} {
  if (!runtime) {
    return {
      title: t("proxy.checkingTitle"),
      detail: t("proxy.checkingDetail"),
      tone: "idle",
    };
  }
  if (runtime.starting) {
    return {
      title: t("proxy.startingTitle"),
      detail: t("proxy.startingDetail"),
      tone: "idle",
    };
  }
  if (runtime.running && runtime.proxyReachable) {
    return {
      title: t("proxy.onlineTitle"),
      detail: t("proxy.onlineDetail"),
      tone: "ready",
    };
  }
  if (runtime.paused) {
    return {
      title: t("proxy.pausedTitle"),
      detail: t("proxy.pausedDetail"),
      tone: "idle",
    };
  }
  if (runtime.autoPaused || runtime.startupError) {
    return {
      title: t("proxy.attentionTitle"),
      detail: runtime.startupErrorHint ?? runtime.startupError ?? t("proxy.attentionFallback"),
      tone: "attention",
    };
  }
  return {
    title: t("proxy.offlineTitle"),
    detail: t("proxy.offlineDetail"),
    tone: "attention",
  };
}

function toolStatusLabel(tool: ManagedTool, t: Translate): string {
  if (tool.unavailableReason) return t("tools.status.unavailable");
  if (tool.status === "healthy") return t("tools.status.ready");
  if (tool.status === "installing") return t("tools.status.installing");
  if (tool.status === "degraded") return t("tools.status.attention");
  return t("tools.status.notInstalled");
}

function toolModeLabel(mode: string, t: Translate): string {
  const labelKey = toolModeLabelKeys[mode];
  return labelKey ? t(labelKey) : mode;
}

function selectedToolMode(tool: ManagedTool, supportedModes: readonly string[]): string | undefined {
  if (tool.defaultMode && supportedModes.includes(tool.defaultMode)) return tool.defaultMode;
  return supportedModes.includes("full") ? "full" : supportedModes[0];
}

function formatNumber(value: number | undefined, locale: string): string {
  return value === undefined ? "—" : new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(value);
}

function formatCurrency(value: number, locale: string): string {
  return new Intl.NumberFormat(locale, {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 2,
  }).format(value);
}

function redactDiagnosticLogLine(line: string): string {
  return line
    .replace(/(authorization\s*[:=]\s*)(\S+)/gi, "$1[redacted]")
    .replace(/((?:api[_-]?key|token|secret|password)\s*[:=]\s*)(\S+)/gi, "$1[redacted]");
}

function learnPrereqMessage(
  agent: LearnAgent,
  prereq: HeadroomLearnPrereqStatus | null,
  runtime: RuntimeStatus | null,
  hasClaudeProject: boolean,
  t: Translate,
): string | null {
  if (runtime?.headroomLearnSupported === false) {
    return runtime.headroomLearnDisabledReason ?? t("learn.unsupported");
  }
  if (!prereq) return t("learn.checking");
  if (agent === "claude") {
    if (!prereq.claudeCliAvailable) return t("learn.needsClaude");
    if (!hasClaudeProject) return t("learn.noProjects");
    return null;
  }
  if (agent === "codex") {
    if (!prereq.codexCliAvailable || !prereq.codexLoggedIn) return t("learn.needsCodex");
    return null;
  }
  if (!prereq.claudeCliAvailable && !(prereq.codexCliAvailable && prereq.codexLoggedIn)) {
    return t("learn.needsAnalyzer");
  }
  return null;
}

function DiagnosticStatus({
  label,
  detail,
  tone,
}: {
  label: string;
  detail: string;
  tone: "ready" | "idle" | "attention";
}) {
  return (
    <article className={`community-diagnostic-status is-${tone}`}>
      <span className="community-diagnostic-status__indicator" aria-hidden="true" />
      <div>
        <h3>{label}</h3>
        <p>{detail}</p>
      </div>
    </article>
  );
}

function DashboardMetric({ label, value, detail }: { label: string; value: string; detail: string }) {
  return (
    <article className="community-metric">
      <p>{label}</p>
      <strong>{value}</strong>
      <span>{detail}</span>
    </article>
  );
}

export function CommunityApp() {
  const { locale, resolvedLocale, setLocale, t } = useI18n();
  const [activeView, setActiveView] = useState<CommunityView>("overview");
  const [dashboard, setDashboard] = useState<DashboardState | null>(null);
  const [runtime, setRuntime] = useState<RuntimeStatus | null>(null);
  const [connectors, setConnectors] = useState<ClientConnectorStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [activityFeed, setActivityFeed] = useState<ActivityFeedResponse>(EMPTY_ACTIVITY_FEED);
  const [activityLoaded, setActivityLoaded] = useState(false);
  const [activityError, setActivityError] = useState<string | null>(null);
  const [claudeProjects, setClaudeProjects] = useState<ClaudeCodeProject[]>([]);
  const [selectedClaudeProjectPath, setSelectedClaudeProjectPath] = useState<string | null>(null);
  const [learnAgent, setLearnAgent] = useState<LearnAgent>("claude");
  const [learnPrereq, setLearnPrereq] = useState<HeadroomLearnPrereqStatus | null>(null);
  const [learnStatus, setLearnStatus] = useState<HeadroomLearnStatus | null>(null);
  const [learnResourcesLoading, setLearnResourcesLoading] = useState(false);
  const [learnError, setLearnError] = useState<string | null>(null);
  const [autoLearnEnabled, setAutoLearnEnabled] = useState<boolean | null>(null);
  const [logs, setLogs] = useState<string[]>([]);
  const [logsLoaded, setLogsLoaded] = useState(false);
  const [logsError, setLogsError] = useState<string | null>(null);
  const [autostartEnabled, setAutostartEnabled] = useState<boolean | null>(null);
  const busyActionRef = useRef<string | null>(null);
  const learnResourcesRequestRef = useRef(0);
  const learnRunKeyRef = useRef<string | null>(null);

  const refresh = useCallback(async (showLoading = false) => {
    if (showLoading) setLoading(true);

    const [dashboardResult, runtimeResult, connectorsResult] = await Promise.allSettled([
      invoke<DashboardState>("get_dashboard_state"),
      invoke<RuntimeStatus>("get_runtime_status"),
      invoke<ClientConnectorStatus[]>("get_client_connectors"),
    ]);

    const errors: string[] = [];
    if (dashboardResult.status === "fulfilled") {
      setDashboard(dashboardResult.value);
    } else {
      errors.push(describeError(dashboardResult.reason, t));
    }
    if (runtimeResult.status === "fulfilled") {
      setRuntime(runtimeResult.value);
    } else {
      errors.push(describeError(runtimeResult.reason, t));
    }
    if (connectorsResult.status === "fulfilled") {
      setConnectors(connectorsResult.value);
    } else {
      errors.push(describeError(connectorsResult.reason, t));
    }

    setLoadError(errors.length ? errors[0] : null);
    if (showLoading) setLoading(false);
  }, [t]);

  useEffect(() => {
    void refresh(true).finally(() => {
      window.dispatchEvent(new CustomEvent("headroom:boot-complete"));
    });
    const interval = window.setInterval(() => {
      void refresh();
    }, 5_000);
    return () => window.clearInterval(interval);
  }, [refresh]);

  const refreshActivity = useCallback(async () => {
    try {
      const next = await invoke<ActivityFeedResponse>("get_activity_feed");
      setActivityFeed(next);
      setActivityError(null);
    } catch (error) {
      setActivityError(describeError(error, t));
    } finally {
      setActivityLoaded(true);
    }
  }, [t]);

  const refreshLearnResources = useCallback(async () => {
    const requestId = ++learnResourcesRequestRef.current;
    setLearnResourcesLoading(true);
    const [projectsResult, prereqResult, autoLearnResult] = await Promise.allSettled([
      invoke<ClaudeCodeProject[]>("get_claude_code_projects"),
      invoke<HeadroomLearnPrereqStatus>("get_headroom_learn_prereq_status"),
      invoke<boolean>("get_auto_learn_enabled"),
    ]);

    const errors: string[] = [];
    if (projectsResult.status === "fulfilled") {
      if (requestId === learnResourcesRequestRef.current) {
        const projects = projectsResult.value;
        setClaudeProjects(projects);
        setSelectedClaudeProjectPath((current) => (
          current && projects.some((project) => project.projectPath === current)
            ? current
            : projects[0]?.projectPath ?? null
        ));
      }
    } else {
      errors.push(describeError(projectsResult.reason, t));
    }
    if (prereqResult.status === "fulfilled") {
      if (requestId === learnResourcesRequestRef.current) {
        const prereq = prereqResult.value;
        setLearnPrereq(prereq);
        const hasClaudeProjects = projectsResult.status === "fulfilled" && projectsResult.value.length > 0;
        if (prereq.codexCliAvailable && prereq.codexLoggedIn
          && (!prereq.claudeCliAvailable || !hasClaudeProjects)) {
          setLearnAgent((current) => current === "claude" ? "codex" : current);
        }
      }
    } else {
      errors.push(describeError(prereqResult.reason, t));
    }
    if (autoLearnResult.status === "fulfilled") {
      if (requestId === learnResourcesRequestRef.current) setAutoLearnEnabled(autoLearnResult.value);
    } else {
      errors.push(describeError(autoLearnResult.reason, t));
    }
    if (requestId === learnResourcesRequestRef.current) {
      setLearnError(errors[0] ?? null);
      setLearnResourcesLoading(false);
    }
  }, [t]);

  const learnRunKey = learnAgent === "claude" ? selectedClaudeProjectPath : learnAgent;
  learnRunKeyRef.current = learnRunKey;
  useEffect(() => {
    learnResourcesRequestRef.current += 1;
  }, [learnRunKey]);

  const refreshLearnStatus = useCallback(async () => {
    if (!learnRunKey) {
      setLearnStatus(null);
      return;
    }
    const requestedRunKey = learnRunKey;
    try {
      const next = await invoke<HeadroomLearnStatus>("get_headroom_learn_status", { projectPath: learnRunKey });
      if (learnRunKeyRef.current !== requestedRunKey) return;
      setLearnStatus(next);
      setLearnError(null);
    } catch (error) {
      if (learnRunKeyRef.current !== requestedRunKey) return;
      setLearnError(describeError(error, t));
    }
  }, [learnRunKey, t]);

  const refreshLogs = useCallback(async () => {
    try {
      const next = await invoke<string[]>("get_headroom_logs", { maxLines: 120 });
      setLogs(next.map(redactDiagnosticLogLine));
      setLogsError(null);
    } catch (error) {
      setLogsError(describeError(error, t));
    } finally {
      setLogsLoaded(true);
    }
  }, [t]);

  useEffect(() => {
    if (activeView !== "activity") return;
    void refreshActivity();
    const interval = window.setInterval(() => {
      void refreshActivity();
    }, 4_000);
    return () => window.clearInterval(interval);
  }, [activeView, refreshActivity]);

  useEffect(() => {
    if (activeView !== "optimize") return;
    void refreshLearnResources();
  }, [activeView, refreshLearnResources]);

  useEffect(() => {
    if (activeView !== "optimize") return;
    void refreshLearnStatus();
    const interval = window.setInterval(() => {
      void refreshLearnStatus();
    }, learnStatus?.running ? 900 : 3_200);
    return () => window.clearInterval(interval);
  }, [activeView, learnStatus?.running, refreshLearnStatus]);

  // A completed Learn run updates the project's persisted metadata (including
  // `lastLearnRanAt` and the pattern count). Refresh that snapshot before the
  // applied-patterns panel decides whether this is still an unscanned project;
  // otherwise the first successful run remains stuck on "not scanned yet".
  useEffect(() => {
    if (activeView !== "optimize" || !learnStatus?.finishedAt) return;
    void refreshLearnResources();
  }, [activeView, learnStatus?.finishedAt, refreshLearnResources]);

  useEffect(() => {
    if (activeView !== "diagnostics") return;
    void refreshLogs();
  }, [activeView, refreshLogs]);

  useEffect(() => {
    if (activeView !== "settings") return;
    let active = true;
    void invoke<boolean>("get_autostart_enabled")
      .then((enabled) => {
        if (active) setAutostartEnabled(enabled);
      })
      .catch((error) => {
        if (active) setActionError(describeError(error, t));
      });
    return () => {
      active = false;
    };
  }, [activeView, t]);

  const runAction = useCallback(
    async <T,>(
      key: string,
      action: () => Promise<T>,
      successMessage?: (result: T) => string | null,
      afterSuccess?: (result: T) => Promise<void> | void,
    ) => {
      if (busyActionRef.current) return undefined;
      busyActionRef.current = key;
      setBusyAction(key);
      setActionError(null);
      setNotice(null);
      try {
        const result = await action();
        await afterSuccess?.(result);
        const message = successMessage?.(result);
        if (message) setNotice(message);
      await refresh();
      return result;
    } catch (error) {
      // A command may mutate part of the installation before reporting an
      // error (for example, a post-install verification can fail). Re-read
      // the authoritative state so the controls never keep their pre-action
      // labels after a partial success.
      await refresh();
      setActionError(describeError(error, t));
      return undefined;
      } finally {
        busyActionRef.current = null;
        setBusyAction(null);
      }
    },
    [refresh, t],
  );

  const handleRuntime = (command: "start_headroom" | "pause_headroom" | "force_restart_headroom") => {
    const key = `runtime:${command}`;
    const success = command === "pause_headroom"
      ? t("messages.routingPaused")
      : command === "force_restart_headroom"
        ? t("messages.runtimeRestarted")
        : t("messages.routingStarted");
    void runAction(key, () => invoke<void>(command), () => success);
  };

  const handleConnector = (connector: ClientConnectorStatus) => {
    const key = `connector:${connector.clientId}`;
    if (connector.enabled) {
      void runAction(
        key,
        () => invoke<void>("disable_client_setup", { clientId: connector.clientId }),
        () => t("messages.connectorDisconnected", { name: connector.name }),
      );
      return;
    }

    // Another tool may own the Codex route right now. Taking it over replaces
    // that tool's provider, so ask first; Headroom records the displaced
    // provider and restores it when the connector is disabled again.
    const foreignProvider = connector.verification?.foreignProvider ?? null;
    if (
      foreignProvider &&
      !window.confirm(t("connections.setup.takeoverConfirm", { provider: foreignProvider }))
    ) {
      return;
    }

    void runAction(
      key,
      () =>
        invoke<ClientSetupResult>("apply_client_setup", {
          clientId: connector.clientId,
          allowTakeover: Boolean(foreignProvider),
        }),
      () => t("messages.connectorConfigured", { name: connector.name }),
    );
  };

  const handleTool = (tool: ManagedTool) => {
    const key = `tool:${tool.id}`;
    if (tool.status === "not_installed") {
      void runAction(
        key,
        () => invoke<DashboardState>("install_addon", { id: tool.id }),
        () => t("messages.toolInstalled", { name: tool.name }),
      );
      return;
    }
    void runAction(
      key,
      () => invoke<DashboardState>("set_addon_enabled", { id: tool.id, enabled: !tool.enabled }),
      () => t(tool.enabled ? "messages.toolDisabled" : "messages.toolEnabled", { name: tool.name }),
    );
  };

  const handleToolRemoval = (tool: ManagedTool) => {
    void runAction(
      `tool:remove:${tool.id}`,
      () => invoke<DashboardState>("uninstall_addon", { id: tool.id }),
      () => t("messages.toolRemoved", { name: tool.name }),
    );
  };

  const handleToolMode = (tool: ManagedTool, mode: string) => {
    void runAction(
      `tool:mode:${tool.id}:${mode}`,
      () => invoke<DashboardState>("set_addon_mode", { id: tool.id, mode }),
      () => t("messages.toolModeSaved", { name: tool.name, mode: toolModeLabel(mode, t) }),
    );
  };

  const selectedClaudeProject = claudeProjects.find((project) => project.projectPath === selectedClaudeProjectPath) ?? null;
  const selectedLearnIssue = learnPrereqMessage(
    learnAgent,
    learnPrereq,
    runtime,
    Boolean(selectedClaudeProject),
    t,
  );

  const handleStartLearn = () => {
    if (selectedLearnIssue) {
      setLearnError(selectedLearnIssue);
      return;
    }
    const projectPath = learnAgent === "claude" ? selectedClaudeProjectPath : null;
    const agent = learnAgent;
    void runAction(
      `learn:${agent}`,
      () => invoke<void>("start_headroom_learn", { agent, projectPath }),
      () => t("messages.learnStarted", { name: t(learnAgents.find((item) => item.id === agent)?.labelKey ?? "learn.agent.claude") }),
      refreshLearnStatus,
    );
  };

  const handleAutoLearnToggle = (enabled: boolean) => {
    void runAction(
      "auto-learn",
      () => invoke<boolean>("set_auto_learn_enabled", { enabled }),
      (nextEnabled) => {
        setAutoLearnEnabled(nextEnabled);
        return t(nextEnabled ? "messages.autoLearnEnabled" : "messages.autoLearnDisabled");
      },
    );
  };

  const handleAutostartToggle = (enabled: boolean) => {
    void runAction(
      "autostart",
      () => invoke<boolean>("set_autostart_enabled", { enabled }),
      (nextEnabled) => {
        setAutostartEnabled(nextEnabled);
        return t(nextEnabled ? "messages.autostartEnabled" : "messages.autostartDisabled");
      },
    );
  };

  const handleLocalNotification = () => {
    void runAction(
      "local-notification",
      () => invoke<void>("show_notification", {
        title: t("settings.localNotificationTitle"),
        body: t("settings.localNotificationBody"),
        action: "local-test",
      }),
      () => t("messages.localNotificationSent"),
    );
  };

  const handleQuit = () => {
    if (busyActionRef.current) return;
    busyActionRef.current = "quit";
    setBusyAction("quit");
    setActionError(null);
    setNotice(null);
    void invoke<void>("quit_headroom")
      .catch((error) => setActionError(describeError(error, t)))
      .finally(() => {
        busyActionRef.current = null;
        setBusyAction(null);
      });
  };

  const handleUninstall = () => {
    const confirmed = window.confirm(t("confirm.remove"));
    if (!confirmed) return;
    void runAction(
      "uninstall",
      () => invoke<string[]>("uninstall_and_quit"),
      () => t("messages.cleanupComplete"),
    );
  };

  const currentProxy = proxyStatus(runtime, t);
  const controlsDisabled = busyAction !== null;
  const tools = dashboard?.tools ?? [];
  const groupedTools = useMemo(() => groupToolsByCategory(tools), [tools]);
  const activeNavigationItem = navigationItems.find((item) => item.id === activeView);
  const selectedLearnAgent = learnAgents.find((item) => item.id === learnAgent) ?? learnAgents[0];
  const learnTargetPath = learnAgent === "claude" ? selectedClaudeProjectPath : learnAgent;
  const learnStatusMatchesTarget = Boolean(
    learnStatus && learnTargetPath && learnStatus.projectPath === learnTargetPath,
  );
  // A completed run is authoritative for the currently selected target even
  // before the project-list cache catches up. This matters for Codex, which
  // has no Claude project row (and therefore no `lastLearnRanAt` field).
  const completedLearnForTarget = Boolean(
    learnStatusMatchesTarget
      && learnStatus?.success === true
      && (learnStatus.finishedAt || learnStatus.lastRunAt),
  );
  const savingsTrend = useMemo(() => {
    if (!dashboard) return { points: [], periodKey: "overview.trendDays" as TranslationKey };
    if (dashboard.dailySavings.length) {
      return {
        points: dashboard.dailySavings.slice(-7).map((point) => ({
          label: point.date,
          savings: point.estimatedSavingsUsd,
          tokens: point.estimatedTokensSaved,
        })),
        periodKey: "overview.trendDays" as TranslationKey,
      };
    }
    return {
      points: dashboard.hourlySavings.slice(-8).map((point) => ({
        label: point.hour,
        savings: point.estimatedSavingsUsd,
        tokens: point.estimatedTokensSaved,
      })),
      periodKey: "overview.trendHours" as TranslationKey,
    };
  }, [dashboard]);
  const trendMaximum = Math.max(1, ...savingsTrend.points.map((point) => point.savings));
  const rtkInstalled = tools.some((tool) => tool.id === "rtk" && tool.status !== "not_installed");
  const serenaInstalled = tools.some((tool) => tool.id === "serena" && tool.status !== "not_installed");

  return (
    <main className="community-app">
      <aside className="community-sidebar" aria-label={t("aria.communityNavigation")}>
        <div className="community-brand">
          <span className="community-brand__mark" aria-hidden="true">H</span>
          <div>
            <strong>{LOCAL_COMMUNITY_NAME}</strong>
            <span>{t("brand.tagline")}</span>
          </div>
        </div>

        <nav className="community-nav" aria-label={t("aria.sections")}>
          {navigationItems.map((item) => {
            const Icon = item.icon;
            const isActive = activeView === item.id;
            return (
              <button
                aria-current={isActive ? "page" : undefined}
                className={`community-nav__item${isActive ? " is-active" : ""}`}
                key={item.id}
                onClick={() => setActiveView(item.id)}
                type="button"
              >
                <Icon size={17} weight={isActive ? "fill" : "regular"} />
                <span>{t(item.labelKey)}</span>
              </button>
            );
          })}
        </nav>

        <RuntimeStatusIndicator
          className="community-sidebar__footer"
          runtime={runtime}
          label={t("brand.runsOnThisMac")}
        />
      </aside>

      <section className="community-main">
        <header className="community-header">
          <div>
            <p>{t("brand.localCommunityEdition")}</p>
            <h1>{t(activeNavigationItem?.labelKey ?? "nav.overview")}</h1>
          </div>
          <button
            className="community-icon-button"
            disabled={controlsDisabled}
            onClick={() => void refresh(true)}
            title={t("header.refresh")}
            type="button"
          >
            <ArrowClockwise size={18} weight="bold" />
            <span className="visually-hidden">{t("header.refresh")}</span>
          </button>
        </header>

        <p className="community-edition-note">{t("brand.editionNote")}</p>

        {loadError ? <p className="community-message community-message--error" role="alert">{loadError}</p> : null}
        {actionError ? <p className="community-message community-message--error" role="alert">{actionError}</p> : null}
        {notice ? <p className="community-message community-message--success" role="status">{notice}</p> : null}

        {activeView === "overview" ? (
          <div className="community-content">
            <section className={`community-proxy-card is-${currentProxy.tone}`}>
              <div>
                <span className="community-kicker">{t("proxy.localKicker")}</span>
                <h2>{currentProxy.title}</h2>
                <p>{currentProxy.detail}</p>
              </div>
              <div className="community-proxy-card__actions">
                {runtime?.running && !runtime.paused ? (
                  <button
                    className="community-button community-button--secondary"
                    disabled={controlsDisabled}
                    onClick={() => handleRuntime("pause_headroom")}
                    type="button"
                  >
                    {busyAction === "runtime:pause_headroom" ? t("proxy.pausing") : t("proxy.pause")}
                  </button>
                ) : (
                  <button
                    className="community-button"
                    disabled={controlsDisabled || runtime?.starting === true}
                    onClick={() => handleRuntime("start_headroom")}
                    type="button"
                  >
                    {busyAction === "runtime:start_headroom" || runtime?.starting ? t("proxy.starting") : t("proxy.start")}
                  </button>
                )}
                <button
                  className="community-button community-button--ghost"
                  disabled={controlsDisabled || runtime?.starting === true}
                  onClick={() => handleRuntime("force_restart_headroom")}
                  type="button"
                >
                  {busyAction === "runtime:force_restart_headroom" ? t("proxy.restarting") : t("proxy.restart")}
                </button>
              </div>
            </section>

            <section className="community-metrics" aria-label={t("aria.localActivity")}>
              <DashboardMetric
                detail={t("metrics.requestsDetail")}
                label={t("metrics.requestsLabel")}
                value={formatNumber(dashboard?.lifetimeRequests, resolvedLocale)}
              />
              <DashboardMetric
                detail={t("metrics.tokensDetail")}
                label={t("metrics.tokensLabel")}
                value={formatNumber(dashboard?.lifetimeEstimatedTokensSaved, resolvedLocale)}
              />
              <DashboardMetric
                detail={t("metrics.savingsDetail")}
                label={t("metrics.savingsLabel")}
                value={dashboard ? formatCurrency(dashboard.lifetimeEstimatedSavingsUsd, resolvedLocale) : "—"}
              />
            </section>

            <section className="community-panel community-trend" aria-label={t("aria.localTrend")}>
              <div className="community-trend__heading">
                <span className="community-kicker">{t("overview.trendKicker")}</span>
                <h2>{t("overview.trendHeading")}</h2>
                {savingsTrend.points.length ? (
                  <p>{t("overview.trendDescription", {
                    count: savingsTrend.points.length,
                    periods: t(savingsTrend.periodKey),
                  })}</p>
                ) : (
                  <p>{t("overview.trendEmpty")}</p>
                )}
              </div>
              {savingsTrend.points.length ? (
                <ol className="community-trend__bars">
                  {savingsTrend.points.map((point) => {
                    const amount = formatCurrency(point.savings, resolvedLocale);
                    const height = Math.max(10, Math.round((point.savings / trendMaximum) * 100));
                    return (
                      <li
                        aria-label={t("overview.trendPoint", { label: point.label, amount })}
                        className="community-trend__bar"
                        key={point.label}
                      >
                        <span className="community-trend__bar-fill" style={{ height: `${height}%` }} />
                        <span>{point.label}</span>
                      </li>
                    );
                  })}
                </ol>
              ) : null}
            </section>

            <section className="community-panel community-panel--overview">
              <div>
                <span className="community-kicker">{t("overview.connectionsKicker")}</span>
                <h2>{t("overview.heading")}</h2>
                <p>{t("overview.description")}</p>
              </div>
              <button className="community-button community-button--secondary" onClick={() => setActiveView("connections")} type="button">
                {t("overview.manage")}
              </button>
            </section>

            {loading && !dashboard ? <p className="community-loading">{t("overview.loading")}</p> : null}
          </div>
        ) : null}

        {activeView === "connections" ? (
          <div className="community-content">
            <section className="community-panel community-panel--title">
              <div>
                <span className="community-kicker">{t("connections.kicker")}</span>
                <h2>{t("connections.heading")}</h2>
                <p>{t("connections.description")}</p>
              </div>
            </section>
            <section className="community-list" aria-label={t("aria.clientConnectors")}>
              {connectors.length ? connectors.map((connector) => (
                <article className="community-connector" key={connector.clientId}>
                  <div className="community-connector__icon" aria-hidden="true">
                    <ConnectorIcon clientId={connector.clientId} size={19} />
                  </div>
                  <div className="community-connector__copy">
                    <h3>{connector.name}</h3>
                    <p>
                      {!connector.installed
                        ? t("connections.notInstalled")
                        : connector.enabled
                          ? connector.verification?.foreignProvider
                            ? t("connections.foreignProviderShort", {
                                provider: connector.verification.foreignProvider,
                              })
                            : connector.verified
                              ? t("connections.connectedVerified")
                              : t("connections.connectedPending")
                          : t("connections.available")}
                    </p>
                  </div>
                  <span className={`community-state${connector.enabled ? " is-ready" : ""}`}>
                    {connector.enabled ? t("connections.connected") : connector.installed ? t("connections.available") : t("connections.notDetected")}
                  </span>
                  <button
                    className="community-button community-button--secondary"
                    disabled={controlsDisabled || !connector.installed}
                    onClick={() => handleConnector(connector)}
                    type="button"
                  >
                    {busyAction === `connector:${connector.clientId}`
                      ? t("connections.working")
                      : connector.enabled
                        ? t("connections.disconnect")
                        : t("connections.connect")}
                  </button>
                </article>
              )) : (
                <p className="community-empty">{t("connections.empty")}</p>
              )}
            </section>
          </div>
        ) : null}

        {activeView === "tools" ? (
          <div className="community-content">
            <section className="community-panel community-panel--title">
              <div>
                <span className="community-kicker">{t("tools.kicker")}</span>
                <h2>{t("tools.heading")}</h2>
                <p>{t("tools.description")}</p>
              </div>
            </section>
          {tools.length ? TOOL_CATEGORY_ORDER.map((category) => {
            const categoryTools = groupedTools.get(category) ?? [];
            if (!categoryTools.length) return null;
            const categoryCopy = toolCategoryCopy[category];
            const workflowGroup = categoryTools.find((tool) => tool.workflowGroup)?.workflowGroup;
            const singleSelectLabel = workflowGroup
              ? workflowGroupCopy[workflowGroup]?.[resolvedLocale]
              : null;
 return <section className={`community-tool-group community-tool-group--${category}`} key={category} aria-labelledby={`tool-group-${category}`}>
              <div className="community-tool-group__heading">
                <h3 id={`tool-group-${category}`}>{categoryCopy.title[resolvedLocale]}</h3>
                <p>{categoryCopy.description[resolvedLocale]}</p>
                {singleSelectLabel ? <span className="tool-group__rule">{singleSelectLabel}</span> : null}
              </div>
              <div className="community-tool-grid" aria-label={categoryCopy.title[resolvedLocale]}>
            {categoryTools.map((tool) => {
                const actionKey = `tool:${tool.id}`;
                const removalKey = `tool:remove:${tool.id}`;
                const canToggle = !tool.unavailableReason && tool.status !== "installing";
                const supportedModes = tool.supportedModes?.filter(Boolean) ?? [];
                const selectedMode = selectedToolMode(tool, supportedModes);
                const descriptionKey = toolDescriptionKeys[tool.id];
                const description = descriptionKey ? t(descriptionKey) : tool.description;
                const canSetMode = supportedModes.length > 0
                  && tool.status !== "not_installed"
                  && tool.status !== "installing"
                  && tool.enabled
                  && !tool.unavailableReason
                  && !controlsDisabled;
                const modeHelpId = `tool-mode-help-${tool.id}`;
                const cavemanModeHelpId = `tool-mode-caveman-help-${tool.id}`;
                const primaryLabel = tool.status === "not_installed"
                  ? t("tools.install")
                  : tool.enabled
                    ? t("tools.disable")
                    : t("tools.enable");
                return (
                  <article className={`community-tool${supportedModes.length ? " community-tool--with-modes" : ""}`} key={tool.id}>
                    <div className="community-tool__topline">
                      <span className="community-tool__runtime">{tool.runtime === "node" ? "Node.js" : t(toolRuntimeKeys[tool.runtime]!)}</span>
                            {tool.workflowGroup ? <span className="community-state">{workflowGroupCopy[tool.workflowGroup]?.[resolvedLocale] ?? tool.workflowGroup}</span> : null}
                      <span className={`community-state${tool.status === "healthy" ? " is-ready" : ""}`}>{toolStatusLabel(tool, t)}</span>
                    </div>
                    <h3>{tool.name}</h3>
              <p>{tool.unavailableReason ?? toolCopy[tool.id]?.[resolvedLocale] ?? description}</p>
                      <p className="community-tool__activation-note">
                        {getActivationScopeCopy(tool.activationScope)[resolvedLocale]}
                      </p>
                    {supportedModes.length ? (
                      <fieldset className="community-tool__modes">
                        <legend>{t("tools.defaultMode.title")}</legend>
                        <p className="community-tool__mode-help" id={modeHelpId}>{t("tools.defaultMode.help")}</p>
                        {tool.id === "caveman" ? (
                          <p className="community-tool__mode-help" id={cavemanModeHelpId}>{t("tools.defaultMode.cavemanHelp")}</p>
                        ) : null}
                        <div
                          aria-busy={busyAction?.startsWith(`tool:mode:${tool.id}:`) || undefined}
                          aria-describedby={tool.id === "caveman" ? `${modeHelpId} ${cavemanModeHelpId}` : modeHelpId}
                          aria-label={t("aria.defaultToolMode", { name: tool.name })}
                          className="community-tool__mode-options"
                          role="radiogroup"
                        >
                          {supportedModes.map((mode) => {
                            const modeActionKey = `tool:mode:${tool.id}:${mode}`;
                            return (
                              <label className="community-tool__mode-option" key={mode}>
                                <input
                                  checked={selectedMode === mode}
                                  disabled={!canSetMode}
                                  name={`tool-mode-${tool.id}`}
                                  onChange={() => handleToolMode(tool, mode)}
                                  type="radio"
                                  value={mode}
                                />
                                <span>{toolModeLabel(mode, t)}</span>
                                {busyAction === modeActionKey ? <span className="visually-hidden">{t("tools.savingMode")}</span> : null}
                              </label>
                            );
                          })}
                        </div>
                      </fieldset>
                    ) : null}
                    <div className="community-tool__footer">
                            <a href={tool.sourceUrl} target="_blank" rel="noreferrer" className="community-text-button">{sourceLinkCopy[resolvedLocale]}</a>
                      <button
                        className="community-button community-button--secondary"
                        disabled={controlsDisabled || !canToggle}
                        onClick={() => handleTool(tool)}
                        type="button"
                      >
                        {busyAction === actionKey ? t("tools.working") : primaryLabel}
                      </button>
                      {!tool.required && tool.status !== "not_installed" ? (
                        <button
                          className="community-text-button"
                          disabled={controlsDisabled}
                          onClick={() => handleToolRemoval(tool)}
                          type="button"
                        >
                          {busyAction === removalKey ? t("tools.removing") : t("tools.remove")}
                        </button>
                      ) : null}
                    </div>
                  </article>
                );
            })}
              </div>
            </section>;
          }) : (
                <p className="community-empty">{t("tools.empty")}</p>
              )}
          <section className="community-workflow-guide" aria-labelledby="workflow-guide-heading">
                <div className="community-tool-group__heading">
                  <h3 id="workflow-guide-heading">{conflictHeadingCopy[resolvedLocale]}</h3>
                  <p>{conflictMatrixCopy[resolvedLocale]}</p>
                </div>
              </section>
          </div>
        ) : null}

        {activeView === "activity" ? (
          <div className="community-content">
            <section className="community-panel community-panel--title">
              <div>
                <span className="community-kicker">{t("activity.kicker")}</span>
                <h2>{t("activity.heading")}</h2>
                <p>{t("activity.description")}</p>
              </div>
              <button
                className="community-button community-button--secondary"
                disabled={controlsDisabled}
                onClick={() => void refreshActivity()}
                type="button"
              >
                {t("activity.refresh")}
              </button>
            </section>
            <section className="community-activity-feed" aria-label={t("aria.activityFeed")}>
              <ActivityFeed
                error={activityError}
                feed={activityFeed}
                loaded={activityLoaded}
              onNavigateToOptimize={(projectPath) => {
                if (projectPath === "codex") {
                  setLearnAgent("codex");
                } else if (projectPath) {
                  setLearnAgent("claude");
                  setSelectedClaudeProjectPath(projectPath);
                }
                setActiveView("optimize");
              }}
                rtkInstalled={rtkInstalled}
                serenaInstalled={serenaInstalled}
              />
            </section>
          </div>
        ) : null}

        {activeView === "optimize" ? (
          <div className="community-content">
            <section className="community-panel community-panel--title">
              <div>
                <span className="community-kicker">{t("learn.kicker")}</span>
                <h2>{t("learn.heading")}</h2>
                <p>{t("learn.description")}</p>
              </div>
              <button
                className="community-button community-button--secondary"
                disabled={controlsDisabled || learnResourcesLoading}
                onClick={() => void refreshLearnResources()}
                type="button"
              >
                {learnResourcesLoading ? t("learn.refreshing") : t("learn.refresh")}
              </button>
            </section>

            {learnError ? <p className="community-message community-message--error" role="alert">{learnError}</p> : null}

            <section className="community-learn-card" aria-label={t("aria.learnTargets")}>
              <div className="community-learn-card__fields">
                <label className="community-field" htmlFor="community-learn-agent">
                  <span>{t("learn.agentLabel")}</span>
                  <select
                    id="community-learn-agent"
                    onChange={(event) => setLearnAgent(event.target.value as LearnAgent)}
                    value={learnAgent}
                  >
                    {learnAgents.map((agent) => (
                      <option key={agent.id} value={agent.id}>{t(agent.labelKey)}</option>
                    ))}
                  </select>
                </label>
                {learnAgent === "claude" ? (
                  <label className="community-field" htmlFor="community-learn-project">
                    <span>{t("learn.projectLabel")}</span>
                    <select
                      disabled={learnResourcesLoading || claudeProjects.length === 0}
                      id="community-learn-project"
                      onChange={(event) => setSelectedClaudeProjectPath(event.target.value || null)}
                      value={selectedClaudeProjectPath ?? ""}
                    >
                      {claudeProjects.length ? claudeProjects.map((project) => (
                        <option key={project.id} value={project.projectPath}>{project.displayName}</option>
                      )) : <option value="">{t("learn.noProjects")}</option>}
                    </select>
                  </label>
                ) : null}
              </div>

              <p className="community-learn-card__target-description">{t(selectedLearnAgent.descriptionKey)}</p>
              {selectedClaudeProject ? (
                <p className="community-learn-card__project-summary">
                  {t("learn.projectSummary", {
                    sessions: selectedClaudeProject.sessionCount,
                    days: selectedClaudeProject.activeDaysSinceLastLearn,
                  })}
                </p>
              ) : null}

              <div className="community-learn-card__actions">
                <div>
                  <span className={`community-state${selectedLearnIssue ? "" : " is-ready"}`}>
                    {selectedLearnIssue ? t("learn.notReady") : t("learn.ready")}
                  </span>
                  <p>{selectedLearnIssue ?? t("learn.readyDescription")}</p>
                </div>
                <button
                  className="community-button"
                  disabled={controlsDisabled || Boolean(selectedLearnIssue)}
                  onClick={handleStartLearn}
                  type="button"
                >
                  {busyAction === `learn:${learnAgent}` ? t("learn.starting") : t("learn.start")}
                </button>
              </div>
            </section>

            <section className="community-learn-status" aria-live="polite">
              <div>
                <span className="community-kicker">{t("learn.statusKicker")}</span>
                <h3>{t("learn.statusHeading")}</h3>
                {learnStatus?.running ? (
                  <LearnScanStatusLine elapsedSeconds={learnStatus.elapsedSeconds} step={learnStatus.currentStep} />
                ) : (
                  <p>{learnStatus?.error ?? learnStatus?.summary ?? t("learn.idle")}</p>
                )}
              </div>
              {learnStatus?.outputTail?.length ? (
                <pre className="community-learn-status__output">{learnStatus.outputTail.slice(-4).join("\n")}</pre>
              ) : null}
            </section>

            <section className="community-auto-learn">
              <div>
                <h3>{t("learn.autoTitle")}</h3>
                <p>{t("learn.autoDescription")}</p>
              </div>
              <label className="community-toggle">
                <input
                  aria-label={t("learn.autoTitle")}
                  checked={autoLearnEnabled ?? false}
                  disabled={controlsDisabled || autoLearnEnabled === null}
                  onChange={(event) => handleAutoLearnToggle(event.target.checked)}
                  type="checkbox"
                />
                <span>{autoLearnEnabled ? t("learn.autoOn") : t("learn.autoOff")}</span>
              </label>
            </section>

            {learnAgent === "claude" && selectedClaudeProject ? (
              <section className="community-learn-applied">
                <div>
                  <span className="community-kicker">{t("learn.appliedKicker")}</span>
                  <h3>{t("learn.appliedHeading")}</h3>
                  <p>{t("learn.appliedDescription")}</p>
                </div>
                <OptimizePanel
                  neverScanned={!selectedClaudeProject.lastLearnRanAt && !completedLearnForTarget}
                  projectPath={selectedClaudeProject.projectPath}
                  refreshSignal={learnStatus?.finishedAt ? Date.parse(learnStatus.finishedAt) : 0}
                />
              </section>
            ) : null}
            {learnAgent === "codex" ? (
              <section className="community-learn-applied">
                <div>
                  <span className="community-kicker">{t("learn.appliedKicker")}</span>
                  <h3>{t("learn.appliedHeading")}</h3>
                  <p>{t("learn.appliedDescription")}</p>
                </div>
                <OptimizePanel
                  neverScanned={!(learnStatus?.lastRunAt && learnStatusMatchesTarget) && !completedLearnForTarget}
                  projectPath="codex"
                  refreshSignal={learnStatus?.finishedAt ? Date.parse(learnStatus.finishedAt) : 0}
                  source="codex"
                />
              </section>
            ) : null}
          </div>
        ) : null}

        {activeView === "diagnostics" ? (
          <div className="community-content">
            <section className="community-panel community-panel--title">
              <div>
                <span className="community-kicker">{t("diagnostics.kicker")}</span>
                <h2>{t("diagnostics.heading")}</h2>
                <p>{t("diagnostics.description")}</p>
              </div>
            </section>

            <section className="community-diagnostics-grid" aria-label={t("aria.diagnostics")}>
              <DiagnosticStatus
                detail={!runtime ? t("diagnostics.checking") : runtime.running ? t("diagnostics.runtimeRunning") : t("diagnostics.runtimeStopped")}
                label={t("diagnostics.runtime")}
                tone={!runtime ? "idle" : runtime.running ? "ready" : "attention"}
              />
              <DiagnosticStatus
                detail={!runtime ? t("diagnostics.checking") : runtime.proxyReachable ? t("diagnostics.proxyOnline") : t("diagnostics.proxyOffline")}
                label={t("diagnostics.proxy")}
                tone={!runtime ? "idle" : runtime.proxyReachable ? "ready" : "attention"}
              />
              <DiagnosticStatus
                detail={!runtime
                  ? t("diagnostics.checking")
                  : runtime.mcpError
                    ? runtime.mcpError
                    : runtime.mcpConfigured
                      ? t("diagnostics.mcpReady")
                      : t("diagnostics.mcpNotConfigured")}
                label={t("diagnostics.mcp")}
                tone={!runtime ? "idle" : runtime.mcpError ? "attention" : runtime.mcpConfigured ? "ready" : "idle"}
              />
              <DiagnosticStatus
                detail={!runtime
                  ? t("diagnostics.checking")
                  : runtime.kompressEnabled === null || runtime.kompressEnabled === undefined
                    ? t("diagnostics.checking")
                    : runtime.kompressEnabled
                      ? t("diagnostics.kompressEnabled")
                      : t("diagnostics.kompressDisabled")}
                label={t("diagnostics.kompress")}
                tone={!runtime ? "idle" : runtime.kompressEnabled ? "ready" : "idle"}
              />
            </section>

            <section className="community-log-panel">
              <div className="community-log-panel__head">
                <div>
                  <span className="community-kicker">{t("diagnostics.logsKicker")}</span>
                  <h3>{t("diagnostics.logsHeading")}</h3>
                  <p>{t("diagnostics.logsDescription")}</p>
                </div>
                <button
                  className="community-button community-button--secondary"
                  disabled={controlsDisabled}
                  onClick={() => void refreshLogs()}
                  type="button"
                >
                  {t("diagnostics.refreshLogs")}
                </button>
              </div>
              {logsError ? <p className="community-message community-message--error" role="alert">{logsError}</p> : null}
              {!logsLoaded ? <p className="community-loading">{t("diagnostics.logsLoading")}</p> : null}
              {logsLoaded && !logsError && !logs.length ? <p className="community-empty">{t("diagnostics.logsEmpty")}</p> : null}
              {logs.length ? <pre className="community-log-panel__content">{logs.join("\n")}</pre> : null}
            </section>

            <section className="community-settings-card community-settings-card--quit">
              <div>
                <h3>{t("diagnostics.quitTitle")}</h3>
                <p>{t("diagnostics.quitDescription")}</p>
              </div>
              <button
                className="community-button community-button--secondary"
                disabled={controlsDisabled}
                onClick={handleQuit}
                type="button"
              >
                {busyAction === "quit" ? t("diagnostics.quitting") : t("diagnostics.quit")}
              </button>
            </section>
          </div>
        ) : null}

        {activeView === "settings" ? (
          <div className="community-content">
            <section className="community-panel community-panel--title">
              <div>
                <span className="community-kicker">{t("settings.kicker")}</span>
                <h2>{t("settings.heading")}</h2>
                <p>{t("settings.description")}</p>
              </div>
            </section>
            <section className="community-settings-card">
              <dl>
                <div>
                  <dt>{t("settings.edition")}</dt>
                  <dd>{LOCAL_COMMUNITY_NAME}</dd>
                </div>
                <div>
                  <dt>{t("settings.version")}</dt>
                  <dd>{dashboard?.appVersion ?? t("settings.loading")}</dd>
                </div>
                <div>
                  <dt>{t("settings.runtime")}</dt>
                  <dd>{runtime?.platform ?? t("settings.checking")}</dd>
                </div>
                <div>
                  <dt>
                    <label htmlFor="community-language">{t("settings.language")}</label>
                  </dt>
                  <dd>
                    <select
                      aria-label={t("settings.language")}
                      className="community-language-select"
                      id="community-language"
                      onChange={(event) => setLocale(event.target.value as Locale)}
                      value={locale}
                    >
                      {localeOptions.map((option) => (
                        <option key={option.value} value={option.value}>{t(option.labelKey)}</option>
                      ))}
                    </select>
                  </dd>
                </div>
                <div>
                  <dt>{t("settings.autostart")}</dt>
                  <dd>
                    <label className="community-toggle">
                      <input
                        aria-label={t("settings.autostart")}
                        checked={autostartEnabled ?? false}
                        disabled={controlsDisabled || autostartEnabled === null}
                        onChange={(event) => handleAutostartToggle(event.target.checked)}
                        type="checkbox"
                      />
                      <span>{autostartEnabled ? t("settings.autostartOn") : t("settings.autostartOff")}</span>
                    </label>
                  </dd>
                </div>
              </dl>
              <div className="community-settings-card__local-action">
                <div>
                  <h3>{t("settings.localNotification")}</h3>
                  <p>{t("settings.localNotificationDescription")}</p>
                </div>
                <button
                  className="community-button community-button--secondary"
                  disabled={controlsDisabled}
                  onClick={handleLocalNotification}
                  type="button"
                >
                  {busyAction === "local-notification" ? t("settings.sendingLocalNotification") : t("settings.sendLocalNotification")}
                </button>
              </div>
              <div className="community-settings-card__danger">
                <div>
                  <h3>{t("settings.removeTitle")}</h3>
                  <p>{t("settings.removeDescription")}</p>
                </div>
                <button
                  className="community-button community-button--danger"
                  disabled={controlsDisabled}
                  onClick={handleUninstall}
                  type="button"
                >
                  {busyAction === "uninstall" ? t("settings.removing") : t("settings.removeAndQuit")}
                </button>
              </div>
            </section>
          </div>
        ) : null}
      </section>
    </main>
  );
}
