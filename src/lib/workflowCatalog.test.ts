import { describe, expect, it } from "vitest";

import {
  addonManagementPage,
  activationScopeCopyByScope,
  getActivationScopeCopy,
  groupToolsByCategory,
  toolCategoryCopy,
  toolCopy,
  TOOL_CATEGORY_ORDER,
  workflowSwitchPeers,
} from "./workflowCatalog";

describe("addonManagementPage", () => {
  it("separates MCP registrations, skill workflows, and local commands", () => {
    for (const id of ["serena", "context7", "codebase-memory"]) {
      expect(addonManagementPage({ id })).toBe("mcp");
    }
    expect(addonManagementPage({ id: "openspec", runtime: "node", activationScope: "new_session" })).toBe("skills");
    expect(addonManagementPage({ id: "caveman", runtime: "plugin" })).toBe("skills");
    for (const id of ["rtk", "markitdown", "new-cli"]) {
      expect(addonManagementPage({ id, activationScope: "immediate" })).toBe("addons");
    }
  });

  it("omits host-curated plugins from every Headroom management page", () => {
    for (const id of ["superpowers", "codex-security"]) {
      expect(addonManagementPage({ id, runtime: "plugin", activationScope: "new_session" })).toBeNull();
    }
  });
});

function tool(
  id: string,
  workflowGroup: string | null,
  enabled: boolean,
  status = "healthy"
) {
  return { id, name: id.toUpperCase(), workflowGroup, enabled, status };
}

describe("workflowSwitchPeers", () => {
  const tools = [
    tool("openspec", "primary_workflow", true),
    tool("superpowers", "primary_workflow", false),
    // On in the manifest but never installed: nothing to switch off on disk.
    tool("gstack", "primary_workflow", true, "not_installed"),
    tool("ralph-loop", "execution_engine", true),
    tool("serena", null, true),
  ];

  it("lists the enabled, installed peers of the same group", () => {
    expect(workflowSwitchPeers(tools, "superpowers").map((peer) => peer.id)).toEqual(["openspec"]);
  });

  it("ignores the other group, disabled peers, and uninstalled peers", () => {
    // gstack reads as enabled but was never installed, and serena has no group.
    expect(workflowSwitchPeers(tools, "gstack").map((peer) => peer.id)).toEqual(["openspec"]);
    expect(workflowSwitchPeers(tools, "serena")).toEqual([]);
  });

  it("reports the active peer when the tool is already on", () => {
    expect(workflowSwitchPeers(tools, "openspec").map((peer) => peer.id)).toEqual([]);
  });

  it("returns nothing for an unknown id", () => {
    expect(workflowSwitchPeers(tools, "missing")).toEqual([]);
  });
});

describe("groupToolsByCategory", () => {
  it("creates every group in the documented order and maps unknown categories to other", () => {
    const tools = [
      { id: "unknown", category: "future" },
      { id: "workflow", category: "workflow" },
      { id: "core", category: "core" },
      { id: "missing", category: null },
    ];

    const groups = groupToolsByCategory(tools);

    expect([...groups.keys()]).toEqual(TOOL_CATEGORY_ORDER);
    expect(groups.get("core")).toEqual([{ id: "core", category: "core" }]);
    expect(groups.get("workflow")).toEqual([{ id: "workflow", category: "workflow" }]);
    expect(groups.get("other")).toEqual([
      { id: "unknown", category: "future" },
      { id: "missing", category: null },
    ]);
  });
});

describe("activation scope copy", () => {
  it("explains immediate, new-session, and client-restart activation in Chinese", () => {
    expect(getActivationScopeCopy("immediate")["zh-CN"]).toContain("即时生效");
    expect(getActivationScopeCopy("new_session")["zh-CN"]).toContain("新 Codex 会话生效");
    expect(getActivationScopeCopy("client_restart")["zh-CN"]).toContain("重载 MCP");
    expect(getActivationScopeCopy("client_restart")["zh-CN"]).toContain("或重启客户端");
    expect(getActivationScopeCopy("client_restart")["zh-CN"]).toContain("不会自动刷新");
  });

  it("falls back to an explicit unknown scope without claiming a restart", () => {
    expect(getActivationScopeCopy("unexpected")).toEqual(activationScopeCopyByScope.unknown);
    expect(getActivationScopeCopy(null)["zh-CN"]).toContain("取决于具体工具");
    expect(getActivationScopeCopy(undefined)["zh-CN"]).not.toContain("重启");
  });
});

describe("Headroom workflow catalog copy", () => {
  it("lists third-party workflows and guardrails without repeating official entries", () => {
    expect(toolCopy.superpowers).toBeUndefined();
    expect(toolCopy["codex-security"]).toBeUndefined();
    expect(toolCategoryCopy.guardrails.description["zh-CN"]).toContain("Stop That Shit");
    expect(toolCategoryCopy.guardrails.description["zh-CN"]).toContain("Agent Guard");
    for (const copy of Object.values(toolCategoryCopy.workflow.description)) {
      expect(copy).toContain("OpenSpec");
      expect(copy).toContain("gstack");
      expect(copy).not.toContain("Superpowers");
    }
    for (const copy of Object.values(toolCategoryCopy.guardrails.description)) {
      expect(copy).not.toContain("Codex Security");
    }
  });
});
