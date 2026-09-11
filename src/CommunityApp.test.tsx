import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import { CommunityApp } from "./CommunityApp";
import { I18nProvider, LOCALE_STORAGE_KEY, resolveSystemLocale } from "./lib/i18n";
import { LOCAL_COMMUNITY_NAME } from "./lib/localEdition";
import { mockDashboard } from "./lib/mockData";
import type { AppliedPatterns, ClientConnectorStatus, DashboardState, HeadroomLearnStatus, RuntimeStatus } from "./lib/types";

let connectorEnabled = false;
let connectorForeignProvider: string | null = null;
let rtkInstalled = false;
let rtkEnabled = false;
let cavemanInstalled = true;
let cavemanEnabled = true;
let cavemanStatus: "not_installed" | "installing" | "healthy" | "degraded" = "healthy";
let cavemanUnavailableReason: string | null = null;
let cavemanMode = "wenyan-full";
let ponytailInstalled = true;
let ponytailEnabled = true;
let ponytailMode = "ultra";
// Single-select workflow peers start uninstalled so the other tool tests keep
// their install-first flow; the switch tests opt them in explicitly.
let openspecInstalled = false;
let openspecEnabled = false;
let superpowersInstalled = false;
let superpowersEnabled = false;
let autoLearnEnabled = true;
let autostartEnabled = false;
let claudeCliAvailable = true;
let codexCliAvailable = true;
let codexLoggedIn = true;
let exposeClaudeProjects = true;
let codexLastRunAt: string | null = null;
let appliedPatterns: AppliedPatterns = {
  claudeMd: [],
  memoryMd: [],
  codexAgentsMd: [],
  codexInstructionsMd: [],
};
let learnStatus: HeadroomLearnStatus = {
  running: false,
  projectPath: "/Users/example/project",
  projectDisplayName: "Example project",
  progressPercent: 0,
  summary: "No local scan is running.",
  outputTail: [] as string[],
};

