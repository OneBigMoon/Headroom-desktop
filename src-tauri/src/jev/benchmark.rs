use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::{JevState, ParsedResponse};

const DATASET_VERSION: &str = "intent-v1";
const REQUESTED_MODEL: &str = "jev-latest";
const MAX_STORED_BYTES: u64 = 128 * 1024;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct BenchmarkSample {
    pub id: &'static str,
    pub text: &'static str,
    pub expected: &'static str,
}

// Public synthetic examples; expected labels are used locally and never sent to Jev.
const SAMPLES: [BenchmarkSample; 12] = [
    BenchmarkSample { id: "coding-zh-1", text: "请修复这段 Rust 程序的编译错误，并补充单元测试。", expected: "coding" },
    BenchmarkSample { id: "coding-en-1", text: "Implement a Python function that sorts a list of integers in ascending order.", expected: "coding" },
    BenchmarkSample { id: "coding-zh-2", text: "这个网页点击保存后没有反应，请定位 JavaScript 事件处理函数中的错误。", expected: "coding" },
    BenchmarkSample { id: "research-zh-1", text: "请查找有关海洋微塑料对珊瑚影响的研究论文，并列出来源。", expected: "research" },
    BenchmarkSample { id: "research-en-1", text: "Find published studies comparing the energy efficiency of heat pumps and gas boilers, with citations.", expected: "research" },
    BenchmarkSample { id: "research-en-2", text: "Investigate the historical causes of the fall of the Roman Empire using scholarly sources.", expected: "research" },
    BenchmarkSample { id: "writing-zh-1", text: "请写一封感谢老师耐心指导的信，语气真诚自然。", expected: "writing" },
    BenchmarkSample { id: "writing-en-1", text: "Write a short fictional story about a lighthouse keeper who discovers a message in a bottle.", expected: "writing" },
    BenchmarkSample { id: "writing-zh-2", text: "请把这句话改写得更简洁：由于目前时间方面的原因，我们不得不将会议向后推迟。", expected: "writing" },
    BenchmarkSample { id: "other-zh-1", text: "你好，早上好！", expected: "other" },
    BenchmarkSample { id: "other-en-1", text: "Thank you, goodbye!", expected: "other" },
    BenchmarkSample { id: "other-en-2", text: "I'm excited about my birthday tomorrow!", expected: "other" },
];

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BenchmarkCase {
    pub case_id: String,
    pub expected: String,
    pub predicted: Option<String>,
    pub correct: bool,
    pub status: String,
    pub model: Option<String>,
    pub latency_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub confidence: Option<f64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BenchmarkReport {
    pub id: String,
    pub dataset_version: String,
    pub started_at: String,
    pub completed_at: String,
    pub provider: String,
    pub requested_model: String,
    pub total: usize,
    pub successful: usize,
    pub failed: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub cases: Vec<BenchmarkCase>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkSnapshot {
    pub dataset_version: &'static str,
    pub samples: &'static [BenchmarkSample],
    pub busy: bool,
    pub result: Option<BenchmarkReport>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct StoredReport {
    schema_version: u32,
    report: BenchmarkReport,
}

#[tauri::command]
pub async fn get_jev_benchmark(state: State<'_, JevState>) -> Result<BenchmarkSnapshot, String> {
    super::run_state_task(state.inner(), |state| snapshot(state, &report_path())).await
}

#[tauri::command]
pub async fn run_jev_benchmark(state: State<'_, JevState>) -> Result<BenchmarkSnapshot, String> {
    super::run_state_task(state.inner(), |state| {
        run_with(state, &report_path(), |key, text| {
            super::dispatch(super::JEV_URL, REQUESTED_MODEL, key, text)
        })
    })
    .await
}

struct BusyGuard<'a>(&'a JevState);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.0.inner.lock() {
            inner.busy = false;
            inner.benchmark_busy = false;
        }
    }
}

fn begin(state: &JevState) -> Result<(BusyGuard<'_>, String), String> {
    let mut inner = super::try_lock_state(state)?;
    if inner.busy {
        return Err("jev_busy".into());
    }
    super::restore_jev_key_from_store(&mut inner)?;
    let key = inner.key.clone().ok_or("jev_key_missing")?;
    inner.busy = true;
    inner.benchmark_busy = true;
    Ok((BusyGuard(state), key))
}

