use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::{DailySavingsPoint, HourlySavingsPoint, ModelInputPrice};
use crate::storage::{app_data_dir, config_file, ensure_data_dirs};

pub(crate) const OFFICIAL_PRICING_URL: &str = "https://developers.openai.com/api/docs/pricing";
const MODEL_PAGE_BASE_URL: &str = "https://platform.openai.com/docs/models";
const PRICE_CACHE_FILE: &str = "official-model-prices.json";
const PRICE_ADJUSTMENTS_FILE: &str = "official-model-price-adjustments.json";
const CACHE_SCHEMA_VERSION: u32 = 1;
const ADJUSTMENTS_SCHEMA_VERSION: u32 = 1;
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const FAILED_RETRY_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const BUILTIN_VERIFIED_AT: &str = "2026-09-30T00:00:00Z";

// Release-time fallback. Runtime sync refreshes these values from each
// model's official OpenAI page. Keep exact model IDs here: unknown/internal
// IDs must remain unpriced rather than inheriting a similarly named model.
const BUILTIN_INPUT_PRICES: &[(&str, f64)] = &[
    ("gpt-6-astra", 10.0),
    ("gpt-6.1-sol", 2.0),
    ("gpt-6-sol", 2.0),
    ("gpt-6-luna", 0.10),
    ("gpt-5.6-sol", 4.0),
    ("gpt-5.6-terra", 2.0),
    ("gpt-5.6-luna", 0.20),
    ("gpt-5.3-codex", 1.75),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedPrice {
    input_usd_per_million: f64,
    synced_at: String,
    source_url: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PriceCacheFile {
    schema_version: u32,
    last_successful_sync_at: Option<String>,
    prices: BTreeMap<String, CachedPrice>,
}

#[derive(Default)]
struct PriceRuntime {
    loaded: bool,
    cache: PriceCacheFile,
    in_flight: HashSet<String>,
    last_attempt: HashMap<String, Instant>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ModelCounterBaseline {
    tokens_saved: u64,
    recorded_savings_usd: f64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PriceAdjustmentFile {
    schema_version: u32,
    models: BTreeMap<String, ModelCounterBaseline>,
    daily_adjustments_usd: BTreeMap<String, f64>,
    hourly_adjustments_usd: BTreeMap<String, f64>,
}

#[derive(Default)]
struct AdjustmentRuntime {
    loaded: bool,
    state: PriceAdjustmentFile,
    session_adjustment_usd: f64,
}

static PRICE_RUNTIME: OnceLock<Mutex<PriceRuntime>> = OnceLock::new();
static ADJUSTMENT_RUNTIME: OnceLock<Mutex<AdjustmentRuntime>> = OnceLock::new();

fn runtime() -> &'static Mutex<PriceRuntime> {
    PRICE_RUNTIME.get_or_init(|| Mutex::new(PriceRuntime::default()))
}

fn lock_runtime() -> std::sync::MutexGuard<'static, PriceRuntime> {
    runtime()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn adjustment_runtime() -> &'static Mutex<AdjustmentRuntime> {
    ADJUSTMENT_RUNTIME.get_or_init(|| Mutex::new(AdjustmentRuntime::default()))
}

fn lock_adjustments() -> std::sync::MutexGuard<'static, AdjustmentRuntime> {
    adjustment_runtime()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cache_path() -> std::path::PathBuf {
    config_file(&app_data_dir(), PRICE_CACHE_FILE)
}

fn adjustments_path() -> std::path::PathBuf {
    config_file(&app_data_dir(), PRICE_ADJUSTMENTS_FILE)
}

fn ensure_cache_loaded(runtime: &mut PriceRuntime) {
    if runtime.loaded {
        return;
    }
    runtime.loaded = true;

    // Unit tests must never read or mutate the developer's real app profile.
    if cfg!(test) {
        runtime.cache.schema_version = CACHE_SCHEMA_VERSION;
        return;
    }

    let Ok(bytes) = std::fs::read(cache_path()) else {
        runtime.cache.schema_version = CACHE_SCHEMA_VERSION;
        return;
    };
    let Ok(cache) = serde_json::from_slice::<PriceCacheFile>(&bytes) else {
        runtime.cache.schema_version = CACHE_SCHEMA_VERSION;
        return;
    };
    if cache.schema_version == CACHE_SCHEMA_VERSION {
        runtime.cache = cache;
    } else {
        runtime.cache.schema_version = CACHE_SCHEMA_VERSION;
    }
}

fn persist_cache(cache: &PriceCacheFile) -> anyhow::Result<()> {
    let base = app_data_dir();
    ensure_data_dirs(&base)?;
    let bytes = serde_json::to_vec_pretty(cache)?;
    crate::client_adapters::atomic_write(&config_file(&base, PRICE_CACHE_FILE), &bytes)
}

fn ensure_adjustments_loaded(runtime: &mut AdjustmentRuntime) {
    if runtime.loaded {
        return;
    }
    runtime.loaded = true;
    if cfg!(test) {
        runtime.state.schema_version = ADJUSTMENTS_SCHEMA_VERSION;
        return;
    }

    let Ok(bytes) = std::fs::read(adjustments_path()) else {
        runtime.state.schema_version = ADJUSTMENTS_SCHEMA_VERSION;
        return;
    };
    let Ok(state) = serde_json::from_slice::<PriceAdjustmentFile>(&bytes) else {
        runtime.state.schema_version = ADJUSTMENTS_SCHEMA_VERSION;
        return;
    };
    if state.schema_version == ADJUSTMENTS_SCHEMA_VERSION {
        runtime.state = state;
    } else {
        runtime.state.schema_version = ADJUSTMENTS_SCHEMA_VERSION;
    }
}

fn persist_adjustments(state: &PriceAdjustmentFile) -> anyhow::Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let base = app_data_dir();
    ensure_data_dirs(&base)?;
    let bytes = serde_json::to_vec_pretty(state)?;
    crate::client_adapters::atomic_write(&config_file(&base, PRICE_ADJUSTMENTS_FILE), &bytes)
}

fn builtin_price(model: &str) -> Option<CachedPrice> {
    BUILTIN_INPUT_PRICES
        .iter()
        .find(|(candidate, _)| *candidate == model)
        .map(|(_, price)| CachedPrice {
            input_usd_per_million: *price,
            synced_at: BUILTIN_VERIFIED_AT.to_string(),
            source_url: format!("{MODEL_PAGE_BASE_URL}/{model}"),
        })
}

pub(crate) fn canonical_model_id(raw: &str) -> Option<String> {
    let mut model = raw.trim().to_ascii_lowercase();
    if let Some(stripped) = model.strip_prefix("openai/") {
        model = stripped.to_string();
    }
    if !model.starts_with("gpt-")
        || model == "codex-auto-review"
        || model.contains("spark")
        || !model
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    {
        return None;
    }

    // Official model pages are keyed by the stable alias. Dated snapshots use
    // the same price, so resolve `...-2026-09-23` and `...-20260923` to it.
    if model.len() > 11 {
        let suffix = &model[model.len() - 10..];
        if suffix.as_bytes()[4] == b'-'
            && suffix.as_bytes()[7] == b'-'
            && suffix
                .chars()
                .enumerate()
                .all(|(idx, ch)| idx == 4 || idx == 7 || ch.is_ascii_digit())
            && model.as_bytes()[model.len() - 11] == b'-'
        {
            model.truncate(model.len() - 11);
        }
    }
    if model.len() > 9 {
        let suffix = &model[model.len() - 8..];
        if suffix.chars().all(|ch| ch.is_ascii_digit()) && model.as_bytes()[model.len() - 9] == b'-'
        {
            model.truncate(model.len() - 9);
        }
    }

    Some(model)
}

fn price_snapshot(raw_model: &str) -> Option<ModelInputPrice> {
    let model = canonical_model_id(raw_model)?;
    let mut runtime = lock_runtime();
    ensure_cache_loaded(&mut runtime);
    let price = runtime
        .cache
        .prices
        .get(&model)
        .cloned()
        .or_else(|| builtin_price(&model))?;
    if !price.input_usd_per_million.is_finite() || price.input_usd_per_million <= 0.0 {
        return None;
    }
    Some(ModelInputPrice {
        model,
        input_usd_per_million: price.input_usd_per_million,
        synced_at: price.synced_at,
    })
}

pub(crate) fn input_price_for_model(raw_model: &str) -> Option<ModelInputPrice> {
    price_snapshot(raw_model)
}

pub(crate) fn catalog_for_stats(root: &Value) -> (Vec<ModelInputPrice>, Option<String>) {
    let mut prices = BTreeMap::<String, ModelInputPrice>::new();
    let mut synced_at: Option<String> = None;
    if let Some(entries) = root.get("by_model").and_then(Value::as_object) {
        for raw_model in entries.keys() {
            let Some(price) = price_snapshot(raw_model) else {
                continue;
            };
            synced_at = Some(match synced_at {
                Some(current) => current.min(price.synced_at.clone()),
                None => price.synced_at.clone(),
            });
            prices.insert(price.model.clone(), price);
        }
    }
    (prices.into_values().collect(), synced_at)
}

pub(crate) fn record_forward_adjustments_from_stats_json(body: &str) {
    let Ok(root) = serde_json::from_str::<Value>(body) else {
        return;
    };
    record_forward_adjustments(&root, Utc::now());
}

fn record_forward_adjustments(root: &Value, now: DateTime<Utc>) {
    let Some(entries) = root.get("by_model").and_then(Value::as_object) else {
        return;
    };
    let local_now = now.with_timezone(&Local);
    let day_key = now.format("%Y-%m-%d").to_string();
    let hour_key = local_now.format("%Y-%m-%dT%H:00").to_string();
    let mut runtime = lock_adjustments();
    ensure_adjustments_loaded(&mut runtime);
    let mut changed = false;

    for (raw_model, node) in entries {
        let Some(model) = canonical_model_id(raw_model) else {
            continue;
        };
        let current = ModelCounterBaseline {
            tokens_saved: node
                .get("tokens_saved")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            recorded_savings_usd: node
                .get("compression_savings_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .max(0.0),
        };
        let previous = runtime.state.models.insert(model.clone(), current.clone());
        changed |= previous.as_ref() != Some(&current);
        let (Some(previous), Some(price)) = (previous, price_snapshot(&model)) else {
            continue;
        };
        let Some(adjustment) =
            calculate_forward_adjustment(&previous, &current, price.input_usd_per_million)
        else {
            continue;
        };
        *runtime
            .state
            .daily_adjustments_usd
            .entry(day_key.clone())
            .or_default() += adjustment;
        *runtime
            .state
            .hourly_adjustments_usd
            .entry(hour_key.clone())
            .or_default() += adjustment;
        runtime.session_adjustment_usd += adjustment;
        changed = true;
    }

    if !changed {
        return;
    }
    runtime.state.schema_version = ADJUSTMENTS_SCHEMA_VERSION;
    // Daily adjustments are part of the immutable lifetime total. Pruning them
    // would silently rewrite historical savings after the retention window.
    trim_oldest(&mut runtime.state.hourly_adjustments_usd, 31 * 24);
    if let Err(err) = persist_adjustments(&runtime.state) {
        eprintln!("model pricing: failed to persist forward adjustments: {err:#}");
    }
}

fn calculate_forward_adjustment(
    previous: &ModelCounterBaseline,
    current: &ModelCounterBaseline,
    input_usd_per_million: f64,
) -> Option<f64> {
    if current.tokens_saved <= previous.tokens_saved
        || current.recorded_savings_usd < previous.recorded_savings_usd
        || !input_usd_per_million.is_finite()
        || input_usd_per_million <= 0.0
    {
        return None;
    }
    let tokens_delta = current.tokens_saved - previous.tokens_saved;
    let recorded_delta = current.recorded_savings_usd - previous.recorded_savings_usd;
    let official_delta = tokens_delta as f64 / 1_000_000.0 * input_usd_per_million;
    let adjustment = official_delta - recorded_delta;
    adjustment.is_finite().then_some(adjustment)
}

fn trim_oldest(map: &mut BTreeMap<String, f64>, max_entries: usize) {
    while map.len() > max_entries {
        let Some(oldest) = map.keys().next().cloned() else {
            break;
        };
        map.remove(&oldest);
    }
}

pub(crate) fn apply_forward_adjustments(
    daily: &mut Vec<DailySavingsPoint>,
    hourly: &mut Vec<HourlySavingsPoint>,
) {
    let (daily_adjustments, hourly_adjustments) = {
        let mut runtime = lock_adjustments();
        ensure_adjustments_loaded(&mut runtime);
        (
            runtime.state.daily_adjustments_usd.clone(),
            runtime.state.hourly_adjustments_usd.clone(),
        )
    };

    for (date, adjustment) in daily_adjustments {
        if let Some(point) = daily.iter_mut().find(|point| point.date == date) {
            point.estimated_savings_usd += adjustment;
        }
    }
    for (hour, adjustment) in hourly_adjustments {
        if let Some(point) = hourly.iter_mut().find(|point| point.hour == hour) {
            point.estimated_savings_usd += adjustment;
            if let Some(openai) = point
                .by_provider
                .iter_mut()
                .find(|provider| provider.provider == "openai")
            {
                openai.estimated_savings_usd += adjustment;
            }
        }
    }
}

pub(crate) fn session_forward_adjustment_usd() -> f64 {
    let mut runtime = lock_adjustments();
    ensure_adjustments_loaded(&mut runtime);
    runtime.session_adjustment_usd
}

pub(crate) fn schedule_refresh_from_stats_json(body: &str) {
    if cfg!(test) {
        return;
    }
    let mut models: HashSet<String> = BUILTIN_INPUT_PRICES
        .iter()
        .map(|(model, _)| (*model).to_string())
        .collect();
    if let Ok(root) = serde_json::from_str::<Value>(body) {
        if let Some(entries) = root.get("by_model").and_then(Value::as_object) {
            models.extend(entries.keys().filter_map(|model| canonical_model_id(model)));
        }
    }
    schedule_refresh(models);
}

fn schedule_refresh(models: HashSet<String>) {
    let now = Instant::now();
    let mut runtime = lock_runtime();
    ensure_cache_loaded(&mut runtime);

    let due: Vec<String> =
        models
            .into_iter()
            .filter(|model| {
                if runtime.in_flight.contains(model) {
                    return false;
                }
                if runtime.last_attempt.get(model).is_some_and(|attempt| {
                    now.saturating_duration_since(*attempt) < FAILED_RETRY_TTL
                }) {
                    return false;
                }
                !runtime
                    .cache
                    .prices
                    .get(model)
                    .is_some_and(cache_entry_is_fresh)
            })
            .collect();

    if due.is_empty() {
        return;
    }
    for model in &due {
        runtime.in_flight.insert(model.clone());
        runtime.last_attempt.insert(model.clone(), now);
    }
    drop(runtime);

    let thread_due = due.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("official-model-price-sync".to_string())
        .spawn(move || refresh_prices(thread_due))
    {
        let mut runtime = lock_runtime();
        for model in due {
            runtime.in_flight.remove(&model);
        }
        eprintln!("model pricing: failed to start sync thread: {err}");
    }
}

fn cache_entry_is_fresh(entry: &CachedPrice) -> bool {
    let Ok(synced) = DateTime::parse_from_rfc3339(&entry.synced_at) else {
        return false;
    };
    let age = Utc::now().signed_duration_since(synced.with_timezone(&Utc));
    age.num_seconds() >= 0 && age.num_seconds() < CACHE_TTL.as_secs() as i64
}

fn refresh_prices(models: Vec<String>) {
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("codexbox-model-pricing/1")
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            finish_refresh(models, Vec::new());
            eprintln!("model pricing: failed to build HTTP client: {err}");
            return;
        }
    };

    let mut refreshed = Vec::new();
    for model in &models {
        match fetch_official_input_price(&client, model) {
            Ok(price) => refreshed.push((model.clone(), price)),
            Err(err) => eprintln!("model pricing: could not refresh {model}: {err:#}"),
        }
    }
    finish_refresh(models, refreshed);
}

