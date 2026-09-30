import { useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Bell, WifiSlash } from "@phosphor-icons/react";
import type { ReactNode } from "react";
import { formatDateTime, formatRelativeTime } from "../lib/dashboardHelpers";
import {
  estimateCostSavingsUsd,
  formatEstimatedUsd,
  type ModelInputPriceMap
} from "../lib/modelPricing";
import { useI18n, type TranslationKey } from "../lib/i18n";
import type {
  ActivityFeedResponse,
  ActivityFeedSnapshot,
  LearningsMilestoneEvent,
  RecordEvent,
  RecordTag,
  RtkTodayStats,
  SerenaTodayStats,
  TrainSuggestionEvent,
  TransformationFeedEvent,
  TransformationRequestMessage,
  WeeklyRecapEvent
} from "../lib/types";

interface ActivityFeedProps {
  feed: ActivityFeedResponse;
  error: string | null;
  // True after the first fetch attempt resolves. Before that the `feed`
  // prop is a default placeholder whose `proxyReachable: false` would
  // otherwise render the "proxy unreachable" state on initial load.
  loaded?: boolean;
  // Invoked with the suggested target when a TrainSuggestion row is clicked,
  // so the parent can open Optimize on the matching session source. Left
  // optional for contexts that cannot navigate (tests, embedded previews).
  onNavigateToOptimize?: (projectPath?: string) => void;
  // RTK and Serena are opt-in, so their tiles (and empty placeholders) only
  // render for users who installed them. Data in the slot still wins: stats
  // from a just-uninstalled addon render rather than vanish mid-session.
  rtkInstalled?: boolean;
  serenaInstalled?: boolean;
  modelInputPrices?: ModelInputPriceMap;
}

// One entry per tile kind. `kind` matches the `ActivityFeedSnapshot` slot name
// so the backend shape and frontend render stay in lockstep.
type TileKind = keyof ActivityFeedSnapshot;

const EMPTY_TILE_COPY: Record<
  TileKind,
  { badgeClass: string; badgeLabelKey: TranslationKey; copyKey: TranslationKey; itemModifier: string }
> = {
  trainSuggestion: {
    badgeClass: "activity-feed__badge--train",
    badgeLabelKey: "nav.optimize",
    copyKey: "activity.empty.scan",
    itemModifier: "activity-feed__item--train"
  },
  transformation: {
    badgeClass: "activity-feed__badge--transformation",
    badgeLabelKey: "activity.badge.compression",
    copyKey: "activity.empty.compression",
    itemModifier: "activity-feed__item--transformation"
  },
  rtkToday: {
    badgeClass: "activity-feed__badge--rtk",
    badgeLabelKey: "activity.badge.rtk",
    copyKey: "activity.empty.rtk",
    itemModifier: "activity-feed__item--rtk"
  },
  serenaToday: {
    badgeClass: "activity-feed__badge--serena",
    badgeLabelKey: "activity.badge.serena",
    copyKey: "activity.empty.serena",
    itemModifier: "activity-feed__item--serena"
  },
  record: {
    badgeClass: "activity-feed__badge--record",
    badgeLabelKey: "activity.badge.record",
    copyKey: "activity.empty.record",
    itemModifier: "activity-feed__item--record"
  },
  learningsMilestone: {
    badgeClass: "activity-feed__badge--learnings-milestone",
    badgeLabelKey: "activity.badge.learnings",
    copyKey: "activity.empty.learnings",
    itemModifier: "activity-feed__item--learnings-milestone"
  },
  weeklyRecap: {
    badgeClass: "activity-feed__badge--weekly-recap",
    badgeLabelKey: "activity.badge.weekly",
    copyKey: "activity.empty.weekly",
    itemModifier: "activity-feed__item--weekly-recap"
  }
};

