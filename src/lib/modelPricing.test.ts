import { describe, expect, it } from "vitest";
import {
  canonicalPricingModel,
  estimateCostSavingsUsd,
  formatEstimatedUsd
} from "./modelPricing";

describe("estimateCostSavingsUsd", () => {
  it("returns null for zero or negative savings", () => {
    expect(estimateCostSavingsUsd("claude-sonnet-4-6", 0)).toBeNull();
    expect(estimateCostSavingsUsd("claude-sonnet-4-6", -5)).toBeNull();
    expect(estimateCostSavingsUsd("claude-sonnet-4-6", null)).toBeNull();
  });

  it("does not invent a price for unknown, missing, or internal models", () => {
    expect(estimateCostSavingsUsd(null, 1_000_000)).toBeNull();
    expect(estimateCostSavingsUsd("mystery-model-9000", 1_000_000)).toBeNull();
    expect(estimateCostSavingsUsd("gpt-5.3-codex-spark", 1_000_000)).toBeNull();
    expect(estimateCostSavingsUsd("codex-auto-review", 1_000_000)).toBeNull();
  });

  it("includes the current GPT-6 and GPT-5.6 release fallback prices", () => {
    expect(estimateCostSavingsUsd("gpt-6-astra", 1_000_000)).toBeCloseTo(10);
    expect(estimateCostSavingsUsd("gpt-6.1-sol", 1_000_000)).toBeCloseTo(2);
    expect(estimateCostSavingsUsd("gpt-6-sol", 1_000_000)).toBeCloseTo(2);
    expect(estimateCostSavingsUsd("gpt-6-luna", 1_000_000)).toBeCloseTo(0.1);
    expect(estimateCostSavingsUsd("gpt-5.6-sol", 1_000_000)).toBeCloseTo(4);
    expect(estimateCostSavingsUsd("gpt-5.6-terra", 1_000_000)).toBeCloseTo(2);
    expect(estimateCostSavingsUsd("gpt-5.6-luna", 1_000_000)).toBeCloseTo(0.2);
  });

  it("lets the auto-synced official catalog override the release fallback", () => {
    expect(
      estimateCostSavingsUsd("gpt-6-sol", 1_000_000, { "gpt-6-sol": 2.25 })
    ).toBeCloseTo(2.25);
  });

  it("normalizes provider prefixes and dated snapshots", () => {
    expect(canonicalPricingModel("openai/gpt-6-sol-2026-09-23")).toBe("gpt-6-sol");
    expect(canonicalPricingModel("gpt-6-luna-20260923")).toBe("gpt-6-luna");
    expect(estimateCostSavingsUsd("openai/gpt-6-sol-2026-09-23", 1_000_000)).toBe(2);
  });
});

describe("formatEstimatedUsd", () => {
  it("formats values with decreasing precision so sub-cent values stay readable", () => {
    expect(formatEstimatedUsd(12.345)).toBe("~$12.35");
    expect(formatEstimatedUsd(1.2345)).toBe("~$1.23");
    expect(formatEstimatedUsd(0.1234)).toBe("~$0.123");
    expect(formatEstimatedUsd(0.0123)).toBe("~$0.012");
    expect(formatEstimatedUsd(0.00123)).toBe("~$0.0012");
    expect(formatEstimatedUsd(0.00001)).toBe("~<$0.0001");
  });
});