fn finish_refresh(models: Vec<String>, refreshed: Vec<(String, CachedPrice)>) {
    let mut runtime = lock_runtime();
    ensure_cache_loaded(&mut runtime);
    for model in models {
        runtime.in_flight.remove(&model);
    }
    if refreshed.is_empty() {
        return;
    }

    let synced_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    for (model, mut price) in refreshed {
        price.synced_at = synced_at.clone();
        runtime.cache.prices.insert(model, price);
    }
    runtime.cache.schema_version = CACHE_SCHEMA_VERSION;
    runtime.cache.last_successful_sync_at = Some(synced_at);
    if let Err(err) = persist_cache(&runtime.cache) {
        eprintln!("model pricing: failed to persist official price cache: {err:#}");
    }
}

fn fetch_official_input_price(
    client: &reqwest::blocking::Client,
    model: &str,
) -> anyhow::Result<CachedPrice> {
    let requested_url = format!("{MODEL_PAGE_BASE_URL}/{model}");
    let response = client.get(&requested_url).send()?;
    anyhow::ensure!(
        response.status().is_success(),
        "{} returned HTTP {}",
        requested_url,
        response.status()
    );
    let source_url = response.url().to_string();
    let html = response.text()?;
    anyhow::ensure!(
        html.contains(&format!("/models/{model}")),
        "official page did not identify model {model}"
    );
    let input_usd_per_million = parse_official_input_price(&html)
        .ok_or_else(|| anyhow::anyhow!("official page had no standard per-1M input price"))?;
    anyhow::ensure!(
        input_usd_per_million.is_finite() && (0.0..=1_000.0).contains(&input_usd_per_million),
        "official page returned an invalid input price"
    );
    Ok(CachedPrice {
        input_usd_per_million,
        synced_at: String::new(),
        source_url,
    })
}