export function ActivityFeed({
  feed,
  error,
  loaded = true,
  onNavigateToOptimize,
  rtkInstalled = false,
  serenaInstalled = false,
  modelInputPrices = {}
}: ActivityFeedProps) {
  const { t } = useI18n();
  const { tiles } = feed;
  const learningsMilestone = tiles.learningsMilestone
    && (
      tiles.learningsMilestone.patternsToday > 0
      || tiles.learningsMilestone.remindersToday > 0
      || tiles.learningsMilestone.learningsToday > 0
    )
    ? tiles.learningsMilestone
    : null;
  // "Waiting for proxy" only fires when we've got nothing to show AND the
  // proxy isn't answering. If any slot is populated (persisted state from a
  // prior session), fall through to render it even while the proxy is down.
  const hasAnyTile = Object.values({ ...tiles, learningsMilestone }).some((v) => v != null);

  return (
    <>
      <article className="soft-card activity-card">
        <header className="activity-card__head">
          <div className="activity-card__title-row">
            <span className="activity-card__title-icon" aria-hidden="true">
              <Bell weight="duotone" />
            </span>
            <h1>{t("activity.title")}</h1>
          </div>
          <p className="activity-card__blurb">
            {t("activity.description")}
          </p>
        </header>
      </article>
      {error ? (
        <p className="loading-copy">{error}</p>
      ) : !loaded ? (
        <div className="activity-feed__skeleton" aria-hidden="true">
          <div className="activity-feed__skeleton-row" />
          <div className="activity-feed__skeleton-row" />
          <div className="activity-feed__skeleton-row" />
        </div>
      ) : !feed.proxyReachable && !hasAnyTile ? (
        <div className="activity-feed__empty">
          <div className="activity-feed__empty-icon activity-feed__empty-icon--waiting" aria-hidden="true">
            <WifiSlash weight="duotone" />
          </div>
          <p className="activity-feed__empty-title">{t("activity.waitingProxy")}</p>
          <p className="activity-feed__empty-body">
            {t("activity.reconnect")}
          </p>
        </div>
      ) : (
        <ul className="activity-feed__list">
          {tiles.record ? (
            <RecordRow event={tiles.record} modelInputPrices={modelInputPrices} />
          ) : (
            <EmptyTile kind="record" />
          )}
          {tiles.transformation ? (
            <TransformationRow
              event={tiles.transformation}
              modelInputPrices={modelInputPrices}
            />
          ) : (
            <EmptyTile kind="transformation" />
          )}
          {learningsMilestone ? (
            <LearningsMilestoneRow event={learningsMilestone} />
          ) : (
            <EmptyTile kind="learningsMilestone" />
          )}
          {tiles.trainSuggestion ? (
            <TrainSuggestionRow
              event={tiles.trainSuggestion}
              onNavigate={onNavigateToOptimize}
            />
          ) : (
            <EmptyTile kind="trainSuggestion" onNavigateToOptimize={onNavigateToOptimize} />
          )}
          {tiles.rtkToday ? (
            <RtkTodayRow event={tiles.rtkToday} />
          ) : rtkInstalled ? (
            <EmptyTile kind="rtkToday" />
          ) : null}
          {tiles.serenaToday ? (
            <SerenaTodayRow event={tiles.serenaToday} />
          ) : serenaInstalled ? (
            <EmptyTile kind="serenaToday" />
          ) : null}
          {tiles.weeklyRecap ? (
            <WeeklyRecapRow event={tiles.weeklyRecap} />
          ) : (
            <EmptyTile kind="weeklyRecap" />
          )}
        </ul>
      )}
    </>
  );
}

function EmptyTile({
  kind,
  onNavigateToOptimize
}: {
  kind: TileKind;
  onNavigateToOptimize?: (projectPath?: string) => void;
}) {
  const { t } = useI18n();
  const { badgeClass, badgeLabelKey, copyKey, itemModifier } = EMPTY_TILE_COPY[kind];
  const itemClass = `activity-feed__item activity-feed__item--empty ${itemModifier}`;
  const canNavigate = kind === "trainSuggestion" && typeof onNavigateToOptimize === "function";
  const handleActivate = () => {
    if (canNavigate) onNavigateToOptimize?.();
  };
  /* v8 ignore start — keyboard activation requires a DOM; see ExpandableRow. */
  const onKeyDown = (e: ReactKeyboardEvent<HTMLLIElement>) => {
    if (!canNavigate) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      handleActivate();
    }
  };
  /* v8 ignore stop */
  return (
    <li
      className={canNavigate ? `${itemClass} activity-feed__item--clickable` : itemClass}
      role={canNavigate ? "button" : undefined}
      tabIndex={canNavigate ? 0 : undefined}
      onClick={canNavigate ? handleActivate : undefined}
      onKeyDown={onKeyDown}
    >
      <div className="activity-feed__row activity-feed__row--meta">
        <span className={`activity-feed__badge ${badgeClass} activity-feed__badge--empty`}>
          {t(badgeLabelKey)}
        </span>
      </div>
      <p className="activity-feed__content activity-feed__content--empty">{t(copyKey)}</p>
    </li>
  );
}

/**
 * Wraps a feed row and toggles an expanded detail block below the main
 * content when clicked. No-op when `detail` is null — the row renders
 * non-clickable and the caller just gets a plain `<li>` wrapper.
 */
function ExpandableRow({
  className,
  detail,
  children
}: {
  className: string;
  detail: ReactNode | null;
  children: ReactNode;
}) {
  const [expanded, setExpanded] = useState(false);
  const canExpand = detail != null;
  /* v8 ignore start — interactive handlers require a DOM; SSR tests can pin
     role/aria/class but cannot dispatch click or keyboard events. Same reason
     OptimizePanel.tsx is excluded from coverage entirely. */
  const toggle = () => {
    if (canExpand) setExpanded((prev) => !prev);
  };
  const onKeyDown = (e: ReactKeyboardEvent<HTMLLIElement>) => {
    if (!canExpand) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      toggle();
    }
  };
  /* v8 ignore stop */
  return (
    <li
      className={
        className +
        (canExpand ? " activity-feed__item--clickable" : "") +
        (expanded ? " is-expanded" : "")
      }
      role={canExpand ? "button" : undefined}
      tabIndex={canExpand ? 0 : undefined}
      aria-expanded={canExpand ? expanded : undefined}
      onClick={toggle}
      onKeyDown={onKeyDown}
    >
      {children}
      {expanded && detail ? (
        <div className="activity-feed__detail">{detail}</div>
      ) : null}
    </li>
  );
}

function TimeChip({ iso }: { iso: string | null | undefined }) {
  const { resolvedLocale } = useI18n();
  const relative = resolvedLocale === "en" ? formatRelativeTime(iso) : formatRelativeTimeForLocale(iso, resolvedLocale);
  return (
    <span className="activity-feed__time" title={formatDateTime(iso)}>
      {relative}
    </span>
  );
}

