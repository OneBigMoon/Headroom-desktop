import { aggregateClientConnectors, connectorUsesProxy } from "./dashboardHelpers";
import type {
  ClaudePlanTier,
  ClientConnectorStatus,
  CodexPlanTier,
  HeadroomSubscriptionTier,
  LaunchExperience,
} from "./types";

export const EMAIL_ADDRESS_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

// Linear onboarding flow shown in the launcher window:
// install → client_setup → proxy_verify → post_install. Back buttons can jump
// backwards. The install step doubles as the pre-install landing.
// Paywall-first experiment (server flag, fresh installs only) reorders to:
// install(landing) → client_setup → proxy_verify(passthrough) → paywall →
// install(bootstrap) → post_install.
export type LauncherStage =
  | "install"
  | "client_setup"
  | "proxy_verify"
  | "paywall"
  | "post_install";

// Canonical install-wizard funnel steps, in order. Single source of truth for
// the per-user drop-off tracking; mirror of DesktopFunnelStep::ORDER in the
// headroom-web repo. Emitted via the `report_funnel_step` Tauri command, which
// piggybacks the name onto POST /desktop/grace/start. Keep the two lists in sync.
export const INSTALL_WIZARD_STEPS = [
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
] as const;

export type InstallWizardStep = (typeof INSTALL_WIZARD_STEPS)[number];

export type LauncherAutoConfigureDecision =
  | "show_client_setup"
  | "apply_client_setup"
  | "begin_proxy_verification";

/// Step the launcher's auto-configure flow should take next, given a fresh
/// connector probe. The component is responsible for performing the IPC
/// calls; this helper isolates the decision logic so it can be unit-tested.
export type AutoConfigureStep =
  | { kind: "show_client_setup" }
  | { kind: "apply"; clientIds: string[] }
  | { kind: "begin_proxy_verification" };

export interface ProxyVerificationRowState {
  clientId: string;
  name: string;
  state: "processing" | "waiting" | "verified";
  message: string;
}

export function isValidEmailAddress(email: string) {
  return EMAIL_ADDRESS_PATTERN.test(email.trim());
}

// Mirrors headroom_tier_for_claude_plan / headroom_tier_for_codex_plan in
// models.rs, with one paywall-specific difference: undetected/free maps to
// "pro" (the paywall always recommends something) instead of None.
export function recommendedHeadroomTier(
  claudeTier: ClaudePlanTier | null | undefined,
  codexTier: CodexPlanTier | null | undefined,
  fallback: HeadroomSubscriptionTier = "pro"
): HeadroomSubscriptionTier {
  const TIER_RANK: Record<HeadroomSubscriptionTier, number> = {
    pro: 1,
    max5x: 2,
    max20x: 3,
  };
  const fromClaude: HeadroomSubscriptionTier | null =
    claudeTier === "pro" ? "pro"
    : claudeTier === "max5x" ? "max5x"
    : claudeTier === "max20x" ? "max20x"
    : null;
  const fromCodex: HeadroomSubscriptionTier | null =
    codexTier === "go" ||
    codexTier === "plus" ||
    codexTier === "team" ||
    codexTier === "business"
      ? "pro"
    : codexTier === "prolite" ||
        codexTier === "self_serve_business_usage_based" ||
        codexTier === "edu"
      ? "max5x"
    : codexTier === "pro" ||
        codexTier === "enterprise" ||
        codexTier === "enterprise_cbp_usage_based"
      ? "max20x"
    : null;
  const candidates = [fromClaude, fromCodex].filter(
    (t): t is HeadroomSubscriptionTier => t !== null
  );
  if (candidates.length === 0) {
    return fallback;
  }
  return candidates.reduce((best, t) =>
    TIER_RANK[t] > TIER_RANK[best] ? t : best
  );
}

/// True when the user must (re-)accept the Terms of Service before using the
/// app: the version the app requires is newer than what they've accepted.
export function needsTermsAcceptance(
  requiredVersion: number,
  acceptedVersion: number
) {
  return acceptedVersion < requiredVersion;
}

export function getContactRequestValidationError(
  contactFormUrl: string | undefined,
  email: string
) {
  if (!contactFormUrl) {
    return "Set VITE_HEADROOM_CONTACT_FORM_URL to enable contact requests.";
  }
  if (!isValidEmailAddress(email)) {
    return "Enter a valid email address.";
  }
  return null;
}

