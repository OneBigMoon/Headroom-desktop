import { afterEach, describe, expect, it, vi } from "vitest";
import {
  aggregateClientConnectors,
  baseUrlTakeoverNotice,
  buildHourlySavingsChartData,
  buildHourlySavingsWindow,
  buildMonthlySavingsChartData,
  buildMonthlySavingsWindow,
  compressibleInputSavingsRate,
  allTimeCacheHitPair,
  cacheHitPair,
  outputReductionForWindow,
  compactNumber,
  connectorDashboardStatus,
  connectorStatusLine,
  shouldAutoRestartCodex,
  clientSetupNotice,
  currency,
  currencyExact,
  dayOfMonthTickFormatter,
  earliestHourlyDay,
  earliestSavingsMonth,
  formatDateTime,
  formatDayKey,
  formatLearnStatus,
  hasNeverScanned,
  getEnabledSupportedConnectors,
  hasEnabledConnector,
  hourOfDayTickFormatter,
  mergeProviderSavingsForDisplay,
  percent1,
  savingsRate,
  shouldShowConnectorDetectionWarning,
  sortClientConnectors
} from "./dashboardHelpers";
import type {
  ClientConnectorStatus,
  ClientSetupResult,
  DailySavingsPoint,
  HourlySavingsPoint
} from "./types";

