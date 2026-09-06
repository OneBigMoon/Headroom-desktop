import { describe, expect, it } from "vitest";
import {
  bridgeConnectionNeedsRepair,
  bridgeNeedsRestart,
  bridgePairingIsUsable,
  bridgeResponseIsCurrent,
  chooseCodexBridgeActionWorkspace,
  nextBridgeWorkspaceGeneration,
  recommendedAddonPresetMatches,
} from "./App";
import { RECOMMENDED_ADDON_PRESET } from "./components/AddonPresetBar";
import { mockDashboard } from "./lib/mockData";

describe("bridge connector endpoint changes", () => {
  it("restarts a stopped bridge without reauthorizing a valid connector", () => {
    expect(bridgeNeedsRestart({ paired: true, installed: true, running: false })).toBe(true);
    expect(bridgeNeedsRestart({ paired: true, installed: true, running: true })).toBe(false);
    expect(bridgeNeedsRestart({ paired: false, installed: true, running: false })).toBe(false);
  });
  it("invalidates an old action after switching away and back to the same workspace", () => {
    const original = { workspace: "/workspace/a", generation: 0 };
    const switched = nextBridgeWorkspaceGeneration(original, "/workspace/b");
    const switchedBack = nextBridgeWorkspaceGeneration(switched, "/workspace/a");

    expect(switchedBack.workspace).toBe(original.workspace);
    expect(switchedBack.generation).toBe(2);
    expect(switchedBack.generation).not.toBe(original.generation);
  });

  it("flags a new endpoint only after the bridge is connected", () => {
    expect(bridgeConnectionNeedsRepair("https://old.example/mcp", "https://new.example/mcp", true)).toBe(true);
    expect(bridgeConnectionNeedsRepair("https://old.example/mcp", "https://new.example/mcp", false)).toBe(false);
  });

  it("does not flag the initial endpoint or an unchanged endpoint", () => {
    expect(bridgeConnectionNeedsRepair(null, "https://new.example/mcp", true)).toBe(false);
    expect(bridgeConnectionNeedsRepair("https://same.example/mcp", "https://same.example/mcp", true)).toBe(false);
    expect(bridgeConnectionNeedsRepair("HTTPS://SAME.EXAMPLE/mcp/", "https://same.example", true)).toBe(false);
  });

  it("flags a backend-reported change after an app restart through the same comparison", () => {
    expect(bridgeConnectionNeedsRepair("https://old.example/mcp", "https://new.example/mcp/", true)).toBe(true);
  });

  it("keeps a fresh pairing code while it is unexpired", () => {
    expect(
      bridgePairingIsUsable({ pairingCode: "ABCD-EFGH", pairingExpiresAt: 2_000_000_000_000 }, 1_999_999_999_999),
    ).toBe(true);
    expect(
      bridgePairingIsUsable({ pairingCode: "ABCD-EFGH", pairingExpiresAt: 2_000_000_000_000 }, 2_000_000_000_000),
    ).toBe(false);
  });

  it("chooses the explicit workspace, otherwise the freshly read workspace", () => {
    expect(chooseCodexBridgeActionWorkspace(" /repo/selected ", "/repo/stale")).toBe("/repo/selected");
    expect(chooseCodexBridgeActionWorkspace("", "/repo/fresh")).toBe("/repo/fresh");
    expect(chooseCodexBridgeActionWorkspace("  ", null)).toBe(null);
  });

  it("rejects bridge responses from either an older request or workspace", () => {
    expect(bridgeResponseIsCurrent(4, 4, 2, 2)).toBe(true);
    expect(bridgeResponseIsCurrent(3, 4, 2, 2)).toBe(false);
    expect(bridgeResponseIsCurrent(4, 4, 1, 2)).toBe(false);
  });

  it("treats a dashboard that differs from the recommendation as custom", () => {
    expect(recommendedAddonPresetMatches(mockDashboard)).toBe(false);
    const template = mockDashboard.tools[0];
    const matched = {
      ...mockDashboard,
      tools: Object.entries(RECOMMENDED_ADDON_PRESET).map(([id, target]) => ({
        ...template,
        id,
        enabled: target.enabled,
        status: target.enabled ? ("healthy" as const) : ("not_installed" as const),
        defaultMode: target.mode ?? null,
      })),
    };
    expect(recommendedAddonPresetMatches(matched)).toBe(true);
    expect(recommendedAddonPresetMatches({
      ...matched,
      tools: [...matched.tools, { ...template, id: "optional-extra", required: false, enabled: true, status: "healthy" as const }],
    })).toBe(false);
    expect(recommendedAddonPresetMatches({
      ...matched,
      tools: [...matched.tools, { ...template, id: "required-core", required: true, enabled: true, status: "healthy" as const }],
    })).toBe(true);
  });
});
