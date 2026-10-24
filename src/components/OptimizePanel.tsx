import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "../lib/i18n";
import type { AppliedPatterns, AppliedSection } from "../lib/types";

interface OptimizePanelProps {
  projectPath: string;
  source?: "claude" | "codex";
  // Bump this to force a refetch after an external event (e.g. a Learn run
  // finishes). The value itself is ignored; only changes matter.
  refreshSignal?: number;
  // When provided, the panel uses this slice of applied patterns rather than
  // invoking `list_applied_patterns` itself. Lets the parent batch one IPC
  // across every panel instead of N.
  preloadedApplied?: AppliedPatterns | null;
  // Called after the panel mutates applied patterns (delete) so the parent
  // can refetch the shared aggregate. Only used when preloadedApplied is set.
  onAppliedMutated?: () => void;
  // Learn has never run here, so zero counts mean "not scanned yet" rather
  // than "scanned and found nothing". Collapses the two pills into one.
  neverScanned?: boolean;
  /** Run Learn for this target from the empty-state action. */
  onScan?: () => void;
  scanDisabled?: boolean;
}

type ModalKind = null | "claude" | "memory";
type AppliedFileKind = "claude" | "memory" | "codex_agents" | "codex_instructions";

export function OptimizePanel({
  projectPath,
  source = "claude",
  refreshSignal,
  preloadedApplied,
  onAppliedMutated,
  neverScanned = false,
  onScan,
  scanDisabled = false,
}: OptimizePanelProps) {
  const { t } = useI18n();
  const hasPreloadedApplied = preloadedApplied !== undefined;
  const [applied, setApplied] = useState<AppliedPatterns | null>(
    hasPreloadedApplied ? preloadedApplied ?? null : null,
  );
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busyIds, setBusyIds] = useState<Set<string>>(new Set());
  const [modal, setModal] = useState<ModalKind>(null);
  const closeModal = useCallback(() => setModal(null), []);

  // Reading `hasPreloadedApplied` via a ref inside `refetch` keeps its identity
  // stable when the parent flips from "still loading the aggregate" to
  // "aggregate resolved". Without this, refetch's useCallback dep churn
  // re-fires the refetch effect and causes a duplicate IPC round per panel.
  const hasPreloadedAppliedRef = useRef(hasPreloadedApplied);
  hasPreloadedAppliedRef.current = hasPreloadedApplied;

  useEffect(() => {
    if (hasPreloadedApplied) {
      setApplied(preloadedApplied ?? null);
    }
  }, [hasPreloadedApplied, preloadedApplied]);

  const refetch = useCallback(() => {
    if (hasPreloadedAppliedRef.current) return () => {};
    let active = true;
    invoke<AppliedPatterns>("list_applied_patterns", { projectPath })
      .then((a) => {
        if (!active) return;
        setApplied(a);
        setLoadError(null);
      })
      .catch((err) => {
        if (!active) return;
        setLoadError(err instanceof Error ? err.message : t("messages.localOperationFailed"));
      });
    return () => {
      active = false;
    };
  }, [projectPath, t]);

  useEffect(() => {
    const cancel = refetch();
    return cancel;
  }, [refetch, refreshSignal]);

  const handleDeleteApplied = async (
    fileKind: AppliedFileKind,
    sectionTitle: string,
    bulletText: string,
  ) => {
    const key = `${fileKind}|${sectionTitle}|${bulletText}`;
    setBusyIds((prev) => new Set(prev).add(key));
    try {
      await invoke("delete_applied_pattern", {
        projectPath,
        fileKind,
        sectionTitle,
        bulletText,
      });
      if (hasPreloadedApplied) {
        onAppliedMutated?.();
      } else {
        refetch();
      }
    } catch (err) {
      setLoadError(err instanceof Error ? err.message : t("messages.localOperationFailed"));
    } finally {
      setBusyIds((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  };

  const primarySections = source === "codex"
    ? applied?.codexAgentsMd ?? []
    : applied?.claudeMd ?? [];
  const secondarySections = source === "codex"
    ? applied?.codexInstructionsMd ?? []
    : applied?.memoryMd ?? [];
  const primaryCount = primarySections.reduce((n, s) => n + s.bullets.length, 0);
  const secondaryCount = secondarySections.reduce((n, s) => n + s.bullets.length, 0);
  const primaryDisabled = applied === null || primaryCount === 0;
  const secondaryDisabled = applied === null || secondaryCount === 0;
  const primaryKind: AppliedFileKind = source === "codex" ? "codex_agents" : "claude";
  const secondaryKind: AppliedFileKind = source === "codex" ? "codex_instructions" : "memory";
  if (neverScanned) {
    return (
      <span className="optimize-panel__pills" aria-live="polite">
        <button type="button" className="optimize-panel__pill optimize-panel__pill--empty" disabled>
          {t("optimize.notScanned")}
        </button>
        {onScan ? (
          <button
            type="button"
            className="optimize-panel__pill"
            onClick={onScan}
            disabled={scanDisabled}
          >
            {scanDisabled ? t("actions.scanning") : t("actions.scanNow")}
          </button>
        ) : null}
      </span>
    );
  }

  // A failed first load leaves `applied` null, which disables both pills -- so
  // the modal holding `loadError` can never be opened and the panel reads
  // "0 learnings", indistinguishable from a clean empty scan. Surface a retry.
  // (A refetch failure after a good load still surfaces inside the modal.)
  if (loadError && applied === null) {
    return (
      <span className="optimize-panel__pills">
        <button
          type="button"
          className="optimize-panel__pill optimize-panel__pill--empty"
          onClick={() => {
            setLoadError(null);
            refetch();
          }}
          title={loadError}
        >
          {t("optimize.loadRetry")}
        </button>
      </span>
    );
  }

  return (
    <>
      <span className="optimize-panel__pills">
        <button
          type="button"
          className={`optimize-panel__pill${primaryDisabled ? " optimize-panel__pill--empty" : ""}`}
          onClick={() => setModal("claude")}
          disabled={primaryDisabled}
        >
          {source === "codex"
            ? t(primaryCount === 1 ? "optimize.codexAgentsCountOne" : "optimize.codexAgentsCount", { count: primaryCount })
            : t(primaryCount === 1 ? "optimize.learningCountOne" : "optimize.learningCount", { count: primaryCount })}
        </button>
        <button
          type="button"
          className={`optimize-panel__pill${secondaryDisabled ? " optimize-panel__pill--empty" : ""}`}
          onClick={() => setModal("memory")}
          disabled={secondaryDisabled}
        >
          {source === "codex"
            ? t(secondaryCount === 1 ? "optimize.codexInstructionsCountOne" : "optimize.codexInstructionsCount", { count: secondaryCount })
            : t(secondaryCount === 1 ? "optimize.reminderCountOne" : "optimize.reminderCount", { count: secondaryCount })}
        </button>
      </span>

      {modal === "claude" ? (
        <Modal title={t(source === "codex" ? "optimize.codexAgentsTitle" : "optimize.claudeTitle")} onClose={closeModal}>
          {loadError ? <p className="install-progress__error">{loadError}</p> : null}
          {applied === null ? (
            <p className="optimize-panel__empty">{t("settings.loading")}</p>
          ) : primaryCount === 0 ? (
            <p className="optimize-panel__empty">
              {t(source === "codex" ? "optimize.noCodexAgents" : "optimize.noLearnings")}
            </p>
          ) : (
            <AppliedSections
              fileKind={primaryKind}
              sections={primarySections}
              busyIds={busyIds}
              onDelete={handleDeleteApplied}
            />
          )}
        </Modal>
      ) : null}

      {modal === "memory" ? (
        <Modal title={t(source === "codex" ? "optimize.codexInstructionsTitle" : "optimize.memoryTitle")} onClose={closeModal}>
          {loadError ? <p className="install-progress__error">{loadError}</p> : null}
          {applied === null ? (
            <p className="optimize-panel__empty">{t("settings.loading")}</p>
          ) : secondaryCount === 0 ? (
            <p className="optimize-panel__empty">
              {t(source === "codex" ? "optimize.noCodexInstructions" : "optimize.noReminders")}
            </p>
          ) : (
            <AppliedSections
              fileKind={secondaryKind}
              sections={secondarySections}
              busyIds={busyIds}
              onDelete={handleDeleteApplied}
            />
          )}
        </Modal>
      ) : null}
    </>
  );
}

function Modal({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: React.ReactNode;
}) {
  const { t } = useI18n();
  const titleId = useId();
  const closeButtonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const previousFocus = document.activeElement;
    closeButtonRef.current?.focus();
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) {
        previousFocus.focus();
      }
    };
  }, [onClose]);

  return createPortal(
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      onClick={onClose}
    >
      <div
        className="modal-card optimize-panel__modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="optimize-panel__modal-header">
          <h3 id={titleId}>{title}</h3>
          <button
            ref={closeButtonRef}
            type="button"
            className="optimize-panel__modal-close"
            onClick={onClose}
            aria-label={t("actions.close")}
          >
            ×
          </button>
        </div>
        <div className="optimize-panel__modal-body">{children}</div>
      </div>
    </div>,
    document.body,
  );
}

