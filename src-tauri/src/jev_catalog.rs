//! Read-only model catalogs for the built-in Jev-compatible providers.
//! Catalogs never make an inference request and never accept a caller-supplied URL.

use std::io::Read;
use std::time::Duration;

use reqwest::header::ACCEPT;
use serde_json::Value;

use crate::jev::JevProvider;
use crate::system_one_adapter::{AdapterConfig, AdapterProtocol};

const MAX_CATALOG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_MODELS: usize = 2000;
const QWEN_PAGE_SIZE: usize = 100;
const QWEN_MAX_PAGES: usize = 20;
// OpenCode's model endpoint has no reliable per-model protocol field. Keep this
// allowlist aligned with the endpoint table in its Go documentation.
const GO_CHAT_MODELS: &[&str] = &[
    "glm-5.3-flash",
    "glm-5.3",
    "glm-5.2",
    "glm-5.1",
    "kimi-k3",
    "kimi-k2.7-code",
    "kimi-k2.6",
    "longcat-2.0",
    "deepseek-v4.1-flash",
    "deepseek-v4-pro",
    "deepseek-v4-flash",
    "deepseek-v4-flash-vision-exp",
    "mimo-v2.6-flash",
    "mimo-v2.6-pro",
    "mimo-v2.5",
    "mimo-v2.5-pro",
    "hy4-preview",
    "hy3",
];
const GO_MESSAGES_MODELS: &[&str] = &[
    "minimax-m3",
    "minimax-m2.7",
    "minimax-m2.5",
    "qwen3.8-max",
    "qwen3.8-flash",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.6-plus",
];

fn catalog_url(config: &AdapterConfig) -> Result<String, String> {
    let provider = config.provider;
    match provider {
        JevProvider::DeepSeek => Ok("https://api.deepseek.com/models".into()),
        JevProvider::Anthropic => Ok("https://api.anthropic.com/v1/models?limit=1000".into()),
        JevProvider::OpenRouter => Ok("https://openrouter.ai/api/v1/models".into()),
        JevProvider::Qwen => {
            let workspace = config
                .workspace_id
                .as_deref()
                .ok_or("adapter_workspace_required")?;
            crate::system_one_adapter::validate_workspace_id(workspace)?;
            let region = config.region.as_deref().unwrap_or("cn-beijing");
            if !matches!(region, "cn-beijing" | "ap-southeast-1") {
                return Err("adapter_invalid_region".into());
            }
            if region == "ap-southeast-1" {
                Ok("https://dashscope-intl.aliyuncs.com/api/v1/models".into())
            } else {
                Ok(format!(
                    "https://{workspace}.{region}.maas.aliyuncs.com/api/v1/models"
                ))
            }
        }
        JevProvider::Gemini => {
            Ok("https://generativelanguage.googleapis.com/v1beta/openai/models".into())
        }
        JevProvider::OpenCodeGo => Ok("https://opencode.ai/zen/go/v1/models".into()),
        JevProvider::Volcengine => Ok("https://ark.cn-beijing.volces.com/api/v3/models".into()),
        JevProvider::MiniMaxCn => Ok("https://api.minimax.cn/v1/models".into()),
        JevProvider::MiniMaxGlobal => Ok("https://api.minimax.io/v1/models".into()),
        JevProvider::Mimo => Ok("https://api.xiaomimimo.com/v1/models".into()),
        JevProvider::SiliconFlow => Ok("https://api.siliconflow.cn/v1/models".into()),
        JevProvider::KimiCn => Ok("https://api.moonshot.cn/v1/models".into()),
        JevProvider::KimiGlobal => Ok("https://api.moonshot.ai/v1/models".into()),
        JevProvider::AwsBedrockMantle => {
            let region = config.region.as_deref().ok_or("adapter_region_required")?;
            Ok(format!("https://bedrock-mantle.{region}.api.aws/v1/models"))
        }
        JevProvider::TencentTokenHub => {
            let host = match config.region.as_deref() {
                Some("singapore") | Some("ap-singapore") | Some("global") => {
                    "tokenhub-intl.tencentmaas.com"
                }
                _ => "tokenhub.tencentmaas.com",
            };
            Ok(format!("https://{host}/v1/models"))
        }
        JevProvider::TypeSafe
        | JevProvider::OpenAi
        | JevProvider::Custom
        | JevProvider::BigModel
        | JevProvider::Zai
        | JevProvider::BytePlus
        | JevProvider::ModelScope
        | JevProvider::Ppio => Err("jev_catalog_unsupported".into()),
    }
}

