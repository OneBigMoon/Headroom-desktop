import type { ManagedTool } from "./types";

export interface AddonUpdateCheck {
  id: string;
  latestVersion: string | null;
  error: string | null;
}

export interface CheckedManagedTool extends ManagedTool {
  /** Whether a confirmed update can be started for this tool. */
  updateActionAvailable?: boolean;
  /** Whether an installed degraded tool can be repaired by reinstalling it. */
  repairActionAvailable?: boolean;
  /** Whether this tool's upstream version check failed. */
  updateCheckFailed?: boolean;
  upstreamVersion?: string | null;
  upstreamUpdateAvailable?: boolean;
  updateRequiresAppUpdate?: boolean;
}

/// Addons whose update is driven by upstream's own latest version: whatever
/// the source publishes is what the installer pulls, so any newer version is
/// deliverable.
///
/// `headroom` is deliberately absent. Its runtime is pinned
/// (`HEADROOM_PINNED_VERSION`) and shipped with a verified per-platform wheel
/// plus a dependency lock, so this Community build can only install the
/// version it was packed against -- the upgrade path resolves the pin, not
/// upstream's latest. Listing it here advertised the raw PyPI latest while the
/// build pinned an older runtime, so the button promised a version its click
/// could never install. It falls through to the supported-version gate, which
/// offers the pin while the runtime is behind it and names the gap once
/// upstream moves ahead again.
const DIRECT_UPSTREAM_UPDATE_IDS = new Set([
  "rtk",
  "markitdown",
  "serena",
  "codebase-memory",
  "context7",
]);

const REPAIRABLE_DEGRADED_IDS = new Set(["codebase-memory", "context7"]);

interface ParsedVersion {
  core: number[];
  prerelease: Array<number | string>;
}

function numericVersion(value: string | null | undefined): ParsedVersion | null {
  if (!value) return null;
  const normalized = value.trim().replace(/^[vV]/, "").split("+", 1)[0];
  const match = /^(\d+\.\d+(?:\.\d+)?)(?:-([0-9A-Za-z.-]+))?$/.exec(normalized);
  if (!match) return null;
  return {
    core: match[1].split(".").map(Number),
    prerelease: match[2]
      ? match[2].split(".").map((part) => (/^\d+$/.test(part) ? Number(part) : part.toLowerCase()))
      : [],
  };
}

export function compareAddonVersions(left: string, right: string): number | null {
  const a = numericVersion(left);
  const b = numericVersion(right);
  if (!a || !b) return null;
  for (let index = 0; index < Math.max(a.core.length, b.core.length); index += 1) {
    const difference = (a.core[index] ?? 0) - (b.core[index] ?? 0);
    if (difference !== 0) return Math.sign(difference);
  }
  if (!a.prerelease.length || !b.prerelease.length) {
    return Math.sign(b.prerelease.length - a.prerelease.length);
  }
  for (let index = 0; index < Math.max(a.prerelease.length, b.prerelease.length); index += 1) {
    const leftPart = a.prerelease[index];
    const rightPart = b.prerelease[index];
    if (leftPart === undefined) return -1;
    if (rightPart === undefined) return 1;
    if (leftPart === rightPart) continue;
    if (typeof leftPart === "number" && typeof rightPart === "number") {
      return Math.sign(leftPart - rightPart);
    }
    if (typeof leftPart === "number") return -1;
    if (typeof rightPart === "number") return 1;
    return leftPart < rightPart ? -1 : 1;
  }
  return 0;
}