function AppliedSections({
  fileKind,
  sections,
  busyIds,
  onDelete,
}: {
  fileKind: AppliedFileKind;
  sections: AppliedSection[];
  busyIds: Set<string>;
  onDelete: (
    fileKind: AppliedFileKind,
    sectionTitle: string,
    bulletText: string,
  ) => Promise<void> | void;
}) {
  return (
    <>
      {sections.map((section) => (
        <div className="optimize-panel__applied-section" key={section.title}>
          <h6 className="optimize-panel__applied-section-title">{section.title}</h6>
          <ul className="optimize-panel__list">
            {section.bullets.map((bullet) => {
              const key = `${fileKind}|${section.title}|${bullet}`;
              return (
                <AppliedBullet
                  key={key}
                  bullet={bullet}
                  busy={busyIds.has(key)}
                  onDelete={() => void onDelete(fileKind, section.title, bullet)}
                />
              );
            })}
          </ul>
        </div>
      ))}
    </>
  );
}

function AppliedBullet({
  bullet,
  busy,
  onDelete,
}: {
  bullet: string;
  busy: boolean;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState(false);
  const canExpand = bullet.length > 140 || bullet.includes("\n");
  return (
    <li className="optimize-panel__row">
      <div className="optimize-panel__row-main">
        <p
          className={
            expanded
              ? "activity-feed__content"
              : "activity-feed__content activity-feed__content--clamped"
          }
          title={canExpand && !expanded ? bullet : undefined}
        >
          {bullet}
        </p>
        {canExpand ? (
          <button
            type="button"
            className="activity-feed__expand"
            onClick={() => setExpanded((prev) => !prev)}
          >
            {expanded ? t("actions.showLess") : t("actions.showMore")}
          </button>
        ) : null}
      </div>
      <button
        type="button"
        className="optimize-panel__delete"
        disabled={busy}
        onClick={onDelete}
      >
        {busy ? "…" : t("actions.delete")}
      </button>
    </li>
  );
}