function formatRelativeTimeForLocale(iso: string | null | undefined, locale: string): string {
  if (!iso) return "";
  const timestamp = Date.parse(iso);
  if (Number.isNaN(timestamp)) return "";
  const deltaSeconds = Math.round((timestamp - Date.now()) / 1000);
  const absoluteSeconds = Math.abs(deltaSeconds);
  const formatter = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  if (absoluteSeconds < 60) return formatter.format(deltaSeconds, "second");
  if (absoluteSeconds < 3600) return formatter.format(Math.round(deltaSeconds / 60), "minute");
  if (absoluteSeconds < 86400) return formatter.format(Math.round(deltaSeconds / 3600), "hour");
  return formatter.format(Math.round(deltaSeconds / 86400), "day");
}

function workspaceBasename(path: string | null | undefined): string | null {
  if (!path) return null;
  const segments = path.split("/").filter(Boolean);
  return segments.length > 0 ? segments[segments.length - 1] : null;
}

// Extract displayable text from a single proxy-logged message. Anthropic sends
// `content` as a block list (text / tool_use / tool_result / ...); OpenAI sends
// a plain string. Text blocks are flattened verbatim; other block types are
// shown as a short `[type]` marker rather than dropped, so the reader sees
// that something was there.
function messageText(msg: TransformationRequestMessage): string {
  return flattenContent(msg.content);
}

// Flatten a message/block `content` value to text. `tool_result` blocks carry
// their (compressible) payload in `block.content` — a string or a nested block
// list — not in `block.text`, so without recursing here both the original and
// compressed sides render as a bare `[tool_result]` and the diff sees no change.
function flattenContent(c: unknown): string {
  if (typeof c === "string") return c;
  if (!Array.isArray(c)) return "";
  return c
    .map((block) => {
      if (!block || typeof block !== "object") return "";
      const b = block as { type?: unknown; text?: unknown; content?: unknown };
      if (typeof b.text === "string") return b.text;
      if (b.content !== undefined) {
        const inner = flattenContent(b.content);
        if (inner.length > 0) return inner;
      }
      if (typeof b.type === "string") return `[${b.type}]`;
      return "";
    })
    .filter((s) => s.length > 0)
    .join("\n");
}

export function formatRequestMessages(messages: TransformationRequestMessage[]): string {
  return messages
    .map((m) => {
      const role = (m.role ?? "").trim() || "(unknown)";
      return `${role}:\n${messageText(m)}`;
    })
    .join("\n\n");
}

export type DiffLine = { type: "same" | "add" | "del"; text: string };

// LCS line diff. ponytail: O(n*m) flat Uint16 table. Large compressions — the
// whole point of this view — routinely run to thousands of lines, so the cap is
// on the cell product (memory), not per-side line count. ~30M cells = 60MB,
// computed once on expand. Over that we fall back to side-by-side dumps.
// Upgrade to Hirschberg/Myers if the cap ever bites.
const MAX_DIFF_CELLS = 30_000_000;

export function diffLines(a: string, b: string): DiffLine[] | null {
  const oldL = a.split("\n");
  const newL = b.split("\n");
  const n = oldL.length;
  const m = newL.length;
  // Uint16 caps LCS values at 65535; the cell cap keeps n,m well under that.
  if ((n + 1) * (m + 1) > MAX_DIFF_CELLS) return null;
  const w = m + 1;
  const dp = new Uint16Array((n + 1) * w);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i * w + j] =
        oldL[i] === newL[j]
          ? dp[(i + 1) * w + (j + 1)] + 1
          : Math.max(dp[(i + 1) * w + j], dp[i * w + (j + 1)]);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (oldL[i] === newL[j]) {
      out.push({ type: "same", text: oldL[i] });
      i++;
      j++;
    } else if (dp[(i + 1) * w + j] >= dp[i * w + (j + 1)]) {
      out.push({ type: "del", text: oldL[i] });
      i++;
    } else {
      out.push({ type: "add", text: newL[j] });
      j++;
    }
  }
  while (i < n) out.push({ type: "del", text: oldL[i++] });
  while (j < m) out.push({ type: "add", text: newL[j++] });
  return out;
}

export type CollapsedDiffLine = DiffLine | { type: "skip"; text: string };

// Keep `context` unchanged lines around each change and collapse the rest, so
// the removed (red) / added (green) lines are visible the moment the row opens
// instead of buried under hundreds of identical context lines.
const DIFF_CONTEXT = 3;

export function collapseDiff(diff: DiffLine[], context = DIFF_CONTEXT): CollapsedDiffLine[] {
  const keep = new Array(diff.length).fill(false);
  for (let i = 0; i < diff.length; i++) {
    if (diff[i].type === "same") continue;
    for (let j = Math.max(0, i - context); j <= Math.min(diff.length - 1, i + context); j++) {
      keep[j] = true;
    }
  }
  const out: CollapsedDiffLine[] = [];
  let i = 0;
  while (i < diff.length) {
    if (diff[i].type !== "same" || keep[i]) {
      out.push(diff[i]);
      i++;
      continue;
    }
    let j = i;
    while (j < diff.length && diff[j].type === "same" && !keep[j]) j++;
    const n = j - i;
    out.push({ type: "skip", text: `... ${n} unchanged line${n === 1 ? "" : "s"}` });
    i = j;
  }
  return out;
}

