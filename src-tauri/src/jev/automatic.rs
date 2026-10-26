use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::{JevDashboard, JevInner, JevProvider, JevRecord, JevSource, JevStatus};

const MAX_EVENT_BYTES: u64 = (super::MAX_INPUT_BYTES * 8) as u64;
const MAX_RECORD_BYTES: u64 = 8 * 1024;
const MAX_DEDUP_BYTES: u64 = 64 * 1024;
const MAX_DEDUP_KEYS: usize = 256;
const DEDUP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PromptEvent {
    text: String,
    dedup_key: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct DedupLedger {
    schema_version: u32,
    keys: Vec<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
struct StoredRecord {
    schema_version: u32,
    record: JevRecord,
}

pub(super) fn run_if_requested() -> bool {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--jev-auto-classify")) {
        return false;
    }
    // A side evaluation must never gate Codex, open a window, or print prompt/key data.
    let _ = run(std::io::stdin().lock());
    true
}

fn run(reader: impl Read) -> Result<(), String> {
    if !crate::client_adapters::is_jev_auto_enabled() {
        return Ok(());
    }
    run_with(
        reader,
        &records_dir(),
        &dedup_path(),
        || {
            let mut inner = JevInner::default();
            super::restore_jev_key_from_store_without_interaction(&mut inner)?;
            inner.key.ok_or_else(|| "jev_key_missing".to_string())
        },
        |key, text| super::dispatch(super::JEV_URL, "jev-latest", key, text),
    )
}

fn run_with(
    reader: impl Read,
    records: &Path,
    dedup: &Path,
    load_key: impl FnOnce() -> Result<String, String>,
    dispatch: impl FnOnce(&str, &str) -> Result<super::ParsedResponse, String>,
) -> Result<(), String> {
    let event = read_prompt(reader)?;
    if load_dedup_for_dispatch(dedup)?
        .keys
        .contains(&event.dedup_key)
    {
        return Ok(());
    }
    let started = Instant::now();
    let outcome = match load_key() {
        Ok(key) => {
            if !claim(dedup, &event.dedup_key)? {
                return Ok(());
            }
            // Retain the claim even when the process dies or the response is unknown.
            // This observer is at-most-once within the last 256 claims: a skipped
            // observation is preferable to automatically paying for the same call twice.
            dispatch(&key, &event.text)
        }
        Err(error) => Err(error),
    };
    let record = record_outcome(
        outcome,
        started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    );
    store_record(records, record)
}

fn read_prompt(reader: impl Read) -> Result<PromptEvent, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_EVENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "jev_auto_input".to_string())?;
    if bytes.len() as u64 > MAX_EVENT_BYTES {
        return Err("jev_input_too_large".into());
    }
    let event: PromptEvent =
        serde_json::from_slice(&bytes).map_err(|_| "jev_auto_input".to_string())?;
    if event.text.trim().is_empty() {
        return Err("jev_input_empty".into());
    }
    if event.text.len() > super::MAX_INPUT_BYTES {
        return Err("jev_input_too_large".into());
    }
    if !valid_dedup_key(&event.dedup_key) {
        return Err("jev_auto_input".into());
    }
    Ok(event)
}

fn valid_dedup_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn dedup_path() -> PathBuf {
    records_dir().with_file_name("jev-auto-dedup.json")
}

fn read_dedup(path: &Path) -> Result<DedupLedger, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DedupLedger {
                schema_version: DEDUP_SCHEMA_VERSION,
                ..DedupLedger::default()
            });
        }
        Err(_) => return Err("jev_auto_records_read".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_DEDUP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "jev_auto_records_read".to_string())?;
    if bytes.len() as u64 > MAX_DEDUP_BYTES {
        return Err("jev_auto_records_read".into());
    }
    let ledger: DedupLedger =
        serde_json::from_slice(&bytes).map_err(|_| "jev_auto_records_read".to_string())?;
    let mut unique = std::collections::HashSet::new();
    if ledger.schema_version != DEDUP_SCHEMA_VERSION
        || ledger.keys.len() > MAX_DEDUP_KEYS
        || ledger
            .keys
            .iter()
            .any(|key| !valid_dedup_key(key) || !unique.insert(key))
    {
        return Err("jev_auto_records_read".into());
    }
    Ok(ledger)
}