describe("dashboard helpers", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("formats stable numeric summaries", () => {
    expect(currencyExact(12.345)).toBe("$12.35");
    expect(currency(9999)).toBe("$9,999");
    expect(currency(15_432)).toContain("K");
    expect(compactNumber(12_345)).toBe("12.3K");
    expect(percent1(18)).toBe("18.0");
  });

  it("computes savings rate against the would-be total", () => {
    expect(savingsRate(15, 85)).toBe(15);
    expect(savingsRate(0, 0)).toBeNull();
    expect(savingsRate(0, 100)).toBe(0);
    expect(savingsRate(-5, 100)).toBe(0);
  });

  it("renders near-zero negatives as clean zero, not -$0", () => {
    expect(currency(-0.003)).toBe("$0");
    expect(currencyExact(-0.003)).toBe("$0.00");
    expect(currency(-1.5)).toBe("-$2"); // genuine negatives still show
  });

  it("builds full monthly windows with zero-filled gaps", () => {
    const data: DailySavingsPoint[] = [
      {
        date: "2024-02-02",
        estimatedSavingsUsd: 2.5,
        estimatedTokensSaved: 250,
        actualCostUsd: 1.5,
        totalTokensSent: 1000
      }
    ];

    const month = new Date(2024, 1, 18);
    const windowed = buildMonthlySavingsWindow(data, month);

    expect(windowed).toHaveLength(29);
    expect(windowed[0]).toEqual({
      date: "2024-02-01",
      estimatedSavingsUsd: 0,
      estimatedTokensSaved: 0,
      actualCostUsd: 0,
      totalTokensSent: 0,
      outputSavingsUsd: 0,
      outputTokensSaved: 0
    });
    expect(windowed[1]).toEqual(data[0]);
    expect(windowed[28].date).toBe("2024-02-29");
  });

  it("builds hourly windows and chart data with derived totals", () => {
    const data: HourlySavingsPoint[] = [
      {
        hour: "2024-03-05T04:00",
        estimatedSavingsUsd: 1.25,
        estimatedTokensSaved: 125,
        actualCostUsd: 0.75,
        totalTokensSent: 500,
        byProvider: [
          {
            provider: "anthropic",
            estimatedSavingsUsd: 1.25,
            estimatedTokensSaved: 125,
            actualCostUsd: 0.75,
            totalTokensSent: 500
          }
        ]
      }
    ];

    const windowed = buildHourlySavingsWindow(data, new Date(2024, 2, 5, 12));
    const chartData = buildHourlySavingsChartData(windowed);

    expect(windowed).toHaveLength(24);
    expect(windowed[4]).toEqual(data[0]);
    expect(windowed[3].hour).toBe("2024-03-05T03:00");
    expect(chartData[4]).toMatchObject({
      bucketKey: "2024-03-05T04:00",
      estimatedSavingsUsd: 1.25,
      estimatedTokensSaved: 125,
      actualCostUsd: 0.75,
      totalTokensSent: 500,
      totalCostBeforeOptimization: 2,
      totalTokensBeforeOptimization: 625
    });
    // Per-provider breakdown carries through; padded hours default to empty.
    expect(chartData[4].byProvider).toEqual(data[0].byProvider);
    expect(chartData[3].byProvider).toEqual([]);
  });

  it("plots the compressible slice of spend, not the cache-read-inflated total", () => {
    const covered: HourlySavingsPoint[] = [
      {
        hour: "2024-03-05T04:00",
        estimatedSavingsUsd: 1,
        estimatedTokensSaved: 100,
        actualCostUsd: 10,
        totalTokensSent: 1000,
        cacheReadTokens: 600,
        // $9 of discount earned => $1 of actual read cost.
        cacheSavingsUsd: 9,
        byProvider: []
      }
    ];

    const chartData = buildHourlySavingsChartData(covered);
    expect(chartData[0]).toMatchObject({
      actualCostUsd: 10,
      totalTokensSent: 1000,
      compressibleCostUsd: 9,
      // $9 compressible out of $19 of full-price input, applied to our own
      // token count -- never `totalTokensSent - cacheReadTokens`.
      compressibleTokensSent: 474
    });

    // No cache coverage: nothing to subtract, bars keep the full figure.
    const uncovered = buildHourlySavingsChartData([{ ...covered[0], cacheReadTokens: undefined, cacheSavingsUsd: undefined }]);
    expect(uncovered[0]).toMatchObject({ compressibleCostUsd: 10, compressibleTokensSent: 1000 });
  });

  it("keeps the token bar non-zero when provider cache reads exceed forwarded input", () => {
    // Real 2026-08-21T08:00 bucket: differencing the two scales clamped this
    // to a flat zero bar while the dollar bar showed $11.78.
    const chartData = buildHourlySavingsChartData([
      {
        hour: "2026-08-21T08:00",
        estimatedSavingsUsd: 1,
        estimatedTokensSaved: 100,
        actualCostUsd: 98.46,
        totalTokensSent: 77_148_652,
        cacheReadTokens: 102_285_108,
        cacheSavingsUsd: 780.1,
        byProvider: []
      }
    ]);
    expect(chartData[0].compressibleCostUsd).toBeGreaterThan(0);
    expect(chartData[0].compressibleTokensSent).toBeGreaterThan(0);
    expect(chartData[0].compressibleTokensSent).toBeLessThan(77_148_652);
  });

  it("builds monthly chart data and finds earliest visible history", () => {
    const dailyData: DailySavingsPoint[] = [
      {
        date: "2024-01-30",
        estimatedSavingsUsd: 1,
        estimatedTokensSaved: 100,
        actualCostUsd: 3,
        totalTokensSent: 1000
      },
      {
        date: "2024-03-01",
        estimatedSavingsUsd: 2,
        estimatedTokensSaved: 200,
        actualCostUsd: 4,
        totalTokensSent: 2000
      }
    ];
    const hourlyData: HourlySavingsPoint[] = [
      {
        hour: "2024-02-14T21:00",
        estimatedSavingsUsd: 0.5,
        estimatedTokensSaved: 50,
        actualCostUsd: 1,
        totalTokensSent: 300,
        byProvider: []
      }
    ];

    const chartData = buildMonthlySavingsChartData(dailyData);

    expect(chartData[0]).toMatchObject({
      bucketKey: "2024-01-30",
      totalCostBeforeOptimization: 4,
      totalTokensBeforeOptimization: 1100
    });
    expect(formatDayKey(earliestSavingsMonth(dailyData) as Date)).toBe("2024-01-01");
    expect(formatDayKey(earliestHourlyDay(hourlyData) as Date)).toBe("2024-02-14");
  });

  it("formats chart ticks predictably", () => {
    expect(dayOfMonthTickFormatter("2024-02-01")).toBe("1");
    expect(dayOfMonthTickFormatter("2024-02-02")).toBe("");
    expect(dayOfMonthTickFormatter("2024-02-29")).toBe("29");
    expect(hourOfDayTickFormatter("2024-02-01T04:00")).toBe("04");
    expect(hourOfDayTickFormatter("2024-02-01T05:00")).toBe("");
    expect(hourOfDayTickFormatter("2024-02-01T23:00")).toBe("23");
  });

  it("filters and sorts client connectors", () => {
    const connectors: ClientConnectorStatus[] = [
      { clientId: "zed", name: "Zed", installed: false, enabled: false, verified: false },
      { clientId: "claude_code", name: "Claude Code", installed: true, enabled: true, verified: true },
      { clientId: "cursor", name: "Cursor", installed: true, enabled: false, verified: false }
    ];

    expect(aggregateClientConnectors(connectors)).toEqual([connectors[1]]);
    expect(sortClientConnectors(connectors).map((connector) => connector.clientId)).toEqual([
      "claude_code",
      "cursor",
      "zed"
    ]);
  });

  it("keeps connector setup follow-up steps visible", () => {
    const result: ClientSetupResult = {
      clientId: "codex",
      applied: true,
      alreadyConfigured: false,
      summary: "Configured",
      changedFiles: [],
      backupFiles: [],
      nextSteps: ["Restart Codex.", "Run /hooks."],
      verification: {
        clientId: "codex",
        verified: true,
        proxyReachable: true,
        checks: [],
        failures: []
      }
    };

    expect(clientSetupNotice("Codex", result)).toBe(
      "Codex is configured through Headroom. Restart Codex. Run /hooks."
    );
    expect(baseUrlTakeoverNotice("https://gateway.example")).toContain(
      "https://gateway.example"
    );
  });

  it("reports connector status without mistaking an unrestarted client for a broken one", () => {
    const now = Date.parse("2026-08-22T12:00:00Z");
    const base: ClientConnectorStatus = {
      clientId: "codex",
      name: "Codex",
      installed: false,
      enabled: true,
      verified: true,
      lastConfiguredAt: "2026-08-22T11:00:00Z",
      verification: {
        clientId: "codex",
        verified: true,
        proxyReachable: true,
        checks: ["Found Headroom-managed provider block in ~/.codex/config.toml."],
        failures: []
      }
    };

    // A confirmed stale Codex process is a real restart requirement.
    expect(connectorStatusLine({ ...base, restartRequired: true }, now)).toEqual({
      text: "Restart Codex to apply the configuration.",
      tone: "restart"
    });
    // Known-restarted clients do not receive the old timestamp-only hint.
    expect(connectorStatusLine({ ...base, restartRequired: false }, now)).toBeNull();
    // Unknown process state retains the short fallback hint.
    expect(connectorStatusLine({ ...base, restartRequired: null }, now)).toEqual({
      text: "Quit and reopen Codex if it was running when you enabled this.",
      tone: "restart"
    });
    // ...and it stops nagging a working setup the next day.
    expect(connectorStatusLine(base, now + 25 * 60 * 60 * 1000)).toBeNull();
    // Not installed is not a fault: Codex desktop/IDE share ~/.codex/config.toml.
    expect(connectorStatusLine({ ...base, lastConfiguredAt: null }, now)).toBeNull();
    expect(connectorStatusLine({ ...base, enabled: false }, now)).toBeNull();

    // Another tool owning the Codex route is a coexist state, not a failure:
    // report who owns it and how to take over, and do not unlock the restart
    // action (`note`, not `restart`).
    expect(
      connectorStatusLine(
        {
          ...base,
          verified: false,
          verification: {
            clientId: "codex",
            verified: false,
            proxyReachable: true,
            checks: [],
            failures: [],
            foreignProvider: "codex_local_access"
          }
        },
        now
      )
    ).toEqual({
      text: "Codex routing is currently handled by codex_local_access, so Headroom is not intercepting. Turn this connector off and on to route Codex through Headroom.",
      tone: "note"
    });

    const staleCodex = {
      ...base,
      installed: true,
      restartRequired: true
    };
    expect(shouldAutoRestartCodex("codex", true, [staleCodex])).toBe(true);
    expect(shouldAutoRestartCodex("codex", false, [staleCodex])).toBe(false);
    expect(shouldAutoRestartCodex("claude_code", true, [staleCodex])).toBe(false);
    expect(
      shouldAutoRestartCodex("codex", true, [
        { ...staleCodex, verified: false }
      ])
    ).toBe(false);
    expect(
      shouldAutoRestartCodex("codex", true, [
        {
          ...staleCodex,
          verification: {
            clientId: "codex",
            verified: true,
            proxyReachable: false,
            checks: [],
            failures: []
          }
        }
      ])
    ).toBe(false);

    expect(connectorStatusLine({ ...base, verified: false }, now)).toEqual({
      text: "Setup is incomplete - open the info panel for the exact checks.",
      tone: "reason"
    });
    expect(
      connectorStatusLine({ ...base, verified: false, verification: null }, now)
    ).toEqual({
      text: "Setup could not be verified - open the info panel and re-check.",
      tone: "reason"
    });
    expect(
      connectorStatusLine(
        { ...base, verification: { ...base.verification!, proxyReachable: false } },
        now
      )
    ).toEqual({
      text: "Quit and reopen Codex if it was running when you enabled this.",
      tone: "restart"
    });
  });

  it("shows missing-client guidance only for enabled connectors", () => {
    const connector: ClientConnectorStatus = {
      clientId: "grok_build",
      name: "Grok Build",
      installed: false,
      enabled: false,
      verified: false
    };

    expect(shouldShowConnectorDetectionWarning(connector)).toBe(false);
    expect(
      shouldShowConnectorDetectionWarning({ ...connector, enabled: true })
    ).toBe(true);
    expect(
      shouldShowConnectorDetectionWarning({
        ...connector,
        enabled: true,
        installed: true
      })
    ).toBe(false);
  });

  it("keeps codex, grok_build and opencode alongside claude_code as supported connectors", () => {
    const connectors: ClientConnectorStatus[] = [
      { clientId: "codex", name: "Codex", installed: true, enabled: false, verified: false },
      { clientId: "grok_build", name: "Grok Build", installed: true, enabled: false, verified: false },
      { clientId: "opencode", name: "OpenCode", installed: true, enabled: true, verified: false },
      { clientId: "claude_code", name: "Claude Code", installed: true, enabled: true, verified: true },
      { clientId: "cursor", name: "Cursor", installed: true, enabled: false, verified: false }
    ];

    expect(
      aggregateClientConnectors(connectors).map((connector) => connector.clientId).sort()
    ).toEqual(["claude_code", "codex", "grok_build", "opencode"]);
  });

  it("reports enabled supported connectors regardless of which tool", () => {
    const connectors: ClientConnectorStatus[] = [
      { clientId: "claude_code", name: "Claude Code", installed: true, enabled: false, verified: false },
      { clientId: "codex", name: "Codex", installed: true, enabled: true, verified: true },
      { clientId: "cursor", name: "Cursor", installed: true, enabled: true, verified: true }
    ];

    expect(getEnabledSupportedConnectors(connectors).map((c) => c.clientId)).toEqual(["codex"]);
    expect(hasEnabledConnector(connectors)).toBe(true);
    expect(
      hasEnabledConnector([
        { clientId: "claude_code", name: "Claude Code", installed: true, enabled: false, verified: false }
      ])
    ).toBe(false);
  });

  it("derives a dashboard status label/tone per connector state", () => {
    expect(
      connectorDashboardStatus({ clientId: "codex", name: "Codex", installed: false, enabled: false, verified: false })
    ).toEqual({ label: "Not installed", tone: "off" });
    expect(
      connectorDashboardStatus({ clientId: "codex", name: "Codex", installed: true, enabled: false, verified: false })
    ).toEqual({ label: "Off", tone: "off" });
    expect(
      connectorDashboardStatus({ clientId: "codex", name: "Codex", installed: true, enabled: true, verified: false })
    ).toEqual({ label: "Verifying", tone: "pending" });
    // Coexist state: the connector is on but another tool owns the route, so
    // `verified` is false by design. Pending amber would claim setup is still
    // running, and the label has to name whoever is actually routing.
    expect(
      connectorDashboardStatus({
        clientId: "codex",
        name: "Codex",
        installed: true,
        enabled: true,
        verified: false,
        verification: {
          clientId: "codex",
          verified: false,
          proxyReachable: true,
          checks: [],
          failures: [],
          foreignProvider: "Cockpit (codex_local_access)",
        },
      })
    ).toEqual({ label: "Routed by Cockpit (codex_local_access)", tone: "off" });
    expect(
      connectorDashboardStatus({ clientId: "codex", name: "Codex", installed: true, enabled: true, verified: true })
    ).toEqual({ label: "Active", tone: "active" });
    expect(
      connectorDashboardStatus({ clientId: "codex", name: "Codex", installed: true, enabled: true, verified: true, restartRequired: true })
    ).toEqual({ label: "Restart needed", tone: "pending" });
    // Red is reserved for enabled-but-broken: proxy down while a connector is on.
    expect(
      connectorDashboardStatus(
        { clientId: "codex", name: "Codex", installed: true, enabled: true, verified: true, restartRequired: true },
        { proxyReachable: false }
      )
    ).toEqual({ label: "Proxy unreachable", tone: "idle" });
  });

  it("formats timestamps and learn recency with clear fallbacks", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-03-27T12:00:00Z"));

    expect(formatDateTime(null)).toBe("Never");
    expect(formatDateTime("not-a-date")).toBe("Unknown");
    expect(formatLearnStatus({ lastLearnRanAt: null })).toBe("never scan");
    expect(formatLearnStatus({ lastLearnRanAt: "invalid" })).toBe("never scan");
    expect(formatLearnStatus({ lastLearnRanAt: "2026-03-27T08:00:00Z" })).toBe("last scan: today");
    expect(formatLearnStatus({ lastLearnRanAt: "2026-03-26T08:00:00Z" })).toBe("last scan: yesterday");
    expect(formatLearnStatus({ lastLearnRanAt: "2026-03-22T08:00:00Z" })).toBe("last scan: 5 days ago");
    expect(hasNeverScanned({ lastLearnRanAt: null })).toBe(true);
    expect(hasNeverScanned({ lastLearnRanAt: "invalid" })).toBe(true);
    expect(hasNeverScanned({ lastLearnRanAt: "2026-03-22T08:00:00Z" })).toBe(false);
  });
});