pub fn requires_key(provider: JevProvider) -> bool {
    matches!(
        provider,
        JevProvider::DeepSeek
            | JevProvider::Anthropic
            | JevProvider::Qwen
            | JevProvider::Gemini
            | JevProvider::Volcengine
            | JevProvider::MiniMaxCn
            | JevProvider::MiniMaxGlobal
            | JevProvider::Mimo
            | JevProvider::SiliconFlow
            | JevProvider::KimiCn
            | JevProvider::KimiGlobal
            | JevProvider::AwsBedrockMantle
            | JevProvider::TencentTokenHub
            | JevProvider::OpenCodeGo
    )
}

pub fn fetch_models(
    config: &AdapterConfig,
    protocol: AdapterProtocol,
    key: Option<&str>,
) -> Result<Vec<String>, String> {
    let provider = config.provider;
    if provider == JevProvider::Qwen {
        return fetch_qwen_models(config, protocol, key);
    }
    let url = catalog_url(config)?;
    if requires_key(provider) && key.is_none() {
        return Err("adapter_key_missing".into());
    }
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "jev_catalog_unavailable")?;
    let mut request = client.get(url).header(ACCEPT, "application/json");
    if let Some(key) = key {
        request = if provider == JevProvider::Anthropic {
            request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
        } else {
            request.bearer_auth(key)
        };
    }
    let response = request.send().map_err(|error| {
        if error.is_timeout() {
            "jev_catalog_timeout"
        } else {
            "jev_catalog_unavailable"
        }
    })?;
    if matches!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return Err("jev_catalog_unauthorized".into());
    }
    if !response.status().is_success() {
        return Err("jev_catalog_unavailable".into());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "jev_catalog_unavailable")?;
    if bytes.len() as u64 > MAX_CATALOG_BYTES {
        return Err("jev_catalog_invalid_response".into());
    }
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| "jev_catalog_invalid_response")?;
    let models = parse_models(&body, provider, protocol)?;
    if models.is_empty() {
        return Err("jev_catalog_invalid_response".into());
    }
    Ok(models)
}

fn fetch_qwen_models(
    config: &AdapterConfig,
    protocol: AdapterProtocol,
    key: Option<&str>,
) -> Result<Vec<String>, String> {
    let key = key.ok_or("adapter_key_missing")?;
    let base_url = catalog_url(config)?;
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "jev_catalog_unavailable")?;
    let mut models = Vec::new();
    let mut total = None;
    let mut expected_pages = None;
    for page_no in 1..=QWEN_MAX_PAGES {
        let url = format!("{base_url}?page_no={page_no}&page_size={QWEN_PAGE_SIZE}");
        let response = client
            .get(url)
            .header(ACCEPT, "application/json")
            .bearer_auth(key)
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    "jev_catalog_timeout"
                } else {
                    "jev_catalog_unavailable"
                }
            })?;
        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err("jev_catalog_unauthorized".into());
        }
        if !response.status().is_success() {
            return Err("jev_catalog_unavailable".into());
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_CATALOG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "jev_catalog_unavailable")?;
        if bytes.len() as u64 > MAX_CATALOG_BYTES {
            return Err("jev_catalog_invalid_response".into());
        }
        let body: Value =
            serde_json::from_slice(&bytes).map_err(|_| "jev_catalog_invalid_response")?;
        let page_total = validate_qwen_page(&body, page_no)?;
        if total.is_some_and(|known| known != page_total) {
            return Err("jev_catalog_invalid_response".into());
        }
        total.get_or_insert(page_total);
        if page_total > MAX_MODELS as u64 {
            return Err("jev_catalog_too_large".into());
        }
        let pages = page_total.div_ceil(QWEN_PAGE_SIZE as u64) as usize;
        if pages == 0 || pages > QWEN_MAX_PAGES {
            return Err("jev_catalog_invalid_response".into());
        }
        expected_pages.get_or_insert(pages);
        let page_models = parse_models(&body, JevProvider::Qwen, protocol)?;
        for model in page_models {
            if !models.iter().any(|existing| existing == &model) {
                models.push(model);
            }
        }
        if page_no == pages {
            break;
        }
    }
    if total.is_none() || expected_pages.is_none() || models.is_empty() {
        return Err("jev_catalog_invalid_response".into());
    }
    models.sort();
    Ok(models)
}