export function getClaudeConnector(connectors: ClientConnectorStatus[]) {
  return (
    aggregateClientConnectors(connectors).find(
      (connector) => connector.clientId === "claude_code"
    ) ?? null
  );
}

export function getLauncherAutoConfigureDecision(
  connectors: ClientConnectorStatus[]
): LauncherAutoConfigureDecision {
  const installed = aggregateClientConnectors(connectors).filter(
    (connector) => connector.installed
  );
  if (installed.length === 0) {
    return "show_client_setup";
  }
  if (installed.some((connector) => !connector.enabled)) {
    return "apply_client_setup";
  }
  return "begin_proxy_verification";
}

/// Copy for the launcher's magic-link screen (headroom://auth). Success has no
/// screen of its own - it drops straight through to the next onboarding step.
export type MagicLinkState = "verifying" | "failed";

export function magicLinkScreenCopy(
  state: MagicLinkState,
  email: string,
  error: string | null
): { title: string; body: string } {
  if (state === "verifying") {
    return { title: "Signing you in…", body: `Finishing sign-in for ${email}.` };
  }
  return {
    title: "That sign-in link did not work",
    body: error ?? "Request a new sign-in link and try again.",
  };
}

/// Given a launcher-window startup result, return the stage the launcher
/// should land on, or `null` to leave the current stage untouched (the caller
/// is in a non-launcher window, or bootstrap hasn't completed yet).
export function getInitialLauncherStage(
  windowLabel: string,
  bootstrapComplete: boolean,
  dashboardBootstrapComplete: boolean,
  launchExperience: LaunchExperience
): LauncherStage | null {
  if (windowLabel !== "launcher") {
    return null;
  }
  if (!bootstrapComplete && !dashboardBootstrapComplete) {
    return null;
  }
  return launchExperience === "first_run" ? "install" : "post_install";
}

/// First step of the launcher's auto-configure flow: decide what to do
/// given a fresh connector probe. Pre-apply only.
export function nextAutoConfigureStep(
  decision: LauncherAutoConfigureDecision,
  connectors: ClientConnectorStatus[]
): AutoConfigureStep {
  if (decision === "show_client_setup") {
    return { kind: "show_client_setup" };
  }
  if (decision === "apply_client_setup") {
    const clientIds = aggregateClientConnectors(connectors)
      .filter((connector) => connector.installed && !connector.enabled)
      .map((connector) => connector.clientId);
    if (clientIds.length === 0) {
      // No connector to apply against — fall back to manual setup.
      return { kind: "show_client_setup" };
    }
    return { kind: "apply", clientIds };
  }
  return { kind: "begin_proxy_verification" };
}

/// Second step of the launcher's auto-configure flow: after the apply IPC
/// resolved, decide whether to advance to proxy verification or bail back to
/// the manual setup screen. Reuses `nextAutoConfigureStep`'s decision branch
/// since the post-apply state is just a re-evaluation of the connector probe.
export function nextAutoConfigureStepAfterApply(
  postApplyDecision: LauncherAutoConfigureDecision
): AutoConfigureStep {
  if (postApplyDecision === "begin_proxy_verification") {
    return { kind: "begin_proxy_verification" };
  }
  return { kind: "show_client_setup" };
}

export function buildInitialProxyVerificationRows(
  connectors: ClientConnectorStatus[]
): ProxyVerificationRowState[] {
  return aggregateClientConnectors(connectors)
    .filter((connector) => connector.enabled && connector.installed)
    .sort((left, right) => left.name.localeCompare(right.name))
    .map((connector) => ({
      clientId: connector.clientId,
      name: connector.name,
      state: connectorUsesProxy(connector.clientId)
        ? "processing"
        : connector.verified ? "verified" : "waiting",
      message: connectorUsesProxy(connector.clientId)
        ? `Waiting for a ${connector.name} prompt...`
        : connector.verified ? "MCP configuration verified." : "MCP configuration needs attention."
    }));
}

/// Join connector names for the skip warning on the proxy-verify step. Up to
/// four connectors can be enabled at once, so a plain `join(" and ")` mangles
/// the 3+ case. `Intl.ListFormat` does this properly but needs an ES2021 lib,
/// and one warning line is not worth raising the whole project's target.
export function formatConnectorNameList(names: string[]): string {
  if (names.length <= 1) {
    return names.join("");
  }
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}