// Unified line diff of original vs compressed request bodies, so pruned content
// (red) and inserted truncation markers (green) pop instead of two near-identical
// dumps. Returns dt/dd fragment for the detail grid. Shared by the transformation
// and record rows.
function CompressionDiff({
  requestMessages,
  compressedMessages,
  inputTokensOriginal,
  inputTokensOptimized
}: {
  requestMessages: TransformationRequestMessage[];
  compressedMessages: TransformationRequestMessage[];
  inputTokensOriginal?: number | null;
  inputTokensOptimized?: number | null;
}) {
  const { t } = useI18n();
  const original = formatRequestMessages(requestMessages);
  const compressed = formatRequestMessages(compressedMessages);
  const diff = diffLines(original, compressed);
  if (!diff) {
    // Too large to diff — fall back to side-by-side dumps.
    return (
      <>
        <dt>{t("activity.detail.requestOriginal")}</dt>
        <dd>
          <pre className="activity-feed__message-dump">{original}</pre>
        </dd>
        <dt>{t("activity.detail.requestCompressed")}</dt>
        <dd>
          <pre className="activity-feed__message-dump">{compressed}</pre>
        </dd>
      </>
    );
  }
  return (
    <>
      <dt>
        {t("activity.detail.compressionDiff")}
        {inputTokensOriginal != null && inputTokensOptimized != null
          ? ` (${inputTokensOriginal.toLocaleString()} → ${inputTokensOptimized.toLocaleString()} tokens)`
          : ""}
      </dt>
      <dd>
        <pre className="activity-feed__message-dump activity-feed__diff">
          {collapseDiff(diff).map((line, idx) => (
            <div
              key={idx}
              className={`activity-feed__diff-line activity-feed__diff-line--${line.type}`}
            >
              {line.type === "del"
                ? "- "
                : line.type === "add"
                  ? "+ "
                  : line.type === "skip"
                    ? ""
                    : "  "}
              {line.text}
            </div>
          ))}
        </pre>
      </dd>
    </>
  );
}

// Backends predating the upstream denominator fix (headroom#3106) measure
// tokensSaved across all layers (tool schemas included) but count "in" over
// messages only, so on schema-heavy turns "out" clamps to 0 and the percent
// tops 100 ("4,436 -> 0, 208.4%"). Those two displays are meaningless for
// such records - suppress them; the saved count itself is real and stays.
function isClampedTokenPair(
  saved: number,
  original: number | null | undefined,
  optimized: number | null | undefined
): boolean {
  return optimized === 0 && original != null && saved > original;
}

function TransformationRow({
  event,
  modelInputPrices
}: {
  event: TransformationFeedEvent;
  modelInputPrices: ModelInputPriceMap;
}) {
  const { t } = useI18n();
  const saved = event.tokensSaved ?? 0;
  const pct = event.savingsPercent ?? 0;
  const showPct = pct <= 100;
  const workspace = workspaceBasename(event.workspace);
  const hasExactTokens =
    event.inputTokensOriginal != null &&
    event.inputTokensOptimized != null &&
    !isClampedTokenPair(saved, event.inputTokensOriginal, event.inputTokensOptimized);
  const hasRequestId = !!event.requestId;
  const hasRawTransforms = event.transformsApplied.length > 0;
  const groups = hasRawTransforms ? groupTransforms(event.transformsApplied) : [];
  const groupsWithTargets = groups.filter((g) => g.targets.length > 0);
  const estimatedUsd = estimateCostSavingsUsd(event.model, saved, modelInputPrices);
  const hasRequestMessages = !!event.requestMessages && event.requestMessages.length > 0;
  const hasCompressedMessages =
    !!event.compressedMessages && event.compressedMessages.length > 0;
  const hasExtra =
    hasRequestId ||
    hasRawTransforms ||
    event.workspace != null ||
    estimatedUsd != null ||
    hasRequestMessages ||
    hasCompressedMessages;
  const detail = hasExtra ? (
    <dl className="activity-feed__detail-grid">
      {estimatedUsd != null ? (
        <>
          <dt>{t("activity.detail.estimatedSaved")}</dt>
          <dd>{formatEstimatedUsd(estimatedUsd)}</dd>
        </>
      ) : null}
      {hasExactTokens ? (
        <>
          <dt>{t("activity.detail.tokensInOut")}</dt>
          <dd>
            {event.inputTokensOriginal!.toLocaleString()} →{" "}
            {event.inputTokensOptimized!.toLocaleString()}
          </dd>
        </>
      ) : null}
      {groupsWithTargets.length > 0 ? (
        <>
          <dt>{t("activity.detail.touched")}</dt>
          <dd>
            <ul className="activity-feed__targets">
              {groupsWithTargets.map((grp) => (
                <li key={grp.label} className="activity-feed__target">
                  <span className="activity-feed__target-label">{localizeTransformLabel(t, grp.label)}</span>
                  <span className="activity-feed__target-values">
                    {grp.targets.join(", ")}
                  </span>
                </li>
              ))}
            </ul>
          </dd>
        </>
      ) : null}
      {event.workspace ? (
        <>
          <dt>{t("activity.detail.workspace")}</dt>
          <dd className="activity-feed__detail-mono">{event.workspace}</dd>
        </>
      ) : null}
      {hasRequestId ? (
        <>
          <dt>{t("activity.detail.requestId")}</dt>
          <dd className="activity-feed__detail-mono">{event.requestId}</dd>
        </>
      ) : null}
      {hasRequestMessages && hasCompressedMessages ? (
        <CompressionDiff
          requestMessages={event.requestMessages!}
          compressedMessages={event.compressedMessages!}
          inputTokensOriginal={hasExactTokens ? event.inputTokensOriginal : null}
          inputTokensOptimized={hasExactTokens ? event.inputTokensOptimized : null}
        />
      ) : hasRequestMessages ? (
        // Legacy proxy shape: only `requestMessages` exists. Its content may
        // actually be the post-compression list (field was inconsistent
        // across sites before the upstream split) — we can't tell, so label
        // it neutrally and keep today's behaviour.
        <>
          <dt>{t("activity.detail.request")}</dt>
          <dd>
            <pre className="activity-feed__message-dump">
              {formatRequestMessages(event.requestMessages!)}
            </pre>
          </dd>
        </>
      ) : null}
    </dl>
  ) : null;
  return (
    <ExpandableRow
      className="activity-feed__item activity-feed__item--transformation"
      detail={detail}
    >
      <div className="activity-feed__row activity-feed__row--meta">
        <span className="activity-feed__badge activity-feed__badge--transformation">
          {t("activity.badge.compression")}
        </span>
        <TimeChip iso={event.timestamp} />
        {event.model ? <span className="activity-feed__model">{event.model}</span> : null}
        {workspace ? (
          <span className="activity-feed__project">{workspace}</span>
        ) : null}
      </div>
      <div className="activity-feed__row activity-feed__row--savings">
        <strong className="activity-feed__savings">
          Saved {saved.toLocaleString()} tokens
          {showPct ? ` (${pct.toFixed(1)}%)` : ""}
          {estimatedUsd != null ? (
            <span className="activity-feed__savings-usd">
              {" "}
              · {formatEstimatedUsd(estimatedUsd)}
            </span>
          ) : null}
        </strong>
        {hasExactTokens ? (
          <span className="activity-feed__delta">
            {event.inputTokensOriginal!.toLocaleString()} →{" "}
            {event.inputTokensOptimized!.toLocaleString()}
          </span>
        ) : null}
      </div>
      {hasRawTransforms ? (
        <ul className="activity-feed__transforms">
          {groups.map((grp) => (
            <li
              key={grp.label}
              className="activity-feed__transform"
              title={chipTitle(t, { ...grp, label: localizeTransformLabel(t, grp.label), title: localizeTransformLabel(t, grp.title) })}
            >
              {grp.count > 1 ? `${localizeTransformLabel(t, grp.label)} × ${grp.count}` : localizeTransformLabel(t, grp.label)}
            </li>
          ))}
        </ul>
      ) : null}
    </ExpandableRow>
  );
}

