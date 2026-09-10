//! Per-day counters for provider rate limiting (HTTP 429s) and client mix,
//! observed at the intercept proxy. Aggregate counts only — never content.
//! The counters ride the milestone/heartbeat savings payload to headroom-web
//! (`SavingsDay.client_requests` / `SavingsDay.rate_limit_429s`).
//!
//! Day keys are LOCAL dates via `storage::user_day_key`, matching the local
//! tracker buckets that make up the recent days of the merged savings series
//! these counters are joined against (state.rs `merge_daily_savings`). Older
//! series days are UTC rollups and join approximately — UserDailySaving on
//! the server documents its day boundaries as approximate for that reason.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Retention for the on-disk map. The savings payload only reads back 30
/// days; 60 gives slack without letting the file grow unbounded.
const MAX_DAYS: usize = 60;

/// Minimum interval between disk flushes. Counters are aggregate telemetry:
/// losing the final <30s of increments on quit is acceptable, blocking the
/// request hot path on a write per request is not.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "usage-counters.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DayCounters {
    /// Provider-bound requests per client bucket, same keys as
    /// `intercept_request_counts` (`claude-code`, `codex`, `opencode`,
    /// `grok-build`).
    pub client_requests: BTreeMap<String, u64>,
    /// Upstream HTTP 429 responses per client bucket.
    pub rate_limit_429s: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PersistedCounters {
    schema_version: u32,
    days: BTreeMap<String, DayCounters>,
}

