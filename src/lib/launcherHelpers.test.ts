import { describe, expect, it } from "vitest";

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
  INSTALL_WIZARD_STEPS
} from "./launcherHelpers";
import type { ClientConnectorStatus } from "./types";

describe("install-wizard funnel steps", () => {
  // Pins the ordered step list. MUST stay in sync with DesktopFunnelStep::ORDER
  // in the headroom-web repo — the server ignores any step not in its list, so a
  // silent drift here means silently-dropped beacons.
  it("matches the canonical order shared with the server", () => {
    expect([...INSTALL_WIZARD_STEPS]).toEqual([
      "signup_gate_shown",
      "email_code_requested",
      "email_code_verified",
      "client_setup_shown",
      "client_setup_applied",
      "proxy_verify_started",
      "proxy_verified",
      "bootstrap_started",
      "bootstrap_completed",
      "bootstrap_failed",
      "post_install_shown",
      "first_optimized_request",
      "first_prompt_request",
      "first_savings_recorded"
    ]);
  });
});

describe("launcher helpers", () => {
  it("validates trimmed email addresses for auth and contact flows", () => {
    expect(isValidEmailAddress("  user@example.com  ")).toBe(true);
    expect(isValidEmailAddress("missing-at-symbol")).toBe(false);
    expect(isValidEmailAddress("user@example")).toBe(false);
  });

  it("gates on terms acceptance until the accepted version catches up", () => {
    // New install: nothing accepted yet.
    expect(needsTermsAcceptance(1, 0)).toBe(true);
    // Already accepted the current version.
    expect(needsTermsAcceptance(1, 1)).toBe(false);
    // Terms bumped in an update: previously-accepted version is now stale.
    expect(needsTermsAcceptance(2, 1)).toBe(true);
    // Accepted version somehow ahead (downgrade): do not block.
    expect(needsTermsAcceptance(1, 2)).toBe(false);
  });

  it("returns contact validation errors before submit work begins", () => {
    expect(getContactRequestValidationError(undefined, "user@example.com")).toBe(
      "Set VITE_HEADROOM_CONTACT_FORM_URL to enable contact requests."
    );
    expect(getContactRequestValidationError("https://example.com/form", "invalid")).toBe(
      "Enter a valid email address."
    );
    expect(
      getContactRequestValidationError("https://example.com/form", "user@example.com")
    ).toBeNull();
  });

  it("finds the managed Claude connector from mixed connector lists", () => {
    const connectors: ClientConnectorStatus[] = [
      { clientId: "cursor", name: "Cursor", installed: true, enabled: false, verified: false },
      {
        clientId: "claude_code",
        name: "Claude Code",
        installed: true,
        enabled: true,
        verified: true
      }
    ];

    expect(getClaudeConnector(connectors)).toEqual(connectors[1]);
  });

  it("decides whether launcher auto-setup should wait, apply setup, or continue", () => {
    expect(getLauncherAutoConfigureDecision([])).toBe("show_client_setup");
    expect(
      getLauncherAutoConfigureDecision([
        {
          clientId: "claude_code",
          name: "Claude Code",
          installed: true,
          enabled: false,
          verified: false
        }
      ])
    ).toBe("apply_client_setup");
    expect(
      getLauncherAutoConfigureDecision([
        {
          clientId: "claude_code",
          name: "Claude Code",
          installed: true,
          enabled: true,
          verified: false
        }
      ])
    ).toBe("begin_proxy_verification");
    // Codex-only: a non-Claude tool drives the same auto-configure decision.
    expect(
      getLauncherAutoConfigureDecision([
        { clientId: "codex", name: "Codex", installed: true, enabled: false, verified: false }
      ])
    ).toBe("apply_client_setup");
  });

  it("builds initial proxy verification rows from enabled installed Claude connectors", () => {
    const rows = buildInitialProxyVerificationRows([
      { clientId: "cursor", name: "Cursor", installed: true, enabled: true, verified: false },
      {
        clientId: "claude_code",
        name: "Claude Code",
        installed: true,
        enabled: true,
        verified: false
      },
      {
        clientId: "claude_code",
        name: "Claude Code Beta",
        installed: true,
        enabled: false,
        verified: false
      }
    ]);

    expect(rows).toEqual([
      {
        clientId: "claude_code",
        name: "Claude Code",
        state: "processing",
        message: "Waiting for a Claude Code prompt..."
      }
    ]);
  });

  it("checks MCP configuration without waiting for model proxy traffic", () => {
    const zcode = { clientId: "zcode", name: "ZCode", installed: true, enabled: true, verified: true };
    const codex = { clientId: "codex", name: "Codex", installed: true, enabled: true, verified: true };
    expect(buildInitialProxyVerificationRows([zcode])[0].state).toBe("verified");
    expect(buildInitialProxyVerificationRows([{ ...zcode, verified: false }])[0].state).toBe("waiting");
    const mixed = buildInitialProxyVerificationRows([zcode, codex]);
    expect(mixed.find((row) => row.clientId === "codex")?.state).toBe("processing");
    expect(mixed.find((row) => row.clientId === "zcode")?.state).toBe("verified");
  });

  describe("magicLinkScreenCopy", () => {
    it("names the account while verifying", () => {
      expect(magicLinkScreenCopy("verifying", "a@b.com", null).body).toContain("a@b.com");
    });

    it("surfaces the verify error, falling back to a retry hint", () => {
      expect(magicLinkScreenCopy("failed", "a@b.com", "Code expired.").body).toBe(
        "Code expired."
      );
      expect(magicLinkScreenCopy("failed", "a@b.com", null).body).toContain(
        "Request a new sign-in link"
      );
    });
  });

  describe("getInitialLauncherStage", () => {
    it("returns null in non-launcher windows regardless of bootstrap state", () => {
      expect(getInitialLauncherStage("dashboard", true, true, "first_run")).toBeNull();
      expect(getInitialLauncherStage("tray", true, true, "resume")).toBeNull();
    });

    it("returns null in the launcher window until bootstrap is complete", () => {
      expect(getInitialLauncherStage("launcher", false, false, "first_run")).toBeNull();
    });

    it("lands first-run users on install when bootstrap completed during startup", () => {
      expect(getInitialLauncherStage("launcher", true, false, "first_run")).toBe("install");
    });

    it("lands first-run users on install when bootstrap was already complete", () => {
      expect(getInitialLauncherStage("launcher", false, true, "first_run")).toBe("install");
    });

    it("lands returning users on post_install once bootstrap is complete", () => {
      expect(getInitialLauncherStage("launcher", true, true, "resume")).toBe("post_install");
      expect(getInitialLauncherStage("launcher", false, true, "dashboard")).toBe(
        "post_install"
      );
    });
  });

  describe("nextAutoConfigureStep", () => {
    const claude: ClientConnectorStatus = {
      clientId: "claude_code",
      name: "Claude Code",
      installed: true,
      enabled: false,
      verified: false
    };

    const codex: ClientConnectorStatus = {
      clientId: "codex",
      name: "Codex",
      installed: true,
      enabled: false,
      verified: false
    };

    it("routes show_client_setup decisions to manual setup", () => {
      expect(nextAutoConfigureStep("show_client_setup", [claude])).toEqual({
        kind: "show_client_setup"
      });
    });

    it("routes apply_client_setup to an apply step for every installed, not-yet-enabled connector", () => {
      expect(nextAutoConfigureStep("apply_client_setup", [claude, codex])).toEqual({
        kind: "apply",
        clientIds: ["claude_code", "codex"]
      });
    });

    it("only applies connectors that are installed and not already enabled", () => {
      expect(
        nextAutoConfigureStep("apply_client_setup", [
          { ...claude, enabled: true },
          codex,
          { clientId: "codex", name: "Codex", installed: false, enabled: false, verified: false }
        ])
      ).toEqual({ kind: "apply", clientIds: ["codex"] });
    });

    it("falls back to manual setup when apply_client_setup has no detected connector", () => {
      expect(nextAutoConfigureStep("apply_client_setup", [])).toEqual({
        kind: "show_client_setup"
      });
    });

    it("routes begin_proxy_verification straight to proxy verification", () => {
      expect(nextAutoConfigureStep("begin_proxy_verification", [])).toEqual({
        kind: "begin_proxy_verification"
      });
    });
  });

  describe("nextAutoConfigureStepAfterApply", () => {
    it("advances to proxy verification when apply produced a verified setup", () => {
      expect(nextAutoConfigureStepAfterApply("begin_proxy_verification")).toEqual({
        kind: "begin_proxy_verification"
      });
    });

    it("falls back to manual setup when post-apply state still needs attention", () => {
      expect(nextAutoConfigureStepAfterApply("show_client_setup")).toEqual({
        kind: "show_client_setup"
      });
      expect(nextAutoConfigureStepAfterApply("apply_client_setup")).toEqual({
        kind: "show_client_setup"
      });
    });
  });

  describe("recommendedHeadroomTier", () => {
    it("maps Claude tiers directly", () => {
      expect(recommendedHeadroomTier("pro", null)).toBe("pro");
      expect(recommendedHeadroomTier("max5x", null)).toBe("max5x");
      expect(recommendedHeadroomTier("max20x", null)).toBe("max20x");
    });

    it("maps Codex tiers per the models.rs table", () => {
      expect(recommendedHeadroomTier(null, "go")).toBe("pro");
      expect(recommendedHeadroomTier(null, "plus")).toBe("pro");
      // Standard Business/Team seats carry a Plus-level Codex allowance.
      expect(recommendedHeadroomTier(null, "team")).toBe("pro");
      expect(recommendedHeadroomTier(null, "business")).toBe("pro");
      // ChatGPT Pro Lite is ~$100/mo, the Claude Max x5 price point.
      expect(recommendedHeadroomTier(null, "prolite")).toBe("max5x");
      expect(recommendedHeadroomTier(null, "self_serve_business_usage_based")).toBe("max5x");
      expect(recommendedHeadroomTier(null, "edu")).toBe("max5x");
      expect(recommendedHeadroomTier(null, "pro")).toBe("max20x");
      expect(recommendedHeadroomTier(null, "enterprise")).toBe("max20x");
      expect(recommendedHeadroomTier(null, "enterprise_cbp_usage_based")).toBe("max20x");
    });

    it("takes the higher of the two detected tiers", () => {
      expect(recommendedHeadroomTier("pro", "pro")).toBe("max20x");
      expect(recommendedHeadroomTier("max20x", "plus")).toBe("max20x");
      expect(recommendedHeadroomTier("max5x", "go")).toBe("max5x");
    });

    it("defaults to pro when nothing is confidently detected", () => {
      expect(recommendedHeadroomTier(null, null)).toBe("pro");
      expect(recommendedHeadroomTier(undefined, undefined)).toBe("pro");
      expect(recommendedHeadroomTier("free", "free")).toBe("pro");
      expect(recommendedHeadroomTier("unknown", "unknown")).toBe("pro");
    });
  });
});

describe("formatConnectorNameList", () => {
  // Feeds the proxy-verify skip warning, which names the connectors that never
  // checked in. Four can be enabled at once, so the 3+ case is real.
  it("reads naturally at every connector count", () => {
    expect(formatConnectorNameList([])).toBe("");
    expect(formatConnectorNameList(["Claude Code"])).toBe("Claude Code");
    expect(formatConnectorNameList(["Claude Code", "Codex"])).toBe("Claude Code and Codex");
    expect(formatConnectorNameList(["Claude Code", "Codex", "OpenCode"])).toBe(
      "Claude Code, Codex and OpenCode"
    );
  });
});