function localizeTransformLabel(
  t: (key: TranslationKey, values?: Record<string, string | number>) => string,
  value: string,
): string {
  const exact: Record<string, TranslationKey> = {
    "Stale Read": "activity.transform.staleRead",
    "Superseded Read": "activity.transform.supersededRead",
    "Tool result excluded": "activity.transform.toolExcluded",
    "Cache aligned": "activity.transform.cacheAligned",
    "file edited after read": "activity.transform.fileEdited",
    "file re-read later": "activity.transform.fileReread",
    "tool output dropped": "activity.transform.toolDropped",
    "aligned to cache boundary": "activity.transform.cacheBoundary",
    "Protected: user message": "activity.transform.protectedUser",
    "Protected: system message": "activity.transform.protectedSystem",
    "Protected: recent code": "activity.transform.protectedCode",
    "Protected: analysis context": "activity.transform.protectedAnalysis",
    "user message preserved": "activity.transform.userPreserved",
    "system message preserved": "activity.transform.systemPreserved",
    "recent code preserved": "activity.transform.codePreserved",
    "analysis preserved": "activity.transform.analysisPreserved",
    "semantic code search": "activity.transform.semanticSearch",
    "tool outputs compacted": "activity.transform.outputsCompacted",
  };
  if (exact[value]) return t(exact[value]);
  const crushed = value.match(/^Crushed (\d+) tools?$/);
  if (crushed) return t("activity.transform.crushedTools", { count: crushed[1] });
  const breakpoints = value.match(/^Inserted (\d+) cache breakpoints?$/);
  if (breakpoints) return t("activity.transform.cacheBreakpoints", { count: breakpoints[1] });
  return value;
}

function chipTitle(
  t: (key: TranslationKey, values?: Record<string, string | number>) => string,
  grp: TransformGroup,
): string {
  const base = grp.count > 1 ? `${grp.title} (×${grp.count})` : grp.title;
  if (grp.targets.length === 0) return base;
  const preview = grp.targets.slice(0, 3).join(", ");
  const suffix =
    grp.targets.length > 3
      ? `${preview}, ${t("activity.transform.more", { count: grp.targets.length - 3 })}`
      : preview;
  return `${base} — ${suffix}`;
}

/**
 * Collapses a transformsApplied list into one entry per friendly label with a
 * count. A single compression that fires 70 "Stale Read"s renders as one
 * "Stale Read × 70" chip instead of 70 identical chips flooding the row.
 * Preserves first-seen order so the display is stable.
 */