fn validate_qwen_page(body: &Value, expected_page_no: usize) -> Result<u64, String> {
    let page_total = body
        .pointer("/output/total")
        .and_then(Value::as_u64)
        .ok_or("jev_catalog_invalid_response")?;
    let response_page_no = body
        .pointer("/output/page_no")
        .and_then(Value::as_u64)
        .ok_or("jev_catalog_invalid_response")?;
    let response_page_size = body
        .pointer("/output/page_size")
        .and_then(Value::as_u64)
        .ok_or("jev_catalog_invalid_response")?;
    if response_page_no != expected_page_no as u64 || response_page_size != QWEN_PAGE_SIZE as u64 {
        return Err("jev_catalog_invalid_response".into());
    }
    Ok(page_total)
}

fn parse_models(
    body: &Value,
    provider: JevProvider,
    protocol: AdapterProtocol,
) -> Result<Vec<String>, String> {
    let entries = body
        .get("data")
        .or_else(|| body.pointer("/output/models"))
        .or_else(|| body.get("models"))
        .and_then(Value::as_array)
        .ok_or("jev_catalog_invalid_response")?;
    if entries.len() > MAX_MODELS {
        return Err("jev_catalog_too_large".into());
    }
    let mut models = Vec::new();
    for entry in entries {
        let Some(id) = entry
            .get("id")
            .or_else(|| entry.get("model"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if id.is_empty() || id.len() > 128 || id.trim() != id || id.chars().any(char::is_control) {
            continue;
        }
        let compatible = match provider {
            JevProvider::DeepSeek => id.starts_with("deepseek-"),
            JevProvider::Anthropic => id.starts_with("claude-"),
            JevProvider::OpenRouter => {
                let modalities = &entry["architecture"];
                ["input_modalities", "output_modalities"]
                    .iter()
                    .all(|field| {
                        modalities
                            .get(*field)
                            .and_then(Value::as_array)
                            .is_none_or(|values| {
                                values.iter().any(|value| value.as_str() == Some("text"))
                            })
                    })
            }
            JevProvider::Qwen => is_aliyun_text_chat_model(entry, id),
            JevProvider::Gemini => {
                let name = id.strip_prefix("models/").unwrap_or(id);
                name.starts_with("gemini-")
                    && !["embedding", "image", "audio", "tts", "live"]
                        .iter()
                        .any(|part| name.contains(part))
            }
            JevProvider::OpenCodeGo => match protocol {
                AdapterProtocol::OpenAi => GO_CHAT_MODELS.contains(&id),
                AdapterProtocol::Anthropic => GO_MESSAGES_MODELS.contains(&id),
            },
            JevProvider::Volcengine
            | JevProvider::MiniMaxCn
            | JevProvider::MiniMaxGlobal
            | JevProvider::Mimo
            | JevProvider::SiliconFlow
            | JevProvider::KimiCn
            | JevProvider::KimiGlobal
            | JevProvider::AwsBedrockMantle
            | JevProvider::TencentTokenHub => is_text_chat_model(entry, id),
            _ => false,
        };
        if compatible {
            let normalized = if provider == JevProvider::Gemini {
                id.strip_prefix("models/").unwrap_or(id)
            } else {
                id
            };
            if !models.iter().any(|existing| existing == normalized) {
                models.push(normalized.to_string());
            }
        }
    }
    models.sort();
    Ok(models)
}

fn is_text_chat_model(entry: &Value, id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    if [
        "embedding",
        "embed",
        "rerank",
        "moderation",
        "image",
        "vision",
        "video",
        "audio",
        "speech",
        "tts",
        "asr",
        "live",
    ]
    .iter()
    .any(|part| lower.contains(part))
    {
        return false;
    }

    let metadata = entry
        .get("architecture")
        .or_else(|| entry.get("capabilities"))
        .or_else(|| entry.get("modalities"));
    let Some(metadata) = metadata else {
        return true;
    };
    let input = ["input_modalities", "inputModalities", "input"]
        .iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_array));
    let output = ["output_modalities", "outputModalities", "output"]
        .iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_array));
    let (Some(input), Some(output)) = (input, output) else {
        return true;
    };
    input.iter().any(|value| {
        value
            .as_str()
            .is_some_and(|value| value.eq_ignore_ascii_case("text"))
    }) && output.iter().all(|value| {
        value
            .as_str()
            .is_some_and(|value| value.eq_ignore_ascii_case("text"))
    })
}

