use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::{JevDashboard, JevInner, JevProvider, JevRecord, JevSource, JevStatus};

const MAX_EVENT_BYTES: u64 = (super::MAX_INPUT_BYTES * 8) as u64;
const MAX_RECORD_BYTES: u64 = 8 * 1024;

#[derive(Deserialize)]
struct PromptEvent {
    text: String,
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
    let text = read_prompt(reader)?;
    let started = Instant::now();
    let outcome = (|| {
        let mut inner = JevInner::default();
        super::restore_jev_key_from_store_without_interaction(&mut inner)?;
        let key = inner.key.ok_or_else(|| "jev_key_missing".to_string())?;
        super::dispatch(super::JEV_URL, "jev-latest", &key, &text)
    })();
    let record = record_outcome(
        outcome,
        started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    );
    store_record(&records_dir(), record)
}

fn read_prompt(reader: impl Read) -> Result<String, String> {
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
    Ok(event.text)
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
    dashboard.auto_error = error;
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

    #[test]
    fn prompt_input_is_bounded_and_only_extracts_text() {
        assert_eq!(
            read_prompt(br#"{"text":"test","dedupKey":"opaque"}"#.as_slice()).unwrap(),
            "test"
        );
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
