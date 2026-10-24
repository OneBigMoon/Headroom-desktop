import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  AddonPresetBar,
  listPresetDisables,
  matchesRecommendedAddonPreset,
  RECOMMENDED_ADDON_PRESET,
} from "./AddonPresetBar";
import { I18nProvider, LOCALE_STORAGE_KEY } from "../lib/i18n";
import { mockDashboard } from "../lib/mockData";
import type { DashboardState } from "../lib/types";

function tool(id: string, name: string, enabled: boolean, status: string) {
  return {
    id,
    name,
    description: `${name} description.`,
    runtime: "plugin" as const,
    required: false,
    enabled,
    status: status as DashboardState["tools"][number]["status"],
    sourceUrl: `https://example.invalid/${id}`,
    version: "1.0.0",
  };
}

function dashboardWith(tools: DashboardState["tools"]): DashboardState {
  return { ...mockDashboard, tools };
}

function recommendedDashboard(): DashboardState {
  return dashboardWith(Object.entries(RECOMMENDED_ADDON_PRESET).map(([id, target]) => ({
    ...tool(id, id, target.enabled, target.enabled ? "healthy" : "not_installed"),
    defaultMode: target.mode,
    workflowGroup: ["openspec", "gstack"].includes(id) ? "primary_workflow" : null,
  })));
}

function renderBar(dashboard: DashboardState, props: Partial<Parameters<typeof AddonPresetBar>[0]> = {}) {
  return render(
    <I18nProvider>
      <AddonPresetBar
        busy={false}
        dashboard={dashboard}
        mode="custom"
        onApplyRecommended={vi.fn()}
        onSelectCustom={vi.fn()}
        {...props}
      />
    </I18nProvider>
  );
}

beforeEach(() => {
  localStorage.removeItem(LOCALE_STORAGE_KEY);
});

describe("listPresetDisables", () => {
  it("keeps codebase indexing and learning prompts opt-in without changing current switches", () => {
    const dashboard = dashboardWith([
      tool("codebase-memory", "Codebase Memory", true, "healthy"),
      tool("grill-me", "Grill Me", true, "healthy"),
    ]);
    expect(RECOMMENDED_ADDON_PRESET["codebase-memory"].enabled).toBe(false);
    expect(RECOMMENDED_ADDON_PRESET["grill-me"].enabled).toBe(false);
    expect(listPresetDisables(dashboard).map((entry) => entry.id)).toEqual([
      "codebase-memory", "grill-me",
    ]);
    expect(dashboard.tools.every((entry) => entry.enabled)).toBe(true);
  });

  it("names the enabled tools the recommended preset would switch off", () => {
    const dashboard = dashboardWith([
      tool("openspec", "OpenSpec", true, "healthy"),
      tool("gstack", "gstack", true, "healthy"),
      tool("rtk", "RTK", true, "healthy"),
    ]);

    expect(listPresetDisables(dashboard).map((entry) => entry.name)).toEqual(["gstack"]);
  });

  it("ignores tools the preset does not own and tools that are already off", () => {
    const dashboard = dashboardWith([
      tool("serena", "Serena", true, "healthy"),
      tool("openspec", "OpenSpec", false, "healthy"),
      tool("caveman", "Caveman", false, "not_installed"),
    ]);

    expect(listPresetDisables(dashboard)).toEqual([]);
  });

  it("leaves official plugins outside the recommended preset", () => {
    expect(RECOMMENDED_ADDON_PRESET.openspec).toEqual({ enabled: true });
    expect(RECOMMENDED_ADDON_PRESET.superpowers).toBeUndefined();
    expect(RECOMMENDED_ADDON_PRESET["codex-security"]).toBeUndefined();

    const dashboard = dashboardWith([
      tool("codex-security", "Codex Security", true, "healthy"),
    ]);
    expect(listPresetDisables(dashboard)).toEqual([]);
  });

  it("names active official workflow peers before enabling recommended OpenSpec", () => {
    const dashboard = dashboardWith([
      { ...tool("openspec", "OpenSpec", false, "not_installed"), workflowGroup: "primary_workflow" },
      { ...tool("superpowers", "Superpowers", true, "healthy"), workflowGroup: "primary_workflow" },
      tool("codex-security", "Codex Security", true, "healthy"),
    ]);
    expect(listPresetDisables(dashboard)).toEqual([{ id: "superpowers", name: "Superpowers" }]);
    expect(dashboard.tools[1].enabled).toBe(true);
  });
});

describe("matchesRecommendedAddonPreset", () => {
  it("ignores enabled official plugins that do not conflict with the workflow", () => {
    const dashboard = recommendedDashboard();
    dashboard.tools.push(tool("codex-security", "Codex Security", true, "healthy"));
    expect(matchesRecommendedAddonPreset(dashboard)).toBe(true);
  });

  it("does not call conflicting OpenSpec and Superpowers a matching preset", () => {
    const dashboard = recommendedDashboard();
    dashboard.tools.push({ ...tool("superpowers", "Superpowers", true, "healthy"), workflowGroup: "primary_workflow" });
    expect(matchesRecommendedAddonPreset(dashboard)).toBe(false);
  });
});

describe("AddonPresetBar", () => {
  it("switches to the recommended preset on click", async () => {
    const onApplyRecommended = vi.fn();
    const user = userEvent.setup();
    renderBar(dashboardWith([]), { onApplyRecommended });

    await user.click(screen.getByRole("button", { name: "Recommended" }));

    expect(onApplyRecommended).toHaveBeenCalledTimes(1);
  });

  it("translates the preset controls instead of hardcoding one language", () => {
    localStorage.setItem(LOCALE_STORAGE_KEY, "zh-CN");
    renderBar(dashboardWith([]), { mode: "recommended" });

    expect(screen.getByRole("group", { name: "工具档位" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "推荐" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "自定义" })).toBeInTheDocument();
    expect(screen.getByText("推荐档位已锁定设置；先点自定义才能调整。")).toBeInTheDocument();
  });

  it("explains the custom preset while adjustments are unlocked", () => {
    renderBar(dashboardWith([]), { mode: "custom" });

    expect(screen.getByText("Custom lets you tune each tool and the savings level.")).toBeInTheDocument();
  });
});