fn load_dedup_for_dispatch(path: &Path) -> Result<DedupLedger, String> {
    read_dedup(path).inspect_err(|_| match crate::client_adapters::backup_if_exists(path) {
        Ok(_) => log::error!("Jev automatic dedup ledger could not be read; preserving original"),
        Err(error) => {
            log::error!("Jev automatic dedup backup failed; preserving original: {error}")
        }
    })
}

fn claim(path: &Path, key: &str) -> Result<bool, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "jev_auto_records_write".to_string())?;
    std::fs::create_dir_all(parent).map_err(|_| "jev_auto_records_write".to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // This empty file is only an OS lock, never ledger contents. Keep its inode
    // stable; removing it after unlock would allow two processes to lock different files.
    let lock = options
        .open(path.with_extension("lock"))
        .map_err(|_| "jev_auto_records_write".to_string())?;
    lock.lock()
        .map_err(|_| "jev_auto_records_write".to_string())?;
    let mut ledger = load_dedup_for_dispatch(path)?;
    if ledger.keys.iter().any(|existing| existing == key) {
        return Ok(false);
    }
    ledger.keys.push(key.to_string());
    if ledger.keys.len() > MAX_DEDUP_KEYS {
        ledger.keys.drain(..ledger.keys.len() - MAX_DEDUP_KEYS);
    }
    let bytes = serde_json::to_vec(&ledger).map_err(|_| "jev_auto_records_write".to_string())?;
    if bytes.len() as u64 > MAX_DEDUP_BYTES {
        return Err("jev_auto_records_write".into());
    }
    crate::client_adapters::atomic_write(path, &bytes)
        .map_err(|_| "jev_auto_records_write".to_string())?;
    Ok(true)
}

fn record_outcome(outcome: Result<super::ParsedResponse, String>, latency_ms: u64) -> JevRecord {
    let mut record = JevRecord {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        provider: JevProvider::TypeSafe,
        source: JevSource::Codex,
        endpoint: None,
        model: None,
        status: JevStatus::Error,
        latency_ms,
        input_tokens: None,
        output_tokens: None,
        choice: None,
        confidence: None,
        error: None,
    };
    match outcome {
        Ok(parsed) => {
            record.status = JevStatus::Success;
            record.model = parsed.model;
            record.input_tokens = parsed.input_tokens;
            record.output_tokens = parsed.output_tokens;
            record.choice = Some(parsed.choice);
            record.confidence = parsed.confidence;
        }
        Err(error) => record.error = Some(super::bounded_error(&error)),
    }
    record
}

fn records_dir() -> PathBuf {
    crate::storage::app_data_dir()
        .join("telemetry")
        .join("jev-auto")
}

fn load_records(dir: &Path) -> (Vec<(PathBuf, JevRecord)>, Option<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), None),
        Err(_) => return (Vec::new(), Some("jev_auto_records_read".into())),
    };
    let mut records = Vec::new();
    let mut error = None;
    for entry in entries {
        let item = (|| {
            let path = entry.map_err(|_| ())?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return Ok(None);
            }
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .map_err(|_| ())?
                .take(MAX_RECORD_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ())?;
            if bytes.len() as u64 > MAX_RECORD_BYTES {
                return Err(());
            }
            let stored: StoredRecord = serde_json::from_slice(&bytes).map_err(|_| ())?;
            if stored.schema_version != 1
                || stored.record.source != JevSource::Codex
                || stored.record.provider != JevProvider::TypeSafe
            {
                return Err(());
            }
            Ok(Some((path, stored.record)))
        })();
        match item {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(()) => error = Some("jev_auto_records_read".into()),
        }
    }
    records.sort_by(|left, right| {
        left.1
            .timestamp
            .cmp(&right.1.timestamp)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    (records, error)
}

fn store_record(dir: &Path, record: JevRecord) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|_| "jev_auto_records_write".to_string())?;
    let path = dir.join(format!("{}.json", record.id));
    let bytes = serde_json::to_vec(&StoredRecord {
        schema_version: 1,
        record,
    })
    .map_err(|_| "jev_auto_records_write".to_string())?;
    crate::client_adapters::atomic_write(&path, &bytes)
        .map_err(|_| "jev_auto_records_write".to_string())?;
    // Per-record atomic files avoid lost updates when old/new backends overlap at restart.
    let (records, _) = load_records(dir);
    let excess = records.len().saturating_sub(super::MAX_RECORDS);
    for (path, _) in records.into_iter().take(excess) {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("jev_auto_records_write".into()),
        }
    }
    Ok(())
}

