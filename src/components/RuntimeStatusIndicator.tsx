import { useI18n } from "../lib/i18n";
import type { RuntimeStatus } from "../lib/types";

type RuntimeState = "running" | "error" | "starting" | "stopped";

const stateLabels = {
  running: "proxy.onlineTitle",
  error: "proxy.attentionTitle",
  starting: "proxy.startingTitle",
  stopped: "proxy.offlineTitle",
} as const;

function runtimeState(runtime: RuntimeStatus | null): RuntimeState {
  if (!runtime || runtime.starting) return "starting";
  if (runtime.autoPaused) return "error";
  if (runtime.paused) return "stopped";
  if (runtime.startupError) return "error";
  if (runtime.bypassed || !runtime.installed) return "stopped";
  if (runtime.running !== runtime.proxyReachable) return "error";
  return runtime.running ? "running" : "stopped";
}

export function RuntimeStatusIndicator({ runtime, label, className }: {
  runtime: RuntimeStatus | null;
  label: string;
  className: string;
}) {
  const { t } = useI18n();
  const state = runtimeState(runtime);
  const statusLabel = t(runtime ? stateLabels[state] : "proxy.checkingTitle");

  return (
    <span
      className={`${className} runtime-status-indicator`}
      data-runtime-state={state}
      role="status"
      aria-label={`${label}: ${statusLabel}`}
      title={statusLabel}
    >
      <span className="runtime-status-indicator__dot" aria-hidden="true" />
      <span>{label}</span>
    </span>
  );
}