export function groupTransforms(
  raws: string[]
): TransformGroup[] {
  const byLabel = new Map<string, TransformGroup>();
  for (const raw of raws) {
    if (raw === "router:noop") continue;
    const { label, title, target } = formatTransform(raw);
    const existing = byLabel.get(label);
    if (existing) {
      existing.count += 1;
      if (target && !existing.targets.includes(target)) {
        existing.targets.push(target);
      }
    } else {
      byLabel.set(label, {
        label,
        title,
        count: 1,
        targets: target ? [target] : []
      });
    }
  }
  return Array.from(byLabel.values());
}

export interface TransformGroup {
  label: string;
  title: string;
  count: number;
  // Accumulated across all raws that mapped to this label (file paths for
  // read_lifecycle, tool names for tool_crush, etc.). Empty for transforms
  // emitted by older proxies that don't carry attribution, and for
  // transforms that have no meaningful target (e.g. cache_align).
  targets: string[];
}

function formatTransform(raw: string): { label: string; title: string; target?: string } {
  // Exact-match table for known labels (covers older proxies that emit the
  // un-enriched form, and transforms that have no parameter tail).
  const exact: Record<string, { label: string; title: string }> = {
    "read_lifecycle:stale": { label: "Stale Read", title: "file edited after read" },
    "read_lifecycle:superseded": { label: "Superseded Read", title: "file re-read later" },
    "interceptor:ast-grep": { label: "ast-grep", title: "semantic code search" },
    "router:excluded:tool": { label: "Tool result excluded", title: "tool output dropped" },
    "router:protected:user_message": {
      label: "Protected: user message",
      title: "user message preserved"
    },
    "router:protected:system_message": {
      label: "Protected: system message",
      title: "system message preserved"
    },
    "router:protected:recent_code": {
      label: "Protected: recent code",
      title: "recent code preserved"
    },
    "router:protected:analysis_context": {
      label: "Protected: analysis context",
      title: "analysis preserved"
    },
    cache_align: { label: "Cache aligned", title: "aligned to cache boundary" }
  };

  const hit = exact[raw];
  if (hit) return hit;

  // read_lifecycle:<state>:<file_path> — new enriched form from upstream PR.
  // Bound the split to 3 parts so paths containing `:` survive.
  if (raw.startsWith("read_lifecycle:")) {
    const parts = splitColonN(raw, 3);
    if (parts.length === 3 && parts[2]) {
      const state = parts[1];
      const target = parts[2];
      if (state === "stale") {
        return { label: "Stale Read", title: "file edited after read", target };
      }
      if (state === "superseded") {
        return { label: "Superseded Read", title: "file re-read later", target };
      }
    }
  }

  // tool_crush:<n>[:<name1,name2,...>]. The tail may be absent when the proxy
  // couldn't resolve tool names (legacy shape) — fall back to the count-only
  // label.
  const crushWithNames = /^tool_crush:(\d+):(.+)$/.exec(raw);
  if (crushWithNames) {
    const n = Number(crushWithNames[1]);
    return {
      label: `Crushed ${n} tool${n === 1 ? "" : "s"}`,
      title: "tool outputs compacted",
      target: crushWithNames[2]
    };
  }
  const crush = /^tool_crush:(\d+)$/.exec(raw);
  if (crush) {
    const n = Number(crush[1]);
    return {
      label: `Crushed ${n} tool${n === 1 ? "" : "s"}`,
      title: "tool outputs compacted"
    };
  }

  // smart_crush:<n>[:<name1,name2,...>] — the current proxy tag (SmartCrusher
  // replaced the retired ToolCrusher). Same shape/labels as tool_crush above,
  // which is kept for older activity records.
  const smartCrushWithNames = /^smart_crush:(\d+):(.+)$/.exec(raw);
  if (smartCrushWithNames) {
    const n = Number(smartCrushWithNames[1]);
    return {
      label: `Crushed ${n} tool${n === 1 ? "" : "s"}`,
      title: "tool outputs compacted",
      target: smartCrushWithNames[2]
    };
  }
  const smartCrush = /^smart_crush:(\d+)$/.exec(raw);
  if (smartCrush) {
    const n = Number(smartCrush[1]);
    return {
      label: `Crushed ${n} tool${n === 1 ? "" : "s"}`,
      title: "tool outputs compacted"
    };
  }

  const breakpoints = /^inserted_(\d+)_cache_breakpoints$/.exec(raw);
  if (breakpoints) {
    const n = Number(breakpoints[1]);
    return {
      label: `Inserted ${n} cache breakpoint${n === 1 ? "" : "s"}`,
      title: "cache prefix tuned"
    };
  }

  const routerTool = /^router:tool_result:(.+)$/.exec(raw);
  if (routerTool) {
    return { label: `Tool result: ${routerTool[1]}`, title: "tool result compressed" };
  }

  const routerRatio = /^router:([^:]+):([\d.]+)$/.exec(raw);
  if (routerRatio) {
    return {
      label: `Compressed: ${routerRatio[1]} (${routerRatio[2]}x)`,
      title: "router compression"
    };
  }

  const kompress = /^kompress:([^:]+):([\d.]+)$/.exec(raw);
  if (kompress) {
    return {
      label: `Kompress ${kompress[1]} (${kompress[2]}x)`,
      title: "kompress compression"
    };
  }

  const cacheOpt = /^cache_optimizer:(.+)$/.exec(raw);
  if (cacheOpt) {
    return { label: `Cache optimizer: ${cacheOpt[1]}`, title: "cache tuning" };
  }

  // Unknown transform — render verbatim, tooltip shows the raw id.
  return { label: raw, title: raw };
}