const activityFeed = {
  proxyReachable: true,
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

const claudeProjects = [
  {
    id: "example-project",
    projectPath: "/Users/example/project",
    displayName: "Example project",
    lastWorkedAt: "2026-08-26T08:00:00Z",
    sessionCount: 8,
    lastLearnRanAt: null,
    hasPersistedLearnings: false,
    activeDaysSinceLastLearn: 3,
    lastLearnPatternCount: null,
  },
];

const runtime: RuntimeStatus = {
  platform: "macOS",
  supportTier: "native",
  installed: true,
  running: true,
  starting: false,
  paused: false,
  autoPaused: false,
  bypassed: false,
  proxyReachable: true,
  headroomLearnSupported: true,
  mcpConfigured: true,
  mcpError: null,
  kompressEnabled: true,
  rtk: {
    installed: true,
    enabled: true,
    pathConfigured: true,
    hookConfigured: true,
  },
};

// Built per read so the workflow flags below can change between tests.
const additionalTools = (): DashboardState["tools"] => [
  { id: "stop-that-shit", name: "Stop That Shit", description: "Guarded scope checks.", runtime: "plugin", required: false, enabled: false, status: "not_installed", sourceUrl: "https://example.invalid/stop-that-shit", version: "latest", category: "guardrails", activationScope: "new_session" },
  { id: "agent-guard", name: "Agent Guard", description: "Local secret guard.", runtime: "plugin", required: false, enabled: false, status: "not_installed", sourceUrl: "https://example.invalid/agent-guard", version: "latest", category: "guardrails", activationScope: "new_session" },
  { id: "grill-me", name: "Grill Me", description: "Read-only understanding check.", runtime: "plugin", required: false, enabled: false, status: "not_installed", sourceUrl: "https://example.invalid/grill-me", version: "latest", category: "learning", activationScope: "new_session" },
  { id: "openspec", name: "OpenSpec", description: "Specification workflow.", runtime: "plugin", required: false, enabled: openspecEnabled, status: openspecInstalled ? "healthy" : "not_installed", sourceUrl: "https://github.com/Fission-AI/OpenSpec", version: "latest", category: "workflow", workflowGroup: "primary_workflow", activationScope: "new_session" },
  { id: "superpowers", name: "Superpowers", description: "Disciplined coding workflow.", runtime: "plugin", required: false, enabled: superpowersEnabled, status: superpowersInstalled ? "healthy" : "not_installed", sourceUrl: "https://github.com/obra/superpowers", version: "latest", category: "workflow", workflowGroup: "primary_workflow", activationScope: "new_session" },
  { id: "gstack", name: "gstack", description: "Product to ship workflow.", runtime: "plugin", required: false, enabled: false, status: "not_installed", sourceUrl: "https://github.com/garrytan/gstack", version: "latest", category: "workflow", workflowGroup: "primary_workflow", activationScope: "new_session" },
  { id: "ralph-loop", name: "Ralph Loop", description: "Bounded automation loop.", runtime: "plugin", required: false, enabled: false, status: "not_installed", sourceUrl: "https://github.com/SantanderAI/ralph", version: "latest", category: "automation", workflowGroup: "execution_engine", activationScope: "new_session" },
];

function dashboardState(): DashboardState {
  return {
    ...mockDashboard,
    appVersion: "0.8.7",
    bootstrapComplete: true,
    lifetimeRequests: 42,
    lifetimeEstimatedTokensSaved: 18_500,
    lifetimeEstimatedSavingsUsd: 3.5,
    tools: [
      ...additionalTools(),
      {
        id: "rtk",
        name: "RTK",
        description: "Compact local command output.",
        runtime: "binary",
        required: false,
        enabled: rtkEnabled,
        status: rtkInstalled ? "healthy" : "not_installed",
        sourceUrl: "https://example.invalid/rtk",
        version: "1.0.0",
        activationScope: "immediate",
      },
      {
        id: "caveman",
        name: "Caveman",
        description: "Local prompt compression modes.",
        runtime: "plugin",
        required: false,
        enabled: cavemanEnabled,
        status: cavemanInstalled ? cavemanStatus : "not_installed",
        sourceUrl: "https://example.invalid/caveman",
        version: "1.0.0",
        activationScope: "new_session",
        defaultMode: cavemanMode,
        supportedModes: ["lite", "full", "ultra", "wenyan-lite", "wenyan-full", "wenyan-ultra"],
        unavailableReason: cavemanUnavailableReason,
      },
      {
        id: "ponytail",
        name: "Ponytail",
        description: "Local output minimization modes.",
        runtime: "plugin",
        required: false,
        enabled: ponytailEnabled,
        status: ponytailInstalled ? "healthy" : "not_installed",
        sourceUrl: "https://example.invalid/ponytail",
        version: "1.0.0",
        activationScope: "new_session",
        defaultMode: ponytailMode,
        supportedModes: ["lite", "full", "ultra"],
      },
      {
        id: "allinluna",
        name: "All in Luna",
        description: "Codex multi-agent orchestration.",
        runtime: "plugin",
        required: false,
        enabled: false,
        status: "not_installed",
      sourceUrl: "https://github.com/zenx0x/allinluna",
      version: "latest",
      category: "automation",
      workflowGroup: "execution_engine",
      activationScope: "new_session",
    },
    ],
  };
}

function connectorsState(): ClientConnectorStatus[] {
  return [
    {
      clientId: "codex",
      name: "Codex",
      installed: true,
      enabled: connectorEnabled,
      verified: connectorEnabled && !connectorForeignProvider,
      // The backend reads the Codex route owner out of `~/.codex/config.toml`,
      // so it reports it while the connector is OFF too -- which is exactly
      // when the enable toggle needs it. `verification` still only exists for
      // an enabled connector.
      foreignProvider: connectorForeignProvider,
      verification: connectorEnabled
        ? {
            clientId: "codex",
            verified: !connectorForeignProvider,
            proxyReachable: true,
            checks: [],
            failures: [],
            foreignProvider: connectorForeignProvider,
          }
        : undefined,
    },
  ];
}

function renderCommunityApp() {
  return render(
    <I18nProvider>
      <CommunityApp />
    </I18nProvider>,
  );
}

function setNavigatorLanguages(languages: string[]) {
  Object.defineProperty(navigator, "languages", {
    configurable: true,
    value: languages,
  });
  Object.defineProperty(navigator, "language", {
    configurable: true,
    value: languages[0] ?? "en-US",
  });
}

beforeEach(() => {
  localStorage.removeItem(LOCALE_STORAGE_KEY);
  document.documentElement.lang = "";
  setNavigatorLanguages(["en-US"]);
  connectorEnabled = false;
  connectorForeignProvider = null;
  rtkInstalled = false;
  rtkEnabled = false;
  cavemanInstalled = true;
  cavemanEnabled = true;
  cavemanStatus = "healthy";
  cavemanUnavailableReason = null;
  cavemanMode = "wenyan-full";
  ponytailInstalled = true;
  ponytailEnabled = true;
  ponytailMode = "ultra";
  openspecInstalled = false;
  openspecEnabled = false;
  superpowersInstalled = false;
  superpowersEnabled = false;
  autoLearnEnabled = true;
  autostartEnabled = false;
  claudeCliAvailable = true;
  codexCliAvailable = true;
  codexLoggedIn = true;
  exposeClaudeProjects = true;
  codexLastRunAt = null;
  appliedPatterns = {
    claudeMd: [],
    memoryMd: [],
    codexAgentsMd: [],
    codexInstructionsMd: [],
  };
  runtime.headroomLearnSupported = true;
  runtime.headroomLearnDisabledReason = null;
  learnStatus = {
    running: false,
    projectPath: "/Users/example/project",
    projectDisplayName: "Example project",
    progressPercent: 0,
    summary: "No local scan is running.",
    outputTail: [],
  };
  invokeMock.mockReset();
  invokeMock.mockImplementation((command: string, args?: unknown) => {
    switch (command) {
      case "get_dashboard_state":
        return Promise.resolve(dashboardState());
      case "get_runtime_status":
        return Promise.resolve(runtime);
      case "get_client_connectors":
        return Promise.resolve(connectorsState());
      case "get_activity_feed":
        return Promise.resolve(activityFeed);
      case "get_claude_code_projects":
        return Promise.resolve(exposeClaudeProjects ? claudeProjects : []);
      case "get_headroom_learn_prereq_status":
        return Promise.resolve({
          claudeCliAvailable,
          claudeCliPath: claudeCliAvailable ? "/usr/local/bin/claude" : null,
          codexCliAvailable,
          codexCliPath: codexCliAvailable ? "/usr/local/bin/codex" : null,
          codexLoggedIn,
        });
      case "get_headroom_learn_status":
        return Promise.resolve((args as { projectPath?: string } | undefined)?.projectPath === "codex"
          ? { ...learnStatus, projectPath: "codex", lastRunAt: codexLastRunAt }
          : learnStatus);
      case "list_applied_patterns":
        return Promise.resolve(appliedPatterns);
      case "start_headroom_learn":
        learnStatus = {
          ...learnStatus,
          running: true,
          progressPercent: 8,
          summary: "Local scan is running.",
          currentStep: "Collecting local sessions",
        };
        return Promise.resolve();
      case "get_auto_learn_enabled":
        return Promise.resolve(autoLearnEnabled);
      case "set_auto_learn_enabled":
        autoLearnEnabled = Boolean((args as { enabled?: boolean } | undefined)?.enabled);
        return Promise.resolve(autoLearnEnabled);
      case "get_headroom_logs":
        return Promise.resolve(["runtime online", "proxy listening on 127.0.0.1:6867"]);
      case "get_autostart_enabled":
        return Promise.resolve(autostartEnabled);
      case "set_autostart_enabled":
        autostartEnabled = Boolean((args as { enabled?: boolean } | undefined)?.enabled);
        return Promise.resolve(autostartEnabled);
      case "show_notification":
      case "quit_headroom":
        return Promise.resolve();
      case "apply_client_setup":
        connectorEnabled = true;
        return Promise.resolve({
          clientId: "codex",
          applied: true,
          alreadyConfigured: false,
          summary: "Codex is connected locally.",
          changedFiles: [],
          backupFiles: [],
          nextSteps: [],
          verification: {
            clientId: "codex",
            verified: true,
            proxyReachable: true,
            checks: [],
            failures: [],
          },
        });
      case "disable_client_setup":
        connectorEnabled = false;
        return Promise.resolve();
      case "install_addon":
        rtkInstalled = true;
        rtkEnabled = true;
        return Promise.resolve(dashboardState());
      case "set_addon_enabled":
        rtkEnabled = !rtkEnabled;
        return Promise.resolve(dashboardState());
      case "uninstall_addon":
        rtkInstalled = false;
        rtkEnabled = false;
        return Promise.resolve(dashboardState());
      case "set_addon_mode": {
        const { id, mode } = (args ?? {}) as { id?: string; mode?: string };
        if (id === "caveman" && mode) cavemanMode = mode;
        if (id === "ponytail" && mode) ponytailMode = mode;
        return Promise.resolve(dashboardState());
      }
      case "start_headroom":
      case "pause_headroom":
      case "force_restart_headroom":
        return Promise.resolve();
      default:
        return Promise.resolve();
    }
  });
});

describe("CommunityApp", () => {
  it("resolves supported system languages and falls back to English", () => {
    expect(resolveSystemLocale(["zh-CN"])).toBe("zh-CN");
    expect(resolveSystemLocale(["zh-SG"])).toBe("zh-CN");
    expect(resolveSystemLocale(["zh"])).toBe("zh-CN");
    expect(resolveSystemLocale(["zh-Hant-HK"])).toBe("zh-TW");
    expect(resolveSystemLocale(["zh-TW"])).toBe("zh-TW");
    expect(resolveSystemLocale(["ja-JP"])).toBe("ja");
    expect(resolveSystemLocale(["ko-KR"])).toBe("ko");
    expect(resolveSystemLocale(["fr-FR", "zh-CN"])).toBe("zh-CN");
    expect(resolveSystemLocale(["fr-FR"])).toBe("en");
  });

  it("renders Community branding, live local proxy state, and local activity", async () => {
    renderCommunityApp();

    expect(await screen.findByText("Proxy online")).toBeInTheDocument();
    expect(screen.getByText(LOCAL_COMMUNITY_NAME)).toBeInTheDocument();
    expect(screen.getByText("Requests processed")).toBeInTheDocument();
    expect(screen.getByText("Tokens saved")).toBeInTheDocument();
    expect(screen.getByText("42")).toBeInTheDocument();
    expect(screen.queryByText(/Sign in|Upgrade|Pricing|Subscription/i)).not.toBeInTheDocument();
  });

  it("connects and disconnects a local client connector", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Connections" }));
    await user.click(await screen.findByRole("button", { name: "Connect" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("apply_client_setup", {
        clientId: "codex",
        allowTakeover: false,
      });
    });

    await user.click(await screen.findByRole("button", { name: "Disconnect" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("disable_client_setup", { clientId: "codex" });
    });
  });

  it("names the tool that owns the Codex route instead of pending verification", async () => {
    // A connector that is on while another tool owns the route must not read as
    // "verification pending", which implies Headroom is mid-setup. The card
    // names the tool that is actually routing.
    connectorEnabled = true;
    connectorForeignProvider = "Cockpit (codex_local_access)";
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Connections" }));
    await screen.findByText("Connected; routing is handled by Cockpit (codex_local_access)");
    expect(screen.queryByText("Connected; verification pending")).toBeNull();
  });

  it("asks for confirmation before taking over a Codex route owned by another tool", async () => {
    connectorForeignProvider = "codex_local_access";
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Connections" }));
    await user.click(await screen.findByRole("button", { name: "Connect" }));

    // The prompt is an in-app dialog, not `window.confirm`: WebKit answers the
    // native call with "cancel" when the UI delegate does not implement the
    // panel (wry's does not), which silently swallowed every enable.
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("codex_local_access");
    await user.click(within(dialog).getByRole("button", { name: "Continue" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("apply_client_setup", {
        clientId: "codex",
        allowTakeover: true,
      });
    });
  });

  it("leaves another tool's Codex route untouched when the takeover is declined", async () => {
    connectorForeignProvider = "codex_local_access";
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Connections" }));
    await user.click(await screen.findByRole("button", { name: "Connect" }));

    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));

    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(invokeMock).not.toHaveBeenCalledWith("apply_client_setup", {
      clientId: "codex",
      allowTakeover: true,
    });
  });

  it("names the tools view so assistive tech announces the group", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));

    const groups = await screen.findAllByRole("group", { name: "Local tools and add-ons" });
    expect(groups.length).toBeGreaterThan(0);
  });

  it("installs and toggles a local tool", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));
    const rtkCard = screen.getByText("RTK").closest("article");
    expect(rtkCard).not.toBeNull();
    await user.click(await within(rtkCard as HTMLElement).findByRole("button", { name: "Install" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("install_addon", { id: "rtk" });
    });

    await user.click(await within(rtkCard as HTMLElement).findByRole("button", { name: "Disable" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_addon_enabled", { id: "rtk", enabled: false });
    });
  });

  it("refreshes dashboard state after a successful install", async () => {
    const user = userEvent.setup();
    renderCommunityApp();
    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));

    const rtkCard = screen.getByText("RTK").closest("article");
    expect(rtkCard).not.toBeNull();
    const dashboardReadsBeforeAction = invokeMock.mock.calls.filter(
      ([command]) => command === "get_dashboard_state",
    ).length;
    await user.click(await within(rtkCard as HTMLElement).findByRole("button", { name: "Install" }));

    await waitFor(() => {
      expect(within(rtkCard as HTMLElement).getByRole("button", { name: "Disable" })).toBeInTheDocument();
      const dashboardReadsAfterAction = invokeMock.mock.calls.filter(
        ([command]) => command === "get_dashboard_state",
      ).length;
      expect(dashboardReadsAfterAction).toBeGreaterThan(dashboardReadsBeforeAction);
    });
  });

  it("refreshes dashboard state after a failed install", async () => {
    const user = userEvent.setup();
    renderCommunityApp();
    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));

    const rtkCard = screen.getByText("RTK").closest("article");
    expect(rtkCard).not.toBeNull();
    const dashboardReadsBeforeAction = invokeMock.mock.calls.filter(
      ([command]) => command === "get_dashboard_state",
    ).length;
    const defaultInvoke = invokeMock.getMockImplementation();
    expect(defaultInvoke).toBeDefined();
    invokeMock.mockImplementation((command: string, args?: unknown) => {
      if (command === "install_addon") return Promise.reject(new Error("simulated install failure"));
      return defaultInvoke?.(command, args);
    });
    await user.click(await within(rtkCard as HTMLElement).findByRole("button", { name: "Install" }));

    await waitFor(() => {
      const dashboardReadsAfterAction = invokeMock.mock.calls.filter(
        ([command]) => command === "get_dashboard_state",
      ).length;
      expect(dashboardReadsAfterAction).toBeGreaterThan(dashboardReadsBeforeAction);
    });
    expect(within(rtkCard as HTMLElement).getByRole("button", { name: "Install" })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("simulated install failure");
  });

  it("offers All in Luna as a Codex-only plugin", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));
    const card = (await screen.findByText("All in Luna", { selector: "h3" })).closest(
      ".community-tool"
    );
    expect(card).not.toBeNull();
    expect(
      within(card as HTMLElement).getByText(/Installing registers the All in Luna marketplace/)
    ).toBeInTheDocument();
    expect(within(card as HTMLElement).queryByRole("radiogroup")).not.toBeInTheDocument();

    await user.click(within(card as HTMLElement).getByRole("button", { name: "Install" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("install_addon", { id: "allinluna" });
    });
  });

  it("groups tools, exposes install controls, and documents session activation", async () => {
    const user = userEvent.setup();
    renderCommunityApp();
    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));
    expect(screen.getByRole("heading", { name: "Safety constraints" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Learning" })).toBeInTheDocument();
    const workflowHeading = screen.getByRole("heading", { name: "Primary workflow" });
    const automationHeading = screen.getByRole("heading", { name: "Automation executor" });
    expect(workflowHeading).toBeInTheDocument();
    expect(automationHeading).toBeInTheDocument();
    expect(within(workflowHeading.parentElement as HTMLElement).getByText("Only one can be enabled · Primary workflow")).toBeInTheDocument();
    expect(within(automationHeading.parentElement as HTMLElement).getByText("Only one can be enabled · Automation executor")).toBeInTheDocument();
    expect(screen.getAllByText(/Takes effect immediately/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/Takes effect in a new Codex session/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/no Headroom restart is needed/).length).toBeGreaterThan(0);
    for (const name of ["Stop That Shit", "Agent Guard", "Grill Me", "OpenSpec", "Superpowers", "gstack", "Ralph Loop"]) {
      const card = screen.getByText(name, { selector: "h3" }).closest("article");
      expect(card).not.toBeNull();
      expect(within(card as HTMLElement).getByRole("button", { name: "Install" })).toBeInTheDocument();
      expect(within(card as HTMLElement).getByRole("link", { name: "Source and acknowledgements" })).toBeInTheDocument();
    }
    expect(screen.getByRole("heading", { name: "Conflict and switching rules" })).toBeInTheDocument();
    expect(screen.getByText(/Headroom and port 6867 stay running/)).toBeInTheDocument();
  });

  it("explains every functional group and marks single-select groups in Chinese", async () => {
    localStorage.setItem(LOCALE_STORAGE_KEY, "zh-CN");
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("代理已在线");
    await user.click(screen.getByRole("button", { name: "工具" }));

    expect(screen.getByText("限制越界和危险操作：Stop That Shit 约束任务范围，Agent Guard 检查密钥与高风险命令；两者可同时启用。")).toBeInTheDocument();
    const workflowHeading = screen.getByRole("heading", { name: "主工作流" });
    const automationHeading = screen.getByRole("heading", { name: "自动执行器" });
    expect(within(workflowHeading.parentElement as HTMLElement).getByText("本组只能启用 1 个 · 主工作流")).toBeInTheDocument();
    expect(within(automationHeading.parentElement as HTMLElement).getByText("本组只能启用 1 个 · 自动执行器")).toBeInTheDocument();
    expect(screen.getByText("决定开发任务如何从需求推进到交付：OpenSpec 偏规格与验收，Superpowers 偏计划与 TDD，gstack 偏产品到发布全流程；本组只能启用 1 个。")).toBeInTheDocument();
    expect(screen.getByText("决定由谁持续推动任务执行：All in Luna 负责多代理协作与持久目标，Ralph Loop 负责循环执行到完成条件；本组只能启用 1 个，且都需用户明确启动。")).toBeInTheDocument();
  });

  it("confirms a single-select switch and leaves the peer alone when cancelled", async () => {
    openspecInstalled = true;
    openspecEnabled = true;
    superpowersInstalled = true;
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));

    const superpowersCard = screen.getByText("Superpowers", { selector: "h3" }).closest("article") as HTMLElement;
    await user.click(within(superpowersCard).getByRole("button", { name: "Enable" }));

    expect(confirmSpy).toHaveBeenCalledWith(expect.stringContaining("OpenSpec"));
    expect(invokeMock).not.toHaveBeenCalledWith("set_addon_enabled", { id: "superpowers", enabled: true });
    confirmSpy.mockRestore();
  });

  it("enables the peer once the single-select switch is confirmed", async () => {
    openspecInstalled = true;
    openspecEnabled = true;
    superpowersInstalled = true;
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));

    const superpowersCard = screen.getByText("Superpowers", { selector: "h3" }).closest("article") as HTMLElement;
    await user.click(within(superpowersCard).getByRole("button", { name: "Enable" }));

    expect(invokeMock).toHaveBeenCalledWith("set_addon_enabled", { id: "superpowers", enabled: true });
    confirmSpy.mockRestore();
  });

  it("enables a lone workflow tool without asking", async () => {
    superpowersInstalled = true;
    const confirmSpy = vi.spyOn(window, "confirm");
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));

    const superpowersCard = screen.getByText("Superpowers", { selector: "h3" }).closest("article") as HTMLElement;
    await user.click(within(superpowersCard).getByRole("button", { name: "Enable" }));

    expect(confirmSpy).not.toHaveBeenCalled();
    expect(invokeMock).toHaveBeenCalledWith("set_addon_enabled", { id: "superpowers", enabled: true });
    confirmSpy.mockRestore();
  });

  it("uses addon controls without restarting the proxy for new-session tools", async () => {
    const user = userEvent.setup();
    renderCommunityApp();
    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));
    const card = screen.getByText("Agent Guard", { selector: "h3" }).closest("article") as HTMLElement;
    await user.click(within(card).getByRole("button", { name: "Install" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("install_addon", { id: "agent-guard" }));
    expect(invokeMock).not.toHaveBeenCalledWith("force_restart_headroom");
  });

  it("shows each add-on's actual default-mode choices and saves a selected mode", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));

    const cavemanModes = screen.getByRole("radiogroup", { name: "Default mode for Caveman" });
    const ponytailModes = screen.getByRole("radiogroup", { name: "Default mode for Ponytail" });
    expect(within(cavemanModes).getAllByRole("radio")).toHaveLength(6);
    expect(within(ponytailModes).getAllByRole("radio")).toHaveLength(3);
    expect(within(cavemanModes).getByRole("radio", { name: "Wenyan Full" })).toBeChecked();
    expect(within(ponytailModes).getByRole("radio", { name: "Ultra" })).toBeChecked();
    expect(within(ponytailModes).queryByRole("radio", { name: /Wenyan/i })).not.toBeInTheDocument();

    await user.click(within(cavemanModes).getByRole("radio", { name: "Wenyan Ultra" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_addon_mode", { id: "caveman", mode: "wenyan-ultra" });
    });
    expect(await screen.findByText("Caveman default mode set to Wenyan Ultra. New agent sessions use it.")).toBeInTheDocument();
  });

  it("keeps default-mode controls unavailable until an add-on is installed and enabled", async () => {
    ponytailInstalled = false;
    ponytailEnabled = false;
    cavemanEnabled = false;
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await userEvent.setup().click(screen.getByRole("button", { name: "Tools" }));

    const cavemanModes = screen.getByRole("radiogroup", { name: "Default mode for Caveman" });
    const ponytailModes = screen.getByRole("radiogroup", { name: "Default mode for Ponytail" });
    for (const mode of within(cavemanModes).getAllByRole("radio")) {
      expect(mode).toBeDisabled();
    }
    for (const mode of within(ponytailModes).getAllByRole("radio")) {
      expect(mode).toBeDisabled();
    }
  });

  it("loads the local Activity feed only after the Activity view is opened", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    expect(invokeMock).not.toHaveBeenCalledWith("get_activity_feed");
    await user.click(screen.getByRole("button", { name: "Activity" }));

    expect(await screen.findByRole("heading", { name: "Recent local activity" })).toBeInTheDocument();
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("get_activity_feed");
    });
  });

  it("starts a local Claude Learn scan and exposes every supported session source", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Optimize" }));
    expect(await screen.findByRole("heading", { name: "Scan local coding sessions" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Claude Code" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Codex" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "OpenCode" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Grok" })).toBeInTheDocument();

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Start Learn" })).toBeEnabled();
    });
    await user.click(screen.getByRole("button", { name: "Start Learn" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("start_headroom_learn", {
        agent: "claude",
        projectPath: "/Users/example/project",
      });
    });
  });

  it("defaults Learn to Codex when no usable Claude source exists", async () => {
    claudeCliAvailable = false;
    exposeClaudeProjects = false;
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Optimize" }));
    expect(await screen.findByRole("heading", { name: "Scan local coding sessions" })).toBeInTheDocument();

    const agentSelect = screen.getByLabelText("Session source");
    await waitFor(() => expect(agentSelect).toHaveValue("codex"));
    await waitFor(() => expect(screen.getByRole("button", { name: "Start Learn" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "Start Learn" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("start_headroom_learn", {
        agent: "codex",
        projectPath: null,
      });
    });
  });

  it("renders Codex applied learnings from AGENTS.md and does not use Claude metadata", async () => {
    codexLastRunAt = "2026-09-03T08:00:00Z";
    appliedPatterns = {
      claudeMd: [],
      memoryMd: [{ title: "Live memory", bullets: ["Claude-only memory"] }],
      codexAgentsMd: [{ title: "Codex rules", bullets: ["Read AGENTS.md before editing."] }],
      codexInstructionsMd: [{ title: "Codex instructions", bullets: ["Keep retries bounded."] }],
    };
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Optimize" }));
    await user.selectOptions(screen.getByLabelText("Session source"), "codex");

    expect(await screen.findByRole("button", { name: /Headroom learnings in AGENTS\.md: 1/i })).toBeEnabled();
    expect(screen.getByRole("button", { name: /Headroom reminders in instructions\.md: 1/i })).toBeEnabled();
    expect(screen.queryByRole("button", { name: /not scanned yet/i })).toBeNull();
    expect(invokeMock).toHaveBeenCalledWith("get_headroom_learn_status", { projectPath: "codex" });
    expect(invokeMock).toHaveBeenCalledWith("list_applied_patterns", { projectPath: "codex" });

    await user.click(screen.getByRole("button", { name: /Headroom learnings in AGENTS\.md: 1/i }));
    expect(await screen.findByRole("dialog")).toHaveTextContent("Read AGENTS.md before editing.");
    expect(screen.queryByText("Claude-only memory")).not.toBeInTheDocument();
  });

  it("keeps applied results visible after a successful Learn before project metadata refreshes", async () => {
    learnStatus = {
      ...learnStatus,
      projectPath: "/Users/example/project",
      finishedAt: "2026-09-03T08:00:00Z",
      success: true,
      summary: "Learn completed.",
    };
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Optimize" }));
    expect(screen.queryByRole("button", { name: /not scanned yet/i })).toBeNull();
    expect(screen.getByRole("button", { name: /0 learnings in CLAUDE\.local\.md/i })).toBeDisabled();
  });

  it("shows a local unsupported state instead of starting Learn on an unsupported platform", async () => {
    runtime.headroomLearnSupported = false;
    runtime.headroomLearnDisabledReason = null;
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Optimize" }));
    expect(await screen.findByText("Learn is unavailable on this platform.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start Learn" })).toBeDisabled();
  });

  it("shows local diagnostics logs and can refresh them", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Diagnostics" }));
    expect(await screen.findByRole("heading", { name: "Runtime and local logs" })).toBeInTheDocument();
    expect(await screen.findByText(/runtime online/)).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("get_headroom_logs", { maxLines: 120 });

    await user.click(screen.getByRole("button", { name: "Refresh logs" }));
    await waitFor(() => {
      expect(invokeMock.mock.calls.filter(([command]) => command === "get_headroom_logs")).toHaveLength(2);
    });
  });

  it("changes local autostart and sends only an explicit local test notification", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Local settings" }));
    const autostart = await screen.findByLabelText("Start at login");
    expect(autostart).not.toBeChecked();
    await user.click(autostart);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_autostart_enabled", { enabled: true });
    });
    await user.click(screen.getByRole("button", { name: "Send local test notification" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("show_notification", {
        title: "Headroom Local Community test",
        body: "This is a local notification test from Headroom Local Community.",
        action: "local-test",
      });
    });
  });

  it("prevents competing default-mode writes while one mode save is busy", async () => {
    const user = userEvent.setup();
    let resolveModeSave: ((state: DashboardState) => void) | undefined;
    const pendingModeSave = new Promise<DashboardState>((resolve) => {
      resolveModeSave = resolve;
    });
    invokeMock.mockImplementation((command: string, args?: unknown) => {
      if (command === "set_addon_mode") {
        const { mode } = (args ?? {}) as { mode?: string };
        if (mode) cavemanMode = mode;
        return pendingModeSave;
      }
      if (command === "get_dashboard_state") return Promise.resolve(dashboardState());
      if (command === "get_runtime_status") return Promise.resolve(runtime);
      if (command === "get_client_connectors") return Promise.resolve(connectorsState());
      return Promise.resolve(args);
    });
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Tools" }));
    const cavemanModes = screen.getByRole("radiogroup", { name: "Default mode for Caveman" });
    await user.click(within(cavemanModes).getByRole("radio", { name: "Lite" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_addon_mode", { id: "caveman", mode: "lite" });
    });
    for (const mode of within(cavemanModes).getAllByRole("radio")) {
      expect(mode).toBeDisabled();
    }
    await user.click(within(cavemanModes).getByRole("radio", { name: "Ultra" }));
    expect(invokeMock.mock.calls.filter(([command]) => command === "set_addon_mode")).toHaveLength(1);

    resolveModeSave?.(dashboardState());
    await screen.findByText("Caveman default mode set to Lite. New agent sessions use it.");
  });

  it("localizes the user-level default-mode explanation in every supported language", async () => {
    const locales = [
      { locale: "en", toolsLabel: "Tools", help: "Saved as your user default. New agent sessions use it; environment variables can override it." },
      { locale: "zh-CN", toolsLabel: "工具", help: "保存为用户级默认值，仅对新的 agent 会话生效；环境变量可能覆盖它。" },
      { locale: "zh-TW", toolsLabel: "工具", help: "儲存為使用者層級預設值，只對新的 agent 工作階段生效；環境變數可能覆蓋它。" },
      { locale: "ja", toolsLabel: "ツール", help: "ユーザー既定値として保存され、新しい agent セッションで有効になります。環境変数が上書きする場合があります。" },
      { locale: "ko", toolsLabel: "도구", help: "사용자 기본값으로 저장되며 새 agent 세션에 적용됩니다. 환경 변수가 이를 덮어쓸 수 있습니다." },
    ] as const;

    for (const { locale, toolsLabel, help } of locales) {
      localStorage.setItem(LOCALE_STORAGE_KEY, locale);
      const user = userEvent.setup();
      const { unmount } = renderCommunityApp();
      await screen.findByText(locale === "en" ? "Proxy online" : /代理已在线|代理已上線|プロキシはオンラインです|프록시 온라인/);
      await user.click(screen.getByRole("button", { name: toolsLabel }));
      const cavemanCard = screen.getByText("Caveman").closest("article");
      expect(cavemanCard).not.toBeNull();
      expect(await within(cavemanCard as HTMLElement).findByText(help)).toBeInTheDocument();
      unmount();
    }
  });

  it("changes the language immediately and persists the selected locale", async () => {
    const user = userEvent.setup();
    renderCommunityApp();

    await screen.findByText("Proxy online");
    await user.click(screen.getByRole("button", { name: "Local settings" }));
    await user.selectOptions(screen.getByLabelText("Language"), "zh-CN");

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "独立的社区版" })).toBeInTheDocument();
      expect(document.documentElement.lang).toBe("zh-CN");
    });
    expect(localStorage.getItem(LOCALE_STORAGE_KEY)).toBe("zh-CN");
  });

  it("uses a persisted locale when the Community app starts", async () => {
    localStorage.setItem(LOCALE_STORAGE_KEY, "ja");
    const user = userEvent.setup();
    renderCommunityApp();

    expect(await screen.findByText("プロキシはオンラインです")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "ローカル設定" }));

    await waitFor(() => expect(document.documentElement.lang).toBe("ja"));
    expect(screen.getByLabelText("言語")).toHaveValue("ja");
  });

  it("falls back to the system locale for an invalid stored value", async () => {
    localStorage.setItem(LOCALE_STORAGE_KEY, "not-a-supported-locale");
    setNavigatorLanguages(["zh-HK"]);
    const user = userEvent.setup();
    renderCommunityApp();

    expect(await screen.findByText("代理已上線")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "本機設定" }));

    await waitFor(() => expect(document.documentElement.lang).toBe("zh-TW"));
    expect(screen.getByLabelText("語言")).toHaveValue("system");
  });

  it("does not render account or paid-product controls in any supported language", async () => {
    const forbiddenLabels = [
      /Sign in|Account|Upgrade|Pricing|Subscription/i,
      /登录|登入|账户|帳戶|注册|註冊|升级|升級|定价|定價|订阅|訂閱/,
      /サインイン|アカウント|登録|アップグレード|料金|サブスクリプション/,
      /로그인|계정|가입|업그레이드|요금|구독/,
    ];

    const proxyTitles = {
      system: "Proxy online",
      en: "Proxy online",
      "zh-CN": "代理已在线",
      "zh-TW": "代理已上線",
      ja: "プロキシはオンラインです",
      ko: "프록시 온라인",
    } as const;

    for (const locale of ["system", "en", "zh-CN", "zh-TW", "ja", "ko"] as const) {
      localStorage.removeItem(LOCALE_STORAGE_KEY);
      if (locale !== "system") localStorage.setItem(LOCALE_STORAGE_KEY, locale);
      const { unmount } = renderCommunityApp();
      await screen.findByText(proxyTitles[locale]);
      const renderedDom = document.body.innerHTML;
      for (const forbiddenLabel of forbiddenLabels) {
        expect(renderedDom).not.toMatch(forbiddenLabel);
      }
      unmount();
    }
  });
});