fn parse_official_input_price(html: &str) -> Option<f64> {
    let pricing = html
        .find("Pricing is based on")
        .or_else(|| html.find(">Pricing</div>"))?;
    let section = &html[pricing..];
    let per_million = section.find("Per 1M tokens")?;
    let section = &section[per_million..];
    let input = section.find(">Input</div>")?;
    let after_input = &section[input + ">Input</div>".len()..];
    let dollar = after_input.find('$')?;
    let digits: String = after_input[dollar + 1..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ','))
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.replace(',', "").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_input_price_from_official_model_markup() {
        let html = r#"
          <div>Pricing is based on token usage.</div>
          <div>Per 1M tokens</div>
          <div><div>Input</div><div class="text-2xl font-semibold">$2.00</div></div>
          <div><div>Cached input</div><div>$0.20</div></div>
        "#;
        assert_eq!(parse_official_input_price(html), Some(2.0));
    }

    #[test]
    fn normalizes_openai_prefix_and_dated_snapshots_only() {
        assert_eq!(
            canonical_model_id("openai/gpt-6-sol-2026-09-23").as_deref(),
            Some("gpt-6-sol")
        );
        assert_eq!(
            canonical_model_id("gpt-6-luna-20260923").as_deref(),
            Some("gpt-6-luna")
        );
        assert_eq!(
            canonical_model_id("gpt-6-sol-high").as_deref(),
            Some("gpt-6-sol-high")
        );
        assert!(canonical_model_id("gpt-5.3-codex-spark").is_none());
        assert!(canonical_model_id("codex-auto-review").is_none());
    }

    #[test]
    fn release_fallback_contains_current_gpt6_prices() {
        assert_eq!(
            builtin_price("gpt-6.1-sol").unwrap().input_usd_per_million,
            2.0
        );
        assert_eq!(
            builtin_price("gpt-6-astra").unwrap().input_usd_per_million,
            10.0
        );
        assert_eq!(
            builtin_price("gpt-6-sol").unwrap().input_usd_per_million,
            2.0
        );
        assert_eq!(
            builtin_price("gpt-6-luna").unwrap().input_usd_per_million,
            0.10
        );
        assert!(builtin_price("gpt-5.3-codex-spark").is_none());
    }

    #[test]
    fn forward_adjustment_prices_only_the_new_counter_delta() {
        let previous = ModelCounterBaseline {
            tokens_saved: 80_000_000,
            recorded_savings_usd: 400.0,
        };
        let current = ModelCounterBaseline {
            tokens_saved: 81_000_000,
            recorded_savings_usd: 405.0,
        };
        // Existing $400 is untouched. Only the new 1M tokens change from the
        // backend's recorded $5 to the current official $4, a -$1 adjustment.
        assert_eq!(
            calculate_forward_adjustment(&previous, &current, 4.0),
            Some(-1.0)
        );
        // A price change alone must never revalue already recorded counters.
        assert_eq!(calculate_forward_adjustment(&current, &current, 2.0), None);
        assert_eq!(calculate_forward_adjustment(&current, &current, 20.0), None);
    }

    #[test]
    fn counter_reset_rebaselines_without_repricing_history() {
        let previous = ModelCounterBaseline {
            tokens_saved: 10_000,
            recorded_savings_usd: 1.0,
        };
        let reset = ModelCounterBaseline {
            tokens_saved: 100,
            recorded_savings_usd: 0.01,
        };
        assert_eq!(calculate_forward_adjustment(&previous, &reset, 2.0), None);
    }
}