pub(super) fn merge_records(dashboard: &mut JevDashboard) {
    let (records, error) = load_records(&records_dir());
    dashboard.auto_error = error.or_else(|| read_dedup(&dedup_path()).err());
    dashboard
        .records
        .extend(records.into_iter().map(|(_, record)| record));
    dashboard.records.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.id.cmp(&right.id))
    });
    super::trim_records(&mut dashboard.records);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(key: &str) -> String {
        serde_json::json!({"text": "synthetic prompt, never persist", "dedupKey": key}).to_string()
    }

    fn parsed() -> super::super::ParsedResponse {
        super::super::ParsedResponse {
            model: Some("jev-1.14.0".into()),
            choice: "coding".into(),
            confidence: Some(0.9),
            input_tokens: Some(412),
            output_tokens: Some(45),
        }
    }

    #[test]
    fn prompt_input_is_bounded_and_only_extracts_text() {
        let valid = serde_json::json!({"text": "test", "dedupKey": "a".repeat(64)}).to_string();
        assert_eq!(read_prompt(valid.as_bytes()).unwrap().text, "test");
        for input in [
            br#"{"text":" "}"#.as_slice(),
            br#"{"text":2}"#,
            br#"{}"#,
            b"not-json",
        ] {
            assert!(read_prompt(input).is_err());
        }
        let over_limit =
            serde_json::json!({"text": "x".repeat(super::super::MAX_INPUT_BYTES + 1)}).to_string();
        assert_eq!(
            read_prompt(over_limit.as_bytes()).unwrap_err(),
            "jev_input_too_large"
        );
    }

    #[test]
    fn missing_or_invalid_dedup_keys_do_not_create_records_or_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("records");
        let ledger = dir.path().join("dedup.json");
        for input in [
            serde_json::json!({"text": "test"}).to_string(),
            event(""),
            event("opaque"),
            event(&"A".repeat(64)),
            event(&"g".repeat(64)),
        ] {
            assert_eq!(
                run_with(
                    input.as_bytes(),
                    &records,
                    &ledger,
                    || panic!("invalid input must not load credentials"),
                    |_, _| panic!("invalid input must not dispatch"),
                )
                .unwrap_err(),
                "jev_auto_input"
            );
        }
        assert!(!records.exists());
        assert!(!ledger.exists());
    }

    #[test]
    fn persisted_claim_skips_restarted_observer_without_rewriting_real_records() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("records");
        let ledger = dir.path().join("dedup.json");
        let input = event(&"a".repeat(64));
        let calls = std::cell::Cell::new(0);
        run_with(
            input.as_bytes(),
            &records,
            &ledger,
            || Ok("synthetic-key-marker".into()),
            |key, text| {
                assert_eq!(key, "synthetic-key-marker");
                assert_eq!(text, "synthetic prompt, never persist");
                calls.set(calls.get() + 1);
                Ok(parsed())
            },
        )
        .unwrap();
        let (stored, error) = load_records(&records);
        assert!(error.is_none());
        assert_eq!(stored.len(), 1);
        let original = std::fs::read(&stored[0].0).unwrap();
        run_with(
            input.as_bytes(),
            &records,
            &ledger,
            || panic!("a persisted duplicate must not load credentials"),
            |_, _| panic!("a persisted duplicate must not dispatch"),
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(std::fs::read(&stored[0].0).unwrap(), original);
        assert_eq!(load_records(&records).0.len(), 1);
        for bytes in [original, std::fs::read(ledger).unwrap()] {
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("synthetic prompt"));
            assert!(!text.contains("synthetic-key-marker"));
        }
    }

    #[test]
    fn interrupted_or_failed_dispatch_is_not_retried_and_no_outcome_is_invented() {
        for dispatched in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let records = dir.path().join("records");
            let ledger = dir.path().join("dedup.json");
            let key = "b".repeat(64);
            let input = event(&key);
            if dispatched {
                run_with(
                    input.as_bytes(),
                    &records,
                    &ledger,
                    || Ok("synthetic".into()),
                    |_, _| Err("jev_timeout".into()),
                )
                .unwrap();
            } else {
                // Simulate a process exit after the durable claim but before any result.
                assert!(claim(&ledger, &key).unwrap());
            }
            run_with(
                input.as_bytes(),
                &records,
                &ledger,
                || panic!("an uncertain or failed dispatch must not be retried"),
                |_, _| panic!("an uncertain or failed dispatch must not be retried"),
            )
            .unwrap();
            let (stored, error) = load_records(&records);
            assert!(error.is_none());
            assert_eq!(stored.len(), usize::from(dispatched));
            if dispatched {
                assert!(matches!(stored[0].1.status, JevStatus::Error));
                assert_eq!(stored[0].1.error.as_deref(), Some("jev_timeout"));
                assert!(stored[0].1.input_tokens.is_none());
            }
        }
    }

    #[test]
    fn missing_credentials_do_not_claim_and_existing_failure_remains_after_retry() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("records");
        let ledger = dir.path().join("dedup.json");
        let input = event(&"c".repeat(64));
        run_with(
            input.as_bytes(),
            &records,
            &ledger,
            || Err("jev_key_missing".into()),
            |_, _| panic!("missing credentials must not dispatch"),
        )
        .unwrap();
        assert!(!ledger.exists());
        let prior = load_records(&records).0;
        let prior_bytes = std::fs::read(&prior[0].0).unwrap();
        run_with(
            input.as_bytes(),
            &records,
            &ledger,
            || Ok("synthetic".into()),
            |_, _| Ok(parsed()),
        )
        .unwrap();
        assert_eq!(load_records(&records).0.len(), 2);
        assert_eq!(std::fs::read(&prior[0].0).unwrap(), prior_bytes);
    }

    #[test]
    fn failed_record_write_keeps_claim_to_prevent_repeating_a_paid_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("records");
        let ledger = dir.path().join("dedup.json");
        crate::client_adapters::atomic_write(&records, b"blocking fixture").unwrap();
        let input = event(&"d".repeat(64));
        assert_eq!(
            run_with(
                input.as_bytes(),
                &records,
                &ledger,
                || Ok("synthetic".into()),
                |_, _| Ok(parsed()),
            )
            .unwrap_err(),
            "jev_auto_records_write"
        );
        run_with(
            input.as_bytes(),
            &records,
            &ledger,
            || panic!("the attempt already has a durable claim"),
            |_, _| panic!("the attempt must not be sent again"),
        )
        .unwrap();
        assert_eq!(std::fs::read(records).unwrap(), b"blocking fixture");
    }

    #[test]
    fn invalid_dedup_ledgers_are_backed_up_preserved_and_block_dispatch() {
        for bytes in [
            b"broken".to_vec(),
            br#"{"schemaVersion":2,"keys":[],"future":"keep"}"#.to_vec(),
            vec![b'x'; MAX_DEDUP_BYTES as usize + 1],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let ledger = dir.path().join("dedup.json");
            let records = dir.path().join("records");
            crate::client_adapters::atomic_write(&ledger, &bytes).unwrap();
            for _ in 0..5 {
                assert_eq!(
                    run_with(
                        event(&"a".repeat(64)).as_bytes(),
                        &records,
                        &ledger,
                        || panic!("unreadable dedup state must block new calls"),
                        |_, _| panic!("unreadable dedup state must block new calls"),
                    )
                    .unwrap_err(),
                    "jev_auto_records_read"
                );
            }
            assert_eq!(std::fs::read(&ledger).unwrap(), bytes);
            assert!(!records.exists());
            let backups: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("dedup.json.headroom-local-community-backup-")
                })
                .collect();
            assert_eq!(backups.len(), 3);
            for backup in backups {
                assert_eq!(std::fs::read(backup.path()).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn dedup_retention_is_bounded_and_unknown_fields_survive_updates() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("dedup.json");
        crate::client_adapters::atomic_write(
            &ledger,
            br#"{"schemaVersion":1,"keys":[],"future":{"keep":true}}"#,
        )
        .unwrap();
        for index in 0..=MAX_DEDUP_KEYS {
            assert!(claim(&ledger, &format!("{index:064x}")).unwrap());
        }
        let loaded = read_dedup(&ledger).unwrap();
        assert_eq!(loaded.keys.len(), MAX_DEDUP_KEYS);
        assert!(!loaded.keys.contains(&format!("{:064x}", 0)));
        assert!(loaded.keys.contains(&format!("{MAX_DEDUP_KEYS:064x}")));
        assert_eq!(loaded.extra["future"]["keep"], true);
        assert!(std::fs::metadata(ledger).unwrap().len() <= MAX_DEDUP_BYTES);
    }

    #[test]
    #[ignore = "subprocess helper for the cross-process claim test"]
    fn dedup_claim_process_helper() {
        let root = PathBuf::from(std::env::var_os("HEADROOM_JEV_DEDUP_TEST_DIR").unwrap());
        let actor = std::env::var("HEADROOM_JEV_DEDUP_TEST_ACTOR").unwrap();
        crate::client_adapters::atomic_write(&root.join(format!("ready-{actor}")), b"ready")
            .unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !root.join("start").exists() {
            assert!(
                Instant::now() < deadline,
                "parent did not release the test barrier"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let dispatched = std::cell::Cell::new(0);
        run_with(
            event(&"e".repeat(64)).as_bytes(),
            &root.join("records"),
            &root.join("dedup.json"),
            || Ok("synthetic".into()),
            |_, _| {
                dispatched.set(dispatched.get() + 1);
                Ok(parsed())
            },
        )
        .unwrap();
        println!("dispatch-count:{}", dispatched.get());
    }

    #[test]
    fn dedup_claim_is_atomic_across_processes() {
        let dir = tempfile::tempdir().unwrap();
        let mut children = Vec::new();
        for actor in ["a", "b"] {
            children.push(
                crate::proc::command(std::env::current_exe().unwrap())
                    .args([
                        "--ignored",
                        "--exact",
                        "jev::automatic::tests::dedup_claim_process_helper",
                        "--nocapture",
                    ])
                    .env("HEADROOM_JEV_DEDUP_TEST_DIR", dir.path())
                    .env("HEADROOM_JEV_DEDUP_TEST_ACTOR", actor)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !["a", "b"]
            .iter()
            .all(|actor| dir.path().join(format!("ready-{actor}")).exists())
        {
            if Instant::now() >= deadline {
                for child in &mut children {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                panic!("claim subprocesses did not reach the test barrier");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        crate::client_adapters::atomic_write(&dir.path().join("start"), b"go").unwrap();
        let outputs: Vec<_> = children
            .into_iter()
            .map(|child| child.wait_with_output().unwrap())
            .collect();
        for output in &outputs {
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            outputs
                .iter()
                .filter(
                    |output| String::from_utf8_lossy(&output.stdout).contains("dispatch-count:1")
                )
                .count(),
            1
        );
        let (stored, error) = load_records(&dir.path().join("records"));
        assert!(error.is_none());
        assert_eq!(stored.len(), 1);
        assert_eq!(
            read_dedup(&dir.path().join("dedup.json"))
                .unwrap()
                .keys
                .len(),
            1
        );
    }

    #[test]
    fn persisted_records_are_metadata_only_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..super::super::MAX_RECORDS + 2 {
            store_record(
                dir.path(),
                record_outcome(
                    Ok(super::super::ParsedResponse {
                        model: Some("jev-1.14.0".into()),
                        choice: "coding".into(),
                        confidence: Some(0.9),
                        input_tokens: Some(412),
                        output_tokens: Some(45),
                    }),
                    20,
                ),
            )
            .unwrap();
        }
        let (records, error) = load_records(dir.path());
        assert_eq!(records.len(), super::super::MAX_RECORDS);
        assert!(error.is_none());
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&records[0].0).unwrap()).unwrap();
        assert_eq!(stored["record"]["source"], "codex");
        assert!(stored["record"].get("text").is_none());
        assert!(stored["record"].get("key").is_none());
    }

    #[test]
    fn corrupt_and_future_records_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.json"), b"broken").unwrap();
        let mut future = serde_json::to_value(StoredRecord {
            schema_version: 2,
            record: record_outcome(Err("jev_timeout".into()), 10),
        })
        .unwrap();
        future["extra"] = serde_json::json!("future-data");
        let future_bytes = serde_json::to_vec(&future).unwrap();
        std::fs::write(dir.path().join("future.json"), &future_bytes).unwrap();
        store_record(dir.path(), record_outcome(Err("jev_timeout".into()), 10)).unwrap();
        let (records, error) = load_records(dir.path());
        assert_eq!(records.len(), 1);
        assert_eq!(error.as_deref(), Some("jev_auto_records_read"));
        assert_eq!(
            std::fs::read(dir.path().join("future.json")).unwrap(),
            future_bytes
        );
        assert_eq!(
            std::fs::read(dir.path().join("broken.json")).unwrap(),
            b"broken"
        );
    }
}
