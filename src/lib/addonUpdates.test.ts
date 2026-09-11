import { describe, expect, it } from "vitest";

import { applyAddonUpdateChecks, compareAddonVersions } from "./addonUpdates";
import type { CheckedManagedTool } from "./addonUpdates";

function tool(overrides: Partial<CheckedManagedTool>): CheckedManagedTool {
  return {
    id: "context7",
    name: "Context7",
    description: "docs",
    runtime: "plugin",
    required: false,
    enabled: true,
    status: "healthy",
    sourceUrl: "https://example.test",
    version: "4.0.2",
    supportedVersion: "4.0.2",
    ...overrides,
  };
}

const DIRECT_UPSTREAM_UPDATE_IDS = [
  "rtk",
  "markitdown",
  "serena",
  "codebase-memory",
  "context7",
] as const;

describe("addon update checks", () => {
  it("compares normalized numeric versions", () => {
    expect(compareAddonVersions("v4.0.4", "4.0.3")).toBe(1);
    expect(compareAddonVersions("4.0", "4.0.0")).toBe(0);
    expect(compareAddonVersions("2.0.0-rc.8", "2.0.0-rc.7")).toBe(1);
    expect(compareAddonVersions("2.0.0", "2.0.0-rc.8")).toBe(1);
    expect(compareAddonVersions("2.0.0-rc.7", "2.0.0-rc.7")).toBe(0);
    expect(compareAddonVersions("latest", "4.0.0")).toBeNull();
  });

  it.each(DIRECT_UPSTREAM_UPDATE_IDS)("allows a direct upstream update for %s", (id) => {
    const [checked] = applyAddonUpdateChecks(
      [tool({ id, version: "1.0.0", supportedVersion: "1.0.0" })],
      [{ id, latestVersion: "1.1.0", error: null }],
    );

    expect(checked).toMatchObject({
      updateAvailable: true,
      availableVersion: "1.1.0",
      upstreamVersion: "1.1.0",
      upstreamUpdateAvailable: true,
      updateRequiresAppUpdate: false,
    });
  });

  it("does not show update notices for uninstalled tools", () => {
    const [checked] = applyAddonUpdateChecks(
      [tool({ status: "not_installed" })],
      [{ id: "context7", latestVersion: "4.0.4", error: null }],
    );
    expect(checked.upstreamUpdateAvailable).toBeUndefined();
    expect(checked.updateAvailable).toBe(false);
    expect(checked.availableVersion).toBeNull();
  });

  it("does not expose an update action without a confirmed update", () => {
    const [checked] = applyAddonUpdateChecks(
      [tool({ id: "allinluna", version: "2.0.0", supportedVersion: undefined })],
      [],
    );
    expect(checked.updateActionAvailable).toBe(false);
    expect(checked.updateAvailable).toBe(false);
    expect(checked.availableVersion).toBeNull();
  });

  it("clears stale update state when no fresh check is available", () => {
    const [checked] = applyAddonUpdateChecks(
      [
        tool({
          id: "allinluna",
          version: "2.0.0",
          supportedVersion: undefined,
          updateAvailable: true,
          availableVersion: "2.1.0",
        }),
      ],
      [],
    );
    expect(checked.updateActionAvailable).toBe(false);
    expect(checked.updateAvailable).toBe(false);
    expect(checked.availableVersion).toBeNull();
    expect(checked.upstreamVersion).toBeNull();
    expect(checked.upstreamUpdateAvailable).toBe(false);
    expect(checked.updateRequiresAppUpdate).toBe(false);
  });

  it("does not turn an upstream check failure into an update", () => {
    const [checked] = applyAddonUpdateChecks(
      [tool({ updateAvailable: true, availableVersion: "4.0.4" })],
      [{ id: "context7", latestVersion: null, error: "network unavailable" }],
    );
    expect(checked).toMatchObject({
      updateActionAvailable: false,
      updateCheckFailed: true,
      updateAvailable: undefined,
      availableVersion: null,
    });
  });

  it("exposes repair for degraded plugins and repairable MCP tools", () => {
    const [degraded, context7, codebaseMemory, healthy, missing, degradedBinary] = applyAddonUpdateChecks(
      [
        tool({ id: "degraded", status: "degraded" }),
        tool({ id: "context7", runtime: "node", status: "degraded" }),
        tool({ id: "codebase-memory", runtime: "binary", status: "degraded" }),
        tool({ id: "healthy", status: "healthy" }),
        tool({ id: "missing", status: "not_installed" }),
        tool({ id: "binary", runtime: "binary", status: "degraded" }),
      ],
      [],
    );
    expect(degraded.repairActionAvailable).toBe(true);
    expect(context7.repairActionAvailable).toBe(true);
    expect(codebaseMemory.repairActionAvailable).toBe(true);
    expect(healthy.repairActionAvailable).toBe(false);
    expect(missing.repairActionAvailable).toBe(false);
    expect(degradedBinary.repairActionAvailable).toBe(false);
  });

  it("shows the confirmed target version for enabled marketplace plugins", () => {
    const [checked] = applyAddonUpdateChecks(
      [tool({ id: "ponytail", version: "4.9.0", supportedVersion: "latest" })],
      [{ id: "ponytail", latestVersion: "5.0.0", error: null }],
    );
    expect(checked).toMatchObject({
      updateActionAvailable: true,
      updateAvailable: true,
      availableVersion: "5.0.0",
      upstreamUpdateAvailable: true,
      updateRequiresAppUpdate: false,
    });
  });

  it("does not expose the manual update action for non-plugin tools", () => {
    const [checked] = applyAddonUpdateChecks(
      [tool({ id: "headroom", runtime: "binary" })],
      [],
    );
    expect(checked.updateActionAvailable).toBe(false);
  });

  it("keeps the update actionable while the tool stays disabled", () => {
    const [directUpstream, marketplacePlugin] = applyAddonUpdateChecks(
      [
        tool({ id: "context7", enabled: false }),
        tool({ id: "ponytail", enabled: false, version: "4.9.0", supportedVersion: "latest" }),
      ],
      [
        { id: "context7", latestVersion: "4.0.4", error: null },
        { id: "ponytail", latestVersion: "5.0.0", error: null },
      ],
    );

    expect(directUpstream).toMatchObject({
      enabled: false,
      updateActionAvailable: true,
      updateAvailable: true,
      availableVersion: "4.0.4",
      upstreamUpdateAvailable: true,
      updateRequiresAppUpdate: false,
    });
    expect(marketplacePlugin).toMatchObject({
      enabled: false,
      updateActionAvailable: true,
      updateAvailable: true,
      availableVersion: "5.0.0",
      upstreamUpdateAvailable: true,
      updateRequiresAppUpdate: false,
    });
  });

  it("ignores an upstream version that cannot be compared", () => {
    const [checked] = applyAddonUpdateChecks(
      [tool({ id: "context7" })],
      [{ id: "context7", latestVersion: "latest", error: null }],
    );
    expect(checked).toMatchObject({
      updateAvailable: false,
      availableVersion: null,
      upstreamUpdateAvailable: false,
      updateRequiresAppUpdate: false,
    });
  });

  it("removes the marketplace's optimistic update action after a current version is confirmed", () => {
    const [checked] = applyAddonUpdateChecks(
      [tool({ id: "ponytail", version: "4.9.0", supportedVersion: "latest", updateAvailable: true })],
      [{ id: "ponytail", latestVersion: "4.9.0", error: null }],
    );
    expect(checked.updateAvailable).toBe(false);
    expect(checked.updateActionAvailable).toBe(false);
    expect(checked.availableVersion).toBeNull();
  });

  it("clears a stale direct-upstream update when the installed version catches up", () => {
    const [checked] = applyAddonUpdateChecks(
      [
        tool({
          id: "context7",
          version: "4.0.4",
          supportedVersion: "4.0.4",
          updateAvailable: true,
          availableVersion: "4.0.5",
        }),
      ],
      [{ id: "context7", latestVersion: "4.0.4", error: null }],
    );

    expect(checked).toMatchObject({
      updateActionAvailable: false,
      updateAvailable: false,
      availableVersion: null,
      upstreamVersion: "4.0.4",
      upstreamUpdateAvailable: false,
      updateCheckFailed: false,
    });
  });

  it("clears stale update state for a non-plugin when upstream is not ahead", () => {
    const [checked] = applyAddonUpdateChecks(
      [
        tool({
          id: "allinluna",
          runtime: "binary",
          version: "2.0.0",
          supportedVersion: "2.0.0",
          updateAvailable: true,
          availableVersion: "2.1.0",
          upstreamUpdateAvailable: true,
          updateRequiresAppUpdate: true,
        }),
      ],
      [{ id: "allinluna", latestVersion: "2.0.0", error: null }],
    );

    expect(checked).toMatchObject({
      updateActionAvailable: false,
      updateAvailable: false,
      availableVersion: null,
      upstreamVersion: "2.0.0",
      upstreamUpdateAvailable: false,
      updateRequiresAppUpdate: false,
      updateCheckFailed: false,
    });
  });

  it("clears stale upstream state when an update check fails", () => {
    const [stale] = applyAddonUpdateChecks(
      [
        tool({
          id: "context7",
          updateAvailable: true,
          availableVersion: "4.0.4",
        }),
      ],
      [{ id: "context7", latestVersion: "4.0.4", error: null }],
    );
    const [checked] = applyAddonUpdateChecks(
      [stale],
      [{ id: "context7", latestVersion: null, error: "network unavailable" }],
    );

    expect(checked).toMatchObject({
      updateActionAvailable: false,
      availableVersion: null,
      upstreamVersion: null,
      upstreamUpdateAvailable: false,
      updateRequiresAppUpdate: false,
      updateCheckFailed: true,
    });
  });

  it("keeps a failed tool non-actionable while preserving a successful update in a partial check", () => {
    const [updated, failed] = applyAddonUpdateChecks(
      [
        tool({ id: "context7", version: "4.0.2" }),
        tool({ id: "serena", version: "1.0.0", name: "Serena" }),
      ],
      [
        { id: "context7", latestVersion: "4.0.4", error: null },
        { id: "serena", latestVersion: null, error: "rate limited" },
      ],
    );

    expect(updated).toMatchObject({
      updateActionAvailable: true,
      updateAvailable: true,
      availableVersion: "4.0.4",
      updateCheckFailed: false,
    });
    expect(failed).toMatchObject({
      updateActionAvailable: false,
      updateAvailable: undefined,
      availableVersion: null,
      upstreamVersion: null,
      upstreamUpdateAvailable: false,
      updateCheckFailed: true,
    });
  });

  it("keeps a compatible pinned update actionable after the upstream check", () => {
    const [checked] = applyAddonUpdateChecks(
      [
        tool({
          version: "4.0.2",
          supportedVersion: "4.0.4",
          updateAvailable: true,
          availableVersion: "4.0.4",
        }),
      ],
      [{ id: "context7", latestVersion: "4.0.4", error: null }],
    );

    expect(checked).toMatchObject({
      updateAvailable: true,
      availableVersion: "4.0.4",
      upstreamVersion: "4.0.4",
      upstreamUpdateAvailable: true,
      updateRequiresAppUpdate: false,
    });
  });

  it("keeps the supported Headroom CLI update actionable", () => {
    const [checked] = applyAddonUpdateChecks(
      [
        tool({
          id: "headroom",
          name: "Headroom CLI",
          required: true,
          version: "0.35.0",
          supportedVersion: "0.36.5",
          updateAvailable: true,
          availableVersion: "0.36.5",
        }),
      ],
      [{ id: "headroom", latestVersion: "0.36.5", error: null }],
    );

    expect(checked).toMatchObject({
      updateAvailable: true,
      availableVersion: "0.36.5",
      upstreamVersion: "0.36.5",
      updateRequiresAppUpdate: false,
    });
  });

  /// Regression: this row once advertised the raw PyPI latest ("更新到
  /// v0.37.0") while the build pins the runtime it was packed against, so the
  /// click resolved the PIN -- no newer version to install -- and the button
  /// looked dead. It has to come back with nothing actionable no matter what
  /// the incoming payload claimed, because `...tool` carries the backend's own
  /// offer through the function.
  it("never advertises a Headroom CLI version this build cannot install", () => {
    const [checked] = applyAddonUpdateChecks(
      [
        tool({
          id: "headroom",
          name: "Headroom CLI",
          runtime: "python",
          required: true,
          version: "0.36.5",
          supportedVersion: "0.36.5",
          updateAvailable: true,
          availableVersion: "0.37.0",
          updateActionAvailable: true,
        }),
      ],
      [{ id: "headroom", latestVersion: "0.37.0", error: null }],
    );

    expect(checked).toMatchObject({
      updateAvailable: false,
      availableVersion: null,
      updateActionAvailable: false,
      upstreamVersion: "0.37.0",
      upstreamUpdateAvailable: true,
      updateRequiresAppUpdate: true,
    });
  });

  /// The pin still has to be installable: an older on-disk runtime is exactly
  /// what the transactional upgrade exists for, and it is the only Headroom
  /// offer this build can actually deliver.
  it("keeps offering the pinned Headroom CLI when the runtime is behind", () => {
    const [checked] = applyAddonUpdateChecks(
      [
        tool({
          id: "headroom",
          name: "Headroom CLI",
          runtime: "python",
          required: true,
          version: "0.35.0",
          supportedVersion: "0.36.5",
          updateAvailable: true,
          availableVersion: "0.36.5",
        }),
      ],
      [{ id: "headroom", latestVersion: "0.37.0", error: null }],
    );

    expect(checked).toMatchObject({
      updateAvailable: true,
      availableVersion: "0.36.5",
      updateRequiresAppUpdate: true,
      upstreamVersion: "0.37.0",
    });
  });

});