describe("mergeProviderSavingsForDisplay", () => {
  it("folds anthropic and unknown into Claude Code, openai into Codex, and xai into Grok Build", () => {
    const merged = mergeProviderSavingsForDisplay([
      {
        provider: "openai",
        estimatedSavingsUsd: 0.04,
        estimatedTokensSaved: 40,
        actualCostUsd: 0.16,
        totalTokensSent: 80
      },
      {
        provider: "anthropic",
        estimatedSavingsUsd: 0.1,
        estimatedTokensSaved: 100,
        actualCostUsd: 0.24,
        totalTokensSent: 120
      },
      {
        provider: "unknown",
        estimatedSavingsUsd: 0.01,
        estimatedTokensSaved: 15,
        actualCostUsd: 0.03,
        totalTokensSent: 20
      },
      {
        provider: "xai",
        estimatedSavingsUsd: 0.02,
        estimatedTokensSaved: 25,
        actualCostUsd: 0.08,
        totalTokensSent: 50
      }
    ]);

    expect(merged).toEqual([
      {
        label: "Claude Code",
        estimatedSavingsUsd: 0.1 + 0.01,
        estimatedTokensSaved: 115,
        actualCostUsd: 0.24 + 0.03,
        totalTokensSent: 140
      },
      {
        label: "Codex",
        estimatedSavingsUsd: 0.04,
        estimatedTokensSaved: 40,
        actualCostUsd: 0.16,
        totalTokensSent: 80
      },
      {
        label: "Grok Build",
        estimatedSavingsUsd: 0.02,
        estimatedTokensSaved: 25,
        actualCostUsd: 0.08,
        totalTokensSent: 50
      }
    ]);
  });

  it("omits a connector with no attributed providers", () => {
    const merged = mergeProviderSavingsForDisplay([
      {
        provider: "anthropic",
        estimatedSavingsUsd: 0.1,
        estimatedTokensSaved: 100,
        actualCostUsd: 0.24,
        totalTokensSent: 120
      }
    ]);

    expect(merged).toHaveLength(1);
    expect(merged[0].label).toBe("Claude Code");
  });

  it("returns nothing for an empty breakdown", () => {
    expect(mergeProviderSavingsForDisplay([])).toEqual([]);
  });

});