function splitColonN(s: string, parts: number): string[] {
  if (parts <= 1) return [s];
  const result: string[] = [];
  let cursor = 0;
  for (let i = 0; i < parts - 1; i++) {
    const idx = s.indexOf(":", cursor);
    if (idx === -1) {
      result.push(s.slice(cursor));
      return result;
    }
    result.push(s.slice(cursor, idx));
    cursor = idx + 1;
  }
  result.push(s.slice(cursor));
  return result;
}

function RtkTodayRow({ event }: { event: RtkTodayStats }) {
  const { t } = useI18n();
  return (
    <li className="activity-feed__item activity-feed__item--rtk">
      <div className="activity-feed__row activity-feed__row--meta">
        <span className="activity-feed__badge activity-feed__badge--rtk">RTK</span>
      </div>
      <div className="activity-feed__row activity-feed__row--savings">
        <strong className="activity-feed__savings">
          {t("activity.rtk.savedToday", { count: event.savedTokens.toLocaleString() })}
        </strong>
        <span className="activity-feed__delta">
          {t("activity.rtk.commands", { count: event.commands.toLocaleString() })}
        </span>
      </div>
    </li>
  );
}

function SerenaTodayRow({ event }: { event: SerenaTodayStats }) {
  const { t } = useI18n();
  // The backend guarantees at least one line; calls lead when present.
  const strong = localizeSerenaLine(event.callsLine ?? event.tokensLine, t);
  const delta = event.callsLine ? localizeSerenaLine(event.tokensLine, t) : null;
  return (
    <li className="activity-feed__item activity-feed__item--serena">
      <div className="activity-feed__row activity-feed__row--meta">
        <span className="activity-feed__badge activity-feed__badge--serena">Serena</span>
      </div>
      <div className="activity-feed__row activity-feed__row--savings">
        <strong className="activity-feed__savings">{strong}</strong>
        {delta ? <span className="activity-feed__delta">{delta}</span> : null}
      </div>
    </li>
  );
}

function localizeSerenaLine(
  line: string | null,
  t: (key: TranslationKey, values?: Record<string, string | number>) => string,
): string | null {
  if (!line) return null;
  const calls = line.match(/^([\d,.]+) tool calls today$/i);
  if (calls) return t("activity.serena.callsToday", { count: calls[1] });
  const tokens = line.match(/^~?([^ ]+) tokens returned in (.+)$/i);
  if (tokens) return t("activity.serena.tokensReturned", { count: tokens[1], duration: tokens[2] });
  return line;
}

const RECORD_TAG_ORDER: RecordTag[] = ["daily", "weekly", "allTime"];

const RECORD_TAG_KEY: Record<RecordTag, TranslationKey> = {
  daily: "activity.tag.daily",
  weekly: "activity.tag.weekly",
  allTime: "activity.tag.allTime"
};

function RecordRow({
  event,
  modelInputPrices
}: {
  event: RecordEvent;
  modelInputPrices: ModelInputPriceMap;
}) {
  const { t } = useI18n();
  const workspace = workspaceBasename(event.workspace);
  const pct = event.savingsPercent;
  const orderedTags = RECORD_TAG_ORDER.filter((tag) => event.tags.includes(tag));
  const hasRequestMessages = !!event.requestMessages && event.requestMessages.length > 0;
  const hasCompressedMessages =
    !!event.compressedMessages && event.compressedMessages.length > 0;
  const hasExactTokens =
    event.inputTokensOriginal != null &&
    event.inputTokensOptimized != null &&
    !isClampedTokenPair(
      event.tokensSaved,
      event.inputTokensOriginal,
      event.inputTokensOptimized
    );
  const hasRequestId = !!event.requestId;
  const estimatedUsd = estimateCostSavingsUsd(
    event.model,
    event.tokensSaved,
    modelInputPrices
  );
  const hasExtra =
    estimatedUsd != null ||
    hasExactTokens ||
    hasRequestId ||
    hasRequestMessages ||
    hasCompressedMessages;
  const detail = hasExtra ? (
    <dl className="activity-feed__detail-grid">
      {estimatedUsd != null ? (
        <>
          <dt>{t("activity.detail.estimatedSaved")}</dt>
          <dd>{formatEstimatedUsd(estimatedUsd)}</dd>
        </>
      ) : null}
      {hasExactTokens ? (
        <>
          <dt>{t("activity.detail.tokensInOut")}</dt>
          <dd>
            {event.inputTokensOriginal!.toLocaleString()} →{" "}
            {event.inputTokensOptimized!.toLocaleString()}
          </dd>
        </>
      ) : null}
      {hasRequestId ? (
        <>
          <dt>{t("activity.detail.requestId")}</dt>
          <dd className="activity-feed__detail-mono">{event.requestId}</dd>
        </>
      ) : null}
      {hasRequestMessages && hasCompressedMessages ? (
        <CompressionDiff
          requestMessages={event.requestMessages!}
          compressedMessages={event.compressedMessages!}
          inputTokensOriginal={hasExactTokens ? event.inputTokensOriginal : null}
          inputTokensOptimized={hasExactTokens ? event.inputTokensOptimized : null}
        />
      ) : hasRequestMessages ? (
        <>
          <dt>{t("activity.detail.request")}</dt>
          <dd>
            <pre className="activity-feed__message-dump">
              {formatRequestMessages(event.requestMessages!)}
            </pre>
          </dd>
        </>
      ) : null}
    </dl>
  ) : null;
  return (
    <ExpandableRow
      className="activity-feed__item activity-feed__item--record"
      detail={detail}
    >
      <div className="activity-feed__row activity-feed__row--meta">
        <span className="activity-feed__badge activity-feed__badge--record">{t("activity.badge.record")}</span>
        {orderedTags.map((tag) => (
          <span
            key={tag}
            className={`activity-feed__tag activity-feed__tag--${tag}`}
          >
            {t(RECORD_TAG_KEY[tag])}
          </span>
        ))}
        <TimeChip iso={event.observedAt} />
        {event.model ? <span className="activity-feed__model">{event.model}</span> : null}
        {workspace ? <span className="activity-feed__project">{workspace}</span> : null}
      </div>
      <div className="activity-feed__row activity-feed__row--savings">
        <strong className="activity-feed__savings">
          {t("activity.record.saved", { count: event.tokensSaved.toLocaleString() })}
          {pct != null && pct <= 100 ? ` (${pct.toFixed(1)}%)` : ""}
        </strong>
        {event.previousRecord != null ? (
          <span className="activity-feed__delta">
            {t("activity.record.previous", { count: event.previousRecord.toLocaleString() })}
          </span>
        ) : null}
      </div>
    </ExpandableRow>
  );
}

