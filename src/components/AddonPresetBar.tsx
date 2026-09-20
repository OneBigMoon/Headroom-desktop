import { useI18n } from "../lib/i18n";
import type { DashboardState } from "../lib/types";

export type AddonPresetTarget = { enabled: boolean; mode?: string };

export const RECOMMENDED_ADDON_PRESET: Record<string, AddonPresetTarget> = {
  openspec: { enabled: false },
  superpowers: { enabled: true },
  gstack: { enabled: false },
  "allinluna": { enabled: false },
  "ralph-loop": { enabled: false },
  "stop-that-shit": { enabled: true },
  "agent-guard": { enabled: true },
  "codex-security": { enabled: false },
  serena: { enabled: true },
  "codebase-memory": { enabled: true },
  context7: { enabled: true },
  ponytail: { enabled: true, mode: "full" },
  caveman: { enabled: false },
  rtk: { enabled: true },
  markitdown: { enabled: true },
  "grill-me": { enabled: true },
};

function matchesPreset(dashboard: DashboardState): boolean {
  const matches = Object.entries(RECOMMENDED_ADDON_PRESET).every(([id, target]) => {
    const tool = dashboard.tools.find((candidate) => candidate.id === id);
    if (!tool) return false;
    if (target.enabled !== (tool.status !== "not_installed" && tool.enabled)) return false;
    return !target.mode || target.mode === tool.defaultMode;
  });
  return matches && dashboard.tools.every((tool) => tool.required || RECOMMENDED_ADDON_PRESET[tool.id] || !tool.enabled);
}

/**
 * Tools that applying the recommended preset would switch off right now.
 *
 * The preset writes every entry it owns, so it also turns off tools the user
 * enabled by hand - including the active peer of a single-select workflow
 * group. Naming them before the click is the difference between a preset and a
 * silent regression.
 */
export function listPresetDisables(
  dashboard: DashboardState
): Array<{ id: string; name: string }> {
  return dashboard.tools
    .filter((tool) => {
      const target = RECOMMENDED_ADDON_PRESET[tool.id];
      return (
        target !== undefined &&
        !target.enabled &&
        tool.enabled &&
        tool.status !== "not_installed"
      );
    })
    .map((tool) => ({ id: tool.id, name: tool.name }));
}

export function AddonPresetBar({
  dashboard,
  busy,
  mode,
  onApplyRecommended,
  onSelectCustom,
}: {
  dashboard: DashboardState;
  busy: boolean;
  mode: "recommended" | "custom";
  onApplyRecommended: () => void;
  onSelectCustom: () => void;
}) {
  const { t } = useI18n();
  const recommended = matchesPreset(dashboard);
  return (
    <div className="addon-preset-bar" role="group" aria-label={t("aria.addonPresets")}>
      <button
        type="button"
        className={`addon-preset-bar__button${mode === "recommended" && recommended ? " is-active" : ""}`}
        disabled={busy || mode === "recommended"}
        onClick={onApplyRecommended}
        aria-busy={busy}
      >
        {busy ? t("addons.preset.applying") : t("addons.preset.recommended")}
      </button>
      <button
        type="button"
        className={`addon-preset-bar__button${mode === "custom" || !recommended ? " is-active" : ""}`}
        disabled={busy}
        onClick={onSelectCustom}
        aria-pressed={mode === "custom" || !recommended}
      >
        {t("addons.preset.custom")}
      </button>
      <span className="addon-preset-bar__hint">
        {mode === "recommended"
          ? t("addons.preset.lockedHint")
          : t("addons.preset.customHint")}
      </span>
      {busy ? (
        <span className="addon-preset-bar__status">{t("addons.preset.busyStatus")}</span>
      ) : null}
    </div>
  );
}