fn is_aliyun_text_chat_model(entry: &Value, id: &str) -> bool {
    if !is_text_chat_model(entry, id) {
        return false;
    }
    if let Some(metadata) = entry.get("inference_metadata") {
        for field in ["request_modality", "response_modality"] {
            if let Some(modalities) = metadata.get(field).and_then(Value::as_array) {
                if !modalities.iter().any(|value| {
                    value
                        .as_str()
                        .is_some_and(|value| value.eq_ignore_ascii_case("text"))
                }) {
                    return false;
                }
            }
        }
    }
    if let Some(capabilities) = entry.get("capabilities").and_then(Value::as_array) {
        if !capabilities.is_empty()
            && !capabilities.iter().any(|value| {
                value.as_str().is_some_and(|value| {
                    value.eq_ignore_ascii_case("TG") || value.eq_ignore_ascii_case("Reasoning")
                })
            })
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(provider: JevProvider) -> AdapterConfig {
        AdapterConfig {
            provider,
            protocol: Some(AdapterProtocol::OpenAi),
            url: None,
            model: None,
            region: None,
            endpoint_id: None,
            workspace_id: None,
        }
    }

    #[test]
    fn fixed_catalog_hosts_reject_openai_custom_and_typesafe() {
        assert_eq!(
            catalog_url(&config(JevProvider::DeepSeek)).unwrap(),
            "https://api.deepseek.com/models"
        );
        assert_eq!(
            catalog_url(&config(JevProvider::OpenCodeGo)).unwrap(),
            "https://opencode.ai/zen/go/v1/models"
        );
        for provider in [
            JevProvider::OpenAi,
            JevProvider::Custom,
            JevProvider::TypeSafe,
        ] {
            assert_eq!(
                catalog_url(&config(provider)).unwrap_err(),
                "jev_catalog_unsupported"
            );
        }
    }

    #[test]
    fn rejects_workspace_host_injection_and_requires_safe_workspace() {
        let mut qwen = config(JevProvider::Qwen);
        qwen.workspace_id = Some("workspace.evil?x=1".into());
        assert_eq!(catalog_url(&qwen).unwrap_err(), "adapter_invalid_workspace");
        qwen.workspace_id = Some("workspace-123".into());
        assert_eq!(
            catalog_url(&qwen).unwrap(),
            "https://workspace-123.cn-beijing.maas.aliyuncs.com/api/v1/models"
        );
        qwen.region = Some("ap-southeast-1".into());
        assert_eq!(
            catalog_url(&qwen).unwrap(),
            "https://dashscope-intl.aliyuncs.com/api/v1/models"
        );
        qwen.region = Some("ap-southeast-1.evil".into());
        assert_eq!(catalog_url(&qwen).unwrap_err(), "adapter_invalid_region");
    }

    #[test]
    fn filters_new_provider_catalogs_to_text_chat_models() {
        let providers = [
            JevProvider::Volcengine,
            JevProvider::MiniMaxCn,
            JevProvider::MiniMaxGlobal,
            JevProvider::Mimo,
            JevProvider::SiliconFlow,
            JevProvider::KimiCn,
            JevProvider::KimiGlobal,
            JevProvider::AwsBedrockMantle,
            JevProvider::TencentTokenHub,
        ];
        let body = json!({
            "data": [
                {"id": "provider-chat", "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]}},
                {"id": "provider-vision", "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}},
                {"id": "provider-embedding", "architecture": {"input_modalities": ["text"], "output_modalities": ["embedding"]}}
            ]
        });
        for provider in providers {
            assert_eq!(
                parse_models(&body, provider, AdapterProtocol::OpenAi).unwrap(),
                vec!["provider-chat"],
                "provider {:?}",
                provider
            );
        }
    }

    #[test]
    fn refuses_to_silently_truncate_large_catalogs() {
        let body = json!({
            "data": (0..=MAX_MODELS)
                .map(|index| json!({"id": format!("model-{index}")}))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            parse_models(&body, JevProvider::OpenRouter, AdapterProtocol::OpenAi).unwrap_err(),
            "jev_catalog_too_large"
        );
    }

    #[test]
    fn opencode_only_exposes_models_for_the_selected_protocol() {
        let body = json!({"data": [
            {"id": "deepseek-v4.1-flash"}, {"id": "minimax-m3"},
            {"id": "kimi-k3"}, {"id": "qwen3.8-flash"},
            {"id": "gpt-5.6-luna"}, {"id": "deepseek-v4.1-flash"}
        ]});
        assert_eq!(
            parse_models(&body, JevProvider::OpenCodeGo, AdapterProtocol::OpenAi).unwrap(),
            vec!["deepseek-v4.1-flash", "kimi-k3"]
        );
        assert_eq!(
            parse_models(&body, JevProvider::OpenCodeGo, AdapterProtocol::Anthropic).unwrap(),
            vec!["minimax-m3", "qwen3.8-flash"]
        );
    }

    #[test]
    fn filters_non_text_and_malformed_models() {
        let body = json!({"data": [
            {"id": "vendor/text", "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]}},
            {"id": "vendor/image", "architecture": {"input_modalities": ["text"], "output_modalities": ["image"]}},
            {"id": "bad\nmodel"}, {"id": "vendor/text"}
        ]});
        assert_eq!(
            parse_models(&body, JevProvider::OpenRouter, AdapterProtocol::OpenAi).unwrap(),
            vec!["vendor/text"]
        );
        assert!(parse_models(
            &json!({"data": []}),
            JevProvider::DeepSeek,
            AdapterProtocol::OpenAi
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn malformed_middle_qwen_page_is_rejected() {
        let first = json!({
            "output": {"total": 201, "page_no": 1, "page_size": 100, "models": [{"model": "qwen3.8-flash"}]}
        });
        let malformed_middle = json!({
            "output": {"total": 201, "page_no": 3, "page_size": 100, "models": []}
        });
        assert_eq!(validate_qwen_page(&first, 1).unwrap(), 201);
        assert_eq!(
            validate_qwen_page(&malformed_middle, 2).unwrap_err(),
            "jev_catalog_invalid_response"
        );
        assert_eq!(
            parse_models(
                &malformed_middle,
                JevProvider::Qwen,
                AdapterProtocol::OpenAi
            )
            .unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn aliyun_text_catalog_includes_non_qwen_models_and_gemini_ids_are_normalized() {
        let qwen = json!({"output": {"models": [
            {"model": "qwen-plus"},
            {"model": "deepseek-v4-pro", "capabilities": ["TG", "Reasoning"], "inference_metadata": {"request_modality": ["Text"], "response_modality": ["Text"]}},
            {"model": "wan2.6-image", "capabilities": ["ImageGeneration"], "inference_metadata": {"request_modality": ["Text"], "response_modality": ["Image"]}}
        ]}});
        assert_eq!(
            parse_models(&qwen, JevProvider::Qwen, AdapterProtocol::OpenAi).unwrap(),
            vec!["deepseek-v4-pro", "qwen-plus"]
        );
        let gemini =
            json!({"data": [{"id": "models/gemini-2.5-flash"}, {"id": "gemini-embedding-001"}]});
        assert_eq!(
            parse_models(&gemini, JevProvider::Gemini, AdapterProtocol::OpenAi).unwrap(),
            vec!["gemini-2.5-flash"]
        );
    }
}