describe("cacheHitPair", () => {
  it("computes both rates in dollars over covered buckets only", () => {
    const pair = cacheHitPair([
      // Read discount $9 -> read cost $1, reads' full-price value $10.
      { cacheSavingsUsd: 9, actualCostUsd: 3, estimatedSavingsUsd: 0.5 },
      // No cache coverage: excluded from BOTH rates, not just the hit rate.
      { actualCostUsd: 50, estimatedSavingsUsd: 999 },
      { cacheSavingsUsd: 4.5, actualCostUsd: 2.5, estimatedSavingsUsd: 1.5 }
    ]);
    expect(pair).not.toBeNull();
    // reads at full price 15 / full-price input (5.5 + 13.5)
    expect(pair!.hitPct).toBeCloseTo((15 / 19) * 100);
    // saved 2 / (saved 2 + rest (5.5 - 1.5))
    expect(pair!.compressedPct).toBeCloseTo((2 / 6) * 100);
  });

  it("returns null when no bucket carries cache data", () => {
    expect(cacheHitPair([{ actualCostUsd: 3, estimatedSavingsUsd: 1 }])).toBeNull();
    expect(cacheHitPair([])).toBeNull();
  });

  it("does not saturate when provider cache reads exceed our token count", () => {
    // The 2026-08-17 regression: the token form of this pair ratioed the
    // provider's cache reads against our own tokenizer's forwarded count;
    // reads exceeded input, pinning the display at "100% hits, 100% of the
    // rest compressed". The dollar form has no cross-tokenizer ratio at all.
    const pair = cacheHitPair([
      { cacheSavingsUsd: 9, actualCostUsd: 3, estimatedSavingsUsd: 0.5 }
    ]);
    expect(pair!.hitPct).toBeCloseTo((10 / 12) * 100);
    expect(pair!.compressedPct).toBeCloseTo(20);
  });

  it("reports a fully-cached window as 0% of an empty remainder", () => {
    const pair = cacheHitPair([
      // Actual cost is exactly the read cost: nothing was left to compress.
      { cacheSavingsUsd: 9, actualCostUsd: 1, estimatedSavingsUsd: 0 }
    ]);
    expect(pair!.hitPct).toBe(100);
    expect(pair!.compressedPct).toBe(0);
  });
});