struct Store {
    path: PathBuf,
    days: BTreeMap<String, DayCounters>,
    dirty: bool,
    last_saved: Instant,
    /// Set when recovery could not preserve an unreadable/stale source file.
    /// Until that source is removed, never replace it with a fresh map.
    persistence_blocked: bool,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

impl Store {
    fn load_or_create(base_dir: &Path) -> Self {
        let path = crate::storage::config_file(base_dir, FILE_NAME);
        let mut persistence_blocked = false;
        let days = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<PersistedCounters>(&bytes) {
                Ok(persisted) if persisted.schema_version == SCHEMA_VERSION => persisted.days,
                Ok(persisted) => {
                    // A schema mismatch is still a valid JSON document. Do not
                    // silently throw away the retained counters and let the
                    // next flush overwrite the only copy; preserve the bytes
                    // first and then start with an empty in-memory map.
                    persistence_blocked = preserve_invalid_file(
                        &path,
                        &format!(
                            "schema {} (expected {})",
                            persisted.schema_version, SCHEMA_VERSION
                        ),
                    );
                    BTreeMap::new()
                }
                Err(err) => {
                    // Never silently overwrite a file we failed to parse:
                    // back it up so a truncation bug stays diagnosable.
                    persistence_blocked =
                        preserve_invalid_file(&path, &format!("corrupt JSON ({err})"));
                    BTreeMap::new()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => {
                // A non-missing read failure (for example, a transient lock or
                // permission error) must be visible and must not be mistaken
                // for an empty counters file that can overwrite user history.
                persistence_blocked = preserve_invalid_file(&path, &format!("read failed ({err})"));
                BTreeMap::new()
            }
        };
        Self {
            path,
            days,
            dirty: false,
            last_saved: Instant::now(),
            persistence_blocked,
        }
    }

    fn maybe_save(&mut self) {
        if !self.dirty || self.last_saved.elapsed() < SAVE_INTERVAL {
            return;
        }
        if self.persistence_blocked {
            // A recovery copy could not be created when the source was first
            // rejected. Keep the source untouched until the user removes or
            // repairs it; a fresh map must never overwrite the only copy.
            if self.path.exists() {
                log::warn!(
                    "refusing to replace {} while its recovery copy is unavailable",
                    self.path.display()
                );
                // Throttle the diagnostic and the next recovery check to the
                // normal save interval; `with_store` runs on every request.
                self.last_saved = Instant::now();
                return;
            }
            // The source disappeared out-of-band, so there is no longer a
            // file at risk of being overwritten. Allow a new file to be
            // published on the next save.
            self.persistence_blocked = false;
        }
        let persisted = PersistedCounters {
            schema_version: SCHEMA_VERSION,
            // Keep the live map intact until the replacement is durably
            // published. A failed serialization/write must remain retryable
            // and must not silently discard the in-memory counters.
            days: self.days.clone(),
        };
        let bytes = match serde_json::to_vec(&persisted) {
            Ok(bytes) => bytes,
            Err(err) => {
                log::error!("failed to serialize {FILE_NAME}: {err}");
                return;
            }
        };
        if let Err(err) = crate::client_adapters::atomic_write(&self.path, &bytes) {
            log::warn!("failed to persist {FILE_NAME}: {err:#}");
            return;
        }
        self.dirty = false;
        self.last_saved = Instant::now();
    }
}

/// Preserve an unreadable or stale counters file before a fresh map is
/// allowed to replace it. The backup helper uses a unique sibling name and
/// records the exact bytes, so a failed backup leaves the source untouched.
fn preserve_invalid_file(path: &Path, reason: &str) -> bool {
    match crate::client_adapters::backup_if_exists(path) {
        Ok(Some(backup)) => {
            log::warn!(
                "{FILE_NAME} is unusable ({reason}); starting fresh with rollback copy {}",
                backup.display()
            );
            false
        }
        Ok(None) => {
            log::warn!("{FILE_NAME} is unusable ({reason}); starting fresh (source disappeared)");
            false
        }
        Err(err) => {
            log::error!(
                "{FILE_NAME} is unusable ({reason}); could not preserve original, leaving it in place: {err:#}"
            );
            true
        }
    }
}

fn today_key() -> String {
    crate::storage::user_day_key(chrono::Local::now())
}

fn with_store(f: impl FnOnce(&mut Store)) {
    let mut guard = match STORE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let store = guard.get_or_insert_with(|| Store::load_or_create(&crate::storage::app_data_dir()));
    f(store);
    store.maybe_save();
}

fn bump(days: &mut BTreeMap<String, DayCounters>, day: String, client: &str, is_429: bool) {
    let counters = days.entry(day).or_default();
    let map = if is_429 {
        &mut counters.rate_limit_429s
    } else {
        &mut counters.client_requests
    };
    *map.entry(client.to_string()).or_default() += 1;
    // BTreeMap orders ISO date keys chronologically, so pruning the first
    // entry always drops the oldest day.
    while days.len() > MAX_DAYS {
        let oldest = days.keys().next().cloned();
        match oldest {
            Some(key) => days.remove(&key),
            None => break,
        };
    }
}

/// Count one provider-bound request for a client bucket (today, local day).
pub fn record_request(client: &str) {
    with_store(|store| {
        bump(&mut store.days, today_key(), client, false);
        store.dirty = true;
    });
}

/// Count one upstream HTTP 429 for a client bucket (today, local day).
pub fn record_429(client: &str) {
    with_store(|store| {
        bump(&mut store.days, today_key(), client, true);
        store.dirty = true;
    });
}

/// Snapshot of all retained days, for joining into the savings payload.
pub fn recent_days() -> BTreeMap<String, DayCounters> {
    let mut out = BTreeMap::new();
    with_store(|store| out = store.days.clone());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn bump_counts_requests_and_429s_separately() {
        let mut days = BTreeMap::new();
        bump(&mut days, "2026-08-17".into(), "claude-code", false);
        bump(&mut days, "2026-08-17".into(), "claude-code", false);
        bump(&mut days, "2026-08-17".into(), "claude-code", true);
        bump(&mut days, "2026-08-17".into(), "codex", false);

        let day = &days["2026-08-17"];
        assert_eq!(day.client_requests["claude-code"], 2);
        assert_eq!(day.client_requests["codex"], 1);
        assert_eq!(day.rate_limit_429s["claude-code"], 1);
        assert!(!day.rate_limit_429s.contains_key("codex"));
    }

    #[test]
    fn bump_prunes_oldest_days_past_retention() {
        let mut days = BTreeMap::new();
        for i in 0..(MAX_DAYS + 5) {
            bump(
                &mut days,
                format!("2026-01-{:02}", i + 1),
                "claude-code",
                false,
            );
        }
        assert_eq!(days.len(), MAX_DAYS);
        assert!(!days.contains_key("2026-01-01"));
        assert!(days.contains_key(&format!("2026-01-{:02}", MAX_DAYS + 5)));
    }

    #[test]
    fn persisted_counters_tolerate_missing_fields() {
        // A future field added to DayCounters must not wipe history on
        // rollback; serde(default) has to absorb both directions.
        let parsed: PersistedCounters =
            serde_json::from_str(r#"{"schemaVersion":1,"days":{"2026-08-17":{}}}"#).unwrap();
        assert_eq!(parsed.days["2026-08-17"], DayCounters::default());
    }

    #[test]
    fn round_trips_through_json() {
        let mut days = BTreeMap::new();
        bump(&mut days, "2026-08-17".into(), "codex", true);
        let persisted = PersistedCounters {
            schema_version: SCHEMA_VERSION,
            days,
        };
        let bytes = serde_json::to_vec(&persisted).unwrap();
        let back: PersistedCounters = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.days["2026-08-17"].rate_limit_429s["codex"], 1);
    }

    #[test]
    fn schema_mismatch_preserves_original_before_reset() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config").join(FILE_NAME);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original =
            br#"{"schemaVersion":999,"days":{"2026-08-17":{"clientRequests":{"codex":4}}}}"#;
        std::fs::write(&path, original).unwrap();

        let store = Store::load_or_create(dir.path());
        assert!(store.days.is_empty(), "unknown schema must not be trusted");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .any(|backup| {
                    backup
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("usage-counters.json.headroom-local-community-backup-")
                        })
                        && std::fs::read(backup)
                            .map(|bytes| bytes == original)
                            .unwrap_or(false)
                }),
            "schema mismatch must retain a byte-preserving rollback copy"
        );
    }

    #[test]
    fn failed_save_keeps_counters_dirty_for_retry() {
        let dir = TempDir::new().unwrap();
        let days = BTreeMap::from([(
            "2026-08-17".to_string(),
            DayCounters {
                client_requests: BTreeMap::from([("codex".to_string(), 2)]),
                rate_limit_429s: BTreeMap::new(),
            },
        )]);
        let mut store = Store {
            // The parent directory is intentionally absent, so atomic_write
            // fails before publication and the counters must remain retryable.
            path: dir.path().join("missing").join(FILE_NAME),
            days: days.clone(),
            dirty: true,
            last_saved: Instant::now() - SAVE_INTERVAL - Duration::from_secs(1),
            persistence_blocked: false,
        };

        store.maybe_save();

        assert!(
            store.dirty,
            "failed writes must stay dirty for a later retry"
        );
        assert_eq!(
            store.days, days,
            "failed writes must retain in-memory counts"
        );
    }

    #[test]
    fn backup_failure_blocks_replacement_of_unreadable_source() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config").join(FILE_NAME);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // `read` reports EISDIR and `backup_if_exists` cannot copy a
        // directory, giving us a deterministic recovery-copy failure without
        // changing filesystem permissions for the test process.
        std::fs::create_dir(&path).unwrap();

        let mut store = Store::load_or_create(dir.path());
        assert!(
            store.persistence_blocked,
            "a failed recovery copy must install a write barrier"
        );
        store.days = BTreeMap::from([(
            "2026-08-17".to_string(),
            DayCounters {
                client_requests: BTreeMap::from([("codex".to_string(), 1)]),
                rate_limit_429s: BTreeMap::new(),
            },
        )]);
        store.dirty = true;
        store.last_saved = Instant::now() - SAVE_INTERVAL - Duration::from_secs(1);
        store.maybe_save();

        assert!(store.persistence_blocked);
        assert!(path.is_dir(), "the unreadable source must remain untouched");
    }
}