fn run_with(
    state: &JevState,
    path: &Path,
    mut dispatch: impl FnMut(&str, &str) -> Result<ParsedResponse, String>,
) -> Result<BenchmarkSnapshot, String> {
    // Refuse to overwrite unreadable, corrupt, or future-version evidence.
    read_report(path)?;
    let (guard, key) = begin(state)?;
    let mut report = BenchmarkReport {
        id: super::new_id(),
        dataset_version: DATASET_VERSION.into(),
        started_at: chrono::Utc::now().to_rfc3339(),
        provider: "typesafe".into(),
        requested_model: REQUESTED_MODEL.into(),
        ..Default::default()
    };
    for sample in SAMPLES {
        let started = Instant::now();
        let outcome = dispatch(&key, sample.text);
        let mut case = BenchmarkCase {
            case_id: sample.id.into(),
            expected: sample.expected.into(),
            latency_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            ..Default::default()
        };
        match outcome {
            Ok(parsed) => {
                case.status = "success".into();
                case.predicted = Some(parsed.choice);
                case.model = parsed.model;
                case.input_tokens = parsed.input_tokens;
                case.output_tokens = parsed.output_tokens;
                case.confidence = parsed.confidence;
            }
            Err(error) => {
                case.status = "error".into();
                case.error = Some(super::bounded_error(&error));
            }
        }
        report.cases.push(case);
    }
    report.completed_at = chrono::Utc::now().to_rfc3339();
    validate_and_summarize(&mut report)?;
    write_report(path, report)?;
    drop(guard);
    snapshot(state, path)
}

fn report_path() -> PathBuf {
    crate::storage::app_data_dir()
        .join("telemetry")
        .join("jev-benchmark.json")
}

fn snapshot(state: &JevState, path: &Path) -> Result<BenchmarkSnapshot, String> {
    let busy = super::lock_state(state)?.benchmark_busy;
    Ok(BenchmarkSnapshot {
        dataset_version: DATASET_VERSION,
        samples: &SAMPLES,
        busy,
        result: read_report(path)?,
    })
}

fn read_report(path: &Path) -> Result<Option<BenchmarkReport>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("jev_benchmark_read".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STORED_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "jev_benchmark_read")?;
    if bytes.len() as u64 > MAX_STORED_BYTES {
        return Err("jev_benchmark_read".into());
    }
    let mut stored: StoredReport =
        serde_json::from_slice(&bytes).map_err(|_| "jev_benchmark_read")?;
    if stored.schema_version != 1 {
        return Err("jev_benchmark_read".into());
    }
    validate_and_summarize(&mut stored.report)?;
    Ok(Some(stored.report))
}

fn write_report(path: &Path, report: BenchmarkReport) -> Result<(), String> {
    read_report(path)?;
    let bytes = serde_json::to_vec(&StoredReport {
        schema_version: 1,
        report,
    })
    .map_err(|_| "jev_benchmark_write")?;
    if bytes.len() as u64 > MAX_STORED_BYTES {
        return Err("jev_benchmark_write".into());
    }
    let parent = path.parent().ok_or("jev_benchmark_write")?;
    std::fs::create_dir_all(parent).map_err(|_| "jev_benchmark_write")?;
    crate::client_adapters::atomic_write(path, &bytes).map_err(|_| "jev_benchmark_write".into())
}

