use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

#[cfg(all(not(debug_assertions), target_os = "macos"))]
use crate::keychain;
use crate::system_one_adapter::{self, AdapterConfig, AdapterProtocol};

const JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";
#[cfg(all(not(debug_assertions), target_os = "macos"))]
const JEV_KEYCHAIN_SERVICE: &str = "org.headroomlocal.community.jev";
#[cfg(all(not(debug_assertions), target_os = "macos"))]
const JEV_KEYCHAIN_ACCOUNT: &str = "typesafe";
const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const MAX_RECORDS: usize = 200;
const MAX_MODEL_BYTES: usize = 128;
const MAX_ERROR_BYTES: usize = 64;

const CHOICES: [&str; 4] = ["coding", "research", "writing", "other"];

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub enum JevProvider {
    #[serde(rename = "typesafe")]
    TypeSafe,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "qwen")]
    Qwen,
    #[serde(rename = "volcengine")]
    Volcengine,
    #[serde(rename = "minimax_cn")]
    MiniMaxCn,
    #[serde(rename = "minimax_global")]
    MiniMaxGlobal,
    #[serde(rename = "bigmodel")]
    BigModel,
    #[serde(rename = "mimo")]
    Mimo,
    #[serde(rename = "siliconflow")]
    SiliconFlow,
    #[serde(rename = "zai")]
    Zai,
    #[serde(rename = "kimi_cn")]
    KimiCn,
    #[serde(rename = "kimi_global")]
    KimiGlobal,
    #[serde(rename = "byteplus")]
    BytePlus,
    #[serde(rename = "aws_bedrock_mantle")]
    AwsBedrockMantle,
    #[serde(rename = "tencent_tokenhub")]
    TencentTokenHub,
    #[serde(rename = "modelscope")]
    ModelScope,
    #[serde(rename = "ppio")]
    Ppio,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "opencode_go")]
    OpenCodeGo,
    #[serde(rename = "custom")]
    Custom,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JevDashboard {
    pub key_configured: bool,
    pub key_persistent: bool,
    pub configured_targets: Vec<ConfiguredTarget>,
    pub busy: bool,
    pub records: Vec<JevRecord>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredTarget {
    pub provider: JevProvider,
    pub protocol: AdapterProtocol,
    pub url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JevRecord {
    pub id: String,
    pub timestamp: String,
    pub provider: JevProvider,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub status: JevStatus,
    pub latency_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub choice: Option<String>,
    pub confidence: Option<f64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JevStatus {
    Success,
    Error,
}

#[derive(Default)]
pub struct JevState {
    inner: Mutex<JevInner>,
}

#[derive(Default)]
struct JevInner {
    key: Option<String>,
    adapter_keys: HashMap<JevProvider, AdapterCredential>,
    busy: bool,
    records: Vec<JevRecord>,
}

struct AdapterCredential {
    protocol: AdapterProtocol,
    url: String,
    key: String,
}

#[derive(Debug)]
struct ParsedResponse {
    model: Option<String>,
    choice: String,
    confidence: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct JevResponse {
    answers: Option<JevAnswers>,
    usage: Option<JevUsage>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JevAnswers {
    category: Option<JevCategoryAnswer>,
}

#[derive(Debug, Deserialize)]
struct JevCategoryAnswer {
    #[serde(rename = "type")]
    answer_type: Option<String>,
    choice: Option<String>,
    confidence: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct JevUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[tauri::command]
pub fn get_jev_dashboard(state: State<'_, JevState>) -> Result<JevDashboard, String> {
    dashboard(&state)
}

#[tauri::command]
pub async fn get_jev_models(
    state: State<'_, JevState>,
    config: AdapterConfig,
) -> Result<Vec<String>, String> {
    let provider = config.provider;
    if matches!(
        provider,
        JevProvider::TypeSafe | JevProvider::OpenAi | JevProvider::Custom
    ) {
        return Err("jev_catalog_unsupported".into());
    }
    let target = system_one_adapter::resolve(&config)?;
    let key = if crate::jev_catalog::requires_key(provider) {
        let inner = lock_state(&state)?;
        adapter_key(&inner, &target)?
    } else {
        None
    };
    tauri::async_runtime::spawn_blocking(move || {
        crate::jev_catalog::fetch_models(&config, target.protocol, key.as_deref())
    })
    .await
    .map_err(|_| "jev_catalog_unavailable".to_string())?
}

#[tauri::command]
pub fn set_jev_key(state: State<'_, JevState>, key: String) -> Result<(), String> {
    let mut inner = lock_state(&state)?;
    #[cfg(all(not(debug_assertions), target_os = "macos"))]
    {
        set_jev_key_with_store(
            &mut inner,
            key,
            |secret| keychain::write_secret(JEV_KEYCHAIN_SERVICE, JEV_KEYCHAIN_ACCOUNT, secret),
            || keychain::delete_secret(JEV_KEYCHAIN_SERVICE, JEV_KEYCHAIN_ACCOUNT),
        )
    }
    #[cfg(not(all(not(debug_assertions), target_os = "macos")))]
    {
        if inner.busy {
            return Err("jev_busy".to_string());
        }
        inner.key = session_key(key)?;
        Ok(())
    }
}

#[cfg(any(test, all(not(debug_assertions), target_os = "macos")))]
fn set_jev_key_with_store(
    inner: &mut JevInner,
    key: String,
    write: impl FnOnce(&str) -> Result<(), String>,
    delete: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if inner.busy {
        return Err("jev_busy".to_string());
    }
    let parsed = session_key(key)?;
    let stored = match parsed.as_deref() {
        Some(secret) => write(secret).map_err(|_| "jev_key_store_write".to_string()),
        None => delete().map_err(|_| "jev_key_store_delete".to_string()),
    };
    if let Err(error) = stored {
        // A failed update may have changed the store. Never claim the cached
        // credential is still durable.
        inner.key = None;
        return Err(error);
    }
    inner.key = parsed;
    Ok(())
}

#[cfg(any(test, all(not(debug_assertions), target_os = "macos")))]
fn restore_jev_key(
    inner: &mut JevInner,
    read: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<(), String> {
    let stored = read().map_err(|_| "jev_key_store_read".to_string());
    inner.key = None;
    inner.key = stored?
        .map(session_key)
        .transpose()
        .map_err(|_| "jev_key_store_invalid".to_string())?
        .flatten();
    Ok(())
}

#[cfg(all(not(debug_assertions), target_os = "macos"))]
fn restore_jev_key_from_store(inner: &mut JevInner) -> Result<(), String> {
    restore_jev_key(inner, || {
        keychain::read_secret(JEV_KEYCHAIN_SERVICE, JEV_KEYCHAIN_ACCOUNT)
    })
}

#[cfg(not(all(not(debug_assertions), target_os = "macos")))]
fn restore_jev_key_from_store(_inner: &mut JevInner) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub fn set_adapter_key(
    state: State<'_, JevState>,
    config: AdapterConfig,
    key: String,
) -> Result<(), String> {
    let target = system_one_adapter::resolve(&config)?;
    let parsed_key = session_key(key)?;
    if parsed_key.is_none() && !target.local {
        let mut inner = lock_state(&state)?;
        inner.adapter_keys.remove(&target.provider);
        return Ok(());
    }
    let mut inner = lock_state(&state)?;
    match parsed_key {
        Some(key) => {
            inner.adapter_keys.insert(
                target.provider,
                AdapterCredential {
                    protocol: target.protocol,
                    url: target.url,
                    key,
                },
            );
        }
        None => {
            inner.adapter_keys.remove(&target.provider);
        }
    }
    Ok(())
}

fn session_key(key: String) -> Result<Option<String>, String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > 4096 || trimmed.chars().any(char::is_control) {
        return Err("jev_invalid_key".to_string());
    }
    Ok(Some(trimmed.to_string()))
}

fn adapter_key(
    inner: &JevInner,
    target: &system_one_adapter::ResolvedTarget,
) -> Result<Option<String>, String> {
    match inner.adapter_keys.get(&target.provider) {
        Some(credential)
            if credential.protocol == target.protocol && credential.url == target.url =>
        {
            Ok(Some(credential.key.clone()))
        }
        _ if target.local => Ok(None),
        _ => Err("adapter_key_missing".into()),
    }
}

#[tauri::command]
pub async fn evaluate_jev(
    state: State<'_, JevState>,
    text: String,
    config: AdapterConfig,
) -> Result<JevDashboard, String> {
    if text.trim().is_empty() {
        return Err("jev_input_empty".to_string());
    }
    if text.len() > MAX_INPUT_BYTES {
        return Err("jev_input_too_large".to_string());
    }
    validate_evaluation_model(&config)?;
    let provider = config.provider;
    let target = if provider == JevProvider::TypeSafe {
        None
    } else {
        Some(system_one_adapter::resolve(&config)?)
    };
    let endpoint = target.as_ref().map(|target| target.url.clone());

    let key = {
        let mut inner = try_lock_state(&state)?;
        let key = if provider == JevProvider::TypeSafe {
            restore_jev_key_from_store(&mut inner)?;
            Some(inner.key.clone().ok_or("jev_key_missing")?)
        } else {
            let target = target.as_ref().ok_or("adapter_invalid_provider")?;
            adapter_key(&inner, target)?
        };
        if inner.busy {
            return Err("jev_busy".to_string());
        }
        inner.busy = true;
        key
    };

    let started = Instant::now();
    let result = tauri::async_runtime::spawn_blocking(move || match target {
        None => dispatch(key.as_deref().unwrap_or_default(), &text),
        Some(target) => {
            system_one_adapter::dispatch(&target, key.as_deref(), &text).map(|decision| {
                ParsedResponse {
                    model: decision.model,
                    choice: decision.choice,
                    confidence: None,
                    input_tokens: decision.input_tokens,
                    output_tokens: decision.output_tokens,
                }
            })
        }
    })
    .await;
    let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let outcome = match result {
        Ok(result) => result,
        Err(_) => Err("jev_internal".to_string()),
    };

    let mut inner = lock_state(&state)?;
    inner.busy = false;

    match outcome {
        Ok(parsed) => {
            inner.records.push(JevRecord {
                id: new_id(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                provider,
                endpoint: endpoint.clone(),
                model: parsed.model,
                status: JevStatus::Success,
                latency_ms,
                input_tokens: parsed.input_tokens,
                output_tokens: parsed.output_tokens,
                choice: Some(parsed.choice),
                confidence: parsed.confidence,
                error: None,
            });
            trim_records(&mut inner.records);
            Ok(snapshot(&inner))
        }
        Err(error) => {
            let safe_error = bounded_error(&error);
            inner.records.push(JevRecord {
                id: new_id(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                provider,
                endpoint,
                model: None,
                status: JevStatus::Error,
                latency_ms,
                input_tokens: None,
                output_tokens: None,
                choice: None,
                confidence: None,
                error: Some(safe_error.clone()),
            });
            trim_records(&mut inner.records);
            Err(safe_error)
        }
    }
}

fn provider_requires_model(config: &AdapterConfig) -> bool {
    config.provider != JevProvider::TypeSafe
}

fn validate_evaluation_model(config: &AdapterConfig) -> Result<(), String> {
    if provider_requires_model(config) {
        let model = config.model.as_deref().unwrap_or("");
        if model.is_empty() || model == "catalog" {
            return Err("adapter_invalid_model".into());
        }
    }
    Ok(())
}

fn dispatch(key: &str, text: &str) -> Result<ParsedResponse, String> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| "jev_client".to_string())?;

    let request = native_request(text);

    let response = client
        .post(JEV_URL)
        .bearer_auth(key)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(request.to_string())
        .send()
        .map_err(|error| {
            if error.is_timeout() {
                "jev_timeout".to_string()
            } else {
                "jev_network".to_string()
            }
        })?;

    if !response.status().is_success() {
        return Err(if response.status().is_client_error() {
            "jev_http_4xx".to_string()
        } else if response.status().is_server_error() {
            "jev_http_5xx".to_string()
        } else {
            "jev_http_status".to_string()
        });
    }

    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|_| "jev_response_read".to_string())?;
    let response: JevResponse =
        serde_json::from_slice(&bytes).map_err(|_| "jev_invalid_response".to_string())?;
    parse_response(response)
}

fn native_request(text: &str) -> serde_json::Value {
    json!({
        "state": text,
        "model": "jev-latest",
        "questions": {
            "category": {
                "type": "choice",
                "instructions": "Classify the input into exactly one category. Return only a valid choice from the provided criteria.",
                "criteria": {
                    "coding": "Software development, debugging, code review, or technical implementation.",
                    "research": "Information gathering, investigation, comparison, or analysis.",
                    "writing": "Drafting, editing, rewriting, summarizing, or other language-focused work.",
                    "other": "Anything that does not clearly fit coding, research, or writing."
                }
            }
        },
    })
}

fn parse_response(response: JevResponse) -> Result<ParsedResponse, String> {
    let category = response
        .answers
        .and_then(|answers| answers.category)
        .ok_or_else(|| "jev_missing_category".to_string())?;
    if category.answer_type.as_deref() != Some("choice") {
        return Err("jev_invalid_answer_type".to_string());
    }

    let choice = category
        .choice
        .filter(|choice| CHOICES.contains(&choice.as_str()))
        .ok_or_else(|| "jev_invalid_choice".to_string())?;
    let confidence = category
        .confidence
        .filter(|confidence| confidence.is_finite() && (0.0..=1.0).contains(confidence))
        .ok_or_else(|| "jev_invalid_confidence".to_string())?;
    let model = response
        .model
        .map(|model| bounded_model(&model))
        .transpose()?
        .flatten();

    Ok(ParsedResponse {
        model,
        choice,
        confidence: Some(confidence),
        input_tokens: response.usage.as_ref().and_then(|usage| usage.input_tokens),
        output_tokens: response.usage.and_then(|usage| usage.output_tokens),
    })
}

fn bounded_model(model: &str) -> Result<Option<String>, String> {
    if model.is_empty() {
        return Ok(None);
    }
    if model.len() > MAX_MODEL_BYTES || !model.is_ascii() || model.chars().any(char::is_control) {
        return Err("jev_invalid_model".to_string());
    }
    Ok(Some(model.to_string()))
}

fn dashboard(state: &JevState) -> Result<JevDashboard, String> {
    let mut inner = lock_state(state)?;
    restore_jev_key_from_store(&mut inner)?;
    Ok(snapshot(&inner))
}

fn snapshot(inner: &JevInner) -> JevDashboard {
    JevDashboard {
        key_configured: inner.key.is_some(),
        key_persistent: cfg!(all(not(debug_assertions), target_os = "macos")),
        configured_targets: inner
            .adapter_keys
            .iter()
            .map(|(provider, credential)| ConfiguredTarget {
                provider: *provider,
                protocol: credential.protocol,
                url: credential.url.clone(),
            })
            .collect(),
        busy: inner.busy,
        records: inner.records.clone(),
    }
}

fn lock_state<'a>(state: &'a JevState) -> Result<std::sync::MutexGuard<'a, JevInner>, String> {
    state
        .inner
        .lock()
        .map_err(|_| "jev_state_unavailable".to_string())
}

fn try_lock_state<'a>(state: &'a JevState) -> Result<std::sync::MutexGuard<'a, JevInner>, String> {
    match state.inner.try_lock() {
        Ok(guard) => Ok(guard),
        Err(std::sync::TryLockError::Poisoned(_)) => Err("jev_state_unavailable".to_string()),
        Err(std::sync::TryLockError::WouldBlock) => Err("jev_busy".to_string()),
    }
}

fn trim_records(records: &mut Vec<JevRecord>) {
    if records.len() > MAX_RECORDS {
        let excess = records.len() - MAX_RECORDS;
        records.drain(..excess);
    }
}

fn new_id() -> String {
    format!("jev-{}", uuid_like_timestamp())
}

fn uuid_like_timestamp() -> String {
    format!(
        "{}-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        std::process::id()
    )
}

fn bounded_error(error: &str) -> String {
    error.chars().take(MAX_ERROR_BYTES).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_response() -> JevResponse {
        serde_json::from_value(json!({
            "model": "jev-latest",
            "answers": { "category": {
                "type": "choice", "choice": "coding", "confidence": 0.75
            }},
            "usage": { "input_tokens": 12, "output_tokens": 4 }
        }))
        .expect("valid response")
    }

    #[test]
    fn parses_valid_response_and_optional_usage() {
        let parsed = parse_response(valid_response()).expect("parse");
        assert_eq!(parsed.choice, "coding");
        assert_eq!(parsed.input_tokens, Some(12));
        assert_eq!(parsed.output_tokens, Some(4));
        assert_eq!(parsed.model.as_deref(), Some("jev-latest"));
    }

    #[test]
    fn preserves_missing_and_zero_output_usage() {
        let missing: JevResponse = serde_json::from_value(json!({
            "answers": { "category": { "type": "choice", "choice": "coding", "confidence": 0.75 } },
            "usage": { "input_tokens": 12 }
        }))
        .expect("deserialize missing output usage");
        assert_eq!(parse_response(missing).expect("parse").output_tokens, None);

        let zero: JevResponse = serde_json::from_value(json!({
            "answers": { "category": { "type": "choice", "choice": "coding", "confidence": 0.75 } },
            "usage": { "input_tokens": 12, "output_tokens": 0 }
        }))
        .expect("deserialize zero output usage");
        assert_eq!(parse_response(zero).expect("parse").output_tokens, Some(0));
    }

    #[test]
    fn native_request_puts_text_in_state() {
        let request = native_request("classify this");
        assert_eq!(request["state"], "classify this");
        assert!(request.get("input").is_none());
        assert_eq!(request["questions"]["category"]["type"], "choice");
    }

    #[test]
    fn rejects_missing_answer_and_invalid_confidence() {
        let missing: JevResponse = serde_json::from_value(json!({})).expect("deserialize");
        assert_eq!(
            parse_response(missing).unwrap_err().as_str(),
            "jev_missing_category"
        );

        let invalid: JevResponse = serde_json::from_value(json!({
            "answers": { "category": {
                "type": "choice", "choice": "coding", "confidence": 1.5
            }}
        }))
        .expect("deserialize");
        assert_eq!(
            parse_response(invalid).unwrap_err().as_str(),
            "jev_invalid_confidence"
        );
    }

    #[test]
    fn bounds_errors_and_records() {
        assert_eq!(bounded_error(&"x".repeat(100)), "x".repeat(MAX_ERROR_BYTES));
        let mut records = (0..201)
            .map(|_| JevRecord {
                id: "x".into(),
                timestamp: "x".into(),
                provider: JevProvider::TypeSafe,
                endpoint: None,
                model: None,
                status: JevStatus::Error,
                latency_ms: 0,
                input_tokens: None,
                output_tokens: None,
                choice: None,
                confidence: None,
                error: Some("e".into()),
            })
            .collect();
        trim_records(&mut records);
        assert_eq!(records.len(), MAX_RECORDS);
    }

    #[test]
    fn busy_gate_is_try_lock_compatible() {
        let state = JevState::default();
        let mut inner = state.inner.lock().expect("lock");
        inner.busy = true;
        assert!(inner.busy);
    }

    #[test]
    fn typesafe_key_survives_state_restart_and_disconnect() {
        use std::cell::RefCell;

        let stored = RefCell::new(None::<String>);
        let mut first_run = JevInner::default();
        set_jev_key_with_store(
            &mut first_run,
            "  typesafe-test-secret  ".into(),
            |secret| {
                *stored.borrow_mut() = Some(secret.to_string());
                Ok(())
            },
            || panic!("save must not delete"),
        )
        .expect("save");
        assert!(snapshot(&first_run).key_configured);
        assert!(!serde_json::to_string(&snapshot(&first_run))
            .unwrap()
            .contains("typesafe-test-secret"));

        let mut restarted = JevInner::default();
        restore_jev_key(&mut restarted, || Ok(stored.borrow().clone())).expect("restore");
        assert!(snapshot(&restarted).key_configured);
        assert_eq!(restarted.key.as_deref(), Some("typesafe-test-secret"));

        set_jev_key_with_store(
            &mut restarted,
            String::new(),
            |_| panic!("disconnect must not write"),
            || {
                stored.borrow_mut().take();
                Ok(())
            },
        )
        .expect("disconnect");
        let mut after_disconnect = JevInner::default();
        restore_jev_key(&mut after_disconnect, || Ok(stored.borrow().clone()))
            .expect("restore after disconnect");
        assert!(!snapshot(&after_disconnect).key_configured);
    }

    #[test]
    fn typesafe_key_store_failure_does_not_claim_success() {
        let mut inner = JevInner {
            key: Some("old-test-secret".into()),
            ..Default::default()
        };
        assert_eq!(
            set_jev_key_with_store(
                &mut inner,
                "new-test-secret".into(),
                |_| Err("store unavailable".into()),
                || panic!("save must not delete"),
            )
            .unwrap_err(),
            "jev_key_store_write"
        );
        assert!(!snapshot(&inner).key_configured);

        inner.key = Some("stale-test-secret".into());
        assert_eq!(
            restore_jev_key(&mut inner, || Err("store unavailable".into())).unwrap_err(),
            "jev_key_store_read"
        );
        assert!(!snapshot(&inner).key_configured);
        restore_jev_key(&mut inner, || Ok(Some("old-test-secret".into())))
            .expect("retry after store recovers");
        assert!(snapshot(&inner).key_configured);

        assert_eq!(
            set_jev_key_with_store(
                &mut inner,
                String::new(),
                |_| panic!("disconnect must not write"),
                || Err("store unavailable".into()),
            )
            .unwrap_err(),
            "jev_key_store_delete"
        );
        assert!(!snapshot(&inner).key_configured);
    }

    #[test]
    fn typesafe_key_store_rejects_invalid_value_without_exposing_it() {
        let mut inner = JevInner::default();
        assert_eq!(
            restore_jev_key(&mut inner, || Ok(Some("bad\nsecret".into()))).unwrap_err(),
            "jev_key_store_invalid"
        );
        assert!(!snapshot(&inner).key_configured);
    }

    #[test]
    fn typesafe_key_store_rechecks_external_changes() {
        let mut inner = JevInner::default();
        restore_jev_key(&mut inner, || Ok(Some("first-test-secret".into()))).expect("first read");
        assert_eq!(inner.key.as_deref(), Some("first-test-secret"));
        restore_jev_key(&mut inner, || Ok(Some("updated-test-secret".into())))
            .expect("external update");
        assert_eq!(inner.key.as_deref(), Some("updated-test-secret"));
        restore_jev_key(&mut inner, || Ok(None)).expect("external delete");
        assert!(!snapshot(&inner).key_configured);
    }

    #[test]
    fn typesafe_key_cannot_disconnect_during_request() {
        let mut inner = JevInner {
            key: Some("active-test-secret".into()),
            busy: true,
            ..Default::default()
        };
        assert_eq!(
            set_jev_key_with_store(
                &mut inner,
                String::new(),
                |_| panic!("busy request must not write"),
                || panic!("busy request must not delete"),
            )
            .unwrap_err(),
            "jev_busy"
        );
        assert_eq!(inner.key.as_deref(), Some("active-test-secret"));
    }

    #[cfg(all(not(debug_assertions), target_os = "macos"))]
    #[test]
    fn release_keychain_round_trip_uses_isolated_service() {
        let service = format!("{}.test.{}", JEV_KEYCHAIN_SERVICE, uuid::Uuid::new_v4());
        let result = (|| -> Result<(), String> {
            let mut first_run = JevInner::default();
            set_jev_key_with_store(
                &mut first_run,
                "jev-test-only".into(),
                |secret| keychain::write_secret(&service, JEV_KEYCHAIN_ACCOUNT, secret),
                || keychain::delete_secret(&service, JEV_KEYCHAIN_ACCOUNT),
            )?;
            let mut restarted = JevInner::default();
            restore_jev_key(&mut restarted, || {
                keychain::read_secret(&service, JEV_KEYCHAIN_ACCOUNT)
            })?;
            assert!(snapshot(&restarted).key_configured);
            set_jev_key_with_store(
                &mut restarted,
                String::new(),
                |secret| keychain::write_secret(&service, JEV_KEYCHAIN_ACCOUNT, secret),
                || keychain::delete_secret(&service, JEV_KEYCHAIN_ACCOUNT),
            )?;
            assert_eq!(keychain::read_secret(&service, JEV_KEYCHAIN_ACCOUNT)?, None);
            Ok(())
        })();
        let cleanup = keychain::delete_secret(&service, JEV_KEYCHAIN_ACCOUNT);
        assert!(cleanup.is_ok(), "isolated test keychain cleanup failed");
        result.expect("release keychain round trip");
    }

    #[test]
    fn provider_keys_are_separate_and_not_exposed_in_dashboard() {
        let mut inner = JevInner::default();
        inner.adapter_keys.insert(
            JevProvider::DeepSeek,
            AdapterCredential {
                protocol: AdapterProtocol::OpenAi,
                url: "https://api.deepseek.com/chat/completions".into(),
                key: "deepseek-test-secret".into(),
            },
        );
        let view = snapshot(&inner);
        assert!(!view.key_configured);
        assert_eq!(view.configured_targets.len(), 1);
        assert!(!serde_json::to_string(&view)
            .unwrap()
            .contains("deepseek-test-secret"));
        assert!(session_key("bad\nkey".into()).is_err());
        assert_eq!(session_key("  ".into()).unwrap(), None);
    }

    #[test]
    fn custom_key_is_bound_to_exact_url_and_protocol() {
        let mut inner = JevInner::default();
        let config = |protocol, url: &str| AdapterConfig {
            provider: JevProvider::Custom,
            protocol: Some(protocol),
            url: Some(url.into()),
            model: Some("test-model".into()),
            region: None,
            endpoint_id: None,
            workspace_id: None,
        };
        let original = system_one_adapter::resolve(&config(
            AdapterProtocol::OpenAi,
            "https://one.example/v1/chat/completions",
        ))
        .unwrap();
        inner.adapter_keys.insert(
            JevProvider::Custom,
            AdapterCredential {
                protocol: original.protocol,
                url: original.url.clone(),
                key: "secret".into(),
            },
        );
        assert_eq!(
            adapter_key(&inner, &original).unwrap().as_deref(),
            Some("secret")
        );
        let other_url = system_one_adapter::resolve(&config(
            AdapterProtocol::OpenAi,
            "https://two.example/v1/chat/completions",
        ))
        .unwrap();
        assert_eq!(
            adapter_key(&inner, &other_url).unwrap_err(),
            "adapter_key_missing"
        );
        let other_protocol = system_one_adapter::resolve(&config(
            AdapterProtocol::Anthropic,
            "https://one.example/v1/chat/completions",
        ))
        .unwrap();
        assert_eq!(
            adapter_key(&inner, &other_protocol).unwrap_err(),
            "adapter_key_missing"
        );
    }

    #[test]
    fn evaluation_requires_a_real_model_after_catalog_binding() {
        let mut config = AdapterConfig {
            provider: JevProvider::DeepSeek,
            protocol: Some(AdapterProtocol::OpenAi),
            url: None,
            model: Some(String::new()),
            region: None,
            endpoint_id: None,
            workspace_id: None,
        };
        assert_eq!(
            validate_evaluation_model(&config).unwrap_err(),
            "adapter_invalid_model"
        );
        config.model = Some("catalog".into());
        assert_eq!(
            validate_evaluation_model(&config).unwrap_err(),
            "adapter_invalid_model"
        );
        config.model = Some("deepseek-chat".into());
        assert!(validate_evaluation_model(&config).is_ok());
    }

    #[test]
    fn opencode_go_key_does_not_cross_protocol_or_provider() {
        let config = |provider, protocol| AdapterConfig {
            provider,
            protocol,
            url: None,
            model: Some("test-model".into()),
            region: None,
            endpoint_id: None,
            workspace_id: None,
        };
        let chat = system_one_adapter::resolve(&config(
            JevProvider::OpenCodeGo,
            Some(AdapterProtocol::OpenAi),
        ))
        .unwrap();
        let messages = system_one_adapter::resolve(&config(
            JevProvider::OpenCodeGo,
            Some(AdapterProtocol::Anthropic),
        ))
        .unwrap();
        let openai = system_one_adapter::resolve(&config(JevProvider::OpenAi, None)).unwrap();
        let mut inner = JevInner::default();
        inner.adapter_keys.insert(
            JevProvider::OpenCodeGo,
            AdapterCredential {
                protocol: chat.protocol,
                url: chat.url.clone(),
                key: "go-secret".into(),
            },
        );
        assert_eq!(
            adapter_key(&inner, &chat).unwrap().as_deref(),
            Some("go-secret")
        );
        assert_eq!(
            adapter_key(&inner, &messages).unwrap_err(),
            "adapter_key_missing"
        );
        assert_eq!(
            adapter_key(&inner, &openai).unwrap_err(),
            "adapter_key_missing"
        );
        assert!(!serde_json::to_string(&snapshot(&inner))
            .unwrap()
            .contains("go-secret"));
    }
}