export function applyAddonUpdateChecks(
  tools: ManagedTool[],
  checks: AddonUpdateCheck[],
): CheckedManagedTool[] {
  const byId = new Map(checks.map((check) => [check.id, check]));
  return tools.map((tool) => {
    const check = byId.get(tool.id);
    const installed = tool.status !== "not_installed";
    const repairActionAvailable =
      installed &&
      tool.status === "degraded" &&
      (tool.runtime === "plugin" || REPAIRABLE_DEGRADED_IDS.has(tool.id));
    if (!installed) {
      return {
        ...tool,
        updateActionAvailable: false,
        repairActionAvailable: false,
        updateCheckFailed: false,
        updateAvailable: false,
        availableVersion: null,
      };
    }
    if (check?.error) {
      return {
        ...tool,
        updateActionAvailable: false,
        repairActionAvailable,
        updateCheckFailed: true,
        updateAvailable: undefined,
        availableVersion: null,
        upstreamVersion: null,
        upstreamUpdateAvailable: false,
        updateRequiresAppUpdate: false,
      };
    }
    if (!check?.latestVersion) {
      return {
        ...tool,
        updateActionAvailable: false,
        repairActionAvailable,
        updateCheckFailed: false,
        updateAvailable: false,
        availableVersion: null,
        upstreamVersion: null,
        upstreamUpdateAvailable: false,
        updateRequiresAppUpdate: false,
      };
    }

    const comparison = compareAddonVersions(check.latestVersion, tool.version);
    const upstreamAhead = comparison === 1;
    // A fresh successful check is authoritative; do not carry an optimistic
    // action over after the upstream version catches up. Updating installs the
    // pinned version and never touches the enable state, so a disabled tool is
    // still updatable.
    const updateActionAvailable = upstreamAhead;
    if (DIRECT_UPSTREAM_UPDATE_IDS.has(tool.id) && comparison === null) {
      return {
        ...tool,
        updateActionAvailable,
        repairActionAvailable,
        updateCheckFailed: false,
        upstreamVersion: check.latestVersion,
        upstreamUpdateAvailable: false,
        updateAvailable: false,
        availableVersion: null,
        updateRequiresAppUpdate: false,
      };
    }
    if (DIRECT_UPSTREAM_UPDATE_IDS.has(tool.id) && comparison !== null) {
      return {
        ...tool,
        updateActionAvailable,
        repairActionAvailable,
        updateCheckFailed: false,
        updateAvailable: upstreamAhead,
        availableVersion: upstreamAhead ? check.latestVersion : null,
        upstreamVersion: check.latestVersion,
        upstreamUpdateAvailable: upstreamAhead,
        updateRequiresAppUpdate: false,
      };
    }
    if (tool.runtime === "plugin") {
      return {
        ...tool,
        updateActionAvailable,
        repairActionAvailable,
        updateAvailable: upstreamAhead,
        availableVersion: upstreamAhead ? check.latestVersion : null,
        upstreamVersion: check.latestVersion,
        upstreamUpdateAvailable: upstreamAhead,
        updateRequiresAppUpdate: false,
      };
    }
    if (!upstreamAhead) {
      return {
        ...tool,
        updateActionAvailable: false,
        repairActionAvailable,
        updateCheckFailed: false,
        updateAvailable: false,
        availableVersion: null,
        upstreamVersion: check.latestVersion,
        upstreamUpdateAvailable: false,
        updateRequiresAppUpdate: false,
      };
    }

    const supportedVersion = tool.supportedVersion;
    const beyondSupported =
      !supportedVersion || compareAddonVersions(check.latestVersion, supportedVersion) === 1;
    if (beyondSupported) {
      // Upstream has moved past what this build was packed against. The
      // upgrade path installs the version the build supports, never upstream's
      // latest, so only that pin is a real offer: a `availableVersion` above
      // `supportedVersion` can only have been computed from upstream's latest,
      // and its click resolves the pin and looks dead (that is exactly how
      // "更新到 v0.37.0" behaved). Keep the pin, drop everything the build
      // cannot deliver, and hand the caller both versions to explain the gap.
      const offered = tool.availableVersion ?? null;
      const offerDeliverable =
        tool.updateAvailable === true &&
        offered !== null &&
        (supportedVersion === undefined ||
          compareAddonVersions(offered, supportedVersion) !== 1);
      return {
        ...tool,
        updateActionAvailable: offerDeliverable,
        repairActionAvailable,
        updateCheckFailed: false,
        updateAvailable: offerDeliverable,
        availableVersion: offerDeliverable ? offered : null,
        upstreamVersion: check.latestVersion,
        upstreamUpdateAvailable: true,
        updateRequiresAppUpdate: true,
      };
    }
    return {
      ...tool,
      updateActionAvailable,
      repairActionAvailable,
      updateCheckFailed: false,
      upstreamVersion: check.latestVersion,
      upstreamUpdateAvailable: true,
      updateRequiresAppUpdate: false,
    };
  });
}