fn validate_and_summarize(report: &mut BenchmarkReport) -> Result<(), String> {
    let invalid = || "jev_benchmark_read".to_string();
    if report.dataset_version != DATASET_VERSION
        || report.provider != "typesafe"
        || report.requested_model != REQUESTED_MODEL
        || report.id.is_empty()
        || report.id.len() > 128
        || report.cases.len() != SAMPLES.len()
    {
        return Err(invalid());
    }
    let started =
        chrono::DateTime::parse_from_rfc3339(&report.started_at).map_err(|_| invalid())?;
    let completed =
        chrono::DateTime::parse_from_rfc3339(&report.completed_at).map_err(|_| invalid())?;
    if completed < started {
        return Err(invalid());
    }
    for (case, sample) in report.cases.iter_mut().zip(SAMPLES) {
        if case.case_id != sample.id || case.expected != sample.expected {
            return Err(invalid());
        }
        if let Some(model) = &case.model {
            super::bounded_model(model).map_err(|_| invalid())?;
        }
        match case.status.as_str() {
            "success" => {
                if !case
                    .predicted
                    .as_deref()
                    .is_some_and(|value| super::CHOICES.contains(&value))
                    || !case
                        .confidence
                        .is_some_and(|value| value.is_finite() && (0.0..=1.0).contains(&value))
                    || case.error.is_some()
                {
                    return Err(invalid());
                }
            }
            "error" => {
                if case.predicted.is_some()
                    || case.confidence.is_some()
                    || case.model.is_some()
                    || case.input_tokens.is_some()
                    || case.output_tokens.is_some()
                    || !case.error.as_deref().is_some_and(|value| {
                        !value.is_empty()
                            && value.len() <= super::MAX_ERROR_BYTES
                            && !value.chars().any(char::is_control)
                    })
                {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        case.correct =
            case.status == "success" && case.predicted.as_deref() == Some(sample.expected);
    }
    report.total = SAMPLES.len();
    report.successful = report
        .cases
        .iter()
        .filter(|case| case.status == "success")
        .count();
    report.failed = report.total - report.successful;
    report.correct = report.cases.iter().filter(|case| case.correct).count();
    report.accuracy = report.correct as f64 * 100.0 / report.total as f64;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_key() -> JevState {
        let state = JevState::default();
        state.inner.lock().unwrap().key = Some("synthetic-benchmark-test-key".into());
        state
    }

    fn parsed(choice: &str) -> Result<ParsedResponse, String> {
        Ok(ParsedResponse {
            model: Some("jev-1.14.0".into()),
            choice: choice.into(),
            confidence: Some(0.9),
            input_tokens: Some(25),
            output_tokens: Some(1),
        })
    }

    #[test]
    fn scores_correct_wrong_and_failed_cases_without_leaking_labels_or_key() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("benchmark.json");
        let state = state_with_key();
        let mut index = 0;
        let result = run_with(&state, &path, |key, text| {
            assert_eq!(key, "synthetic-benchmark-test-key");
            assert_eq!(text, SAMPLES[index].text);
            index += 1;
            match index {
                1 => parsed("research"),
                2 => Err("jev_network".into()),
                _ => parsed(SAMPLES[index - 1].expected),
            }
        })
        .unwrap();
        let report = result.result.unwrap();
        assert_eq!(
            (
                report.total,
                report.successful,
                report.failed,
                report.correct
            ),
            (12, 11, 1, 10)
        );
        assert!((report.accuracy - 100.0 * 10.0 / 12.0).abs() < 0.000001);
        assert!(!report.cases[0].correct && !report.cases[1].correct);
        assert_eq!(report.cases[1].input_tokens, None);
        assert!(!result.busy);
        assert!(state.inner.lock().unwrap().records.is_empty());
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.contains("synthetic-benchmark-test-key"));
        assert!(!stored.contains(SAMPLES[0].text));
        assert_eq!(read_report(&path).unwrap().unwrap().correct, 10);
    }

    #[test]
    fn recomputes_persisted_totals_and_rejects_incomplete_or_changed_labels() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("benchmark.json");
        let mut index = 0;
        let result = run_with(&state_with_key(), &path, |_, _| {
            let outcome = parsed(SAMPLES[index].expected);
            index += 1;
            outcome
        })
        .unwrap();
        let mut report = result.result.unwrap();
        report.accuracy = 0.0;
        report.correct = 0;
        report.cases[0].correct = false;
        write_report(&path, report.clone()).unwrap();
        let restored = read_report(&path).unwrap().unwrap();
        assert_eq!(restored.accuracy, 100.0);
        assert_eq!(restored.correct, 12);
        report.cases[0].expected = "other".into();
        assert!(validate_and_summarize(&mut report).is_err());
        report.cases.remove(0);
        assert!(validate_and_summarize(&mut report).is_err());
    }

    #[test]
    fn rejects_missing_credentials_and_busy_state_before_dispatch() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("benchmark.json");
        let missing = JevState::default();
        assert_eq!(
            run_with(&missing, &path, |_, _| panic!("must not send")).unwrap_err(),
            "jev_key_missing"
        );
        assert!(!missing.inner.lock().unwrap().busy);
        let state = state_with_key();
        let (guard, _) = begin(&state).unwrap();
        assert!(snapshot(&state, &path).unwrap().busy);
        assert_eq!(
            run_with(&state, &path, |_, _| panic!("must not send")).unwrap_err(),
            "jev_busy"
        );
        drop(guard);
        assert!(!state.inner.lock().unwrap().busy);
        assert!(!path.exists());
    }

    #[test]
    fn preserves_corrupt_future_and_oversized_files_without_sending() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("benchmark.json");
        for bytes in [
            b"broken".to_vec(),
            br#"{"schema_version":2,"report":{}}"#.to_vec(),
            vec![b' '; MAX_STORED_BYTES as usize + 1],
        ] {
            std::fs::write(&path, &bytes).unwrap();
            let state = state_with_key();
            assert_eq!(
                run_with(&state, &path, |_, _| panic!("must not send")).unwrap_err(),
                "jev_benchmark_read"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(!state.inner.lock().unwrap().busy);
        }
    }

    #[test]
    fn write_failure_releases_busy_and_all_failed_calls_stay_zero() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("benchmark.json");
        let state = state_with_key();
        let result = run_with(&state, &path, |_, _| Err("jev_timeout".into())).unwrap();
        let report = result.result.unwrap();
        assert_eq!(
            (report.correct, report.successful, report.failed),
            (0, 0, 12)
        );
        assert_eq!(report.accuracy, 0.0);
        let outcome = run_with(&state, &path, |_, _| {
            std::fs::remove_file(&path).ok();
            std::fs::create_dir(&path).ok();
            Err("jev_timeout".into())
        });
        assert!(outcome.is_err());
        assert!(!state.inner.lock().unwrap().busy);
        assert!(!state.inner.lock().unwrap().benchmark_busy);
        assert!(path.is_dir());
    }
}
