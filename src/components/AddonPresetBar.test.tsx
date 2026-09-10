import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { AddonPresetBar, listPresetDisables } from "./AddonPresetBar";
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
  it("names the enabled tools the recommended preset would switch off", () => {
    const dashboard = dashboardWith([
      tool("openspec", "OpenSpec", true, "healthy"),
      tool("superpowers", "Superpowers", false, "not_installed"),
      tool("rtk", "RTK", true, "healthy"),
    ]);

    expect(listPresetDisables(dashboard).map((entry) => entry.name)).toEqual(["OpenSpec"]);
  });

  it("ignores tools the preset does not own and tools that are already off", () => {
    const dashboard = dashboardWith([
      tool("serena", "Serena", true, "healthy"),
      tool("openspec", "OpenSpec", false, "healthy"),
      tool("caveman", "Caveman", false, "not_installed"),
    ]);

    expect(listPresetDisables(dashboard)).toEqual([]);
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