describe("allTimeCacheHitPair", () => {
  const breakdown = {
    compressionSavingsUsd: 4,
    outputSavingsUsd: 0,
    cacheSavingsUsd: 9, // read discount $9 -> read cost $1
    cacheReadTokens: 900,
    totalInputTokens: 1000,
    totalInputCostUsd: 3 // $1 reads + $2 billable
  };

  it("prices the lifetime breakdown as one synthetic bucket", () => {
    const pair = allTimeCacheHitPair(breakdown, 4);
    const direct = cacheHitPair([
      {
        cacheSavingsUsd: breakdown.cacheSavingsUsd,
        actualCostUsd: breakdown.totalInputCostUsd,
        estimatedSavingsUsd: 4
      }
    ]);
    expect(pair).toEqual(direct);
    expect(pair!.hitPct).toBeCloseTo(83.33, 2);
    // $4 saved against $4 saved + $2 paid at full price.
    expect(pair!.compressedPct).toBeCloseTo(66.67, 2);
  });

  it("is null without cache coverage, whatever the dollars say", () => {
    // cacheReadTokens is the existence signal: no reads means no hit rate to
    // report, even though cacheSavingsUsd would divide fine.
    expect(allTimeCacheHitPair({ ...breakdown, cacheReadTokens: 0 }, 4)).toBeNull();
    expect(allTimeCacheHitPair(null, 4)).toBeNull();
    expect(allTimeCacheHitPair(undefined, 4)).toBeNull();
  });
});

