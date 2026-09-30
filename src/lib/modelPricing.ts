/**
 * Standard uncached input-token prices (USD per 1M tokens). Runtime values
 * synced from OpenAI's official model pages override this release fallback.
 *
 * Compression removes input before it is sent, so output pricing never
 * applies. The estimate keeps a `~` prefix because long-context, regional,
 * batch, flex, fast-mode, and cache-write pricing cannot be reconstructed from
 * an aggregate activity record alone.
 */

export type ModelInputPriceMap = Readonly<Record<string, number>>;

// Verified against official OpenAI model pages on 2026-09-30.
const RELEASE_FALLBACK_PRICES: ModelInputPriceMap = {
  "gpt-6-astra": 10,
  "gpt-6.1-sol": 2,
  "gpt-6-sol": 2,
  "gpt-6-luna": 0.1,
  "gpt-5.6-sol": 4,
  "gpt-5.6-terra": 2,
  "gpt-5.6-luna": 0.2,
  "gpt-5.3-codex": 1.75
};

export function canonicalPricingModel(model: string | null | undefined): string | null {
  if (!model) return null;
  let normalized = model.trim().toLowerCase().replace(/^openai\//, "");
  if (
    !normalized.startsWith("gpt-") ||
    normalized === "codex-auto-review" ||
    normalized.includes("spark") ||
    !/^[a-z0-9._-]+$/.test(normalized)
  ) {
    return null;
  }
  normalized = normalized.replace(/-\d{4}-\d{2}-\d{2}$/, "").replace(/-\d{8}$/, "");
  return normalized;
}

export function estimateCostSavingsUsd(
  model: string | null | undefined,
  tokensSaved: number | null | undefined,
  syncedPrices: ModelInputPriceMap = {}
): number | null {
  if (!tokensSaved || tokensSaved <= 0) return null;
  const canonical = canonicalPricingModel(model);
  if (!canonical) return null;
  const rate = syncedPrices[canonical] ?? RELEASE_FALLBACK_PRICES[canonical];
  if (rate == null || !Number.isFinite(rate) || rate <= 0) return null;
  return (tokensSaved / 1_000_000) * rate;
}

/**
 * Format a small per-request USD estimate. Always prefixed with `~`, and uses
 * enough precision to show sub-cent values that matter for one compression.
 */
export function formatEstimatedUsd(usd: number): string {
  if (usd >= 1) return `~$${usd.toFixed(2)}`;
  if (usd >= 0.01) return `~$${usd.toFixed(3)}`;
  if (usd >= 0.0001) return `~$${usd.toFixed(4)}`;
  return "~<$0.0001";
}