function TrainSuggestionRow({
  event,
  onNavigate
}: {
  event: TrainSuggestionEvent;
  onNavigate?: (projectPath: string) => void;
}) {
  const { t } = useI18n();
  const isNeverTrained = event.kind === "never_trained";
  const badgeLabel = isNeverTrained ? t("activity.train.tryOptimize") : t("activity.train.rescan");
  const copy = isNeverTrained
    ? t("activity.train.never", { count: event.sessionCount, project: event.projectDisplayName })
    : t("activity.train.stale", { count: event.activeDaysSinceLastLearn, project: event.projectDisplayName });
  const canNavigate = typeof onNavigate === "function";
  const handleActivate = () => {
    if (canNavigate) onNavigate?.(event.projectPath);
  };
  /* v8 ignore start — keyboard activation requires a DOM; see ExpandableRow. */
  const onKeyDown = (e: ReactKeyboardEvent<HTMLLIElement>) => {
    if (!canNavigate) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      handleActivate();
    }
  };
  /* v8 ignore stop */
  return (
    <li
      className={`activity-feed__item activity-feed__item--train${canNavigate ? " activity-feed__item--clickable" : ""}`}
      role={canNavigate ? "button" : undefined}
      tabIndex={canNavigate ? 0 : undefined}
      onClick={canNavigate ? handleActivate : undefined}
      onKeyDown={onKeyDown}
    >
      <div className="activity-feed__row activity-feed__row--meta">
        <span className="activity-feed__badge activity-feed__badge--train">
          {badgeLabel}
        </span>
        <TimeChip iso={event.observedAt} />
        <span className="activity-feed__project">{event.projectDisplayName}</span>
      </div>
      <p className="activity-feed__content">{copy}</p>
    </li>
  );
}

function LearningsMilestoneRow({ event }: { event: LearningsMilestoneEvent }) {
  const { resolvedLocale, t } = useI18n();
  const { patternsToday, remindersToday, learningsToday, projectDisplayName } = event;
  const summary = resolvedLocale === "en"
    ? `Today's additions: ${patternsToday} pattern${patternsToday === 1 ? "" : "s"} identified, ${remindersToday} reminder${remindersToday === 1 ? "" : "s"} and ${learningsToday} learning${learningsToday === 1 ? "" : "s"} written to memory.`
    : t("activity.learnings.summary", { patterns: patternsToday, reminders: remindersToday, learnings: learningsToday });
  return (
    <li className="activity-feed__item activity-feed__item--learnings-milestone">
      <div className="activity-feed__row activity-feed__row--meta">
        <span className="activity-feed__badge activity-feed__badge--learnings-milestone">
          {t("activity.badge.learnings")}
        </span>
        <TimeChip iso={event.observedAt} />
        {projectDisplayName ? (
          <span className="activity-feed__project">{projectDisplayName}</span>
        ) : null}
      </div>
      <p className="activity-feed__content">
        {summary}
      </p>
    </li>
  );
}

function WeeklyRecapRow({ event }: { event: WeeklyRecapEvent }) {
  const { t } = useI18n();
  return (
    <li className="activity-feed__item activity-feed__item--weekly-recap">
      <div className="activity-feed__row activity-feed__row--meta">
        <span className="activity-feed__badge activity-feed__badge--weekly-recap">
          {t("activity.badge.weekly")}
        </span>
        <TimeChip iso={event.observedAt} />
        <span className="activity-feed__week-range">
          {event.weekStart} – {event.weekEnd}
        </span>
      </div>
      <div className="activity-feed__row activity-feed__row--savings">
        <strong className="activity-feed__savings">
          {t("activity.weekly.saved", { tokens: event.totalTokensSaved.toLocaleString(), amount: `$${event.totalSavingsUsd.toFixed(2)}` })}
        </strong>
        <span className="activity-feed__delta">
          {t("activity.weekly.activeDays", { count: event.activeDays })}
        </span>
      </div>
    </li>
  );
}