describe("compressibleInputSavingsRate", () => {
  const bucket = {
    cacheReadTokens: 900,
    cacheSavingsUsd: 9, // read discount $9 -> read cost $1
    totalTokensSent: 1000,
    estimatedTokensSaved: 25,
    actualCostUsd: 3, // $1 reads + $2 billable
    estimatedSavingsUsd: 0.5
  };

  it("prices reads via the discount and excludes them", () => {
    // 0.5 / (0.5 + (3 - 9/9))
    expect(compressibleInputSavingsRate([bucket])!.pct).toBeCloseTo(20);
  });

  it("skips buckets without the dollar counter", () => {
    const uncovered = { ...bucket, cacheReadTokens: null, cacheSavingsUsd: null };
    expect(compressibleInputSavingsRate([uncovered])).toBeNull();
    expect(compressibleInputSavingsRate([bucket, uncovered])!.pct).toBeCloseTo(20);
  });

  it("stays sane when provider cache reads exceed our own input count", () => {
    // Real 2026-08-14 data: 195.8M derived cache reads against 163.4M forwarded
    // input tokens, because the two counters use different tokenizers. The old
    // token-based form clamped the denominator to 0 and reported 100%.
    const skewed = { ...bucket, cacheReadTokens: 1_200, totalTokensSent: 1_000 };
    expect(compressibleInputSavingsRate([skewed])!.pct).toBeCloseTo(20);
  });
});

describe("outputReductionForWindow", () => {
  it("computes reduction over sampled buckets only", () => {
    const pct = outputReductionForWindow([
      { outputSampledTokensSaved: 300, outputBaselineTokens: 1000 },
      { outputSampledTokensSaved: null, outputBaselineTokens: null },
      { outputSampledTokensSaved: 100, outputBaselineTokens: 1000 }
    ]);
    expect(pct!.pct).toBeCloseTo(20);
  });

  it("returns null without coverage or baseline", () => {
    expect(outputReductionForWindow([{}])).toBeNull();
    expect(
      outputReductionForWindow([{ outputSampledTokensSaved: 0, outputBaselineTokens: 0 }])
    ).toBeNull();
  });
});
